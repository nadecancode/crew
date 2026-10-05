//! Operator actions on an agent: kill, restart (resumed or fresh) and pause.

use anyhow::Result;
use crew_core::rpc::RpcError;
use crew_core::{AgentStatus, TaskStatus};

use super::Start;
use crate::daemon::handlers::internal;
use crate::daemon::Daemon;
use crate::persistence::NewEvent;
use crate::tasks::active_task;

impl Daemon {
    pub(crate) async fn kill_agent(&self, id: &str) -> Result<(), RpcError> {
        if self.model().agent(id).is_none() {
            return Err(RpcError::not_found(format!("no agent '{id}'")));
        }
        // A start in progress gives up at its next step.
        let mine = self.stop_requested(id);
        let procs = self.procs()?;
        {
            let mut model = self.model();
            let rt = model.rt(id);
            rt.expected_exit = rt.other_build || procs.is_alive(id);
            rt.ready = false;
            rt.turn_active = false;
            rt.pending_prompt = None;
        }
        self.kill_process(procs.as_ref(), id)
            .await
            .map_err(internal)?;
        // A restart asked for while this waited has taken over: what it
        // started (a sidecar, a session) and set is not this kill's.
        if self.stops(id) != mine {
            return Ok(());
        }
        self.stop_sidecar(id);
        self.update_agent(id, |a| {
            a.status = AgentStatus::Stopped;
            a.pid = None;
            a.paused = false;
            a.block_kind = None;
            a.activity = "stopped".into();
        });
        let task = {
            let model = self.model();
            active_task(&model.tasks, id).map(|t| t.id.clone())
        };
        if let Some(task) = task {
            self.update_task(&task, |t| {
                t.status = TaskStatus::Blocked;
                t.note = Some(format!("agent {id} was stopped"));
            });
        }
        self.emit(NewEvent::new("daemon", "agent.killed", format!("{id} stopped")).agent(id));
        self.replan();
        Ok(())
    }

    pub(crate) async fn restart_agent(&self, id: &str) -> Result<(), RpcError> {
        self.restart_agent_with(id, None).await
    }

    /// Restart an agent in a new session, never resuming the old one: its
    /// session id and confirmation are dropped first, so nothing (this
    /// start, a later restart) goes back to it.
    pub(crate) async fn restart_agent_fresh(&self, id: &str) -> Result<(), RpcError> {
        if self.model().agent(id).is_none() {
            return Err(RpcError::not_found(format!("no agent '{id}'")));
        }
        {
            let mut model = self.model();
            let local = model.local.entry(id.to_string()).or_default();
            local.session_confirmed = false;
        }
        self.save_local(id);
        self.update_agent(id, |a| a.session_id = None);
        self.restart(id, None, Start::Fresh).await
    }

    /// Restart an agent, resuming its session; `notice` replaces the usual
    /// "you were restarted" line of its first prompt.
    pub(crate) async fn restart_agent_with(
        &self,
        id: &str,
        notice: Option<String>,
    ) -> Result<(), RpcError> {
        self.restart(id, notice, Start::Resume).await
    }

