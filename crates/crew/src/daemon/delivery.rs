//! Getting bus messages into an agent's session.
//!
//! Two channels exist, tried in this order:
//!
//! 1. **Socket** — the agent's runtime keeps a connection open to the daemon
//!    (`Call::Listen`, held by `crew bridge`, which a small Claude Code mod
//!    runs inside the session). The daemon pushes a `Deliver` frame and the
//!    runtime injects it itself. Nothing is typed anywhere, it works while a
//!    permission prompt is on screen, and delivery is confirmed
//!    (`Call::Delivered`), so it is at-least-once.
//! 2. **Terminal** — the text is submitted to the agent's terminal as a
//!    prompt. This works for any interactive CLI and is the fallback.
//!
//! Either way a direct message goes sender → bus → recipient's session; the
//! orchestrator is never on the path.

use std::sync::Arc;

use crew_core::rpc::{Push, ServerFrame};
use crew_core::util::now_ms;
use crew_core::{Agent, AgentStatus, Message, MessageType, RuntimeKind, OPERATOR_ID};
use tokio::sync::mpsc;

use super::sandbox::{command, Run};
use super::{Daemon, Listener};
use crate::agents::runtime_for;
use crate::persistence::NewEvent;

/// Minimum gap between two prompts typed into the same terminal.
pub(crate) const DELIVERY_GAP: std::time::Duration = std::time::Duration::from_millis(300);
/// A pushed message that is not confirmed within this time is re-sent
/// through the terminal.
const IN_FLIGHT_TIMEOUT_MS: i64 = 20_000;
/// How long after start-up to wait for a runtime's socket channel before
/// falling back to the terminal.
const SOCKET_GRACE_MS: i64 = 12_000;
/// How long to wait for a queue-capable runtime to report its session id.
const QUEUE_GRACE_MS: i64 = 120_000;
/// A single argument is limited to 128 KiB on Linux; stay well below.
const MAX_ARGV_TEXT: usize = 60_000;

/// Senders whose status messages are crew's own notices (a task finished,
/// failed, stalled; everything is done), which the orchestrator acts on.
const CREW_SENDERS: [&str; 2] = ["scheduler", OPERATOR_ID];

/// A resting agent's messages are gathered for this long after the latest
/// one came, and at most this long after the first: a burst is one prompt,
/// and one turn, instead of one each.
const GATHER_MS: i64 = 1_500;
const GATHER_MAX_MS: i64 = 5_000;

/// Whether `message` waits for company before it reaches `to`: a
/// teammate's status message is news, not a call to act, and a turn of its
/// own would cost a pass over the whole context to acknowledge it. It goes
/// in with the next message that does start a turn (crew's notice that a
/// task finished, say), or is read with agent_inbox. For the orchestrator
/// always; for a member at rest (`resting`) unless it said it is waiting,
/// when news may be what it waits for. A member mid-turn reads it in that
/// turn, at no cost of its own.
pub(crate) fn waits_for_company(to: &Agent, message: &Message, resting: bool) -> bool {
    let news =
        message.kind == MessageType::Status && !CREW_SENDERS.contains(&message.from.as_str());
    news && (to.orchestrator || (resting && to.status != AgentStatus::Waiting))
}

/// Whether anything waiting for `to` would start a turn.
pub(crate) fn wakes(to: &Agent, pending: &[Message], resting: bool) -> bool {
    pending.iter().any(|m| !waits_for_company(to, m, resting))
}

fn clip_for_argv(mut text: String) -> String {
    if text.len() > MAX_ARGV_TEXT {
        let mut cut = MAX_ARGV_TEXT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n… (truncated — call agent_inbox for the full text)");
    }
    text
}

enum Route {
    Socket(u64, mpsc::Sender<ServerFrame>, Vec<Message>),
    /// Queue through the runtime's own socket (Codex's app-server): the
    /// socket path and the session (thread) id.
    Queue(std::path::PathBuf, String),
    /// The socket channel went quiet; forget it and use the terminal.
    SocketStale(u64),
    Terminal,
    /// Not now; try again after this many milliseconds (0 = wait for an event).
    Hold(u64),
}

