//! Version control operations requested by agents, run by the daemon on
//! their behalf, and the `vcs.*` events every one of them leaves.
//!
//! Doing commits, cherry-picks and merges here (rather than letting agents
//! shell out) keeps a team-visible record, attaches commits to tasks as
//! artifacts, and lets conflicts be aborted and reported uniformly. The
//! operations go through the session's backend (`Daemon::vcs`: jj or git),
//! and each one's events are emitted from the job that runs it, by a
//! [`VcsOp`], so an operation that started always ends in a terminal event,
//! whoever waits for it (notes/jj-design.md §events).

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crew_core::rpc::{CommitInfo, GitOutcome, RpcError};
use crew_core::util::{is_valid_id, now_ms};
use crew_core::vcs::{VcsAction, VcsEvent, VcsOutcome, INTEGRATION_AGENT, VCS_SOURCE};
use crew_core::{Artifact, EventLevel};
use serde_json::{json, Value};

use super::Daemon;
use crate::git;
use crate::persistence::NewEvent;
use crate::tasks::active_task;
use crate::vcs::{IntegrationStep, Kept};

fn git_error(e: impl std::fmt::Display) -> RpcError {
    RpcError::new("git", e.to_string())
}

/// One VCS operation: its events, emitted on the thread that runs it.
///
/// `start` emits `started`; `finish` emits the terminal event. One that is
/// armed (started, or `begin` for an operation without a start) and never
/// finished ends in `error`, summary `interrupted`, when it is dropped: an
/// early return, a panic, a job whose request went away. One that never
/// began (refused before any VCS ran) emits nothing.
pub(crate) struct VcsOp {
    daemon: Daemon,
    event: VcsEvent,
    task: Option<String>,
    armed: bool,
    started: bool,
}

impl VcsOp {
    /// Takes the next `op`.
    pub fn new(daemon: &Daemon, action: VcsAction, agent: &str) -> Self {
        let op = format!("v{}", daemon.vcs_ops.fetch_add(1, Ordering::Relaxed));
        let backend = daemon.vcs().kind().as_str();
        Self {
            daemon: daemon.clone(),
            event: VcsEvent::new(action, VcsOutcome::Started, &op, backend, agent),
            task: None,
            armed: false,
            started: false,
        }
    }

    pub fn peer(mut self, peer: Option<&str>) -> Self {
        self.event.peer = peer.map(str::to_string);
        self
    }

    pub fn source(mut self, source: &str) -> Self {
        self.event.source = Some(source.to_string());
        self
    }

    pub fn branch(mut self, branch: &str) -> Self {
        self.event.branch = Some(branch.to_string());
        self
    }

    /// The task the operation's events belong to.
    pub fn task(mut self, task: Option<String>) -> Self {
        self.task = task;
        self
    }

    pub fn started(&self) -> bool {
        self.started
    }

    /// Emit `started`.
    pub fn start(&mut self) {
        // What an earlier operation on this thread left unsaid is not this
        // one's.
        crate::vcs::take_warnings();
        self.armed = true;
        self.started = true;
        let event = self.event.clone();
        self.emit(&event);
    }

    /// The operation began to run; it has no `started` event of its own.
    pub fn begin(&mut self) {
        crate::vcs::take_warnings();
        self.armed = true;
    }

    /// Emit the terminal event; `fill` adds what the operation found out.
    pub fn finish(mut self, outcome: VcsOutcome, fill: impl FnOnce(&mut VcsEvent)) {
        self.armed = false;
        let mut event = self.event.clone();
        event.outcome = outcome;
        fill(&mut event);
        self.emit(&event);
        for warning in crate::vcs::take_warnings() {
            let mut warn = NewEvent::new(VCS_SOURCE, "vcs.warning", warning).warn();
            if event.agent != INTEGRATION_AGENT {
                warn = warn.agent(&event.agent);
            }
            self.daemon.emit(warn);
        }
    }

