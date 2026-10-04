//! The team roster: who is on the team right now, kept on disk.
//!
//! A team file can list its agents, or leave that to the orchestrator, which
//! then builds the team with `agent_add` and reshapes it with `agent_add` /
//! `agent_remove` as the work evolves. Either way the current roster is
//! written to `.crew/roster.yaml` after every change — a complete, runnable
//! team file — so the next `crew run` starts from the team that was settled
//! on instead of designing it again.
//!
//! What is saved is the founding team: the members the orchestrator has
//! when its first turn with a plan ends. Members it adds after that are
//! temporary, for extra hands or a passing need: they are not saved, and
//! they leave once their work is done and the orchestrator has heard so.

use crew_core::config::{TeamConfig, ROSTERS_DIR, ROSTER_FILE};
use crew_core::rpc::{Push, RpcError};
use crew_core::util::now_ms;
use crew_core::{AgentStatus, RuntimeKind, Task, TaskStatus};

use super::handlers::internal;
use super::Daemon;
use crate::persistence::NewEvent;

const HEADER: &str = "\
# The team as it currently stands, maintained by crew.
# Written whenever a member is added or removed (by you or by the
# orchestrator). `crew run` reuses it when the team file lists no agents;
# pass --replan to let the orchestrator design the team again.
";

impl Daemon {
    /// The current team as a runnable team file (no tasks: those belong to a
    /// session, not to the team).
    pub(crate) fn roster(&self) -> Option<TeamConfig> {
        let model = self.model();
        let mut team = model.session.team.clone()?;
        team.agents = model
            .agents
            .iter()
            // Subagents come and go with their parent's work, and temporary
            // members with theirs; they are not standing members of the team.
            .filter(|a| !a.orchestrator && a.parent.is_none() && !a.temporary)
            .map(|a| {
                let mut spec = model
                    .local
                    .get(&a.id)
                    .map(|l| l.spec.clone())
                    .unwrap_or_default();
                spec.id = a.id.clone();
                spec.role = a.role.clone();
                // On a work style's fallback, the member is still its own model's.
                match model.local.get(&a.id).and_then(|l| l.home.as_ref()) {
                    Some(home) => {
                        spec.runtime = Some(home.runtime);
                        spec.model = home.model.clone();
                    }
                    None => {
                        spec.runtime = Some(a.runtime);
                        spec.model = a.model.clone();
                    }
                }
                spec.task = None;
                spec
            })
            .collect();
        team.tasks = Vec::new();
        Some(team)
    }

    /// Save the roster: into the session (so a restarted daemon knows the
    /// team) and into `.crew/roster.yaml` (so the next run does).
    pub(crate) fn persist_roster(&self) {
        let Some(team) = self.roster() else { return };
        {
            let mut model = self.model();
            if let Some(current) = model.session.team.as_mut() {
                current.agents = team.agents.clone();
            }
        }
        self.save_session();
        match team.to_yaml() {
            Ok(yaml) => {
                let mut paths = vec![self.state_dir.join(ROSTER_FILE)];
                // A workspace can be worked on in several styles; each keeps
                // the team it settled on.
                if let Some(style) = &team.style {
                    let dir = self.state_dir.join(ROSTERS_DIR);
                    if let Err(e) = std::fs::create_dir_all(&dir) {
                        tracing::error!("creating {}: {e}", dir.display());
                    }
                    paths.push(dir.join(format!("{style}.yaml")));
                }
                for path in paths {
                    if let Err(e) = std::fs::write(&path, format!("{HEADER}{yaml}")) {
                        tracing::error!("writing {}: {e}", path.display());
                    }
                }
            }
            Err(e) => tracing::error!("encoding the roster: {e:#}"),
        }
    }

