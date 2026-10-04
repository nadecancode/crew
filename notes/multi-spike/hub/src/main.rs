//! Spike for notes/web-multi.md: one loopback server for every running crew
//! session of this user.
//!
//!   hub <port> <label> <pair-code> <session-token> <secret> <verifier> [nocheck]
//!
//! - `GET /ws` with the exact Host and Origin upgrades to a WebSocket.
//! - `web_pair {code}` answers `{token, verifier}`; `hello {token}` answers
//!   with `web_proof` and makes the connection the *index*: `workspaces`
//!   answers the listing, and a `workspaces` push follows every change. Each
//!   listing carries `rev`, which only grows.
//! - `hello {token, workspace}` dials that workspace's daemon, says hello to
//!   it as the operator (the token file is read at that moment), and from
//!   then on passes lines through as they are, both ways.
//! - A message with a line break in it is refused (it would be two requests
//!   on the daemon's socket) unless `nocheck` is given, which shows why.
//! - A workspace is joined with a *generation*. Taking it out revokes the
//!   generation first, refuses pending and new opens, makes every connection
//!   of that generation let go of its daemon, and answers only then.
//! - Nothing is read on a timer. The server holds one connection to each
//!   joined daemon (the *watch*); it learns that a daemon is gone from the
//!   end of its watch and that one started from a rescan request. Both are
//!   *reconciled* against the registry: a watch belongs to one incarnation
//!   of a daemon (pid, start), and the end of a superseded watch is ignored.
//!
//! Spike only: a rescan joins every running session it has not been told to
//! leave alone (the design's `join = "all"`); and the control calls are calls
//! of the index connection (`spike_rescan`, `spike_remove`, `spike_join`,
//! `spike_knobs`, `spike_stats`) where the design has `web_rescan`,
//! `web_close` and `web_open` on a control socket that no WebSocket reaches.
//! The knobs bring back what revision 1 of the design said, to show what was
//! wrong with it, and slow things down so a test can get in between.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crew_core::paths::DaemonInfo;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Notify};
use tokio::task::AbortHandle;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// How many pending opens and connections of one generation still hold, or
/// may still get, a connection to the daemon.
struct Gauge(watch::Sender<usize>);
struct Held(Arc<Gauge>);

impl Gauge {
    fn new() -> Arc<Self> {
        Arc::new(Self(watch::channel(0).0))
    }
    fn hold(self: &Arc<Self>) -> Held {
        self.0.send_modify(|n| *n += 1);
        Held(self.clone())
    }
    async fn drained(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|n| *n == 0).await;
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.0 .0.send_modify(|n| *n -= 1);
    }
}

/// One run of a workspace's daemon, as the registry publishes it.
type Incarnation = (u32, Option<u64>, i64);

fn incarnation(info: &DaemonInfo) -> Incarnation {
    (info.pid, info.pid_start, info.started_at)
}

struct Watch {
    /// Which of the server's watches this is: the end of any other is not this one's.
    epoch: u64,
    of: Incarnation,
    task: tokio::task::JoinHandle<()>,
}

/// One membership of a workspace in the dashboard.
struct Joined {
    generation: u64,
    /// What the registry last said of its daemon.
    last: DaemonInfo,
    running: bool,
    watch: Option<Watch>,
    /// Dials that failed while the registry said the daemon runs.
    retries: u8,
    /// True once this membership was revoked.
    revoked: watch::Sender<bool>,
    open: Arc<Gauge>,
    /// The tasks of the connections admitted under it.
    tasks: Vec<AbortHandle>,
}

#[derive(Default)]
struct Tables {
    joined: BTreeMap<String, Joined>,
    /// Taken out by the operator: not joined again by a rescan.
    removed: BTreeSet<String>,
    /// Memberships removed the way revision 1 read: nobody told their connections.
    zombies: Vec<Joined>,
    generations: u64,
    epochs: u64,
    /// Grows with every change of the listing.
    rev: u64,
    /// The rev last pushed: a listing is pushed when it changed, once, in order.
    pushed: u64,
    scans: u64,
}

