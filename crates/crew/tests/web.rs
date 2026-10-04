//! The web dashboard's server against a real daemon (notes/web-design.md
//! §tests, "Rust: the bridge"). A WebSocket client and plain TCP connect to
//! `127.0.0.1:<port>` and send the `Host` and `Origin` of the address
//! `crew web --json` printed.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream as StdTcp;
use std::time::Duration;

use common::*;
use crew_core::rpc::{WebInfo, PROTOCOL_VERSION};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

type Ws = WebSocketStream<TcpStream>;

fn start_empty_daemon() -> Session {
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    s.ok(&["start", "-C", &s.workspace.to_string_lossy()]);
    s
}

fn web_open(s: &Session) -> WebInfo {
    let out = s.ok(&["web", "--json", "-C", &s.workspace.to_string_lossy()]);
    serde_json::from_str(&out).expect("crew web --json")
}

/// `<label>.localhost:<port>` of an address.
fn host_of(info: &WebInfo) -> String {
    info.url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

fn code_of(info: &WebInfo) -> String {
    info.pair_url.split("#pair=").nth(1).unwrap().to_string()
}

/// One plain HTTP request; the whole response.
fn http(port: u16, request: &str) -> String {
    let mut stream = StdTcp::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = stream.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn get(port: u16, path: &str, host: &str) -> String {
    http(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    )
}

async fn ws_with(port: u16, host: &str, origin: Option<&str>) -> Result<Ws, String> {
    let mut request = format!("ws://{host}/ws").into_client_request().unwrap();
    let headers = request.headers_mut();
    headers.insert("Host", HeaderValue::from_str(host).unwrap());
    headers.insert(
        "Sec-WebSocket-Key",
        HeaderValue::from_str(&generate_key()).unwrap(),
    );
    match origin {
        Some(origin) => {
            headers.insert("Origin", HeaderValue::from_str(origin).unwrap());
        }
        None => {
            headers.remove("Origin");
        }
    }
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tokio_tungstenite::client_async(request, stream)
        .await
        .map(|(ws, _)| ws)
        .map_err(|e| e.to_string())
}

async fn ws(info: &WebInfo) -> Ws {
    let host = host_of(info);
    ws_with(info.port, &host, Some(&format!("http://{host}")))
        .await
        .unwrap()
}

/// Send a request and return its response, skipping pushes.
async fn call(ws: &mut Ws, id: u64, method: &str, params: Value) -> Value {
    let request = json!({"id": id, "method": method, "params": params});
    ws.send(Message::text(request.to_string())).await.unwrap();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("an answer within 5 s")
            .expect("the socket is open")
            .expect("a message");
        if let Message::Text(text) = message {
            let frame: Value = serde_json::from_str(text.as_str()).unwrap();
            if frame["id"] == id {
                return frame;
            }
        }
    }
}

async fn hello(ws: &mut Ws, token: &str) -> Value {
    call(
        ws,
        1,
        "hello",
        json!({"role": "operator", "token": token, "client": "web", "protocol": PROTOCOL_VERSION}),
    )
    .await
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Pair with the code of `info`: (token, verifier).
async fn pair(info: &WebInfo) -> (String, String) {
    let mut socket = ws(info).await;
    let reply = call(&mut socket, 1, "web_pair", json!({"code": code_of(info)})).await;
    let result = &reply["result"];
    (
        result["token"].as_str().expect("a token").to_string(),
        result["verifier"].as_str().expect("a verifier").to_string(),
    )
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn off_until_asked_then_served_and_stopped() {
    let s = start_empty_daemon();
    let port = crew_core::paths::web_port(&s.workspace.canonicalize().unwrap().join(".crew"));
    assert!(
        StdTcp::connect(("127.0.0.1", port)).is_err(),
        "something listens before crew web"
    );
    let info = web_open(&s);
    assert!(info.started);
    assert!(info.url.starts_with("http://") && info.url.contains(".localhost:"));
    assert_eq!(s.snapshot().session.web, info.url);
    assert!(s.has_event("web.started"));
    // Run again: the same address, a new code.
    let again = web_open(&s);
    assert!(!again.started);
    assert_eq!(again.url, info.url);
    assert_ne!(again.pair_url, info.pair_url);
    rt().block_on(async {
        let (token, _) = pair(&info).await;
        let mut open = ws(&info).await;
        assert!(hello(&mut open, &token).await["result"].is_object());
        s.ok(&["web", "--stop", "-C", &s.workspace.to_string_lossy()]);
        // The open socket was closed, the port is free on both addresses.
        let end = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match open.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    _ => {}
                }
            }
        })
        .await;
        assert!(end.is_ok(), "the socket stayed open");
    });
    assert!(StdTcp::connect(("127.0.0.1", info.port)).is_err());
    assert!(std::net::TcpListener::bind(("127.0.0.1", info.port)).is_ok());
    assert_eq!(s.snapshot().session.web, "");
    assert!(s.has_event("web.stopped"));
}