impl Daemon {
    pub(crate) fn delivery_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.delivery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    pub(crate) fn listener(&self, agent: &str) -> Option<(u64, mpsc::Sender<ServerFrame>)> {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent)
            .map(|l| (l.conn, l.tx.clone()))
    }

    /// An agent's runtime opened its delivery channel.
    pub(crate) fn listener_joined(&self, agent: &str, conn: u64, tx: mpsc::Sender<ServerFrame>) {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(agent.to_string(), Listener { conn, tx });
        self.model().rt(agent).in_flight.clear();
        self.update_agent(agent, |a| a.delivery = "socket".into());
        self.emit(
            NewEvent::new(
                "bus",
                "delivery.socket",
                format!("{agent}: messages are now pushed over the socket"),
            )
            .agent(agent),
        );
        self.request_delivery(agent);
    }

    /// A delivery channel closed (the session ended or the mod unloaded).
    pub(crate) fn listener_gone(&self, agent: &str, conn: u64) {
        let removed = {
            let mut listeners = self.listeners.lock().unwrap_or_else(|e| e.into_inner());
            match listeners.get(agent) {
                Some(l) if l.conn == conn => listeners.remove(agent).is_some(),
                _ => false,
            }
        };
        if removed {
            self.model().rt(agent).in_flight.clear();
            self.update_agent(agent, |a| a.delivery = "terminal".into());
            // Anything still pending goes through the terminal.
            self.request_delivery(agent);
        }
    }

    /// Deliver whatever is waiting for an agent into its session.
    pub(crate) fn request_delivery(&self, id: &str) {
        if id == OPERATOR_ID {
            return;
        }
        let daemon = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            daemon.deliver(&id).await;
        });
    }

    fn retry_delivery(&self, id: &str, after_ms: u64) {
        let daemon = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(after_ms)).await;
            daemon.deliver(&id).await;
        });
    }

    async fn deliver(&self, id: &str) {
        let lock = self.delivery_lock(id);
        let _guard = lock.lock().await;
        let pending = match self.bus.undelivered(id) {
            Ok(pending) if !pending.is_empty() => pending,
            _ => return,
        };
        if let Err(e) = self.record_task_wakeups(id, &pending) {
            tracing::error!("{}", e.message);
            drop(_guard);
            self.retry_delivery(id, 1_000);
            return;
        }
        let now = now_ms();
        let listener = self.listener(id);
        let sidecar = self.sidecar_socket(id);
        let (kind, route) = {
            let mut model = self.model();
            let Some(agent) = model.agent(id).cloned() else {
                return;
            };
            let resting = !model.rt(id).turn_active;
            if !wakes(&agent, &pending, resting) {
                // Only news: it rides along later.
                return;
            }
            // A resting session gathers what comes in a burst into one prompt.
            let newest = pending.iter().map(|m| m.timestamp).max().unwrap_or(now);
            let oldest = pending.iter().map(|m| m.timestamp).min().unwrap_or(now);
            let due = (newest + GATHER_MS).min(oldest + GATHER_MAX_MS);
            if resting && agent.runtime != RuntimeKind::Shell && now < due {
                drop(model);
                drop(_guard);
                self.retry_delivery(id, (due - now) as u64);
                return;
            }
            // Claude Code's mod and pi's extension hold the socket open.
            let wants_socket = matches!(agent.runtime, RuntimeKind::Claude | RuntimeKind::Pi)
                && model
                    .local
                    .get(id)
                    .is_some_and(|l| l.settings.socket_delivery);
            let rt = model.rt(id);
            let gone = agent.paused
                // Parked at a usage limit: a prompt now would only fail again.
                || agent.limit.is_some()
                || matches!(agent.status, AgentStatus::Failed | AgentStatus::Stopped)
                || rt.pending_prompt.is_some();
            let route = if gone {
                Route::Hold(0)
            } else if let Some((conn, tx)) = listener {
                if rt
                    .in_flight
                    .values()
                    .any(|sent| now - *sent > IN_FLIGHT_TIMEOUT_MS)
                {
                    rt.in_flight.clear();
                    Route::SocketStale(conn)
                } else {
                    let fresh: Vec<Message> = pending
                        .iter()
                        .filter(|m| !rt.in_flight.contains_key(&m.id))
                        .cloned()
                        .collect();
                    for m in &fresh {
                        rt.in_flight.insert(m.id.clone(), now);
                    }
                    Route::Socket(conn, tx, fresh)
                }
            } else if let (Some(socket), false) = (sidecar, rt.queue_failed) {
                match &agent.session_id {
                    Some(thread) => Route::Queue(socket, thread.clone()),
                    // The thread id arrives with the first turn-complete
                    // notification; give it time, then use the terminal.
                    None if now - rt.spawned_at < QUEUE_GRACE_MS => Route::Hold(0),
                    None if rt.ready && !rt.turn_active => Route::Terminal,
                    None => Route::Hold(0),
                }
            } else if !rt.ready
                || agent.status == AgentStatus::Spawning
                // Typing into a permission prompt would answer it.
                || matches!(agent.block_kind.as_deref(), Some("permission" | "dialog"))
                // Codex drops text typed while a turn is running; wait for the
                // turn to end (which triggers another delivery attempt).
                || (agent.runtime == RuntimeKind::Codex && rt.turn_active)
            {
                Route::Hold(0)
            } else if wants_socket && now - rt.spawned_at < SOCKET_GRACE_MS {
                // The runtime's channel is probably about to connect.
                Route::Hold(1_000)
            } else {
                Route::Terminal
            };
            // What a resting session is given starts its turn; the turn's
            // final reply answers it.
            if resting {
                let given: Vec<String> = match &route {
                    Route::Socket(_, _, fresh) => fresh.iter().map(|m| m.id.clone()).collect(),
                    Route::Terminal | Route::Queue(..) => {
                        pending.iter().map(|m| m.id.clone()).collect()
                    }
                    _ => Vec::new(),
                };
                model.rt(id).turn_openers.extend(given);
            }
            (agent.runtime, route)
        };

        match route {
            Route::Hold(0) => {}
            Route::Hold(ms) => self.retry_delivery(id, ms),
            Route::Socket(conn, tx, fresh) => {
                if fresh.is_empty() {
                    return;
                }
                let ids: Vec<String> = fresh.iter().map(|m| m.id.clone()).collect();
                // The channel carries the messages themselves, whatever the
                // runtime's terminal phrasing would be.
                let text = crate::agents::prompt::format_nudge(&fresh);
                let frame = ServerFrame::push(Push::Deliver {
                    ids,
                    text,
                    immediate: false,
                });
                if tx.send(frame).await.is_err() {
                    drop(_guard);
                    self.listener_gone(id, conn);
                    return;
                }
                // If no confirmation arrives, come back and use the terminal.
                self.retry_delivery(id, IN_FLIGHT_TIMEOUT_MS as u64 + 500);
            }
            Route::Queue(socket, thread) => {
                let ids: Vec<String> = pending.iter().map(|m| m.id.clone()).collect();
                let text = clip_for_argv(runtime_for(kind).nudge(&pending));
                match self.queue(id, kind, &socket, &thread, &text).await {
                    Ok(()) => self.delivered(id, &ids, "socket"),
                    Err(e) => {
                        self.model().rt(id).queue_failed = true;
                        self.update_agent(id, |a| a.delivery = "terminal".into());
                        self.emit(
                            NewEvent::new(
                                "bus",
                                "delivery.fallback",
                                format!("{id}: queueing over the runtime socket failed ({e}); using the terminal"),
                            )
                            .agent(id)
                            .warn(),
                        );
                        self.retry_delivery(id, 200);
                    }
                }
            }
            Route::SocketStale(conn) => {
                self.emit(
                    NewEvent::new(
                        "bus",
                        "delivery.fallback",
                        format!("{id}: socket delivery was not confirmed; using the terminal"),
                    )
                    .agent(id)
                    .warn(),
                );
                drop(_guard);
                self.listener_gone(id, conn);
            }
            Route::Terminal => {
                let Ok(procs) = self.procs() else { return };
                if !procs.is_alive(id) {
                    return;
                }
                let text = runtime_for(kind).nudge(&pending);
                match procs.submit_prompt(id, &text).await {
                    Ok(()) => {
                        let ids: Vec<String> = pending.iter().map(|m| m.id.clone()).collect();
                        self.delivered(id, &ids, "terminal");
                        tokio::time::sleep(DELIVERY_GAP).await;
                    }
                    Err(e) => self.emit(
                        NewEvent::new(
                            "bus",
                            "message.delivery_failed",
                            format!("could not deliver to {id}: {e:#}"),
                        )
                        .agent(id)
                        .warn(),
                    ),
                }
            }
        }
    }

    /// The agent's runtime confirmed it injected these messages.
    pub(crate) fn confirm_delivery(&self, agent: &str, ids: &[String]) {
        {
            let mut model = self.model();
            let rt = model.rt(agent);
            for id in ids {
                rt.in_flight.remove(id);
            }
        }
        if !ids.is_empty() {
            self.delivered(agent, ids, "socket");
        }
    }

    /// Bookkeeping once messages are in the session: a turn is (or will be)
    /// running on them.
    fn delivered(&self, id: &str, ids: &[String], via: &str) {
        if let Err(e) = self.bus.mark_delivered(id, ids) {
            tracing::error!("marking messages delivered for {id}: {e}");
        }
        self.resume_waiting_from_delivery(id, ids);
        let now = now_ms();
        let Some(agent) = self.agent(id) else { return };
        let interactive = agent.runtime != RuntimeKind::Shell;
        {
            let mut model = self.model();
            let rt = model.rt(id);
            rt.nudges = 0;
            rt.idle_since = None;
            rt.delivered_at = now;
            rt.turn_messages.extend(ids.iter().cloned());
            if interactive {
                rt.turn_active = true;
                rt.self_status = None;
            }
        }
        if interactive {
            self.update_agent(id, |a| {
                // A session sitting at a permission prompt stays blocked; the
                // message is queued behind it.
                if !matches!(a.block_kind.as_deref(), Some("permission" | "dialog"))
                    && !a.status.is_terminal()
                {
                    a.status = AgentStatus::Working;
                    a.block_kind = None;
                }
                a.last_activity_at = now;
            });
            // Mid-card messages never resume a different parked card.
            if crate::tasks::executing_task(&self.model().tasks, id).is_some() {
                if let Err(e) = self.mirror_status_to_task(id, AgentStatus::Working, None) {
                    tracing::error!("{}", e.message);
                }
            }
        }
        self.replan();
        self.emit(
            NewEvent::new(
                "bus",
                "message.delivered",
                format!(
                    "delivered {} message(s) into {id}'s session via {via}",
                    ids.len()
                ),
            )
            .agent(id)
            .level(crew_core::EventLevel::Debug),
        );
    }

    fn sidecar_socket(&self, id: &str) -> Option<std::path::PathBuf> {
        self.sidecars
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|s| s.socket.clone())
    }

    /// Inject text through the runtime's own socket by running its queue
    /// command (an argument array; no shell). In a team that requires
    /// sandboxes it runs in the agent's sandbox, with the agent's variables.
    async fn queue(
        &self,
        id: &str,
        kind: RuntimeKind,
        socket: &std::path::Path,
        session: &str,
        text: &str,
    ) -> Result<(), String> {
        let mut argv = runtime_for(kind)
            .queue_command(socket, session, text)
            .ok_or("this runtime has no queue command")?;
        let agent = self.agent(id).ok_or("the agent is gone")?;
        let env = self
            .sidecars
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|s| s.env.clone())
            .unwrap_or_default();
        // Held by the task that owns the command, not by this request: a
        // launch of the agent waits until the command and its sandbox are
        // done, whoever stops waiting for it.
        let lease = self.launch_lease(id).read_owned().await;
        let sandbox = self
            .agent_sandbox(&agent, Run::helper(id, &argv[0], &env, socket))
            .await
            .map_err(|e| format!("{e:#}"))?
            .map(|sandboxed| {
                // The CLI file its profile bound, by its absolute path.
                if let Some(program) = sandboxed.program {
                    argv[0] = program.to_string_lossy().into_owned();
                }
                sandboxed.spec
            });
        // In the ledger, on disk, before it starts: its sandbox reports there
        // when it is gone.
        let record = sandbox
            .as_ref()
            .map(|_| self.ledger.start(id, "queue"))
            .transpose()
            .map_err(|e| format!("{e:#}"))?;
        let marked = sandbox.zip(record.as_ref()).map(|(mut sandbox, record)| {
            sandbox.done = Some(record.done().to_path_buf());
            sandbox
        });
        // Unsandboxed it keeps running with the daemon's own environment.
        let env = if marked.is_some() { env } else { Vec::new() };
        let mut cmd = command(marked.as_ref(), &argv, &env).map_err(|e| format!("{e:#}"))?;
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let spawned = cmd.spawn();
        if let (Err(_), Some(record)) = (&spawned, &record) {
            self.ledger.never_started(record);
        }
        let child = spawned.map_err(|e| e.to_string())?;
        tokio::spawn(run_queue(child, record, lease))
            .await
            .map_err(|e| e.to_string())?
    }

    /// Put crew's own text (a reminder) into a session. Returns whether it
    /// was handed over.
    pub(crate) async fn inject(&self, id: &str, text: &str) -> bool {
        let lock = self.delivery_lock(id);
        let _guard = lock.lock().await;
        if let (Some(socket), Some(agent)) = (self.sidecar_socket(id), self.agent(id)) {
            if let Some(thread) = &agent.session_id {
                if self
                    .queue(id, agent.runtime, &socket, thread, text)
                    .await
                    .is_ok()
                {
                    return true;
                }
            }
        }
        if let Some((_, tx)) = self.listener(id) {
            let frame = ServerFrame::push(Push::Deliver {
                ids: Vec::new(),
                text: text.to_string(),
                immediate: false,
            });
            return tx.send(frame).await.is_ok();
        }
        match self.procs() {
            Ok(procs) => procs.submit_prompt(id, text).await.is_ok(),
            Err(_) => false,
        }
    }
}

