use super::*;

#[path = "review_regressions.rs"]
mod review_regressions;

fn serving() -> Registry {
    Registry {
        serving: Some(Serving {
            label: random_label(),
            port: 23417,
        }),
        ..Registry::default()
    }
}

#[test]
fn a_code_works_once_until_it_expires() {
    let mut r = serving();
    let code = r.code(0);
    let paired = r.pair(&code, 1_000).unwrap();
    assert_eq!(paired.verifier.len(), 64);
    assert_eq!(r.pair(&code, 1_000).unwrap_err().code, "unauthorized");
    let late = r.code(0);
    assert_eq!(r.pair(&late, CODE_TTL_MS).unwrap_err().code, "unauthorized");
    let fresh = r.code(0);
    assert!(r.pair(&fresh, CODE_TTL_MS - 1).is_ok());
    assert_eq!(r.pair("", 0).unwrap_err().code, "unauthorized");
}

#[test]
fn the_ninth_code_drops_the_oldest() {
    let mut r = serving();
    let codes: Vec<String> = (0..=MAX_CODES).map(|i| r.code(i as i64)).collect();
    assert_eq!(r.pair(&codes[0], 100).unwrap_err().code, "unauthorized");
    for code in &codes[1..] {
        assert!(r.pair(code, 100).is_ok());
    }
}

#[test]
fn the_seventeenth_session_drops_the_idle_one_with_the_oldest_hello() {
    let mut r = serving();
    let mut tokens = Vec::new();
    for i in 0..MAX_SESSIONS {
        let code = r.code(i as i64);
        tokens.push(r.pair(&code, i as i64).unwrap().token);
    }
    // Every session but the first two has a connection open; of the two
    // idle ones, the second said hello last (on a connection gone since).
    let mut open = Vec::new();
    for token in &tokens[2..] {
        open.push(signed_in(&mut r, token, 50));
    }
    let gone = signed_in(&mut r, &tokens[1], 60);
    r.release(gone);
    let code = r.code(70);
    let newest = r.pair(&code, 70).unwrap().token;
    assert_eq!(r.sessions(), MAX_SESSIONS);
    let (conn, _) = r.admit().unwrap();
    assert_eq!(
        r.hello(conn, &tokens[0], 80).unwrap_err().code,
        "unauthorized"
    );
    r.release(conn);
    // All sixteen have connections: refused, and the code stays good.
    open.push(signed_in(&mut r, &tokens[1], 85));
    open.push(signed_in(&mut r, &newest, 85));
    let code = r.code(90);
    assert_eq!(r.pair(&code, 90).unwrap_err().code, "unavailable");
    r.release(open[0]);
    assert!(r.pair(&code, 91).is_ok());
}

/// A connection admitted that said hello with `token`: its id.
fn signed_in(r: &mut Registry, token: &str, now: i64) -> u64 {
    let (conn, _) = r.admit().unwrap();
    r.hello(conn, token, now).unwrap();
    conn
}

#[test]
fn hello_answers_the_secret_whose_hash_the_verifier_is() {
    let mut r = serving();
    let code = r.code(0);
    let paired = r.pair(&code, 0).unwrap();
    let (conn, _) = r.admit().unwrap();
    let (_, secret) = r.hello(conn, &paired.token, 1).unwrap();
    assert_eq!(verifier(&secret), paired.verifier);
    assert_ne!(secret, paired.token);
    // Once per connection.
    assert_eq!(
        r.hello(conn, &paired.token, 1).unwrap_err().code,
        "forbidden"
    );
    let (conn, _) = r.admit().unwrap();
    assert_eq!(r.hello(conn, "nope", 1).unwrap_err().code, "unauthorized");
    // Stopping: nothing works, and nothing survives.
    r.stopping = true;
    assert_eq!(
        r.hello(conn, &paired.token, 2).unwrap_err().code,
        "unavailable"
    );
    r.revoke();
    r.stopping = false;
    assert_eq!(
        r.hello(conn, &paired.token, 3).unwrap_err().code,
        "unauthorized"
    );
    // A connection already gone is let in by nothing.
    r.release(conn);
    let code = r.code(4);
    let token = r.pair(&code, 4).unwrap().token;
    assert_eq!(r.hello(conn, &token, 5).unwrap_err().code, "unavailable");
}

