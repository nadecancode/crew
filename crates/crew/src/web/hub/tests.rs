//! The hub against stand-in daemons (notes/web-multi.md §multi-tests
//! "Rust"): a registry the test writes, and daemons over in-memory pipes that
//! answer as each test says, recording every line they receive and, with its
//! time, the moment the server let go of each connection. Where only the hub
//! is concerned time is frozen; where a browser's WebSockets are, a server
//! runs in this process on real loopback sockets, in real time.

use super::*;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::task::{Context, Poll};

use crew_core::rpc::{
    HelloInfo, Role, CLOSE_WORKSPACE_ENDED, CLOSE_WORKSPACE_REMOVED, PROTOCOL_VERSION,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader, DuplexStream, ReadBuf};
use tokio::sync::{broadcast, Semaphore};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use super::super::daemons::{say_hello, BoxFuture};
use super::super::process::{Server, Settings};
use super::super::{Serving, Web};

// ---------------------------------------------------------------------------
// Stand-in daemons and their registry.

/// How a stand-in daemon takes a connection and answers its `hello`.
#[derive(Clone, Debug)]
enum Answer {
    Ok,
    /// `protocol`: another version of crew.
    Protocol,
    Unauthorized,
    /// Takes the connection and never answers.
    Silent,
    /// Answers after this long.
    After(Duration),
    /// Takes no connection.
    Refuse,
}

/// Something a stand-in daemon saw.
#[derive(Clone, Debug)]
struct Seen {
    pid: u32,
    /// The connection, numbered by the fake.
    conn: u64,
    /// Dialled for a browser.
    web: bool,
    what: What,
    at: Instant,
}

#[derive(Clone, Debug)]
enum What {
    /// The server dialled it (whether or not it took the connection).
    Dialled,
    /// A line it received.
    Line(Value),
    /// The server let go of the connection: its side was dropped.
    LetGo,
}

type Log = Arc<Mutex<Vec<Seen>>>;

fn note(log: &Log, pid: u32, conn: u64, web: bool, what: What) {
    log.lock().unwrap().push(Seen {
        pid,
        conn,
        web,
        what,
        at: Instant::now(),
    });
}

/// One run of a stand-in daemon.
struct Stand {
    info: DaemonInfo,
    answer: Mutex<Answer>,
    /// This many dials are refused before `answer` applies again.
    refuse: AtomicUsize,
    /// What `summary` answers.
    summary: Mutex<WorkspaceSummary>,
    /// Ends its connections: every one (`true`), or the browsers'.
    kill: broadcast::Sender<bool>,
    /// Lines for the connections that watch its summary.
    pushes: broadcast::Sender<String>,
    /// Bytes written by a `flood`.
    flooded: AtomicU64,
}

impl Stand {
    fn answer(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn refuse(&self, dials: usize) {
        self.refuse.store(dials, Ordering::SeqCst);
    }

    fn set_summary(&self, summary: WorkspaceSummary) {
        *self.summary.lock().unwrap() = summary;
    }

    /// Every connection ends, as when the daemon exits.
    fn end_all(&self) {
        let _ = self.kill.send(true);
    }

    /// The browsers' connections end; the watch stands.
    fn end_web(&self) {
        let _ = self.kill.send(false);
    }

    fn push_summary(&self, summary: &WorkspaceSummary) {
        let line = json!({"push": {"kind": "summary", "data": summary}}).to_string();
        let _ = self.pushes.send(line);
    }

    /// One connection, as a daemon serves it: every line is recorded;
    /// `hello`, `web_attach`, `summary` and `ping` are answered, `flood`
    /// writes until the server stops reading, the rest is only recorded.
    async fn serve(self: Arc<Self>, stream: DuplexStream, conn: u64, web: bool, log: Log) {
        let pid = self.info.pid;
        let mut kill = self.kill.subscribe();
        let mut pushes = self.pushes.subscribe();
        let (read, mut write) = tokio::io::split(stream);
        let mut lines = BufReader::new(read).lines();
        let mut watching = false;
        loop {
            let line = tokio::select! {
                all = kill.recv() => match all {
                    Ok(false) if !web => continue,
                    _ => return,
                },
                push = pushes.recv(), if watching => {
                    if let Ok(line) = push {
                        if write_line(&mut write, &line).await.is_err() {
                            return;
                        }
                    }
                    continue;
                }
                line = lines.next_line() => match line {
                    Ok(Some(line)) => line,
                    _ => return,
                },
            };
            let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            note(&log, pid, conn, web, What::Line(request.clone()));
            let id = request["id"].clone();
            let reply = match request["method"].as_str() {
                Some("hello") => {
                    let answer = self.answer.lock().unwrap().clone();
                    match answer {
                        Answer::Silent => continue,
                        Answer::After(wait) => {
                            tokio::time::sleep(wait).await;
                            json!({"id": id, "result": self.hello_info()})
                        }
                        Answer::Protocol => json!({"id": id, "error": {
                            "code": "protocol",
                            "message": "client speaks protocol 6, the daemon speaks 5"}}),
                        Answer::Unauthorized => json!({"id": id, "error": {
                            "code": "unauthorized", "message": "that is not the operator's token"}}),
                        Answer::Ok | Answer::Refuse => {
                            json!({"id": id, "result": self.hello_info()})
                        }
                    }
                }
                Some("web_attach") => json!({"id": id, "result": {"attached": true}}),
                Some("summary") => {
                    if request["params"]["watch"] == json!(true) {
                        watching = true;
                        pushes = self.pushes.subscribe();
                    }
                    let summary = self.summary.lock().unwrap().clone();
                    json!({"id": id, "result": summary})
                }
                Some("ping") => json!({"id": id, "result": {"pong": true}}),
                Some("flood") => {
                    let line = json!({"push": {"kind": "noise", "data": "x".repeat(32 * 1024)}})
                        .to_string();
                    while write_line(&mut write, &line).await.is_ok() {
                        self.flooded
                            .fetch_add(line.len() as u64 + 1, Ordering::SeqCst);
                    }
                    return;
                }
                _ => continue,
            };
            if write_line(&mut write, &reply.to_string()).await.is_err() {
                return;
            }
        }
    }

    fn hello_info(&self) -> HelloInfo {
        HelloInfo {
            role: Role::Operator,
            agent_id: None,
            project: self.info.project.clone(),
            version: "stand-in".into(),
            protocol: PROTOCOL_VERSION,
            pid: self.info.pid,
            web_proof: None,
        }
    }
}

async fn write_line<W: AsyncWrite + Unpin>(write: &mut W, line: &str) -> std::io::Result<()> {
    write.write_all(line.as_bytes()).await?;
    write.write_all(b"\n").await
}

/// The server's side of a connection to a stand-in: when both halves are
/// dropped, the log says so, with the time.
struct ServerSide {
    log: Log,
    pid: u32,
    conn: u64,
    web: bool,
}

impl Drop for ServerSide {
    fn drop(&mut self) {
        note(&self.log, self.pid, self.conn, self.web, What::LetGo);
    }
}

struct Tracked<T> {
    io: T,
    _side: Arc<ServerSide>,
}

impl<T: AsyncRead + Unpin> AsyncRead for Tracked<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Tracked<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

/// The registry and the daemons, as the test makes them.
#[derive(Default)]
struct Fake {
    /// What the registry publishes: live entries only, as `list_sessions`
    /// drops the dead.
    published: Mutex<Vec<DaemonInfo>>,
    /// Every run started, by pid.
    stands: Mutex<HashMap<u32, Arc<Stand>>>,
    /// State directories whose entry names a socket the check refuses.
    misplaced: Mutex<BTreeSet<String>>,
    log: Log,
    conns: AtomicU64,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A daemon of workspace `name` starts as `pid` and publishes its entry
    /// (in place of an earlier run's, which keeps its connections).
    fn start(&self, name: &str, pid: u32) -> Arc<Stand> {
        let info = entry(name, pid);
        let stand = Arc::new(Stand {
            info: info.clone(),
            answer: Mutex::new(Answer::Ok),
            refuse: AtomicUsize::new(0),
            summary: Mutex::new(WorkspaceSummary {
                rev: 1,
                agents: 1,
                ..WorkspaceSummary::default()
            }),
            kill: broadcast::channel(16).0,
            pushes: broadcast::channel(4096).0,
            flooded: AtomicU64::new(0),
        });
        self.stands.lock().unwrap().insert(pid, stand.clone());
        let mut published = self.published.lock().unwrap();
        published.retain(|i| i.state_dir != info.state_dir);
        published.push(info);
        stand
    }