    /// Take an agent off the team. Its process is stopped and its unfinished
    /// tasks go back to the pool for the scheduler (or the orchestrator) to
    /// hand to someone else. Its branch and worktree are left in place, and
    /// integration still merges its branch. `why` goes in the event.
    pub(crate) async fn remove_agent(&self, id: &str, why: &str) -> Result<(), RpcError> {
        let agent = self
            .agent(id)
            .ok_or_else(|| RpcError::not_found(format!("no agent '{id}'")))?;
        if agent.orchestrator {
            return Err(RpcError::forbidden("the orchestrator cannot be removed"));
        }
        // Stop the process first (this also parks its active task).
        self.kill_agent(id).await?;
        let orphaned: Vec<String> = {
            let model = self.model();
            model
                .tasks
                .iter()
                .filter(|t| t.assigned_agent.as_deref() == Some(id) && !t.status.is_terminal())
                .map(|t| t.id.clone())
                .collect()
        };
        for task in &orphaned {
            self.update_task(task, |t| {
                t.assigned_agent = None;
                t.status = TaskStatus::Queued;
                t.started_at = None;
                t.progress = None;
                t.note = Some(format!("returned to the pool when {id} left the team"));
            });
        }
        // Subagents of a removed agent have nobody to report to.
        let children: Vec<String> = {
            let model = self.model();
            model
                .agents
                .iter()
                .filter(|a| a.parent.as_deref() == Some(id))
                .map(|a| a.id.clone())
                .collect()
        };
        for child in children {
            Box::pin(self.remove_agent(&child, "")).await?;
        }
        {
            let mut model = self.model();
            model.agents.retain(|a| a.id != id);
            model.tokens.remove(id);
            model.local.remove(id);
            model.rt.remove(id);
        }
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        self.store.delete_agent(id).map_err(internal)?;
        let _ = self.store.delete_meta(&format!("agent:{id}"));
        self.push(Push::AgentRemoved { id: id.to_string() });
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.removed",
                format!(
                    "{id} left the team{why}{}",
                    if orphaned.is_empty() {
                        String::new()
                    } else {
                        format!("; tasks returned to the pool: {}", orphaned.join(", "))
                    }
                ),
            )
            .agent(id),
        );
        self.persist_roster();
        self.replan();
        Ok(())
    }

    /// The orchestrator's turn ended. The first one to end with a plan in
    /// the task graph settles the founding team: the members on the team now
    /// are the ones saved with the roster, and those the orchestrator adds
    /// from here on are temporary.
    pub(crate) fn found_team_once_planned(&self) {
        let founders: Vec<String> = {
            let mut model = self.model();
            if model.session.founded_at.is_some() || model.tasks.is_empty() {
                return;
            }
            model.session.founded_at = Some(now_ms());
            model
                .agents
                .iter()
                .filter(|a| !a.orchestrator && a.parent.is_none() && !a.temporary)
                .map(|a| a.id.clone())
                .collect()
        };
        self.save_session();
        let founders = if founders.is_empty() {
            "no members".to_string()
        } else {
            founders.join(", ")
        };
        self.emit(NewEvent::new(
            "daemon",
            "team.founded",
            format!(
                "the founding team is settled: {founders}; members the orchestrator adds from now on are temporary"
            ),
        ));
    }

    /// What the orchestrator is told with the news that a temporary member's
    /// task finished or failed, when the member has nothing else to do.
    pub(crate) fn leaving_note(&self, id: &str) -> String {
        let model = self.model();
        let temporary = model.agent(id).is_some_and(|a| a.temporary);
        if !temporary || work_of(&model.tasks, id).0 {
            return String::new();
        }
        format!(
            " {id} is temporary and has nothing else to do: it leaves the team after this turn \
unless you give it more work now."
        )
    }

    /// Temporary members whose work is done leave the team, once the
    /// orchestrator has read that and had its turn to give them more.
    pub(crate) async fn temporary_housekeeping(&self) {
        let now = now_ms();
        let (orchestrator, leaving) = {
            let mut model = self.model();
            if !model.agents.iter().any(|a| a.temporary) {
                return;
            }
            let Some(orchestrator) = model.orchestrator().cloned() else {
                return;
            };
            let rt = model.rt(&orchestrator.id);
            // What it has read: messages put into a turn that has since
            // ended, and stayed ended (a message queued during a turn can
            // start the next one).
            let heard = if orchestrator.runtime == RuntimeKind::Shell {
                rt.delivered_at
            } else if rt.turn_active
                || rt.turn_ended_at < rt.delivered_at
                || now - rt.turn_ended_at < SETTLE_MS
            {
                return;
            } else {
                rt.delivered_at
            };
            let leaving: Vec<String> = model
                .agents
                .iter()
                .filter(|a| a.temporary && !a.paused && a.status != AgentStatus::Spawning)
                .filter(|a| !model.rt.get(&a.id).is_some_and(|rt| rt.turn_active))
                // Its subagents still report to it.
                .filter(|a| {
                    !model.agents.iter().any(|c| {
                        c.parent.as_deref() == Some(a.id.as_str())
                            && c.status != AgentStatus::Done
                            && !c.status.is_terminal()
                    })
                })
                .filter(|a| matches!(work_of(&model.tasks, &a.id), (false, Some(ended)) if heard > ended))
                .map(|a| a.id.clone())
                .collect();
            (orchestrator, leaving)
        };
        if leaving.is_empty() {
            return;
        }
        // News still on its way to the orchestrator may be about them.
        let pending = self.bus.undelivered(&orchestrator.id).unwrap_or_default();
        if super::delivery::wakes(&orchestrator, &pending, true) {
            return;
        }
        for id in leaving {
            // Given work, or a message, since.
            if work_of(&self.model().tasks, &id).0
                || !self.bus.undelivered(&id).unwrap_or_default().is_empty()
            {
                continue;
            }
            if let Err(e) = self
                .remove_agent(&id, ": a temporary member, its work done")
                .await
            {
                tracing::warn!("taking {id} off the team: {}", e.message);
            }
        }
    }
}

/// How long the orchestrator stays at rest after a turn before what it was
/// told counts as read.
const SETTLE_MS: i64 = 5_000;

/// Where a member stands with its work: whether a task it is on, or is to
/// review, is still open; and when the last of its finished ones ended
/// (`None`: it has had none).
fn work_of(tasks: &[Task], id: &str) -> (bool, Option<i64>) {
    let mut open = false;
    let mut ended = None;
    for task in tasks
        .iter()
        .filter(|t| t.assigned_agent.as_deref() == Some(id) || t.reviewer.as_deref() == Some(id))
    {
        if task.status.is_terminal() {
            ended = ended.max(Some(task.completed_at.unwrap_or(task.created_at)));
        } else {
            open = true;
        }
    }
    (open, ended)
}