    /// The terminal event of an operation that failed with `e`.
    pub fn fail(self, e: &anyhow::Error) {
        let kept = e
            .chain()
            .find_map(|c| c.downcast_ref::<Kept>())
            .map(|k| k.commit.clone());
        let summary = format!("{e:#}");
        self.finish(VcsOutcome::Error, |event| {
            if kept.is_some() {
                event.commit = kept;
            }
            event.summary = Some(summary);
        });
    }

    /// Nothing happened that deserves an event (a workspace that was
    /// reused): it ends without one.
    pub fn disarm(mut self) {
        self.armed = false;
    }

    fn emit(&self, event: &VcsEvent) {
        let level = match event.outcome {
            VcsOutcome::Started => EventLevel::Debug,
            VcsOutcome::Ok if event.action.is_read() => EventLevel::Debug,
            VcsOutcome::Ok => EventLevel::Info,
            VcsOutcome::Conflict | VcsOutcome::Error => EventLevel::Warn,
        };
        let mut new = NewEvent::new(VCS_SOURCE, &event.event_type(), message(event))
            .level(level)
            .data(serde_json::to_value(event).unwrap_or(Value::Null));
        if event.agent != INTEGRATION_AGENT {
            new = new.agent(&event.agent);
        }
        if let Some(task) = &self.task {
            new = new.task(task);
        }
        self.daemon.emit(new);
    }
}

impl Drop for VcsOp {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let mut event = self.event.clone();
        event.outcome = VcsOutcome::Error;
        event.summary = Some("interrupted".to_string());
        self.emit(&event);
    }
}

/// The line the log shows for a VCS event.
pub(crate) fn message(e: &VcsEvent) -> String {
    let agent = &e.agent;
    let short = e.short().unwrap_or("");
    let source = e.source.as_deref().unwrap_or("");
    let short_source = if source.len() >= 40 && source.bytes().all(|b| b.is_ascii_hexdigit()) {
        &source[..8]
    } else {
        source
    };
    let branch = e.branch.as_deref().unwrap_or("");
    let peer = e.peer.as_deref();
    let files = e.files.join(", ");
    let summary = e.summary.as_deref().unwrap_or("");
    match (e.action, e.outcome) {
        (VcsAction::Workspace, VcsOutcome::Started) => {
            format!("creating the workspace of {agent}…")
        }
        (VcsAction::Workspace, VcsOutcome::Ok) => {
            format!("workspace for {agent} on {branch} at {short}")
        }
        (VcsAction::Commit, VcsOutcome::Ok) => {
            format!("{agent} committed {short} on {branch}: {summary}")
        }
        (VcsAction::CherryPick, VcsOutcome::Started) => {
            format!("{agent}: cherry-pick of {short_source}…")
        }
        (VcsAction::CherryPick, VcsOutcome::Ok) => match peer {
            Some(peer) => format!("{agent} cherry-picked {short_source} from {peer} as {short}"),
            None => format!("{agent} cherry-picked {short_source} as {short}"),
        },
        (VcsAction::CherryPick, VcsOutcome::Conflict) => match peer {
            Some(peer) => format!(
                "{agent}: cherry-pick of {short_source} conflicts with {peer} in {files}; nothing \
was changed"
            ),
            None => format!(
                "{agent}: cherry-pick of {short_source} conflicts in {files}; nothing was changed"
            ),
        },
        (VcsAction::Merge, VcsOutcome::Started) => format!("{agent}: merging {source}…"),
        (VcsAction::Merge, VcsOutcome::Ok) => format!("{agent} merged {source} ({short})"),
        (VcsAction::Merge, VcsOutcome::Conflict) => {
            format!("{agent}: merge of {source} conflicts in {files}; nothing was changed")
        }
        (VcsAction::Integrate, VcsOutcome::Started) => {
            format!("integrating into {branch}…")
        }
        (VcsAction::Integrate, VcsOutcome::Ok) if peer.is_some() => {
            format!("integration merged {source} ({short})")
        }
        (VcsAction::Integrate, VcsOutcome::Conflict) if peer.is_some() => {
            format!("integration: merge of {source} conflicts in {files}; skipped")
        }
        (VcsAction::Integrate, VcsOutcome::Ok | VcsOutcome::Conflict) => {
            format!("integration on {branch}: {summary}")
        }
        (VcsAction::Status, VcsOutcome::Ok) => format!("{agent}: status"),
        (VcsAction::Diff, VcsOutcome::Ok) => match &e.source {
            Some(against) => format!("{agent}: diff against {against}"),
            None => format!("{agent}: diff"),
        },
        (action, VcsOutcome::Error) => {
            let what = match action {
                VcsAction::Workspace => "creating the workspace".to_string(),
                VcsAction::CherryPick => "cherry-pick".to_string(),
                other => other.as_str().to_string(),
            };
            format!("{agent}: {what} failed: {summary}")
        }
        (action, outcome) => format!("{agent}: {} {:?}", action.as_str(), outcome),
    }
}

