//! Daemon connections and the threads that feed the event loop.
//!
//! Three threads, one channel:
//! - the subscriber holds a subscribed connection and forwards pushes as they
//!   happen (reconnecting once a second if the daemon goes away);
//! - the requester owns a second connection and runs user-initiated calls so
//!   the render loop never blocks on the socket;
//! - the input thread forwards terminal events.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crew_core::client::{Client, ClientError, ClientResult};
use crew_core::rpc::{
    Call, GitOutcome, InboxResult, ModelChoice, ModelsResult, OutputResult, Push, SendParams,
    Snapshot, TranscriptResult,
};
use crew_core::types::{Event, MessageType};
use ratatui::crossterm::event as term;

use crate::app::{Command, Reply};

const SNAPSHOT_MESSAGES: usize = 300;
const SNAPSHOT_EVENTS: usize = 400;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// Stopping a team waits for each agent to exit.
const STOP_TIMEOUT: Duration = Duration::from_secs(90);
/// Listing the models waits for pi to list its own.
const MODELS_TIMEOUT: Duration = Duration::from_secs(20);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// Terminal rows requested for the inspector's output capture: the screen,
/// and scrollback where the backend has it at hand.
const OUTPUT_LINES: usize = 300;
/// Entries of the session transcript the inspector asks for.
const TRANSCRIPT_ENTRIES: usize = 400;

pub enum UiEvent {
    Input(term::Event),
    Snapshot(Box<Snapshot>),
    Push(Box<Push>),
    Disconnected,
    Reply(Box<Reply>),
}

fn fetch_snapshot(socket: &Path) -> ClientResult<Snapshot> {
    let (mut client, _) = Client::operator(socket, "tui")?;
    client.set_timeout(Some(REQUEST_TIMEOUT))?;
    client.call(Call::Snapshot {
        messages: Some(SNAPSHOT_MESSAGES),
        events: Some(SNAPSHOT_EVENTS),
    })
}

/// Check that a daemon is listening before taking over the terminal.
pub fn probe(socket: &Path) -> ClientResult<()> {
    fetch_snapshot(socket).map(|_| ())
}

/// Forward pushes for as long as the UI lives.
///
/// Subscribing happens *before* the snapshot is fetched, so no change can
/// fall between the two: anything that happened earlier is in the snapshot,
/// anything later arrives as a push (and reducers are idempotent for the
/// overlap).
pub fn spawn_subscriber(socket: PathBuf, tx: Sender<UiEvent>, stop: Arc<AtomicBool>) {
    let run = move || {
        while !stop.load(Ordering::Relaxed) {
            let session = (|| -> ClientResult<()> {
                let (client, _) = Client::operator(&socket, "tui")?;
                let mut stream = client.subscribe()?;
                let snapshot = fetch_snapshot(&socket)?;
                if tx.send(UiEvent::Snapshot(Box::new(snapshot))).is_err() {
                    return Ok(());
                }
                loop {
                    match stream.next()? {
                        Push::Resync => {
                            let snapshot = fetch_snapshot(&socket)?;
                            if tx.send(UiEvent::Snapshot(Box::new(snapshot))).is_err() {
                                return Ok(());
                            }
                        }
                        Push::Shutdown => return Err(ClientError::Closed),
                        push => {
                            if tx.send(UiEvent::Push(Box::new(push))).is_err() {
                                return Ok(());
                            }
                        }
                    }
                }
            })();
            if session.is_ok() || stop.load(Ordering::Relaxed) {
                // The UI went away.
                return;
            }
            if tx.send(UiEvent::Disconnected).is_err() {
                return;
            }
            thread::sleep(RECONNECT_DELAY);
        }
    };
    let _ = thread::Builder::new()
        .name("crew-tui-sub".into())
        .spawn(run);
}

/// Run commands against the daemon on a worker thread; replies come back on
/// the UI channel.
/// The dashboard's line to the daemon: a thread for requests, and one for
/// keys typed into a terminal, which must never queue behind a request in
/// flight (a screen read takes the backend a while).
#[derive(Clone)]
pub struct Requests {
    main: Sender<Command>,
    keys: Sender<Command>,
}

impl Requests {
    pub fn send(&self, command: Command) -> Result<(), mpsc::SendError<Command>> {
        match command {
            Command::Keys { .. } => self.keys.send(command),
            other => self.main.send(other),
        }
    }
}

