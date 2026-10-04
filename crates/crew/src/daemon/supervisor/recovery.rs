//! Recovery after a daemon restart: agents still running re-adopted, the rest restarted.

use crew_core::{AgentStatus, TaskStatus};

use crate::daemon::Daemon;
use crate::persistence::NewEvent;

impl Daemon {
    /// After a daemon restart: rebuild the process backend from persisted
    /// state, re-adopt agents that are still running, and restart the rest.
    pub(crate) async fn recover(&self) {
        let (session, agents, task_count) = {
            let model = self.model();
            (
                model.session.clone(),
                model.agents.clone(),
                model.tasks.len(),
            )
        };
        let Some(team) = session.team.clone() else {
            return;
        };
        let procs = match self.build_backend(
            &session.backend,
            &team,
            session.herdr_socket.as_deref(),
            session.herdr_workspace.clone(),
        ) {
            Ok(procs) => procs,
            Err(e) => {
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "daemon.recovery_failed",
                        format!("cannot restore the {} backend: {e:#}", session.backend),
                    )
                    .error(),
                );
                return;
            }
        };
        *self.procs.write().unwrap_or_else(|e| e.into_inner()) = Some(procs.clone());

        let mut adopted = 0;
        let mut restarted = 0;
        for agent in &agents {
            if agent.status.is_terminal() {
                continue;
            }
            if procs.adopt(agent).await.unwrap_or(false) {
                adopted += 1;
                self.model().rt(&agent.id).ready = true;
                self.request_delivery(&agent.id);
                continue;
            }
            // The process did not outlive the previous daemon.
            let needed = {
                let model = self.model();
                // Its own work, or a review it still owes.
                let open = model.tasks.iter().any(|t| {
                    (t.assigned_agent.as_deref() == Some(agent.id.as_str())
                        && !t.status.is_terminal())
                        || (t.status == TaskStatus::Review
                            && t.reviewer.as_deref() == Some(agent.id.as_str()))
                });
                open || (agent.orchestrator && model.session.summary.is_none())
            } || super::super::delivery::wakes(
                agent,
                &self.bus.undelivered(&agent.id).unwrap_or_default(),
                true,
            );
            if needed {
                restarted += 1;
                self.requeue_if_sessionless(&agent.id).await;
                self.update_agent(&agent.id, |a| {
                    a.status = AgentStatus::Spawning;
                    a.pid = None;
                    a.activity = "restarting after a daemon restart".into();
                });
                self.spawn_in_background(
                    &agent.id,
                    true,
                    Some(
                        "[crew] The crew daemon was restarted and your session was resumed. \
Continue your current task where you left off, and call agent_inbox to check for messages you \
may have missed."
                            .to_string(),
                    ),
                );
            } else {
                self.update_agent(&agent.id, |a| {
                    if a.status != AgentStatus::Done {
                        a.status = AgentStatus::Stopped;
                    }
                    a.pid = None;
                });
            }
        }
        self.emit(NewEvent::new(
            "daemon",
            "daemon.recovered",
            format!(
                "recovered {} agents and {task_count} tasks from the database ({adopted} still running, {restarted} restarted)",
                agents.len()
            ),
        ));
        self.replan();
    }
}
