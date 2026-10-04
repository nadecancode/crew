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
        let deadline = Instant::now() + Duration::from_secs(10);
        while !server.runtime.join("web.json").exists() {
            assert!(Instant::now() < deadline, "the server did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        server
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