#[test]
fn connections_are_bounded_and_counted_by_what_is_open() {
    let mut r = serving();
    let code = r.code(0);
    let token = r.pair(&code, 0).unwrap().token;
    let signed: Vec<u64> = (0..MAX_AUTHENTICATED)
        .map(|_| signed_in(&mut r, &token, 1))
        .collect();
    let (conn, _) = r.admit().unwrap();
    assert_eq!(r.hello(conn, &token, 1).unwrap_err().code, "unavailable");
    // A connection that said hello holds its place until it is gone,
    // whatever became of the reply; then the place is free again.
    r.release(signed[0]);
    assert!(r.hello(conn, &token, 2).is_ok());
    assert_eq!(
        (r.authenticated(), r.unauthenticated()),
        (MAX_AUTHENTICATED, 0)
    );
    // Waiting for their hello: as many as allowed, then none.
    let waiting: Vec<u64> = (0..MAX_UNAUTHENTICATED)
        .map(|_| r.admit().unwrap().0)
        .collect();
    assert!(r.admit().is_none());
    r.release(waiting[0]);
    assert!(r.admit().is_some());
    // Closing: nothing more is let in.
    r.closing = true;
    r.release(waiting[1]);
    assert!(r.admit().is_none());
}

#[test]
fn refused_logins_are_reported_at_most_once_a_minute() {
    let mut r = serving();
    assert_eq!(r.failed(0), Some(1));
    for t in 1..7 {
        assert_eq!(r.failed(t * 1000), None);
    }
    assert_eq!(r.failed(AUTH_FAILED_EVERY_MS), Some(7));
}

#[test]
fn two_starts_never_give_the_same_label() {
    let labels: std::collections::HashSet<String> = (0..1000).map(|_| random_label()).collect();
    assert_eq!(labels.len(), 1000);
    assert!(labels
        .iter()
        .all(|l| l.len() == 32 && l.bytes().all(|b| b.is_ascii_hexdigit())));
}

// A daemon's web server, driven from here: the one-address case with the
// bind injected, and the order of stopping with a hook between the steps.

use std::net::IpAddr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use crew_core::rpc::PROTOCOL_VERSION;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

type Ws = WebSocketStream<tokio::net::TcpStream>;
type Steps = mpsc::UnboundedReceiver<(u8, oneshot::Sender<()>)>;

const V4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

async fn daemon() -> (tempfile::TempDir, crate::daemon::Running) {
    let temp = tempfile::tempdir().unwrap();
    let running = crate::daemon::start(crate::daemon::DaemonOptions {
        workspace: temp.path().to_path_buf(),
        crew_bin: std::env::current_exe().unwrap(),
    })
    .await
    .unwrap();
    (temp, running)
}

fn host_has_ipv6() -> bool {
    std::net::TcpListener::bind((V6, 0)).is_ok()
}

/// Whether the running server holds `[::1]` too.
async fn on_ipv6(daemon: &Daemon) -> bool {
    daemon
        .web
        .lifecycle
        .lock()
        .await
        .as_ref()
        .is_some_and(|server| server.v6.is_some())
}

/// The addresses the running server answers on.
async fn addresses(daemon: &Daemon) -> Vec<IpAddr> {
    if on_ipv6(daemon).await {
        vec![V4, V6]
    } else {
        vec![V4]
    }
}

fn code_of(info: &WebInfo) -> String {
    info.pair_url.split("#pair=").nth(1).unwrap().to_string()
}

/// A WebSocket to the server on `ip`, with the name and origin it answers.
async fn connect(serving: &Serving, ip: IpAddr) -> Ws {
    let mut request = format!("ws://{}/ws", serving.host())
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert("Host", HeaderValue::from_str(&serving.host()).unwrap());
    headers.insert(
        "Origin",
        HeaderValue::from_str(&format!("http://{}", serving.host())).unwrap(),
    );
    headers.insert(
        "Sec-WebSocket-Key",
        HeaderValue::from_str(&generate_key()).unwrap(),
    );
    let stream = tokio::net::TcpStream::connect((ip, serving.port))
        .await
        .unwrap();
    tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap()
        .0
}

/// A request's response, pushes skipped.
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

