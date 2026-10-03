//! Git operations requested by agents, run by the daemon on their behalf.
//!
//! Doing commits, cherry-picks and merges here (rather than letting agents
//! shell out) keeps a team-visible record, attaches commits to tasks as
//! artifacts, and lets conflicts be aborted and reported uniformly.

use std::path::PathBuf;

use crew_core::rpc::{CommitInfo, GitOutcome, RpcError};
use crew_core::util::{is_valid_id, now_ms};
use crew_core::Artifact;
use serde_json::{json, Value};

use super::Daemon;
use crate::git;
use crate::persistence::NewEvent;
use crate::tasks::active_task;

fn git_error(e: impl std::fmt::Display) -> RpcError {
    RpcError::new("git", e.to_string())
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

    /// Run a blocking git operation, one at a time per daemon.
    async fn git<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) -> Result<T, RpcError> {
        let _guard = self.git_lock.lock().await;
        tokio::task::spawn_blocking(f)
            .await
            .map_err(git_error)?
            .map_err(|e| git_error(format!("{e:#}")))
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

    async fn ensure_repo(&self, dir: &std::path::Path) -> Result<(), RpcError> {
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
        let commit = self
            .git(move || git::commit_all(&dir, &author, &message))
            .await?;
        // Attach the commit to the task the agent is working on.
        let task = {
            let model = self.model();
            active_task(&model.tasks, agent).map(|t| t.id.clone())
        };
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
        let mut event = NewEvent::new(
            "git",
            "git.commit",
            format!(
                "{agent} committed {} on {}: {}",
                commit.short, commit.branch, commit.summary
            ),
        )
        .agent(agent)
        .data(json!({ "sha": commit.sha, "branch": commit.branch }));
        if let Some(task) = &task {
            event = event.task(task);
        }
        self.emit(event);
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
        let text = self
            .git(move || git::diff(&dir, against.as_deref()))
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
        let text = self.git(move || git::status(&dir)).await?;
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
        let rev = commit.clone();
        let outcome = self
            .git(move || git::cherry_pick(&dir, &author, &rev))
            .await?;
        self.report_git(agent, &outcome);
        if let Some(applied) = &outcome.commit {
            self.emit(
                NewEvent::new(
                    "git",
                    "git.cherry_pick",
                    format!("{agent} cherry-picked {commit} as {}", applied.short),
                )
                .agent(agent)
                .data(json!({ "source": commit, "sha": applied.sha })),
            );
        }
        Ok(outcome)
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
                let (repo, branch) = (self.workspace.clone(), git::branch_for(&from_agent));
                let exists = worktrees && {
                    let branch = branch.clone();
                    self.git(move || Ok(git::branch_exists(&repo, &branch)))
                        .await?
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
        let rev = branch.clone();
        let outcome = self.git(move || git::merge(&dir, &author, &rev)).await?;
        self.report_git(agent, &outcome);
        if let Some(applied) = &outcome.commit {
            self.emit(
                NewEvent::new(
                    "git",
                    "git.merge",
                    format!("{agent} merged {branch} ({})", applied.short),
                )
                .agent(agent),
            );
        }
        Ok(outcome)
    }

    /// Merge every agent branch into the integration branch.
    pub(crate) async fn integrate(&self) -> Result<Value, RpcError> {
        let (agents, base, worktrees) = {
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
            )
        };
        if !worktrees {
            return Err(git_error(
                "agents share the workspace, so there are no branches to integrate",
            ));
        }
        let repo = self.workspace.clone();
        let dir = self.worktrees_dir();
        let (worktree, steps) = self
            .git(move || git::integrate(&repo, &dir, &base, &agents))
            .await?;
        let mut merged = Vec::new();
        let mut conflicts = Vec::new();
        for step in &steps {
            self.report_git("integration", &step.outcome);
            if step.outcome.ok {
                merged.push(step.agent.clone());
            } else if let Some(conflict) = &step.outcome.conflict {
                conflicts.push(json!({ "agent": step.agent, "files": conflict.files }));
            }
        }
        self.emit(NewEvent::new(
            "git",
            "git.integrate",
            format!(
                "integration on {}: merged {} branch(es), {} conflict(s)",
                worktree.branch,
                merged.len(),
                conflicts.len()
            ),
        ));
        Ok(json!({
            "branch": worktree.branch,
            "path": worktree.path.to_string_lossy(),
            "merged": merged,
            "conflicts": conflicts,
        }))
    }
}
