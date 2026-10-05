//! Process events: output, an exit (restarted after a crash, or failed), the work it forgot.

use crew_core::util::now_ms;
use crew_core::{AgentStatus, RuntimeKind, TaskStatus, OPERATOR_ID};
use serde_json::Value;

use super::Start;
use crate::agents::Signal;
use crate::daemon::Daemon;
use crate::persistence::NewEvent;
use crate::process::{Hint, ProcEvent};
use crate::tasks::active_task;

impl Daemon {
    pub(crate) async fn on_proc_event(&self, event: ProcEvent) {
        match event {
            ProcEvent::Output { agent_id } => {
                let now = now_ms();
                let (starting, push) = {
                    let mut model = self.model();
                    let Some(agent) = model.agent(&agent_id).cloned() else {
                        return;
                    };
                    let rt = model.rt(&agent_id);
                    rt.last_output_at = now;
                    let push = now - rt.last_activity_push > 5_000;
                    if push {
                        rt.last_activity_push = now;
                    }
                    (
                        !rt.ready || agent.block_kind.as_deref() == Some("dialog"),
                        push || agent.block_kind.as_deref() == Some("unresponsive"),
                    )
                };
                if push {
                    self.update_agent(&agent_id, |a| {
                        a.last_activity_at = now;
                        if a.block_kind.as_deref() == Some("unresponsive") {
                            a.status = AgentStatus::Working;
                            a.block_kind = None;
                        }
                    });
                }
                if starting {
                    // Output ticks are throttled, so the tick that completes a
                    // dialog may never arrive: look again once the screen has
                    // had time to settle.
                    let schedule = {
                        let mut model = self.model();
                        let rt = model.rt(&agent_id);
                        !std::mem::replace(&mut rt.dialog_check_pending, true)
                    };
                    if schedule {
                        let daemon = self.clone();
                        let id = agent_id.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(900)).await;
                            daemon.model().rt(&id).dialog_check_pending = false;
                            daemon.check_startup_dialog(&id).await;
                        });
                    }
                }
            }
            ProcEvent::Hint { agent_id, hint } => {
                let Some(agent) = self.agent(&agent_id) else {
                    return;
                };
                let (ready, turn_active) = {
                    let mut model = self.model();
                    let rt = model.rt(&agent_id);
                    (rt.ready, rt.turn_active)
                };
                if !ready {
                    // Still starting: the start-up watcher decides when the
                    // session can take input (a dialog may be in the way).
                    return;
                }
                // Claude reports its own turns through hooks; the backend's
                // reading is only needed for runtimes without them.
                let hooks = agent.runtime == RuntimeKind::Claude;
                match hint {
                    Hint::Blocked => {
                        // A menu that needs no answer is not a block: it goes.
                        let screen = match self.procs() {
                            Ok(procs) => procs.visible(&agent_id, 60).await.unwrap_or_default(),
                            Err(_) => String::new(),
                        };
                        if !self.dismiss_notice(&agent_id, &screen).await {
                            self.on_signal(
                                &agent_id,
                                Signal::Blocked("waiting for an answer in its terminal".into()),
                            )
                            .await
                        }
                    }
                    Hint::Working if !hooks => self.on_signal(&agent_id, Signal::TurnStarted).await,
                    Hint::Working => self.on_signal(&agent_id, Signal::Unblocked).await,
                    Hint::Idle if !hooks && turn_active => {
                        self.on_signal(&agent_id, Signal::TurnEnded).await
                    }
                    Hint::Idle => {}
                }
            }
            ProcEvent::Exited {
                agent_id,
                instance,
                code,
                detail,
            } => {
                // Ignore the exit of a process that has since been replaced
                // (or that an explicit stop already accounted for).
                let current = self.agent(&agent_id).and_then(|a| {
                    if a.backend == "herdr" {
                        a.backend_ref
                    } else {
                        a.pid.map(|p| p.to_string())
                    }
                });
                if current.as_deref() == Some(instance.as_str()) {
                    self.on_exit(&agent_id, code, &detail).await;
                }
            }
        }
    }

    async fn on_exit(&self, id: &str, code: Option<i32>, detail: &str) {
        let (expected, agent, max_restarts, has_task) = {
            let mut model = self.model();
            let Some(agent) = model.agent(id).cloned() else {
                return;
            };
            let has_task = active_task(&model.tasks, id).is_some();
            let max_restarts = model
                .local
                .get(id)
                .map(|l| l.settings.max_restarts)
                .unwrap_or(0);
            let rt = model.rt(id);
            // A process of another build recovery could not stop ends when
            // a stop of it works (stop_other_builds), not when its backend
            // reports an exit or loses sight of it.
            if rt.other_build {
                return;
            }
            rt.ready = false;
            rt.turn_active = false;
            rt.pending_prompt = None;
            (
                std::mem::take(&mut rt.expected_exit),
                agent,
                max_restarts,
                has_task,
            )
        };
        // The session is gone, so its helper is not needed either (a restart
        // starts a fresh one).
        self.stop_sidecar(id);
        if expected || agent.status == AgentStatus::Stopped {
            self.update_agent(id, |a| {
                // A retired subagent stays "done"; everything else is "stopped".
                if a.status != AgentStatus::Done {
                    a.status = AgentStatus::Stopped;
                }
                a.pid = None;
                a.exit_code = code;
                a.subagents = 0;
            });
            return;
        }
        if code == Some(0) && !has_task {
            // A clean exit with nothing left to do is not a failure.
            self.update_agent(id, |a| {
                if a.status != AgentStatus::Done {
                    a.status = AgentStatus::Stopped;
                }
                a.pid = None;
                a.exit_code = code;
                a.activity = "exited".into();
            });
            self.emit(NewEvent::new("daemon", "agent.exited", format!("{id} exited")).agent(id));
            self.replan();
            return;
        }
        self.emit(
            NewEvent::new("daemon", "agent.crashed", format!("{id} {detail}"))
                .agent(id)
                .error(),
        );
        if agent.restarts < max_restarts {
            let attempt = agent.restarts + 1;
            self.update_agent(id, |a| {
                a.restarts = attempt;
                a.status = AgentStatus::Spawning;
                a.pid = None;
                a.exit_code = code;
                a.activity = "restarting after a crash".into();
            });
            self.emit(
                NewEvent::new(
                    "daemon",
                    "agent.restarting",
                    format!("restarting {id} (attempt {attempt}/{max_restarts})"),
                )
                .agent(id)
                .warn(),
            );
            self.requeue_if_sessionless(id).await;
            let daemon = self.clone();
            let since = self.stops(id);
            let id = id.to_string();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Err(e) = daemon.spawn_agent(&id, Start::Resume, None, since).await {
                    daemon
                        .mark_failed(&id, format!("restart failed: {e:#}"))
                        .await;
                }
                daemon.replan();
            });
        } else {
            self.update_agent(id, |a| a.exit_code = code);
            self.mark_failed(id, detail.to_string()).await;
        }
    }

    /// A restarted agent that cannot resume its session has forgotten its
    /// task and the reviews it was asked for (requeue_forgotten_work).
    pub(super) async fn requeue_if_sessionless(&self, id: &str) {
        let confirmed = self
            .model()
            .local
            .get(id)
            .is_some_and(|l| l.session_confirmed);
        if !confirmed {
            self.requeue_forgotten_work(id).await;
        }
    }

    /// Agent `id` starts in a session that knows nothing of its work: its
    /// task goes back to the scheduler to be handed out again, and each
    /// review it was asked for is asked for again.
    pub(super) async fn requeue_forgotten_work(&self, id: &str) {
        let (tasks, reviews) = {
            let model = self.model();
            (
                model
                    .tasks
                    .iter()
                    .filter(|t| {
                        t.assigned_agent.as_deref() == Some(id)
                            && (crate::tasks::occupies_agent(t) || t.status == TaskStatus::Waiting)
                    })
                    .map(|t| t.id.clone())
                    .collect::<Vec<_>>(),
                model
                    .tasks
                    .iter()
                    .filter(|t| t.status == TaskStatus::Review && t.reviewer.as_deref() == Some(id))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        for task in tasks {
            self.update_task(&task, |t| {
                t.status = TaskStatus::Ready;
                t.started_at = None;
                t.note = Some("re-dispatched after the agent restarted".into());
            });
            self.clear_task_wakeups(id, &task);
        }
        self.project_task(id);
        // A request still waiting to be delivered reaches the new session.
        let waiting = self.bus.undelivered(id).unwrap_or_default();
        for task in reviews {
            if waiting.iter().any(|m| {
                m.kind == crew_core::MessageType::ReviewRequest
                    && m.metadata.get("task_id").and_then(Value::as_str) == Some(task.id.as_str())
            }) {
                continue;
            }
            // Queued now, delivered once the new session is up.
            let author = task
                .assigned_agent
                .clone()
                .unwrap_or_else(|| OPERATOR_ID.into());
            self.send_review_request(&task, &author, id).await;
        }
    }

    pub(crate) async fn mark_failed(&self, id: &str, error: String) {
        self.update_agent(id, |a| {
            a.status = AgentStatus::Failed;
            a.error = Some(error.clone());
            a.pid = None;
            a.activity = "failed".into();
        });
        let task = {
            let model = self.model();
            active_task(&model.tasks, id).map(|t| t.id.clone())
        };
        if let Some(task) = task {
            self.fail_task(&task, format!("agent {id} failed: {error}"))
                .await;
        } else {
            self.notify_orchestrator(&format!("Agent {id} failed: {error}"))
                .await;
        }
        self.replan();
    }
}