pub fn spawn_requester(socket: PathBuf, tx: Sender<UiEvent>) -> Requests {
    let (cmd_tx, cmd_rx): (Sender<Command>, Receiver<Command>) = mpsc::channel();
    let keys = spawn_typist(socket.clone(), tx.clone());
    let run = move || {
        let mut requester = Requester {
            socket,
            client: None,
        };
        for command in cmd_rx {
            let Some(reply) = requester.run(command) else {
                continue;
            };
            if tx.send(UiEvent::Reply(Box::new(reply))).is_err() {
                return;
            }
        }
    };
    let _ = thread::Builder::new()
        .name("crew-tui-req".into())
        .spawn(run);
    Requests { main: cmd_tx, keys }
}

/// Keys typed into an agent's terminal go over a connection of their own:
/// the daemon's backend answers each call on its own clock (herdr: about a
/// tenth of a second), so behind a screen read a key would arrive late, and
/// keys that piled up meanwhile go in one call.
fn spawn_typist(socket: PathBuf, tx: Sender<UiEvent>) -> Sender<Command> {
    let (keys_tx, keys_rx): (Sender<Command>, Receiver<Command>) = mpsc::channel();
    let run = move || {
        let mut requester = Requester {
            socket,
            client: None,
        };
        let mut carried: Option<Command> = None;
        loop {
            let first = match carried.take() {
                Some(command) => command,
                None => match keys_rx.recv() {
                    Ok(command) => command,
                    Err(_) => return,
                },
            };
            let Command::Keys { id, mut text } = first else {
                continue;
            };
            carried = gather_keys(&id, &mut text, &keys_rx);
            if let Some(reply) = requester.run(Command::Keys { id, text }) {
                if tx.send(UiEvent::Reply(Box::new(reply))).is_err() {
                    return;
                }
            }
        }
    };
    let _ = thread::Builder::new()
        .name("crew-tui-keys".into())
        .spawn(run);
    keys_tx
}

/// Add the keys already waiting for the same terminal to `text`; the first
/// command for anything else is handed back to go next.
fn gather_keys(id: &str, text: &mut String, keys_rx: &Receiver<Command>) -> Option<Command> {
    while let Ok(next) = keys_rx.try_recv() {
        match next {
            Command::Keys {
                id: same,
                text: more,
            } if same == id => text.push_str(&more),
            other => return Some(other),
        }
    }
    None
}

struct Requester {
    socket: PathBuf,
    client: Option<Client>,
}

impl Requester {
    fn call<T: serde::de::DeserializeOwned>(&mut self, call: Call) -> Result<T, String> {
        if self.client.is_none() {
            let (client, _) = Client::operator(&self.socket, "tui").map_err(|e| e.to_string())?;
            client
                .set_timeout(Some(REQUEST_TIMEOUT))
                .map_err(|e| e.to_string())?;
            self.client = Some(client);
        }
        let client = self.client.as_mut().expect("client was just connected");
        match client.call::<T>(call) {
            Ok(value) => Ok(value),
            Err(ClientError::Rpc(error)) => Err(error.message),
            Err(other) => {
                // The connection is in an unknown state (timeout, EOF, garbage);
                // drop it so the next request starts clean.
                self.client = None;
                Err(other.to_string())
            }
        }
    }

    /// Stop every agent and the daemon. Agents are given a moment to exit
    /// one after the other, so this gets a connection with more patience.
    fn stop_team(&mut self) -> Result<(), String> {
        self.client = None;
        let (mut client, _) = Client::operator(&self.socket, "tui").map_err(|e| e.to_string())?;
        client
            .set_timeout(Some(STOP_TIMEOUT))
            .map_err(|e| e.to_string())?;
        match client.call::<serde_json::Value>(Call::Shutdown { stop_agents: true }) {
            Ok(_) => Ok(()),
            Err(ClientError::Rpc(error)) => Err(error.message),
            Err(other) => Err(other.to_string()),
        }
    }

    /// The models agents can be put on. The daemon asks pi for its own,
    /// which takes a moment, so this gets a connection with more patience.
    fn models(&mut self) -> Result<Vec<ModelChoice>, String> {
        let (mut client, _) = Client::operator(&self.socket, "tui").map_err(|e| e.to_string())?;
        client
            .set_timeout(Some(MODELS_TIMEOUT))
            .map_err(|e| e.to_string())?;
        match client.call::<ModelsResult>(Call::ModelsList) {
            Ok(result) => Ok(result.models),
            Err(ClientError::Rpc(error)) => Err(error.message),
            Err(other) => Err(other.to_string()),
        }
    }