impl Tables {
    fn join(&mut self, id: &str, last: DaemonInfo) -> u64 {
        self.generations += 1;
        self.rev += 1;
        let joined = Joined { generation: self.generations, last, running: false, watch: None, retries: 0, revoked: watch::channel(false).0, open: Gauge::new(), tasks: Vec::new() };
        self.joined.insert(id.to_string(), joined);
        self.generations
    }

    fn listing(&self) -> Value {
        let joined: Vec<Value> = self
            .joined
            .iter()
            .map(|(id, j)| json!({"id": id, "project": j.last.project, "workspace": j.last.workspace, "pid": j.last.pid, "running": j.running, "generation": j.generation}))
            .collect();
        json!({"rev": self.rev, "joined": joined})
    }
}

enum Event {
    WatchEnded { id: String, epoch: u64 },
    Reconcile { id: String },
}

#[derive(Default)]
struct Knobs {
    /// A scan takes this long after it read the registry.
    scan_delay_ms: AtomicU64,
    /// The end of a watch is noticed this late.
    watch_end_delay_ms: AtomicU64,
    /// Revision 1: a rescan dials only entries without a watch, and the end
    /// of a watch means stopped.
    v1_reconcile: AtomicBool,
    /// Revision 1 as one could read it: a rescan asked for while one runs is dropped.
    lossy_rescan: AtomicBool,
    /// A connection that is told its membership was revoked holds its daemon this much longer.
    stuck_pipe_ms: AtomicU64,
}

struct Shared {
    host: String,
    pair_code: String,
    token: String,
    secret: String,
    verifier: String,
    newline_check: bool,
    tables: Mutex<Tables>,
    changed: broadcast::Sender<String>,
    events: mpsc::UnboundedSender<Event>,
    rescan_wanted: AtomicBool,
    scanning: AtomicBool,
    wake: Notify,
    knobs: Knobs,
}

fn id_of(info: &DaemonInfo) -> String {
    PathBuf::from(&info.socket)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

async fn registry() -> BTreeMap<String, DaemonInfo> {
    tokio::task::spawn_blocking(crew_core::paths::list_sessions)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|i| (id_of(&i), i))
        .collect()
}

/// Dial a daemon as the operator: its token file is read now.
async fn dial(info: &DaemonInfo, protocol: &Value) -> Option<(BufReader<OwnedReadHalf>, OwnedWriteHalf, String)> {
    let socket = PathBuf::from(&info.socket);
    let operator = crew_core::paths::read_operator_token(&socket).ok().flatten();
    let unix = UnixStream::connect(&socket).await.ok()?;
    let (unix_rx, mut unix_tx) = unix.into_split();
    let mut lines = BufReader::with_capacity(64 * 1024, unix_rx);
    let hello = json!({"id": 0, "method": "hello", "params": {"role": "operator", "token": operator, "client": "web", "protocol": protocol}});
    unix_tx.write_all(format!("{hello}\n").as_bytes()).await.ok()?;
    let mut line = String::new();
    lines.read_line(&mut line).await.ok()?;
    serde_json::from_str::<Value>(&line).ok()?.get("result")?;
    Some((lines, unix_tx, line))
}