    /// Its entry is withdrawn; its connections stand.
    fn withdraw(&self, name: &str) {
        let dir = dir(name).to_string_lossy().into_owned();
        self.published
            .lock()
            .unwrap()
            .retain(|i| i.state_dir != dir);
    }

    /// Every run of it exits: the entry is withdrawn, the connections end.
    fn stop(&self, name: &str) {
        self.withdraw(name);
        let dir = dir(name).to_string_lossy().into_owned();
        for stand in self.stands.lock().unwrap().values() {
            if stand.info.state_dir == dir {
                stand.end_all();
            }
        }
    }

    fn misplace(&self, name: &str) {
        self.misplaced
            .lock()
            .unwrap()
            .insert(dir(name).to_string_lossy().into_owned());
    }

    fn seen(&self) -> Vec<Seen> {
        self.log.lock().unwrap().clone()
    }

    /// When the server dialled `pid`, for a browser (`web`) or for itself.
    fn dials(&self, pid: u32, web: bool) -> Vec<Instant> {
        self.seen()
            .into_iter()
            .filter(|s| s.pid == pid && s.web == web && matches!(s.what, What::Dialled))
            .map(|s| s.at)
            .collect()
    }

    /// The connections the server dialled to `pid`.
    fn conns(&self, pid: u32, web: bool) -> Vec<u64> {
        self.seen()
            .into_iter()
            .filter(|s| s.pid == pid && s.web == web && matches!(s.what, What::Dialled))
            .map(|s| s.conn)
            .collect()
    }

    /// Every line `pid` received, with its connection and time.
    fn lines(&self, pid: u32) -> Vec<(u64, Value, Instant)> {
        self.seen()
            .into_iter()
            .filter(|s| s.pid == pid)
            .filter_map(|s| match s.what {
                What::Line(line) => Some((s.conn, line, s.at)),
                _ => None,
            })
            .collect()
    }

    fn methods(&self, pid: u32, conn: u64) -> Vec<String> {
        self.lines(pid)
            .into_iter()
            .filter(|(c, _, _)| *c == conn)
            .map(|(_, line, _)| line["method"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// When the server let go of `conn`.
    fn let_go(&self, conn: u64) -> Option<Instant> {
        self.seen()
            .into_iter()
            .find(|s| s.conn == conn && matches!(s.what, What::LetGo))
            .map(|s| s.at)
    }
}

impl Daemons for Fake {
    fn sessions(&self) -> BoxFuture<'_, Vec<DaemonInfo>> {
        let published = self.published.lock().unwrap().clone();
        Box::pin(async move { published })
    }

    fn check(&self, info: &DaemonInfo) -> Result<(), String> {
        if self.misplaced.lock().unwrap().contains(&info.state_dir) {
            return Err(format!(
                "its registry entry names the socket {}, not the one derived for it: refused",
                info.socket
            ));
        }
        Ok(())
    }

    fn dial(&self, info: &DaemonInfo, web: bool) -> BoxFuture<'static, Result<Dialled, DialError>> {
        let stand = self.stands.lock().unwrap().get(&info.pid).cloned();
        let log = self.log.clone();
        let conn = self.conns.fetch_add(1, Ordering::SeqCst) + 1;
        let pid = info.pid;
        Box::pin(async move {
            note(&log, pid, conn, web, What::Dialled);
            let Some(stand) = stand else {
                return Err(DialError::NoAnswer("connecting: no such socket".into()));
            };
            let refused = stand
                .refuse
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            let answer = stand.answer.lock().unwrap().clone();
            if refused || matches!(answer, Answer::Refuse) {
                return Err(DialError::NoAnswer("connecting: connection refused".into()));
            }
            let (ours, theirs) = tokio::io::duplex(64 * 1024);
            tokio::spawn(stand.serve(theirs, conn, web, log.clone()));
            let (read, write) = tokio::io::split(ours);
            let side = Arc::new(ServerSide {
                log,
                pid,
                conn,
                web,
            });
            say_hello(
                Box::pin(Tracked {
                    io: read,
                    _side: side.clone(),
                }),
                Box::pin(Tracked {
                    io: write,
                    _side: side,
                }),
                format!("token-{pid}"),
                web,
            )
            .await
        })
    }
}

fn dir(name: &str) -> PathBuf {
    PathBuf::from(format!("/crew-hub-test/{name}/.crew"))
}

fn dir_arg(name: &str) -> String {
    dir(name).to_string_lossy().into_owned()
}

fn id(name: &str) -> String {
    paths::session_id(&dir(name))
}

fn entry(name: &str, pid: u32) -> DaemonInfo {
    DaemonInfo {
        project: Some(name.to_string()),
        workspace: format!("/crew-hub-test/{name}"),
        state_dir: dir_arg(name),
        socket: format!("/run/crew-hub-test/{}.sock", id(name)),
        pid,
        started_at: 1_000 * pid as i64,
        pid_start: Some(pid as u64),
        boot_id: None,
    }
}

fn summary(rev: u64, agents: u32) -> WorkspaceSummary {
    WorkspaceSummary {
        rev,
        agents,
        ..WorkspaceSummary::default()
    }
}

// ---------------------------------------------------------------------------
// The hub.

const URL: &str = "http://hubtest.localhost:23999/";

fn hub(fake: &Arc<Fake>, join_all: bool) -> Arc<Hub> {
    let hub = Hub::new(fake.clone(), join_all);
    hub.start(URL.into());
    hub
}

fn member(listing: &Workspaces, name: &str) -> Option<WorkspaceEntry> {
    listing.joined.iter().find(|e| e.id == id(name)).cloned()
}

fn state(hub: &Hub, name: &str) -> Option<WorkspaceState> {
    member(&hub.listing(), name).map(|e| e.state)
}

fn running_on(listing: &Workspaces, name: &str, pid: u32) -> bool {
    member(listing, name)
        .is_some_and(|e| e.state == WorkspaceState::Running && e.daemon.map(|d| d.pid) == Some(pid))
}

/// Until the listing satisfies `done`.
async fn until(hub: &Hub, what: &str, done: impl Fn(&Workspaces) -> bool) -> Workspaces {
    let mut changes = hub.listing_changes();
    let waited = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let listing = hub.listing();
            if done(&listing) {
                return listing;
            }
            if changes.changed().await.is_err() {
                tokio::task::yield_now().await;
            }
        }
    })
    .await;
    waited.unwrap_or_else(|_| panic!("waiting for {what}: {:?}", hub.listing()))
}

