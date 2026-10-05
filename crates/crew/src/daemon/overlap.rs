//! Waiting cards keep their owner while releasing the execution slot.
//! Inbox delivery is independent of the board: messages may be read during
//! another card, so durable wakeups point the owner back at parked work later.

use crew_core::rpc::RpcError;
use crew_core::{AgentStatus, Message, MessageType, Task, TaskStatus};
use serde_json::json;

use super::{local_key, Daemon, Model};
use crate::message_bus::NewMessage;
use crate::persistence::NewEvent;
use crate::process::ProcessManager;
use crate::tasks::{active_task, executing_task};

pub(super) fn track_pause(model: &mut Model, before: &Task, after: &Task) -> Option<String> {
    let agent = after.assigned_agent.as_deref()?;
    let changed = before.status != after.status || before.started_at != after.started_at;
    if !changed {
        return None;
    }
    let local = model.local.entry(agent.to_string()).or_default();
    if after.started_at.is_none()
        || after.started_at != before.started_at
        || after.status.is_terminal()
        || after.status == TaskStatus::Review
    {
        local.task_wakeups.remove(&after.id);
        local.task_wakeup_seen.remove(&after.id);
    }
    if after.started_at.is_none()
        || after.started_at != before.started_at
        || after.status.is_terminal()
    {
        local.task_pauses.remove(&after.id);
    } else {
        let now = crew_core::util::now_ms();
        let pause = local.task_pauses.entry(after.id.clone()).or_default();
        if after.status == TaskStatus::Waiting {
            pause.since.get_or_insert(now);
        } else if let Some(since) = pause.since.take() {
            pause.total_ms += (now - since).max(0);
        }
    }
    Some(agent.to_string())
}

impl Daemon {
    /// Agent metadata follows the same selection as id-less task calls.
    pub(crate) fn project_task(&self, agent: &str) {
        let task = active_task(&self.model().tasks, agent).cloned();
        self.update_agent(agent, |a| {
            a.task = task.as_ref().map(|t| t.title.clone());
            if let Some(task) = &task {
                a.progress = task.progress;
            }
        });
    }

    /// New cue messages should reach a free parked owner directly, instead
    /// of starting an extra resume-notice turn before the answer arrives.
    pub(crate) fn pending_task_wakeup(&self, agent: &str, pending: &[Message]) -> bool {
        let model = self.model();
        model.local.get(agent).is_some_and(|local| {
            model.tasks.iter().any(|task| {
                task.assigned_agent.as_deref() == Some(agent)
                    && task.status == TaskStatus::Waiting
                    && local
                        .task_wakeups
                        .get(&task.id)
                        .is_some_and(|ids| pending.iter().any(|message| ids.contains(&message.id)))
            })
        })
    }

    /// At-rest delivery resumes the card whose cue is actually in this
    /// batch. Messages delivered during another card leave a durable cue
    /// instead; only those later need a separate planner notice.
    pub(crate) fn resume_waiting_from_delivery(&self, agent: &str, ids: &[String]) {
        let task = {
            let model = self.model();
            if executing_task(&model.tasks, agent).is_some() {
                return;
            }
            let Some(local) = model.local.get(agent) else {
                return;
            };
            let waiting: Vec<_> = model
                .tasks
                .iter()
                .filter(|task| {
                    task.assigned_agent.as_deref() == Some(agent)
                        && task.status == TaskStatus::Waiting
                        && local
                            .task_wakeups
                            .get(&task.id)
                            .is_some_and(|cues| cues.iter().any(|cue| ids.contains(cue)))
                })
                .cloned()
                .collect();
            active_task(&waiting, agent).map(|t| t.id.clone())
        };
        let Some(task) = task else { return };
        let mut claimed = false;
        let resumed = self.try_update_task_checked(&task, |model, current| {
            if current.status == TaskStatus::Waiting
                && executing_task(&model.tasks, agent).is_none()
                && model.agent(agent).is_some_and(|a| !a.status.is_terminal())
            {
                current.status = TaskStatus::Working;
                claimed = true;
            }
            Ok(())
        });
        match resumed {
            Ok(_) if claimed => {
                self.clear_task_wakeups(agent, &task);
                let mut model = self.model();
                model.rt(agent).self_status = None;
                model.rt(agent).parking_turn = false;
                drop(model);
                self.project_task(agent);
            }
            Err(e) => tracing::error!("{}", e.message),
            _ => {}
        }
    }

