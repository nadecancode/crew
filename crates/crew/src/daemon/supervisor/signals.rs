//! Runtime signals and hooks: what a session reports, turned into the agent's status.

use crew_core::util::now_ms;
use crew_core::{AgentStatus, RuntimeKind};
use serde_json::Value;

use crate::agents::{runtime_for, Signal};
use crate::daemon::delivery::DELIVERY_GAP;
use crate::daemon::Daemon;
use crate::persistence::NewEvent;
use crate::tasks::active_task;

impl Daemon {
    /// Agent `id`'s next launch begins: its mark, put in the environment of
    /// every process the launch starts, from which a hook of an earlier
    /// launch, arriving late, is told apart (on_hook).
    /// A hook still being handled is let finish first, so that everything
    /// it does (its handlers wait, for a dialog, for a listener) is done
    /// before the new launch starts out.
    pub(crate) async fn begin_launch(&self, id: &str) -> String {
        let lease = self.hook_lease(id);
        let _no_hook_running = lease.write().await;
        let launch = uuid::Uuid::new_v4().to_string();
        self.model().local.entry(id.to_string()).or_default().launch = Some(launch.clone());
        self.save_local(id);
        launch
    }

    /// Agent `id`'s hook lease: a hook holds it shared while it is handled,
    /// begin_launch alone for the moment it changes the launch. Neither
    /// waits for the launch lease while holding it, nor does a hook's
    /// handling wait for a launch, so the two cannot wait on each other.
    pub(crate) fn hook_lease(&self, id: &str) -> std::sync::Arc<tokio::sync::RwLock<()>> {
        self.hooks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    /// A runtime hook of agent `id`, run by a process of `launch` (when it
    /// says). One of an earlier launch is dropped: a replaced session's
    /// late report must not change the current one.
    pub(crate) async fn on_hook(
        &self,
        id: &str,
        launch: Option<&str>,
        event: &str,
        payload: &Value,
    ) {
        let Some(agent) = self.agent(id) else { return };
        // Held until the hook is handled, all its signals: its launch stays
        // the current one meanwhile (begin_launch waits), so nothing it
        // does reaches a later launch.
        let lease = self.hook_lease(id);
        let _handling = lease.read().await;
        if let Some(launch) = launch {
            let current = self.model().local.get(id).and_then(|l| l.launch.clone());
            if current.is_some_and(|current| current != launch) {
                tracing::info!("{id}: dropped the {event} hook of a replaced launch");
                return;
            }
        }
        if agent.runtime == RuntimeKind::Codex
            && event == "codex-notify"
            && payload.get("type").and_then(Value::as_str) == Some("agent-turn-complete")
        {
            let thread = payload
                .get("thread-id")
                .or_else(|| payload.get("thread_id"))
                .and_then(Value::as_str)
                .filter(|thread| !thread.is_empty());
            let admitted = match thread {
                Some(thread) => self.admit_codex_parent_hook(id, thread).await,
                None => false,
            };
            if !admitted {
                // A child's completion is not a completion of the member's
                // turn: drop all its signals, including TurnEnded.
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "agent.session_ignored",
                        "ignored an unconfirmed or non-parent Codex thread",
                    )
                    .agent(id)
                    .data(serde_json::json!({"native_thread":thread})),
                );
                return;
            }
            self.update_agent(id, |agent| {
                if agent
                    .activity
                    .starts_with("Codex thread could not be confirmed")
                {
                    agent.activity = "Codex user thread confirmed".into();
                }
            });
        }
        for signal in runtime_for(agent.runtime).on_hook(event, payload) {
            self.on_signal(id, signal).await;
        }
    }

    pub(crate) async fn codex_parent_header(&self, id: &str, thread: &str) -> bool {
        let Some(agent) = self.agent(id) else {
            return false;
        };
        let Ok(files) = self.agent_files(id) else {
            return false;
        };
        let thread = thread.to_string();
        let started = agent.started_at.unwrap_or_else(now_ms);
        tokio::task::spawn_blocking(move || files.codex_parent_thread(&thread, started))
            .await
            .unwrap_or(false)
    }

    async fn codex_header_status(
        &self,
        id: &str,
        thread: &str,
    ) -> crate::agents::files::CodexHeader {
        use crate::agents::files::CodexHeader;
        let Some(agent) = self.agent(id) else {
            return CodexHeader::Unavailable;
        };
        let Ok(files) = self.agent_files(id) else {
            return CodexHeader::Unavailable;
        };
        let thread = thread.to_string();
        let started = agent.started_at.unwrap_or_else(now_ms);
        tokio::task::spawn_blocking(move || files.codex_thread_header(&thread, started))
            .await
            .unwrap_or(CodexHeader::Unavailable)
    }

    async fn admit_codex_parent_hook(&self, id: &str, thread: &str) -> bool {
        use crate::agents::files::CodexHeader;
        let bound = self
            .model()
            .local
            .get(id)
            .and_then(|local| local.codex_parent_session.clone());
        if bound.as_deref() == Some(thread) {
            self.model().rt(id).codex_pending_thread = None;
            return true;
        }
        // A complete header proving a child is final. Only incomplete or
        // unavailable metadata gets a bounded flush-race retry, under the hook lease.
        // Other hooks may run concurrently; a replacement launch waits for
        // this hook's bounded retry window before it changes the launch mark.
        let mut status = CodexHeader::Unavailable;
        for attempt in 0..6 {
            status = self.codex_header_status(id, thread).await;
            if status != CodexHeader::Unavailable {
                break;
            }
            if attempt < 5 {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
        }
        match status {
            CodexHeader::Child => return false,
            CodexHeader::Unavailable => {
                {
                    let mut model = self.model();
                    let local = model.local.entry(id.to_string()).or_default();
                    // An absent child rollout is not evidence that the known
                    // parent stopped being current. Preserve its whole binding.
                    if local.codex_parent_session.is_some() {
                        return false;
                    }
                    local.session_confirmed = false;
                    model.rt(id).codex_pending_thread = Some(thread.to_string());
                }
                self.save_local(id);
                self.update_agent(id, |agent| {
                    agent.activity =
                        "Codex thread could not be confirmed; restart fresh to continue".into();
                });
                return false;
            }
            CodexHeader::Parent => {}
        }
        let changed = {
            let mut model = self.model();
            let local = model.local.entry(id.to_string()).or_default();
            let before = local.codex_parent_session.replace(thread.to_string());
            local.codex_turn = None;
            model.rt(id).rollout = None;
            model.rt(id).quota_checked_at = 0;
            model.rt(id).codex_pending_thread = None;
            before
        };
        if let Some(before) = changed.filter(|before| before != thread) {
            self.emit(
                NewEvent::new(
                    "daemon",
                    "agent.session_changed",
                    "Codex switched to a verified user thread",
                )
                .agent(id)
                .data(serde_json::json!({"previous_thread":before,"native_thread":thread})),
            );
        }
        // Session's normal persistence writes this pin with session_confirmed.
        true
    }

    /// Socket delivery may use only the same header-verified parent as resume.
    pub(crate) fn confirmed_codex_parent(&self, id: &str, thread: &str) -> bool {
        let model = self.model();
        model.local.get(id).is_some_and(|local| {
            local.session_confirmed && local.codex_parent_session.as_deref() == Some(thread)
        }) && model
            .rt
            .get(id)
            .is_none_or(|rt| rt.codex_pending_thread.is_none())
    }

    /// Migrate an old confirmation only after proving its saved thread is a
    /// user thread. Unknown or child records must never become resume targets.
    pub(crate) async fn prepare_codex_parent_resume(&self, id: &str, resume: bool) {
        let Some(agent) = self
            .agent(id)
            .filter(|agent| agent.runtime == RuntimeKind::Codex)
        else {
            return;
        };
        let local = self.model().local.get(id).cloned().unwrap_or_default();
        let pending = self
            .model()
            .rt
            .get(id)
            .is_some_and(|rt| rt.codex_pending_thread.is_some());
        let parent = match agent.session_id.as_deref() {
            Some(thread) if resume && local.session_confirmed && !pending => {
                local.codex_parent_session.as_deref() == Some(thread)
                    || self.codex_parent_header(id, thread).await
            }
            _ => false,
        };
        {
            let mut model = self.model();
            let stored = model.local.entry(id.to_string()).or_default();
            stored.session_confirmed = parent;
            stored.codex_parent_session = if parent { agent.session_id } else { None };
        }
        self.save_local(id);
        if !parent {
            self.update_agent(id, |agent| agent.session_id = None);
        }
    }

    /// Where an agent settles when its turn ends.
    fn resting_status(&self, id: &str) -> AgentStatus {
        let mut model = self.model();
        if let Some(status) = model.rt(id).self_status {
            return status;
        }
        let Some(agent) = model.agent(id) else {
            return AgentStatus::Idle;
        };
        if agent.orchestrator {
            return if model.session.summary.is_some() {
                AgentStatus::Done
            } else if model.session.team.is_some() {
                AgentStatus::Waiting
            } else {
                AgentStatus::Idle
            };
        }
        if active_task(&model.tasks, id).is_some() {
            return if crate::tasks::executing_task(&model.tasks, id).is_some() {
                AgentStatus::Idle
            } else {
                AgentStatus::Waiting
            };
        }
        let open = model
            .tasks
            .iter()
            .any(|t| t.assigned_agent.as_deref() == Some(id) && !t.status.is_terminal());
        let completed = model.local.get(id).map(|l| l.tasks_completed).unwrap_or(0);
        if !open && completed > 0 {
            AgentStatus::Done
        } else {
            AgentStatus::Idle
        }
    }

    /// Re-evaluate an agent's status outside of a turn (e.g. after a review).
    pub(crate) fn settle(&self, id: &str) {
        if self.model().rt(id).turn_active {
            return;
        }
        let Some(agent) = self.agent(id) else { return };
        if matches!(
            agent.status,
            AgentStatus::Spawning | AgentStatus::Failed | AgentStatus::Stopped
        ) || matches!(agent.block_kind.as_deref(), Some("permission" | "dialog"))
        {
            return;
        }
        let status = self.resting_status(id);
        self.update_agent(id, |a| {
            a.status = status;
            if status != AgentStatus::Blocked {
                a.block_kind = None;
            }
        });
    }

    pub(crate) async fn on_signal(&self, id: &str, signal: Signal) {
        let now = now_ms();
        // A session parked at a usage limit that shows signs of life went on
        // by itself (the runtime retried, or someone typed into it).
        if matches!(
            signal,
            Signal::TurnStarted | Signal::Activity(_) | Signal::TurnEnded
        ) {
            self.limit_lifted(id, "the session went on");
        }
        if matches!(signal, Signal::TurnEnded) {
            self.model().rt(id).limit_attempts = 0;
        }
        match signal {
            // An account without the long-context beta refuses every request
            // of a session started in the 1M variant; that is not a limit to
            // wait out.
            Signal::Limited(info) if self.long_context_refused(id, &info.message) => {}
            Signal::Limited(info) => self.agent_limited(id, info).await,
            Signal::Quota(readings) => {
                if let Some(agent) = self.agent(id) {
                    self.record_quota(agent.runtime, readings);
                }
            }
            Signal::Model { model, effort } => self.record_running(id, model, effort),
            Signal::Context { tokens, window } => {
                self.model().rt(id).context_reported = true;
                self.record_context(id, tokens, window).await;
            }
            Signal::Compacted => self.context_compacted(id).await,
            Signal::CompactSkipped(why) => self.compact_skipped(id, &why),
            // The session's end is reported as any exit is.
            Signal::ToolboxFailed {
                server,
                detail,
                ended: true,
            } => self.toolbox_failed_in_session(id, &server, &detail),
            Signal::ToolboxFailed { server, detail, .. } => {
                self.toolbox_report(
                    id,
                    &server,
                    crew_core::rpc::ToolboxReportKind::Failed,
                    &detail,
                )
                .await
            }
            Signal::TurnFailed(what) => {
                {
                    let mut model = self.model();
                    let rt = model.rt(id);
                    rt.turn_failure = Some(what.clone());
                    rt.provider_error = Some(what.clone());
                }
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "agent.turn_failed",
                        format!("{id}: the turn ended in an error: {what}"),
                    )
                    .agent(id)
                    .warn(),
                );
                self.turn_ended(id, now);
                self.long_context_refused(id, &what);
            }
            Signal::Ready => self.on_ready(id).await,
            Signal::TurnStarted => {
                if self.model().rt(id).native_queue_bridge {
                    return;
                }
                // A keep-alive's turn is not work: the member stays as it was.
                let keepalive = self.keepalive_turn_started(id, now);
                {
                    let mut model = self.model();
                    let rt = model.rt(id);
                    rt.turn_active = true;
                    if keepalive {
                        return;
                    }
                    rt.self_status = None;
                    rt.idle_since = None;
                }
                self.update_agent(id, |a| {
                    if !a.status.is_terminal() {
                        a.status = AgentStatus::Working;
                        a.block_kind = None;
                    }
                    a.last_activity_at = now;
                });
            }
            Signal::TurnEnded => {
                if self.model().rt(id).native_queue_bridge {
                    return;
                }
                self.model().rt(id).provider_error = None;
                self.turn_ended(id, now);
            }
            // Parked at a usage limit: a notification changes nothing.
            Signal::Blocked(_) if self.agent(id).is_some_and(|a| a.limit.is_some()) => {}
            Signal::Blocked(reason) => {
                let changed = self
                    .agent(id)
                    .is_some_and(|a| a.status != AgentStatus::Blocked);
                self.update_agent(id, |a| {
                    if !a.status.is_terminal() {
                        a.status = AgentStatus::Blocked;
                        a.block_kind = Some("permission".into());
                        a.activity = reason.clone();
                    }
                });
                if changed {
                    self.emit(
                        NewEvent::new(
                            "daemon",
                            "agent.blocked",
                            format!("{id} needs a human: {reason}"),
                        )
                        .agent(id)
                        .warn(),
                    );
                }
            }
            Signal::Unblocked => {
                let was_blocked = self.agent(id).is_some_and(|a| {
                    a.status == AgentStatus::Blocked
                        && matches!(
                            a.block_kind.as_deref(),
                            Some("permission" | "dialog" | "unresponsive")
                        )
                });
                if was_blocked {
                    self.update_agent(id, |a| {
                        a.status = AgentStatus::Working;
                        a.block_kind = None;
                        a.last_activity_at = now;
                    });
                    self.request_delivery(id);
                } else {
                    self.touch(id, now);
                }
            }
            Signal::Activity(text) => {
                self.update_agent(id, |a| {
                    a.activity = text;
                    a.last_activity_at = now;
                    if a.status == AgentStatus::Blocked
                        && matches!(a.block_kind.as_deref(), Some("permission" | "unresponsive"))
                    {
                        a.status = AgentStatus::Working;
                        a.block_kind = None;
                    }
                });
            }
            Signal::Session {
                id: session_id,
                transcript,
            } => {
                {
                    let mut model = self.model();
                    if transcript.is_some() {
                        model.rt(id).transcript = transcript;
                    }
                    if session_id.is_some() {
                        let local = model.local.entry(id.to_string()).or_default();
                        local.session_confirmed = true;
                        // The new session has been handed the work; a
                        // restart from here resumes it instead.
                        local.handoff = None;
                    }
                }
                if let Some(session_id) = session_id {
                    self.save_local(id);
                    self.update_agent(id, |a| {
                        if a.runtime == RuntimeKind::Codex
                            && a.session_id.as_deref() != Some(&session_id)
                        {
                            a.usage = None;
                            a.context = None;
                        }
                        a.session_id = Some(session_id);
                    });
                }
            }
            Signal::Subagents(delta) => {
                self.update_agent(id, |a| {
                    a.subagents = if delta >= 0 {
                        a.subagents.saturating_add(delta as u32)
                    } else {
                        a.subagents.saturating_sub(delta.unsigned_abs())
                    };
                    a.last_activity_at = now;
                });
            }
            Signal::Exiting => {}
        }
    }

    /// A turn is over (it completed, or it failed): the agent is at rest.
    pub(crate) fn turn_ended(&self, id: &str, now: i64) {
        let (transcript, asked) = {
            let mut model = self.model();
            let rt = model.rt(id);
            rt.turn_active = false;
            rt.parking_turn = false;
            rt.keepalive_turn_since = None;
            rt.turn_ended_at = now;
            rt.idle_since = Some(now);
            // What this turn was given; what is delivered from here on is the next one's.
            let given = (
                std::mem::take(&mut rt.turn_messages),
                std::mem::take(&mut rt.turn_openers),
            );
            (rt.transcript.clone(), given)
        };
        let status = self.resting_status(id);
        self.update_agent(id, |a| {
            if !a.status.is_terminal() {
                a.status = status;
                if status != AgentStatus::Blocked {
                    a.block_kind = None;
                }
                if status != AgentStatus::Working {
                    a.activity.clear();
                }
            }
            a.last_activity_at = now;
        });
        if let Some(transcript) = transcript {
            self.refresh_usage(id, transcript);
        }
        self.request_delivery(id);
        self.replan();
        self.retire_if_finished(id);
        if self.is_orchestrator(id) {
            self.found_team_once_planned();
        }
        // A compaction queued for this agent has its moment now.
        let daemon = self.clone();
        let agent = id.to_string();
        tokio::spawn(async move { daemon.try_compact(&agent).await });
        // Did the turn do what it was asked, or did the model refuse?
        let daemon = self.clone();
        let agent = id.to_string();
        tokio::spawn(async move { daemon.check_turn(&agent, asked.0, asked.1).await });
    }

    fn touch(&self, id: &str, now: i64) {
        self.update_agent(id, |a| a.last_activity_at = now);
    }

    async fn on_ready(&self, id: &str) {
        // A dialog (workspace trust) may be sitting in front of the session;
        // typing a prompt into it would answer the dialog with garbage.
        if self.dialog_blocks(id).await {
            return;
        }
        let pending = {
            let mut model = self.model();
            let rt = model.rt(id);
            rt.ready = true;
            rt.pending_prompt.take()
        };
        let kind = self.agent(id).map(|a| a.runtime);
        // Prefer the runtime's socket channel for the first prompt too: it
        // does not depend on the terminal host considering the session ready.
        let wants_socket = kind == Some(RuntimeKind::Claude)
            && self
                .model()
                .local
                .get(id)
                .is_some_and(|l| l.settings.socket_delivery);
        let mut pending = pending;
        if let (true, Some((prompt, ids))) = (wants_socket, pending.clone()) {
            for _ in 0..20 {
                if self.listener(id).is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            if let Some((_, tx)) = self.listener(id) {
                let native_queue = self.model().rt(id).native_queue_bridge;
                let batch_id = if native_queue && !ids.is_empty() {
                    match self.register_receipt(id, ids.clone(), now_ms()) {
                        Ok(key) => Some(key),
                        Err(error) => {
                            self.emit(
                                NewEvent::new("daemon", "agent.prompt_failed", error.message)
                                    .agent(id)
                                    .warn(),
                            );
                            return;
                        }
                    }
                } else {
                    None
                };
                let frame = crew_core::rpc::ServerFrame::push(crew_core::rpc::Push::Deliver {
                    ids: ids.clone(),
                    text: prompt,
                    immediate: false,
                    batch_id,
                });
                if tx.send(frame).await.is_ok() {
                    pending = None;
                    let now = now_ms();
                    {
                        let mut model = self.model();
                        let rt = model.rt(id);
                        if !native_queue {
                            rt.turn_active = true;
                            rt.turn_openers.extend(ids.iter().cloned());
                        }
                        for message in &ids {
                            rt.in_flight.insert(message.clone(), now);
                        }
                    }
                    self.update_agent(id, |a| {
                        a.status = AgentStatus::Working;
                        a.activity.clear();
                    });
                }
            }
        }
        if let Some((prompt, ids)) = pending {
            let lock = self.delivery_lock(id);
            let _guard = lock.lock().await;
            match self.procs() {
                Ok(procs) => match procs.submit_prompt(id, &prompt).await {
                    Ok(()) => {
                        if !ids.is_empty() {
                            let _ = self.bus.mark_delivered(id, &ids);
                        }
                        {
                            let mut model = self.model();
                            let rt = model.rt(id);
                            rt.turn_messages.extend(ids.iter().cloned());
                            rt.turn_openers.extend(ids.iter().cloned());
                        }
                        if kind != Some(RuntimeKind::Shell) {
                            self.model().rt(id).turn_active = true;
                            self.update_agent(id, |a| {
                                a.status = AgentStatus::Working;
                                a.activity.clear();
                            });
                        }
                        tokio::time::sleep(DELIVERY_GAP).await;
                    }
                    Err(e) => self.emit(
                        NewEvent::new(
                            "daemon",
                            "agent.prompt_failed",
                            format!("could not submit the first prompt to {id}: {e:#}"),
                        )
                        .agent(id)
                        .warn(),
                    ),
                },
                Err(e) => tracing::warn!("{}", e.message),
            }
        }
        let turn_active = self.model().rt(id).turn_active;
        if !turn_active {
            let status = self.resting_status(id);
            self.update_agent(id, |a| {
                if a.status == AgentStatus::Spawning {
                    a.status = status;
                    a.activity.clear();
                }
            });
        }
        self.request_delivery(id);
        self.replan();
    }

    fn refresh_usage(&self, id: &str, transcript: String) {
        let Some(agent) = self.agent(id) else { return };
        let runtime = runtime_for(agent.runtime);
        let daemon = self.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            if let Some(usage) = runtime.usage(std::path::Path::new(&transcript)) {
                daemon.update_agent(&id, |a| a.usage = Some(usage));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::Agent;
    use std::sync::Arc;

    #[tokio::test]
    async fn a_hook_being_handled_when_its_launch_is_replaced_finishes_before_the_new_one_begins() {
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::super::start(super::super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = running.daemon.clone();
        let agent: Agent = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "role": "work", "status": "spawning", "runtime": "claude",
            "session_id": "before", "workdir": temp.path().to_str().unwrap(), "backend": "pty",
            "last_activity_at": 0
        }))
        .unwrap();
        daemon.model().agents.push(agent);
        let old = daemon.begin_launch("a").await;
        *daemon.procs.write().unwrap() = Some(Arc::new(crate::process::pty::PtyManager::new(
            daemon.proc_tx.clone(),
        )));

        // A startup dialog being looked at holds the old launch's Ready
        // (rev-core's fixture): its session-start hook is half handled.
        let dialog = daemon.delivery_lock("dialog:a");
        let held = dialog.lock().await;
        let hook = tokio::spawn({
            let (daemon, old) = (daemon.clone(), old.clone());
            async move {
                let payload = serde_json::json!({ "session_id": "old-session" });
                daemon
                    .on_hook("a", Some(&old), "session-start", &payload)
                    .await;
            }
        });
        for _ in 0..1000 {
            if daemon.agent("a").unwrap().session_id.as_deref() == Some("old-session") {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            daemon.agent("a").unwrap().session_id.as_deref(),
            Some("old-session"),
            "the hook did not get to its Ready"
        );

        // The replacement begins, and starts out as start_agent does.
        let replacement = tokio::spawn({
            let daemon = daemon.clone();
            async move {
                let new = daemon.begin_launch("a").await;
                daemon.model().rt.insert(
                    "a".into(),
                    super::super::super::AgentRuntimeState::default(),
                );
                daemon.update_agent("a", |a| {
                    a.session_id = Some("new-session".into());
                    a.status = AgentStatus::Spawning;
                });
                new
            }
        });
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert!(
            !replacement.is_finished(),
            "the new launch began while a hook of the old one was being handled"
        );
        drop(held);
        hook.await.unwrap();
        let new = replacement.await.unwrap();
        // A hook of the old launch arriving now is dropped.
        let payload = serde_json::json!({ "session_id": "old-session" });
        daemon
            .on_hook("a", Some(&old), "session-start", &payload)
            .await;

        let a = daemon.agent("a").unwrap();
        assert_eq!(
            (a.status, a.session_id.as_deref()),
            (AgentStatus::Spawning, Some("new-session")),
            "the old launch's hook changed the new one"
        );
        assert!(
            !daemon.model().rt("a").ready,
            "the old launch's Ready made the new one ready"
        );
        assert_eq!(
            daemon.model().local["a"].launch.as_deref(),
            Some(new.as_str())
        );

        // The new launch's own hooks are taken while its start still holds
        // the launch lease: they wait for nothing it holds.
        let starting = daemon.launch_lease("a");
        let _starting = starting.write().await;
        let activity =
            serde_json::json!({ "tool_name": "Bash", "tool_input": { "command": "new" } });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            daemon.on_hook("a", Some(&new), "pre-tool-use", &activity),
        )
        .await
        .expect("a hook of the starting launch waited for its start");
        assert!(daemon.agent("a").unwrap().activity.contains("new"));
    }
}

