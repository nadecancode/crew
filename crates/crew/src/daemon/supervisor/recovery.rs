//! Recovery after a daemon restart: agents still running re-adopted, the rest restarted.

use std::sync::Arc;

use crew_core::{Agent, AgentStatus, TaskStatus};

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
        self.recover_agents(procs).await
    }

    /// Re-adopt each agent still running that was launched with this
    /// daemon's copy of crew; restart the others that are needed. Returns
    /// whether every agent was decided about: not when one of another build
    /// could not be stopped and runs on.
    pub(crate) async fn recover_agents(&self, procs: Arc<dyn ProcessManager>) -> bool {
        let (agents, task_count) = {
            let model = self.model();
            (model.agents.clone(), model.tasks.len())
        };
        let mut adopted = 0;
        let mut restarted = 0;
        let mut held = 0;
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
                if self.binary.launched_this(launched_with.as_deref()) {
                    adopted += 1;
                    self.model().rt(&agent.id).ready = true;
                    self.request_delivery(&agent.id);
                    continue;
                }
                // Its crew tools are another build's, which would speak to
                // this daemon in another protocol, or its copy was missing
                // or changed before this start made it again: it is stopped,
                // and goes on as an agent that did not survive.
                let what = match launched_with.as_deref() {
                    Some(name) if Some(name) == self.binary.name() => format!(
                        "{} was launched with {name}, which was not found intact when this daemon \
started; it is stopped, not adopted",
                        agent.id
                    ),
                    _ => format!(
                        "{} runs another build of crew ({}); it is stopped, not adopted",
                        agent.id,
                        launched_with.as_deref().unwrap_or("none recorded")
                    ),
                };
                self.emit(
                    NewEvent::new("daemon", "agent.other_build", what)
                        .agent(&agent.id)
                        .warn(),
                );
                // Held from here: what its backend reports while it is being
                // stopped is not its end.
                self.model().rt(&agent.id).other_build = true;
                if let Some(Err(e)) = self.stop_other_build(procs.as_ref(), &agent.id, true).await {
                    // It may still run, with the copy it was launched with:
                    // it is left as it is until a stop of it works.
                    held += 1;
                    self.hold_other_build(&agent.id, &e);
                    continue;
                }
            }
            // The process did not outlive the previous daemon.
            if self.resume_or_stop(agent).await {
                restarted += 1;
            }
        }
        if held > 0 {
            self.binary.defer_sweep();
        }
        let unstopped = if held > 0 {
            format!(", {held} of another build could not be stopped")
        } else {
            String::new()
        };
        self.emit(NewEvent::new(
            "daemon",
            "daemon.recovered",
            format!(
                "recovered {} agents and {task_count} tasks from the database ({adopted} still running, {restarted} restarted{unstopped})",
                agents.len()
            ),
        ));
        self.replan();
        held == 0
    }

    /// Agent `id`'s process of another build could not be stopped: its
    /// record (pid, backend reference) stays, and nothing restarts it until
    /// a stop of it works (stop_other_builds, or an operator's stop).
    fn hold_other_build(&self, id: &str, error: &anyhow::Error) {
        self.model().rt(id).other_build = true;
        self.update_agent(id, |a| {
            a.activity = "runs another build of crew, which could not be stopped".into();
        });
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.stop_failed",
                format!(
                    "{id} could not be stopped ({error:#}): it is left running another build of \
crew, and restarted once a stop of it works"
                ),
            )
            .agent(id)
            .warn(),
        );
    }

    /// An agent whose process did not outlive the previous daemon, or was
    /// stopped for running another build: restarted if it is needed,
    /// stopped otherwise. Returns whether it was restarted.
    async fn resume_or_stop(&self, agent: &Agent) -> bool {
        let needed = {
            let model = self.model();
            // Its own work, or a review it still owes.
            let open = model.tasks.iter().any(|t| {
                (t.assigned_agent.as_deref() == Some(agent.id.as_str()) && !t.status.is_terminal())
                    || (t.status == TaskStatus::Review
                        && t.reviewer.as_deref() == Some(agent.id.as_str()))
            });
            open || (agent.orchestrator && model.session.summary.is_none())
        } || super::super::delivery::wakes(
            agent,
            &self.bus.undelivered(&agent.id).unwrap_or_default(),
            true,
        );
        if !needed {
            self.update_agent(&agent.id, |a| {
                if a.status != AgentStatus::Done {
                    a.status = AgentStatus::Stopped;
                }
                a.pid = None;
            });
            return false;
        }
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
                "[crew] The crew daemon was restarted and your session was resumed. Continue your \
current task where you left off, and call agent_inbox to check for messages you may have missed."
                    .to_string(),
            ),
        );
        true
    }

    /// The agents recovery left running another build's copy: each that
    /// can be stopped now is, and goes on as an agent that did not survive
    /// the previous daemon. What a backend reports of such a process (an
    /// exit, a lost connection) is not taken for its end: a stop that
    /// worked is. Returns whether none is left.
    pub(crate) async fn stop_other_builds(&self) -> bool {
        let held: Vec<String> = {
            let model = self.model();
            model
                .rt
                .iter()
                .filter(|(_, rt)| rt.other_build)
                .map(|(id, _)| id.clone())
                .collect()
        };
        if held.is_empty() {
            return true;
        }
        let Ok(procs) = self.procs() else {
            return false;
        };
        for id in held {
            let since = self.stops(&id);
            match self.stop_other_build(procs.as_ref(), &id, false).await {
                Some(Ok(())) => {}
                Some(Err(e)) => {
                    tracing::debug!("stopping {id}, of another build, again: {e:#}");
                    continue;
                }
                // Another stop of it is under way, or has worked: it
                // decides what follows.
                None => continue,
            }
            // A stop or restart asked for meanwhile takes it from here.
            if self.stops(&id) != since {
                continue;
            }
            if let Some(agent) = self.agent(&id) {
                self.resume_or_stop(&agent).await;
                self.replan();
            }
        }
        self.model().rt.values().all(|rt| !rt.other_build)
    }

    /// Stop agent `id`'s process for an operator's stop or restart, or a
    /// retirement. A process recovery left running another build's copy
    /// is stopped one stop at a time, and stays held until one worked: a
    /// stop that finds another worked before it has nothing left to stop.
    pub(crate) async fn kill_process(
        &self,
        procs: &dyn ProcessManager,
        id: &str,
    ) -> anyhow::Result<()> {
        if !self.model().rt(id).other_build {
            return procs.kill(id).await;
        }
        self.stop_other_build(procs, id, true)
            .await
            .unwrap_or(Ok(()))
    }

    /// Stop agent `id`'s process of another build, one stop of it at a
    /// time: it stays held while the stop runs, so neither the sweep of the
    /// copy it runs nor what its backend reports of it comes between, and
    /// a stop given up halfway leaves it held. Once a stop worked, its
    /// record goes (an exit reported for the old process now is not this
    /// agent's), then the hold. `None` when it was not tried: no longer
    /// held, or, unless `wait`, another stop of it under way.
    async fn stop_other_build(
        &self,
        procs: &dyn ProcessManager,
        id: &str,
        wait: bool,
    ) -> Option<anyhow::Result<()>> {
        let one = self.model().rt(id).other_build_stop.clone();
        let _one = if wait {
            one.lock_owned().await
        } else {
            one.try_lock_owned().ok()?
        };
        {
            let mut model = self.model();
            let rt = model.rt(id);
            if !rt.other_build {
                return None;
            }
            rt.expected_exit = true;
        }
        if let Err(e) = procs.kill(id).await {
            return Some(Err(e));
        }
        self.update_agent(id, |a| {
            a.pid = None;
            a.backend_ref = None;
        });
        self.model().rt(id).other_build = false;
        Some(Ok(()))
    }
}