    /// Save before publishing the cue. Delivery retries this operation before
    /// acknowledging a message, so a refused write cannot consume its wakeup.
    pub(crate) fn record_task_wakeups(
        &self,
        agent: &str,
        messages: &[Message],
    ) -> Result<(), RpcError> {
        let mut model = self.model();
        let parked: Vec<_> = model
            .tasks
            .iter()
            .filter(|t| {
                t.assigned_agent.as_deref() == Some(agent) && t.status == TaskStatus::Waiting
            })
            .map(|t| t.id.clone())
            .collect();
        if parked.is_empty() {
            return Ok(());
        }
        let mut local = model.local.get(agent).cloned().unwrap_or_default();
        let before = local.task_wakeups.clone();
        for message in messages {
            // A new card or review request asks for independent work. Our
            // own resume notice must not wake the other parked cards again.
            if matches!(message.kind, MessageType::Task | MessageType::ReviewRequest)
                || message
                    .metadata
                    .get("task_resumption")
                    .and_then(|v| v.as_bool())
                    == Some(true)
            {
                continue;
            }
            let explicit = message.metadata.get("task_id").and_then(|v| v.as_str());
            // Generic scheduler bookkeeping is not news for parked work.
            // A scoped card edit is relevant; dispatches and review requests
            // never are. Teammate/operator traffic may be unscoped.
            if message.from == "scheduler"
                && message.kind == MessageType::Status
                && explicit.is_none()
            {
                continue;
            }
            let replied = message
                .reply_to
                .as_deref()
                .and_then(|id| self.bus.get(id).ok().flatten())
                .filter(|(original, _)| original.from == agent)
                .and_then(|(original, _)| {
                    original
                        .metadata
                        .get("from_task_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                });
            let target = explicit.or(replied.as_deref());
            for task in &parked {
                // Untargeted news may concern any parked card. Each gets one
                // reminder, in selection order, without losing the original
                // message even if the inbox was acknowledged mid-card.
                if target.is_some_and(|id| id != task) {
                    continue;
                }
                if local
                    .task_wakeup_seen
                    .get(task)
                    .is_some_and(|seen| seen.contains(&message.id))
                {
                    continue;
                }
                let ids = local.task_wakeups.entry(task.clone()).or_default();
                if !ids.contains(&message.id) {
                    ids.push(message.id.clone());
                }
            }
        }
        if local.task_wakeups == before {
            return Ok(());
        }
        let json = serde_json::to_string(&local).map_err(|e| RpcError::internal(e.to_string()))?;
        self.store.set_meta(&local_key(agent), &json).map_err(|e| {
            RpcError::internal(format!(
                "persisting parked messages for {agent} failed: {e:#}"
            ))
        })?;
        model.local.insert(agent.to_string(), local);
        drop(model);
        self.replan();
        Ok(())
    }

    pub(crate) fn clear_task_wakeups(&self, agent: &str, task: &str) {
        let changed = self.model().local.get_mut(agent).is_some_and(|local| {
            if let Some(ids) = local.task_wakeups.remove(task) {
                local
                    .task_wakeup_seen
                    .entry(task.to_string())
                    .or_default()
                    .extend(ids);
                true
            } else {
                false
            }
        });
        if changed {
            self.save_local(agent);
        }
    }

    pub(crate) fn forget_task_waiting_state(&self, agent: &str, task: &str) {
        let changed = self.model().local.get_mut(agent).is_some_and(|local| {
            let cues = local.task_wakeups.remove(task).is_some();
            let seen = local.task_wakeup_seen.remove(task).is_some();
            let pause = local.task_pauses.remove(task).is_some();
            cues || seen || pause
        });
        if changed {
            self.save_local(agent);
        }
    }

    /// Called under the planning lock. A pending parked card resumes before
    /// new Ready work, newest start first; ties keep board order. A running
    /// turn must yield first, including the turn that parked its card.
    pub(super) async fn resume_parked_tasks(
        &self,
        procs: Option<&dyn ProcessManager>,
        homeward: &[String],
    ) {
        let candidates: Vec<_> = {
            let model = self.model();
            model
                .agents
                .iter()
                .filter_map(|agent| {
                    if agent.paused
                        || agent.status.is_terminal()
                        || agent.limit.is_some()
                        || matches!(agent.status, AgentStatus::Spawning | AgentStatus::Blocked)
                        || homeward.contains(&agent.id)
                        || matches!(agent.block_kind.as_deref(), Some("permission" | "dialog"))
                        || !procs.is_some_and(|p| p.is_alive(&agent.id))
                        || executing_task(&model.tasks, &agent.id).is_some()
                        || model
                            .rt
                            .get(&agent.id)
                            .is_some_and(|rt| rt.turn_active || rt.pending_prompt.is_some())
                    {
                        return None;
                    }
                    let wakeups = &model.local.get(&agent.id)?.task_wakeups;
                    let waiting: Vec<_> = model
                        .tasks
                        .iter()
                        .filter(|t| {
                            t.status == TaskStatus::Waiting
                                && wakeups.get(&t.id).is_some_and(|ids| !ids.is_empty())
                        })
                        .cloned()
                        .collect();
                    let task = active_task(&waiting, &agent.id)?;
                    Some((agent.id.clone(), task.clone(), wakeups[&task.id].clone()))
                })
                .collect()
        };
        for (agent, task, messages) in candidates {
            let pending = self.bus.undelivered(&agent).unwrap_or_default();
            if messages
                .iter()
                .all(|id| pending.iter().any(|message| message.id == *id))
            {
                self.request_delivery(&agent);
                continue;
            }
            let quoted: Vec<_> = messages.iter().take(32).cloned().collect();
            let body = format!(
                "Resume your parked task {} (\"{}\"). task_get(\"{}\") reads its card. \
Messages received while it waited: {}. agent_inbox(id: \"…\") reads each original message, \
including one already read during another task; agent_inbox(all: true) reads earlier mail. \
Inspect them and continue this card, or \
report waiting again if its answer is still missing. Id-less task calls now address this card.",
                task.id,
                task.title,
                task.id,
                quoted.join(", ")
            );
            let notice = NewMessage::new("scheduler", &agent, MessageType::Status, body).metadata(
                json!({"task_id": task.id, "task_resumption": true, "message_ids": quoted}),
            );
            let resumed = self.try_update_task_telling_checked(
                &task.id,
                |model, current| {
                    if current.status != TaskStatus::Waiting
                        || current.assigned_agent.as_deref() != Some(&agent)
                        || executing_task(&model.tasks, &agent).is_some()
                        || model.rt.get(&agent).is_some_and(|rt| rt.turn_active)
                    {
                        return Err(RpcError::conflict(
                            "execution capacity changed before resuming parked task",
                        ));
                    }
                    current.status = TaskStatus::Working;
                    Ok(())
                },
                notice,
                vec![agent.clone()],
            );
            match resumed {
                Ok(Some((task, outcome))) => {
                    self.clear_task_wakeups(&agent, &task.id);
                    self.model().rt(&agent).self_status = None;
                    self.project_task(&agent);
                    self.settle(&agent);
                    self.update_agent(&agent, |a| a.activity = format!("resuming {}", task.title));
                    self.emit(
                        NewEvent::new(
                            "scheduler",
                            "task.resumed",
                            format!("task {} resumed by {agent}", task.id),
                        )
                        .task(&task.id)
                        .agent(&agent),
                    );
                    self.message_sent(&outcome, false).await;
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("resuming task {}: {}", task.id, e.message),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{start, DaemonOptions};
    use crate::process::Spawned;
    use crew_core::Agent;

    fn card(id: &str, status: TaskStatus, started: i64) -> Task {
        serde_json::from_value(json!({
            "id":id, "title":id, "status":status, "assigned_agent":"owner",
            "created_at":started, "started_at":started, "progress":41
        }))
        .unwrap()
    }

    fn owner(workspace: &std::path::Path) -> Agent {
        serde_json::from_value(json!({
            "id":"owner", "name":"owner", "role":"architect", "status":"waiting",
            "runtime":"shell", "workdir":workspace, "backend":"pty", "last_activity_at":0
        }))
        .unwrap()
    }

    struct Alive;
    #[async_trait::async_trait]
    impl ProcessManager for Alive {
        fn name(&self) -> &'static str {
            "pty"
        }
        async fn spawn(&self, _: &crate::agents::LaunchSpec) -> anyhow::Result<Spawned> {
            unreachable!()
        }
        async fn submit_prompt(&self, _: &str, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn write_raw(&self, _: &str, _: &[u8]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn screen(&self, _: &str, _: usize) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn pause(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn resume(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn kill(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_alive(&self, _: &str) -> bool {
            true
        }
        async fn adopt(&self, _: &Agent) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn a_saved_read_answer_survives_daemon_restart_and_resumes_once() {
        let temp = tempfile::tempdir().unwrap();
        let options = || DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        };
        let answer;
        {
            let running = start(options()).await.unwrap();
            let daemon = &running.daemon;
            let agent = owner(temp.path());
            daemon.store.save_agent(&agent).unwrap();
            daemon.model().agents.push(agent);
            let first = card("first", TaskStatus::Working, 1);
            daemon.store.save_task(&first).unwrap();
            daemon.model().tasks.push(first);
            daemon
                .mirror_status_to_task("owner", AgentStatus::Waiting, Some("on schema".into()))
                .unwrap();
            let current = card("current", TaskStatus::Working, 2);
            daemon.store.save_task(&current).unwrap();
            daemon.model().tasks.push(current);
            answer = daemon
                .bus
                .send(
                    NewMessage::new("schema", "owner", MessageType::Answer, "schema available"),
                    vec!["owner".into()],
                )
                .unwrap()
                .message;
            daemon
                .record_task_wakeups("owner", std::slice::from_ref(&answer))
                .unwrap();
            daemon
                .bus
                .mark_delivered("owner", std::slice::from_ref(&answer.id))
                .unwrap();
            daemon.bus.acknowledge("owner", None).unwrap();
        }
        // A new daemon reloads the durable task and private local state.
        let running = start(options()).await.unwrap();
        let daemon = &running.daemon;
        assert_eq!(
            daemon.model().local["owner"].task_wakeups["first"],
            std::slice::from_ref(&answer.id)
        );
        assert!(daemon.model().local["owner"].task_pauses["first"]
            .since
            .is_some());
        assert!(daemon.bus.undelivered("owner").unwrap().is_empty());
        daemon
            .complete_task(
                &super::super::server::Who::Agent("owner".into()),
                None,
                "current finished".into(),
                vec![],
            )
            .await
            .unwrap();
        daemon.resume_parked_tasks(Some(&Alive), &[]).await;
        daemon.resume_parked_tasks(Some(&Alive), &[]).await;
        assert_eq!(
            daemon.model().task("first").unwrap().status,
            TaskStatus::Working
        );
        assert_eq!(
            daemon.agent("owner").unwrap().task.as_deref(),
            Some("first")
        );
        assert_eq!(daemon.agent("owner").unwrap().progress, Some(41));
        let notices: Vec<_> = daemon
            .bus
            .history(Some("owner"), 20)
            .unwrap()
            .into_iter()
            .filter(|m| m.metadata["task_resumption"] == true)
            .collect();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].metadata["message_ids"], json!([answer.id]));
        assert!(daemon.bus.get(&answer.id).unwrap().is_some());
    }

    #[tokio::test]
    async fn scheduler_bookkeeping_is_cue_free_scoped_news_targets_only_its_card() {
        let temp = tempfile::tempdir().unwrap();
        let running = start(DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        daemon.model().tasks.extend([
            card("older", TaskStatus::Waiting, 1),
            card("newer", TaskStatus::Waiting, 2),
        ]);
        let send = |from, kind, metadata| {
            daemon
                .bus
                .send(
                    NewMessage::new(from, "owner", kind, "news").metadata(metadata),
                    vec!["owner".into()],
                )
                .unwrap()
                .message
        };
        for message in [
            send("scheduler", MessageType::Status, json!({})),
            send(
                "scheduler",
                MessageType::Task,
                json!({"task_id":"independent"}),
            ),
            send(
                "other",
                MessageType::ReviewRequest,
                json!({"task_id":"review-other"}),
            ),
            send(
                "scheduler",
                MessageType::Status,
                json!({"task_id":"older", "task_resumption":true}),
            ),
        ] {
            daemon.record_task_wakeups("owner", &[message]).unwrap();
        }
        assert!(!daemon.model().local.contains_key("owner"));
        let scoped = send("operator", MessageType::Message, json!({"task_id":"older"}));
        daemon
            .record_task_wakeups("owner", std::slice::from_ref(&scoped))
            .unwrap();
        assert_eq!(daemon.model().local["owner"].task_wakeups.len(), 1);
        assert_eq!(
            daemon.model().local["owner"].task_wakeups["older"],
            std::slice::from_ref(&scoped.id)
        );
        daemon.clear_task_wakeups("owner", "older");
        daemon.record_task_wakeups("owner", &[scoped]).unwrap();
        assert!(
            daemon.model().local["owner"].task_wakeups.is_empty(),
            "replayed delivery cannot recreate a consumed cue"
        );
        // A real teammate may also be named scheduler. Its answers are
        // ordinary news; only the daemon's generic status notices are held.
        let answer = send("scheduler", MessageType::Answer, serde_json::Value::Null);
        daemon.record_task_wakeups("owner", &[answer]).unwrap();
        assert_eq!(daemon.model().local["owner"].task_wakeups.len(), 2);
    }

    #[test]
    fn waiting_time_is_excluded_without_changing_task_identity_or_start_order() {
        let mut model = Model::default();
        let working = card("first", TaskStatus::Working, 1);
        let mut waiting = working.clone();
        waiting.status = TaskStatus::Waiting;
        track_pause(&mut model, &working, &waiting);
        model
            .local
            .get_mut("owner")
            .unwrap()
            .task_pauses
            .get_mut("first")
            .unwrap()
            .since = Some(crew_core::util::now_ms() - 50_000);
        track_pause(&mut model, &waiting, &working);
        let pause = &model.local["owner"].task_pauses["first"];
        assert!(pause.total_ms >= 50_000);
        assert!(pause.since.is_none());
        assert_eq!(working.started_at, waiting.started_at);
        let mut retried = working.clone();
        retried.status = TaskStatus::Queued;
        retried.started_at = None;
        track_pause(&mut model, &working, &retried);
        assert!(!model.local["owner"].task_pauses.contains_key("first"));
    }

    #[tokio::test]
    async fn a_blocked_owner_stalls_using_the_daemons_actual_agent_view() {
        let temp = tempfile::tempdir().unwrap();
        let running = start(DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        {
            let mut model = daemon.model();
            model.session.team =
                Some(crew_core::config::TeamConfig::from_yaml("project: blocked-view\n").unwrap());
            let mut agent = owner(temp.path());
            agent.status = AgentStatus::Blocked;
            agent.block_kind = Some("self".into());
            model.agents.push(agent);
            model.tasks.push(card("blocked", TaskStatus::Blocked, 1));
            model.tasks.push(card("next", TaskStatus::Ready, 2));
        }
        *daemon.procs.write().unwrap() = Some(std::sync::Arc::new(Alive));
        daemon.apply_plan().await;
        let model = daemon.model();
        assert!(model
            .stall
            .as_ref()
            .is_some_and(|(reason, _, _)| reason.contains("blocked")));
        assert_eq!(model.task("next").unwrap().status, TaskStatus::Ready);
    }

    #[tokio::test]
    async fn only_the_turn_that_parked_a_card_holds_the_next_ready_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let running = start(DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        {
            let mut model = daemon.model();
            model.session.team =
                Some(crew_core::config::TeamConfig::from_yaml("project: parking-turn\n").unwrap());
            let mut agent = owner(temp.path());
            agent.status = AgentStatus::Working;
            model.agents.push(agent);
            model.tasks.push(card("first", TaskStatus::Ready, 1));
            // A launch optimistically marks its first prompt active. That
            // alone must not starve ordinary Ready cards if no stop hook
            // arrives (the existing fresh-restart runtime contract).
            model.rt("owner").turn_active = true;
        }
        *daemon.procs.write().unwrap() = Some(std::sync::Arc::new(Alive));
        daemon.apply_plan().await;
        assert_eq!(
            daemon.model().task("first").unwrap().status,
            TaskStatus::Working
        );
        daemon
            .mirror_status_to_task("owner", AgentStatus::Waiting, Some("on schema".into()))
            .unwrap();
        daemon
            .model()
            .tasks
            .push(card("next", TaskStatus::Ready, 2));
        daemon.apply_plan().await;
        assert_eq!(
            daemon.model().task("next").unwrap().status,
            TaskStatus::Ready
        );
        daemon
            .on_signal("owner", crate::agents::Signal::TurnEnded)
            .await;
        daemon.apply_plan().await;
        assert_eq!(
            daemon.model().task("next").unwrap().status,
            TaskStatus::Working
        );
        assert_eq!(
            daemon.model().task("first").unwrap().status,
            TaskStatus::Waiting
        );
    }
}
