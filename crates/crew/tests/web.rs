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
    let s = start_empty_daemon();
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