/// With time frozen: until every task waits (the clock then moves 1 ms).
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

fn scans(hub: &Hub) -> u64 {
    hub.hooks.scans.load(Ordering::SeqCst)
}

fn reconciles(hub: &Hub, name: &str) -> usize {
    let id = id(name);
    hub.hooks
        .reconciles
        .lock()
        .unwrap()
        .iter()
        .filter(|r| **r == id)
        .count()
}

/// The epoch of the watch on `name`.
fn epoch(hub: &Hub, name: &str) -> u64 {
    hub.table()
        .member(&id(name))
        .and_then(|m| m.watch.as_ref().map(|w| w.epoch))
        .expect("a watch")
}

/// Why `admit` refused an open of `name`.
fn not_admitted(hub: &Hub, name: &str, conn: u64) -> RpcError {
    match hub.admit(&id(name), conn) {
        Ok(_) => panic!("{name} was admitted"),
        Err(e) => e,
    }
}

async fn remove(hub: &Hub, which: &str) -> Result<(), RpcError> {
    hub.remove(which, &Web::default()).await
}

// Joining (§multi-join "Joining").

#[tokio::test(start_paused = true)]
async fn joining_one_workspace_dials_its_daemon_as_the_operator_and_watches_it() {
    let fake = Fake::new();
    let a = fake.start("a", 100);
    a.set_summary(WorkspaceSummary {
        rev: 3,
        project: Some("payments".into()),
        agents: 2,
        working: 1,
        ..WorkspaceSummary::default()
    });
    let hub = hub(&fake, false);
    assert_eq!(hub.join(Join::One(dir("a"))).await.unwrap(), vec![id("a")]);
    let listing = hub.listing();
    assert_eq!(listing.joined.len(), 1);
    let entry = &listing.joined[0];
    assert_eq!(entry.id, id("a"));
    assert_eq!(entry.generation, 1);
    assert_eq!(entry.state, WorkspaceState::Running);
    assert_eq!(
        entry.daemon,
        Some(DaemonRef {
            pid: 100,
            started_at: 100_000
        })
    );
    assert_eq!(entry.workspace, "/crew-hub-test/a");
    assert_eq!(
        entry.project.as_deref(),
        Some("payments"),
        "the team's name"
    );
    assert_eq!(entry.summary.as_ref().map(|s| s.agents), Some(2));
    // The watch: the server's own connection, never a browser's.
    let watch = fake.conns(100, false);
    assert_eq!(watch.len(), 1);
    assert_eq!(
        fake.methods(100, watch[0]),
        ["hello", "web_attach", "summary"]
    );
    let lines = fake.lines(100);
    assert_eq!(lines[0].1["params"]["role"], "operator");
    assert_eq!(lines[0].1["params"]["token"], "token-100");
    assert_ne!(lines[0].1["params"]["web"], json!(true));
    assert_eq!(lines[1].1["params"]["url"], URL);
    assert_eq!(lines[2].1["params"]["watch"], true);
}