#[test]
fn answers_only_to_its_name_and_serves_the_bundle() {
    let s = start_empty_daemon();
    let info = web_open(&s);
    let host = host_of(&info);
    let port = info.port;
    let index = get(port, "/", &host);
    assert!(index.starts_with("HTTP/1.1 200 OK\r\n"), "{index}");
    for header in [
        "Content-Security-Policy: default-src 'none'; script-src 'self';",
        "X-Frame-Options: DENY",
        "Cache-Control: no-store",
        "X-Content-Type-Options: nosniff",
    ] {
        assert!(index.contains(header), "{header}");
    }
    assert!(get(port, "/nope", &host).starts_with("HTTP/1.1 404"));
    for other in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("ffffffffffffffffffffffffffffffff.localhost:{port}"),
        "evil.example".to_string(),
    ] {
        assert!(
            get(port, "/", &other).starts_with("HTTP/1.1 421"),
            "{other}"
        );
    }
    for method in ["POST", "OPTIONS", "HEAD"] {
        let answer = http(
            port,
            &format!("{method} / HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert!(answer.starts_with("HTTP/1.1 405"), "{method}: {answer}");
    }
    assert!(!index.to_ascii_lowercase().contains("access-control-"));
    rt().block_on(async {
        for origin in [
            None,
            Some("http://evil.example".to_string()),
            Some(format!("http://127.0.0.1:{port}")),
            Some("http://127.0.0.1:5173".to_string()),
        ] {
            let refused = ws_with(port, &host, origin.as_deref()).await;
            assert!(refused.unwrap_err().contains("403"), "{origin:?}");
        }
    });
}

#[test]
fn pairing_works_once_and_the_proof_is_per_session() {
    let s = start_empty_daemon();
    let info = web_open(&s);
    rt().block_on(async {
        // `web_pair` answers a token and a verifier, never the proof.
        let mut pairing = ws(&info).await;
        let spare = code_of(&web_open(&s));
        let reply = call(&mut pairing, 1, "web_pair", json!({"code": spare})).await;
        let mut keys: Vec<&String> = reply["result"].as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["token", "verifier"], "{reply}");
        let (token, verifier) = pair(&info).await;
        let mut socket = ws(&info).await;
        let reply = hello(&mut socket, &token).await;
        let proof = reply["result"]["web_proof"].as_str().expect("a proof");
        assert_eq!(sha256_hex(proof), verifier);
        assert_ne!(proof, verifier);
        // The code is spent.
        let mut again = ws(&info).await;
        let reply = call(&mut again, 1, "web_pair", json!({"code": code_of(&info)})).await;
        assert_eq!(reply["error"]["code"], "unauthorized");
        // Another pairing: another verifier and proof.
        let second = web_open(&s);
        let (token2, verifier2) = pair(&second).await;
        assert_ne!(verifier2, verifier);
        let mut other = ws(&info).await;
        let proof2 = hello(&mut other, &token2).await["result"]["web_proof"].clone();
        assert_ne!(proof2, json!(proof));
        // Failed hellos carry no proof.
        for params in [
            json!({"role": "operator", "token": "wrong", "protocol": PROTOCOL_VERSION}),
            json!({"role": "operator", "token": token, "protocol": 4}),
            json!({"role": "agent", "agent_id": "a", "token": token, "protocol": PROTOCOL_VERSION}),
        ] {
            let mut fresh = ws(&info).await;
            let reply = call(&mut fresh, 1, "hello", params.clone()).await;
            assert!(reply["error"].is_object(), "{params}");
            assert!(reply.get("result").is_none());
        }
    });
}

#[test]
fn nothing_before_hello_and_each_credential_on_its_own_transport() {
    let s = team_of_one(&[]);
    let info = web_open(&s);
    rt().block_on(async {
        let mut socket = ws(&info).await;
        let reply = call(&mut socket, 1, "snapshot", json!({})).await;
        assert_eq!(reply["error"]["code"], "unauthorized");
        let reply = call(&mut socket, 2, "ping", json!({})).await;
        assert_eq!(reply["result"]["pong"], true);
        let (token, _) = pair(&info).await;
        let mut socket = ws(&info).await;
        assert!(hello(&mut socket, &token).await["result"].is_object());
        for method in ["web_open", "web_close"] {
            let reply = call(&mut socket, 3, method, json!({})).await;
            assert_eq!(reply["error"]["code"], "forbidden", "{method}");
        }
        // The socket's credentials are nothing here: a wrong token, the
        // operator token, an agent's own token.
        let operator_token = crew_core::paths::read_operator_token(&socket_of(&s))
            .unwrap()
            .unwrap();
        let identity: Value =
            serde_json::from_str(&std::fs::read_to_string(s.identity_path("a")).unwrap()).unwrap();
        for params in [
            json!({"role": "operator", "token": "wrong", "protocol": PROTOCOL_VERSION}),
            json!({"role": "operator", "token": operator_token, "protocol": PROTOCOL_VERSION}),
            json!({"role": "agent", "agent_id": "a", "token": identity["token"],
                "protocol": PROTOCOL_VERSION}),
        ] {
            let mut fresh = ws(&info).await;
            let reply = call(&mut fresh, 1, "hello", params.clone()).await;
            assert_eq!(reply["error"]["code"], "unauthorized", "{params}: {reply}");
            let reply = call(&mut fresh, 2, "snapshot", json!({})).await;
            assert_eq!(reply["error"]["code"], "unauthorized", "{params}: {reply}");
        }
        // No hello within 5 s: closed.
        let mut idle = ws(&info).await;
        let ended = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                match idle.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    _ => {}
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "a socket without hello stayed open");
    });
}

