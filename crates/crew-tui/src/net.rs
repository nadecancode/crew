//! Daemon connections and the threads that feed the event loop.
//!
//! Dedicated workers feed one UI channel:
//! - the subscriber holds a subscribed connection and forwards pushes as they
//!   happen (reconnecting once a second if the daemon goes away);
//! - fast requests, slow lifecycle/model calls and terminal keys use separate
//!   workers, so none blocks the render loop or queues keys behind a request;
//! - the input thread forwards terminal events.
//!
//! Commands on different workers may finish out of order; pushes refresh the UI.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crew_core::client::{Client, ClientError, ClientResult, ShutdownHandle};
use crew_core::rpc::{Call, Push, Snapshot};
#[cfg(test)]
use crew_core::rpc::{
    GitOutcome, InboxResult, ModelChoice, ModelsResult, OutputResult, SendParams, TranscriptResult,
};
#[cfg(test)]
use crew_core::types::{Event, MessageType};
use ratatui::crossterm::event as term;

use crate::app::{Command, Reply};

const SNAPSHOT_MESSAGES: usize = 300;
const SNAPSHOT_EVENTS: usize = 400;
const REQUEST_TIMEOUT: Duration = Duration::from_millis(crew_client::policy::DEFAULT_TIMEOUT_MS);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const STOP_POLL: Duration = Duration::from_millis(25);
/// Terminal rows requested for the inspector's output capture: the screen,
/// and scrollback where the backend has it at hand.
#[cfg(test)]
const OUTPUT_LINES: usize = 300;
/// Entries of the session transcript the inspector asks for.
#[cfg(test)]
const TRANSCRIPT_ENTRIES: usize = 400;

#[cfg(test)]
fn request_timeout(call: &Call) -> Duration {
    Duration::from_millis(crew_client::policy::timeout_for(call.method()))
}

pub enum UiEvent {
    ScopedSnapshot {
        epoch: u64,
        snapshot: Box<Snapshot>,
    },
    ScopedPush {
        epoch: u64,
        push: Box<Push>,
    },
    ScopedDisconnected {
        epoch: u64,
    },
    PlanKeys {
        scope: crew_client::ClientScope,
        id: String,
        text: String,
        reply: Sender<Option<crate::shared::Planned>>,
    },
    DiscardedInput,
    Input(term::Event),
    Reply(Box<Reply>),
    Fatal(ClientError),
}

fn fetch_snapshot(socket: &Path) -> ClientResult<Snapshot> {
    fetch_controlled_snapshot(socket, None)
}

fn fetch_controlled_snapshot(
    socket: &Path,
    control: Option<&WorkerControl>,
) -> ClientResult<Snapshot> {
    let connection = match control {
        Some(control) => {
            Client::operator_with_control(socket, "tui", |handle| control.register(handle))
        }
        None => Client::operator(socket, "tui"),
    };
    let result = (|| {
        let (mut client, _) = connection?;
        client.set_timeout(Some(REQUEST_TIMEOUT))?;
        client.call(Call::Snapshot {
            messages: Some(SNAPSHOT_MESSAGES),
            events: Some(SNAPSHOT_EVENTS),
        })
    })();
    if let Some(control) = control {
        control.clear();
    }
    result
}

/// Each worker publishes only a close handle. Registration and shutdown use
/// the same lock, so a connect completing after stop cannot escape cleanup.
/// Token reads and a dial that has not yet connected remain OS operations.
#[derive(Clone)]
struct WorkerControl {
    stop: Arc<AtomicBool>,
    active: Arc<Mutex<Option<ShutdownHandle>>>,
}

impl WorkerControl {
    fn new(stop: Arc<AtomicBool>) -> Self {
        Self {
            stop,
            active: Arc::new(Mutex::new(None)),
        }
    }

    fn register(&self, handle: ShutdownHandle) {
        let mut active = self.active.lock().expect("worker cancellation lock");
        if let Some(old) = active.take() {
            old.shutdown();
        }
        if self.stop.load(Ordering::Acquire) {
            handle.shutdown();
        } else {
            *active = Some(handle);
        }
    }

    fn clear(&self) {
        if let Some(handle) = self.active.lock().expect("worker cancellation lock").take() {
            handle.shutdown();
        }
    }
}

