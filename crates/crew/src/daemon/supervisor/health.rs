//! Agent health, checked over time: idle nudges, stalls, unresponsive agents, provider errors.

use crew_core::util::now_ms;
use crew_core::{AgentStatus, TaskStatus};

use crate::daemon::Daemon;
use crate::persistence::NewEvent;
use crate::tasks::active_task;

impl Daemon {
    /// Agent `id` went quiet (`why`). If its last turn ended in the
    /// provider's error, that error is the reason: reported as one, with
    /// the provider's words, never as "unresponsive" (a reminder only gets
    /// the same error). Otherwise it is unresponsive.
    pub(crate) async fn stalled(&self, id: &str, why: String) {
        let provider_error = self.model().rt(id).provider_error.clone();
        let Some(said) = provider_error else {
            self.update_agent(id, |a| {
                a.status = AgentStatus::Blocked;
                a.block_kind = Some("unresponsive".into());
                a.activity = why.clone();
            });
            self.emit(
                NewEvent::new(
                    "scheduler",
                    "agent.unresponsive",
                    format!("{id} is unresponsive: {why}"),
                )
                .agent(id)
                .warn(),
            );
            self.notify_orchestrator(&format!("Agent {id} is unresponsive: {why}."))
                .await;
            return;
        };
        let runtime = self
            .agent(id)
            .map(|a| crate::models::runtime_label(a.runtime).to_string())
            .unwrap_or_default();
        let error = format!("{runtime} said: {said}");
        self.update_agent(id, |a| {
            a.status = AgentStatus::Blocked;
            a.block_kind = Some("provider_error".into());
            a.activity = format!("its turns end in an error: {said}");
            a.error = Some(error.clone());
        });
        self.emit(
            NewEvent::new(
                "scheduler",
                "agent.error",
                format!("{id}: its turns end in an error. {error}"),
            )
            .agent(id)
            .error(),
        );
        self.notify_orchestrator(&format!(
            "{id}'s turns end in an error from its provider ({error}), so reminders do not help. \
If it persists, hand its work to a member on another model, or ask the operator (operator_ask) to \
restart {id} fresh or move it to another CLI."
        ))
        .await;
    }

    pub(crate) async fn housekeeping(&self) {
        let now = now_ms();
        // Before anything else, team or none: the copy of crew agents are
        // handed is still the one made.
        self.binary_housekeeping().await;
        let Some(limits) = self.model().session.team.as_ref().map(|t| t.limits.clone()) else {
            return;
        };
        self.limit_housekeeping().await;
        self.context_housekeeping().await;
        self.transcript_housekeeping().await;
        self.temporary_housekeeping().await;

        self.task_health_at(now, &limits).await;
        self.keepalive_housekeeping().await;
    }

    /// Task health uses the tick's captured time, so the reminder boundary and
    /// retry budget can be tested without sleeping or changing their defaults.
    pub(crate) async fn task_health_at(&self, now: i64, limits: &crew_core::config::Limits) {
        enum Todo {
            Remind {
                agent: String,
                task_id: String,
                title: String,
            },
            Unresponsive {
                agent: String,
                why: String,
            },
            Timeout {
                task_id: String,
                secs: u64,
            },
        }
        let mut todo = Vec::new();
        {
            let mut model = self.model();
            let agents = model.agents.clone();
            let tasks = model.tasks.clone();
            for agent in &agents {
                if agent.status.is_terminal() || agent.paused {
                    continue;
                }
                let task = active_task(&tasks, &agent.id);
                let rt = model.rt(&agent.id);
                match agent.status {
                    AgentStatus::Idle if rt.ready && !rt.turn_active => {
                        let Some(task) = task.filter(|t| t.status == TaskStatus::Working) else {
                            continue;
                        };
                        let since = *rt.idle_since.get_or_insert(now);
                        if now - since < (limits.idle_nudge_secs as i64) * 1000 {
                            continue;
                        }
                        if rt.nudges < limits.max_nudges {
                            rt.nudges += 1;
                            rt.idle_since = Some(now);
                            todo.push(Todo::Remind {
                                agent: agent.id.clone(),
                                task_id: task.id.clone(),
                                title: task.title.clone(),
                            });
                        } else {
                            todo.push(Todo::Unresponsive {
                                agent: agent.id.clone(),
                                why: format!(
                                    "went quiet with task '{}' open and ignored {} reminders",
                                    task.id, limits.max_nudges
                                ),
                            });
                        }
                    }
                    AgentStatus::Working => {
                        let last = agent.last_activity_at.max(rt.last_output_at);
                        if now - last > (limits.unresponsive_secs as i64) * 1000 {
                            todo.push(Todo::Unresponsive {
                                agent: agent.id.clone(),
                                why: format!("no activity for {}s", (now - last) / 1000),
                            });
                        }
                    }
                    // A cache keep-alive's turn leaves the member Waiting
                    // (keepalive.rs), but one that never ends is as stuck as
                    // any other turn, and what is queued for the member waits
                    // behind it.
                    AgentStatus::Waiting if rt.turn_active => {
                        let Some(since) = rt.keepalive_turn_since else {
                            continue;
                        };
                        let last = since.max(agent.last_activity_at).max(rt.last_output_at);
                        if now - last > (limits.unresponsive_secs as i64) * 1000 {
                            todo.push(Todo::Unresponsive {
                                agent: agent.id.clone(),
                                why: format!(
                                    "its cache keep-alive turn shows no activity for {}s",
                                    (now - last) / 1000
                                ),
                            });
                        }
                    }
                    _ => {}
                }
            }
            for task in &tasks {
                // The clock of a task stands still while its agent is parked
                // at a usage limit.
                let parked = task
                    .assigned_agent
                    .as_deref()
                    .is_some_and(|id| agents.iter().any(|a| a.id == id && a.limit.is_some()));
                if let (TaskStatus::Working, Some(secs), Some(started), false) =
                    (task.status, task.timeout_secs, task.started_at, parked)
                {
                    let waiting_ms = task
                        .assigned_agent
                        .as_deref()
                        .and_then(|agent| model.local.get(agent))
                        .and_then(|local| local.task_pauses.get(&task.id))
                        .map_or(0, |pause| pause.total_ms);
                    if now - started - waiting_ms > (secs as i64) * 1000 {
                        todo.push(Todo::Timeout {
                            task_id: task.id.clone(),
                            secs,
                        });
                    }
                }
            }
        }

        for item in todo {
            match item {
                Todo::Remind {
                    agent,
                    task_id,
                    title,
                } => {
                    let text = format!(
                        "[crew] Reminder: task \"{title}\" (id {task_id}) is still open. Continue \
working on it; call task_complete when it is done, or agent_status(\"waiting\", …) if you are \
waiting on a teammate."
                    );
                    {
                        if self.inject(&agent, &text).await {
                            self.emit(
                                NewEvent::new(
                                    "scheduler",
                                    "agent.reminded",
                                    format!("reminded {agent} about task {task_id}"),
                                )
                                .agent(&agent)
                                .task(&task_id),
                            );
                        }
                    }
                }
                Todo::Unresponsive { agent, why } => self.stalled(&agent, why).await,
                Todo::Timeout { task_id, secs } => {
                    self.fail_task(&task_id, format!("timed out after {secs}s"))
                        .await;
                }
            }
        }
    }
}