#[test]
fn pushes_reach_the_browser_after_the_subscribe_reply() {
    let s = start_empty_daemon();
    let info = web_open(&s);
    rt().block_on(async {
        let (token, _) = pair(&info).await;
        let mut socket = ws(&info).await;
        assert!(hello(&mut socket, &token).await["result"].is_object());
        socket
            .send(Message::text(
                json!({"id": 2, "method": "subscribe"}).to_string(),
            ))
            .await
            .unwrap();
        let first = socket.next().await.unwrap().unwrap();
        let first: Value = serde_json::from_str(first.to_text().unwrap()).unwrap();
        assert_eq!(first["id"], 2, "{first}");
        let reply = call(&mut socket, 3, "web_prefs", json!({"set": {"keys": false}})).await;
        assert_eq!(reply["result"]["prefs"]["keys"], false);
        let reply = call(
            &mut socket,
            4,
            "snapshot",
            json!({"messages": 0, "events": 5}),
        )
        .await;
        assert!(reply["result"]["session"]["web"]
            .as_str()
            .unwrap()
            .contains(".localhost:"));
    });
}

/// One connection to the daemon's socket, in raw JSON lines.
struct Raw {
    writer: std::os::unix::net::UnixStream,
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    next_id: u64,
}

impl Raw {
    fn connect(s: &Session) -> Self {
        let stream = std::os::unix::net::UnixStream::connect(socket_of(s)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: std::io::BufReader::new(stream),
            next_id: 1,
        }
    }

    /// The reply to one call; pushes are skipped.
    fn call(&mut self, method: &str, params: Value) -> Value {
        use std::io::BufRead;
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "id": id, "method": method, "params": params });
        writeln!(self.writer, "{request}").unwrap();
        loop {
            let mut line = String::new();
            assert_ne!(self.reader.read_line(&mut line).unwrap(), 0, "closed");
            let frame: Value = serde_json::from_str(&line).unwrap();
            if frame["id"] == id {
                return frame;
            }
        }
    }

    fn operator(s: &Session) -> Self {
        let mut raw = Self::connect(s);
        let token = crew_core::paths::read_operator_token(&socket_of(s))
            .unwrap()
            .unwrap();
        let reply = raw.call(
            "hello",
            json!({"role": "operator", "token": token, "client": "test", "protocol": PROTOCOL_VERSION}),
        );
        assert!(reply["result"].is_object(), "{reply}");
        raw
    }

    fn agent(s: &Session, agent: &str) -> Self {
        let mut raw = Self::connect(s);
        let identity: Value =
            serde_json::from_str(&std::fs::read_to_string(s.identity_path(agent)).unwrap())
                .unwrap();
        let reply = raw.call(
            "hello",
            json!({"role": "agent", "agent_id": agent, "token": identity["token"],
                "client": "test", "protocol": PROTOCOL_VERSION}),
        );
        assert!(reply["result"].is_object(), "{reply}");
        raw
    }
}

fn socket_of(s: &Session) -> std::path::PathBuf {
    let info: Value = serde_json::from_str(
        &std::fs::read_to_string(s.workspace.join(".crew/daemon.json")).unwrap(),
    )
    .unwrap();
    info["socket"].as_str().unwrap().into()
}

/// A team of one scripted agent `a` that runs `steps` at start and then
/// waits.
fn team_of_one(steps: &[&str]) -> Session {
    let s = Session::new();
    let team = s.team("web", vec![agent("a", "", steps)], vec![]);
    s.run(&team);
    s.wait("the agent to come up", |snap| {
        find_agent(snap, "a").status != crew_core::AgentStatus::Spawning
    });
    s
}

/// A signed-in WebSocket on a fresh pairing.
async fn signed_in(info: &WebInfo) -> (Ws, String) {
    let (token, _) = pair(info).await;
    let mut socket = ws(info).await;
    let reply = hello(&mut socket, &token).await;
    assert!(reply["result"].is_object(), "{reply}");
    (socket, token)
}