impl Shared {
    /// Push the listing if it changed since the last push. Under the table's
    /// lock, so pushes leave in the order of their revs.
    fn publish(&self) {
        let mut t = self.tables.lock().unwrap();
        if t.rev == t.pushed {
            return;
        }
        t.pushed = t.rev;
        let listing = t.listing();
        let _ = self.changed.send(format!(r#"{{"push":{{"kind":"workspaces","data":{listing}}}}}"#));
    }

    /// A rescan was asked for. The flag is cleared before a scan reads the
    /// registry, so a request made while one runs is followed by another.
    fn rescan(&self) {
        if self.knobs.lossy_rescan.load(Ordering::SeqCst) && self.scanning.load(Ordering::SeqCst) {
            return;
        }
        self.rescan_wanted.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Bring one membership in line with what the registry publishes now.
    async fn reconcile(self: &Arc<Self>, id: &str, published: Option<DaemonInfo>) {
        let v1 = self.knobs.v1_reconcile.load(Ordering::SeqCst);
        let (to_dial, generation) = {
            let mut guard = self.tables.lock().unwrap();
            let t = &mut *guard;
            let Some(j) = t.joined.get_mut(id) else { return };
            let generation = j.generation;
            match (&published, &j.watch) {
                // Its watch is on the daemon the registry publishes.
                (Some(info), Some(w)) if w.of == incarnation(info) => (None, generation),
                (Some(_), Some(_)) if v1 => (None, generation),
                // No watch, or one on an earlier daemon: that one is
                // superseded, whatever it still has to say.
                (Some(info), _) => {
                    j.watch = None;
                    (Some(info.clone()), generation)
                }
                (None, _) => {
                    if let Some(w) = j.watch.take() {
                        w.task.abort();
                    }
                    j.retries = 0;
                    if j.running {
                        j.running = false;
                        t.rev += 1;
                    }
                    (None, generation)
                }
            }
        };
        let Some(info) = to_dial else { return self.publish() };
        let dialled = dial(&info, &json!(5)).await;
        let mut guard = self.tables.lock().unwrap();
        let t = &mut *guard;
        let Some(j) = t.joined.get_mut(id) else { return };
        // The membership it was dialled for is gone, or another dial got there first.
        if j.generation != generation || j.watch.as_ref().is_some_and(|w| w.of == incarnation(&info)) {
            return;
        }
        match dialled {
            Some((lines, tx, _)) => {
                t.epochs += 1;
                let epoch = t.epochs;
                let task = tokio::spawn(watching(self.clone(), id.to_string(), epoch, lines, tx));
                j.watch = Some(Watch { epoch, of: incarnation(&info), task });
                j.last = info;
                j.running = true;
                j.retries = 0;
                t.rev += 1;
            }
            None => {
                // Published but not reachable (it is going, or coming): look
                // again shortly, a few times; the registry decides.
                if j.running {
                    j.running = false;
                    t.rev += 1;
                }
                if j.retries < 5 {
                    j.retries += 1;
                    let (events, id) = (self.events.clone(), id.to_string());
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        let _ = events.send(Event::Reconcile { id });
                    });
                }
            }
        }
        drop(guard);
        self.publish();
    }

    /// One scan: the registry, read once, and every membership against it.
    async fn scan(self: &Arc<Self>) {
        self.scanning.store(true, Ordering::SeqCst);
        let published = registry().await;
        tokio::time::sleep(Duration::from_millis(self.knobs.scan_delay_ms.load(Ordering::SeqCst))).await;
        let ids: Vec<String> = {
            let mut guard = self.tables.lock().unwrap();
            let t = &mut *guard;
            t.scans += 1;
            for (id, info) in &published {
                if !t.joined.contains_key(id) && !t.removed.contains(id) {
                    t.join(id, info.clone());
                }
            }
            t.joined.keys().cloned().collect()
        };
        for id in ids {
            self.reconcile(&id, published.get(&id).cloned()).await;
        }
        self.scanning.store(false, Ordering::SeqCst);
    }
}

/// The watch on one daemon: it says nothing and is told nothing; its end is the news.
async fn watching(shared: Arc<Shared>, id: String, epoch: u64, mut lines: BufReader<OwnedReadHalf>, _tx: OwnedWriteHalf) {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match lines.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    tokio::time::sleep(Duration::from_millis(shared.knobs.watch_end_delay_ms.load(Ordering::SeqCst))).await;
    let _ = shared.events.send(Event::WatchEnded { id, epoch });
}

/// The one task that reconciles: rescans and watch ends, one after another.
async fn reconciler(shared: Arc<Shared>, mut events: mpsc::UnboundedReceiver<Event>) {
    loop {
        tokio::select! {
            event = events.recv() => match event {
                None => return,
                Some(Event::Reconcile { id }) => {
                    let published = registry().await.remove(&id);
                    shared.reconcile(&id, published).await;
                }
                Some(Event::WatchEnded { id, epoch }) => {
                    if shared.knobs.v1_reconcile.load(Ordering::SeqCst) {
                        // Revision 1: the end of a watch means stopped.
                        let mut guard = shared.tables.lock().unwrap();
                        let t = &mut *guard;
                        if let Some(j) = t.joined.get_mut(&id) {
                            j.watch = None;
                            j.running = false;
                            t.rev += 1;
                        }
                        drop(guard);
                        shared.publish();
                        continue;
                    }
                    let current = {
                        let mut t = shared.tables.lock().unwrap();
                        match t.joined.get_mut(&id) {
                            Some(j) if j.watch.as_ref().is_some_and(|w| w.epoch == epoch) => {
                                j.watch = None;
                                true
                            }
                            // The end of a watch that was superseded: not news.
                            _ => false,
                        }
                    };
                    if current {
                        let published = registry().await.remove(&id);
                        shared.reconcile(&id, published).await;
                    }
                }
            },
            _ = shared.wake.notified() => {}
        }
        while shared.rescan_wanted.swap(false, Ordering::SeqCst) {
            shared.scan().await;
        }
    }
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
    let (events, events_rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        host,
        pair_code,
        token,
        secret,
        verifier,
        newline_check,
        tables: Mutex::new(Tables::default()),
        changed,
        events,
        rescan_wanted: AtomicBool::new(true), // the first scan: what runs now
        scanning: AtomicBool::new(false),
        wake: Notify::new(),
        knobs: Knobs::default(),
    });
    eprintln!("hub on http://{}/", shared.host);
    shared.wake.notify_one();
    tokio::spawn(reconciler(shared.clone(), events_rx));

