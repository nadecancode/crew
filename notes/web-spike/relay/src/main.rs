//! Spike for notes/web-design.md: one loopback port serving static files and a
//! WebSocket that relays crew's socket protocol, one daemon connection per WebSocket.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const MAX_HEAD: usize = 8 * 1024;

struct Shared {
    socket: PathBuf,
    dist: PathBuf,
    /// The only host name answered: `<label>.localhost:<port>`.
    host: String,
    web_token: String,
    operator_token: String,
    /// `style-src 'self'` instead of allowing style elements (what xterm.js needs).
    strict_styles: bool,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let socket = PathBuf::from(&args[1]);
    let dist = PathBuf::from(&args[2]);
    let port: u16 = args[3].parse().unwrap();
    let web_token = args[4].clone();
    let label = args[5].clone();
    let flag = |name: &str| args[6..].iter().any(|a| a == name);
    let strict_styles = flag("strict");
    // The design binds both loopback addresses; `v4only` leaves `::1` to whoever takes it.
    let v4_only = flag("v4only");
    let operator_token = crew_core::paths::read_operator_token(&socket)
        .unwrap()
        .expect("operator token");
    let v4 = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let port = v4.local_addr().unwrap().port();
    // `<label>.localhost` resolves to both loopback addresses: own both.
    let v6 = if v4_only { None } else { Some(TcpListener::bind(("::1", port)).await.expect("binding [::1]")) };
    let host = format!("{label}.localhost:{port}");
    eprintln!("listening on http://{host}/#k={web_token}");
    let shared = Arc::new(Shared { socket, dist, host, web_token, operator_token, strict_styles });
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
    if let Some(v6) = v6 {
        tokio::spawn(accept(v6, shared.clone()));
    }
    accept(v4, shared).await;
}

async fn read_head(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(buf);
        }
        if buf.len() > MAX_HEAD {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    }
}

async fn respond(stream: &mut TcpStream, status: &str, ctype: &str, extra: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\n{extra}\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}

async fn serve(shared: Arc<Shared>, mut stream: TcpStream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let head = tokio::time::timeout(Duration::from_secs(5), read_head(&mut stream)).await??;
    let mut headers = [httparse::EMPTY_HEADER; 48];
    let mut req = httparse::Request::new(&mut headers);
    let status = req.parse(&head)?;
    if status.is_partial() {
        return Err("partial".into());
    }
    let get = |name: &str| -> Option<String> {
        req.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .and_then(|h| std::str::from_utf8(h.value).ok())
            .map(str::to_string)
    };
    // Exactly this server's name: not `127.0.0.1`, not `localhost`, not an older label.
    let host_ok = get("host").is_some_and(|h| h == shared.host);
    if !host_ok {
        return Ok(respond(&mut stream, "421 Misdirected Request", "text/plain", "", b"wrong host\n").await?);
    }
    if req.method != Some("GET") {
        return Ok(respond(&mut stream, "405 Method Not Allowed", "text/plain", "Allow: GET\r\n", b"").await?);
    }
    let path = req.path.unwrap_or("/").split('?').next().unwrap_or("/").to_string();
    if path == "/ws" {
        let origin_ok = get("origin").is_some_and(|o| o == format!("http://{}", shared.host));
        let upgrade = get("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
        let key = get("sec-websocket-key");
        let version_ok = get("sec-websocket-version").as_deref() == Some("13");
        if !origin_ok {
            return Ok(respond(&mut stream, "403 Forbidden", "text/plain", "", b"wrong origin\n").await?);
        }
        let (true, Some(key), true) = (upgrade, key, version_ok) else {
            return Ok(respond(&mut stream, "400 Bad Request", "text/plain", "", b"not a websocket upgrade\n").await?);
        };
        let accept = derive_accept_key(key.as_bytes());
        stream
            .write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes())
            .await?;
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(1024 * 1024);
        config.max_frame_size = Some(1024 * 1024);
        let ws = WebSocketStream::from_raw_socket(stream, Role::Server, Some(config)).await;
        return relay(shared, ws).await;
    }
    // Static files (the real server embeds them).
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    if rel.contains("..") {
        return Ok(respond(&mut stream, "404 Not Found", "text/plain", "", b"").await?);
    }
    let file = shared.dist.join(rel);
    match tokio::fs::read(&file).await {
        Ok(body) => {
            let ctype = match Path::new(rel).extension().and_then(|e| e.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("svg") => "image/svg+xml",
                Some("json") => "application/json",
                _ => "application/octet-stream",
            };
            let styles = if shared.strict_styles { "style-src 'self'" } else { "style-src-elem 'self' 'unsafe-inline'; style-src-attr 'none'" };
            let csp = if rel == "index.html" {
                format!("Content-Security-Policy: default-src 'none'; script-src 'self'; {styles}; img-src 'self' data:; font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\nCache-Control: no-store\r\nX-Frame-Options: DENY\r\nCross-Origin-Opener-Policy: same-origin\r\n")
            } else {
                "Cache-Control: no-cache\r\n".to_string()
            };
            respond(&mut stream, "200 OK", ctype, &csp, &body).await?;
        }
        Err(_) => respond(&mut stream, "404 Not Found", "text/plain", "", b"").await?,
    }
    Ok(())
}

/// One WebSocket, one daemon connection. Frames pass through as they are;
/// only `hello` is rewritten: the browser's web token is checked here and
/// replaced by the operator token.
async fn relay(shared: Arc<Shared>, ws: WebSocketStream<TcpStream>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut ws_tx, mut ws_rx) = ws.split();
    let unix = UnixStream::connect(&shared.socket).await?;
    let (unix_rx, mut unix_tx) = unix.into_split();
    let mut lines = BufReader::new(unix_rx).lines();
    let mut authed = false;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                ws_tx.send(Message::text(line)).await?;
            }
            msg = ws_rx.next() => {
                let Some(msg) = msg else { break };
                match msg? {
                    Message::Text(text) => {
                        let mut out = text.as_str().to_string();
                        if !authed {
                            let mut v: serde_json::Value = serde_json::from_str(&out)?;
                            if v["method"] == "hello" {
                                if v["params"]["token"].as_str() != Some(shared.web_token.as_str()) {
                                    let id = v["id"].as_u64().unwrap_or(0);
                                    ws_tx.send(Message::text(format!(r#"{{"id":{id},"error":{{"code":"unauthorized","message":"web token does not match"}}}}"#))).await?;
                                    continue;
                                }
                                v["params"]["token"] = serde_json::Value::String(shared.operator_token.clone());
                                authed = true;
                                out = v.to_string();
                            } else if v["method"] != "ping" {
                                let id = v["id"].as_u64().unwrap_or(0);
                                ws_tx.send(Message::text(format!(r#"{{"id":{id},"error":{{"code":"unauthorized","message":"say hello first"}}}}"#))).await?;
                                continue;
                            }
                        }
                        out.push('\n');
                        unix_tx.write_all(out.as_bytes()).await?;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