/// Every text frame up to the close, and the close's code.
async fn until_closed(ws: &mut Ws) -> (Vec<Value>, Option<u16>) {
    let mut frames = Vec::new();
    let code = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    frames.push(serde_json::from_str(text.as_str()).unwrap())
                }
                Some(Ok(Message::Close(frame))) => break frame.map(|f| u16::from(f.code)),
                None | Some(Err(_)) => break None,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .expect("closed within 10 s");
    (frames, code)
}

/// The next text frame.
async fn next_frame(ws: &mut Ws) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("a frame within 10 s")
            .expect("the socket is open")
            .expect("a message");
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

fn has_ipv6() -> bool {
    std::net::TcpListener::bind(("::1", 0)).is_ok()
}

fn bind_error(addr: std::net::SocketAddr) -> Option<std::io::ErrorKind> {
    std::net::TcpListener::bind(addr).err().map(|e| e.kind())
}

#[test]
fn owns_both_loopback_addresses() {
    if !has_ipv6() {
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    }
    let s = start_empty_daemon();
    let first = crew_core::paths::web_port(&s.workspace.canonicalize().unwrap().join(".crew"));
    // Someone holds [::1] of the workspace's port: that port is skipped.
    let squatter = std::net::TcpListener::bind(("::1", first));
    let info = web_open(&s);
    if squatter.is_ok() {
        assert!(info.port > first && info.port < first + 16, "{}", info.port);
    }
    let host = host_of(&info);
    for ip in ["127.0.0.1", "::1"] {
        let mut stream = StdTcp::connect((ip, info.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(stream, "GET / HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 200 OK\r\n"), "{ip}: {out}");
    }
    for ip in ["127.0.0.1", "::1"] {
        let addr: std::net::SocketAddr =
            format!("{}:{}", if ip == "::1" { "[::1]" } else { ip }, info.port)
                .parse()
                .unwrap();
        assert_eq!(
            bind_error(addr),
            Some(std::io::ErrorKind::AddrInUse),
            "{addr}"
        );
    }
}

/// Bind `ip:port` with `SO_REUSEPORT` (and `SO_REUSEADDR`) set: the errno.
fn bind_reuseport(v6: bool, port: u16) -> i32 {
    // SAFETY: plain socket calls on a descriptor this function owns and
    // closes; the address structs are zeroed and filled before use.
    unsafe {
        let fd = libc::socket(
            if v6 { libc::AF_INET6 } else { libc::AF_INET },
            libc::SOCK_STREAM,
            0,
        );
        assert!(fd >= 0);
        let one: libc::c_int = 1;
        for option in [libc::SO_REUSEPORT, libc::SO_REUSEADDR] {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                (&one as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        let rc = if v6 {
            let mut addr: libc::sockaddr_in6 = std::mem::zeroed();
            addr.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            addr.sin6_port = port.to_be();
            addr.sin6_addr.s6_addr[15] = 1;
            libc::bind(
                fd,
                (&addr as *const libc::sockaddr_in6).cast(),
                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
            )
        } else {
            let mut addr: libc::sockaddr_in = std::mem::zeroed();
            addr.sin_family = libc::AF_INET as libc::sa_family_t;
            addr.sin_port = port.to_be();
            addr.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();
            libc::bind(
                fd,
                (&addr as *const libc::sockaddr_in).cast(),
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        let errno = if rc == 0 {
            0
        } else {
            std::io::Error::last_os_error().raw_os_error().unwrap()
        };
        libc::close(fd);
        errno
    }
}

#[test]
fn port_is_not_shared() {
    let s = start_empty_daemon();
    let info = web_open(&s);
    assert_eq!(bind_reuseport(false, info.port), libc::EADDRINUSE);
    if has_ipv6() {
        assert_eq!(bind_reuseport(true, info.port), libc::EADDRINUSE);
    }
}

#[test]
fn a_new_start_is_a_new_origin() {
    let s = start_empty_daemon();
    let ws_dir = s.workspace.to_string_lossy().into_owned();
    let first = web_open(&s);
    let (token, spare) = rt().block_on(async {
        let (token, _) = pair(&first).await;
        (token, code_of(&web_open(&s)))
    });

    // Stopped and served again: another name; the old name, token and code
    // are nothing to it.
    s.ok(&["web", "--stop", "-C", &ws_dir]);
    let second = web_open(&s);
    let refused = |old: &WebInfo, new: &WebInfo, token: &str, code: &str| {
        assert_ne!(host_of(old), host_of(new));
        assert!(get(new.port, "/", &host_of(old)).starts_with("HTTP/1.1 421"));
        rt().block_on(async {
            let mut socket = ws(new).await;
            assert_eq!(
                hello(&mut socket, token).await["error"]["code"],
                "unauthorized"
            );
            let mut socket = ws(new).await;
            let reply = call(&mut socket, 1, "web_pair", json!({"code": code})).await;
            assert_eq!(reply["error"]["code"], "unauthorized");
        });
    };
    refused(&first, &second, &token, &spare);

    // The daemon stopped and started: the same.
    let (token, spare) = rt().block_on(async {
        let (token, _) = pair(&second).await;
        (token, code_of(&web_open(&s)))
    });
    s.stop();
    s.ok(&["start", "-C", &ws_dir]);
    let third = web_open(&s);
    assert!(third.started);
    assert_ne!(host_of(&third), host_of(&first));
    refused(&second, &third, &token, &spare);
}

#[test]
fn web_token_stays_on_the_web() {
    let s = start_empty_daemon();
    let info = web_open(&s);
    let (token, _) = rt().block_on(pair(&info));
    let mut raw = Raw::connect(&s);
    let reply = raw.call(
        "hello",
        json!({"role": "operator", "token": token, "client": "test", "protocol": PROTOCOL_VERSION}),
    );
    assert_eq!(reply["error"]["code"], "unauthorized", "{reply}");
    assert!(!reply.to_string().contains("web_proof"));
    // The socket's own hello shows no proof either.
    let operator_token = crew_core::paths::read_operator_token(&socket_of(&s))
        .unwrap()
        .unwrap();
    let reply = Raw::connect(&s).call(
        "hello",
        json!({"role": "operator", "token": operator_token, "client": "test",
            "protocol": PROTOCOL_VERSION}),
    );
    assert!(reply["result"].is_object(), "{reply}");
    assert!(reply["result"].get("web_proof").is_none(), "{reply}");
    // A web session may not load a team.
    let team = serde_json::to_value(s.team("web", vec![], vec![])).unwrap();
    rt().block_on(async {
        let (mut socket, _) = signed_in(&web_open(&s)).await;
        let reply = call(&mut socket, 2, "load_team", json!({"config": team})).await;
        assert_eq!(reply["error"]["code"], "forbidden", "{reply}");
    });
}

#[test]
fn prefs_are_kept() {
    let s = team_of_one(&[]);
    let ws_dir = s.workspace.to_string_lossy().into_owned();
    let prefs = json!({"theme": "dark", "panes": {"log": 0.4}});
    let info = web_open(&s);
    rt().block_on(async {
        let (mut socket, _) = signed_in(&info).await;
        let reply = call(&mut socket, 2, "web_prefs", json!({"set": prefs})).await;
        assert_eq!(reply["result"]["prefs"], prefs, "{reply}");
        let big = json!({"x": "a".repeat(17 * 1024)});
        for set in [big, json!([1, 2]), json!("dark"), json!(3)] {
            let reply = call(&mut socket, 3, "web_prefs", json!({"set": set})).await;
            assert_eq!(reply["error"]["code"], "invalid", "{reply}");
        }
        let reply = call(&mut socket, 4, "web_prefs", json!({})).await;
        assert_eq!(
            reply["result"]["prefs"], prefs,
            "refused sets changed nothing"
        );
    });
    // On the socket, and not for an agent.
    let reply = Raw::operator(&s).call("web_prefs", json!({}));
    assert_eq!(reply["result"]["prefs"], prefs);
    let reply = Raw::agent(&s, "a").call("web_prefs", json!({}));
    assert_eq!(reply["error"]["code"], "forbidden", "{reply}");
    // After a restart of the daemon.
    s.stop();
    s.ok(&["start", "-C", &ws_dir]);
    let reply = Raw::operator(&s).call("web_prefs", json!({}));
    assert_eq!(reply["result"]["prefs"], prefs);
    let info = web_open(&s);
    rt().block_on(async {
        let (mut socket, _) = signed_in(&info).await;
        let reply = call(&mut socket, 2, "web_prefs", json!({})).await;
        assert_eq!(reply["result"]["prefs"], prefs);
    });
}

#[test]
fn same_answers_as_the_socket() {
    let s = Session::new();
    let team = s.team(
        "web",
        vec![agent(
            "a",
            "",
            &["--on-task=progress 50 half", "--on-task=done finished"],
        )],
        vec![task("t1", "a", &[])],
    );
    s.run(&team);
    s.wait_all_done();
    s.send("a", "a word for the record");
    let info = web_open(&s);
    let calls = [
        ("snapshot", json!({})),
        ("tasks_list", Value::Null),
        ("history", json!({"limit": 50})),
        ("events", json!({"limit": 100})),
    ];
    rt().block_on(async {
        let (mut socket, _) = signed_in(&info).await;
        let mut raw = Raw::operator(&s);
        for (i, (method, params)) in calls.iter().enumerate() {
            // The same state read on each side of the web's answer: the
            // web's must be one of them (an agent may tick in between).
            // The clock of the answer is the one thing that differs.
            let read = |mut answer: Value| {
                if let Some(result) = answer["result"].as_object_mut() {
                    result.remove("server_time");
                }
                answer["result"].take()
            };
            let before = read(raw.call(method, params.clone()));
            let web = read(call(&mut socket, 10 + i as u64, method, params.clone()).await);
            let after = read(raw.call(method, params.clone()));
            assert!(!web.is_null(), "{method}");
            assert!(web == before || web == after, "{method}:\n{web}\n{before}");
        }
    });
}

#[test]
fn reconnect_sees_what_it_missed() {
    let s = team_of_one(&[]);
    let info = web_open(&s);
    rt().block_on(async {
        let (mut socket, token) = signed_in(&info).await;
        socket.close(None).await.unwrap();
        drop(socket);
        s.send("a", "sent while the page was away");
        let mut socket = ws(&info).await;
        assert!(hello(&mut socket, &token).await["result"].is_object());
        let snapshot = call(&mut socket, 2, "snapshot", json!({})).await;
        let messages = snapshot["result"]["messages"].as_array().unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m["body"] == "sent while the page was away"),
            "{snapshot}"
        );
    });
}

/// What the `pty` pushes of a connection decode to, until `want` is seen.
async fn screen_until(socket: &mut Ws, want: &str) -> String {
    use base64::Engine as _;
    let mut screen = String::new();
    let seen = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = next_frame(socket).await;
            if frame["push"]["kind"] == "pty" {
                let data = frame["push"]["data"]["data"].as_str().unwrap();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap();
                screen.push_str(&String::from_utf8_lossy(&bytes));
                if screen.contains(want) {
                    break;
                }
            }
        }
    })
    .await;
    assert!(seen.is_ok(), "never saw {want:?} in {screen:?}");
    screen
}