    fn action(&mut self, label: String, call: Call) -> Reply {
        let result = self.call::<serde_json::Value>(call).map(|_| ());
        Reply::Action { label, result }
    }

    fn run(&mut self, command: Command) -> Option<Reply> {
        Some(match command {
            Command::Quit | Command::SetMouse(_) => return None,
            Command::FetchOutput { id } => Reply::Output {
                result: self.call::<OutputResult>(Call::AgentOutput {
                    id: id.clone(),
                    lines: Some(OUTPUT_LINES),
                    screen: true,
                    ansi: true,
                }),
                agent: id,
            },
            Command::FetchTranscript { id } => Reply::Transcript {
                result: self.call::<TranscriptResult>(Call::AgentTranscript {
                    id: id.clone(),
                    limit: Some(TRANSCRIPT_ENTRIES),
                }),
                agent: id,
            },
            Command::FetchInbox { id } => Reply::Inbox {
                result: self
                    .call::<InboxResult>(Call::Inbox {
                        agent: Some(id.clone()),
                        unread_only: false,
                        ack: false,
                        limit: Some(100),
                    })
                    .map(|r| r.messages),
                agent: id,
            },
            Command::FetchEvents { id } => Reply::Events {
                result: self.call::<Vec<Event>>(Call::Events {
                    agent: Some(id.clone()),
                    limit: Some(200),
                }),
                agent: id,
            },
            Command::FetchDiff { id } => Reply::Diff {
                result: self.call::<GitOutcome>(Call::GitDiff {
                    against: None,
                    agent: Some(id.clone()),
                }),
                agent: id,
            },
            Command::Kill(id) => self.action(format!("kill {id}"), Call::AgentKill { id }),
            Command::Restart(id) => self.action(format!("restart {id}"), Call::AgentRestart { id }),
            Command::Pause(id) => self.action(format!("pause {id}"), Call::AgentPause { id }),
            Command::Resume(id) => self.action(format!("resume {id}"), Call::AgentResume { id }),
            Command::ResumeLimited { restart } => self.action(
                if restart {
                    "restarted the paused agents on the current login".to_string()
                } else {
                    "told the paused agents to continue".to_string()
                },
                Call::Resume {
                    agents: Vec::new(),
                    restart,
                },
            ),
            Command::Focus(id) => self.action(format!("focus {id}"), Call::AgentFocus { id }),
            Command::Compact(id) => self.action(
                format!("compaction of {id}'s context queued"),
                Call::AgentCompact {
                    id: Some(id),
                    keep: None,
                },
            ),
            // Text goes in as one prompt (pasted, then Enter); nothing typed
            // is just the Enter.
            Command::Type { id, text } => {
                let submit = !text.is_empty();
                self.action(
                    format!("typed into {id}"),
                    Call::AgentInput {
                        id,
                        text: Some(if submit { text } else { "\r".to_string() }),
                        submit,
                    },
                )
            }
            Command::Keys { id, text } => Reply::Keys(
                self.call::<serde_json::Value>(Call::AgentInput {
                    id,
                    text: Some(text),
                    submit: false,
                })
                .map(|_| ()),
            ),
            Command::StopTeam => Reply::Stopped(self.stop_team()),
            Command::FetchModels => Reply::Models(self.models()),
            Command::SetModel {
                id,
                model,
                runtime,
                label,
            } => self.action(
                label,
                Call::AgentSetModel {
                    id,
                    model,
                    runtime: Some(runtime),
                    note: None,
                },
            ),
            Command::Answer { id, body } => Reply::Answered {
                result: self
                    .call::<serde_json::Value>(Call::Answer {
                        id: id.clone(),
                        body,
                    })
                    .map(|_| ()),
                id,
            },
            Command::Send { to, body } => {
                let label = if to == crew_core::types::BROADCAST {
                    "broadcast sent".to_string()
                } else {
                    format!("message sent to {to}")
                };
                self.action(
                    label,
                    Call::Send(SendParams {
                        to,
                        kind: MessageType::Message,
                        body,
                        ..SendParams::default()
                    }),
                )
            }
        })
    }
}

