//! Creating agents: their records, worktrees and tokens; adding one to a running team.

use anyhow::Result;
use crew_core::config::{validate_agent_spec, AgentSpec, TeamConfig, MAX_AGENTS};
use crew_core::rpc::{Push, RpcError};
use crew_core::util::{new_token, now_ms};
use crew_core::{Agent, AgentStatus, RuntimeKind};

use super::{agent_settings, chosen_session_id};
use crate::daemon::handlers::internal;
use crate::daemon::{AgentLocal, Daemon, IdReservation};
use crate::persistence::NewEvent;

impl Daemon {
    /// Create the persistent record of an agent (worktree, token, settings).
    /// The process is started separately by `spawn_agent`.
    pub(crate) fn create_agent(
        &self,
        spec: &AgentSpec,
        team: &TeamConfig,
        orchestrator: bool,
        worktrees: bool,
        backend: &str,
        parent: Option<&Agent>,
    ) -> Result<Agent> {
        // A model from the catalog decides the CLI it runs on.
        let catalog = crate::models::Catalog::load();
        let assigned = catalog.assign(
            spec.runtime,
            spec.model.as_deref(),
            team.defaults.runtime,
            team.defaults.model.as_deref(),
        );
        // The model's proper name, for the dashboard: from its id in the
        // catalog (`opus` is `claude-opus-5-5`, shown as `Opus 5.5`).
        let model_name = assigned.model.as_deref().map(|model| {
            let id = catalog
                .find(model)
                .map(|profile| profile.id.as_str())
                .filter(|id| !id.is_empty())
                .unwrap_or(model);
            crew_core::types::model_display_name(id)
        });
        let runtime = assigned.runtime;
        if let Some(note) = &assigned.note {
            self.emit(
                NewEvent::new("daemon", "agent.model", format!("{}: {note}", spec.id))
                    .agent(&spec.id),
            );
        }
        // A subagent starts from its parent's branch, so it sees the parent's
        // committed work.
        let base = parent
            .and_then(|p| p.branch.clone())
            .unwrap_or_else(|| team.workspace.base.clone());
        let (workdir, branch) = if worktrees && !orchestrator {
            let wt = self.member_worktree(team, &spec.id, &base)?;
            (wt.path, Some(wt.branch))
        } else {
            (self.workspace.clone(), None)
        };
        let settings = agent_settings(team, spec, orchestrator);
        let now = now_ms();
        let agent = Agent {
            id: spec.id.clone(),
            name: spec.name.clone().unwrap_or_else(|| spec.id.clone()),
            role: if orchestrator && spec.role.is_empty() {
                "orchestrator".to_string()
            } else {
                spec.role.clone()
            },
            task: None,
            status: AgentStatus::Spawning,
            activity: String::new(),
            runtime,
            model: assigned.model,
            workdir: workdir.to_string_lossy().into_owned(),
            branch,
            backend: backend.to_string(),
            backend_ref: None,
            pid: None,
            session_id: chosen_session_id(runtime),
            orchestrator,
            parent: parent.map(|p| p.id.clone()),
            subagents: 0,
            temporary: false,
            delivery: "terminal".to_string(),
            started_at: None,
            last_activity_at: now,
            progress: None,
            paused: false,
            block_kind: None,
            limit: None,
            context: None,
            compact_pending: false,
            compactions: 0,
            running_model: None,
            model_name,
            effort: None,
            fallback: None,
            exit_code: None,
            restarts: 0,
            inbox_unread: 0,
            inbox_total: 0,
            outbox_total: 0,
            usage: None,
            error: None,
        };
        let token = new_token();
        self.store.save_token(&agent.id, &token)?;
        self.store.save_agent(&agent)?;
        {
            let mut model = self.model();
            model.tokens.insert(agent.id.clone(), token);
            model.local.insert(
                agent.id.clone(),
                AgentLocal {
                    settings,
                    spec: spec.clone(),
                    ..AgentLocal::default()
                },
            );
            model.agents.retain(|a| a.id != agent.id);
            model.agents.push(agent.clone());
        }
        self.save_local(&agent.id);
        self.push(Push::Agent(Box::new(agent.clone())));
        Ok(agent)
    }