#[test]
fn terminal_over_the_web() {
    use base64::Engine as _;
    let s = team_of_one(&[]);
    let info = web_open(&s);
    rt().block_on(async {
        let (_live, token) = signed_in(&info).await;
        let attach = || async {
            let mut socket = ws(&info).await;
            assert!(hello(&mut socket, &token).await["result"].is_object());
            let reply = call(
                &mut socket,
                2,
                "attach",
                json!({"agent": "a", "rows": 24, "cols": 80}),
            )
            .await;
            assert_eq!(reply["result"]["attached"], "a", "{reply}");
            socket
        };
        let mut terminal = attach().await;
        // Typed into the agent's terminal, echoed back as `pty` pushes; the
        // input itself is not answered.
        let typed = base64::engine::general_purpose::STANDARD.encode("crew-web-echo");
        terminal
            .send(Message::text(
                json!({"id": 3, "method": "attach_input", "params": {"data": typed}}).to_string(),
            ))
            .await
            .unwrap();
        screen_until(&mut terminal, "crew-web-echo").await;
        // Closing detaches: another attachment gets the terminal.
        terminal.close(None).await.unwrap();
        drop(terminal);
        let mut again = attach().await;
        let typed = base64::engine::general_purpose::STANDARD.encode("second-look");
        again
            .send(Message::text(
                json!({"id": 3, "method": "attach_input", "params": {"data": typed}}).to_string(),
            ))
            .await
            .unwrap();
        let screen = screen_until(&mut again, "second-look").await;
        assert!(!screen.contains("\"id\":3"));
    });
    assert!(!find_agent(&s.snapshot(), "a").status.is_terminal());
}