/// Forward terminal events. The poll timeout only bounds how long the thread
/// takes to notice `stop`; it does not drive any redraw.
pub fn spawn_input(tx: Sender<UiEvent>, stop: Arc<AtomicBool>) {
    let run = move || {
        while !stop.load(Ordering::Relaxed) {
            match term::poll(Duration::from_millis(200)) {
                Ok(true) => match term::read() {
                    Ok(event) => {
                        if tx.send(UiEvent::Input(event)).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Ok(false) => {}
                Err(_) => return,
            }
        }
    };
    let _ = thread::Builder::new()
        .name("crew-tui-input".into())
        .spawn(run);
}

#[cfg(test)]
mod tests {
    //! The connection layer against a scripted stand-in for the daemon that
    //! speaks the real wire protocol over a real Unix socket.

    use std::io::{BufRead, BufReader, Write};
    use std::net::Shutdown;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Mutex;
    use std::time::Instant;

    use crew_core::rpc::{HelloInfo, Request, Role, RpcError, ServerFrame, PROTOCOL_VERSION};
    use serde_json::json;

    use super::*;
    use crate::fixture;

    struct FakeDaemon {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        connections: Arc<Mutex<Vec<UnixStream>>>,
        subscribers: Arc<Mutex<Vec<UnixStream>>>,
        calls: Arc<Mutex<Vec<Call>>>,
    }

    impl FakeDaemon {
        fn start(path: &Path) -> Self {
            let _ = std::fs::remove_file(path);
            let listener = UnixListener::bind(path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let daemon = Self {
                path: path.to_path_buf(),
                stop: Arc::new(AtomicBool::new(false)),
                connections: Arc::default(),
                subscribers: Arc::default(),
                calls: Arc::default(),
            };
            let (stop, connections, subscribers, calls) = (
                daemon.stop.clone(),
                daemon.connections.clone(),
                daemon.subscribers.clone(),
                daemon.calls.clone(),
            );
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            connections
                                .lock()
                                .unwrap()
                                .push(stream.try_clone().unwrap());
                            let (subscribers, calls) = (subscribers.clone(), calls.clone());
                            thread::spawn(move || serve(stream, subscribers, calls));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
            daemon
        }

        fn push(&self, push: Push) {
            let mut line = serde_json::to_vec(&ServerFrame::push(push)).unwrap();
            line.push(b'\n');
            for stream in self.subscribers.lock().unwrap().iter_mut() {
                stream.write_all(&line).unwrap();
            }
        }

        fn stop(&self) {
            self.stop.store(true, Ordering::Relaxed);
            for stream in self.connections.lock().unwrap().drain(..) {
                let _ = stream.shutdown(Shutdown::Both);
            }
            let _ = std::fs::remove_file(&self.path);
            // Let the accept loop notice and drop the listener.
            thread::sleep(Duration::from_millis(30));
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn serve(
        stream: UnixStream,
        subscribers: Arc<Mutex<Vec<UnixStream>>>,
        calls: Arc<Mutex<Vec<Call>>>,
    ) {
        let mut writer = stream.try_clone().unwrap();
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let request: Request = serde_json::from_str(&line).unwrap();
            calls.lock().unwrap().push(request.call.clone());
            let frame = match &request.call {
                Call::Hello(_) => ServerFrame::ok(
                    request.id,
                    serde_json::to_value(HelloInfo {
                        role: Role::Operator,
                        agent_id: None,
                        project: Some("checkout-rewrite".into()),
                        version: "test".into(),
                        protocol: PROTOCOL_VERSION,
                        pid: 1,
                    })
                    .unwrap(),
                ),
                Call::Subscribe => {
                    subscribers
                        .lock()
                        .unwrap()
                        .push(writer.try_clone().unwrap());
                    ServerFrame::ok(request.id, json!(null))
                }
                Call::Snapshot { .. } => ServerFrame::ok(
                    request.id,
                    serde_json::to_value(fixture::snapshot(1_000)).unwrap(),
                ),
                Call::AgentOutput { id, .. } => ServerFrame::ok(
                    request.id,
                    json!({ "text": format!("screen of {id}"), "alive": true }),
                ),
                Call::AgentKill { id } => ServerFrame::err(
                    request.id,
                    RpcError::not_found(format!("no agent named {id}")),
                ),
                _ => ServerFrame::ok(request.id, json!(null)),
            };
            let mut out = serde_json::to_vec(&frame).unwrap();
            out.push(b'\n');
            if writer.write_all(&out).is_err() {
                return;
            }
        }
    }

    /// Wait for the first event matching `want`, skipping others.
    fn expect<T>(rx: &Receiver<UiEvent>, what: &str, want: impl Fn(UiEvent) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(event) => {
                    if let Some(found) = want(event) {
                        return found;
                    }
                }
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    fn socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");
        (dir, path)
    }

    #[test]
    fn subscriber_delivers_snapshot_pushes_resync_and_reconnects() {
        let (_dir, path) = socket();
        let daemon = FakeDaemon::start(&path);
        probe(&path).unwrap();

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        spawn_subscriber(path.clone(), tx, stop.clone());

        let snapshot = expect(&rx, "the initial snapshot", |e| match e {
            UiEvent::Snapshot(s) => Some(s),
            _ => None,
        });
        assert_eq!(snapshot.agents.len(), 7);
        // Subscribing precedes the snapshot, so nothing can fall in between.
        let calls = daemon.calls();
        let subscribe = calls.iter().position(|c| *c == Call::Subscribe).unwrap();
        let snapshot_call = calls
            .iter()
            .rposition(|c| matches!(c, Call::Snapshot { .. }))
            .unwrap();
        assert!(subscribe < snapshot_call);
        assert!(calls.contains(&Call::Snapshot {
            messages: Some(SNAPSHOT_MESSAGES),
            events: Some(SNAPSHOT_EVENTS),
        }));

        let message = fixture::message(1, "claude-3", "claude-5", MessageType::Question, "hi", 5);
        daemon.push(Push::Message(message.clone()));
        let got = expect(&rx, "the message push", |e| match e {
            UiEvent::Push(p) => Some(*p),
            _ => None,
        });
        assert_eq!(got, Push::Message(message));

        // Falling behind makes the daemon ask for a fresh snapshot.
        daemon.push(Push::Resync);
        expect(&rx, "the resync snapshot", |e| match e {
            UiEvent::Snapshot(_) => Some(()),
            _ => None,
        });

        // The daemon goes away: the UI is told, and the thread keeps retrying.
        daemon.stop();
        expect(&rx, "the disconnect notice", |e| match e {
            UiEvent::Disconnected => Some(()),
            _ => None,
        });
        assert!(probe(&path).is_err());

        let daemon = FakeDaemon::start(&path);
        expect(&rx, "the snapshot after reconnecting", |e| match e {
            UiEvent::Snapshot(_) => Some(()),
            _ => None,
        });

        // A graceful shutdown notice is treated like a disconnect.
        daemon.push(Push::Shutdown);
        expect(&rx, "the disconnect after shutdown", |e| match e {
            UiEvent::Disconnected => Some(()),
            _ => None,
        });
        stop.store(true, Ordering::Relaxed);
        daemon.stop();
    }

    /// Drives the real event loop on a real terminal against the scripted
    /// daemon; a peer-to-peer message arrives one second in. Needs a TTY:
    ///
    /// ```text
    /// cargo test -p crew-tui --lib live_dashboard -- --ignored --nocapture
    /// ```
    /// Press `q` to leave.
    #[test]
    #[ignore = "needs an interactive terminal"]
    fn live_dashboard_against_a_scripted_daemon() {
        let (_dir, path) = socket();
        let daemon = Arc::new(FakeDaemon::start(&path));
        let pusher = {
            let daemon = daemon.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(1));
                let now = crew_core::util::now_ms();
                daemon.push(Push::Message(fixture::message(
                    77,
                    "claude-6",
                    "claude-3",
                    MessageType::Question,
                    "does search need the ledger index rebuilt?",
                    now,
                )));
            })
        };
        crate::run(&path, Default::default(), None).unwrap();
        pusher.join().unwrap();
        daemon.stop();
    }

    #[test]
    fn keys_that_piled_up_go_together_and_another_terminals_wait_their_turn() {
        let (tx, rx) = mpsc::channel();
        let keys = |id: &str, text: &str| Command::Keys {
            id: id.into(),
            text: text.into(),
        };
        for command in [
            keys("a", "b"),
            keys("a", "c"),
            keys("b", "1"),
            keys("a", "d"),
        ] {
            tx.send(command).unwrap();
        }
        let mut text = "a".to_string();
        let carried = gather_keys("a", &mut text, &rx);
        assert_eq!(text, "abc");
        assert_eq!(carried, Some(keys("b", "1")));
        let mut text = "1".to_string();
        assert_eq!(gather_keys("b", &mut text, &rx), Some(keys("a", "d")));
        assert_eq!(text, "1");
        assert_eq!(gather_keys("a", &mut text, &rx), None);
    }

    #[test]
    fn requester_runs_commands_and_reports_errors() {
        let (_dir, path) = socket();
        let daemon = FakeDaemon::start(&path);
        let (tx, rx) = mpsc::channel();
        let requests = spawn_requester(path.clone(), tx);
        let reply = |what: &str| {
            expect(&rx, what, |e| match e {
                UiEvent::Reply(r) => Some(*r),
                _ => None,
            })
        };

        requests
            .send(Command::FetchOutput {
                id: "claude-3".into(),
            })
            .unwrap();
        match reply("the output") {
            Reply::Output { agent, result } => {
                assert_eq!(agent, "claude-3");
                let out = result.unwrap();
                assert_eq!(out.text, "screen of claude-3");
                assert!(out.alive);
            }
            _ => panic!("expected an output reply"),
        }

        // Daemon-side errors surface as text, and the connection stays usable.
        requests.send(Command::Kill("ghost".into())).unwrap();
        match reply("the kill result") {
            Reply::Action { label, result } => {
                assert_eq!(label, "kill ghost");
                assert_eq!(result.unwrap_err(), "no agent named ghost");
            }
            _ => panic!("expected an action reply"),
        }

        requests
            .send(Command::Send {
                to: "claude-5".into(),
                body: "need a migration".into(),
            })
            .unwrap();
        match reply("the send result") {
            Reply::Action { label, result } => {
                assert_eq!(label, "message sent to claude-5");
                result.unwrap();
            }
            _ => panic!("expected an action reply"),
        }
        requests.send(Command::Pause("claude-1".into())).unwrap();
        reply("the pause result");

        let calls = daemon.calls();
        assert!(calls.contains(&Call::AgentOutput {
            id: "claude-3".into(),
            lines: Some(OUTPUT_LINES),
            screen: true,
            ansi: true,
        }));
        assert!(calls.contains(&Call::AgentPause {
            id: "claude-1".into()
        }));
        let sent = calls
            .iter()
            .find_map(|c| match c {
                Call::Send(params) => Some(params.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            (sent.to.as_str(), sent.body.as_str()),
            ("claude-5", "need a migration")
        );
        assert_eq!(sent.kind, MessageType::Message);
        // Everything went over one operator connection; quitting sends nothing.
        let hellos = calls.iter().filter(|c| matches!(c, Call::Hello(_))).count();
        assert_eq!(hellos, 1);
        assert!(!calls.iter().any(|c| matches!(c, Call::Shutdown { .. })));

        // An answer names its question.
        requests
            .send(Command::Answer {
                id: "m-q".into(),
                body: "keep it".into(),
            })
            .unwrap();
        match reply("the answer result") {
            Reply::Answered { id, result } => {
                assert_eq!(id, "m-q");
                result.unwrap();
            }
            _ => panic!("expected an answer reply"),
        }
        assert!(daemon.calls().contains(&Call::Answer {
            id: "m-q".into(),
            body: "keep it".into()
        }));

        // Stopping the team is the daemon's shutdown, agents included.
        requests.send(Command::StopTeam).unwrap();
        match reply("the stop result") {
            Reply::Stopped(result) => result.unwrap(),
            _ => panic!("expected a stop reply"),
        }
        assert!(daemon
            .calls()
            .contains(&Call::Shutdown { stop_agents: true }));

        // With the daemon gone a command fails cleanly instead of hanging.
        daemon.stop();
        requests.send(Command::Restart("claude-1".into())).unwrap();
        match reply("the failed restart") {
            Reply::Action { result, .. } => assert!(result.is_err()),
            _ => panic!("expected an action reply"),
        }
        requests.send(Command::StopTeam).unwrap();
        match reply("the failed stop") {
            Reply::Stopped(result) => assert!(result.is_err()),
            _ => panic!("expected a stop reply"),
        }
    }
}