    async fn restart(
        &self,
        id: &str,
        notice: Option<String>,
        start: Start,
    ) -> Result<(), RpcError> {
        if self.model().agent(id).is_none() {
            return Err(RpcError::not_found(format!("no agent '{id}'")));
        }
        self.limit_lifted(id, "restarted");
        // A start still on its way (an earlier restart's, a crash restart's)
        // gives up: this restart takes over from it.
        let since = self.stop_requested(id);
        // A stop (a kill, a later restart) asked for while this one waits
        // takes over: this one changes nothing more.
        let overtaken = || self.stops(id) != since;
        let procs = self.procs()?;
        // A process of another build recovery could not stop goes first,
        // whatever its backend says of it.
        let held = self.model().rt(id).other_build;
        if held || procs.is_alive(id) {
            self.model().rt(id).expected_exit = true;
            self.kill_process(procs.as_ref(), id)
                .await
                .map_err(internal)?;
            if overtaken() {
                return Ok(());
            }
        }
        // Tasks that were parked because the agent died or was stopped go
        // back to the scheduler.
        let parked: Vec<String> = {
            let model = self.model();
            model
                .tasks
                .iter()
                .filter(|t| {
                    t.assigned_agent.as_deref() == Some(id)
                        && matches!(t.status, TaskStatus::Failed | TaskStatus::Blocked)
                        && t.note.as_deref().is_some_and(|n| n.starts_with("agent "))
                })
                .map(|t| t.id.clone())
                .collect()
        };
        for task in parked {
            self.update_task(&task, |t| {
                t.status = TaskStatus::Queued;
                t.started_at = None;
                t.completed_at = None;
                t.note = None;
            });
        }
        if start == Start::Fresh {
            // Its session is left whatever the old one reported last (its
            // stop hook while it was killed may have confirmed it again).
            self.requeue_forgotten_work(id).await;
        } else {
            self.requeue_if_sessionless(id).await;
        }
        if overtaken() {
            return Ok(());
        }
        self.update_agent(id, |a| {
            a.status = AgentStatus::Spawning;
            a.error = None;
            a.pid = None;
            a.activity = "restarting".into();
        });
        self.emit(NewEvent::new("daemon", "agent.restart", format!("restarting {id}")).agent(id));
        let daemon = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            if let Err(e) = daemon.spawn_agent(&id, start, notice, since).await {
                daemon
                    .mark_failed(&id, format!("failed to start: {e:#}"))
                    .await;
            }
            daemon.replan();
        });
        Ok(())
    }

    pub(crate) async fn pause_agent(&self, id: &str, pause: bool) -> Result<(), RpcError> {
        if self.model().agent(id).is_none() {
            return Err(RpcError::not_found(format!("no agent '{id}'")));
        }
        let procs = self.procs()?;
        if !procs.is_alive(id) {
            return Err(RpcError::conflict(format!("{id} is not running")));
        }
        if pause {
            procs.pause(id).await.map_err(internal)?;
        } else {
            procs.resume(id).await.map_err(internal)?;
        }
        self.update_agent(id, |a| a.paused = pause);
        self.emit(
            NewEvent::new(
                "daemon",
                if pause {
                    "agent.paused"
                } else {
                    "agent.resumed"
                },
                format!("{id} {}", if pause { "paused" } else { "resumed" }),
            )
            .agent(id),
        );
        if !pause {
            self.request_delivery(id);
        }
        self.replan();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::LaunchSpec;
    use crate::process::ProcessManager;
    use crate::process::Spawned;
    use crew_core::config::TeamConfig;
    use crew_core::Agent;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;

    /// Processes whose first kill waits until it is released (rev-core's
    /// fixture): a restart can be held inside its kill of the old session.
    struct FirstKillWaits {
        alive: AtomicBool,
        kills: AtomicUsize,
        entered: Notify,
        release: Notify,
    }

    #[async_trait::async_trait]
    impl ProcessManager for FirstKillWaits {
        fn name(&self) -> &'static str {
            "pty"
        }
        async fn spawn(&self, _: &LaunchSpec) -> Result<Spawned> {
            anyhow::bail!("a restart a stop overtook must not start anything")
        }
        async fn submit_prompt(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn write_raw(&self, _: &str, _: &[u8]) -> Result<()> {
            Ok(())
        }
        async fn screen(&self, _: &str, _: usize) -> Result<String> {
            Ok(String::new())
        }
        async fn pause(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn resume(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn kill(&self, _: &str) -> Result<()> {
            if self.kills.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.alive.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn is_alive(&self, _: &str) -> bool {
            self.alive.load(Ordering::SeqCst)
        }
        async fn adopt(&self, _: &crew_core::Agent) -> Result<bool> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn a_restart_a_kill_overtook_while_it_stopped_the_old_session_changes_nothing() {
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
            "id": "a", "name": "a", "role": "work", "status": "working", "runtime": "shell",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        daemon.model().agents.push(agent);
        let task: crew_core::Task = serde_json::from_value(serde_json::json!({
            "id": "t", "title": "t", "description": "", "status": "working",
            "assigned_agent": "a", "created_at": 0
        }))
        .unwrap();
        daemon.model().tasks.push(task);
        let procs = Arc::new(FirstKillWaits {
            alive: AtomicBool::new(true),
            kills: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Notify::new(),
        });
        *daemon.procs.write().unwrap() = Some(procs.clone());

        // A restart waits for the old session to go; meanwhile the operator
        // stops the agent, which blocks its task.
        let restart = tokio::spawn({
            let daemon = daemon.clone();
            async move { daemon.restart_agent("a").await }
        });
        procs.entered.notified().await;
        daemon.kill_agent("a").await.unwrap();
        procs.release.notify_one();
        restart.await.unwrap().unwrap();
        tokio::task::yield_now().await;

        let status = daemon.agent("a").unwrap().status;
        let task = daemon.model().tasks[0].clone();
        assert_eq!(
            status,
            AgentStatus::Stopped,
            "the restart the kill overtook set the agent going again"
        );
        assert_eq!(
            (task.status, task.note.as_deref()),
            (TaskStatus::Blocked, Some("agent a was stopped")),
            "the restart the kill overtook reopened the task the kill blocked"
        );
    }

    #[tokio::test]
    async fn a_kill_a_restart_overtook_while_it_stopped_the_session_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::super::start(super::super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = running.daemon.clone();
        let mut team = TeamConfig::from_yaml(
            "project: overtaken\nobjective: overtaken\nagents:\n  - id: a\n    role: work\n",
        )
        .unwrap();
        team.workspace.sandbox.mode = crew_core::config::SandboxMode::Off;
        daemon.model().session.team = Some(team);
        let agent: Agent = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "role": "work", "status": "working", "runtime": "shell",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        daemon.model().agents.push(agent);
        let procs = Arc::new(FirstKillWaits {
            alive: AtomicBool::new(true),
            kills: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Notify::new(),
        });
        *daemon.procs.write().unwrap() = Some(procs.clone());

        // The operator stops the agent; while the kill waits for its session
        // to go, the operator restarts it, and that restart's start runs (and
        // fails: these processes cannot be started).
        let kill = tokio::spawn({
            let daemon = daemon.clone();
            async move { daemon.kill_agent("a").await }
        });
        procs.entered.notified().await;
        daemon.restart_agent("a").await.unwrap();
        for _ in 0..500 {
            if daemon.agent("a").unwrap().status == AgentStatus::Failed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let after_restart = daemon.agent("a").unwrap();
        procs.release.notify_one();
        kill.await.unwrap().unwrap();

        let after_kill = daemon.agent("a").unwrap();
        assert_eq!(
            after_restart.status,
            AgentStatus::Failed,
            "{:?}",
            after_restart.error
        );
        assert_eq!(
            (after_kill.status, after_kill.error),
            (after_restart.status, after_restart.error),
            "the kill the restart overtook overwrote what the restart did"
        );
    }
}