#[test]
fn limits() {
    let s = start_empty_daemon();
    let info = web_open(&s);
    let host = host_of(&info);
    let origin = format!("http://{host}");
    rt().block_on(async {
        // 32 connections may wait for their hello; the 33rd is closed at
        // accept.
        let mut waiting = Vec::new();
        for _ in 0..32 {
            waiting.push(ws(&info).await);
        }
        assert!(ws_with(info.port, &host, Some(&origin)).await.is_err());
        drop(waiting);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while ws_with(info.port, &host, Some(&origin)).await.is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "no room after they closed"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Over 4 KiB before hello.
        let mut early = ws(&info).await;
        early
            .send(Message::text(format!(
                "{{\"id\":1,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}",
                "x".repeat(5 * 1024)
            )))
            .await
            .unwrap();
        let (_, code) = until_closed(&mut early).await;
        assert_eq!(code, Some(1009));

        // Over 1 MiB after it.
        let (mut socket, _) = signed_in(&info).await;
        socket
            .send(Message::text("x".repeat(crew_core::MAX_FRAME_BYTES + 1)))
            .await
            .unwrap();
        let (_, code) = until_closed(&mut socket).await;
        assert_eq!(code, Some(1009));

        // A binary message.
        let (mut socket, _) = signed_in(&web_open(&s)).await;
        socket.send(Message::binary(b"{}".to_vec())).await.unwrap();
        let (_, code) = until_closed(&mut socket).await;
        assert_eq!(code, Some(1003));
    });
}