/// Run a queue command to its end, with its ledger entry and the agent's
/// launch lease (held until then). Its sandbox's init is found before
/// anything can kill it; past its time it is killed.
async fn run_queue(
    mut child: tokio::process::Child,
    record: Option<super::retire::Record>,
    _lease: tokio::sync::OwnedRwLockReadGuard<()>,
) -> Result<(), String> {
    use tokio::io::AsyncReadExt;
    if let (Some(record), Some(pid)) = (&record, child.id()) {
        record.update(|r| r.pid = Some(pid));
        // This task holds the child, not yet waited for, so the number is
        // its own.
        let found = match super::retire::Proc::of(pid) {
            Some(wrapper) => super::retire::discover(wrapper, super::retire::DISCOVER_WITHIN).await,
            None => Err(anyhow::anyhow!("its process could not be identified")),
        };
        match found {
            Ok(init) => record.update(|r| r.init = init),
            Err(e) => {
                let _ = child.kill().await;
                return Err(format!("the queue command was stopped: {e:#}"));
            }
        }
    }
    let mut stderr = child.stderr.take();
    let read = tokio::spawn(async move {
        let mut text = String::new();
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.read_to_string(&mut text).await;
        }
        text
    });
    let status = match tokio::time::timeout(std::time::Duration::from_secs(20), child.wait()).await
    {
        Ok(status) => status.map_err(|e| e.to_string())?,
        Err(_) => {
            let _ = child.kill().await;
            return Err("the queue command timed out".to_string());
        }
    };
    if status.success() {
        return Ok(());
    }
    let stderr = read.await.unwrap_or_default();
    Err(stderr
        .lines()
        .next()
        .unwrap_or("the queue command failed")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> Agent {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "role": "", "status": "idle", "runtime": "claude",
            "workdir": "/w", "backend": "pty", "last_activity_at": 0,
        }))
        .unwrap()
    }

    fn message(from: &str, kind: MessageType) -> Message {
        Message {
            id: "m-1".into(),
            from: from.into(),
            to: "m".into(),
            kind,
            body: "x".into(),
            timestamp: 0,
            correlation_id: None,
            reply_to: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn news_waits_for_company_unless_it_is_what_a_member_waits_for() {
        let member = |status| Agent {
            status,
            ..agent("m")
        };
        let orchestrator = Agent {
            orchestrator: true,
            ..agent("orchestrator")
        };
        let news = message("lead", MessageType::Status);
        // The orchestrator: news always waits; crew's notices never do.
        assert!(waits_for_company(&orchestrator, &news, false));
        assert!(!waits_for_company(
            &orchestrator,
            &message("scheduler", MessageType::Status),
            true
        ));
        // A member at rest: news waits, unless it said it is waiting.
        assert!(waits_for_company(&member(AgentStatus::Idle), &news, true));
        assert!(!waits_for_company(
            &member(AgentStatus::Waiting),
            &news,
            true
        ));
        // Mid-turn it is read in that turn.
        assert!(!waits_for_company(
            &member(AgentStatus::Working),
            &news,
            false
        ));
        // Anything else starts a turn.
        let asked = [news.clone(), message("lead", MessageType::Question)];
        assert!(wakes(&member(AgentStatus::Idle), &asked, true));
        assert!(!wakes(&member(AgentStatus::Idle), &asked[..1], true));
    }

    #[tokio::test]
    async fn a_queue_command_keeps_its_launch_lease_until_it_ends_whoever_waits() {
        let lease = Arc::new(tokio::sync::RwLock::new(()));
        let shared = lease.clone().read_owned().await;
        let child = tokio::process::Command::new("sleep")
            .arg("0.5")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        // Nobody waits for it (the delivery was cancelled).
        drop(tokio::spawn(run_queue(child, None, shared)));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            lease.try_write().is_err(),
            "the lease went before the command"
        );
        assert!(
            std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "the command was stopped"
        );
        for _ in 0..200 {
            if lease.try_write().is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the lease was kept after the command ended");
    }
}