    /// Add a member. A `temporary` one is not saved with the roster and
    /// leaves once its work is done.
    pub(crate) async fn add_agent(
        &self,
        spec: AgentSpec,
        temporary: bool,
    ) -> Result<Agent, RpcError> {
        validate_agent_spec(&spec).map_err(|e| RpcError::invalid(e.to_string()))?;
        let (team, worktrees, backend) = {
            let mut model = self.model();
            let team = model
                .session
                .team
                .clone()
                .ok_or_else(|| RpcError::unavailable("no team is loaded"))?;
            if model.id_taken(&spec.id) {
                return Err(RpcError::conflict(format!(
                    "agent '{}' already exists",
                    spec.id
                )));
            }
            let members = model.agents.iter().filter(|a| !a.orchestrator).count();
            if members + model.reserved.len() >= MAX_AGENTS {
                return Err(RpcError::invalid(format!(
                    "a team has at most {MAX_AGENTS} agents"
                )));
            }
            // Taken before anything is awaited, so a second add of the same
            // id is refused rather than racing this one.
            model.reserved.insert(spec.id.clone());
            (
                team,
                model.session.isolation == "worktree",
                model.session.backend.clone(),
            )
        };
        let _reserved = IdReservation::new(self, &spec.id);
        // A model whose usage window is used up cannot be started until it
        // resets: the work style's fallback takes its place, or the caller
        // hears what to take instead.
        let (asked, team_for_assign) = (spec.clone(), team.clone());
        let assigned = tokio::task::spawn_blocking(move || {
            crate::models::Catalog::load().assign(
                asked.runtime,
                asked.model.as_deref(),
                team_for_assign.defaults.runtime,
                team_for_assign.defaults.model.as_deref(),
            )
        })
        .await
        .map_err(internal)?;
        // A catalog model decides the CLI, so this is the runtime it gets.
        if assigned.runtime == RuntimeKind::Shell
            && spec.command.is_empty()
            && team.defaults.command.is_empty()
        {
            return Err(RpcError::invalid("shell agents need a command"));
        }
        if let Some((out, instead)) = self
            .out_now(assigned.runtime, assigned.model.as_deref())
            .await
        {
            let model = assigned.model.clone().unwrap_or_default();
            if !self.has_style_fallback(&model, assigned.runtime).await {
                let resets = out
                    .resets_at
                    .map(|at| {
                        format!(
                            " (it resets {})",
                            crew_core::util::fmt_when(at, crew_core::util::now_ms())
                        )
                    })
                    .unwrap_or_default();
                let instead = if instead.is_empty() {
                    "no comparable model is free either".to_string()
                } else {
                    format!("take {} instead", instead.join(" or "))
                };
                return Err(RpcError::invalid(format!(
                    "{model} cannot be started now: {} is used up{resets}; {instead} (usage_status shows what is out)",
                    out.window
                )));
            }
        }
        // Its task is checked first: an agent whose task cannot be created is
        // not created either.
        let own_task = spec
            .task
            .clone()
            .map(|description| self.own_task(&spec.id, description))
            .transpose()?;
        let daemon = self.clone();
        let spec_for_task = spec.clone();
        let agent = tokio::task::spawn_blocking(move || {
            daemon.create_agent(&spec_for_task, &team, false, worktrees, &backend, None)
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        if temporary {
            self.update_agent(&agent.id, |a| a.temporary = true);
        }
        // On the work style's fallback from the start, when its model is out.
        self.fall_back_by_style(&agent.id, None, Some(true)).await;
        let agent = self.agent(&agent.id).unwrap_or(agent);
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.added",
                format!(
                    "added agent {}{}",
                    agent.id,
                    if temporary {
                        " (temporary: it leaves once its work is done)"
                    } else {
                        ""
                    }
                ),
            )
            .agent(&agent.id),
        );
        if let Some(task) = own_task {
            if let Err(e) = self.create_task(task) {
                tracing::warn!("creating the task for {}: {}", spec.id, e.message);
            }
        }
        self.persist_roster();
        self.replan();
        self.spawn_in_background(&agent.id, false, None);
        Ok(agent)
    }
}
