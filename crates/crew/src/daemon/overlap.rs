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
            if message.from == "scheduler" && explicit.is_none() {
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
                        .filter(|t| wakeups.get(&t.id).is_some_and(|ids| !ids.is_empty()))
                        .cloned()
                        .collect();
                    let task = active_task(&waiting, &agent.id)?;
                    Some((agent.id.clone(), task.clone(), wakeups[&task.id].clone()))
                })
                .collect()
        };
        for (agent, task, messages) in candidates {
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