#[test]
fn stop_tells_the_browser() {
    let s = start_empty_daemon();
    let info = web_open(&s);
    rt().block_on(async {
        let (mut socket, _) = signed_in(&info).await;
        assert_eq!(
            call(&mut socket, 2, "subscribe", json!({})).await["result"]["subscribed"],
            true
        );
        // `crew stop` waits for the daemon to exit; the page reads meanwhile.
        let mut stop = s.command();
        stop.args(["stop", "-C", &s.workspace.to_string_lossy()]);
        let stopping = std::thread::spawn(move || stop.output().unwrap());
        let (frames, code) = until_closed(&mut socket).await;
        assert!(
            frames.iter().any(|f| f["push"]["kind"] == "shutdown"),
            "{frames:?}"
        );
        assert_eq!(code, Some(1001), "{frames:?}");
        assert!(stopping.join().unwrap().status.success());
    });
    assert!(std::net::TcpListener::bind(("127.0.0.1", info.port)).is_ok());
    if has_ipv6() {
        assert!(std::net::TcpListener::bind(("::1", info.port)).is_ok());
    }
}

/// Every regular file below `dir`.
fn files_below(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            files_below(&entry.path(), out);
        } else if kind.is_file() {
            out.push(entry.path());
        }
    }
}

/// The files below the session's root holding any of `secrets`, and the
/// names of the files looked at.
fn written(s: &Session, secrets: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    files_below(s.root(), &mut files);
    let mut found = Vec::new();
    for file in &files {
        let Ok(bytes) = std::fs::read(file) else {
            continue;
        };
        for secret in secrets {
            if bytes.windows(secret.len()).any(|w| w == secret.as_bytes()) {
                found.push(format!("{} has {secret}", file.display()));
            }
        }
    }
    let names = files
        .iter()
        .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    (found, names)
}

