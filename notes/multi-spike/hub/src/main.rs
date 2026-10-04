//! Spike for notes/web-multi.md: one loopback server for every running crew
//! session of this user.
//!
//!   hub <port> <label> <pair-code> <session-token> <secret> [nocheck]
//!
//! - `GET /ws` with the exact Host and Origin upgrades to a WebSocket.
//! - `web_pair {code}` answers `{token, verifier}`; `hello {token}` answers
//!   with `web_proof` and makes the connection the *index*: `workspaces`
//!   lists the running sessions, and a `workspaces` push follows every change.
//! - `hello {token, workspace}` dials that workspace's daemon, says hello to
//!   it as the operator (the token file is read at that moment), and from
//!   then on passes lines through as they are, both ways.
//! - A message with a line break in it is refused (it would be two requests
//!   on the daemon's socket) unless `nocheck` is given, which shows why.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crew_core::paths::DaemonInfo;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Shared {
    host: String,
    pair_code: String,
    token: String,
    secret: String,
    verifier: String,
    newline_check: bool,
    /// Running sessions by id (the 16 hex digits that name the socket).
    workspaces: Mutex<BTreeMap<String, DaemonInfo>>,
    changed: broadcast::Sender<String>,
}

fn id_of(info: &DaemonInfo) -> String {
    PathBuf::from(&info.socket)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn listing(map: &BTreeMap<String, DaemonInfo>) -> Value {
    Value::Array(
        map.iter()
            .map(|(id, i)| json!({"id": id, "project": i.project, "workspace": i.workspace, "pid": i.pid, "running": true}))
            .collect(),
    )
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args[1].parse().unwrap();
    let label = args[2].clone();
    let pair_code = args[3].clone();
    let token = args[4].clone();
    let secret = args[5].clone();
    let verifier = args[6].clone(); // sha256(secret), computed by the caller
    let newline_check = !args[7..].iter().any(|a| a == "nocheck");
    let v4 = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let v6 = TcpListener::bind(("::1", port)).await.expect("binding [::1]");
    let host = format!("{label}.localhost:{port}");
    let (changed, _) = broadcast::channel(64);
    let shared = Arc::new(Shared { host, pair_code, token, secret, verifier, newline_check, workspaces: Mutex::new(BTreeMap::new()), changed });
    eprintln!("hub on http://{}/", shared.host);

    // The registry of running daemons, read again when it may have changed.
    // (The spike polls; the design has the daemons and `crew web` say so.)
    let scan = shared.clone();
    tokio::spawn(async move {
        loop {
            let now: BTreeMap<String, DaemonInfo> = tokio::task::spawn_blocking(crew_core::paths::list_sessions)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|i| (id_of(&i), i))
                .collect();
            let push = {
                let mut map = scan.workspaces.lock().unwrap();
                let same = map.len() == now.len() && map.iter().all(|(k, v)| now.get(k).is_some_and(|n| n.pid == v.pid));
                if same {
                    None
                } else {
                    *map = now;
                    Some(json!({"push": {"kind": "workspaces", "data": listing(&map)}}).to_string())
                }
            };
            if let Some(push) = push {
                let _ = scan.changed.send(push);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let accept = |listener: TcpListener, shared: Arc<Shared>| async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).ok();
            let shared = shared.clone();
            tokio::spawn(async move {
                if let Err(e) = serve(shared, stream).await {
                    eprintln!("conn: {e}");
                }
            });
        }
    };
    tokio::spawn(accept(v6, shared.clone()));
    accept(v4, shared).await;
}

async fn respond(stream: &mut TcpStream, status: &str) -> std::io::Result<()> {
    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    stream.shutdown().await
}

async fn serve(shared: Arc<Shared>, mut stream: TcpStream) -> Res<()> {
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await??;
        if n == 0 || head.len() > 8 * 1024 {
            return Ok(());
        }
        head.extend_from_slice(&chunk[..n]);
    }
    let mut headers = [httparse::EMPTY_HEADER; 48];
    let mut req = httparse::Request::new(&mut headers);
    req.parse(&head)?;
    let get = |name: &str| req.headers.iter().find(|h| h.name.eq_ignore_ascii_case(name)).and_then(|h| std::str::from_utf8(h.value).ok()).map(str::to_string);
    if get("host").as_deref() != Some(shared.host.as_str()) {
        return Ok(respond(&mut stream, "421 Misdirected Request").await?);
    }
    if req.path != Some("/ws") {
        return Ok(respond(&mut stream, "404 Not Found").await?);
    }
    if get("origin") != Some(format!("http://{}", shared.host)) {
        return Ok(respond(&mut stream, "403 Forbidden").await?);
    }
    let Some(key) = get("sec-websocket-key") else {
        return Ok(respond(&mut stream, "400 Bad Request").await?);
    };
    let accept = derive_accept_key(key.as_bytes());
    stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await?;
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(1024 * 1024);
    config.max_frame_size = Some(1024 * 1024);
    let ws = WebSocketStream::from_raw_socket(stream, Role::Server, Some(config)).await;
    session(shared, ws).await
}