/// The agent an author's address names (`<agent>@crew.local`), if any.
fn agent_of_email(email: &str) -> Option<String> {
    let id = email.strip_suffix("@crew.local")?;
    (is_valid_id(id) && id != "crew").then(|| id.to_string())
}

/// Fill a terminal event with a commit.
fn fill_commit(event: &mut VcsEvent, commit: &CommitInfo) {
    event.commit = Some(commit.sha.clone());
    event.change = commit.change_id.clone();
    event.summary = Some(commit.summary.clone());
}

/// Fill a terminal event from a cherry-pick's or merge's outcome; its
/// outcome.
fn fill_outcome(event: &mut VcsEvent, outcome: &GitOutcome) -> VcsOutcome {
    if let Some(commit) = &outcome.commit {
        fill_commit(event, commit);
    }
    match &outcome.conflict {
        Some(conflict) if !outcome.ok => {
            event.files = conflict.files.clone();
            VcsOutcome::Conflict
        }
        _ => VcsOutcome::Ok,
    }
}

impl Daemon {
    /// The directory an agent's git operations run in.
    fn git_dir(&self, agent: &str) -> Result<PathBuf, RpcError> {
        let dir = self
            .model()
            .agent(agent)
            .map(|a| PathBuf::from(&a.workdir))
            .ok_or_else(|| RpcError::not_found(format!("no agent '{agent}'")))?;
        Ok(dir)
    }