    let accept = |listener: TcpListener, shared: Arc<Shared>| async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).ok();
            let shared = shared.clone();
            // The task is handed its own handle, so a membership can abort
            // the connections that were admitted under it.
            let (me_tx, me_rx) = oneshot::channel();
            let task = tokio::spawn(async move {
                let Ok(me) = me_rx.await else { return };
                if let Err(e) = serve(shared, stream, me).await {
                    eprintln!("conn: {e}");
                }
            });
            let _ = me_tx.send(task.abort_handle());
        }
    };
    tokio::spawn(accept(v6, shared.clone()));
    accept(v4, shared).await;
}

async fn respond(stream: &mut TcpStream, status: &str) -> std::io::Result<()> {
    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    stream.shutdown().await
}

async fn serve(shared: Arc<Shared>, mut stream: TcpStream, me: AbortHandle) -> Res<()> {
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
    session(shared, ws, me).await
}

fn error(id: &Value, code: &str, message: &str) -> Message {
    Message::text(json!({"id": id, "error": {"code": code, "message": message}}).to_string())
}

fn close(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.into() }))
}

/// An open that passed the table: it counts for its generation from here on.
struct Open {
    info: DaemonInfo,
    revoked: watch::Receiver<bool>,
    held: Held,
    hello_id: Value,
    protocol: Value,
    /// Spike only: wait this long before dialling, so a test can remove the
    /// workspace while the open is under way.
    delay: Duration,
}

const GONE: &str = "that workspace was taken out of this dashboard";