fn next_command(rx: &Receiver<Command>, control: &WorkerControl) -> Option<Command> {
    loop {
        if control.stop.load(Ordering::Acquire) {
            return None;
        }
        match rx.recv_timeout(STOP_POLL) {
            Ok(command) => return Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

pub struct Subscriber {
    stop: Arc<AtomicBool>,
    stream: WorkerControl,
    snapshot: WorkerControl,
    worker: Option<thread::JoinHandle<()>>,
}

impl Subscriber {
    fn restart(&self) {
        self.stream.clear();
        self.snapshot.clear();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.restart();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        self.shutdown();
    }
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
pub fn spawn_subscriber(socket: PathBuf, tx: Sender<UiEvent>, stop: Arc<AtomicBool>) -> Subscriber {
    let stream = WorkerControl::new(stop.clone());
    let snapshot = WorkerControl::new(stop.clone());
    let (active, snapshots, stopping) = (stream.clone(), snapshot.clone(), stop.clone());
    let run = move || {
        let mut epoch = 0u64;
        while !stopping.load(Ordering::Acquire) {
            epoch = match epoch.checked_add(1) {
                Some(epoch) => epoch,
                None => return,
            };
            let session = (|| -> ClientResult<()> {
                let (mut client, peer) = Client::operator_with_control(&socket, "tui", |handle| {
                    active.register(handle)
                })?;
                client.set_timeout(Some(REQUEST_TIMEOUT))?;
                client.call_value(Call::Subscribe)?;
                client.set_timeout(None)?;
                let (mut stream, _writer) = client.split();
                let snapshot = fetch_controlled_snapshot(&socket, Some(&snapshots))?;
                if snapshot.session.daemon_pid != peer.pid {
                    return Err(ClientError::Protocol(
                        "snapshot belongs to a different daemon".into(),
                    ));
                }
                let incarnation = (
                    snapshot.session.workspace.clone(),
                    snapshot.session.daemon_started_at,
                );
                if stopping.load(Ordering::Acquire) {
                    return Ok(());
                }
                if tx
                    .send(UiEvent::ScopedSnapshot {
                        epoch,
                        snapshot: Box::new(snapshot),
                    })
                    .is_err()
                {
                    return Ok(());
                }
                loop {
                    let Some(push) = stream.next()?.push else {
                        continue;
                    };
                    if stopping.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    match push {
                        Push::Resync => {
                            let snapshot = fetch_controlled_snapshot(&socket, Some(&snapshots))?;
                            if snapshot.session.daemon_pid != peer.pid
                                || snapshot.session.workspace != incarnation.0
                                || snapshot.session.daemon_started_at != incarnation.1
                            {
                                return Err(ClientError::Protocol(
                                    "snapshot daemon incarnation changed".into(),
                                ));
                            }
                            if tx
                                .send(UiEvent::ScopedSnapshot {
                                    epoch,
                                    snapshot: Box::new(snapshot),
                                })
                                .is_err()
                            {
                                return Ok(());
                            }
                        }
                        Push::Shutdown => {
                            let _ = tx.send(UiEvent::ScopedPush {
                                epoch,
                                push: Box::new(Push::Shutdown),
                            });
                            return Ok(());
                        }
                        push => {
                            if tx
                                .send(UiEvent::ScopedPush {
                                    epoch,
                                    push: Box::new(push),
                                })
                                .is_err()
                            {
                                return Ok(());
                            }
                        }
                    }
                }
            })();
            active.clear();
            snapshots.clear();
            if session.is_ok() || stopping.load(Ordering::Acquire) {
                // The UI went away.
                return;
            }
            if let Err(error) = session {
                if error.is_protocol_mismatch() || error.is_operator_auth_error() {
                    let _ = tx.send(UiEvent::Fatal(error));
                    return;
                }
            }
            if tx.send(UiEvent::ScopedDisconnected { epoch }).is_err() {
                return;
            }
            let until = std::time::Instant::now() + RECONNECT_DELAY;
            while std::time::Instant::now() < until {
                if stopping.load(Ordering::Acquire) {
                    return;
                }
                thread::sleep(STOP_POLL);
            }
        }
    };
    let worker = thread::Builder::new()
        .name("crew-tui-sub".into())
        .spawn(run)
        .expect("starting terminal subscription worker");
    Subscriber {
        stop,
        stream,
        snapshot,
        worker: Some(worker),
    }
}

/// The dashboard's fast requests, slow actions and terminal keys each have
/// their own worker; replies come back on the UI channel.
pub struct Requests {
    main: SyncSender<Command>,
    slow: SyncSender<Command>,
    keys: SyncSender<Command>,
    pub subscriber: Option<Subscriber>,
    stop: Arc<AtomicBool>,
    controls: Vec<WorkerControl>,
    workers: Vec<thread::JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueError {
    Full,
    Closed,
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Full => "request queue is full",
            Self::Closed => "request worker closed",
        })
    }
}

fn enqueue(tx: &SyncSender<Command>, command: Command) -> Result<(), QueueError> {
    tx.try_send(command).map_err(|error| match error {
        mpsc::TrySendError::Full(_) => QueueError::Full,
        mpsc::TrySendError::Disconnected(_) => QueueError::Closed,
    })
}

impl Requests {
    pub fn restart_subscriber(&self) {
        if let Some(subscriber) = &self.subscriber {
            subscriber.restart();
        }
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        for control in &self.controls {
            control.clear();
        }
        if let Some(mut subscriber) = self.subscriber.take() {
            subscriber.shutdown();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }

    pub fn send(&self, command: Command) -> Result<(), QueueError> {
        if self.stop.load(Ordering::Acquire) {
            return Err(QueueError::Closed);
        }
        if let Command::Planned(planned) = &command {
            if let Ok(typed) = planned.typed() {
                return match typed.kind() {
                    crew_client::CommandKind::AgentInput => enqueue(&self.keys, command),
                    crew_client::CommandKind::AgentKill
                    | crew_client::CommandKind::AgentRestart
                    | crew_client::CommandKind::AgentRestartFresh
                    | crew_client::CommandKind::AgentResume
                    | crew_client::CommandKind::Resume
                    | crew_client::CommandKind::AgentSetModel
                    | crew_client::CommandKind::Shutdown
                    | crew_client::CommandKind::ModelsList => enqueue(&self.slow, command),
                    _ => enqueue(&self.main, command),
                };
            }
        }
        match command {
            Command::Keys { .. } | Command::RawKeys { .. } => enqueue(&self.keys, command),
            Command::Kill(_)
            | Command::Restart(_)
            | Command::RestartFresh(_)
            | Command::Resume(_)
            | Command::ResumeLimited { .. }
            | Command::SetModel { .. }
            | Command::StopTeam
            | Command::FetchModels => enqueue(&self.slow, command),
            other => enqueue(&self.main, other),
        }
    }
}

impl Drop for Requests {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
pub fn spawn_requester(socket: PathBuf, tx: Sender<UiEvent>) -> Requests {
    spawn_scoped_requester(socket, tx, None)
}

pub fn spawn_scoped_requester(
    socket: PathBuf,
    tx: Sender<UiEvent>,
    guard: Option<Arc<Mutex<Option<crew_client::ClientScope>>>>,
) -> Requests {
    let stop = Arc::new(AtomicBool::new(false));
    let controls: Vec<_> = (0..3).map(|_| WorkerControl::new(stop.clone())).collect();
    let (main, fast_worker) = spawn_commands(
        socket.clone(),
        tx.clone(),
        "crew-tui-req",
        guard.clone(),
        controls[0].clone(),
    );
    let (slow, slow_worker) = spawn_commands(
        socket.clone(),
        tx.clone(),
        "crew-tui-slow",
        guard.clone(),
        controls[1].clone(),
    );
    let (keys, keys_worker) = spawn_typist(socket, tx, guard, controls[2].clone());
    Requests {
        main,
        slow,
        keys,
        subscriber: None,
        stop,
        controls,
        workers: vec![fast_worker, slow_worker, keys_worker],
    }
}

fn spawn_commands(
    socket: PathBuf,
    tx: Sender<UiEvent>,
    name: &str,
    guard: Option<Arc<Mutex<Option<crew_client::ClientScope>>>>,
    control: WorkerControl,
) -> (SyncSender<Command>, thread::JoinHandle<()>) {
    let (cmd_tx, cmd_rx): (SyncSender<Command>, Receiver<Command>) = mpsc::sync_channel(64);
    let run = move || {
        let mut requester = Requester::new(socket, control.clone(), guard.clone());
        while let Some(command) = next_command(&cmd_rx, &control) {
            if let Command::Planned(planned) = &command {
                if !scope_current(&guard, &planned.scope) {
                    let reply = Reply::Shared {
                        scope: planned.scope.clone(),
                        operation_id: planned.operation_id,
                        outcome: crew_client::OperationOutcome::NotSent {
                            reason: crew_client::ClientReason {
                                details: None,
                                code: "scope_changed".into(),
                                message: "queued request discarded: scope retired".into(),
                            },
                        },
                    };
                    if tx.send(UiEvent::Reply(Box::new(reply))).is_err() {
                        return;
                    }
                    continue;
                }
            }
            let Some(reply) = requester.run(command) else {
                continue;
            };
            if tx.send(UiEvent::Reply(Box::new(reply))).is_err() {
                return;
            }
        }
    };
    let worker = thread::Builder::new()
        .name(name.into())
        .spawn(run)
        .expect("starting terminal request worker");
    (cmd_tx, worker)
}

/// Keys typed into an agent's terminal go over a connection of their own:
/// the daemon's backend answers each call on its own clock (herdr: about a
/// tenth of a second), so behind a screen read a key would arrive late, and
/// keys that piled up meanwhile go in one call.
fn spawn_typist(
    socket: PathBuf,
    tx: Sender<UiEvent>,
    guard: Option<Arc<Mutex<Option<crew_client::ClientScope>>>>,
    control: WorkerControl,
) -> (SyncSender<Command>, thread::JoinHandle<()>) {
    let (keys_tx, keys_rx): (SyncSender<Command>, Receiver<Command>) = mpsc::sync_channel(64);
    let run = move || {
        let mut requester = Requester::new(socket, control.clone(), guard.clone());
        let mut carried: Option<Command> = None;
        loop {
            if control.stop.load(Ordering::Acquire) {
                return;
            }
            let first = match carried.take() {
                Some(command) => command,
                None => match next_command(&keys_rx, &control) {
                    Some(command) => command,
                    None => return,
                },
            };
            if let Command::RawKeys {
                scope,
                id,
                mut text,
            } = first
            {
                carried = gather_scoped_keys(&scope, &id, &mut text, &keys_rx);
                let (reply, planned_rx) = mpsc::channel();
                if tx
                    .send(UiEvent::PlanKeys {
                        scope: scope.clone(),
                        id: id.clone(),
                        text,
                        reply,
                    })
                    .is_err()
                {
                    return;
                }
                let pending_plan = loop {
                    if control.stop.load(Ordering::Acquire) {
                        return;
                    }
                    match planned_rx.recv_timeout(STOP_POLL) {
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        received => break received,
                    }
                };
                let planned = match pending_plan {
                    Ok(Some(planned)) => planned,
                    _ => {
                        discard_scoped_keys(&scope, &id, &keys_rx, &mut carried);
                        continue;
                    }
                };
                if !scope_current(&guard, &planned.scope) {
                    let _ = tx.send(UiEvent::DiscardedInput);
                    let _ = tx.send(UiEvent::Reply(Box::new(discarded(planned))));
                    discard_scoped_keys(&scope, &id, &keys_rx, &mut carried);
                    continue;
                }
                let reply = requester.planned(planned);
                if input_retired(&reply) {
                    let _ = tx.send(UiEvent::DiscardedInput);
                }
                let failed = matches!(&reply, Reply::Shared { outcome,.. } if !matches!(outcome,crew_client::OperationOutcome::Succeeded { .. }));
                if tx.send(UiEvent::Reply(Box::new(reply))).is_err() {
                    return;
                }
                if failed {
                    discard_scoped_keys(&scope, &id, &keys_rx, &mut carried);
                }
                continue;
            }
            if let Command::Planned(_) = &first {
                if let Command::Planned(planned) = &first {
                    if !scope_current(&guard, &planned.scope) {
                        let _ = tx.send(UiEvent::DiscardedInput);
                        let _ = tx.send(UiEvent::Reply(Box::new(discarded((**planned).clone()))));
                        continue;
                    }
                }
                if let Some(reply) = requester.run(first) {
                    if input_retired(&reply) {
                        let _ = tx.send(UiEvent::DiscardedInput);
                    }
                    if tx.send(UiEvent::Reply(Box::new(reply))).is_err() {
                        return;
                    }
                }
                continue;
            }
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
    let worker = thread::Builder::new()
        .name("crew-tui-keys".into())
        .spawn(run)
        .expect("starting terminal input worker");
    (keys_tx, worker)
}

fn discarded(planned: crate::shared::Planned) -> Reply {
    Reply::Shared {
        scope: planned.scope,
        operation_id: planned.operation_id,
        outcome: crew_client::OperationOutcome::NotSent {
            reason: crew_client::ClientReason {
                details: None,
                code: "scope_changed".into(),
                message: "queued terminal input discarded: scope retired".into(),
            },
        },
    }
}

fn input_retired(reply: &Reply) -> bool {
    matches!(reply, Reply::Shared { outcome: crew_client::OperationOutcome::NotSent { reason }, .. } if reason.code == "scope_changed")
}

fn scope_current(
    guard: &Option<Arc<Mutex<Option<crew_client::ClientScope>>>>,
    scope: &crew_client::ClientScope,
) -> bool {
    guard
        .as_ref()
        .is_none_or(|guard| guard.lock().expect("dispatch scope lock").as_ref() == Some(scope))
}

fn gather_scoped_keys(
    scope: &crew_client::ClientScope,
    id: &str,
    text: &mut String,
    rx: &Receiver<Command>,
) -> Option<Command> {
    const LIMIT: usize = 64 * 1024;
    if text.len() > LIMIT {
        let mut cut = LIMIT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let rest = text.split_off(cut);
        return Some(Command::RawKeys {
            scope: scope.clone(),
            id: id.into(),
            text: rest,
        });
    }
    while let Ok(next) = rx.try_recv() {
        match next {
            Command::RawKeys {
                scope: next_scope,
                id: next_id,
                text: more,
            } if next_scope == *scope && next_id == id => {
                let room = LIMIT - text.len();
                if more.len() <= room {
                    text.push_str(&more);
                } else {
                    let mut cut = room;
                    while !more.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.push_str(&more[..cut]);
                    return Some(Command::RawKeys {
                        scope: next_scope,
                        id: next_id,
                        text: more[cut..].into(),
                    });
                }
            }
            other => return Some(other),
        }
    }
    None
}

fn discard_scoped_keys(
    scope: &crew_client::ClientScope,
    id: &str,
    rx: &Receiver<Command>,
    carried: &mut Option<Command>,
) {
    let matches = |command: &Command| matches!(command,Command::RawKeys { scope:captured,id:agent,.. } if captured==scope && agent==id);
    if carried.as_ref().is_some_and(matches) {
        *carried = None;
    }
    // Stop at a different target, preserving the FIFO order of that target.
    if carried.is_none() {
        while let Ok(next) = rx.try_recv() {
            if !matches(&next) {
                *carried = Some(next);
                break;
            }
        }
    }
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
    control: WorkerControl,
    guard: Option<Arc<Mutex<Option<crew_client::ClientScope>>>>,
}

impl Requester {
    fn new(
        socket: PathBuf,
        control: WorkerControl,
        guard: Option<Arc<Mutex<Option<crew_client::ClientScope>>>>,
    ) -> Self {
        Self {
            socket,
            client: None,
            control,
            guard,
        }
    }

    fn connect(&self) -> ClientResult<(Client, crew_core::rpc::HelloInfo)> {
        let connected = Client::operator_with_control(&self.socket, "tui", |handle| {
            self.control.register(handle)
        });
        if connected.is_err() {
            self.control.clear();
        }
        connected
    }
    #[cfg(test)]
    fn call<T: serde::de::DeserializeOwned>(&mut self, call: Call) -> Result<T, String> {
        if self.client.is_none() {
            let (client, _) = self.connect().map_err(|e| e.to_string())?;
            self.client = Some(client);
        }
        let client = self.client.as_mut().expect("client was just connected");
        let timeout = request_timeout(&call);
        match client
            .set_timeout(Some(timeout))
            .and_then(|()| client.call::<T>(call))
        {
            Ok(value) => Ok(value),
            Err(ClientError::Rpc(error)) => Err(error.message),
            Err(other) => {
                // The connection is in an unknown state (timeout, EOF, garbage);
                // drop it so the next request starts clean.
                self.client = None;
                self.control.clear();
                Err(other.to_string())
            }
        }
    }

    /// Stop every agent and the daemon. Agents are given a moment to exit
    /// one after the other, so this gets a connection with more patience.
    #[cfg(test)]
    fn stop_team(&mut self) -> Result<(), String> {
        self.client = None;
        self.control.clear();
        let (mut client, _) = self.connect().map_err(|e| e.to_string())?;
        client
            .set_timeout(Some(request_timeout(&Call::Shutdown { stop_agents: true })))
            .map_err(|e| e.to_string())?;
        match client.call::<serde_json::Value>(Call::Shutdown { stop_agents: true }) {
            Ok(_) => Ok(()),
            Err(ClientError::Rpc(error)) => Err(error.message),
            Err(other) => Err(other.to_string()),
        }
    }

    /// The models agents can be put on. The daemon asks pi for its own,
    /// which takes a moment, so this gets a connection with more patience.
    #[cfg(test)]
    fn models(&mut self) -> Result<Vec<ModelChoice>, String> {
        let (mut client, _) = self.connect().map_err(|e| e.to_string())?;
        client
            .set_timeout(Some(request_timeout(&Call::ModelsList)))
            .map_err(|e| e.to_string())?;
        match client.call::<ModelsResult>(Call::ModelsList) {
            Ok(result) => Ok(result.models),
            Err(ClientError::Rpc(error)) => Err(error.message),
            Err(other) => Err(other.to_string()),
        }
    }

    #[cfg(test)]
    fn action(&mut self, label: String, call: Call) -> Reply {
        let result = self.call::<serde_json::Value>(call).map(|_| ());
        Reply::Action { label, result }
    }

    fn run(&mut self, command: Command) -> Option<Reply> {
        match command {
            Command::Planned(planned) => Some(self.planned(*planned)),
            Command::Quit | Command::SetMouse(_) | Command::RawKeys { .. } => None,
            #[cfg(test)]
            legacy => self.run_legacy(legacy),
            #[cfg(not(test))]
            _ => Some(Reply::Keys(Err("unplanned daemon command rejected".into()))),
        }
    }

    /// Existing transport tests retain their legacy command surface. Production
    /// requests reach the daemon only after the shared planner returns a request.
    #[cfg(test)]
    fn run_legacy(&mut self, command: Command) -> Option<Reply> {
        Some(match command {
            Command::Planned(planned) => self.planned(*planned),
            Command::Quit | Command::SetMouse(_) | Command::RawKeys { .. } => return None,
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
            Command::RestartFresh(id) => self.action(
                format!("restart {id} fresh (no resume)"),
                Call::AgentRestartFresh { id },
            ),
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

    fn planned(&mut self, planned: crate::shared::Planned) -> Reply {
        use crew_client::{ClientReason, OperationOutcome};
        let reason = |code: &str, message: String| ClientReason {
            details: None,
            code: code.into(),
            message,
        };
        let mut execute = || -> Result<crew_client::OperationOutcome, ClientError> {
            let typed = planned
                .typed()
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            let request: crew_core::rpc::Request = serde_json::from_str(
                &typed
                    .encode(planned.operation_id)
                    .map_err(|error| ClientError::Protocol(error.to_string()))?,
            )
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
            if self.client.is_none() {
                let (client, peer) = self.connect()?;
                if planned.scope.daemon_pid.is_some_and(|pid| pid != peer.pid) {
                    return Ok(OperationOutcome::NotSent {
                        reason: reason(
                            "scope_changed",
                            "daemon process changed before dispatch".into(),
                        ),
                    });
                }
                self.client = Some(client);
            }
            let client = self
                .client
                .as_mut()
                .expect("operator connection established");
            client.set_timeout(Some(REQUEST_TIMEOUT))?;
            let proof: Snapshot = client.call(Call::Snapshot {
                messages: Some(0),
                events: Some(0),
            })?;
            if proof.session.workspace != planned.scope.workspace_id
                || planned
                    .scope
                    .daemon_started_at_ms
                    .is_some_and(|at| at != proof.session.daemon_started_at)
                || planned
                    .scope
                    .daemon_pid
                    .is_some_and(|pid| pid != proof.session.daemon_pid)
            {
                return Ok(OperationOutcome::NotSent {
                    reason: reason(
                        "scope_changed",
                        "daemon incarnation changed before dispatch".into(),
                    ),
                });
            }
            let timeout =
                Duration::from_millis(crew_client::policy::method_policy(typed.kind()).timeout_ms);
            client.set_timeout(Some(timeout))?;
            // Authentication and proof reads can block while the UI retires a
            // scope. Fence the captured command again at the actual write.
            if !scope_current(&self.guard, &planned.scope)
                || self.control.stop.load(Ordering::Acquire)
            {
                return Ok(OperationOutcome::NotSent {
                    reason: reason("scope_changed", "scope retired before request write".into()),
                });
            }
            let outcome = match client.call::<serde_json::Value>(request.call) {
                Ok(value) => match typed.decode_result(value) {
                    Ok(result) => OperationOutcome::Succeeded { result },
                    Err(error) => OperationOutcome::OutcomeUnknown {
                        reason: reason("decode", error.to_string()),
                    },
                },
                Err(ClientError::Rpc(error)) => OperationOutcome::Rejected {
                    details: None,
                    code: error.code,
                    message: error.message,
                },
                Err(error) => OperationOutcome::OutcomeUnknown {
                    reason: reason(
                        if matches!(error, ClientError::Closed | ClientError::Io(_)) {
                            "closed"
                        } else {
                            "timeout"
                        },
                        error.to_string(),
                    ),
                },
            };
            Ok(outcome)
        };
        let outcome = execute().unwrap_or_else(|error| OperationOutcome::NotSent {
            reason: reason("connect", error.to_string()),
        });
        if matches!(
            outcome,
            OperationOutcome::OutcomeUnknown { .. } | OperationOutcome::NotSent { .. }
        ) {
            self.client = None;
            self.control.clear();
        }
        Reply::Shared {
            scope: planned.scope,
            operation_id: planned.operation_id,
            outcome,
        }
    }
}

impl Drop for Requester {
    fn drop(&mut self) {
        self.control.clear();
    }
}

/// Forward terminal events. The poll timeout only bounds how long the thread
/// takes to notice `stop`; it does not drive any redraw.
pub fn spawn_input(
    tx: Sender<UiEvent>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<thread::JoinHandle<()>> {
    let run = move || {
        while !stop.load(Ordering::Relaxed) {
            match term::poll(Duration::from_millis(200)) {
                Ok(true) if stop.load(Ordering::Relaxed) => return,
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
    thread::Builder::new()
        .name("crew-tui-input".into())
        .spawn(run)
}

#[cfg(test)]
#[path = "net/live_tests.rs"]
mod live_tests;

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

    fn captured_scope(attempt: u64) -> crew_client::ClientScope {
        crew_client::ClientScope {
            endpoint_id: "tui:test".into(),
            workspace_id: "/work/demo".into(),
            generation: None,
            daemon_pid: Some(4000),
            daemon_started_at_ms: Some(1),
            attempt,
        }
    }

    #[test]
    fn scoped_coalescing_preserves_utf8_order_and_never_crosses_a_scope_or_target() {
        let scope = captured_scope(1);
        let (tx, rx) = mpsc::channel();
        let raw = |scope: crew_client::ClientScope, id: &str, text: &str| Command::RawKeys {
            scope,
            id: id.into(),
            text: text.into(),
        };
        let original = format!("{}日a", "x".repeat(65535));
        tx.send(raw(scope.clone(), "a", "b")).unwrap();
        tx.send(raw(captured_scope(2), "a", "new epoch")).unwrap();
        let mut first = original.clone();
        let carried = gather_scoped_keys(&scope, "a", &mut first, &rx).unwrap();
        assert_eq!(first.len(), 65535);
        let Command::RawKeys {
            text: mut second, ..
        } = carried
        else {
            panic!("carried");
        };
        let next = gather_scoped_keys(&scope, "a", &mut second, &rx).unwrap();
        assert_eq!(format!("{first}{second}"), format!("{original}b"));
        assert_eq!(next, raw(captured_scope(2), "a", "new epoch"));
        tx.send(raw(scope.clone(), "b", "other terminal")).unwrap();
        let mut current = "a".into();
        assert_eq!(
            gather_scoped_keys(&scope, "a", &mut current, &rx),
            Some(raw(scope, "b", "other terminal"))
        );
        assert_eq!(current, "a");
    }

    #[test]
    fn failed_terminal_scope_discards_its_unsent_bytes_and_preserves_other_targets() {
        let scope = captured_scope(1);
        let (tx, rx) = mpsc::channel();
        for (id, text) in [("a", "unsent1"), ("a", "unsent2"), ("b", "keep")] {
            tx.send(Command::RawKeys {
                scope: scope.clone(),
                id: id.into(),
                text: text.into(),
            })
            .unwrap();
        }
        let mut carried = Some(Command::RawKeys {
            scope: scope.clone(),
            id: "a".into(),
            text: "held".into(),
        });
        discard_scoped_keys(&scope, "a", &rx, &mut carried);
        assert_eq!(
            carried,
            Some(Command::RawKeys {
                scope,
                id: "b".into(),
                text: "keep".into()
            })
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn worker_queue_full_is_visible_and_nonblocking() {
        let (tx, rx) = mpsc::sync_channel(1);
        enqueue(&tx, Command::Quit).unwrap();
        assert_eq!(
            enqueue(&tx, Command::SetMouse(false)),
            Err(QueueError::Full)
        );
        assert_eq!(rx.recv().unwrap(), Command::Quit);
        drop(rx);
        assert_eq!(enqueue(&tx, Command::Quit), Err(QueueError::Closed));
    }

    fn connected_app() -> crate::App {
        let mut app = crate::App::new();
        app.apply_snapshot(fixture::snapshot(1_000), 1_000);
        app
    }

    fn pump(app: &mut crate::App, requests: &Requests, event: UiEvent) {
        app.shared.set_clock(crew_client::ClockSample {
            wall_ms: 1_000,
            monotonic_ms: 1_000,
        });
        crate::handle(
            app,
            event,
            requests,
            20,
            ratatui::layout::Rect::new(0, 0, 80, 24),
        )
        .unwrap();
    }

    #[test]
    fn real_core_and_scoped_workers_keep_raw_input_prompts_and_screen_reads_independent() {
        if crate::test_support::isolate_operator_test("net::tests::real_core_and_scoped_workers_keep_raw_input_prompts_and_screen_reads_independent") {
            return;
        }
        for prompt_first in [false, true] {
            let (_temp, path) = socket();
            let daemon = FakeDaemon::start_with_delay(
                &path,
                PROTOCOL_VERSION,
                Some(("agent_input", Duration::from_millis(500))),
            );
            let mut app = connected_app();
            let (tx, rx) = mpsc::channel();
            let mut requests = spawn_scoped_requester(path, tx, Some(app.shared.dispatch_guard()));
            let scope = app.shared.read(|core| core.scope().unwrap().clone());
            let submit = |app: &mut crate::App| {
                let planned = app.prepare(
                    Command::Type {
                        id: "claude-1".into(),
                        text: "inspect".into(),
                    },
                    1_001,
                );
                assert_eq!(
                    planned.len(),
                    1,
                    "pending raw must not reject a submitted prompt"
                );
                crate::send_planned(app, planned.into_iter().next().unwrap(), &requests, 1_001);
            };
            if prompt_first {
                submit(&mut app);
            }
            for text in ["日本", "!"] {
                requests
                    .send(Command::RawKeys {
                        scope: scope.clone(),
                        id: "claude-1".into(),
                        text: text.into(),
                    })
                    .unwrap();
            }
            let output = app
                .prepare(
                    Command::FetchOutput {
                        id: "claude-1".into(),
                    },
                    1_002,
                )
                .remove(0);
            let output_id = output.operation_id;
            crate::send_planned(&mut app, output, &requests, 1_002);
            let mut submitted = prompt_first;
            let mut planned_raw = 0;
            let mut completions = 0;
            let mut read_completed = false;
            loop {
                let raw_sent: String = daemon
                    .calls()
                    .iter()
                    .filter_map(|call| match call {
                        Call::AgentInput {
                            text,
                            submit: false,
                            ..
                        } => text.clone(),
                        _ => None,
                    })
                    .collect();
                if completions == planned_raw + 2
                    && submitted
                    && planned_raw > 0
                    && raw_sent == "日本!"
                {
                    break;
                }
                let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
                let raw = matches!(event, UiEvent::PlanKeys { .. });
                if let UiEvent::Reply(reply) = &event {
                    let Reply::Shared {
                        operation_id,
                        outcome,
                        ..
                    } = reply.as_ref()
                    else {
                        panic!("production must use shared outcomes")
                    };
                    assert!(
                        matches!(outcome, crew_client::OperationOutcome::Succeeded { .. }),
                        "{outcome:?}"
                    );
                    if *operation_id == output_id {
                        read_completed = true;
                    } else {
                        assert!(
                            read_completed,
                            "a screen read must not queue behind terminal IO"
                        );
                    }
                    completions += 1;
                }
                pump(&mut app, &requests, event);
                if raw {
                    planned_raw += 1;
                    if !submitted {
                        submit(&mut app);
                        submitted = true;
                    }
                }
            }
            let calls = daemon.calls();
            let inputs: Vec<_> = calls
                .iter()
                .filter_map(|call| match call {
                    Call::AgentInput { id, text, submit } => Some((id, text, *submit)),
                    _ => None,
                })
                .collect();
            assert!(inputs.iter().all(|(id, _, _)| *id == "claude-1"));
            let raw: String = inputs
                .iter()
                .filter(|(_, _, submit)| !submit)
                .map(|(_, text, _)| text.as_deref().unwrap())
                .collect();
            assert_eq!(raw, "日本!");
            assert_eq!(
                inputs
                    .iter()
                    .filter(|(_, text, submit)| *submit && text.as_deref() == Some("inspect"))
                    .count(),
                1
            );
            assert!(
                calls.iter().any(|call| matches!(
                    call,
                    Call::Snapshot {
                        messages: Some(0),
                        events: Some(0)
                    }
                )),
                "each lane verifies captured incarnation before dispatch"
            );
            requests.shutdown();
            assert!(requests.workers.is_empty());
            daemon.stop();
        }
    }

    #[test]
    fn retirement_between_key_planning_and_dispatch_is_visible_and_never_reaches_the_socket() {
        let (_temp, path) = socket();
        let daemon = FakeDaemon::start(&path);
        let mut app = connected_app();
        let (tx, rx) = mpsc::channel();
        let mut requests = spawn_scoped_requester(path, tx, Some(app.shared.dispatch_guard()));
        let old = app.shared.read(|core| core.scope().unwrap().clone());
        requests
            .send(Command::RawKeys {
                scope: old,
                id: "claude-1".into(),
                text: "pwd\r".into(),
            })
            .unwrap();
        let UiEvent::PlanKeys {
            id, text, reply, ..
        } = rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("raw planning handshake")
        };
        let planned = app.prepare(Command::Keys { id, text }, 1_001).remove(0);
        let mut replacement = fixture::snapshot(1_000);
        replacement.session.daemon_pid = 9000;
        app.apply_snapshot(replacement, 1_002);
        let revision = app.shared.read(crew_client::ClientCore::revision);
        reply.send(Some(planned)).unwrap();
        pump(
            &mut app,
            &requests,
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        );
        assert!(app.status.as_ref().unwrap().text.contains("discarded"));
        pump(
            &mut app,
            &requests,
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        );
        assert_eq!(
            app.shared.read(crew_client::ClientCore::revision),
            revision,
            "a stale outcome cannot close the replacement scope"
        );
        assert!(app.connected);
        assert!(daemon.calls().is_empty());
        requests.shutdown();
        daemon.stop();
    }

    #[test]
    fn scope_retirement_during_a_proof_read_prevents_the_following_mutation() {
        if crate::test_support::isolate_operator_test(
            "net::tests::scope_retirement_during_a_proof_read_prevents_the_following_mutation",
        ) {
            return;
        }
        let (_temp, path) = socket();
        let daemon = FakeDaemon::start_with_delay(
            &path,
            PROTOCOL_VERSION,
            Some(("snapshot", Duration::from_millis(200))),
        );
        let mut app = connected_app();
        let (tx, rx) = mpsc::channel();
        let mut requests = spawn_scoped_requester(path, tx, Some(app.shared.dispatch_guard()));
        crate::send_shared(
            &mut app,
            Command::Type {
                id: "claude-1".into(),
                text: "stale prompt".into(),
            },
            &requests,
            1_001,
        );
        daemon.wait_for(|calls| {
            calls
                .iter()
                .any(|call| matches!(call, Call::Snapshot { .. }))
        });
        app.disconnected_at(1_002);
        app.apply_snapshot(fixture::snapshot(1_000), 1_003);
        let revision = app.shared.read(crew_client::ClientCore::revision);
        pump(
            &mut app,
            &requests,
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        );
        assert!(app.status.as_ref().unwrap().text.contains("discarded"));
        pump(
            &mut app,
            &requests,
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        );
        assert_eq!(app.shared.read(crew_client::ClientCore::revision), revision);
        assert!(app.connected);
        assert!(!daemon
            .calls()
            .iter()
            .any(|call| matches!(call, Call::AgentInput { .. })));
        requests.shutdown();
        daemon.stop();
    }

    #[test]
    fn connection_registration_cannot_escape_a_stop_that_wins_first_or_races_it() {
        use std::sync::Barrier;
        let (_temp, path) = socket();
        let listener = UnixListener::bind(&path).unwrap();
        for order in [0, 1, 2] {
            let client = Client::connect(&path).unwrap();
            let (mut server, _) = listener.accept().unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let control = WorkerControl::new(Arc::new(AtomicBool::new(false)));
            let handle = client.shutdown_handle().unwrap();
            if order == 0 {
                control.stop.store(true, Ordering::Release);
                control.register(handle);
            } else if order == 1 {
                control.register(handle);
                control.stop.store(true, Ordering::Release);
                control.clear();
            } else {
                let ready = Arc::new(Barrier::new(2));
                let (registered, raced) = (control.clone(), ready.clone());
                let worker = thread::spawn(move || {
                    raced.wait();
                    registered.register(handle);
                });
                ready.wait();
                control.stop.store(true, Ordering::Release);
                control.clear();
                worker.join().unwrap();
            }
            use std::io::Read;
            assert_eq!(
                server.read(&mut [0u8]).unwrap(),
                0,
                "stop must close a late registered connection"
            );
            assert!(control.active.lock().unwrap().is_none());
        }
    }

    #[test]
    fn shutdown_joins_held_hellos_and_a_key_worker_waiting_for_the_ui() {
        if crate::test_support::isolate_operator_test(
            "net::tests::shutdown_joins_held_hellos_and_a_key_worker_waiting_for_the_ui",
        ) {
            return;
        }
        let (_temp, path) = socket();
        let daemon = FakeDaemon::start_with_delay(
            &path,
            PROTOCOL_VERSION,
            Some(("hello", Duration::from_secs(10))),
        );
        let mut app = connected_app();
        let (tx, rx) = mpsc::channel();
        let mut requests =
            spawn_scoped_requester(path.clone(), tx.clone(), Some(app.shared.dispatch_guard()));
        requests.subscriber = Some(spawn_subscriber(path, tx, Arc::new(AtomicBool::new(false))));
        for command in [
            Command::FetchOutput {
                id: "claude-1".into(),
            },
            Command::FetchModels,
        ] {
            crate::send_shared(&mut app, command, &requests, 1_001);
        }
        requests
            .send(Command::RawKeys {
                scope: app.shared.read(|core| core.scope().unwrap().clone()),
                id: "claude-1".into(),
                text: "x".into(),
            })
            .unwrap();
        let held_plan = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(held_plan, UiEvent::PlanKeys { .. }));
        daemon.wait_for(|calls| {
            calls
                .iter()
                .filter(|call| matches!(call, Call::Hello(_)))
                .count()
                == 3
        });
        let before = Instant::now();
        requests.shutdown();
        assert!(
            before.elapsed() < Duration::from_secs(1),
            "registered IO must not wait for hello or policy deadlines"
        );
        assert!(requests.workers.is_empty());
        assert!(requests.subscriber.is_none());
        assert_eq!(requests.send(Command::Quit), Err(QueueError::Closed));
        drop(held_plan);
        daemon.stop();
        assert!(daemon.workers.lock().unwrap().is_empty());
    }

    #[test]
    fn shutdown_after_a_written_input_is_unknown_and_never_replayed() {
        if crate::test_support::isolate_operator_test(
            "net::tests::shutdown_after_a_written_input_is_unknown_and_never_replayed",
        ) {
            return;
        }
        let (_temp, path) = socket();
        let daemon = FakeDaemon::start_with_delay(
            &path,
            PROTOCOL_VERSION,
            Some(("agent_input", Duration::from_secs(10))),
        );
        let mut app = connected_app();
        let (tx, rx) = mpsc::channel();
        let mut requests = spawn_scoped_requester(path, tx, Some(app.shared.dispatch_guard()));
        crate::send_shared(
            &mut app,
            Command::Type {
                id: "claude-1".into(),
                text: "one prompt".into(),
            },
            &requests,
            1_001,
        );
        daemon.wait_for(|calls| {
            calls
                .iter()
                .any(|call| matches!(call, Call::AgentInput { .. }))
        });
        let before = Instant::now();
        requests.shutdown();
        assert!(before.elapsed() < Duration::from_secs(1));
        let UiEvent::Reply(reply) = rx.recv_timeout(Duration::from_secs(1)).unwrap() else {
            panic!("written input completion")
        };
        assert!(matches!(
            reply.as_ref(),
            Reply::Shared {
                outcome: crew_client::OperationOutcome::OutcomeUnknown { .. },
                ..
            }
        ));
        app.apply_reply(*reply, 1_002);
        app.disconnected_at(1_003);
        app.apply_snapshot(fixture::snapshot(1_000), 1_004);
        assert!(app.shared.take_requests().is_empty());
        assert_eq!(
            daemon
                .calls()
                .iter()
                .filter(|call| matches!(call, Call::AgentInput { .. }))
                .count(),
            1
        );
        daemon.stop();
    }

    const OPERATOR_TOKEN: &str = "synthetic-tui-operator-token";

    struct FakeDaemon {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        connections: Arc<Mutex<Vec<UnixStream>>>,
        subscribers: Arc<Mutex<Vec<UnixStream>>>,
        calls: Arc<Mutex<Vec<Call>>>,
        workers: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        acceptor: Mutex<Option<thread::JoinHandle<()>>>,
    }

    impl FakeDaemon {
        fn start(path: &Path) -> Self {
            Self::start_with_protocol(path, PROTOCOL_VERSION)
        }

        fn start_with_protocol(path: &Path, protocol: u32) -> Self {
            Self::start_with_delay(path, protocol, None)
        }

        fn start_with_delay(
            path: &Path,
            protocol: u32,
            delay: Option<(&'static str, Duration)>,
        ) -> Self {
            let _ = std::fs::remove_file(path);
            crew_core::paths::write_private_file(
                &crew_core::paths::operator_token_path(path),
                OPERATOR_TOKEN.as_bytes(),
            )
            .unwrap();
            let listener = UnixListener::bind(path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let daemon = Self {
                path: path.to_path_buf(),
                stop: Arc::new(AtomicBool::new(false)),
                connections: Arc::default(),
                subscribers: Arc::default(),
                calls: Arc::default(),
                workers: Arc::default(),
                acceptor: Mutex::new(None),
            };
            let (stop, connections, subscribers, calls) = (
                daemon.stop.clone(),
                daemon.connections.clone(),
                daemon.subscribers.clone(),
                daemon.calls.clone(),
            );
            let workers = daemon.workers.clone();
            let acceptor = thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            connections
                                .lock()
                                .unwrap()
                                .push(stream.try_clone().unwrap());
                            let (subscribers, calls, stopping) =
                                (subscribers.clone(), calls.clone(), stop.clone());
                            workers.lock().unwrap().push(thread::spawn(move || {
                                serve(stream, subscribers, calls, protocol, delay, stopping)
                            }));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
            *daemon.acceptor.lock().unwrap() = Some(acceptor);
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
            if self.stop.swap(true, Ordering::AcqRel) {
                return;
            }
            if let Some(acceptor) = self.acceptor.lock().unwrap().take() {
                acceptor.join().unwrap();
            }
            for stream in self.connections.lock().unwrap().drain(..) {
                let _ = stream.shutdown(Shutdown::Both);
            }
            let _ = std::fs::remove_file(&self.path);
            for worker in self.workers.lock().unwrap().drain(..) {
                worker.join().unwrap();
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn wait_for(&self, predicate: impl Fn(&[Call]) -> bool) {
            let until = Instant::now() + Duration::from_secs(2);
            loop {
                if predicate(&self.calls()) {
                    return;
                }
                assert!(
                    Instant::now() < until,
                    "scripted daemon did not observe expected request"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn serve(
        stream: UnixStream,
        subscribers: Arc<Mutex<Vec<UnixStream>>>,
        calls: Arc<Mutex<Vec<Call>>>,
        protocol: u32,
        delay: Option<(&'static str, Duration)>,
        stop: Arc<AtomicBool>,
    ) {
        let mut writer = stream.try_clone().unwrap();
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let request: Request = serde_json::from_str(&line).unwrap();
            calls.lock().unwrap().push(request.call.clone());
            if let Some((method, duration)) = delay {
                if request.call.method() == method {
                    let until = Instant::now() + duration;
                    while Instant::now() < until {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            }
            let frame = match &request.call {
                Call::Hello(hello) if hello.token.as_deref() != Some(OPERATOR_TOKEN) => ServerFrame::err(
                    request.id,
                    RpcError::unauthorized("operator token does not match this daemon (it may have restarted; reconnect)"),
                ),
                Call::Hello(_) => ServerFrame::ok(
                    request.id,
                    serde_json::to_value(HelloInfo {
                        role: Role::Operator,
                        agent_id: None,
                        project: Some("checkout-rewrite".into()),
                        version: "test".into(),
                        protocol,
                        pid: fixture::snapshot(1_000).session.daemon_pid,
                        web_proof: None,
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
                Call::AgentInput { .. } | Call::AgentFocus { .. } => ServerFrame::ok(request.id, json!({"ok": true})),
                Call::ModelsList => ServerFrame::ok(request.id, json!({"models": []})),
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

    #[test]
    fn subscriber_reports_operator_token_errors_and_stops_reconnecting() {
        if crate::test_support::isolate_operator_test(
            "net::tests::subscriber_reports_operator_token_errors_and_stops_reconnecting",
        ) {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        for (case, expected) in [
            ("missing", "no operator token"),
            ("mode", "mode 0600"),
            ("wrong", "does not match"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("daemon.sock");
            let token = crew_core::paths::operator_token_path(&path);
            let daemon = FakeDaemon::start(&path);
            match case {
                "missing" => std::fs::remove_file(&token).unwrap(),
                "mode" => std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644))
                    .unwrap(),
                "wrong" => {
                    crew_core::paths::write_private_file(&token, b"wrong-credential-do-not-print")
                        .unwrap()
                }
                _ => unreachable!(),
            }
            let (tx, rx) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let _subscriber = spawn_subscriber(path, tx, stop.clone());
            let event = rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let UiEvent::Fatal(error) = event else {
                panic!("operator credentials must give a visible terminal error");
            };
            assert!(error.to_string().contains(expected), "{error}");
            assert!(!error.to_string().contains("wrong-credential-do-not-print"));
            thread::sleep(RECONNECT_DELAY + Duration::from_millis(100));
            assert!(
                rx.try_recv().is_err(),
                "credentials must not cause a reconnect loop"
            );
            assert_eq!(daemon.calls().len(), usize::from(case != "mode"));
            stop.store(true, Ordering::Relaxed);
            daemon.stop();
        }
    }

    #[test]
    fn subscriber_stops_after_a_protocol_mismatch_on_reconnect() {
        if crate::test_support::isolate_operator_test(
            "net::tests::subscriber_stops_after_a_protocol_mismatch_on_reconnect",
        ) {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon.sock");
        let daemon = FakeDaemon::start(&path);
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let _subscriber = spawn_subscriber(path.clone(), tx, stop.clone());
        expect(&rx, "initial snapshot", |event| match event {
            UiEvent::ScopedSnapshot { .. } => Some(()),
            _ => None,
        });
        daemon.stop();
        expect(&rx, "disconnect", |event| match event {
            UiEvent::ScopedDisconnected { .. } => Some(()),
            _ => None,
        });
        let daemon = FakeDaemon::start_with_protocol(&path, 99);
        let event = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            matches!(event, UiEvent::Fatal(error) if error.is_protocol_mismatch() && error.to_string().contains("restart")),
            "mismatched daemon must report a terminal version error"
        );
        thread::sleep(RECONNECT_DELAY + Duration::from_millis(100));
        assert_eq!(
            daemon
                .calls()
                .iter()
                .filter(|call| matches!(call, Call::Hello(_)))
                .count(),
            1,
            "must not reconnect forever after a version error"
        );
        stop.store(true, Ordering::Relaxed);
        daemon.stop();
    }

    #[test]
    fn fresh_restart_uses_its_own_rpc_without_blocking_screen_reads() {
        if crate::test_support::isolate_operator_test(
            "net::tests::fresh_restart_uses_its_own_rpc_without_blocking_screen_reads",
        ) {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon.sock");
        let daemon = FakeDaemon::start_with_delay(
            &path,
            PROTOCOL_VERSION,
            Some(("agent_restart_fresh", Duration::from_millis(3200))),
        );
        let (tx, rx) = mpsc::channel();
        let requests = spawn_requester(path, tx);
        requests
            .send(Command::RestartFresh("worker".into()))
            .unwrap();
        requests
            .send(Command::FetchOutput {
                id: "worker".into(),
            })
            .unwrap();
        let first = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("screen read waits behind fresh restart");
        assert!(
            matches!(first, UiEvent::Reply(reply) if matches!(*reply, Reply::Output { result: Ok(_), .. }))
        );
        let next = rx.recv_timeout(Duration::from_secs(4)).unwrap();
        assert!(
            matches!(next, UiEvent::Reply(reply) if matches!(*reply, Reply::Action { result: Ok(()), .. }))
        );
        let calls = daemon.calls();
        assert!(calls
            .iter()
            .any(|call| matches!(call, Call::AgentRestartFresh { id } if id == "worker")));
        assert!(!calls
            .iter()
            .any(|call| matches!(call, Call::AgentRestart { .. })));
        daemon.stop();
    }

    #[test]
    fn fresh_restart_has_a_lifecycle_timeout() {
        assert_eq!(
            request_timeout(&Call::AgentRestartFresh {
                id: "worker".into()
            }),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn slow_lifecycle_actions_do_not_hit_the_fast_read_timeout() {
        if crate::test_support::isolate_operator_test(
            "net::tests::slow_lifecycle_actions_do_not_hit_the_fast_read_timeout",
        ) {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon.sock");
        let daemon = FakeDaemon::start_with_delay(
            &path,
            PROTOCOL_VERSION,
            Some(("agent_restart", Duration::from_millis(3200))),
        );
        let mut requester = Requester {
            socket: path,
            client: None,
            control: WorkerControl::new(Arc::new(AtomicBool::new(false))),
            guard: None,
        };
        let reply = requester.run(Command::Restart("worker".into())).unwrap();
        daemon.stop();
        assert!(
            matches!(reply, Reply::Action { result: Ok(()), .. }),
            "completed restart must not be reported as timed out"
        );
    }

    #[test]
    fn shutdown_push_is_forwarded_as_a_terminal_event() {
        if crate::test_support::isolate_operator_test(
            "net::tests::shutdown_push_is_forwarded_as_a_terminal_event",
        ) {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon.sock");
        let daemon = FakeDaemon::start(&path);
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let _subscriber = spawn_subscriber(path, tx, stop.clone());
        expect(&rx, "snapshot", |event| match event {
            UiEvent::ScopedSnapshot { .. } => Some(()),
            _ => None,
        });
        daemon.push(Push::Shutdown);
        let event = rx.recv_timeout(Duration::from_secs(2));
        stop.store(true, Ordering::Relaxed);
        daemon.stop();
        assert!(matches!(event, Ok(UiEvent::ScopedPush { push,.. }) if *push == Push::Shutdown));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn slow_model_listing_does_not_block_screen_requests() {
        if crate::test_support::isolate_operator_test(
            "net::tests::slow_model_listing_does_not_block_screen_requests",
        ) {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon.sock");
        let daemon = FakeDaemon::start_with_delay(
            &path,
            PROTOCOL_VERSION,
            Some(("models_list", Duration::from_secs(1))),
        );
        let (tx, rx) = mpsc::channel();
        let requests = spawn_requester(path, tx);
        requests.send(Command::FetchModels).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !daemon.calls().contains(&Call::ModelsList) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        requests
            .send(Command::FetchOutput {
                id: "worker".into(),
            })
            .unwrap();
        let event = rx.recv_timeout(Duration::from_millis(500));
        daemon.stop();
        assert!(
            matches!(event, Ok(UiEvent::Reply(reply)) if matches!(*reply, Reply::Output { result: Ok(_), .. })),
            "screen requests must finish while models are still loading"
        );
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
        if crate::test_support::isolate_operator_test(
            "net::tests::subscriber_delivers_snapshot_pushes_resync_and_reconnects",
        ) {
            return;
        }
        let (_dir, path) = socket();
        let daemon = FakeDaemon::start(&path);
        probe(&path).unwrap();

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let _subscriber = spawn_subscriber(path.clone(), tx, stop.clone());

        let snapshot = expect(&rx, "the initial snapshot", |e| match e {
            UiEvent::ScopedSnapshot { snapshot: s, .. } => Some(s),
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
            UiEvent::ScopedPush { push: p, .. } => Some(*p),
            _ => None,
        });
        assert_eq!(got, Push::Message(message));

        // Falling behind makes the daemon ask for a fresh snapshot.
        daemon.push(Push::Resync);
        expect(&rx, "the resync snapshot", |e| match e {
            UiEvent::ScopedSnapshot { .. } => Some(()),
            _ => None,
        });

        // The daemon goes away: the UI is told, and the thread keeps retrying.
        daemon.stop();
        expect(&rx, "the disconnect notice", |e| match e {
            UiEvent::ScopedDisconnected { .. } => Some(()),
            _ => None,
        });
        assert!(probe(&path).is_err());

        let daemon = FakeDaemon::start(&path);
        expect(&rx, "the snapshot after reconnecting", |e| match e {
            UiEvent::ScopedSnapshot { .. } => Some(()),
            _ => None,
        });

        // A graceful shutdown ends the subscription and reaches the UI.
        daemon.push(Push::Shutdown);
        expect(&rx, "the shutdown notice", |e| match e {
            UiEvent::ScopedPush { push, .. } if *push == Push::Shutdown => Some(()),
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
        if crate::test_support::isolate_operator_test(
            "net::tests::keys_that_piled_up_go_together_and_another_terminals_wait_their_turn",
        ) {
            return;
        }
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
        if crate::test_support::isolate_operator_test(
            "net::tests::requester_runs_commands_and_reports_errors",
        ) {
            return;
        }
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
        // Fast and lifecycle calls each reuse one connection; quitting sends nothing.
        let hellos = calls.iter().filter(|c| matches!(c, Call::Hello(_))).count();
        assert_eq!(hellos, 2);
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