fn error(id: &Value, code: &str, message: &str) -> Message {
    Message::text(json!({"id": id, "error": {"code": code, "message": message}}).to_string())
}

async fn session(shared: Arc<Shared>, ws: WebSocketStream<TcpStream>) -> Res<()> {
    let (mut tx, mut rx) = ws.split();
    // Before hello: `ping`, `web_pair`, `hello`.
    let workspace = loop {
        let Some(msg) = rx.next().await else { return Ok(()) };
        let Message::Text(text) = msg? else { continue };
        let v: Value = serde_json::from_str(text.as_str())?;
        let id = v["id"].clone();
        match v["method"].as_str() {
            Some("ping") => tx.send(Message::text(json!({"id": id, "result": {"pong": true}}).to_string())).await?,
            Some("web_pair") if v["params"]["code"].as_str() == Some(shared.pair_code.as_str()) => {
                tx.send(Message::text(json!({"id": id, "result": {"token": shared.token, "verifier": shared.verifier}}).to_string())).await?
            }
            Some("hello") if v["params"]["token"].as_str() == Some(shared.token.as_str()) => {
                match v["params"]["workspace"].as_str() {
                    None => {
                        tx.send(Message::text(json!({"id": id, "result": {"role": "operator", "protocol": v["params"]["protocol"], "version": "spike", "pid": std::process::id(), "hub": true, "web_proof": shared.secret}}).to_string())).await?;
                        break None;
                    }
                    Some(ws_id) => {
                        let info = shared.workspaces.lock().unwrap().get(ws_id).cloned();
                        let Some(info) = info else {
                            tx.send(error(&id, "unavailable", "that workspace is not running")).await?;
                            continue;
                        };
                        break Some((info, id, v["params"]["protocol"].clone()));
                    }
                }
            }
            _ => tx.send(error(&id, "unauthorized", "say hello first")).await?,
        }
    };

    let Some((info, hello_id, protocol)) = workspace else {
        // The index: the list, and a push when it changes.
        let mut changed = shared.changed.subscribe();
        loop {
            tokio::select! {
                push = changed.recv() => {
                    if let Ok(push) = push { tx.send(Message::text(push)).await?; }
                }
                msg = rx.next() => {
                    let Some(msg) = msg else { break };
                    let Message::Text(text) = msg? else { continue };
                    let v: Value = serde_json::from_str(text.as_str())?;
                    let reply = match v["method"].as_str() {
                        Some("workspaces") => json!({"id": v["id"], "result": listing(&shared.workspaces.lock().unwrap())}),
                        Some("ping") => json!({"id": v["id"], "result": {"pong": true}}),
                        _ => json!({"id": v["id"], "error": {"code": "invalid", "message": "not a call of the index"}}),
                    };
                    tx.send(Message::text(reply.to_string())).await?;
                }
            }
        }
        return Ok(());
    };

    // One daemon connection for this WebSocket. The operator token is read
    // now: a restarted daemon has a new one.
    let socket = PathBuf::from(&info.socket);
    let operator = crew_core::paths::read_operator_token(&socket).ok().flatten();
    let unix = match UnixStream::connect(&socket).await {
        Ok(unix) => unix,
        Err(_) => {
            tx.send(error(&hello_id, "unavailable", "that workspace is not running")).await?;
            return Ok(());
        }
    };
    let (unix_rx, mut unix_tx) = unix.into_split();
    let mut lines = BufReader::with_capacity(64 * 1024, unix_rx);
    let hello = json!({"id": 0, "method": "hello", "params": {"role": "operator", "token": operator, "client": "web", "protocol": protocol}});
    unix_tx.write_all(format!("{hello}\n").as_bytes()).await?;
    let mut line = String::new();
    lines.read_line(&mut line).await?;
    let mut reply: Value = serde_json::from_str(&line)?;
    reply["id"] = hello_id;
    if let Some(result) = reply.get_mut("result").and_then(Value::as_object_mut) {
        result.insert("web_proof".into(), shared.secret.clone().into());
        result.insert("workspace".into(), id_of(&info).into());
    }
    tx.send(Message::text(reply.to_string())).await?;

    // From here on: lines as they are, both ways.
    let mut buf = Vec::with_capacity(4096);
    loop {
        tokio::select! {
            n = lines.read_until(b'\n', &mut buf) => {
                if n? == 0 {
                    // The daemon went away: say so, the page keeps its index.
                    tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Library(4001), reason: "the workspace stopped".into() }))).await?;
                    break;
                }
                if buf.last() == Some(&b'\n') { buf.pop(); }
                let text = String::from_utf8(std::mem::take(&mut buf))?;
                tx.send(Message::text(text)).await?;
            }
            msg = rx.next() => {
                let Some(msg) = msg else { break };
                match msg? {
                    Message::Text(text) => {
                        if shared.newline_check && text.as_str().bytes().any(|b| b == b'\n' || b == b'\r') {
                            tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Policy, reason: "one request per message, on one line".into() }))).await?;
                            break;
                        }
                        unix_tx.write_all(text.as_str().as_bytes()).await?;
                        unix_tx.write_all(b"\n").await?;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