/// Every text frame up to the close, and the close's code.
async fn until_closed(ws: &mut Ws) -> (Vec<Value>, Option<u16>) {
    let mut frames = Vec::new();
    let code = tokio::time::timeout(Duration::from_secs(5), async {
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
    .expect("closed within 5 s");
    (frames, code)
}

fn bound(ip: IpAddr, port: u16) -> bool {
    match std::net::TcpListener::bind((ip, port)) {
        Ok(_) => false,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => true,
        Err(e) => panic!("binding {ip}:{port}: {e}"),
    }
}

/// What a stop is checked against: a session with a connection signed in
/// and subscribed, a connection on each address that has not said `hello`
/// yet, and a code not spent.
struct Before {
    serving: Serving,
    addresses: Vec<IpAddr>,
    token: String,
    code: String,
    live: Ws,
    waiting: Vec<Ws>,
}

async fn before_the_stop(daemon: &Daemon) -> (Before, Steps) {
    let info = daemon.web.open(daemon, None).await.unwrap();
    let serving = daemon.web.serving().unwrap();
    let addresses = addresses(daemon).await;
    let mut pairing = connect(&serving, V4).await;
    let paired = call(&mut pairing, 1, "web_pair", json!({"code": code_of(&info)})).await;
    let token = paired["result"]["token"].as_str().unwrap().to_string();
    let mut live = connect(&serving, *addresses.last().unwrap()).await;
    assert!(hello(&mut live, &token).await["result"]["web_proof"].is_string());
    assert_eq!(
        call(&mut live, 2, "subscribe", json!({})).await["result"]["subscribed"],
        true
    );
    let mut waiting = Vec::new();
    for ip in &addresses {
        waiting.push(connect(&serving, *ip).await);
    }
    let code = code_of(&daemon.web.open(daemon, None).await.unwrap());
    let (hook, steps) = mpsc::unbounded_channel();
    *daemon.web.stop_hook.lock().unwrap() = Some(hook);
    let before = Before {
        serving,
        addresses,
        token,
        code,
        live,
        waiting,
    };
    (before, steps)
}

async fn step(steps: &mut Steps, n: u8) -> oneshot::Sender<()> {
    let (at, go) = tokio::time::timeout(Duration::from_secs(10), steps.recv())
        .await
        .expect("the next step of stopping")
        .unwrap();
    assert_eq!(at, n, "the steps of stopping come in order");
    go
}

/// The refusal a stopping server gives a `hello` or `web_pair`, which shows
/// no proof.
fn refused_for_stopping(reply: &Value) {
    assert_eq!(reply["error"]["code"], "unavailable", "{reply}");
    assert!(reply.get("result").is_none(), "{reply}");
    assert!(!reply.to_string().contains("web_proof"), "{reply}");
}

/// Walk a stop through its steps (the hook holds it at each) and check what
/// holds there: after 1 the credentials are gone while both listeners are
/// bound, after 2 every connection is closed while they still are, after 3
/// they are free. `shutdown`: the daemon is going, and says so first.
async fn walk_the_stop(daemon: &Daemon, mut before: Before, steps: &mut Steps, shutdown: bool) {
    let port = before.serving.port;
    let go = step(steps, 1).await;
    for ws in &mut before.waiting {
        refused_for_stopping(&hello(ws, &before.token).await);
    }
    for ip in &before.addresses {
        let mut fresh = connect(&before.serving, *ip).await;
        refused_for_stopping(&hello(&mut fresh, &before.token).await);
        let mut fresh = connect(&before.serving, *ip).await;
        refused_for_stopping(&call(&mut fresh, 1, "web_pair", json!({"code": before.code})).await);
    }
    for ip in &before.addresses {
        assert!(bound(*ip, port), "{ip}:{port} was released before step 3");
    }
    go.send(()).unwrap();

    let go = step(steps, 2).await;
    let (frames, code) = until_closed(&mut before.live).await;
    assert_eq!(code, Some(1001), "{frames:?}");
    if shutdown {
        assert!(
            frames.iter().any(|f| f["push"]["kind"] == "shutdown"),
            "{frames:?}"
        );
    }
    assert!(daemon.web.registry().conns.is_empty());
    for ip in &before.addresses {
        assert!(bound(*ip, port), "{ip}:{port} was released before step 3");
    }
    go.send(()).unwrap();

    let go = step(steps, 3).await;
    for ip in &before.addresses {
        assert!(!bound(*ip, port), "{ip}:{port} is still held after step 3");
    }
    assert_eq!(daemon.web.url(), "");
    go.send(()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_close_revokes_then_closes_then_releases_both_listeners() {
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    let (before, mut steps) = before_the_stop(&daemon).await;
    assert_eq!(before.addresses.len(), if host_has_ipv6() { 2 } else { 1 });
    let token = before.token.clone();
    let stop = {
        let daemon = daemon.clone();
        tokio::spawn(async move { daemon.web.stop(&daemon, None).await })
    };
    walk_the_stop(&daemon, before, &mut steps, false).await;
    assert!(stop.await.unwrap());
    // Codes and sessions are gone for good: a new start knows neither.
    *daemon.web.stop_hook.lock().unwrap() = None;
    daemon.web.open(&daemon, None).await.unwrap();
    assert_eq!(daemon.web.registry().sessions(), 0);
    assert_eq!(daemon.web.registry().codes.len(), 1);
    let serving = daemon.web.serving().unwrap();
    let mut ws = connect(&serving, V4).await;
    assert_eq!(
        hello(&mut ws, &token).await["error"]["code"],
        "unauthorized"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_stops_its_web_server_in_the_same_order_before_its_socket() {
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    let socket = daemon.socket.clone();
    let serve = tokio::spawn(running.serve());
    let (before, mut steps) = before_the_stop(&daemon).await;
    daemon.shutdown(false).await;
    let walk = walk_the_stop(&daemon, before, &mut steps, true);
    // The daemon's own socket is there through every step.
    let socket_kept = async {
        loop {
            assert!(socket.exists(), "the socket went before the web server");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::select! {
        _ = walk => {}
        _ = socket_kept => unreachable!(),
    }
    tokio::time::timeout(Duration::from_secs(10), serve)
        .await
        .expect("the daemon stopped")
        .unwrap()
        .unwrap();
    assert!(!socket.exists());
}

/// Binds `127.0.0.1` for real; `[::1]` as `v6` says.
fn binder(v6: Arc<AtomicU8>) -> Binder {
    Arc::new(move |addr: SocketAddr| {
        if addr.is_ipv4() {
            return bind_listener(addr);
        }
        match v6.load(Ordering::SeqCst) {
            NO_IPV6 => Err(io::Error::from_raw_os_error(libc::EADDRNOTAVAIL)),
            TAKEN => Err(io::Error::from_raw_os_error(libc::EADDRINUSE)),
            _ => bind_listener(addr),
        }
    })
}

const NO_IPV6: u8 = 0;
const REAL: u8 = 1;
const TAKEN: u8 = 2;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_one_address_a_later_ipv6_is_kept_and_a_taken_one_stops_the_server() {
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    let v6 = Arc::new(AtomicU8::new(NO_IPV6));
    daemon.web.set_binder(binder(v6.clone()));

    // No IPv6 loopback: served on 127.0.0.1 alone, and still after a tick
    // that finds none.
    daemon.web.open(&daemon, None).await.unwrap();
    let serving = daemon.web.serving().unwrap();
    assert!(!on_ipv6(&daemon).await);
    daemon.web.tick(&daemon).await;
    assert_eq!(daemon.web.serving(), Some(serving.clone()));
    assert!(!on_ipv6(&daemon).await);

    if host_has_ipv6() {
        // [::1] comes up: bound on the next tick and kept, whatever the
        // ticks after it find.
        v6.store(REAL, Ordering::SeqCst);
        daemon.web.tick(&daemon).await;
        assert!(on_ipv6(&daemon).await);
        let mut ws = connect(&serving, V6).await;
        assert_eq!(
            call(&mut ws, 1, "ping", json!({})).await["result"]["pong"],
            true
        );
        v6.store(TAKEN, Ordering::SeqCst);
        daemon.web.tick(&daemon).await;
        assert!(on_ipv6(&daemon).await);
        assert_eq!(daemon.web.serving(), Some(serving.clone()));
        daemon.web.stop(&daemon, None).await;
    } else {
        daemon.web.stop(&daemon, None).await;
    }

    // On 127.0.0.1 alone with a session open, someone takes [::1]: the
    // server stops itself, in the same order, and says why.
    v6.store(NO_IPV6, Ordering::SeqCst);
    let (before, mut steps) = before_the_stop(&daemon).await;
    assert_eq!(before.addresses, vec![V4]);
    let port = before.serving.port;
    let token = before.token.clone();
    v6.store(TAKEN, Ordering::SeqCst);
    let tick = {
        let daemon = daemon.clone();
        tokio::spawn(async move { daemon.web.tick(&daemon).await })
    };
    walk_the_stop(&daemon, before, &mut steps, false).await;
    tick.await.unwrap();
    let stopped = daemon
        .store
        .recent_events(None, 50)
        .unwrap()
        .into_iter()
        .rfind(|e| e.event_type == "web.stopped")
        .expect("web.stopped");
    assert_eq!(stopped.level, crew_core::EventLevel::Warn);
    assert!(
        stopped
            .message
            .contains(&format!("[::1]:{port} was taken by another process")),
        "{}",
        stopped.message
    );
    // Every session was revoked.
    *daemon.web.stop_hook.lock().unwrap() = None;
    v6.store(NO_IPV6, Ordering::SeqCst);
    daemon.web.open(&daemon, None).await.unwrap();
    let mut ws = connect(&daemon.web.serving().unwrap(), V4).await;
    assert_eq!(
        hello(&mut ws, &token).await["error"]["code"],
        "unauthorized"
    );
}