/// The control calls the spike takes on the index connection.
async fn control(shared: &Arc<Shared>, v: &Value) -> Option<Value> {
    let ws_id = v["params"]["id"].as_str().unwrap_or_default().to_string();
    let ok = |result: Value| Some(json!({"id": v["id"], "result": result}));
    let no = |code: &str, message: &str| Some(json!({"id": v["id"], "error": {"code": code, "message": message}}));
    match v["method"].as_str()? {
        "spike_rescan" => {
            shared.rescan();
            ok(json!({"asked": true}))
        }
        "spike_stats" => ok(json!({"scans": shared.tables.lock().unwrap().scans})),
        "spike_knobs" => {
            let p = &v["params"];
            if let Some(ms) = p["scan_delay_ms"].as_u64() {
                shared.knobs.scan_delay_ms.store(ms, Ordering::SeqCst);
            }
            if let Some(ms) = p["watch_end_delay_ms"].as_u64() {
                shared.knobs.watch_end_delay_ms.store(ms, Ordering::SeqCst);
            }
            if let Some(on) = p["v1_reconcile"].as_bool() {
                shared.knobs.v1_reconcile.store(on, Ordering::SeqCst);
            }
            if let Some(on) = p["lossy_rescan"].as_bool() {
                shared.knobs.lossy_rescan.store(on, Ordering::SeqCst);
            }
            if let Some(ms) = p["stuck_pipe_ms"].as_u64() {
                shared.knobs.stuck_pipe_ms.store(ms, Ordering::SeqCst);
            }
            ok(json!({}))
        }
        "spike_remove" => {
            // From here on a `hello` for it finds nothing, and a rescan leaves it alone.
            let taken = {
                let mut t = shared.tables.lock().unwrap();
                t.removed.insert(ws_id.clone());
                let taken = t.joined.remove(&ws_id);
                if taken.is_some() {
                    t.rev += 1;
                }
                taken
            };
            let reply = match taken {
                None => no("not_found", "not in this dashboard"),
                Some(mut j) if v["params"]["revoke"].as_bool().unwrap_or(true) => {
                    // Revoke, then wait until nothing of that generation
                    // holds the daemon; answer then.
                    j.revoked.send_replace(true);
                    if let Some(w) = j.watch.take() {
                        w.task.abort();
                    }
                    // A connection that has not let go within a second is
                    // aborted. Asking a task to stop does not stop it: its
                    // future, and the daemon's side that future owns, is
                    // dropped when the runtime gets to it. So the count is
                    // waited for again, and the answer follows the drop.
                    let mut aborted = 0;
                    if tokio::time::timeout(Duration::from_secs(1), j.open.drained()).await.is_err() {
                        for task in j.tasks.iter().filter(|t| !t.is_finished()) {
                            task.abort();
                            aborted += 1;
                        }
                    }
                    let drained = tokio::time::timeout(Duration::from_secs(5), j.open.drained()).await.is_ok();
                    ok(json!({"removed": true, "drained": drained, "aborted": aborted, "generation": j.generation}))
                }
                Some(j) => {
                    let generation = j.generation;
                    shared.tables.lock().unwrap().zombies.push(j);
                    ok(json!({"removed": true, "drained": false, "generation": generation}))
                }
            };
            shared.publish();
            reply
        }
        "spike_join" => {
            let published = registry().await.remove(&ws_id);
            let Some(info) = published.clone() else { return no("unavailable", "that workspace is not running") };
            let generation = {
                let mut t = shared.tables.lock().unwrap();
                t.removed.remove(&ws_id);
                t.join(&ws_id, info)
            };
            shared.reconcile(&ws_id, published).await;
            ok(json!({"generation": generation}))
        }
        _ => None,
    }
}

