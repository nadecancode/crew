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

use crew_core::client::{Client, ClientError, ClientResult};
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
    Input(term::Event),
    Reply(Box<Reply>),
    Fatal(ClientError),
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
pub fn spawn_subscriber(
    socket: PathBuf,
    tx: Sender<UiEvent>,
    stop: Arc<AtomicBool>,
) -> Arc<Mutex<Option<crew_core::client::FrameWriter>>> {
    let control = Arc::new(Mutex::new(None));
    let active = control.clone();
    let run = move || {
        let mut epoch = 0u64;
        while !stop.load(Ordering::Relaxed) {
            epoch = match epoch.checked_add(1) {
                Some(epoch) => epoch,
                None => return,
            };
            let session = (|| -> ClientResult<()> {
                let (mut client, peer) = Client::operator(&socket, "tui")?;
                client.call_value(Call::Subscribe)?;
                let (mut stream, writer) = client.split();
                *active.lock().expect("subscriber control lock") = Some(writer);
                let snapshot = fetch_snapshot(&socket)?;
                if snapshot.session.daemon_pid != peer.pid {
                    return Err(ClientError::Protocol(
                        "snapshot belongs to a different daemon".into(),
                    ));
                }
                let incarnation = (
                    snapshot.session.workspace.clone(),
                    snapshot.session.daemon_started_at,
                );
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
                    match push {
                        Push::Resync => {
                            let snapshot = fetch_snapshot(&socket)?;
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
            *active.lock().expect("subscriber control lock") = None;
            if session.is_ok() || stop.load(Ordering::Relaxed) {
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
            thread::sleep(RECONNECT_DELAY);
        }
    };
    let _ = thread::Builder::new()
        .name("crew-tui-sub".into())
        .spawn(run);
    control
}

/// The dashboard's fast requests, slow actions and terminal keys each have
/// their own worker; replies come back on the UI channel.
#[derive(Clone)]
pub struct Requests {
    main: SyncSender<Command>,
    slow: SyncSender<Command>,
    keys: SyncSender<Command>,
    pub subscriber: Option<Arc<Mutex<Option<crew_core::client::FrameWriter>>>>,
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
        if let Some(control) = &self.subscriber {
            if let Some(writer) = control.lock().expect("subscriber control lock").as_ref() {
                writer.shutdown();
            }
        }
    }
    pub fn send(&self, command: Command) -> Result<(), QueueError> {
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

#[cfg(test)]
pub fn spawn_requester(socket: PathBuf, tx: Sender<UiEvent>) -> Requests {
    spawn_scoped_requester(socket, tx, None)
}

pub fn spawn_scoped_requester(
    socket: PathBuf,
    tx: Sender<UiEvent>,
    guard: Option<Arc<Mutex<Option<crew_client::ClientScope>>>>,
) -> Requests {
    let main = spawn_commands(socket.clone(), tx.clone(), "crew-tui-req", guard.clone());
    let slow = spawn_commands(socket.clone(), tx.clone(), "crew-tui-slow", guard.clone());
    let keys = spawn_typist(socket, tx, guard);
    Requests {
        main,
        slow,
        keys,
        subscriber: None,
    }
}

fn spawn_commands(
    socket: PathBuf,
    tx: Sender<UiEvent>,
    name: &str,
    guard: Option<Arc<Mutex<Option<crew_client::ClientScope>>>>,
) -> SyncSender<Command> {
    let (cmd_tx, cmd_rx): (SyncSender<Command>, Receiver<Command>) = mpsc::sync_channel(64);
    let run = move || {
        let mut requester = Requester {
            socket,
            client: None,
        };
        for command in cmd_rx {
            if let Command::Planned(planned) = &command {
                if !scope_current(&guard, &planned.scope) {
                    let reply = Reply::Shared {
                        scope: planned.scope.clone(),
                        operation_id: planned.operation_id,
                        outcome: crew_client::OperationOutcome::NotSent {
                            reason: crew_client::ClientReason {
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
    let _ = thread::Builder::new().name(name.into()).spawn(run);
    cmd_tx
}

/// Keys typed into an agent's terminal go over a connection of their own:
/// the daemon's backend answers each call on its own clock (herdr: about a
/// tenth of a second), so behind a screen read a key would arrive late, and
/// keys that piled up meanwhile go in one call.
fn spawn_typist(
    socket: PathBuf,
    tx: Sender<UiEvent>,
    guard: Option<Arc<Mutex<Option<crew_client::ClientScope>>>>,
) -> SyncSender<Command> {
    let (keys_tx, keys_rx): (SyncSender<Command>, Receiver<Command>) = mpsc::sync_channel(64);
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
                let planned = match planned_rx.recv() {
                    Ok(Some(planned)) => planned,
                    _ => {
                        discard_scoped_keys(&scope, &id, &keys_rx, &mut carried);
                        continue;
                    }
                };
                if !scope_current(&guard, &planned.scope) {
                    discard_scoped_keys(&scope, &id, &keys_rx, &mut carried);
                    continue;
                }
                let reply = requester.planned(planned);
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
                        continue;
                    }
                }
                if let Some(reply) = requester.run(first) {
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
    let _ = thread::Builder::new()
        .name("crew-tui-keys".into())
        .spawn(run);
    keys_tx
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
}

impl Requester {
    #[cfg(test)]
    fn call<T: serde::de::DeserializeOwned>(&mut self, call: Call) -> Result<T, String> {
        if self.client.is_none() {
            let (client, _) = Client::operator(&self.socket, "tui").map_err(|e| e.to_string())?;
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
                Err(other.to_string())
            }
        }
    }

    /// Stop every agent and the daemon. Agents are given a moment to exit
    /// one after the other, so this gets a connection with more patience.
    #[cfg(test)]
    fn stop_team(&mut self) -> Result<(), String> {
        self.client = None;
        let (mut client, _) = Client::operator(&self.socket, "tui").map_err(|e| e.to_string())?;
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
        let (mut client, _) = Client::operator(&self.socket, "tui").map_err(|e| e.to_string())?;
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
                let (client, peer) = Client::operator(&self.socket, "tui")?;
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
            let outcome = match client.call::<serde_json::Value>(request.call) {
                Ok(value) => match typed.decode_result(value) {
                    Ok(result) => OperationOutcome::Succeeded { result },
                    Err(error) => OperationOutcome::OutcomeUnknown {
                        reason: reason("decode", error.to_string()),
                    },
                },
                Err(ClientError::Rpc(error)) => OperationOutcome::Rejected {
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
        }
        Reply::Shared {
            scope: planned.scope,
            operation_id: planned.operation_id,
            outcome,
        }
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

    const OPERATOR_TOKEN: &str = "synthetic-tui-operator-token";

    struct FakeDaemon {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        connections: Arc<Mutex<Vec<UnixStream>>>,
        subscribers: Arc<Mutex<Vec<UnixStream>>>,
        calls: Arc<Mutex<Vec<Call>>>,
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
                            thread::spawn(move || {
                                serve(stream, subscribers, calls, protocol, delay)
                            });
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
        protocol: u32,
        delay: Option<(&'static str, Duration)>,
    ) {
        let mut writer = stream.try_clone().unwrap();
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let request: Request = serde_json::from_str(&line).unwrap();
            calls.lock().unwrap().push(request.call.clone());
            if let Some((method, duration)) = delay {
                if request.call.method() == method {
                    thread::sleep(duration);
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
            spawn_subscriber(path, tx, stop.clone());
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
        spawn_subscriber(path.clone(), tx, stop.clone());
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
        spawn_subscriber(path, tx, stop.clone());
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
        spawn_subscriber(path.clone(), tx, stop.clone());

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