    /// Run a blocking git operation, one at a time per daemon. The job
    /// holds the lock, not this request: one whose client went away runs
    /// on, and the next operation waits until it is done.
    async fn git<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) -> Result<T, RpcError> {
        let guard = self.git_lock.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            f()
        })
        .await
        .map_err(git_error)?
        .map_err(|e| git_error(format!("{e:#}")))
    }

    /// Run a blocking sequence of git operations on `agent`'s behalf, one
    /// at a time per daemon, inside the agent's sandbox when the team
    /// requires one.
    async fn agent_git<T: Send + 'static>(
        &self,
        agent: &str,
        f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) -> Result<T, RpcError> {
        let record = self
            .agent(agent)
            .ok_or_else(|| RpcError::not_found(format!("no agent '{agent}'")))?;
        // Held by the job itself, not by this request: a launch of the agent
        // waits until the job and its sandboxes are done, cancelled or not.
        let lease = self.launch_lease(agent).read_owned().await;
        let launcher = self
            .agent_git_launcher(&record)
            .await
            .map_err(|e| git_error(format!("{e:#}")))?;
        self.git(move || {
            let _lease = lease;
            match launcher {
                Some(launcher) => git::launch::with_launcher(launcher, f),
                None => f(),
            }
        })
        .await
    }

    /// In a shared checkout every agent works in one tree, so a commit,
    /// merge or cherry-pick cannot be attributed to one agent and moves
    /// everyone's HEAD.
    fn require_own_worktree(&self, tool: &str) -> Result<(), RpcError> {
        if self.model().session.isolation == "shared" {
            return Err(git_error(format!(
                "{tool} is not available: the checkout is shared, so a commit cannot be attributed \
to one agent. Report your changed files to the orchestrator instead (task_complete summary or \
agent_send)"
            )));
        }
        Ok(())
    }

    /// An agent sharing the checkout needs it to be a repository. A worktree
    /// crew made is one by construction, and is not probed: whatever git
    /// reads there is the agent's to write, so git runs there only on the
    /// agent's behalf (in its sandbox, when the team requires one).
    async fn ensure_repo(&self, dir: &std::path::Path) -> Result<(), RpcError> {
        if self.model().session.isolation == "worktree" {
            return Ok(());
        }
        let probe = dir.to_path_buf();
        let is_root = tokio::task::spawn_blocking(move || git::is_root(&probe))
            .await
            .unwrap_or(false);
        if is_root {
            Ok(())
        } else {
            Err(git_error(
                "this agent's directory is not the root of a git work tree, so crew will not run \
git in it (use a git repository as the workspace, or workspace.init: true)",
            ))
        }
    }

    pub(crate) async fn git_commit(
        &self,
        agent: &str,
        message: String,
    ) -> Result<CommitInfo, RpcError> {
        self.require_own_worktree("git_commit")?;
        let dir = self.git_dir(agent)?;
        self.ensure_repo(&dir).await?;
        let author = agent.to_string();
        let vcs = self.vcs();
        let task = {
            let model = self.model();
            active_task(&model.tasks, agent).map(|t| t.id.clone())
        };
        let mut op = VcsOp::new(self, VcsAction::Commit, agent)
            .branch(&git::branch_for(agent))
            .task(task.clone());
        let commit = self
            .agent_git(agent, move || {
                op.begin();
                match vcs.commit_all(&dir, &author, &message) {
                    Ok(commit) => {
                        op.finish(VcsOutcome::Ok, |e| {
                            fill_commit(e, &commit);
                            e.branch = Some(commit.branch.clone());
                        });
                        Ok(commit)
                    }
                    Err(e) => {
                        op.fail(&e);
                        Err(e)
                    }
                }
            })
            .await?;
        // Attach the commit to the task the agent is working on.
        if let Some(task) = &task {
            self.update_task(task, |t| {
                t.artifacts.push(Artifact {
                    kind: "commit".into(),
                    value: commit.sha.clone(),
                    description: commit.summary.clone(),
                    agent: Some(agent.to_string()),
                    created_at: now_ms(),
                });
            });
        }
        self.update_agent(agent, |a| a.last_activity_at = now_ms());
        Ok(commit)
    }

    pub(crate) async fn git_diff(
        &self,
        agent: &str,
        against: Option<String>,
    ) -> Result<GitOutcome, RpcError> {
        let dir = self.git_dir(agent)?;
        self.ensure_repo(&dir).await?;
        let vcs = self.vcs();
        let mut op = VcsOp::new(self, VcsAction::Diff, agent);
        if let Some(against) = &against {
            op = op.source(against);
        }
        let text = self
            .agent_git(agent, move || {
                op.begin();
                match vcs.diff(&dir, against.as_deref()) {
                    Ok(text) => {
                        op.finish(VcsOutcome::Ok, |_| {});
                        Ok(text)
                    }
                    Err(e) => {
                        op.fail(&e);
                        Err(e)
                    }
                }
            })
            .await?;
        Ok(GitOutcome {
            ok: true,
            text,
            ..GitOutcome::default()
        })
    }

    pub(crate) async fn git_status(&self, agent: &str) -> Result<GitOutcome, RpcError> {
        let dir = self.git_dir(agent)?;
        self.ensure_repo(&dir).await?;
        let vcs = self.vcs();
        let mut op = VcsOp::new(self, VcsAction::Status, agent);
        let text = self
            .agent_git(agent, move || {
                op.begin();
                match vcs.status(&dir) {
                    Ok(text) => {
                        op.finish(VcsOutcome::Ok, |_| {});
                        Ok(text)
                    }
                    Err(e) => {
                        op.fail(&e);
                        Err(e)
                    }
                }
            })
            .await?;
        Ok(GitOutcome {
            ok: true,
            text,
            ..GitOutcome::default()
        })
    }

    pub(crate) async fn git_cherry_pick(
        &self,
        agent: &str,
        commit: String,
    ) -> Result<GitOutcome, RpcError> {
        self.require_own_worktree("git_cherry_pick")?;
        let dir = self.git_dir(agent)?;
        self.ensure_repo(&dir).await?;
        let author = agent.to_string();
        let vcs = self.vcs();
        let mut op = VcsOp::new(self, VcsAction::CherryPick, agent)
            .source(&commit)
            .branch(&git::branch_for(agent));
        self.agent_git(agent, move || {
            op.start();
            match vcs.cherry_pick(&dir, &author, &commit) {
                Ok(picked) => {
                    let peer = agent_of_email(&picked.author_email);
                    let outcome = picked.outcome;
                    let mut result = VcsOutcome::Ok;
                    op.finish(VcsOutcome::Ok, |e| {
                        e.peer = peer;
                        e.source = Some(picked.source.clone());
                        result = fill_outcome(e, &outcome);
                        e.outcome = result;
                    });
                    Ok(outcome)
                }
                Err(e) => {
                    op.fail(&e);
                    Err(e)
                }
            }
        })
        .await
    }

    pub(crate) async fn git_merge(
        &self,
        agent: &str,
        from_agent: String,
    ) -> Result<GitOutcome, RpcError> {
        self.require_own_worktree("git_merge")?;
        if !is_valid_id(&from_agent) {
            return Err(RpcError::not_found(format!("no agent '{from_agent}'")));
        }
        if from_agent == agent {
            return Err(RpcError::invalid(
                "cannot merge an agent's branch into itself",
            ));
        }
        let (member, worktrees) = {
            let model = self.model();
            (
                model.agent(&from_agent).map(|a| a.branch.clone()),
                model.session.isolation == "worktree",
            )
        };
        let branch = match member {
            Some(branch) => branch.ok_or_else(|| {
                git_error(format!(
                    "{from_agent} has no branch (agents share the workspace)"
                ))
            })?,
            // A member who has left the team leaves its branch behind.
            None => {
                let (vcs, branch) = (self.vcs(), git::branch_for(&from_agent));
                let exists = worktrees && {
                    let branch = branch.clone();
                    self.git(move || Ok(vcs.branch_exists(&branch))).await?
                };
                if !exists {
                    return Err(RpcError::not_found(format!("no agent '{from_agent}'")));
                }
                branch
            }
        };
        let dir = self.git_dir(agent)?;
        self.ensure_repo(&dir).await?;
        let author = agent.to_string();
        let vcs = self.vcs();
        let mut op = VcsOp::new(self, VcsAction::Merge, agent)
            .peer(Some(&from_agent))
            .source(&branch)
            .branch(&git::branch_for(agent));
        self.agent_git(agent, move || {
            op.start();
            match vcs.merge(&dir, &author, &branch) {
                Ok(outcome) => {
                    op.finish(VcsOutcome::Ok, |e| e.outcome = fill_outcome(e, &outcome));
                    Ok(outcome)
                }
                Err(e) => {
                    op.fail(&e);
                    Err(e)
                }
            }
        })
        .await
    }

    /// Merge every agent branch into the integration branch.
    pub(crate) async fn integrate(&self) -> Result<Value, RpcError> {
        let (agents, base, worktrees, sandbox) = {
            let model = self.model();
            let team = model
                .session
                .team
                .as_ref()
                .ok_or_else(|| RpcError::unavailable("no team is loaded"))?;
            let mut agents: Vec<String> = model
                .agents
                .iter()
                .filter(|a| a.branch.is_some())
                .map(|a| a.id.clone())
                .collect();
            // Members who have left keep their branches, and their work is
            // part of the result (branches that do not exist are skipped).
            for task in &model.tasks {
                if let Some(agent) = &task.assigned_agent {
                    if model.agent(agent).is_none() && !agents.contains(agent) {
                        agents.push(agent.clone());
                    }
                }
            }
            (
                agents,
                team.workspace.base.clone(),
                model.session.isolation == "worktree",
                team.workspace.sandbox.clone(),
            )
        };
        if !worktrees {
            return Err(git_error(
                "agents share the workspace, so there are no branches to integrate",
            ));
        }
        let daemon = self.clone();
        let mut run = VcsOp::new(self, VcsAction::Integrate, INTEGRATION_AGENT)
            .branch(git::INTEGRATION_BRANCH);
        let (worktree, steps) = self
            .git(move || {
                run.start();
                let mut head = None;
                let mut on_step = |step: &IntegrationStep| {
                    let mut op = VcsOp::new(&daemon, VcsAction::Integrate, INTEGRATION_AGENT)
                        .peer(Some(&step.agent))
                        .source(&git::branch_for(&step.agent))
                        .branch(git::INTEGRATION_BRANCH);
                    op.begin();
                    if step.outcome.ok {
                        if let Some(commit) = &step.outcome.commit {
                            head = Some(commit.sha.clone());
                        }
                    }
                    op.finish(VcsOutcome::Ok, |e| {
                        e.outcome = fill_outcome(e, &step.outcome)
                    });
                };
                match daemon.integration(&sandbox, &base, &agents, &mut on_step) {
                    Ok((worktree, steps)) => {
                        let merged = steps.iter().filter(|s| s.outcome.ok).count();
                        let conflicts = steps
                            .iter()
                            .filter(|s| s.outcome.conflict.is_some())
                            .count();
                        let outcome = if conflicts > 0 {
                            VcsOutcome::Conflict
                        } else {
                            VcsOutcome::Ok
                        };
                        let head = head.or_else(|| daemon.vcs().branch_commit(&worktree.branch));
                        run.finish(outcome, |e| {
                            e.commit = head;
                            e.summary = Some(format!(
                                "{merged} merged, {conflicts} conflict{}",
                                if conflicts == 1 { "" } else { "s" }
                            ));
                        });
                        Ok((worktree, steps))
                    }
                    Err(e) => {
                        run.fail(&e);
                        Err(e)
                    }
                }
            })
            .await?;
        let mut merged = Vec::new();
        let mut conflicts = Vec::new();
        for step in &steps {
            if step.outcome.ok {
                merged.push(step.agent.clone());
            } else if let Some(conflict) = &step.outcome.conflict {
                conflicts.push(json!({ "agent": step.agent, "files": conflict.files }));
            }
        }
        Ok(json!({
            "branch": worktree.branch,
            "path": worktree.path.to_string_lossy(),
            "merged": merged,
            "conflicts": conflicts,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn daemon_with_agent(temp: &tempfile::TempDir) -> super::super::Running {
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
        })
        .await
        .unwrap();
        let agent: crew_core::Agent = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "role": "work", "status": "working", "runtime": "shell",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        running.daemon.model().agents.push(agent);
        running
    }

    /// The VCS events in the daemon's log, oldest first.
    fn vcs_events(daemon: &Daemon) -> Vec<(VcsEvent, crew_core::Event)> {
        let mut events: Vec<_> = daemon
            .store
            .recent_events(None, 1000)
            .unwrap()
            .into_iter()
            .filter_map(|e| VcsEvent::from_event(&e).map(|v| (v, e)))
            .collect();
        events.sort_by_key(|(_, e)| e.seq);
        events
    }

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..300 {
            if done() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("{what} never happened");
    }

    #[tokio::test]
    async fn an_agents_git_job_keeps_its_launch_lease_after_the_request_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let running = daemon_with_agent(&temp).await;
        let daemon = running.daemon.clone();
        let held = |daemon: &Daemon| daemon.launch_lease("a").try_write().is_err();
        let until_held = |daemon: Daemon, want: bool| async move {
            for _ in 0..200 {
                if held(&daemon) == want {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("the lease was never {}", if want { "held" } else { "free" });
        };

        let (release, job) = std::sync::mpsc::channel::<()>();
        let mut op = VcsOp::new(&daemon, VcsAction::Merge, "a")
            .peer(Some("b"))
            .source("agents/b")
            .branch("agents/a");
        let request = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                daemon
                    .agent_git("a", move || {
                        op.start();
                        let _ = job.recv();
                        op.finish(VcsOutcome::Ok, |e| e.commit = Some("c0ffee".repeat(7)));
                        Ok(())
                    })
                    .await
            })
        };
        until_held(daemon.clone(), true).await;
        // The request goes (its client disconnected); the job runs on, and
        // no launch of the agent gets in beside it.
        request.abort();
        let _ = request.await;
        assert!(held(&daemon), "the lease went with the request");
        // Nor does another operation: the job, not the request, holds the
        // daemon's VCS lock.
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let next = {
            let (daemon, ran) = (daemon.clone(), ran.clone());
            tokio::spawn(async move {
                daemon
                    .agent_git("a", move || {
                        ran.store(true, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !ran.load(Ordering::SeqCst),
            "a second operation ran beside the cancelled request's job"
        );
        release.send(()).unwrap();
        next.await.unwrap().unwrap();
        assert!(ran.load(Ordering::SeqCst));
        until_held(daemon.clone(), false).await;
        // ...and the operation still ends in its terminal event, with the
        // start's op.
        until("the terminal merge event", || {
            vcs_events(&daemon).len() == 2
        })
        .await;
        let events = vcs_events(&daemon);
        let (start, end) = (&events[0].0, &events[1].0);
        assert_eq!(start.outcome, VcsOutcome::Started);
        assert_eq!(end.outcome, VcsOutcome::Ok);
        assert_eq!((&start.op, start.action), (&end.op, VcsAction::Merge));
        assert_eq!(end.peer.as_deref(), Some("b"));
        assert_eq!(events[0].1.level, EventLevel::Debug);
        assert_eq!(events[1].1.level, EventLevel::Info);
        assert_eq!(events[1].1.source, VCS_SOURCE);
        assert_eq!(events[1].1.event_type, "vcs.merge");
        assert_eq!(events[1].1.agent_id.as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn a_job_that_panics_or_returns_early_ends_in_an_error_event() {
        let temp = tempfile::tempdir().unwrap();
        let running = daemon_with_agent(&temp).await;
        let daemon = running.daemon.clone();
        let mut op = VcsOp::new(&daemon, VcsAction::CherryPick, "a").source("abc1234");
        let result = daemon
            .agent_git::<()>("a", move || {
                op.start();
                panic!("the job broke");
            })
            .await;
        assert!(result.is_err());
        let mut op = VcsOp::new(&daemon, VcsAction::Commit, "a");
        let _ = daemon
            .agent_git::<()>("a", move || {
                op.begin();
                anyhow::bail!("an early return")
            })
            .await;
        let events = vcs_events(&daemon);
        let outcomes: Vec<_> = events
            .iter()
            .map(|(e, _)| (e.action, e.outcome, e.summary.clone()))
            .collect();
        assert_eq!(
            outcomes,
            [
                (VcsAction::CherryPick, VcsOutcome::Started, None),
                (
                    VcsAction::CherryPick,
                    VcsOutcome::Error,
                    Some("interrupted".to_string())
                ),
                (
                    VcsAction::Commit,
                    VcsOutcome::Error,
                    Some("interrupted".to_string())
                ),
            ]
        );
        assert_eq!(events[0].0.op, events[1].0.op);
        assert_ne!(events[1].0.op, events[2].0.op);
        assert_eq!(events[1].1.level, EventLevel::Warn);
        // An operation refused before any VCS ran says nothing.
        let op = VcsOp::new(&daemon, VcsAction::Merge, "a");
        let refused = daemon
            .agent_git::<()>("nobody", move || {
                drop(op);
                Ok(())
            })
            .await;
        assert!(refused.is_err());
        assert_eq!(vcs_events(&daemon).len(), 3);
    }

    #[tokio::test]
    async fn reads_leave_debug_events_a_failed_commit_an_error_and_a_refusal_nothing() {
        let temp = tempfile::tempdir().unwrap();
        crate::git::init_repo(temp.path()).unwrap();
        let running = daemon_with_agent(&temp).await;
        let daemon = running.daemon.clone();
        daemon.model().session.isolation = "worktree".to_string();
        daemon.git_status("a").await.unwrap();
        daemon
            .git_diff("a", Some("HEAD".to_string()))
            .await
            .unwrap();
        let err = daemon
            .git_commit("a", "nothing".to_string())
            .await
            .unwrap_err();
        assert!(err.message.contains("nothing to commit"), "{}", err.message);
        // Refused before any VCS ran: no event.
        daemon.model().session.isolation = "shared".to_string();
        assert!(daemon.git_commit("a", "x".to_string()).await.is_err());
        let events = vcs_events(&daemon);
        let seen: Vec<_> = events
            .iter()
            .map(|(v, e)| (v.action, v.outcome, e.level))
            .collect();
        assert_eq!(
            seen,
            [
                (VcsAction::Status, VcsOutcome::Ok, EventLevel::Debug),
                (VcsAction::Diff, VcsOutcome::Ok, EventLevel::Debug),
                (VcsAction::Commit, VcsOutcome::Error, EventLevel::Warn),
            ]
        );
        assert_eq!(events[1].0.source.as_deref(), Some("HEAD"));
        assert_eq!(events[0].0.backend, "git");
        assert!(events[2]
            .0
            .summary
            .as_deref()
            .is_some_and(|s| s.contains("nothing to commit")));
        assert_eq!(
            events[2].1.message,
            format!(
                "a: commit failed: {}",
                events[2].0.summary.as_deref().unwrap()
            )
        );
        let ops: std::collections::HashSet<_> = events.iter().map(|(v, _)| &v.op).collect();
        assert_eq!(ops.len(), 3, "one op per operation");
    }

    #[test]
    fn every_event_reads_as_a_line_of_the_log() {
        let event = |action, outcome| {
            let mut e = VcsEvent::new(action, outcome, "v1", "jj", "ledger");
            e.commit = Some("5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f".into());
            e.branch = Some("agents/ledger".into());
            e
        };
        let mut pick = event(VcsAction::CherryPick, VcsOutcome::Ok);
        pick.source = Some("1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b".into());
        pick.peer = Some("db".into());
        assert_eq!(
            message(&pick),
            "ledger cherry-picked 1a2b3c4d from db as 5e6f7a8b"
        );
        pick.outcome = VcsOutcome::Conflict;
        pick.peer = Some("rival".into());
        pick.files = vec!["schema.sql".into()];
        assert_eq!(
            message(&pick),
            "ledger: cherry-pick of 1a2b3c4d conflicts with rival in schema.sql; nothing was \
changed"
        );
        let mut tally = event(VcsAction::Integrate, VcsOutcome::Conflict);
        tally.agent = INTEGRATION_AGENT.into();
        tally.branch = Some("agents/integration".into());
        tally.summary = Some("2 merged, 1 conflict".into());
        assert_eq!(
            message(&tally),
            "integration on agents/integration: 2 merged, 1 conflict"
        );
        let mut failed = event(VcsAction::Commit, VcsOutcome::Error);
        failed.agent = "db".into();
        failed.summary = Some("nothing to commit: the working tree has no changes".into());
        assert_eq!(
            message(&failed),
            "db: commit failed: nothing to commit: the working tree has no changes"
        );
        let mut diff = event(VcsAction::Diff, VcsOutcome::Ok);
        diff.agent = "db".into();
        diff.source = Some("agents/ledger".into());
        assert_eq!(message(&diff), "db: diff against agents/ledger");
        assert_eq!(
            message(&event(VcsAction::Workspace, VcsOutcome::Started)),
            "creating the workspace of ledger…"
        );
        assert_eq!(
            message(&event(VcsAction::Workspace, VcsOutcome::Ok)),
            "workspace for ledger on agents/ledger at 5e6f7a8b"
        );
        assert_eq!(agent_of_email("db@crew.local").as_deref(), Some("db"));
        assert_eq!(agent_of_email("crew@crew.local"), None);
        assert_eq!(agent_of_email("someone@example.com"), None);
    }
}