async fn session(shared: Arc<Shared>, ws: WebSocketStream<TcpStream>, me: AbortHandle) -> Res<()> {
    let (mut tx, mut rx) = ws.split();
    // Before hello: `ping`, `web_pair`, `hello`.
    let open = loop {
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
                let Some(ws_id) = v["params"]["workspace"].as_str() else {
                    tx.send(Message::text(json!({"id": id, "result": {"role": "operator", "protocol": v["params"]["protocol"], "version": "spike", "pid": std::process::id(), "hub": true, "web_proof": shared.secret}}).to_string())).await?;
                    break None;
                };
                // Admission, under the table's lock: the open is counted for
                // the generation it found before anything is dialled.
                let admitted = {
                    let mut t = shared.tables.lock().unwrap();
                    match t.joined.get_mut(ws_id) {
                        None => Err(("not_found", "that workspace is not in this dashboard")),
                        Some(j) if !j.running => Err(("unavailable", "that workspace is not running")),
                        Some(j) => {
                            j.tasks.push(me.clone());
                            Ok((j.last.clone(), j.revoked.subscribe(), j.open.hold()))
                        }
                    }
                };
                match admitted {
                    Err((code, message)) => tx.send(error(&id, code, message)).await?,
                    // Spike only: this open is answered with an error of the test's choosing.
                    Ok(_) if v["params"]["spike_refuse"].is_string() => tx.send(error(&id, v["params"]["spike_refuse"].as_str().unwrap_or_default(), "as the test asked")).await?,
                    Ok(_) if v["params"]["spike_fail_dial"].as_bool() == Some(true) => {
                        // A dial that fails while the watch is healthy. The
                        // server looks at that workspace again; nothing changes.
                        let _ = shared.events.send(Event::Reconcile { id: ws_id.to_string() });
                        tx.send(error(&id, "unavailable", "that workspace did not answer")).await?
                    }
                    Ok((info, revoked, held)) => {
                        let delay = Duration::from_millis(v["params"]["spike_delay_ms"].as_u64().unwrap_or(0));
                        break Some(Open { info, revoked, held, hello_id: id, protocol: v["params"]["protocol"].clone(), delay });
                    }
                }
            }
            _ => tx.send(error(&id, "unauthorized", "say hello first")).await?,
        }
    };

    let Some(open) = open else {
        // The index: the listing, and a push when it changes.
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
                        Some("workspaces") => json!({"id": v["id"], "result": shared.tables.lock().unwrap().listing()}),
                        Some("ping") => json!({"id": v["id"], "result": {"pong": true}}),
                        _ => match control(&shared, &v).await {
                            Some(reply) => reply,
                            None => json!({"id": v["id"], "error": {"code": "invalid", "message": "not a call of the index"}}),
                        },
                    };
                    tx.send(Message::text(reply.to_string())).await?;
                }
            }
        }
        return Ok(());
    };

    let Open { info, mut revoked, held, hello_id, protocol, delay } = open;
    // One daemon connection for this WebSocket. A removal cancels the dial:
    // nothing waits for a daemon to answer.
    let dialled = tokio::select! {
        dialled = async { tokio::time::sleep(delay).await; dial(&info, &protocol).await } => Some(dialled),
        _ = revoked.wait_for(|r| *r) => None,
    };
    let Some(dialled) = dialled else {
        tx.send(error(&hello_id, "not_found", GONE)).await?;
        drop(held);
        return Ok(());
    };
    let Some((mut lines, mut unix_tx, line)) = dialled else {
        let _ = shared.events.send(Event::Reconcile { id: id_of(&info) });
        tx.send(error(&hello_id, "unavailable", "that workspace did not answer")).await?;
        return Ok(());
    };
    // Commit: the connection is the workspace's only if its generation still stands.
    if *revoked.borrow() {
        drop((lines, unix_tx));
        tx.send(error(&hello_id, "not_found", GONE)).await?;
        return Ok(());
    }
    let mut reply: Value = serde_json::from_str(&line)?;
    reply["id"] = hello_id;
    if let Some(result) = reply.get_mut("result").and_then(Value::as_object_mut) {
        result.insert("web_proof".into(), shared.secret.clone().into());
        result.insert("workspace".into(), id_of(&info).into());
    }
    tx.send(Message::text(reply.to_string())).await?;

    // From here on: lines as they are, both ways.
    enum Step {
        Daemon(std::io::Result<usize>),
        Browser(Option<Result<Message, WsError>>),
        Revoked,
    }
    let mut buf = Vec::with_capacity(4096);
    loop {
        let step = tokio::select! {
            biased;
            _ = revoked.wait_for(|r| *r) => Step::Revoked,
            n = lines.read_until(b'\n', &mut buf) => Step::Daemon(n),
            msg = rx.next() => Step::Browser(msg),
        };
        match step {
            Step::Revoked => {
                // Let go of the daemon first; the goodbye to the browser is
                // not what the removal waits for. (The knob: a connection
                // that does not let go.)
                tokio::time::sleep(Duration::from_millis(shared.knobs.stuck_pipe_ms.load(Ordering::SeqCst))).await;
                drop((lines, unix_tx));
                drop(held);
                tx.send(close(4002, GONE)).await?;
                return Ok(());
            }
            Step::Daemon(n) => {
                if n? == 0 {
                    // This connection ended on the daemon's side. Whether the
                    // daemon is gone is the listing's to say.
                    let _ = shared.events.send(Event::Reconcile { id: id_of(&info) });
                    tx.send(close(4001, "the workspace's connection ended")).await?;
                    break;
                }
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                }
                let text = String::from_utf8(std::mem::take(&mut buf))?;
                tx.send(Message::text(text)).await?;
            }
            Step::Browser(msg) => {
                let Some(msg) = msg else { break };
                match msg? {
                    Message::Text(text) => {
                        if shared.newline_check && text.as_str().bytes().any(|b| b == b'\n' || b == b'\r') {
                            tx.send(close(1008, "one request per message, on one line")).await?;
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
