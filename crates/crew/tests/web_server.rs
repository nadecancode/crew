//! The dashboard server process (`crew web-server`) with real daemons
//! beside it (notes/web-multi.md §multi-tests "Rust"): each test has a
//! runtime directory of its own, the server in the foreground, its control
//! socket spoken to as `crew web` does, and a browser's WebSockets.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use common::*;
use crew_core::rpc::{WebInfo, Workspaces, PROTOCOL_VERSION};
use crew_core::types::WorkspaceState;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

type Ws = WebSocketStream<TcpStream>;

#[test]
fn reviewer_restart_preserves_a_dashboard_whose_workspaces_are_stopped() {
    let s = Session::new();
    let state = running(&s);
    let server = Server::start(&s);
    let mut control = server.control();
    let before = control.open(&state);
    s.ok(&["stop", "-C", &s.workspace.to_string_lossy()]);
    let out = s.crew(&["web", "--restart", "--no-open", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let after: WebInfo = serde_json::from_slice(&out.stdout).unwrap();
    assert_ne!(before.url, after.url);
    let mut replacement = Control::connect(&runtime(&s).join("web.sock"), true);
    let listing = replacement.listing();
    assert_eq!(listing.joined.len(), 1);
    assert_eq!(listing.joined[0].id, crew_core::paths::session_id(&state));
    assert_eq!(listing.joined[0].state, WorkspaceState::Stopped);
}

#[test]
fn reviewer_another_protocol_server_is_kept_until_explicit_restart() {
    let s = Session::new();
    let state = running(&s);
    let mut server = Server::start(&s);
    let mut control = server.control();
    let before = control.open(&state);
    // Discovery sees another protocol. The stand-in process still obeys
    // SIGTERM, which is the replacement path for an incompatible server.
    let path = runtime(&s).join("web.json");
    let mut info: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    info["protocol"] = json!(PROTOCOL_VERSION + 1);
    std::fs::write(&path, serde_json::to_vec(&info).unwrap()).unwrap();
    let refused = s.crew(&["web", "--all", "--no-open", "--json"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("another version of crew"));
    assert!(!server.exited_within(Duration::from_millis(100)));
    assert_eq!(control.call("ping", json!({}))["result"]["pong"], true);
    let replaced = s.ok(&[
        "web",
        "--restart",
        "--no-open",
        "--json",
        "-C",
        &s.workspace.to_string_lossy(),
    ]);
    let after: WebInfo = serde_json::from_str(&replaced).unwrap();
    assert_ne!(before.url, after.url);
    assert!(server.exited_within(Duration::from_secs(5)));
    assert!(!s.snapshot().session.web.is_empty());
}

#[test]
fn reviewer_remove_does_not_fall_back_from_an_explicit_missing_project() {
    let s = Session::new();
    let state = running(&s);
    let server = Server::start(&s);
    let mut control = server.control();
    control.open(&state);
    let out = s
        .command()
        .current_dir(&s.workspace)
        .args(["web", "--remove", "--project", "not-a-running-project"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "removing a missing project must fail"
    );
    assert_eq!(
        control.listing().joined.len(),
        1,
        "the current workspace must remain joined"
    );
}

/// A session with a daemon running in `workspace`, and its state directory.
fn running(s: &Session) -> PathBuf {
    std::fs::create_dir_all(&s.workspace).unwrap();
    s.ok(&["start", "-C", &s.workspace.to_string_lossy()]);
    s.workspace.canonicalize().unwrap().join(".crew")
}

/// A second workspace with a daemon, under the same runtime directory.
fn another(s: &Session, name: &str) -> PathBuf {
    let dir = s.root().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    s.ok(&["start", "-C", &dir.to_string_lossy()]);
    dir.canonicalize().unwrap().join(".crew")
}

fn runtime(s: &Session) -> PathBuf {
    s.root().join("run").join("crew")
}

/// The server, in the foreground; killed when dropped.
struct Server {
    child: Child,
    runtime: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn start(s: &Session) -> Self {
        let child = s
            .command()
            .arg("web-server")
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(s.root().join("web.log")).unwrap())
            .spawn()
            .unwrap();
        let server = Self {
            child,
            runtime: runtime(s),
        };
        // Its web.json, not one a server before it left.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !server.published() {
            assert!(Instant::now() < deadline, "the server did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        server
    }

    /// web.json names this server.
    fn published(&self) -> bool {
        std::fs::read_to_string(self.runtime.join("web.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|info| info["pid"] == self.child.id())
    }

    fn control(&self) -> Control {
        Control::connect(&self.runtime.join("web.sock"), true)
    }

    fn exited_within(&mut self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

/// The control socket, in raw lines.
struct Control {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
}

impl Control {
    fn connect(socket: &Path, hello: bool) -> Self {
        let stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut control = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            next: 1,
        };
        if hello {
            let token = crew_core::paths::read_operator_token(socket)
                .unwrap()
                .unwrap();
            let reply = control.call(
                "hello",
                json!({"role": "operator", "token": token, "client": "test", "protocol": PROTOCOL_VERSION}),
            );
            assert!(reply["result"].is_object(), "{reply}");
        }
        control
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        writeln!(
            self.writer,
            "{}",
            json!({"id": id, "method": method, "params": params})
        )
        .unwrap();
        let mut line = String::new();
        assert_ne!(self.reader.read_line(&mut line).unwrap(), 0, "closed");
        serde_json::from_str(&line).unwrap()
    }

    fn open(&mut self, state_dir: &Path) -> WebInfo {
        let reply = self.call("web_open", json!({"workspace": state_dir}));
        serde_json::from_value(reply["result"].clone()).unwrap_or_else(|_| panic!("{reply}"))
    }

    fn listing(&mut self) -> Workspaces {
        serde_json::from_value(self.call("workspaces", json!({}))["result"].clone()).unwrap()
    }
}

fn host_of(info: &WebInfo) -> String {
    info.url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

fn code_of(info: &WebInfo) -> String {
    let fragment = info.pair_url.split("#pair=").nth(1).unwrap();
    fragment.split('&').next().unwrap().to_string()
}

async fn ws(info: &WebInfo) -> Ws {
    let host = host_of(info);
    let mut request = format!("ws://{host}/ws").into_client_request().unwrap();
    let headers = request.headers_mut();
    headers.insert("Host", HeaderValue::from_str(&host).unwrap());
    headers.insert(
        "Origin",
        HeaderValue::from_str(&format!("http://{host}")).unwrap(),
    );
    headers.insert(
        "Sec-WebSocket-Key",
        HeaderValue::from_str(&generate_key()).unwrap(),
    );
    let stream = TcpStream::connect(("127.0.0.1", info.port)).await.unwrap();
    tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap()
        .0
}

/// A frame by id, pushes kept aside.
async fn call(ws: &mut Ws, id: u64, method: &str, params: Value) -> Value {
    let request = json!({"id": id, "method": method, "params": params});
    ws.send(Message::text(request.to_string())).await.unwrap();
    loop {
        let frame = next(ws).await;
        if frame["id"] == id {
            return frame;
        }
    }
}

async fn next(ws: &mut Ws) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("a frame within 10 s")
            .expect("open")
            .expect("a message");
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

/// The close code the socket ends with.
async fn closed(ws: &mut Ws) -> Option<u16> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(frame))) => return frame.map(|f| u16::from(f.code)),
                None | Some(Err(_)) => return None,
                _ => {}
            }
        }
    })
    .await
    .expect("closed within 10 s")
}

async fn hello(ws: &mut Ws, token: &str, workspace: Option<&str>) -> Value {
    let mut params =
        json!({"role": "operator", "token": token, "client": "web", "protocol": PROTOCOL_VERSION});
    if let Some(w) = workspace {
        params["workspace"] = json!(w);
    }
    call(ws, 1, "hello", params).await
}

/// Pair with the code of `info`: the session token.
async fn pair(info: &WebInfo) -> String {
    let mut socket = ws(info).await;
    let reply = call(&mut socket, 1, "web_pair", json!({"code": code_of(info)})).await;
    reply["result"]["token"]
        .as_str()
        .expect("a token")
        .to_string()
}

/// The next `workspaces` push on an index connection.
async fn pushed(index: &mut Ws) -> Workspaces {
    loop {
        let frame = next(index).await;
        if frame["push"]["kind"] == "workspaces" {
            return serde_json::from_value(frame["push"]["data"].clone()).unwrap();
        }
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn a_workspace_is_shown_through_the_server_and_survives_its_daemon() {
    let s = Session::new();
    let state = running(&s);
    let mut server = Server::start(&s);
    let mut control = server.control();
    let info = control.open(&state);
    let id = crew_core::paths::session_id(&state);
    assert_eq!(info.workspace.as_deref(), Some(id.as_str()));
    assert_eq!(info.joined, vec![id.clone()]);
    assert!(info.pair_url.ends_with(&format!("&w={id}")));
    let listing = control.listing();
    assert_eq!(listing.joined.len(), 1);
    assert_eq!(
        listing.joined[0].state,
        WorkspaceState::Running,
        "{listing:?}"
    );
    let pid = listing.joined[0].daemon.unwrap().pid;
    assert_eq!(
        s.snapshot().session.web,
        info.url,
        "the daemon shows the server's address"
    );
    rt().block_on(async {
        let token = pair(&info).await;
        let mut index = ws(&info).await;
        let reply = hello(&mut index, &token, None).await;
        assert!(reply["result"]["web_proof"].is_string(), "{reply}");
        assert_eq!(reply["result"]["pid"], server.child.id());
        let same = call(&mut index, 2, "workspaces", json!({})).await;
        assert_eq!(same["result"]["rev"], json!(control.listing().rev));
        // A workspace connection: the daemon's hello, with the proof.
        let mut state_conn = ws(&info).await;
        let reply = hello(&mut state_conn, &token, Some(&id)).await;
        assert_eq!(reply["result"]["pid"], pid, "{reply}");
        assert!(reply["result"]["web_proof"].is_string());
        let snapshot = call(&mut state_conn, 2, "snapshot", json!({})).await;
        assert_eq!(snapshot["result"]["session"]["web"], json!(info.url));
        // W7, held by the daemon.
        let refused = call(
            &mut state_conn,
            3,
            "web_attach",
            json!({"url": "http://x/"}),
        )
        .await;
        assert_eq!(refused["error"]["code"], "forbidden", "{refused}");
        // The daemon stops: its connection ends with 4001, the index says
        // stopped, and the page's session stands.
        s.ok(&["stop", "-C", &s.workspace.to_string_lossy()]);
        assert_eq!(closed(&mut state_conn).await, Some(4001));
        loop {
            let listing = pushed(&mut index).await;
            if listing.joined[0].state == WorkspaceState::Stopped {
                break;
            }
        }
        // It starts again: it tells the server, which shows it running.
        s.ok(&["start", "-C", &s.workspace.to_string_lossy()]);
        loop {
            let listing = pushed(&mut index).await;
            if listing.joined[0].state == WorkspaceState::Running {
                assert_ne!(listing.joined[0].daemon.unwrap().pid, pid);
                break;
            }
        }
        let mut again = ws(&info).await;
        let reply = hello(&mut again, &token, Some(&id)).await;
        assert!(reply["result"]["web_proof"].is_string(), "{reply}");
        // Taken out: its connections end with 4002.
        let removed = control.call("web_close", json!({"workspace": id}));
        assert_eq!(removed["result"]["removed"], true, "{removed}");
        assert_eq!(closed(&mut again).await, Some(4002));
        let mut late = ws(&info).await;
        let refused = hello(&mut late, &token, Some(&id)).await;
        assert_eq!(refused["error"]["code"], "not_found", "{refused}");
    });
    // Stopped: the files go, the process ends.
    let stopped = control.call("web_close", json!({}));
    assert_eq!(stopped["result"]["stopped"], true, "{stopped}");
    assert!(server.exited_within(Duration::from_secs(10)));
    for file in ["web.sock", "web.token", "web.json"] {
        assert!(!runtime(&s).join(file).exists(), "{file}");
    }
}

#[test]
fn several_workspaces_one_origin_one_session() {
    let s = Session::new();
    let a = running(&s);
    let b = another(&s, "b");
    let server = Server::start(&s);
    let mut control = server.control();
    let first = control.open(&a);
    let second = control.open(&b);
    assert_eq!(first.url, second.url);
    assert_eq!(control.listing().joined.len(), 2);
    rt().block_on(async {
        let token = pair(&first).await;
        for state in [&a, &b] {
            let id = crew_core::paths::session_id(state);
            let mut conn = ws(&second).await;
            let reply = hello(&mut conn, &token, Some(&id)).await;
            assert!(reply["result"]["web_proof"].is_string(), "{reply}");
            let snapshot = call(&mut conn, 2, "snapshot", json!({})).await;
            assert_eq!(
                snapshot["result"]["session"]["state_dir"],
                json!(state.to_string_lossy())
            );
        }
    });
    s.ok(&["stop", "-C", &s.root().join("b").to_string_lossy()]);
}

#[test]
fn a_session_is_in_joined_or_in_others_never_both() {
    let s = Session::new();
    let a = running(&s);
    let b = another(&s, "b");
    let (a_id, b_id) = (
        crew_core::paths::session_id(&a),
        crew_core::paths::session_id(&b),
    );
    let server = Server::start(&s);
    let mut control = server.control();
    control.open(&a);
    let wait = |control: &mut Control, done: &dyn Fn(&Workspaces) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let listing = control.listing();
            if done(&listing) {
                return listing;
            }
            assert!(Instant::now() < deadline, "{listing:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let listing = wait(&mut control, &|l| l.others.iter().any(|o| o.id == b_id));
    assert!(listing.joined.iter().all(|w| w.id != b_id));
    control.open(&b);
    let listing = control.listing();
    let joined: Vec<&str> = listing.joined.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(joined, [a_id.as_str(), b_id.as_str()]);
    assert!(listing.others.iter().all(|o| o.id != b_id), "{listing:?}");
    let removed = control.call("web_close", json!({"workspace": b_id}));
    assert_eq!(removed["result"]["removed"], true, "{removed}");
    let listing = control.listing();
    assert!(listing.joined.iter().all(|w| w.id != b_id));
    assert!(listing.others.iter().any(|o| o.id == b_id), "{listing:?}");
    s.ok(&["stop", "-C", &s.root().join("b").to_string_lossy()]);
}

/// The socket a daemon's state directory publishes.
fn daemon_socket(state: &Path) -> PathBuf {
    let text = std::fs::read_to_string(state.join("daemon.json")).unwrap();
    let info: Value = serde_json::from_str(&text).unwrap();
    PathBuf::from(info["socket"].as_str().unwrap())
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

/// The server's files, once it stopped.
fn gone(s: &Session) {
    for file in ["web.sock", "web.token", "web.json"] {
        assert!(!runtime(s).join(file).exists(), "{file} is left");
    }
}

#[test]
fn one_server_per_user_its_files_private_and_a_dead_ones_replaced() {
    use std::os::unix::fs::DirBuilderExt;
    let s = Session::new();
    // What a server that died left: a socket nobody listens on, its token,
    // its web.json.
    let run = runtime(&s);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .unwrap();
    drop(std::os::unix::net::UnixListener::bind(run.join("web.sock")).unwrap());
    std::fs::write(run.join("web.token"), "stale-token").unwrap();
    let stale = json!({
        "pid": 999_999, "port": 1, "url": "http://stale.localhost:1/",
        "protocol": PROTOCOL_VERSION, "version": "0", "build": "0"
    });
    std::fs::write(run.join("web.json"), stale.to_string()).unwrap();
    let mut server = Server::start(&s);
    // Replaced: the token is new, and it is the one the socket takes.
    let token = std::fs::read_to_string(run.join("web.token")).unwrap();
    assert_ne!(token, "stale-token");
    let mut control = server.control();
    assert_eq!(control.call("ping", json!({}))["result"]["pong"], true);
    for file in ["web.sock", "web.token", "web.json", "web.lock"] {
        assert_eq!(mode(&run.join(file)), 0o600, "{file}");
    }
    assert_eq!(mode(&run), 0o700);
    // A second server: refused, and the first one stands.
    let second = s.crew(&["web-server"]);
    assert!(!second.status.success());
    assert!(
        stderr(&second).contains("already running"),
        "{}",
        stderr(&second)
    );
    assert!(!server.exited_within(Duration::from_millis(300)));
    assert!(server.published());
    assert_eq!(control.call("ping", json!({}))["result"]["pong"], true);
    assert_eq!(
        std::fs::read_to_string(run.join("web.token")).unwrap(),
        token
    );
}

#[test]
fn the_server_refuses_to_start_in_an_agents_environment() {
    let s = Session::new();
    let identity = s.root().join("identity.json");
    std::fs::write(
        &identity,
        json!({"socket": "/nowhere.sock", "agent_id": "a", "token": "t"}).to_string(),
    )
    .unwrap();
    let out = s
        .command()
        .arg("web-server")
        .env("CREW_IDENTITY", &identity)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("CREW_IDENTITY"), "{}", stderr(&out));
    gone(&s);
}

#[test]
fn the_control_socket_takes_its_own_token_and_only_its_own_calls() {
    let s = Session::new();
    let team = s.team("auth", vec![agent("a", "", &[])], vec![]);
    s.run(&team);
    s.wait_for("the agent's identity", || s.identity_path("a").exists());
    let state = s.workspace.canonicalize().unwrap().join(".crew");
    let server = Server::start(&s);
    let socket = runtime(&s).join("web.sock");
    // Before hello: nothing but ping.
    let mut raw = Control::connect(&socket, false);
    assert_eq!(raw.call("ping", json!({}))["result"]["pong"], true);
    for (method, params) in [
        ("workspaces", json!({})),
        ("web_open", json!({"workspace": state})),
        ("web_rescan", json!({})),
        ("web_close", json!({})),
    ] {
        let reply = raw.call(method, params);
        assert_eq!(reply["error"]["code"], "unauthorized", "{method}: {reply}");
    }
    // No token, an agent's, the daemon's own operator token.
    let identity: Value =
        serde_json::from_str(&std::fs::read_to_string(s.identity_path("a")).unwrap()).unwrap();
    let daemon_token = crew_core::paths::read_operator_token(&daemon_socket(&state))
        .unwrap()
        .unwrap();
    let hellos = [
        json!({"role": "operator", "client": "test", "protocol": PROTOCOL_VERSION}),
        json!({"role": "agent", "agent_id": "a", "token": identity["token"],
               "client": "test", "protocol": PROTOCOL_VERSION}),
        json!({"role": "operator", "token": identity["token"],
               "client": "test", "protocol": PROTOCOL_VERSION}),
        json!({"role": "operator", "token": daemon_token,
               "client": "test", "protocol": PROTOCOL_VERSION}),
    ];
    for params in hellos {
        let mut control = Control::connect(&socket, false);
        let reply = control.call("hello", params.clone());
        assert_eq!(reply["error"]["code"], "unauthorized", "{params}: {reply}");
        let after = control.call("web_open", json!({"workspace": state}));
        assert_eq!(after["error"]["code"], "unauthorized", "{after}");
    }
    // With its own token: a WebSocket's calls are not calls of this socket.
    let mut control = server.control();
    for (method, params) in [
        ("snapshot", json!({})),
        (
            "send",
            json!({"to": "a", "body": "from the control socket"}),
        ),
        ("subscribe", json!({})),
        ("web_pair", json!({"code": "nothing"})),
        ("web_prefs", json!({})),
    ] {
        let reply = control.call(method, params);
        assert_eq!(reply["error"]["code"], "invalid", "{method}: {reply}");
    }
    assert!(control.listing().joined.is_empty(), "nothing was joined");
    assert!(s
        .messages()
        .iter()
        .all(|m| m.body != "from the control socket"));
}

#[test]
fn control_calls_do_nothing_on_any_websocket() {
    let s = Session::new();
    let a = running(&s);
    let b = another(&s, "b");
    let (a_id, b_id) = (
        crew_core::paths::session_id(&a),
        crew_core::paths::session_id(&b),
    );
    let mut server = Server::start(&s);
    let mut control = server.control();
    let info = control.open(&a);
    let calls = [
        ("web_open", json!({"workspace": b})),
        ("web_open", json!({"all": true})),
        ("web_close", json!({"workspace": a_id})),
        ("web_close", json!({})),
        ("web_rescan", json!({})),
    ];
    let deadline = Instant::now() + Duration::from_secs(10);
    while !control.listing().others.iter().any(|o| o.id == b_id) {
        assert!(Instant::now() < deadline, "b among the others");
        std::thread::sleep(Duration::from_millis(50));
    }
    rt().block_on(async {
        let token = pair(&info).await;
        // Before hello.
        let mut fresh = ws(&info).await;
        for (n, (method, params)) in calls.iter().enumerate() {
            let reply = call(&mut fresh, 10 + n as u64, method, params.clone()).await;
            assert_eq!(reply["error"]["code"], "unauthorized", "{method}: {reply}");
        }
        // On the index connection: the server's own `invalid`.
        let mut index = ws(&info).await;
        assert!(hello(&mut index, &token, None).await["result"]["web_proof"].is_string());
        for (n, (method, params)) in calls.iter().enumerate() {
            let reply = call(&mut index, 10 + n as u64, method, params.clone()).await;
            assert_eq!(reply["error"]["code"], "invalid", "{method}: {reply}");
        }
        // On a workspace connection: the daemon's, which has none of them.
        let mut conn = ws(&info).await;
        assert!(hello(&mut conn, &token, Some(&a_id)).await["result"]["web_proof"].is_string());
        for (n, (method, params)) in calls.iter().enumerate() {
            let reply = call(&mut conn, 10 + n as u64, method, params.clone()).await;
            assert_eq!(reply["error"]["code"], "invalid", "{method}: {reply}");
        }
        // The same listing, with the same revision, on the index and here.
        let on_index = call(&mut index, 99, "workspaces", json!({})).await;
        let on_index: Workspaces = serde_json::from_value(on_index["result"].clone()).unwrap();
        assert_eq!(on_index, control.listing());
    });
    // Nothing was joined, taken out or stopped.
    let listing = control.listing();
    let joined: Vec<&str> = listing.joined.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(joined, [a_id.as_str()]);
    assert_eq!(listing.joined[0].state, WorkspaceState::Running);
    assert!(!server.exited_within(Duration::from_millis(100)));
    s.ok(&["stop", "-C", &s.root().join("b").to_string_lossy()]);
}

#[test]
fn with_linger_zero_the_server_exits_when_the_last_daemon_stops() {
    let s = Session::new();
    let config = s.root().join("config").join("crew");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.toml"), "[web]\nlinger_secs = 0\n").unwrap();
    let state = running(&s);
    let mut server = Server::start(&s);
    let mut control = server.control();
    control.open(&state);
    assert!(
        !server.exited_within(Duration::from_millis(500)),
        "a running workspace keeps it"
    );
    s.ok(&["stop", "-C", &s.workspace.to_string_lossy()]);
    assert!(server.exited_within(Duration::from_secs(10)));
    assert!(server.child.try_wait().unwrap().unwrap().success());
    gone(&s);
}

#[test]
fn the_server_exits_when_its_control_socket_is_removed() {
    let s = Session::new();
    let mut server = Server::start(&s);
    std::fs::remove_file(runtime(&s).join("web.sock")).unwrap();
    // Within two housekeeping ticks.
    assert!(server.exited_within(Duration::from_secs(10)));
    gone(&s);
}

#[test]
fn sigterm_stops_the_server_in_order_and_no_daemon() {
    let s = Session::new();
    let state = running(&s);
    let mut server = Server::start(&s);
    let mut control = server.control();
    let info = control.open(&state);
    let daemon = control.listing().joined[0].daemon.unwrap().pid;
    rt().block_on(async {
        let token = pair(&info).await;
        let mut index = ws(&info).await;
        assert!(hello(&mut index, &token, None).await["result"]["web_proof"].is_string());
        // SAFETY: a signal to the child this test started.
        assert_eq!(
            unsafe { libc::kill(server.child.id() as i32, libc::SIGTERM) },
            0
        );
        // The index is told before it is closed.
        let mut shutdown = false;
        let code = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match index.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let frame: Value = serde_json::from_str(text.as_str()).unwrap();
                        shutdown |= frame["push"]["kind"] == "shutdown";
                    }
                    Some(Ok(Message::Close(frame))) => return frame.map(|f| u16::from(f.code)),
                    None | Some(Err(_)) => return None,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
        .expect("closed within 10 s");
        assert!(shutdown, "a shutdown push before the close");
        assert_eq!(code, Some(1001));
    });
    assert!(server.exited_within(Duration::from_secs(10)));
    assert!(server.child.try_wait().unwrap().unwrap().success());
    gone(&s);
    // The daemon runs on, and lets go of the server's address.
    s.wait("the daemon to clear its web line", |snapshot| {
        snapshot.session.web.is_empty()
    });
    let published: Value =
        serde_json::from_str(&std::fs::read_to_string(state.join("daemon.json")).unwrap()).unwrap();
    assert_eq!(published["pid"], daemon, "the same daemon");
}

#[test]
fn a_message_with_a_line_break_closes_the_connection_and_reaches_no_daemon() {
    let s = Session::new();
    let state = running(&s);
    let server = Server::start(&s);
    let mut control = server.control();
    let info = control.open(&state);
    let id = crew_core::paths::session_id(&state);
    rt().block_on(async {
        let token = pair(&info).await;
        let set = |marker: u32| {
            json!({"id": marker, "method": "web_prefs", "params": {"set": {"marker": marker}}})
                .to_string()
        };
        for text in [
            format!("{}\n{}", set(1), set(2)),
            format!("{}\r{}", set(3), set(4)),
            format!("{}\r\n", set(5)),
            format!("{}\n", set(6)),
        ] {
            let mut conn = ws(&info).await;
            assert!(hello(&mut conn, &token, Some(&id)).await["result"]["web_proof"].is_string());
            conn.send(Message::text(text)).await.unwrap();
            assert_eq!(closed(&mut conn).await, Some(1008));
        }
        // The daemon got none of them.
        let mut check = ws(&info).await;
        assert!(hello(&mut check, &token, Some(&id)).await["result"]["web_proof"].is_string());
        let prefs = call(&mut check, 2, "web_prefs", json!({})).await;
        assert_eq!(prefs["result"]["prefs"], json!({}), "{prefs}");
    });
}

#[test]
fn one_failed_dial_beside_a_healthy_watch_is_unavailable_and_changes_no_listing() {
    let s = Session::new();
    let state = running(&s);
    let server = Server::start(&s);
    let mut control = server.control();
    let info = control.open(&state);
    let id = crew_core::paths::session_id(&state);
    let token_file = crew_core::paths::operator_token_path(&daemon_socket(&state));
    rt().block_on(async {
        let token = pair(&info).await;
        let mut index = ws(&info).await;
        assert!(hello(&mut index, &token, None).await["result"]["web_proof"].is_string());
        // Whatever the join still pushes, then quiet.
        while tokio::time::timeout(Duration::from_millis(300), pushed(&mut index))
            .await
            .is_ok()
        {}
        let rev = control.listing().rev;
        // The server cannot read the daemon's token for one dial.
        let aside = token_file.with_extension("aside");
        std::fs::rename(&token_file, &aside).unwrap();
        let mut conn = ws(&info).await;
        let refused = hello(&mut conn, &token, Some(&id)).await;
        std::fs::rename(&aside, &token_file).unwrap();
        assert_eq!(refused["error"]["code"], "unavailable", "{refused}");
        let push = tokio::time::timeout(Duration::from_millis(500), pushed(&mut index)).await;
        assert!(push.is_err(), "nothing is pushed: {push:?}");
        let listing = control.listing();
        assert_eq!(listing.rev, rev);
        assert_eq!(listing.joined[0].state, WorkspaceState::Running);
        // The next one is let in.
        let mut again = ws(&info).await;
        let reply = hello(&mut again, &token, Some(&id)).await;
        assert!(reply["result"]["web_proof"].is_string(), "{reply}");
    });
}

/// A stand-in daemon of protocol 5 in `dir`: a live registry entry (this
/// test's process), the socket the server derives, a token file, and an
/// answer of `protocol` to every `hello`.
fn protocol_5_daemon(s: &Session, dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir.join(".crew")).unwrap();
    let state = dir.canonicalize().unwrap().join(".crew");
    let id = crew_core::paths::session_id(&state);
    let socket = runtime(s).join(format!("{id}.sock"));
    std::fs::create_dir_all(runtime(s)).unwrap();
    let token = socket.with_extension("token");
    std::fs::write(&token, "old-token").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut writer = stream.try_clone().unwrap();
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).unwrap_or(0) > 0 {
                let id = serde_json::from_str::<Value>(&line)
                    .map(|v| v["id"].clone())
                    .unwrap_or_default();
                let _ = writeln!(
                    writer,
                    "{}",
                    json!({"id": id, "error": {"code": "protocol",
                    "message": "client speaks protocol 6, daemon speaks 5"}})
                );
            }
        }
    });
    let registry = s.root().join("state/crew/sessions");
    std::fs::create_dir_all(&registry).unwrap();
    let pid = std::process::id();
    let info = crew_core::paths::DaemonInfo {
        project: Some("old".into()),
        workspace: state.parent().unwrap().to_string_lossy().into_owned(),
        state_dir: state.to_string_lossy().into_owned(),
        socket: socket.to_string_lossy().into_owned(),
        pid,
        started_at: 1,
        pid_start: crew_core::paths::process_start(pid),
        boot_id: crew_core::paths::boot_id(),
    };
    std::fs::write(
        registry.join(format!("{id}.json")),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();
    std::fs::write(
        state.join("daemon.json"),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();
    state
}

#[test]
fn a_daemon_of_protocol_5_is_refused_by_crew_web_and_listed_incompatible_under_all() {
    let s = Session::new();
    let old = s.root().join("old");
    let state = protocol_5_daemon(&s, &old);
    // `crew web` for it: the usual sentence, and no server started.
    let out = s.crew(&["web", "--json", "-C", &old.to_string_lossy()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("protocol"), "{}", stderr(&out));
    assert!(!runtime(&s).join("web.json").exists());
    // `crew web --all`: joined, and shown as incompatible.
    s.ok(&["web", "--all", "--json"]);
    let listing: Workspaces = serde_json::from_str(&s.ok(&["web", "--list", "--json"])).unwrap();
    let entry = listing
        .joined
        .iter()
        .find(|w| w.id == crew_core::paths::session_id(&state))
        .unwrap_or_else(|| panic!("{listing:?}"));
    assert_eq!(entry.state, WorkspaceState::Incompatible, "{entry:?}");
    s.ok(&["web", "--stop"]);
}
