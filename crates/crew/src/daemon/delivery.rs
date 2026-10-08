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

use crew_core::rpc::{Push, RpcError, ServerFrame};
use crew_core::util::now_ms;
use crew_core::{Agent, AgentStatus, Message, MessageType, RuntimeKind, OPERATOR_ID};
use tokio::sync::mpsc;

use super::sandbox::{command, Run};
use super::{Daemon, Listener};
use crate::agents::runtime_for;
use crate::persistence::NewEvent;

pub(super) mod native;

pub(crate) fn native_hook(event: &str) -> bool {
    matches!(
        event,
        "native-bridge-ready"
            | "native-turn-started"
            | "native-turn-ended"
            | "native-delivery-accepted"
            | "native-delivery-observed"
            | "native-delivery-started"
            | "native-delivery-unknown"
            | "native-delivery-retired"
    )
}

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
    fn receipt_key(id: &str) -> String {
        format!("native-delivery:{id}")
    }

    fn load_receipts(&self, id: &str) -> Result<(), RpcError> {
        let mut model = self.model();
        if model.rt(id).native_receipts_loaded {
            return Ok(());
        }
        let saved = self
            .store
            .get_meta(&Self::receipt_key(id))
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let mut book: native::Book = match saved {
            Some(text) => serde_json::from_str(&text)
                .map_err(|_| RpcError::invalid("native delivery metadata is corrupt"))?,
            None => native::Book::default(),
        };
        if book.version != 1 {
            return Err(RpcError::invalid(
                "unknown native delivery metadata version",
            ));
        }
        // A crash after persisting native consumption but before updating the
        // bus row is repaired from the receipt, never by submitting again.
        let consumed: Vec<_> = book
            .receipts
            .values()
            .filter(|r| r.phase == native::Phase::Consumed)
            .cloned()
            .collect();
        for receipt in consumed {
            self.bus
                .mark_delivered(id, &receipt.ids)
                .map_err(|e| RpcError::internal(e.to_string()))?;
            book.receipts.remove(&receipt.batch_id);
        }
        model.rt(id).native_receipts = book;
        model.rt(id).native_receipts_loaded = true;
        Ok(())
    }

    fn save_receipts(&self, id: &str) -> Result<(), RpcError> {
        let model = self.model();
        let rt = model
            .rt
            .get(id)
            .ok_or_else(|| RpcError::invalid("missing runtime state"))?;
        let text = serde_json::to_string(&rt.native_receipts)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        self.store
            .set_meta(&Self::receipt_key(id), &text)
            .map_err(|e| RpcError::internal(e.to_string()))
    }

    fn receipt_event(
        &self,
        id: &str,
        kind: &str,
        receipt: &native::Receipt,
        extra: serde_json::Value,
    ) {
        self.emit(
            NewEvent::new("bus", kind, format!("{id}: {}", receipt.batch_id))
                .agent(id)
                .data(serde_json::json!({
                    "batch_id": receipt.batch_id, "ids": receipt.ids,
                    "native_session_id": receipt.session_id, "launch": receipt.launch,
                    "queued_at": receipt.queued_at, "accepted_at": receipt.accepted_at,
                    "observed_turn_at": receipt.observed_at, "consumed_at": receipt.consumed_at,
                    "deadline": receipt.deadline, "phase": receipt.phase, "detail": extra,
                })),
        );
    }

    pub(crate) fn register_receipt(
        &self,
        id: &str,
        ids: Vec<String>,
        now: i64,
    ) -> Result<String, RpcError> {
        self.load_receipts(id)?;
        let receipt = {
            let mut model = self.model();
            let session = model
                .rt(id)
                .native_session
                .clone()
                .ok_or_else(|| RpcError::invalid("native session is not known"))?;
            let launch = model.local.get(id).and_then(|l| l.launch.clone());
            let key = model
                .rt(id)
                .native_receipts
                .offer(ids, session, launch, now)
                .ok_or_else(|| RpcError::invalid("native delivery is already pending or full"))?;
            model.rt(id).native_receipts.receipts[&key].clone()
        };
        if let Err(error) = self.save_receipts(id) {
            // No frame has been sent, so this offer is known not admitted.
            self.model()
                .rt(id)
                .native_receipts
                .receipts
                .remove(&receipt.batch_id);
            return Err(error);
        }
        self.receipt_event(
            id,
            "message.runtime_queued",
            &receipt,
            serde_json::Value::Null,
        );
        let daemon = self.clone();
        let agent = id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(native::RECEIPT_MS as u64)).await;
            daemon.expire_receipts(&agent);
        });
        Ok(receipt.batch_id)
    }

    fn expire_receipts(&self, id: &str) {
        let expired = self.model().rt(id).native_receipts.expire(now_ms());
        if expired.is_empty() {
            return;
        }
        if let Err(e) = self.save_receipts(id) {
            tracing::error!("{}", e.message);
        }
        for receipt in expired {
            self.receipt_event(id, "message.delivery_unknown", &receipt,
                serde_json::json!({ "reason": "native receipt expired; admission is not known retired" }));
        }
    }

    /// These events use the existing authenticated hook channel. Their native
    /// session and launch fences are checked before touching queue accounting.
    pub(crate) async fn native_delivery_hook(
        &self,
        id: &str,
        launch: Option<&str>,
        event: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let lease = self.hook_lease(id);
        let _handling = lease.read().await;
        let agent = self
            .agent(id)
            .ok_or_else(|| RpcError::not_found("agent is gone"))?;
        if agent.runtime != RuntimeKind::Claude {
            return Err(RpcError::invalid("not a Claude bridge"));
        }
        let generation = self.model().local.get(id).and_then(|l| l.launch.clone());
        if generation.as_deref() != launch {
            return Err(RpcError::invalid("replaced native launch"));
        }
        let session = payload
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 200)
            .ok_or_else(|| RpcError::invalid("missing native session"))?;
        self.load_receipts(id)?;
        if event == "native-bridge-ready" {
            let retired = {
                let mut model = self.model();
                let rt = model.rt(id);
                let old: Vec<_> = rt
                    .native_receipts
                    .receipts
                    .values()
                    .filter(|r| r.session_id != session || r.launch != generation)
                    .cloned()
                    .collect();
                for r in &old {
                    rt.native_receipts.receipts.remove(&r.batch_id);
                }
                rt.native_queue_bridge = true;
                rt.native_session = Some(session.into());
                old
            };
            self.save_receipts(id)?;
            for r in retired {
                self.receipt_event(id, "message.delivery_unknown", &r,
                serde_json::json!({"reason":"native session ended; retained bus row may replay"}));
            }
            self.expire_receipts(id);
            self.request_delivery(id);
            return Ok(serde_json::json!({ "ok": true, "launch": generation }));
        }
        if self.model().rt(id).native_session.as_deref() != Some(session) {
            return Err(RpcError::invalid("unmatched native session"));
        }
        if event == "native-turn-started" {
            let turn = payload
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| RpcError::invalid("missing native turn"))?;
            if self.model().rt(id).native_turn.as_deref() == Some(turn) {
                return Ok(serde_json::json!({"ok":true}));
            }
            // A keep-alive's turn is not work: the member stays as it was.
            let keepalive = self.keepalive_turn_started(id, now_ms());
            {
                let mut model = self.model();
                let rt = model.rt(id);
                rt.native_turn = Some(turn.into());
                rt.turn_active = true;
                if !keepalive {
                    rt.self_status = None;
                    rt.idle_since = None;
                }
            }
            if !keepalive {
                self.update_agent(id, |a| {
                    if !a.status.is_terminal() {
                        a.status = AgentStatus::Working;
                        a.block_kind = None;
                    }
                    a.last_activity_at = now_ms();
                });
            }
            return Ok(serde_json::json!({"ok":true}));
        }
        if event == "native-turn-ended" {
            let turn = payload.get("turn_id").and_then(serde_json::Value::as_str);
            if turn.is_some() && self.model().rt(id).native_turn.as_deref() == turn {
                self.model().rt(id).native_turn = None;
                self.turn_ended(id, now_ms());
            }
            return Ok(serde_json::json!({"ok":true}));
        }
        let ids: Vec<String> =
            serde_json::from_value(payload.get("ids").cloned().unwrap_or_default())
                .map_err(|_| RpcError::invalid("missing delivery ids"))?;
        let key = payload
            .get("batch_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| RpcError::invalid("missing native batch id"))?;
        if !self
            .model()
            .rt(id)
            .native_receipts
            .receipts
            .contains_key(key)
        {
            let pending = self
                .bus
                .undelivered(id)
                .map_err(|e| RpcError::internal(e.to_string()))?;
            let already_consumed = !ids.is_empty()
                && ids.iter().all(|message| {
                    !pending.iter().any(|m| &m.id == message)
                        && self
                            .bus
                            .get(message)
                            .ok()
                            .flatten()
                            .is_some_and(|(_, recipients)| recipients.iter().any(|r| r == id))
                });
            if already_consumed {
                return Ok(serde_json::json!({"ok":true,"already_consumed":true}));
            }
            self.emit(
                NewEvent::new(
                    "bus",
                    "message.delivery_stale",
                    "ignored a retired or unmatched native receipt",
                )
                .agent(id)
                .data(serde_json::json!({"batch_id":key,"ids":ids})),
            );
            return Err(RpcError::invalid("native batch is not outstanding"));
        }
        let now = now_ms();
        let changed = {
            let mut model = self.model();
            let rt = model.rt(id);
            let r = rt
                .native_receipts
                .matching_mut(key, &ids, session, &generation)
                .ok_or_else(|| RpcError::invalid("native batch is not outstanding"))?;
            match event {
                "native-delivery-accepted" => {
                    if r.accepted_at.is_none() {
                        r.accepted_at = Some(now);
                    }
                    if r.phase != native::Phase::Unknown {
                        r.phase = native::Phase::Accepted;
                    }
                }
                "native-delivery-observed" => {
                    let turn = payload
                        .get("turn_id")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| RpcError::invalid("missing observed native turn"))?;
                    if rt.native_turn.as_deref() != Some(turn) {
                        return Err(RpcError::invalid("observed turn is not current"));
                    }
                    r.observed_at = Some(now);
                    r.observed_turn = Some(turn.into());
                }
                "native-delivery-started" => {
                    let turn = payload
                        .get("turn_id")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| RpcError::invalid("missing observed native turn"))?;
                    if r.observed_turn.as_deref() != Some(turn) {
                        return Err(RpcError::invalid(
                            "native turn was not observed for this batch",
                        ));
                    }
                    r.consumed_at = Some(now);
                    r.phase = native::Phase::Consumed;
                }
                "native-delivery-unknown" => r.phase = native::Phase::Unknown,
                "native-delivery-retired" => {}
                _ => return Err(RpcError::invalid("unknown native delivery event")),
            }
            let r = r.clone();
            for message in &ids {
                rt.in_flight.remove(message);
            }
            if event == "native-delivery-retired" {
                rt.native_receipts.receipts.remove(key);
            }
            r
        };
        self.save_receipts(id)?;
        match event {
            "native-delivery-accepted" => self.receipt_event(
                id,
                "message.runtime_accepted",
                &changed,
                serde_json::Value::Null,
            ),
            "native-delivery-observed" => {
                let mut model = self.model();
                let rt = model.rt(id);
                for message in &ids {
                    if !rt.turn_openers.contains(message) {
                        rt.turn_openers.push(message.clone());
                    }
                    if !rt.turn_messages.contains(message) {
                        rt.turn_messages.push(message.clone());
                    }
                }
                drop(model);
                self.receipt_event(
                    id,
                    "message.runtime_observed",
                    &changed,
                    serde_json::json!({"turn_id":payload.get("turn_id")}),
                );
            }
            "native-delivery-started" => {
                // Late acceptance may arrive after the observed turn ended.
                // Mark the row consumed without assigning it to another turn.
                self.bus
                    .mark_delivered(id, &ids)
                    .map_err(|e| RpcError::internal(e.to_string()))?;
                self.model().rt(id).native_receipts.receipts.remove(key);
                self.save_receipts(id)?;
                self.refresh_counts(&[id.to_string()]);
                self.receipt_event(
                    id,
                    "message.runtime_consumed",
                    &changed,
                    serde_json::json!({"turn_id":payload.get("turn_id")}),
                );
            }
            "native-delivery-unknown" => self.receipt_event(
                id,
                "message.delivery_unknown",
                &changed,
                serde_json::json!({"reason":payload.get("reason")}),
            ),
            "native-delivery-retired" => {
                self.request_delivery(id);
            }
            _ => {}
        }
        Ok(serde_json::json!({"ok":true}))
    }
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
        let mut pending = match self.bus.undelivered(id) {
            Ok(pending) if !pending.is_empty() => pending,
            _ => return,
        };
        // A held ordinary message never becomes an in-flight submission.
        // Operator steering is selected from bus identity, never prompt text.
        let native_queue = self.model().rt(id).native_queue_bridge;
        if native_queue {
            if let Err(e) = self.load_receipts(id) {
                tracing::error!("{}", e.message);
                return;
            }
            self.expire_receipts(id);
        }
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
            if agent.runtime == RuntimeKind::Claude {
                if pending.iter().any(|m| m.from == OPERATOR_ID) {
                    pending.retain(|m| m.from == OPERATOR_ID);
                } else if !resting
                    && (native_queue
                        || model
                            .local
                            .get(id)
                            .is_some_and(|local| local.settings.socket_delivery))
                {
                    return;
                }
                if native_queue {
                    let rt = model.rt(id);
                    pending.retain(|m| !rt.native_receipts.contains(&m.id));
                }
                if pending.is_empty() {
                    return;
                }
            }
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
                if pending.iter().any(|m| {
                    rt.in_flight
                        .get(&m.id)
                        .is_some_and(|sent| now - *sent > IN_FLIGHT_TIMEOUT_MS)
                }) {
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
            if resting && !native_queue {
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
                let immediate =
                    kind == RuntimeKind::Claude && fresh.iter().all(|m| m.from == OPERATOR_ID);
                let batch_id = if native_queue {
                    match self.register_receipt(id, ids.clone(), now) {
                        Ok(key) => Some(key),
                        Err(e) => {
                            tracing::error!("{}", e.message);
                            return;
                        }
                    }
                } else {
                    None
                };
                let frame = ServerFrame::push(Push::Deliver {
                    ids,
                    text,
                    immediate,
                    batch_id: batch_id.clone(),
                });
                if tx.send(frame).await.is_err() {
                    if let Some(key) = batch_id {
                        self.model().rt(id).native_receipts.receipts.remove(&key);
                        if let Err(error) = self.save_receipts(id) {
                            tracing::error!("{}", error.message);
                        }
                    }
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
            if rt.native_queue_bridge {
                // The new bridge uses Delivered only for successful immediate
                // appends. Queued admissions have their own receipt channel.
                let keys: Vec<_> = rt
                    .native_receipts
                    .receipts
                    .values()
                    .filter(|r| r.ids == ids)
                    .map(|r| r.batch_id.clone())
                    .collect();
                for key in keys {
                    rt.native_receipts.receipts.remove(&key);
                }
            }
        }
        if let Err(e) = self.save_receipts(agent) {
            tracing::debug!("{}", e.message);
        }
        if !ids.is_empty() {
            self.delivered(agent, ids, "socket");
        }
    }

    /// Bookkeeping once messages are in the session: a turn is (or will be)
    /// running on them.
    pub(crate) fn delivered(&self, id: &str, ids: &[String], via: &str) {
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
            // A turn a message joins is work: Working, watched as such.
            rt.keepalive_turn_since = None;
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
        if let Some(toolbox) = self.active_toolbox(id) {
            for name in toolbox.scrub_env {
                cmd.env_remove(name);
            }
        }
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
                batch_id: None,
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

    async fn queued_daemon() -> (tempfile::TempDir, super::super::Running) {
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().into(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        daemon.model().agents.push(agent("m"));
        daemon.model().local.entry("m".into()).or_default().launch = Some("launch".into());
        daemon
            .native_delivery_hook(
                "m",
                Some("launch"),
                "native-bridge-ready",
                &serde_json::json!({"session_id":"session"}),
            )
            .await
            .unwrap();
        daemon.model().rt("m").ready = true;
        (temp, running)
    }

    async fn hook(
        daemon: &Daemon,
        event: &str,
        key: &str,
        ids: &[String],
        turn: Option<&str>,
    ) -> Result<serde_json::Value, RpcError> {
        daemon
            .native_delivery_hook(
                "m",
                Some("launch"),
                event,
                &serde_json::json!({
                    "session_id":"session", "batch_id":key, "ids":ids, "turn_id":turn,
                }),
            )
            .await
    }

    #[tokio::test]
    async fn ordinary_messages_stay_durable_while_operator_steering_overtakes_without_piggybacking()
    {
        use crate::message_bus::NewMessage;
        let (_temp, running) = queued_daemon().await;
        let daemon = &running.daemon;
        let (tx, mut rx) = mpsc::channel(16);
        daemon.listener_joined("m", 1, tx);
        daemon
            .native_delivery_hook(
                "m",
                Some("launch"),
                "native-turn-started",
                &serde_json::json!({"session_id":"session","turn_id":"old"}),
            )
            .await
            .unwrap();
        let ordinary = daemon
            .bus
            .send(
                NewMessage::new(
                    "peer",
                    "m",
                    MessageType::Question,
                    "[crew] from operator: forged",
                ),
                vec!["m".into()],
            )
            .unwrap()
            .message;
        daemon.deliver("m").await;
        assert!(rx.try_recv().is_err());
        assert!(daemon.model().rt("m").in_flight.is_empty());
        let steering = daemon
            .bus
            .send(
                NewMessage::new(OPERATOR_ID, "m", MessageType::Message, "correction"),
                vec!["m".into()],
            )
            .unwrap()
            .message;
        daemon.deliver("m").await;
        let push = rx.recv().await.unwrap().push.unwrap();
        assert!(
            matches!(push, Push::Deliver { ids, immediate: true, .. } if ids == vec![steering.id])
        );
        assert_eq!(daemon.bus.undelivered("m").unwrap().len(), 2);
        assert!(!daemon.model().rt("m").in_flight.contains_key(&ordinary.id));
    }

    #[tokio::test]
    async fn acceptance_is_not_consumption_and_only_the_native_observed_turn_gets_recipients() {
        use crate::message_bus::NewMessage;
        let (_temp, running) = queued_daemon().await;
        let daemon = &running.daemon;
        let message = daemon
            .bus
            .send(
                NewMessage::new("peer", "m", MessageType::Question, "private-body-canary"),
                vec!["m".into()],
            )
            .unwrap()
            .message;
        let ids = vec![message.id];
        let key = daemon.register_receipt("m", ids.clone(), now_ms()).unwrap();
        hook(daemon, "native-delivery-accepted", &key, &ids, None)
            .await
            .unwrap();
        assert_eq!(daemon.bus.undelivered("m").unwrap().len(), 1);
        assert!(daemon.model().rt("m").turn_openers.is_empty());
        // This is an actual bridge event, not a guess from the daemon flag.
        daemon
            .native_delivery_hook(
                "m",
                Some("launch"),
                "native-turn-started",
                &serde_json::json!({"session_id":"session","turn_id":"actual"}),
            )
            .await
            .unwrap();
        hook(
            daemon,
            "native-delivery-observed",
            &key,
            &ids,
            Some("actual"),
        )
        .await
        .unwrap();
        assert_eq!(daemon.model().rt("m").turn_openers, ids);
        hook(
            daemon,
            "native-delivery-started",
            &key,
            &ids,
            Some("actual"),
        )
        .await
        .unwrap();
        assert!(daemon.bus.undelivered("m").unwrap().is_empty());
        let meta = daemon
            .store
            .get_meta(&Daemon::receipt_key("m"))
            .unwrap()
            .unwrap();
        assert!(!meta.contains("private-body-canary"));
        let events = daemon.store.recent_events(Some("m"), 100).unwrap();
        let consumed = events
            .iter()
            .find(|e| e.event_type == "message.runtime_consumed")
            .unwrap();
        assert!(consumed.data["queued_at"].is_number());
        assert!(consumed.data["accepted_at"].is_number());
        assert!(consumed.data["observed_turn_at"].is_number());
        assert!(consumed.data["consumed_at"].is_number());
    }

    #[tokio::test]
    async fn expired_receipt_restores_as_a_tombstone_and_stale_retirement_cannot_bind_a_new_attempt(
    ) {
        use crate::message_bus::NewMessage;
        let (_temp, running) = queued_daemon().await;
        let daemon = &running.daemon;
        let message = daemon
            .bus
            .send(
                NewMessage::new("peer", "m", MessageType::Message, "same words"),
                vec!["m".into()],
            )
            .unwrap()
            .message;
        let ids = vec![message.id];
        let a = daemon.register_receipt("m", ids.clone(), now_ms()).unwrap();
        hook(daemon, "native-delivery-accepted", &a, &ids, None)
            .await
            .unwrap();
        daemon
            .model()
            .rt("m")
            .native_receipts
            .receipts
            .get_mut(&a)
            .unwrap()
            .deadline = now_ms() - 1;
        daemon.expire_receipts("m");
        daemon.model().rt("m").native_receipts = native::Book::default();
        daemon.model().rt("m").native_receipts_loaded = false;
        daemon.load_receipts("m").unwrap();
        assert_eq!(
            daemon.model().rt("m").native_receipts.receipts[&a].phase,
            native::Phase::Unknown
        );
        assert!(daemon.register_receipt("m", ids.clone(), now_ms()).is_err());
        hook(daemon, "native-delivery-retired", &a, &ids, None)
            .await
            .unwrap();
        let b = daemon.register_receipt("m", ids.clone(), now_ms()).unwrap();
        assert_ne!(a, b);
        assert!(
            hook(daemon, "native-delivery-observed", &a, &ids, Some("stale"))
                .await
                .is_err()
        );
        assert_eq!(
            daemon.model().rt("m").native_receipts.receipts[&b].phase,
            native::Phase::Offered
        );
        assert!(daemon.model().rt("m").turn_openers.is_empty());
        assert_eq!(daemon.bus.undelivered("m").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn consumption_repairs_a_crash_before_bus_update_and_rejects_stale_launches() {
        use crate::message_bus::NewMessage;
        let (_temp, running) = queued_daemon().await;
        let daemon = &running.daemon;
        let message = daemon
            .bus
            .send(
                NewMessage::new("peer", "m", MessageType::Message, "body-not-metadata"),
                vec!["m".into()],
            )
            .unwrap()
            .message;
        let ids = vec![message.id];
        let key = daemon.register_receipt("m", ids.clone(), now_ms()).unwrap();
        assert!(daemon
            .native_delivery_hook(
                "m",
                Some("stale-launch"),
                "native-delivery-accepted",
                &serde_json::json!({"session_id":"session","batch_id":key,"ids":ids})
            )
            .await
            .is_err());
        {
            let mut model = daemon.model();
            let receipt = model
                .rt("m")
                .native_receipts
                .receipts
                .get_mut(&key)
                .unwrap();
            receipt.phase = native::Phase::Consumed;
            receipt.observed_turn = Some("actual".into());
            receipt.observed_at = Some(now_ms());
            receipt.consumed_at = Some(now_ms());
        }
        daemon.save_receipts("m").unwrap();
        assert_eq!(daemon.bus.undelivered("m").unwrap().len(), 1);
        daemon.model().rt("m").native_receipts_loaded = false;
        daemon.load_receipts("m").unwrap();
        assert!(daemon.bus.undelivered("m").unwrap().is_empty());
        assert!(daemon.model().rt("m").native_receipts.receipts.is_empty());
    }

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