#[test]
fn no_secret_is_written() {
    let s = start_empty_daemon();
    let info = web_open(&s);
    let spent = code_of(&info);
    let (token, proof, unspent) = rt().block_on(async {
        let (token, _) = pair(&info).await;
        let mut socket = ws(&info).await;
        let proof = hello(&mut socket, &token).await["result"]["web_proof"]
            .as_str()
            .unwrap()
            .to_string();
        // A failed login is logged too, without what it tried.
        let mut wrong = ws(&info).await;
        assert!(hello(&mut wrong, &format!("{token}x")).await["error"].is_object());
        (token, proof, code_of(&web_open(&s)))
    });
    let secrets = [
        spent.as_str(),
        unspent.as_str(),
        token.as_str(),
        proof.as_str(),
    ];
    assert!(secrets.iter().all(|x| x.len() >= 16));
    let logs = s.ok(&[
        "logs",
        "--json",
        "-n",
        "2000",
        "-C",
        &s.workspace.to_string_lossy(),
    ]);
    assert!(logs.contains("web.paired") && logs.contains("web.auth_failed"));
    for secret in secrets {
        assert!(!logs.contains(secret), "crew logs --json shows a secret");
    }
    let (found, names) = written(&s, &secrets);
    assert!(found.is_empty(), "{found:?}");
    for name in ["crew.db", "daemon.json", "daemon.log"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} not among {names:?}"
        );
    }
    // After the daemon stopped, its last writes too.
    s.stop();
    let (found, _) = written(&s, &secrets);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn web_needs_the_operator() {
    let s = start_empty_daemon();
    let out = s
        .command()
        .env("CREW_IDENTITY", s.root().join("identity.json"))
        .args(["web", "--json", "-C", &s.workspace.to_string_lossy()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("operator commands are not available to agents"),
        "{}",
        stderr(&out)
    );
    assert_eq!(s.snapshot().session.web, "");
}

/// `n` of a note `n=<n>`; 0 for anything else.
fn n_of(note: &Value) -> u64 {
    note.as_str()
        .and_then(|a| a.strip_prefix("n="))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// `n` of task `t1` in a `task` push.
fn pushed_n(frame: &Value) -> Option<u64> {
    let push = &frame["push"];
    (push["kind"] == "task" && push["data"]["id"] == "t1").then(|| n_of(&push["data"]["note"]))
}

#[test]
fn snapshot_under_load() {
    const FINAL: u64 = 300;
    // Task t1's note counts up as fast as the agent can report, each
    // report a `task` push.
    let steps: Vec<String> = std::iter::once("--on-task=sleep 1500".to_string())
        .chain((1..=FINAL).map(|n| format!("--on-task=progress 50 n={n}")))
        .collect();
    let steps: Vec<&str> = steps.iter().map(String::as_str).collect();
    let s = Session::new();
    s.run(&s.team(
        "web",
        vec![agent("a", "", &steps)],
        vec![task("t1", "a", &[])],
    ));
    let info = web_open(&s);
    rt().block_on(async {
        let (mut socket, _) = signed_in(&info).await;
        assert_eq!(
            call(&mut socket, 2, "subscribe", json!({})).await["result"]["subscribed"],
            true
        );
        // The page's rule: pushes are dropped while a snapshot is asked
        // for, the reply replaces the state, pushes after it apply.
        let mut model = 0;
        let mut since_reply: Vec<u64> = Vec::new();
        let mut replies = Vec::new();
        let mut moving = 0;
        let started = std::time::Instant::now();
        let mut id = 10;
        while model != FINAL {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "stuck at {model}"
            );
            id += 1;
            let request = json!({"id": id, "method": "snapshot",
                "params": {"messages": 0, "events": 0}});
            socket
                .send(Message::text(request.to_string()))
                .await
                .unwrap();
            loop {
                let frame = next_frame(&mut socket).await;
                if frame["id"] == id {
                    let tasks = frame["result"]["tasks"].as_array().unwrap();
                    let t1 = tasks.iter().find(|t| t["id"] == "t1").unwrap();
                    let n = n_of(&t1["note"]);
                    // Check 1: nothing that came before the reply is newer.
                    for pushed in since_reply.drain(..) {
                        assert!(
                            pushed <= n,
                            "a push of n={pushed} came before a reply of n={n}"
                        );
                    }
                    model = n;
                    replies.push(n);
                    break;
                }
                if let Some(n) = pushed_n(&frame) {
                    if n > 0 && n < FINAL {
                        moving += 1;
                    }
                    since_reply.push(n);
                }
            }
            // Between snapshots, pushes apply.
            let window = tokio::time::sleep(Duration::from_millis(3));
            tokio::pin!(window);
            loop {
                tokio::select! {
                    _ = &mut window => break,
                    message = socket.next() => {
                        let Some(Ok(Message::Text(text))) = message else { break };
                        let frame: Value = serde_json::from_str(text.as_str()).unwrap();
                        if let Some(n) = pushed_n(&frame) {
                            if n > 0 && n < FINAL {
                                moving += 1;
                            }
                            since_reply.push(n);
                            model = n;
                        }
                    }
                }
            }
        }
        // Check 3: the model holds the final state, and the run was under
        // load while it was asked.
        assert_eq!(model, FINAL);
        let amid = replies.iter().filter(|n| **n > 0 && **n < FINAL).count();
        assert!(moving >= 100, "too few pushes under load: {moving}");
        assert!(amid >= 5, "too few snapshots under load: {replies:?}");
        println!(
            "snapshot_under_load: {} snapshots ({amid} under load), {moving} pushes",
            replies.len()
        );
    });
}

#[test]
fn pushes_reach_the_browser_in_time() {
    let s = team_of_one(&[]);
    let info = web_open(&s);
    let sends = 20;
    let latencies = rt().block_on(async {
        let (mut socket, _) = signed_in(&info).await;
        socket
            .send(Message::text(
                json!({"id": 2, "method": "subscribe"}).to_string(),
            ))
            .await
            .unwrap();
        let first = next_frame(&mut socket).await;
        assert_eq!(first["id"], 2, "the reply precedes every push: {first}");
        let commands: Vec<std::process::Command> = (0..sends)
            .map(|i| {
                let mut command = s.command();
                command.args([
                    "send",
                    "a",
                    &format!("latency {i}"),
                    "-C",
                    &s.workspace.to_string_lossy(),
                ]);
                command
            })
            .collect();
        let sender = std::thread::spawn(move || {
            for mut command in commands {
                let out = command.output().unwrap();
                assert!(out.status.success());
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let mut latencies = Vec::new();
        let mut messages = 0;
        while messages < sends {
            let frame = next_frame(&mut socket).await;
            let now = crew_core::util::now_ms();
            match frame["push"]["kind"].as_str() {
                Some("message")
                    if frame["push"]["data"]["body"]
                        .as_str()
                        .is_some_and(|b| b.starts_with("latency ")) =>
                {
                    messages += 1
                }
                Some("event") => {
                    latencies.push(now - frame["push"]["data"]["timestamp"].as_i64().unwrap())
                }
                _ => {}
            }
        }
        sender.join().unwrap();
        latencies
    });
    let mut sorted = latencies.clone();
    sorted.sort();
    let median = sorted[sorted.len() / 2];
    let p95 = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)];
    println!(
        "pushes: emit to receipt over {} events: median {median} ms, p95 {p95} ms",
        sorted.len()
    );
    assert!(median <= 10, "median {median} ms: {sorted:?}");
}

#[test]
fn port_zero_is_refused() {
    let s = start_empty_daemon();
    let ws_dir = s.workspace.to_string_lossy().into_owned();
    let out = s.crew(&["web", "--port", "0", "--json", "-C", &ws_dir]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--port"), "{}", stderr(&out));
    let reply = Raw::operator(&s).call("web_open", json!({"port": 0}));
    assert_eq!(reply["error"]["code"], "invalid", "{reply}");
    assert_eq!(s.snapshot().session.web, "");
}