#[cfg(test)]
mod codex_parent_tests {
    use super::*;
    use chrono::Local;
    use crew_core::Agent;
    use std::path::PathBuf;

    const CAPTURE: &str = include_str!("../../../tests/fixtures/codex-child/native-0.159.json");

    struct Fixture {
        _temp: tempfile::TempDir,
        running: crate::daemon::Running,
        capture: Value,
    }

    impl Fixture {
        fn daemon(&self) -> &Daemon {
            &self.running.daemon
        }

        fn case(&self, pair: usize, index: usize) -> Value {
            self.capture["pairs"][pair]["cases"][index].clone()
        }

        fn path(&self, member: &str, thread: &str) -> PathBuf {
            self.daemon()
                .agent_dir(member)
                .join("rw/home/codex/sessions")
                .join(Local::now().format("%Y/%m/%d").to_string())
                .join(format!("rollout-fixture-{thread}.jsonl"))
        }

        fn header(&self, member: &str, case: &Value) {
            let path = self.path(member, case["notify"]["thread-id"].as_str().unwrap());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("{}\n", case["session_meta"])).unwrap();
        }

        async fn hook(&self, member: &str, case: &Value) {
            let launch = self.daemon().model().local[member].launch.clone().unwrap();
            self.daemon()
                .on_hook(member, Some(&launch), "codex-notify", &case["notify"])
                .await;
        }
    }

    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let running = crate::daemon::start(crate::daemon::DaemonOptions {
            workspace: temp.path().into(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        daemon.model().session.team = Some(
            crew_core::config::TeamConfig::from_yaml(
                "project: codex-parent\nworkspace:\n  sandbox:\n    mode: required\n",
            )
            .unwrap(),
        );
        for id in ["parent", "member-child"] {
            let agent: Agent = serde_json::from_value(serde_json::json!({
                "id":id,"name":id,"role":"worker","status":"working","runtime":"codex",
                "workdir":temp.path(),"backend":"pty","started_at":now_ms(),"last_activity_at":1,
                "parent": (id == "member-child").then_some("parent")
            }))
            .unwrap();
            daemon.store.save_agent(&agent).unwrap();
            daemon.model().agents.push(agent);
            std::fs::create_dir_all(daemon.agent_dir(id).join("rw/home")).unwrap();
            daemon.begin_launch(id).await;
            daemon.model().rt(id).turn_active = true;
        }
        Fixture {
            _temp: temp,
            running,
            capture: serde_json::from_str(CAPTURE).unwrap(),
        }
    }

    #[tokio::test]
    async fn native_child_completion_cannot_replace_or_end_the_confirmed_parent() {
        let f = fixture().await;
        let parent = f.case(0, 0);
        let child = f.case(0, 1);
        f.header("parent", &parent);
        f.header("parent", &child);
        f.daemon().model().rt("parent").rollout =
            Some(f.path("parent", child["notify"]["thread-id"].as_str().unwrap()));
        f.daemon()
            .model()
            .local
            .get_mut("parent")
            .unwrap()
            .codex_turn = Some("old-child-turn".into());
        f.hook("parent", &parent).await;
        let thread = parent["notify"]["thread-id"].as_str().unwrap();
        assert!(f.daemon().confirmed_codex_parent("parent", thread));
        assert!(f.daemon().model().rt("parent").rollout.is_none());
        assert!(f.daemon().model().local["parent"].codex_turn.is_none());
        f.daemon().model().rt("parent").turn_active = true;
        f.daemon().model().rt("parent").turn_openers = vec!["m-current".into()];
        let before = serde_json::to_value(f.daemon().agent("parent")).unwrap();
        f.hook("parent", &child).await;
        assert_eq!(
            serde_json::to_value(f.daemon().agent("parent")).unwrap(),
            before
        );
        assert!(f.daemon().model().rt("parent").turn_active);
        assert_eq!(f.daemon().model().rt("parent").turn_openers, ["m-current"]);
        assert_eq!(
            f.daemon().model().local["parent"]
                .codex_parent_session
                .as_deref(),
            Some(thread)
        );
        let saved = f
            .daemon()
            .store
            .get_meta(&crate::daemon::local_key("parent"))
            .unwrap()
            .unwrap();
        let restored: crate::daemon::AgentLocal = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.codex_parent_session.as_deref(), Some(thread));
        assert!(restored.session_confirmed);
        let queue = runtime_for(RuntimeKind::Codex)
            .queue_command(
                std::path::Path::new("/fixture.sock"),
                f.daemon()
                    .agent("parent")
                    .unwrap()
                    .session_id
                    .as_deref()
                    .unwrap(),
                "message",
            )
            .unwrap();
        let index = queue.iter().position(|arg| arg == "--thread").unwrap();
        assert_eq!(queue[index + 1], thread);
        assert!(!queue
            .iter()
            .any(|arg| arg == child["notify"]["thread-id"].as_str().unwrap()));
    }

    #[tokio::test]
    async fn child_first_and_informational_client_marker_never_confirm_a_thread() {
        let f = fixture().await;
        let mut child = f.case(0, 1);
        child["notify"]["client"] = serde_json::json!("codex-tui");
        f.header("parent", &child);
        let before = serde_json::to_value(f.daemon().agent("parent")).unwrap();
        f.hook("parent", &child).await;
        assert_eq!(
            serde_json::to_value(f.daemon().agent("parent")).unwrap(),
            before
        );
        assert!(!f.daemon().model().local["parent"].session_confirmed);
        assert!(f.daemon().model().local["parent"]
            .codex_parent_session
            .is_none());
        assert!(f.daemon().model().rt("parent").turn_active);
        f.daemon().prepare_codex_parent_resume("parent", true).await;
        assert!(f.daemon().agent("parent").unwrap().session_id.is_none());
    }

    #[tokio::test]
    async fn missing_partial_or_oversized_header_waits_for_actual_parent_proof() {
        let f = fixture().await;
        let parent = f.case(0, 0);
        let thread = parent["notify"]["thread-id"].as_str().unwrap();
        f.hook("parent", &parent).await;
        assert!(!f.daemon().model().local["parent"].session_confirmed);
        let path = f.path("parent", thread);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"type\":\"session_meta\"").unwrap();
        f.hook("parent", &parent).await;
        assert!(!f.daemon().model().local["parent"].session_confirmed);
        let mut oversized = parent.clone();
        oversized["session_meta"]["padding"] = serde_json::json!("x".repeat(256 * 1024));
        f.header("parent", &oversized);
        f.hook("parent", &parent).await;
        assert!(!f.daemon().model().local["parent"].session_confirmed);
        assert!(f.daemon().model().rt("parent").turn_active);
        f.header("parent", &parent);
        f.hook("parent", &parent).await;
        assert!(f.daemon().confirmed_codex_parent("parent", thread));
        assert!(!f.daemon().model().rt("parent").turn_active);
    }

    #[tokio::test]
    async fn same_id_metadata_uses_id_not_the_inherited_session_id_alias() {
        let f = fixture().await;
        let mut parent = f.case(0, 0);
        parent["session_meta"]["payload"]["id"] = f.case(0, 1)["notify"]["thread-id"].clone();
        f.header("parent", &parent);
        f.hook("parent", &parent).await;
        assert!(!f.daemon().model().local["parent"].session_confirmed);
        assert!(f.daemon().agent("parent").unwrap().session_id.is_none());
    }

    #[tokio::test]
    async fn nested_children_are_rejected_and_crew_members_confirm_their_own_roots() {
        let f = fixture().await;
        let parent = f.case(0, 0);
        let own = f.case(1, 0);
        let mut nested = f.case(1, 1);
        nested["session_meta"]["payload"]["source"]["subagent"]["thread_spawn"]["depth"] =
            serde_json::json!(2);
        f.header("parent", &parent);
        f.hook("parent", &parent).await;
        f.daemon().model().rt("parent").turn_active = true;
        f.header("parent", &nested);
        f.hook("parent", &nested).await;
        assert!(f.daemon().model().rt("parent").turn_active);
        f.header("member-child", &own);
        f.hook("member-child", &own).await;
        assert!(f
            .daemon()
            .confirmed_codex_parent("member-child", own["notify"]["thread-id"].as_str().unwrap()));
        assert!(f
            .daemon()
            .confirmed_codex_parent("parent", parent["notify"]["thread-id"].as_str().unwrap()));
    }

    #[tokio::test]
    async fn legacy_child_resume_is_fresh_while_a_proven_parent_resumes() {
        for index in [1, 0] {
            let f = fixture().await;
            let case = f.case(0, index);
            let thread = case["notify"]["thread-id"].as_str().unwrap();
            f.header("parent", &case);
            f.daemon()
                .update_agent("parent", |agent| agent.session_id = Some(thread.into()));
            f.daemon()
                .model()
                .local
                .get_mut("parent")
                .unwrap()
                .session_confirmed = true;
            f.daemon().prepare_codex_parent_resume("parent", true).await;
            if index == 0 {
                assert!(f.daemon().confirmed_codex_parent("parent", thread));
                f.daemon()
                    .prepare_codex_parent_resume("parent", false)
                    .await;
            }
            assert!(f.daemon().agent("parent").unwrap().session_id.is_none());
            assert!(!f.daemon().model().local["parent"].session_confirmed);
            assert!(f.daemon().model().local["parent"]
                .codex_parent_session
                .is_none());
        }
    }

    #[tokio::test]
    async fn an_old_uuidv7_parent_header_is_found_without_scanning_other_homes() {
        let f = fixture().await;
        let parent = f.case(0, 0);
        f.header("parent", &parent);
        f.daemon().update_agent("parent", |agent| {
            agent.started_at = Some(now_ms() + 10 * 86_400_000)
        });
        f.hook("parent", &parent).await;
        assert!(f
            .daemon()
            .confirmed_codex_parent("parent", parent["notify"]["thread-id"].as_str().unwrap()));
    }

    #[tokio::test]
    async fn a_header_flushed_after_notify_confirms_without_a_second_notification() {
        let f = fixture().await;
        let parent = f.case(0, 0);
        let path = f.path("parent", parent["notify"]["thread-id"].as_str().unwrap());
        let header = parent["session_meta"].clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("{header}\n")).unwrap();
        });
        f.hook("parent", &parent).await;
        writer.await.unwrap();
        assert!(f
            .daemon()
            .confirmed_codex_parent("parent", parent["notify"]["thread-id"].as_str().unwrap()));
        assert!(!f.daemon().model().rt("parent").turn_active);
    }

    #[tokio::test]
    async fn permanently_unprovable_header_has_visible_attention_and_holds_native_work() {
        let f = fixture().await;
        let parent = f.case(0, 0);
        f.hook("parent", &parent).await;
        let agent = f.daemon().agent("parent").unwrap();
        assert!(agent.activity.contains("could not be confirmed"));
        assert_eq!(
            agent.last_activity_at, 1,
            "identity observation is not activity"
        );
        assert!(f.daemon().model().rt("parent").turn_active);
        assert!(f
            .daemon()
            .model()
            .rt("parent")
            .codex_pending_thread
            .is_some());
        assert!(!f.daemon().model().local["parent"].session_confirmed);
        f.daemon().prepare_codex_parent_resume("parent", true).await;
        assert!(f.daemon().agent("parent").unwrap().session_id.is_none());
    }

    #[tokio::test]
    async fn a_new_verified_user_root_rebinds_without_accepting_native_children() {
        let f = fixture().await;
        let first = f.case(0, 0);
        let next = f.case(1, 0);
        f.header("parent", &first);
        f.hook("parent", &first).await;
        f.header("parent", &next);
        f.daemon().model().rt("parent").turn_active = true;
        f.hook("parent", &next).await;
        let thread = next["notify"]["thread-id"].as_str().unwrap();
        assert!(f.daemon().confirmed_codex_parent("parent", thread));
        assert_eq!(
            f.daemon().agent("parent").unwrap().session_id.as_deref(),
            Some(thread)
        );
        assert!(f
            .daemon()
            .store
            .recent_events(None, 100)
            .unwrap()
            .iter()
            .any(|event| event.event_type == "agent.session_changed"));
        let child = f.case(0, 1);
        f.header("parent", &child);
        f.daemon().model().rt("parent").turn_active = true;
        f.hook("parent", &child).await;
        assert!(f.daemon().confirmed_codex_parent("parent", thread));
        assert!(f.daemon().model().rt("parent").turn_active);
    }

    #[tokio::test]
    async fn legacy_root_without_thread_source_is_accepted_but_legacy_child_is_not() {
        let f = fixture().await;
        let mut root = f.case(0, 0);
        root["session_meta"]["payload"]
            .as_object_mut()
            .unwrap()
            .remove("thread_source");
        root["session_meta"]["payload"]["source"] = serde_json::json!("cli");
        f.header("parent", &root);
        f.hook("parent", &root).await;
        let mut child = f.case(0, 1);
        child["session_meta"]["payload"]
            .as_object_mut()
            .unwrap()
            .remove("thread_source");
        f.header("parent", &child);
        f.daemon().model().rt("parent").turn_active = true;
        f.hook("parent", &child).await;
        assert!(f
            .daemon()
            .confirmed_codex_parent("parent", root["notify"]["thread-id"].as_str().unwrap()));
        assert!(f.daemon().model().rt("parent").turn_active);
    }

    #[tokio::test]
    async fn a_missing_child_rollout_never_revokes_the_known_parent_binding() {
        let f = fixture().await;
        let parent = f.case(0, 0);
        f.header("parent", &parent);
        f.hook("parent", &parent).await;
        let thread = parent["notify"]["thread-id"].as_str().unwrap();
        let before = serde_json::to_value(f.daemon().agent("parent")).unwrap();
        f.daemon().model().rt("parent").turn_active = true;
        let child = f.case(0, 1);
        f.hook("parent", &child).await;
        assert_eq!(
            serde_json::to_value(f.daemon().agent("parent")).unwrap(),
            before
        );
        assert!(f.daemon().confirmed_codex_parent("parent", thread));
        assert!(f.daemon().model().rt("parent").turn_active);
        assert!(f
            .daemon()
            .model()
            .rt("parent")
            .codex_pending_thread
            .is_none());
        f.daemon().prepare_codex_parent_resume("parent", true).await;
        assert_eq!(
            f.daemon().agent("parent").unwrap().session_id.as_deref(),
            Some(thread)
        );
        assert!(f.daemon().confirmed_codex_parent("parent", thread));
    }
}