#[tokio::test(start_paused = true)]
async fn joining_all_takes_every_running_session_the_check_lets_through() {
    let fake = Fake::new();
    fake.start("a", 100);
    fake.start("b", 101);
    fake.start("c", 102);
    fake.misplace("c");
    let hub = hub(&fake, false);
    let mut joined = hub.join(Join::All).await.unwrap();
    joined.sort();
    let mut expected = vec![id("a"), id("b")];
    expected.sort();
    assert_eq!(joined, expected);
    assert!(running_on(&hub.listing(), "a", 100));
    assert!(running_on(&hub.listing(), "b", 101));
    assert!(
        fake.dials(102, false).is_empty(),
        "a misplaced entry is never dialled"
    );
    // A rescan names it among the others, and dials it no more than before.
    hub.rescan();
    settle().await;
    let others: Vec<String> = hub.listing().others.into_iter().map(|o| o.id).collect();
    assert_eq!(others, vec![id("c")]);
    assert!(fake.dials(102, false).is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_entry_the_check_refuses_is_neither_joined_nor_dialled() {
    let fake = Fake::new();
    fake.start("c", 102);
    fake.misplace("c");
    let hub = hub(&fake, false);
    let refused = hub.join(Join::One(dir("c"))).await.unwrap_err();
    assert_eq!(refused.code, "invalid", "{refused:?}");
    assert!(refused.message.contains("refused"), "{refused:?}");
    assert!(hub.listing().joined.is_empty());
    assert!(fake.dials(102, false).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_workspace_whose_daemon_is_gone_is_not_joined() {
    let fake = Fake::new();
    fake.start("gone", 100);
    fake.stop("gone");
    let hub = hub(&fake, false);
    let refused = hub.join(Join::One(dir("gone"))).await.unwrap_err();
    assert_eq!(refused.code, "unavailable", "{refused:?}");
    assert!(hub.listing().joined.is_empty());
    assert!(fake.dials(100, false).is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_thirty_third_workspace_is_a_conflict() {
    let fake = Fake::new();
    let names: Vec<String> = (0..=MAX_JOINED).map(|i| format!("w{i}")).collect();
    for (i, name) in names.iter().enumerate() {
        fake.start(name, 1000 + i as u32);
    }
    let hub = hub(&fake, false);
    for name in &names[..MAX_JOINED] {
        hub.join(Join::One(dir(name))).await.unwrap();
    }
    let refused = hub
        .join(Join::One(dir(&names[MAX_JOINED])))
        .await
        .unwrap_err();
    assert_eq!(refused.code, "conflict", "{refused:?}");
    assert_eq!(hub.listing().joined.len(), MAX_JOINED);
    assert!(fake.dials(1000 + MAX_JOINED as u32, false).is_empty());
    // All at once: as many as fit are joined, each shown as it is, and the
    // rest is a conflict.
    let all = self::hub(&fake, false);
    let refused = all.join(Join::All).await.unwrap_err();
    assert_eq!(refused.code, "conflict", "{refused:?}");
    let listing = all.listing();
    assert_eq!(listing.joined.len(), MAX_JOINED);
    assert!(
        listing
            .joined
            .iter()
            .all(|e| e.state == WorkspaceState::Running),
        "joined but never reconciled: {:?}",
        listing.joined.iter().map(|e| e.state).collect::<Vec<_>>()
    );
}

#[tokio::test(start_paused = true)]
async fn joining_twice_keeps_the_membership() {
    let fake = Fake::new();
    fake.start("a", 100);
    let hub = hub(&fake, false);
    assert_eq!(hub.join(Join::One(dir("a"))).await.unwrap(), vec![id("a")]);
    let rev = hub.listing().rev;
    assert!(hub.join(Join::One(dir("a"))).await.unwrap().is_empty());
    assert!(hub.join(Join::All).await.unwrap().is_empty());
    let listing = hub.listing();
    assert_eq!(listing.joined.len(), 1);
    assert_eq!(listing.joined[0].generation, 1);
    assert_eq!(listing.rev, rev, "nothing changed");
    assert_eq!(fake.dials(100, false).len(), 1, "its watch stands");
}

#[tokio::test(start_paused = true)]
async fn join_all_picks_up_a_daemon_started_later() {
    let fake = Fake::new();
    let hub = hub(&fake, true);
    hub.rescan();
    settle().await;
    assert!(hub.listing().joined.is_empty());
    fake.start("a", 100);
    hub.rescan();
    let listing = until(&hub, "a to be joined", |l| running_on(l, "a", 100)).await;
    assert_eq!(member(&listing, "a").unwrap().generation, 1);
}

#[tokio::test(start_paused = true)]
async fn a_removed_workspace_stays_out_of_rescans_until_it_is_joined_explicitly() {
    let fake = Fake::new();
    fake.start("a", 100);
    fake.start("b", 101);
    let hub = hub(&fake, true);
    hub.rescan();
    let listing = until(&hub, "both joined", |l| {
        running_on(l, "a", 100) && running_on(l, "b", 101)
    })
    .await;
    let first = member(&listing, "a").unwrap().generation;
    remove(&hub, &id("a")).await.unwrap();
    hub.rescan();
    settle().await;
    let listing = hub.listing();
    assert!(
        member(&listing, "a").is_none(),
        "a rescan does not take it back"
    );
    assert!(listing.others.iter().any(|o| o.id == id("a")));
    // `crew web` in it takes it back, with a new generation.
    assert_eq!(hub.join(Join::One(dir("a"))).await.unwrap(), vec![id("a")]);
    let second = member(&hub.listing(), "a").unwrap();
    assert!(second.generation > first);
    assert_eq!(second.state, WorkspaceState::Running);
    // So does `crew web --all`.
    remove(&hub, &id("a")).await.unwrap();
    hub.rescan();
    settle().await;
    assert!(state(&hub, "a").is_none());
    assert_eq!(hub.join(Join::All).await.unwrap(), vec![id("a")]);
    assert!(member(&hub.listing(), "a").unwrap().generation > second.generation);
}

#[tokio::test(start_paused = true)]
async fn a_session_is_joined_or_one_of_the_others_never_both() {
    let fake = Fake::new();
    fake.start("a", 100);
    fake.start("b", 101);
    let hub = hub(&fake, false);
    hub.rescan();
    settle().await;
    let others = |l: &Workspaces| {
        let mut ids: Vec<String> = l.others.iter().map(|o| o.id.clone()).collect();
        ids.sort();
        ids
    };
    let mut both = vec![id("a"), id("b")];
    both.sort();
    assert_eq!(others(&hub.listing()), both);
    // No scan from here on: what the listing says, the join and the
    // removal said themselves.
    *hub.hooks.scan_hold.lock().unwrap() = Some(Arc::new(Semaphore::new(0)));
    hub.join(Join::One(dir("a"))).await.unwrap();
    let listing = hub.listing();
    assert!(running_on(&listing, "a", 100));
    assert_eq!(others(&listing), vec![id("b")], "{listing:?}");
    // Taken out while it runs: one of the others again.
    remove(&hub, &id("a")).await.unwrap();
    let listing = hub.listing();
    assert!(member(&listing, "a").is_none());
    assert_eq!(others(&listing), both, "{listing:?}");
    let a = listing.others.iter().find(|o| o.id == id("a")).unwrap();
    assert_eq!(a.workspace, "/crew-hub-test/a");
    assert_eq!(a.project.as_deref(), Some("a"));
    // Taken out once it stopped: none of them.
    hub.join(Join::One(dir("b"))).await.unwrap();
    fake.stop("b");
    until(&hub, "b stopped", |l| {
        member(l, "b").is_some_and(|e| e.state == WorkspaceState::Stopped)
    })
    .await;
    remove(&hub, &id("b")).await.unwrap();
    assert_eq!(others(&hub.listing()), vec![id("a")]);
}

// Leaving (§multi-join "Leaving").

#[tokio::test(start_paused = true)]
async fn a_removal_lets_go_of_the_watch_before_it_answers() {
    let fake = Fake::new();
    fake.start("a", 100);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    let watch = fake.conns(100, false)[0];
    remove(&hub, &id("a")).await.unwrap();
    // Already let go of when the answer came.
    assert!(fake.let_go(watch).is_some());
    assert!(hub.listing().joined.is_empty());
    assert_eq!(not_admitted(&hub, "a", 1).code, "not_found");
}

#[tokio::test(start_paused = true)]
async fn a_pending_open_is_revoked_at_once_and_the_removal_waits_for_it() {
    let fake = Fake::new();
    fake.start("a", 100);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    let admission = hub.admit(&id("a"), 7).unwrap();
    let mut revoked = admission.revoked.clone();
    let started = Instant::now();
    let removal = tokio::spawn({
        let hub = hub.clone();
        async move { remove(&hub, &id("a")).await }
    });
    revoked.wait_for(|r| *r).await.unwrap();
    assert_eq!(not_admitted(&hub, "a", 8).code, "not_found");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!removal.is_finished(), "it waits for what holds the daemon");
    drop(admission);
    removal.await.unwrap().unwrap();
    assert!(started.elapsed() < LET_GO_WITHIN, "{:?}", started.elapsed());
}

#[tokio::test(start_paused = true)]
async fn a_guard_that_is_never_dropped_makes_the_removal_internal_and_the_workspace_stays_out() {
    let fake = Fake::new();
    fake.start("a", 100);
    let hub = hub(&fake, true);
    hub.join(Join::One(dir("a"))).await.unwrap();
    let leaked = hub.admit(&id("a"), 7).unwrap();
    *hub.hooks.drained_within.lock().unwrap() = Some(Duration::from_secs(2));
    let started = Instant::now();
    let refused = remove(&hub, &id("a")).await.unwrap_err();
    assert_eq!(refused.code, "internal", "{refused:?}");
    let took = started.elapsed();
    assert!(took >= LET_GO_WITHIN + Duration::from_secs(2), "{took:?}");
    assert!(
        took < LET_GO_WITHIN + Duration::from_millis(2010),
        "{took:?}"
    );
    assert_eq!(not_admitted(&hub, "a", 8).code, "not_found");
    // Revoked for good: a rescan under `join = "all"` does not take it back.
    hub.rescan();
    settle().await;
    assert!(state(&hub, "a").is_none());
    assert!(*leaked.revoked.borrow());
}

#[tokio::test(start_paused = true)]
async fn joining_again_after_a_removal_makes_a_greater_generation() {
    let fake = Fake::new();
    fake.start("a", 100);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    let old = hub.admit(&id("a"), 1).unwrap();
    let removal = tokio::spawn({
        let hub = hub.clone();
        async move { remove(&hub, &id("a")).await }
    });
    let mut revoked = old.revoked.clone();
    revoked.wait_for(|r| *r).await.unwrap();
    drop(old);
    removal.await.unwrap().unwrap();
    hub.join(Join::One(dir("a"))).await.unwrap();
    let entry = member(&hub.listing(), "a").unwrap();
    assert_eq!(entry.generation, 2);
    assert_eq!(entry.state, WorkspaceState::Running);
    let new = hub.admit(&id("a"), 2).unwrap();
    assert!(
        !*new.revoked.borrow(),
        "the old revocation is not the new one's"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stopped_workspace_is_removed_and_one_not_joined_is_not_found() {
    let fake = Fake::new();
    fake.start("a", 100);
    fake.start("b", 101);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    hub.join(Join::One(dir("b"))).await.unwrap();
    fake.stop("a");
    until(&hub, "a stopped", |l| {
        member(l, "a").is_some_and(|e| e.state == WorkspaceState::Stopped)
    })
    .await;
    remove(&hub, &id("a")).await.unwrap();
    // By its state directory too.
    remove(&hub, &dir_arg("b")).await.unwrap();
    assert!(hub.listing().joined.is_empty());
    let unknown = remove(&hub, &id("a")).await.unwrap_err();
    assert_eq!(unknown.code, "not_found", "{unknown:?}");
    let never = remove(&hub, "/nowhere/.crew").await.unwrap_err();
    assert_eq!(never.code, "not_found", "{never:?}");
}

// Restoring (§multi-join "Restoring").

#[tokio::test(start_paused = true)]
async fn restoring_joins_running_and_stopped_workspaces_and_the_stopped_one_runs_when_it_starts() {
    let fake = Fake::new();
    fake.start("a", 100);
    let hub = hub(&fake, false);
    let joined = hub
        .join(Join::Restore(vec![dir("a"), dir("b")]))
        .await
        .unwrap();
    assert_eq!(joined, vec![id("a"), id("b")]);
    let listing = hub.listing();
    assert!(running_on(&listing, "a", 100));
    let b = member(&listing, "b").unwrap();
    assert_eq!(b.state, WorkspaceState::Stopped);
    assert_eq!(b.daemon, None);
    assert_eq!(b.workspace, "/crew-hub-test/b");
    // Its daemon starts, and says so.
    fake.start("b", 200);
    hub.rescan();
    until(&hub, "b running", |l| running_on(l, "b", 200)).await;
}

#[tokio::test(start_paused = true)]
async fn a_restored_workspace_whose_daemon_publishes_another_socket_is_refused_then() {
    let fake = Fake::new();
    let hub = hub(&fake, false);
    hub.join(Join::Restore(vec![dir("c")])).await.unwrap();
    assert_eq!(state(&hub, "c"), Some(WorkspaceState::Stopped));
    fake.start("c", 300);
    fake.misplace("c");
    hub.rescan();
    let listing = until(&hub, "c unreachable", |l| {
        member(l, "c").is_some_and(|e| e.state == WorkspaceState::Unreachable)
    })
    .await;
    assert!(
        member(&listing, "c").unwrap().detail.contains("refused"),
        "{listing:?}"
    );
    assert!(fake.dials(300, false).is_empty(), "never dialled");
    assert_eq!(not_admitted(&hub, "c", 1).code, "unavailable");
}

// Reconciling (§multi-server "Reconciling").

#[tokio::test(start_paused = true)]
async fn the_end_of_a_watch_shows_stopped_and_a_rescan_then_shows_the_new_run() {
    let fake = Fake::new();
    fake.start("a", 100);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    // The daemon exits: it withdraws its entry, its connections end.
    fake.stop("a");
    let listing = until(&hub, "a stopped", |l| {
        member(l, "a").is_some_and(|e| e.state == WorkspaceState::Stopped)
    })
    .await;
    let a = member(&listing, "a").unwrap();
    assert_eq!(a.daemon, None);
    assert!(a.summary.is_some(), "the last summary is kept");
    assert_eq!(scans(&hub), 0, "the end of a watch is not a scan");
    // Another run starts and asks for a rescan.
    fake.start("a", 101);
    hub.rescan();
    let listing = until(&hub, "a on its new run", |l| running_on(l, "a", 101)).await;
    assert_eq!(member(&listing, "a").unwrap().generation, 1);
}

#[tokio::test(start_paused = true)]
async fn a_rescan_before_the_old_watch_ends_shows_the_new_run_and_the_late_end_is_not_news() {
    let fake = Fake::new();
    fake.start("a", 100);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    let old = epoch(&hub, "a");
    let old_watch = fake.conns(100, false)[0];
    // A new run is published while the old one's watch still stands.
    let new = fake.start("a", 101);
    new.set_summary(summary(1, 4));
    hub.rescan();
    until(&hub, "a on its new run", |l| running_on(l, "a", 101)).await;
    assert!(
        fake.let_go(old_watch).is_some(),
        "the superseded watch is dropped"
    );
    settle().await;
    let rev = hub.listing().rev;
    let reconciled = reconciles(&hub, "a");
    // The old watch's end, delivered afterwards.
    hub.events
        .send(Event::WatchEnded {
            id: id("a"),
            epoch: old,
        })
        .unwrap();
    settle().await;
    assert_eq!(hub.listing().rev, rev, "nothing changed");
    assert_eq!(reconciles(&hub, "a"), reconciled, "nothing was reconciled");
    assert!(running_on(&hub.listing(), "a", 101));
    assert_eq!(fake.dials(101, false).len(), 1);
    // Nor is whatever it still had to say.
    hub.apply_summary(&id("a"), old, summary(99, 99));
    let listing = hub.listing();
    assert_eq!(listing.rev, rev);
    assert_eq!(member(&listing, "a").unwrap().summary.unwrap().agents, 4);
}

#[tokio::test(start_paused = true)]
async fn a_rescan_asked_for_during_a_scan_reads_the_registry_again_once() {
    let fake = Fake::new();
    fake.start("a", 100);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    fake.stop("a");
    until(&hub, "a stopped", |l| {
        member(l, "a").is_some_and(|e| e.state == WorkspaceState::Stopped)
    })
    .await;
    let hold = Arc::new(Semaphore::new(0));
    *hub.hooks.scan_hold.lock().unwrap() = Some(hold.clone());
    hub.rescan();
    settle().await;
    assert_eq!(scans(&hub), 1, "held after its reading of the registry");
    // The daemon starts between that reading and the end of the scan.
    fake.start("a", 101);
    hub.rescan();
    settle().await;
    assert_eq!(state(&hub, "a"), Some(WorkspaceState::Stopped));
    hold.add_permits(1);
    settle().await;
    assert_eq!(scans(&hub), 2, "a second reading");
    hold.add_permits(1);
    until(&hub, "a running", |l| running_on(l, "a", 101)).await;
    settle().await;
    assert_eq!(scans(&hub), 2);
    // Ten requests during one scan: one scan more, not ten.
    hub.rescan();
    settle().await;
    assert_eq!(scans(&hub), 3);
    for _ in 0..10 {
        hub.rescan();
    }
    hold.add_permits(1);
    settle().await;
    assert_eq!(scans(&hub), 4);
    hold.add_permits(1);
    settle().await;
    assert_eq!(scans(&hub), 4);
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(scans(&hub), 4);
}

#[tokio::test(start_paused = true)]
async fn a_published_daemon_that_does_not_answer_is_dialled_again_with_backoff_while_published() {
    let fake = Fake::new();
    let a = fake.start("a", 100);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    // It drops the watch and takes no connection, while still published.
    a.answer(Answer::Refuse);
    a.end_all();
    until(&hub, "a unreachable", |l| {
        member(l, "a").is_some_and(|e| e.state == WorkspaceState::Unreachable)
    })
    .await;
    tokio::time::sleep(Duration::from_secs(20)).await;
    let dials = fake.dials(100, false);
    // The join's dial, the one after the watch ended, then the retries.
    let gaps: Vec<u128> = dials[1..]
        .windows(2)
        .map(|w| (w[1] - w[0]).as_millis())
        .collect();
    assert_eq!(gaps, [250, 500, 1000, 2000, 4000, 5000, 5000], "{gaps:?}");
    // Its entry goes: the next retry finds it stopped, and arms nothing.
    fake.withdraw("a");
    until(&hub, "a stopped", |l| {
        member(l, "a").is_some_and(|e| e.state == WorkspaceState::Stopped)
    })
    .await;
    let dialled = fake.dials(100, false).len();
    let reconciled = reconciles(&hub, "a");
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(fake.dials(100, false).len(), dialled);
    assert_eq!(reconciles(&hub, "a"), reconciled);
}

#[tokio::test(start_paused = true)]
async fn a_daemon_that_answers_again_runs_and_an_idle_server_arms_no_timer() {
    let fake = Fake::new();
    let a = fake.start("a", 100);
    fake.start("b", 101);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    hub.join(Join::One(dir("b"))).await.unwrap();
    a.refuse(3);
    a.end_all();
    until(&hub, "a unreachable", |l| {
        member(l, "a").is_some_and(|e| e.state == WorkspaceState::Unreachable)
    })
    .await;
    let listing = until(&hub, "a running again", |l| running_on(l, "a", 100)).await;
    assert_eq!(member(&listing, "a").unwrap().detail, "");
    // Healthy watches, nothing asked: an hour passes and nothing happens.
    settle().await;
    let before = (
        hub.listing(),
        scans(&hub),
        hub.hooks.reconciles.lock().unwrap().len(),
        fake.seen().len(),
    );
    tokio::time::sleep(Duration::from_secs(3600)).await;
    let after = (
        hub.listing(),
        scans(&hub),
        hub.hooks.reconciles.lock().unwrap().len(),
        fake.seen().len(),
    );
    assert_eq!(before, after);
}

#[tokio::test(start_paused = true)]
async fn a_reconcile_request_reconciles_that_workspace_and_scans_nothing() {
    let fake = Fake::new();
    fake.start("a", 100);
    fake.start("b", 101);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    hub.join(Join::One(dir("b"))).await.unwrap();
    settle().await;
    let (a, b, rev) = (
        reconciles(&hub, "a"),
        reconciles(&hub, "b"),
        hub.listing().rev,
    );
    hub.reconcile(&id("a"));
    settle().await;
    assert_eq!(reconciles(&hub, "a"), a + 1);
    assert_eq!(reconciles(&hub, "b"), b);
    assert_eq!(scans(&hub), 0);
    assert_eq!(hub.listing().rev, rev, "its watch stands: nothing changed");
    assert_eq!(fake.dials(100, false).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn how_a_daemon_refuses_the_server_is_what_the_listing_says() {
    let fake = Fake::new();
    let a = fake.start("a", 100);
    a.answer(Answer::Protocol);
    let b = fake.start("b", 101);
    b.answer(Answer::Unauthorized);
    let c = fake.start("c", 102);
    c.answer(Answer::Silent);
    let hub = hub(&fake, false);
    hub.join(Join::All).await.unwrap();
    let listing = hub.listing();
    let a = member(&listing, "a").unwrap();
    assert_eq!(a.state, WorkspaceState::Incompatible);
    assert!(a.detail.contains("another version of crew"), "{a:?}");
    assert_eq!(not_admitted(&hub, "a", 1).code, "protocol");
    let b = member(&listing, "b").unwrap();
    assert_eq!(b.state, WorkspaceState::Unreachable);
    assert!(b.detail.contains("not the operator's token"), "{b:?}");
    let c = member(&listing, "c").unwrap();
    assert_eq!(c.state, WorkspaceState::Unreachable);
    assert!(c.detail.contains("did not answer"), "{c:?}");
    assert_eq!(not_admitted(&hub, "c", 2).code, "unavailable");
    // Only the one that did not answer is dialled again on its own.
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(fake.dials(100, false).len(), 1);
    assert_eq!(fake.dials(101, false).len(), 1);
    assert!(fake.dials(102, false).len() > 3);
    // Restarted by this version: running.
    fake.start("a", 200);
    hub.rescan();
    until(&hub, "a running", |l| running_on(l, "a", 200)).await;
}

// Order (§multi-transport "Order on the index", "Summaries").

#[tokio::test(start_paused = true)]
async fn summaries_apply_in_the_order_of_their_revisions() {
    let fake = Fake::new();
    let a = fake.start("a", 100);
    let hub = hub(&fake, false);
    hub.join(Join::One(dir("a"))).await.unwrap();
    let agents = |l: &Workspaces| member(l, "a").and_then(|e| e.summary).map(|s| s.agents);
    a.push_summary(&summary(5, 5));
    let rev = until(&hub, "rev 5", |l| agents(l) == Some(5)).await.rev;
    a.push_summary(&summary(3, 3));
    a.push_summary(&summary(6, 6));
    let listing = until(&hub, "rev 6", |l| agents(l) == Some(6)).await;
    assert_eq!(listing.rev, rev + 1, "the older one changed nothing");
    // From the current watch, applied directly as well.
    let current = epoch(&hub, "a");
    hub.apply_summary(&id("a"), current, summary(6, 60));
    assert_eq!(agents(&hub.listing()), Some(6), "not newer");
    hub.apply_summary(&id("a"), current + 1, summary(7, 70));
    assert_eq!(agents(&hub.listing()), Some(6), "not its watch");
}

// ---------------------------------------------------------------------------
// The server in this process, with browsers on real sockets.

type Ws = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

struct Served {
    server: Arc<Server>,
    serving: Serving,
    _dir: tempfile::TempDir,
}

impl Served {
    async fn new(fake: &Arc<Fake>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::new(
            fake.clone(),
            &Settings::default(),
            dir.path().join("web-prefs.json"),
        );
        let serving = listen(&server).await;
        server.hub.start(serving.url());
        Self {
            server,
            serving,
            _dir: dir,
        }
    }

    fn hub(&self) -> Arc<Hub> {
        self.server.hub.clone()
    }

    /// `web_open {workspace}`, as `crew web` in it.
    async fn open(&self, name: &str) {
        self.server
            .open(None, Some(dir_arg(name)), false, None)
            .await
            .unwrap();
    }

    async fn ws(&self) -> Ws {
        let host = self.serving.host();
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
        let stream = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, self.serving.port))
            .await
            .unwrap();
        tokio_tungstenite::client_async(request, stream)
            .await
            .unwrap()
            .0
    }

    /// A browser session: its token.
    async fn pair(&self) -> String {
        let code = self.server.web.code();
        let mut ws = self.ws().await;
        let reply = call(&mut ws, 1, "web_pair", json!({"code": code})).await;
        reply["result"]["token"]
            .as_str()
            .expect("a token")
            .to_string()
    }

    /// A workspace connection to `name` that was let in.
    async fn workspace(&self, token: &str, name: &str) -> Ws {
        let mut ws = self.ws().await;
        let reply = hello(&mut ws, token, Some(&id(name))).await;
        assert!(reply["result"]["web_proof"].is_string(), "{reply}");
        ws
    }

    async fn remove(&self, name: &str) -> Result<(), RpcError> {
        self.server.hub.remove(&id(name), &self.server.web).await
    }
}

/// Listen on a port nobody holds.
async fn listen(server: &Arc<Server>) -> Serving {
    for _ in 0..20 {
        let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if let Ok((serving, _)) = server.web.listen(Some(port)).await {
            return serving;
        }
    }
    panic!("no free port to serve on");
}

/// A request's reply, pushes skipped.
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
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a frame within 5 s")
            .expect("the socket is open")
            .expect("a message");
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

async fn hello(ws: &mut Ws, token: &str, workspace: Option<&str>) -> Value {
    let mut params = json!({
        "role": "operator", "token": token, "client": "web", "protocol": PROTOCOL_VERSION
    });
    if let Some(workspace) = workspace {
        params["workspace"] = json!(workspace);
    }
    call(ws, 1, "hello", params).await
}

/// The close code the socket ends with (none: it just ended).
async fn closed<S>(ws: &mut S) -> Option<u16>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(frame))) => return frame.map(|f| u16::from(f.code)),
                None | Some(Err(_)) => return None,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .expect("closed within 5 s")
}

/// In real time: until `done`, polled.
async fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The connections a browser's `hello` made to `pid`.
fn web_conns(fake: &Fake, pid: u32) -> Vec<u64> {
    fake.conns(pid, true)
}

/// `pid` received a `hello` on its browser connection `conn`.
fn said_hello(fake: &Fake, pid: u32, conn: u64) -> bool {
    fake.methods(pid, conn).iter().any(|m| m == "hello")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_dial_or_an_ended_connection_reconciles_that_workspace_and_scans_nothing() {
    let fake = Fake::new();
    let a = fake.start("a", 100);
    let served = Served::new(&fake).await;
    let hub = served.hub();
    served.open("a").await;
    let token = served.pair().await;
    let mut index = served.ws().await;
    assert!(hello(&mut index, &token, None).await["result"]["web_proof"].is_string());
    // The rescan `web_open` asked for is over.
    eventually("the rescan", || scans(&hub) >= 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (rev, scanned, reconciled) = (hub.listing().rev, scans(&hub), reconciles(&hub, "a"));
    // One connection the daemon does not take: `unavailable`, one reconcile.
    a.refuse(1);
    let mut refused = served.ws().await;
    let reply = hello(&mut refused, &token, Some(&id("a"))).await;
    assert_eq!(reply["error"]["code"], "unavailable", "{reply}");
    eventually("a reconcile", || reconciles(&hub, "a") == reconciled + 1).await;
    // The next one is let in: the daemon's hello, with the proof.
    let mut conn = served.ws().await;
    let reply = hello(&mut conn, &token, Some(&id("a"))).await;
    assert!(reply["result"]["web_proof"].is_string(), "{reply}");
    assert_eq!(reply["result"]["pid"], 100);
    let browser = *web_conns(&fake, 100).last().unwrap();
    let said = fake.lines(100);
    let (_, hello_line, _) = said.iter().find(|(c, _, _)| *c == browser).unwrap();
    assert_eq!(
        hello_line["params"]["web"], true,
        "the daemon is told it is a browser's"
    );
    assert_eq!(hello_line["params"]["token"], "token-100");
    // Its daemon's side ends: 4001, and one reconcile more.
    a.end_web();
    assert_eq!(closed(&mut conn).await, Some(CLOSE_WORKSPACE_ENDED));
    eventually("a reconcile", || reconciles(&hub, "a") == reconciled + 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(reconciles(&hub, "a"), reconciled + 2);
    assert_eq!(scans(&hub), scanned, "no scan");
    assert_eq!(hub.listing().rev, rev, "the listing did not change");
    // Nor was anything pushed.
    let pushed = tokio::time::timeout(Duration::from_millis(200), next(&mut index)).await;
    assert!(pushed.is_err(), "{pushed:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_a_removal_answers_nothing_a_stubborn_page_sends_reaches_the_daemon() {
    let fake = Fake::new();
    fake.start("a", 100);
    let served = Served::new(&fake).await;
    served.open("a").await;
    let token = served.pair().await;
    let sent: Arc<Mutex<HashMap<u64, Instant>>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let mut writers = Vec::new();
    let mut readers = Vec::new();
    for c in 0..2u64 {
        let ws = served.workspace(&token, "a").await;
        let (mut sink, mut source) = ws.split();
        readers.push(tokio::spawn(async move { closed(&mut source).await }));
        let (sent, stop) = (sent.clone(), stop.clone());
        // It ignores the listing and every close, and keeps sending.
        writers.push(tokio::spawn(async move {
            let mut n = c * 1_000_000;
            while !stop.load(Ordering::SeqCst) {
                n += 1;
                sent.lock().unwrap().insert(n, Instant::now());
                let message = json!({"id": n, "method": "noise"}).to_string();
                let _ = sink.send(Message::text(message)).await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    served.remove("a").await.unwrap();
    let answered = Instant::now();
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop.store(true, Ordering::SeqCst);
    for writer in writers {
        writer.await.unwrap();
    }
    for reader in readers {
        assert_eq!(reader.await.unwrap(), Some(CLOSE_WORKSPACE_REMOVED));
    }
    // What reached the daemon was sent before the answer.
    let sent = sent.lock().unwrap().clone();
    let noise: Vec<u64> = fake
        .lines(100)
        .into_iter()
        .filter(|(_, line, _)| line["method"] == "noise")
        .map(|(_, line, _)| line["id"].as_u64().unwrap())
        .collect();
    assert!(!noise.is_empty());
    for n in noise {
        assert!(sent[&n] < answered, "message {n} was sent after the answer");
    }
    assert!(
        sent.values().any(|at| *at > answered),
        "the page kept sending"
    );
    // Every connection of that generation, the watch included, was let go
    // of before the answer.
    for conn in fake
        .conns(100, true)
        .into_iter()
        .chain(fake.conns(100, false))
    {
        let at = fake.let_go(conn).expect("let go");
        assert!(at <= answered, "connection {conn}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hello_held_in_its_dial_is_refused_at_once_by_a_removal() {
    let fake = Fake::new();
    let a = fake.start("a", 100);
    let served = Served::new(&fake).await;
    served.open("a").await;
    let token = served.pair().await;
    a.answer(Answer::After(Duration::from_millis(1500)));
    let mut ws = served.ws().await;
    let held = tokio::spawn(async move {
        let reply = hello(&mut ws, &token, Some(&id("a"))).await;
        (reply, Instant::now())
    });
    eventually("the dial", || {
        web_conns(&fake, 100)
            .first()
            .is_some_and(|c| said_hello(&fake, 100, *c))
    })
    .await;
    let dial = web_conns(&fake, 100)[0];
    let started = Instant::now();
    served.remove("a").await.unwrap();
    let answered = Instant::now();
    let (reply, replied) = held.await.unwrap();
    assert_eq!(reply["error"]["code"], "not_found", "{reply}");
    assert!(replied - started < Duration::from_millis(500));
    assert!(
        answered - started < Duration::from_millis(500),
        "nothing waited for the daemon"
    );
    assert!(fake.let_go(dial).expect("let go") <= answered);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dial_that_completes_as_the_workspace_is_removed_fails_its_commit() {
    let fake = Fake::new();
    fake.start("a", 100);
    let served = Served::new(&fake).await;
    served.open("a").await;
    let token = served.pair().await;
    let hold = Arc::new(Semaphore::new(0));
    *served.server.hooks.commit_hold.lock().unwrap() = Some(hold.clone());
    let mut ws = served.ws().await;
    let held = tokio::spawn(async move { hello(&mut ws, &token, Some(&id("a"))).await });
    eventually("the dial", || {
        web_conns(&fake, 100)
            .first()
            .is_some_and(|c| said_hello(&fake, 100, *c))
    })
    .await;
    // The dial is answered, and the connection waits before its commit.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let dial = web_conns(&fake, 100)[0];
    assert!(fake.let_go(dial).is_none());
    let started = Instant::now();
    let removal = tokio::spawn({
        let served_hub = served.hub();
        let web = served.server.web.clone();
        async move {
            let result = served_hub.remove(&id("a"), &web).await;
            (result, Instant::now())
        }
    });
    let hub = served.hub();
    eventually("the revocation", || hub.listing().joined.is_empty()).await;
    hold.add_permits(1);
    let (result, answered) = removal.await.unwrap();
    result.unwrap();
    let reply = held.await.unwrap();
    assert_eq!(reply["error"]["code"], "not_found", "{reply}");
    assert!(fake.let_go(dial).expect("let go") <= answered);
    assert!(answered - started < LET_GO_WITHIN, "it let go by itself");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_that_does_not_let_go_is_cancelled_after_a_second_and_awaited() {
    let fake = Fake::new();
    fake.start("a", 100);
    let served = Served::new(&fake).await;
    served.open("a").await;
    let token = served.pair().await;
    let _ws = served.workspace(&token, "a").await;
    let conn = web_conns(&fake, 100)[0];
    let (held, mut dropped) = oneshot::channel::<()>();
    *served.server.hooks.let_go.lock().unwrap() = Some(Arc::new(Notify::new()));
    *served.server.hooks.held.lock().unwrap() = Some(held);
    let started = Instant::now();
    served.remove("a").await.unwrap();
    let answered = Instant::now();
    // Its future was dropped before the answer, not merely aborted.
    assert!(
        matches!(
            dropped.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ),
        "the held connection was still there when the removal answered"
    );
    let took = answered - started;
    assert!(took >= LET_GO_WITHIN, "{took:?}");
    assert!(took < LET_GO_WITHIN + Duration::from_secs(1), "{took:?}");
    assert!(fake.let_go(conn).expect("let go") <= answered);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_browser_that_does_not_read_is_cancelled_after_a_second_and_its_daemon_gets_nothing_more()
{
    let fake = Fake::new();
    let a = fake.start("a", 100);
    let served = Served::new(&fake).await;
    served.open("a").await;
    let token = served.pair().await;
    let mut ws = served.workspace(&token, "a").await;
    let conn = web_conns(&fake, 100)[0];
    // The daemon sends more than the sockets hold; the browser reads none of it.
    ws.send(Message::text(
        json!({"id": 2, "method": "flood"}).to_string(),
    ))
    .await
    .unwrap();
    let mut last = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let now = a.flooded.load(Ordering::SeqCst);
        if now > 0 && now == last {
            break;
        }
        last = now;
    }
    let started = Instant::now();
    served.remove("a").await.unwrap();
    let answered = Instant::now();
    let took = answered - started;
    assert!(took >= LET_GO_WITHIN, "it was stuck in a write: {took:?}");
    assert!(took < LET_GO_WITHIN + Duration::from_secs(1), "{took:?}");
    assert!(fake.let_go(conn).expect("let go") <= answered);
    // What the browser sends now reaches nobody.
    let _ = ws
        .send(Message::text(
            json!({"id": 3, "method": "noise"}).to_string(),
        ))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fake
        .lines(100)
        .iter()
        .all(|(c, line, _)| *c != conn || line["method"] != "noise"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joining_again_makes_a_greater_generation_and_a_new_hello_works() {
    let fake = Fake::new();
    fake.start("a", 100);
    let served = Served::new(&fake).await;
    served.open("a").await;
    let token = served.pair().await;
    let first = member(&served.hub().listing(), "a").unwrap().generation;
    let mut old = served.workspace(&token, "a").await;
    served.remove("a").await.unwrap();
    assert_eq!(closed(&mut old).await, Some(CLOSE_WORKSPACE_REMOVED));
    served.open("a").await;
    let second = member(&served.hub().listing(), "a").unwrap().generation;
    assert!(second > first);
    let mut new = served.workspace(&token, "a").await;
    let pong = call(&mut new, 2, "ping", json!({})).await;
    assert_eq!(pong["result"]["pong"], true, "{pong}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_that_applies_only_greater_revisions_ends_at_the_servers_listing() {
    let fake = Fake::new();
    fake.start("a", 100);
    let served = Served::new(&fake).await;
    let hub = served.hub();
    served.open("a").await;
    let token = served.pair().await;
    let mut index = served.ws().await;
    assert!(hello(&mut index, &token, None).await["result"]["web_proof"].is_string());
    let current = epoch(&hub, "a");
    let base = hub
        .table()
        .member(&id("a"))
        .and_then(|m| m.watch.as_ref().map(|w| w.summary_rev))
        .unwrap();
    // A thousand changes of the listing...
    let changes = tokio::spawn({
        let hub = hub.clone();
        async move {
            for i in 1..=1000u64 {
                hub.apply_summary(&id("a"), current, summary(base + i, i as u32));
                if i % 10 == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                } else {
                    tokio::task::yield_now().await;
                }
            }
        }
    });
    // ...while the page asks for it a hundred times; every frame is kept,
    // in the order it arrived.
    let mut frames = Vec::new();
    for n in 0..100u64 {
        let request = json!({"id": 100 + n, "method": "workspaces", "params": {}});
        index
            .send(Message::text(request.to_string()))
            .await
            .unwrap();
        loop {
            let frame = next(&mut index).await;
            let done = frame["id"] == 100 + n;
            frames.push(frame);
            if done {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    changes.await.unwrap();
    while let Ok(frame) = tokio::time::timeout(Duration::from_millis(300), next(&mut index)).await {
        frames.push(frame);
    }
    // The page's rule.
    let mut applied: Option<Workspaces> = None;
    let mut pushed = 0;
    let (mut replies, mut pushes) = (0, 0);
    for frame in frames {
        let (listing, push) = if frame["push"]["kind"] == "workspaces" {
            (frame["push"]["data"].clone(), true)
        } else if frame["result"]["rev"].is_u64() {
            (frame["result"].clone(), false)
        } else {
            continue;
        };
        let listing: Workspaces = serde_json::from_value(listing).unwrap();
        if push {
            assert!(listing.rev > pushed, "pushes only grow");
            pushed = listing.rev;
            pushes += 1;
        } else {
            assert!(
                listing.rev >= pushed,
                "a reply is never older than a push before it"
            );
            replies += 1;
        }
        if applied.as_ref().is_none_or(|a| listing.rev > a.rev) {
            applied = Some(listing);
        }
    }
    assert_eq!(replies, 100);
    assert!(pushes > 0);
    let applied = applied.unwrap();
    assert_eq!(applied, hub.listing());
    assert_eq!(member(&applied, "a").unwrap().summary.unwrap().agents, 1000);
}