#[cfg(test)]
mod obligation_clock_tests {
    use super::*;
    use crew_core::{Agent, Task};
    use serde_json::json;

    async fn fixture(
        status: TaskStatus,
    ) -> (
        tempfile::TempDir,
        crate::daemon::Running,
        tokio::sync::mpsc::Receiver<crew_core::rpc::ServerFrame>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let running = crate::daemon::start(crate::daemon::DaemonOptions {
            workspace: temp.path().into(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        let agent: Agent = serde_json::from_value(json!({
            "id":"owner", "name":"owner", "role":"worker", "status":"idle",
            "runtime":"claude", "workdir":temp.path(), "backend":"pty", "last_activity_at":0
        }))
        .unwrap();
        let task: Task = serde_json::from_value(json!({
            "id":"card", "title":"Card", "assigned_agent":"owner", "status":status,
            "created_at":0, "started_at":1
        }))
        .unwrap();
        daemon.model().agents.push(agent);
        daemon.model().tasks.push(task);
        daemon.model().rt("owner").ready = true;
        daemon.model().rt("owner").idle_since = Some(0);
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        daemon
            .listeners
            .lock()
            .unwrap()
            .insert("owner".into(), crate::daemon::Listener { conn: 1, tx });
        (temp, running, rx)
    }

    #[tokio::test(start_paused = true)]
    async fn abandoned_work_keeps_the_ninety_second_three_attempt_reminder_policy() {
        let (_temp, running, mut receiver) = fixture(TaskStatus::Working).await;
        let daemon = &running.daemon;
        let limits = crew_core::config::Limits::default();
        assert_eq!((limits.idle_nudge_secs, limits.max_nudges), (90, 3));
        for attempt in 1..=3 {
            let due = attempt * 90_000;
            daemon.task_health_at(due - 1, &limits).await;
            assert!(receiver.try_recv().is_err());
            daemon.task_health_at(due, &limits).await;
            let frame = serde_json::to_string(&receiver.try_recv().unwrap()).unwrap();
            assert!(frame.contains("Reminder: task"));
            assert!(frame.contains("card"));
            daemon.task_health_at(due, &limits).await;
            assert!(receiver.try_recv().is_err());
        }
        daemon.task_health_at(360_000, &limits).await;
        assert!(receiver.try_recv().is_err());
        let owner = daemon.agent("owner").unwrap();
        assert_eq!(owner.status, AgentStatus::Blocked);
        assert_eq!(owner.block_kind.as_deref(), Some("unresponsive"));
        assert_eq!(
            daemon.model().task("card").unwrap().status,
            TaskStatus::Working
        );
    }

    #[tokio::test(start_paused = true)]
    async fn parked_and_review_cards_never_earn_idle_reminders() {
        for status in [TaskStatus::Waiting, TaskStatus::Review] {
            let (_temp, running, mut receiver) = fixture(status).await;
            let daemon = &running.daemon;
            for now in [89_999, 90_000, 180_000, 270_000, 360_000] {
                daemon
                    .task_health_at(now, &crew_core::config::Limits::default())
                    .await;
                assert!(receiver.try_recv().is_err());
                assert_eq!(daemon.model().task("card").unwrap().status, status);
            }
            assert_eq!(daemon.model().rt("owner").nudges, 0);
        }
    }
}
