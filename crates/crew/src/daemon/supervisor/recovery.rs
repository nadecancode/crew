//! Recovery after a daemon restart: agents still running re-adopted, the rest restarted.

use std::sync::Arc;

use crew_core::{AgentStatus, TaskStatus};

use crate::daemon::Daemon;
use crate::persistence::NewEvent;
use crate::process::ProcessManager;

impl Daemon {
    /// After a daemon restart: rebuild the process backend from persisted
    /// state, re-adopt agents that are still running, and restart the rest.
    /// Returns whether every agent was decided about, so that none still
    /// runs another build's copy of crew (daemon::binary).
    pub(crate) async fn recover(&self) -> bool {
        let (session, agents) = {
            let model = self.model();
            (model.session.clone(), model.agents.clone())
        };
        let Some(team) = session.team.clone() else {
            return true;
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
                // Agents that outlived the previous daemon may still run.
                return agents.iter().all(|a| a.status.is_terminal());
            }
        };
        *self.procs.write().unwrap_or_else(|e| e.into_inner()) = Some(procs.clone());
        self.recover_agents(procs).await;
        true
    }

    /// Re-adopt each agent still running that was launched with this
    /// daemon's copy of crew; restart the others that are needed.
    pub(crate) async fn recover_agents(&self, procs: Arc<dyn ProcessManager>) {
        let (agents, task_count) = {
            let model = self.model();
            (model.agents.clone(), model.tasks.len())
        };
        let mut adopted = 0;
        let mut restarted = 0;
        for agent in &agents {
            if agent.status.is_terminal() {
                continue;
            }
            if procs.adopt(agent).await.unwrap_or(false) {
                let launched_with = self
                    .model()
                    .local
                    .get(&agent.id)
                    .and_then(|l| l.binary.clone());
                if launched_with.as_deref() == self.binary.name() {
                    adopted += 1;
                    self.model().rt(&agent.id).ready = true;
                    self.request_delivery(&agent.id);
                    continue;
                }
                // Its crew tools are another build's, which would speak to
                // this daemon in another protocol: it is stopped, and goes
                // on as an agent that did not survive.
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "agent.other_build",
                        format!(
                            "{} runs another build of crew ({}); it is stopped, not adopted",
                            agent.id,
                            launched_with.as_deref().unwrap_or("none recorded")
                        ),
                    )
                    .agent(&agent.id)
                    .warn(),
                );
                self.model().rt(&agent.id).expected_exit = true;
                if let Err(e) = procs.kill(&agent.id).await {
                    tracing::warn!("stopping {}: {e:#}", agent.id);
                }
                // An exit reported for the old process now is not this agent's.
                self.update_agent(&agent.id, |a| {
                    a.pid = None;
                    a.backend_ref = None;
                });
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
