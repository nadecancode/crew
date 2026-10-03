//! Agent supervision: starting, stopping and restarting processes, turning
//! runtime signals into agent status, delivering messages into sessions, and
//! recovering after a daemon restart.

use anyhow::{anyhow, Result};
use crew_core::config::{validate_agent_spec, AgentSpec, TeamConfig, MAX_AGENTS};
use crew_core::paths::{write_private_file, Identity};
use crew_core::rpc::{Push, RpcError};
use crew_core::util::{new_token, now_ms};
use crew_core::{Agent, AgentStatus, RuntimeKind, TaskStatus, OPERATOR_ID, ORCHESTRATOR_ID};
use serde_json::Value;

use super::handlers::internal;
use super::sandbox::Run;
use super::{AgentLocal, AgentRuntimeState, Daemon, IdReservation};
use crate::agents::prompt::{self, TeamView};
use crate::agents::{runtime_for, AgentSettings, LaunchContext, Signal};
use crate::persistence::NewEvent;
use crate::process::{Hint, ProcEvent, ProcessManager};
use crate::tasks::active_task;

use super::delivery::DELIVERY_GAP;

/// Kill a sidecar and reap it in the background. Its sandbox, if it has
/// one, stays in the ledger until it reports that it is gone.
fn reap_sidecar(mut child: tokio::process::Child) {
    let _ = child.start_kill();
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
}

/// How long an agent's old sandboxes may take to go before its next launch
/// fails (its home is never seeded while they run).
const RETIRE_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

const RESUME_NOTICE: &str = "[crew] Your session was restarted. Continue your current task where \
you left off, and call agent_inbox to check for messages you may have missed.";

/// What a start does with the agent's session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// Starts it in the session it has, not resuming one (a new agent).
    New,
    /// Resumes its session, once that is confirmed.
    Resume,
    /// Leaves its session for a new one, never resuming it.
    Fresh,
}

/// The session id a new session of `runtime` starts with: Claude Code and
/// pi let us choose it, which makes resuming exact.
fn chosen_session_id(runtime: RuntimeKind) -> Option<String> {
    matches!(runtime, RuntimeKind::Claude | RuntimeKind::Pi)
        .then(|| uuid::Uuid::new_v4().to_string())
}

/// How an agent declared by `spec` is run: the team's defaults with the
/// declaration's own settings over them.
pub(crate) fn agent_settings(
    team: &TeamConfig,
    spec: &AgentSpec,
    orchestrator: bool,
) -> AgentSettings {
    let mut settings = AgentSettings::resolve(&team.defaults, spec);
    if orchestrator && spec.permission_mode.is_none() {
        // Guarded mode: the orchestrator coordinates and never needs to
        // edit files, and it must never stall on a permission prompt.
        settings.permission_mode = "dontAsk".to_string();
    }
    settings
}

impl Daemon {
    // ---- creation -------------------------------------------------------

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

    // ---- starting -------------------------------------------------------

    pub(crate) fn spawn_in_background(&self, id: &str, resume: bool, notice: Option<String>) {
        let daemon = self.clone();
        let since = self.stops(id);
        let id = id.to_string();
        tokio::spawn(async move {
            let start = if resume { Start::Resume } else { Start::New };
            if let Err(e) = daemon.spawn_agent(&id, start, notice, since).await {
                daemon
                    .mark_failed(&id, format!("failed to start: {e:#}"))
                    .await;
            }
        });
    }

    /// How many stops of agent `id` have been asked for (a kill, or a
    /// restart taking over). A start takes this when it is asked for and
    /// gives up once it has changed: a stop asked for while the start waits
    /// (for its sandboxes, its sidecar, its process) is not lost to it.
    pub(crate) fn stops(&self, id: &str) -> u64 {
        let stops = self.stops.lock().unwrap_or_else(|e| e.into_inner());
        stops.get(id).copied().unwrap_or(0)
    }

    /// Asks for a stop of agent `id`; the count after it.
    fn stop_requested(&self, id: &str) -> u64 {
        let mut stops = self.stops.lock().unwrap_or_else(|e| e.into_inner());
        let count = stops.entry(id.to_string()).or_default();
        *count += 1;
        *count
    }

    /// Start (or restart) an agent's process; `since` is Daemon::stops when
    /// the start was asked for. A stop asked for after that makes it give up
    /// at its next step, taking down what it had started; what goes wrong
    /// after the stop is not the agent's failure (the stop has made it
    /// Stopped), so it is not returned.
    pub(crate) async fn spawn_agent(
        &self,
        id: &str,
        start: Start,
        notice: Option<String>,
        since: u64,
    ) -> Result<()> {
        match self.start_agent(id, start, notice, since).await {
            Err(e) if self.stops(id) != since => {
                tracing::info!("{id}: a start given up for a stop ended in: {e:#}");
                Ok(())
            }
            started => started,
        }
    }

    async fn start_agent(
        &self,
        id: &str,
        start: Start,
        notice: Option<String>,
        since: u64,
    ) -> Result<()> {
        let stopped = || self.stops(id) != since;
        let resume = start == Start::Resume;
        // One launch of an agent at a time, and none beside its git jobs or
        // queue commands: two restarts close together must not both find
        // nothing running and seed its home while the other's sandboxes
        // start, nor write it beside a helper's sandbox.
        let launch = self.launch_lease(id).write_owned().await;
        // Stopped while it waited for its turn: a launch after the stop may
        // be running, and retiring would take it for this one's predecessor.
        if stopped() {
            return Ok(());
        }
        let procs = self.procs().map_err(|e| anyhow!(e.message))?;
        // Before anything of the agent's writable tree is touched (even made):
        // nothing of its previous sandboxes may be left (D2).
        if self.sandboxed() {
            self.retire_sandboxes(id, procs.as_ref(), RETIRE_WITHIN)
                .await?;
        }
        if stopped() {
            return Ok(());
        }
        // A model known to be out starts on the work style's fallback.
        let fresh = !(resume
            && self
                .model()
                .local
                .get(id)
                .is_some_and(|l| l.session_confirmed));
        self.fall_back_by_style(id, None, Some(fresh)).await;
        let (mut agent, mut local, team, agents, tasks, token, worktrees) = {
            let model = self.model();
            let agent = model
                .agent(id)
                .cloned()
                .ok_or_else(|| anyhow!("no agent '{id}'"))?;
            let team = model
                .session
                .team
                .clone()
                .ok_or_else(|| anyhow!("no team is loaded"))?;
            (
                agent,
                model.local.get(id).cloned().unwrap_or_default(),
                team,
                model.agents.clone(),
                model.tasks.clone(),
                model.tokens.get(id).cloned().unwrap_or_default(),
                model.session.isolation == "worktree",
            )
        };
        if start == Start::Fresh {
            // Its new session is chosen here, where nothing of the old one
            // runs any more, and this start uses it whatever the model says:
            // the old session reporting itself as it was stopped (its stop
            // hook) does not hand it back.
            agent.session_id = chosen_session_id(agent.runtime);
            local.session_confirmed = false;
            self.model()
                .local
                .entry(id.to_string())
                .or_default()
                .session_confirmed = false;
            self.save_local(id);
            self.update_agent(id, |a| a.session_id = agent.session_id.clone());
        }
        let runtime = runtime_for(agent.runtime);

        let agent_dir = self.agent_dir(id);
        let agent_root = self.agent_root(id)?;
        // A sandboxed agent's CLI keeps its configuration, credentials and sessions
        // in a private home under its writable tree, not in the operator's.
        let private_home = self.sandboxed().then(|| agent_dir.join("rw/home"));
        let identity_path = agent_dir.join("identity.json");
        let identity = Identity {
            socket: self.socket.to_string_lossy().into_owned(),
            agent_id: id.to_string(),
            token,
        };
        write_private_file(&identity_path, &serde_json::to_vec_pretty(&identity)?)?;

        // Only the orchestrator staffs the team, so only it gets the catalog.
        let models = if agent.orchestrator {
            let fallbacks = team.defaults.model_fallbacks.clone();
            tokio::task::spawn_blocking(move || {
                let mut text =
                    crate::models::Catalog::load().briefing(&crate::models::Availability::detect());
                text.push_str(&prompt::fallbacks_section(&fallbacks));
                text
            })
            .await?
        } else {
            String::new()
        };
        let view = TeamView {
            project: &team.project,
            objective: &team.objective,
            style: team.style.as_deref(),
            instructions: &team.instructions,
            models: &models,
            agents: &agents,
            worktrees,
            voice: team.defaults.voice,
        };
        let briefing = if agent.orchestrator {
            prompt::orchestrator_briefing(&view)
        } else {
            prompt::worker_briefing(&view, &agent)
        };
        // Kept on disk for every runtime: what the agent was told, to read back.
        write_private_file(&agent_dir.join("briefing.md"), briefing.as_bytes())?;
        let resume = resume && local.session_confirmed;
        // Endpoint and key from the crew config, read afresh at every launch
        // so a changed file or a rotated key applies on the next restart. A
        // config that cannot be used fails the launch: the agent must not
        // quietly run on the operator's own login instead.
        let workspace = self.workspace.clone();
        let (agent_id, parent) = (id.to_string(), agent.parent.clone());
        let agent_runtime = agent.runtime;
        let endpoint = tokio::task::spawn_blocking(move || {
            crate::endpoints::Layers::load(&workspace).resolve(
                agent_runtime,
                &agent_id,
                parent.as_deref(),
            )
        })
        .await??;
        // The 1M-token context window, for a model the catalog says has one
        // (and that the account did not refuse before). A model set by the
        // endpoint config is somebody else's: it is passed as written.
        let long_context = match &agent.model {
            Some(model)
                if agent.runtime != RuntimeKind::Shell
                    && local.settings.long_context
                    && !local.no_long_context
                    && endpoint.model.is_none() =>
            {
                let model = model.clone();
                tokio::task::spawn_blocking(move || {
                    crate::models::Catalog::load()
                        .find(&model)
                        .is_some_and(|m| m.long_context)
                })
                .await?
            }
            _ => false,
        };
        {
            let mut model = self.model();
            if let Some(local) = model.local.get_mut(id) {
                local.long_context = long_context;
            }
        }
        self.save_local(id);
        // Another agent's `pi` would take crew's arguments for prompts:
        // pi's own program is looked for (and may be named by CREW_PI).
        let pi_program = if agent.runtime == RuntimeKind::Pi {
            Some(
                tokio::task::spawn_blocking(crate::agents::pi::program)
                    .await?
                    .map_err(|e| anyhow!(e))?,
            )
        } else {
            None
        };
        let mut spec = runtime.launch(&LaunchContext {
            agent: &agent,
            settings: &local.settings,
            crew_bin: &self.crew_bin,
            agent_dir: &agent_dir,
            identity_path: &identity_path,
            briefing: &briefing,
            resume,
            endpoint: &endpoint,
            long_context,
            private_home: private_home.as_deref(),
        })?;
        if let Some(program) = pi_program {
            spec.program = program.to_string_lossy().into_owned();
        }
        if let Some(origin) = &endpoint.origin {
            self.emit(
                NewEvent::new("daemon", "agent.endpoint", format!("{id} runs on {origin}"))
                    .agent(id),
            );
        }
        if let Some(model) = &endpoint.model {
            self.update_agent(id, |a| a.model = Some(model.clone()));
        }

        // Before anything starts: an agent that must be sandboxed and cannot
        // be starts nothing.
        // Each runs the CLI file its profile bound, by its absolute path.
        if let Some(sandboxed) = self
            .agent_sandbox(&agent, Run::session(&spec, procs.name()))
            .await?
        {
            if let Some(program) = sandboxed.program {
                spec.program = program.to_string_lossy().into_owned();
            }
            spec.sandbox = Some(sandboxed.spec);
        }
        let sidecar_sandbox = match spec.sidecar.as_mut() {
            Some(sidecar) => {
                let run = Run::helper(id, &sidecar.program, &sidecar.env, &sidecar.socket);
                self.agent_sandbox(&agent, run).await?.map(|sandboxed| {
                    if let Some(program) = sandboxed.program {
                        sidecar.program = program.to_string_lossy().into_owned();
                    }
                    sandboxed.spec
                })
            }
            None => None,
        };
        if private_home.is_some() {
            // D2: written through the agent root descriptor, after the previous
            // sandboxes were retired (at the start of this launch, under its lock).
            crate::agents::home::prepare(
                agent.runtime,
                std::os::fd::AsFd::as_fd(&agent_root),
                &crate::agents::home::Operator::detect(&endpoint),
            )?;
        } else {
            self.stop_sidecar(id);
        }
        if stopped() {
            return Ok(());
        }
        if let Some(sidecar) = &spec.sidecar {
            self.start_sidecar(id, sidecar, sidecar_sandbox.as_ref())
                .await?;
            // Stopped while it waited for its sidecar: that goes too.
            if stopped() {
                self.stop_sidecar(id);
                return Ok(());
            }
        }

        // First prompt: whatever is already waiting for the agent, so it
        // starts on its task instead of spending a turn on small talk.
        let scripted = agent.runtime == RuntimeKind::Shell;
        let pending = if scripted {
            Vec::new()
        } else {
            self.bus.undelivered(id).unwrap_or_default()
        };
        let pending_ids: Vec<String> = pending.iter().map(|m| m.id.clone()).collect();
        let mut body = String::new();
        if resume {
            body.push_str(notice.as_deref().unwrap_or(RESUME_NOTICE));
        } else if let Some(handoff) = local.handoff.as_deref().filter(|_| !scripted) {
            // A fresh session on another CLI picks up from the old one.
            body.push_str(handoff);
        }
        if !pending.is_empty() {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(&runtime.nudge(&pending));
        }
        if body.is_empty() && !scripted {
            body = if agent.orchestrator {
                let members = agents.iter().filter(|a| !a.orchestrator).count();
                // What each model has left decides who goes on it.
                self.refresh_claude_usage_now().await;
                let usage = self.usage_report().await;
                prompt::orchestrator_kickoff(&tasks, members, &usage)
            } else {
                prompt::standby_prompt(&agent)
            };
        }
        let first_prompt = (!body.is_empty()).then_some(body);
        spec.prompt = first_prompt.clone();

        let now = now_ms();
        {
            let mut model = self.model();
            model.rt.insert(
                id.to_string(),
                AgentRuntimeState {
                    spawned_at: now,
                    ..AgentRuntimeState::default()
                },
            );
        }
        // The team was stopped while this start was being prepared.
        let stopping = || self.stopping_team.load(std::sync::atomic::Ordering::SeqCst) || stopped();
        if stopping() {
            self.stop_sidecar(id);
            return Ok(());
        }
        self.update_agent(id, |a| {
            a.status = AgentStatus::Spawning;
            a.activity = "starting".into();
            a.error = None;
            a.exit_code = None;
            a.paused = false;
            a.block_kind = None;
            a.started_at = Some(now);
            a.last_activity_at = now;
            a.backend = procs.name().to_string();
            a.delivery = if spec.sidecar.is_some() {
                "socket"
            } else {
                "terminal"
            }
            .to_string();
            a.subagents = 0;
        });

        // In the ledger, on disk, before it starts: its sandbox reports there
        // when it is gone. A spawn that fails leaves the entry: it is not
        // known whether a process was made.
        let record = match spec.sandbox.as_mut() {
            Some(sandbox) => {
                let record = self.ledger.start(id, "session")?;
                sandbox.done = Some(record.done().to_path_buf());
                Some(record)
            }
            None => None,
        };
        let spawned = procs.spawn(&spec).await?;
        if let Some(record) = &record {
            record.update(|r| r.pid = spawned.pid);
            // Its init is needed to know that it is gone if it is killed,
            // before anything kills it (a team stop included).
            // Named by the backend before anything could reap it.
            let found = match (spawned.pid, spawned.start) {
                (Some(pid), Some(start)) => {
                    let wrapper = super::retire::Proc { pid, start };
                    super::retire::discover(wrapper, super::retire::DISCOVER_WITHIN).await
                }
                _ => Err(anyhow!("its process could not be identified")),
            };
            match found {
                Ok(init) => record.update(|r| r.init = init),
                Err(e) => {
                    let _ = procs.kill(id).await;
                    anyhow::bail!("{id}'s session was stopped: {e}");
                }
            }
        }
        if stopping() {
            // Stopped while the process was coming up: it goes too.
            let _ = procs.kill(id).await;
            self.stop_sidecar(id);
            self.update_agent(id, |a| {
                a.status = AgentStatus::Stopped;
                a.pid = None;
                a.activity = "stopped".into();
            });
            return Ok(());
        }
        self.update_agent(id, |a| {
            a.pid = spawned.pid;
            if spawned.backend_ref.is_some() {
                a.backend_ref = spawned.backend_ref.clone();
            }
        });
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.spawned",
                format!(
                    "{id} started ({} on {}{})",
                    agent.runtime,
                    procs.name(),
                    spawned
                        .pid
                        .map(|p| format!(", pid {p}"))
                        .unwrap_or_default()
                ),
            )
            .agent(id),
        );
        if let Some(workspace) = procs.session_ref() {
            let changed = {
                let mut model = self.model();
                let changed = model.session.herdr_workspace.as_deref() != Some(workspace.as_str());
                model.session.herdr_workspace = Some(workspace);
                changed
            };
            if changed {
                self.save_session();
            }
        }

        // Its sandboxes are started and in the ledger: what follows may
        // deliver to it, through a queue command that shares the lease.
        drop(launch);
        if spawned.prompt_delivered {
            if !pending_ids.is_empty() {
                let _ = self.bus.mark_delivered(id, &pending_ids);
            }
            {
                let mut model = self.model();
                let rt = model.rt(id);
                rt.turn_active = true;
                rt.turn_messages.extend(pending_ids.iter().cloned());
                rt.turn_openers.extend(pending_ids.iter().cloned());
            }
            self.update_agent(id, |a| {
                a.status = AgentStatus::Working;
                a.activity.clear();
            });
        } else if let Some(prompt) = first_prompt {
            self.model().rt(id).pending_prompt = Some((prompt, pending_ids));
        }

        if spawned.ready {
            self.on_signal(id, Signal::Ready).await;
        } else {
            self.watch_startup(id, agent.runtime);
        }
        Ok(())
    }

    /// Start the helper process an agent's session depends on and wait until
    /// its socket is up.
    async fn start_sidecar(
        &self,
        id: &str,
        sidecar: &crate::agents::Sidecar,
        sandbox: Option<&crate::sandbox::spec::Spec>,
    ) -> Result<()> {
        use anyhow::Context;
        let log_path = self.agent_dir(id).join("sidecar.log");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("opening {}", log_path.display()))?;
        let argv: Vec<String> = std::iter::once(sidecar.program.clone())
            .chain(sidecar.args.iter().cloned())
            .collect();
        // In the ledger, on disk, before it starts: its sandbox reports there
        // when it is gone.
        let record = sandbox
            .map(|_| self.ledger.start(id, "sidecar"))
            .transpose()?;
        let marked = sandbox.zip(record.as_ref()).map(|(sandbox, record)| {
            let mut sandbox = sandbox.clone();
            sandbox.done = Some(record.done().to_path_buf());
            sandbox
        });
        let mut cmd = super::sandbox::command(marked.as_ref(), &argv, &sidecar.env)?;
        cmd.current_dir(&sidecar.cwd)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            // The sidecar must not outlive the daemon that owns it.
            .kill_on_drop(true);
        let spawned = cmd.spawn();
        if let (Err(_), Some(record)) = (&spawned, &record) {
            self.ledger.never_started(record);
        }
        let mut child = spawned
            .with_context(|| format!("starting `{} {}`", sidecar.program, sidecar.args[0]))?;
        if let (Some(record), Some(pid)) = (&record, child.id()) {
            record.update(|r| r.pid = Some(pid));
            // Its init, before anything can kill it. The child is not
            // waited for until after this, so the number is its own.
            let found = match super::retire::Proc::of(pid) {
                Some(wrapper) => {
                    super::retire::discover(wrapper, super::retire::DISCOVER_WITHIN).await
                }
                None => Err(anyhow!("its process could not be identified")),
            };
            match found {
                Ok(init) => record.update(|r| r.init = init),
                Err(e) => {
                    reap_sidecar(child);
                    anyhow::bail!("`{} {}`: {e}", sidecar.program, sidecar.args[0]);
                }
            }
        }
        for _ in 0..150 {
            if sidecar.socket.exists() {
                self.sidecars
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        id.to_string(),
                        super::SidecarProc {
                            child,
                            socket: sidecar.socket.clone(),
                            env: sidecar.env.clone(),
                        },
                    );
                return Ok(());
            }
            if let Some(status) = child.try_wait()? {
                anyhow::bail!(
                    "`{} {}` exited during start-up ({status}); see {}",
                    sidecar.program,
                    sidecar.args[0],
                    log_path.display()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        reap_sidecar(child);
        anyhow::bail!(
            "`{} {}` did not open its socket within 15s; see {}",
            sidecar.program,
            sidecar.args[0],
            log_path.display()
        )
    }

    /// Wait until none of an agent's previous sandboxes is left, by its
    /// ledger (daemon::retire): the session must have exited, the sidecar
    /// is stopped too, and each sandbox, whoever started it, must be known
    /// to be gone. Before this nothing of the agent's next launch is
    /// written (design-b3.md §3.4, D2).
    pub(super) async fn retire_sandboxes(
        &self,
        id: &str,
        procs: &dyn ProcessManager,
        within: std::time::Duration,
    ) -> Result<()> {
        if procs.is_alive(id) {
            anyhow::bail!(
                "{id}'s previous session is still running; its home is written only once it has \
stopped"
            );
        }
        self.stop_sidecar(id);
        // Under the agent's launch lease, held alone: whatever of its
        // sandboxes still runs is of a launch that has ended.
        let ended = self.ledger.end_orphans(id);
        if ended > 0 {
            tracing::info!("{id}: ended {ended} sandbox(es) its stopped processes left running");
        }
        self.ledger
            .retire(id, within)
            .await
            .map_err(|e| anyhow!("{id}'s home is not written: {e}"))
    }

    /// Agent `id`'s launch lease. A launch holds it alone, from before the
    /// agent's previous sandboxes are retired until its own have started.
    /// The agent's git jobs and queue commands share it while their
    /// sandboxes run, the guard owned by the job itself, so a request that
    /// is cancelled does not let a launch in beside a job still running.
    pub(crate) fn launch_lease(&self, id: &str) -> std::sync::Arc<tokio::sync::RwLock<()>> {
        self.launches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    pub(crate) fn stop_sidecar(&self, id: &str) {
        let removed = self
            .sidecars
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(sidecar) = removed {
            reap_sidecar(sidecar.child);
        }
    }

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

    /// Shepherd a session through start-up: answer or report dialogs that
    /// appear in front of it, and — for runtimes without a "ready" hook, or
    /// when the hook never fires — treat it as ready once its output settles.
    fn watch_startup(&self, id: &str, kind: RuntimeKind) {
        let daemon = self.clone();
        let id = id.to_string();
        let (min_ms, max_ms) = match kind {
            RuntimeKind::Shell => (300, 5_000),
            RuntimeKind::Codex => (3_000, 45_000),
            // pi's extension speaks before its input is on screen.
            RuntimeKind::Pi => (3_000, 45_000),
            // Claude reports readiness through its SessionStart hook.
            RuntimeKind::Claude => (12_000, 60_000),
        };
        tokio::spawn(async move {
            let started = now_ms();
            let mut ticks = 0u32;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                ticks += 1;
                let (ready, spawned_at, last_output, gone) = {
                    let mut model = daemon.model();
                    let gone = model.agent(&id).is_none_or(|a| a.status.is_terminal());
                    let rt = model.rt(&id);
                    (rt.ready, rt.spawned_at, rt.last_output_at, gone)
                };
                if ready || gone || spawned_at > started {
                    return; // ready, stopped, or superseded by a newer spawn
                }
                // Look for a dialog about once a second. This matters most on
                // backends that send no output ticks (herdr).
                if kind != RuntimeKind::Shell && ticks % 4 == 0 {
                    daemon.check_startup_dialog(&id).await;
                }
                let blocked = daemon
                    .agent(&id)
                    .is_none_or(|a| a.block_kind.as_deref() == Some("dialog"));
                if blocked {
                    continue; // until somebody answers it
                }
                let elapsed = now_ms() - started;
                let quiet = last_output == 0 || now_ms() - last_output > 1_500;
                if (elapsed >= min_ms && quiet) || elapsed >= max_ms {
                    // `on_ready` re-checks for a dialog and may decline.
                    daemon.on_signal(&id, Signal::Ready).await;
                }
            }
        });
    }

    // ---- signals --------------------------------------------------------

    pub(crate) async fn on_hook(&self, id: &str, event: &str, payload: &Value) {
        let Some(agent) = self.agent(id) else { return };
        for signal in runtime_for(agent.runtime).on_hook(event, payload) {
            self.on_signal(id, signal).await;
        }
    }

    /// Where an agent settles when its turn ends.
    fn resting_status(&self, id: &str) -> AgentStatus {
        let mut model = self.model();
        if let Some(status) = model.rt(id).self_status {
            return status;
        }
        let Some(agent) = model.agent(id) else {
            return AgentStatus::Idle;
        };
        if agent.orchestrator {
            return if model.session.summary.is_some() {
                AgentStatus::Done
            } else if model.session.team.is_some() {
                AgentStatus::Waiting
            } else {
                AgentStatus::Idle
            };
        }
        if active_task(&model.tasks, id).is_some() {
            return AgentStatus::Idle;
        }
        let open = model
            .tasks
            .iter()
            .any(|t| t.assigned_agent.as_deref() == Some(id) && !t.status.is_terminal());
        let completed = model.local.get(id).map(|l| l.tasks_completed).unwrap_or(0);
        if !open && completed > 0 {
            AgentStatus::Done
        } else {
            AgentStatus::Idle
        }
    }

    /// Re-evaluate an agent's status outside of a turn (e.g. after a review).
    pub(crate) fn settle(&self, id: &str) {
        if self.model().rt(id).turn_active {
            return;
        }
        let Some(agent) = self.agent(id) else { return };
        if matches!(
            agent.status,
            AgentStatus::Spawning | AgentStatus::Failed | AgentStatus::Stopped
        ) || matches!(agent.block_kind.as_deref(), Some("permission" | "dialog"))
        {
            return;
        }
        let status = self.resting_status(id);
        self.update_agent(id, |a| {
            a.status = status;
            if status != AgentStatus::Blocked {
                a.block_kind = None;
            }
        });
    }

    pub(crate) async fn on_signal(&self, id: &str, signal: Signal) {
        let now = now_ms();
        // A session parked at a usage limit that shows signs of life went on
        // by itself (the runtime retried, or someone typed into it).
        if matches!(
            signal,
            Signal::TurnStarted | Signal::Activity(_) | Signal::TurnEnded
        ) {
            self.limit_lifted(id, "the session went on");
        }
        if matches!(signal, Signal::TurnEnded) {
            self.model().rt(id).limit_attempts = 0;
        }
        match signal {
            // An account without the long-context beta refuses every request
            // of a session started in the 1M variant; that is not a limit to
            // wait out.
            Signal::Limited(info) if self.long_context_refused(id, &info.message) => {}
            Signal::Limited(info) => self.agent_limited(id, info).await,
            Signal::Quota(readings) => {
                if let Some(agent) = self.agent(id) {
                    self.record_quota(agent.runtime, readings);
                }
            }
            Signal::Model { model, effort } => self.record_running(id, model, effort),
            Signal::Context { tokens, window } => {
                self.model().rt(id).context_reported = true;
                self.record_context(id, tokens, window).await;
            }
            Signal::Compacted => self.context_compacted(id).await,
            Signal::CompactSkipped(why) => self.compact_skipped(id, &why),
            Signal::TurnFailed(what) => {
                {
                    let mut model = self.model();
                    let rt = model.rt(id);
                    rt.turn_failure = Some(what.clone());
                    rt.provider_error = Some(what.clone());
                }
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "agent.turn_failed",
                        format!("{id}: the turn ended in an error: {what}"),
                    )
                    .agent(id)
                    .warn(),
                );
                self.turn_ended(id, now);
                self.long_context_refused(id, &what);
            }
            Signal::Ready => self.on_ready(id).await,
            Signal::TurnStarted => {
                {
                    let mut model = self.model();
                    let rt = model.rt(id);
                    rt.turn_active = true;
                    rt.self_status = None;
                    rt.idle_since = None;
                }
                self.update_agent(id, |a| {
                    if !a.status.is_terminal() {
                        a.status = AgentStatus::Working;
                        a.block_kind = None;
                    }
                    a.last_activity_at = now;
                });
            }
            Signal::TurnEnded => {
                self.model().rt(id).provider_error = None;
                self.turn_ended(id, now);
            }
            // Parked at a usage limit: a notification changes nothing.
            Signal::Blocked(_) if self.agent(id).is_some_and(|a| a.limit.is_some()) => {}
            Signal::Blocked(reason) => {
                let changed = self
                    .agent(id)
                    .is_some_and(|a| a.status != AgentStatus::Blocked);
                self.update_agent(id, |a| {
                    if !a.status.is_terminal() {
                        a.status = AgentStatus::Blocked;
                        a.block_kind = Some("permission".into());
                        a.activity = reason.clone();
                    }
                });
                if changed {
                    self.emit(
                        NewEvent::new(
                            "daemon",
                            "agent.blocked",
                            format!("{id} needs a human: {reason}"),
                        )
                        .agent(id)
                        .warn(),
                    );
                }
            }
            Signal::Unblocked => {
                let was_blocked = self.agent(id).is_some_and(|a| {
                    a.status == AgentStatus::Blocked
                        && matches!(
                            a.block_kind.as_deref(),
                            Some("permission" | "dialog" | "unresponsive")
                        )
                });
                if was_blocked {
                    self.update_agent(id, |a| {
                        a.status = AgentStatus::Working;
                        a.block_kind = None;
                        a.last_activity_at = now;
                    });
                    self.request_delivery(id);
                } else {
                    self.touch(id, now);
                }
            }
            Signal::Activity(text) => {
                self.update_agent(id, |a| {
                    a.activity = text;
                    a.last_activity_at = now;
                    if a.status == AgentStatus::Blocked
                        && matches!(a.block_kind.as_deref(), Some("permission" | "unresponsive"))
                    {
                        a.status = AgentStatus::Working;
                        a.block_kind = None;
                    }
                });
            }
            Signal::Session {
                id: session_id,
                transcript,
            } => {
                {
                    let mut model = self.model();
                    if transcript.is_some() {
                        model.rt(id).transcript = transcript;
                    }
                    if session_id.is_some() {
                        let local = model.local.entry(id.to_string()).or_default();
                        local.session_confirmed = true;
                        // The new session has been handed the work; a
                        // restart from here resumes it instead.
                        local.handoff = None;
                    }
                }
                if let Some(session_id) = session_id {
                    self.save_local(id);
                    self.update_agent(id, |a| a.session_id = Some(session_id));
                }
            }
            Signal::Subagents(delta) => {
                self.update_agent(id, |a| {
                    a.subagents = if delta >= 0 {
                        a.subagents.saturating_add(delta as u32)
                    } else {
                        a.subagents.saturating_sub(delta.unsigned_abs())
                    };
                    a.last_activity_at = now;
                });
            }
            Signal::Exiting => {}
        }
    }

    /// A turn is over (it completed, or it failed): the agent is at rest.
    fn turn_ended(&self, id: &str, now: i64) {
        let (transcript, asked) = {
            let mut model = self.model();
            let rt = model.rt(id);
            rt.turn_active = false;
            rt.turn_ended_at = now;
            rt.idle_since = Some(now);
            // What this turn was given; what is delivered from here on is the next one's.
            let given = (
                std::mem::take(&mut rt.turn_messages),
                std::mem::take(&mut rt.turn_openers),
            );
            (rt.transcript.clone(), given)
        };
        let status = self.resting_status(id);
        self.update_agent(id, |a| {
            if !a.status.is_terminal() {
                a.status = status;
                if status != AgentStatus::Blocked {
                    a.block_kind = None;
                }
                if status != AgentStatus::Working {
                    a.activity.clear();
                }
            }
            a.last_activity_at = now;
        });
        if let Some(transcript) = transcript {
            self.refresh_usage(id, transcript);
        }
        self.request_delivery(id);
        self.replan();
        self.retire_if_finished(id);
        if self.is_orchestrator(id) {
            self.found_team_once_planned();
        }
        // A compaction queued for this agent has its moment now.
        let daemon = self.clone();
        let agent = id.to_string();
        tokio::spawn(async move { daemon.try_compact(&agent).await });
        // Did the turn do what it was asked, or did the model refuse?
        let daemon = self.clone();
        let agent = id.to_string();
        tokio::spawn(async move { daemon.check_turn(&agent, asked.0, asked.1).await });
    }

    fn touch(&self, id: &str, now: i64) {
        self.update_agent(id, |a| a.last_activity_at = now);
    }

    async fn on_ready(&self, id: &str) {
        // A dialog (workspace trust) may be sitting in front of the session;
        // typing a prompt into it would answer the dialog with garbage.
        if self.dialog_blocks(id).await {
            return;
        }
        let pending = {
            let mut model = self.model();
            let rt = model.rt(id);
            rt.ready = true;
            rt.pending_prompt.take()
        };
        let kind = self.agent(id).map(|a| a.runtime);
        // Prefer the runtime's socket channel for the first prompt too: it
        // does not depend on the terminal host considering the session ready.
        let wants_socket = kind == Some(RuntimeKind::Claude)
            && self
                .model()
                .local
                .get(id)
                .is_some_and(|l| l.settings.socket_delivery);
        let mut pending = pending;
        if let (true, Some((prompt, ids))) = (wants_socket, pending.clone()) {
            for _ in 0..20 {
                if self.listener(id).is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            if let Some((_, tx)) = self.listener(id) {
                let frame = crew_core::rpc::ServerFrame::push(crew_core::rpc::Push::Deliver {
                    ids: ids.clone(),
                    text: prompt,
                });
                if tx.send(frame).await.is_ok() {
                    pending = None;
                    let now = now_ms();
                    {
                        let mut model = self.model();
                        let rt = model.rt(id);
                        rt.turn_active = true;
                        rt.turn_openers.extend(ids.iter().cloned());
                        for message in &ids {
                            rt.in_flight.insert(message.clone(), now);
                        }
                    }
                    self.update_agent(id, |a| {
                        a.status = AgentStatus::Working;
                        a.activity.clear();
                    });
                }
            }
        }
        if let Some((prompt, ids)) = pending {
            let lock = self.delivery_lock(id);
            let _guard = lock.lock().await;
            match self.procs() {
                Ok(procs) => match procs.submit_prompt(id, &prompt).await {
                    Ok(()) => {
                        if !ids.is_empty() {
                            let _ = self.bus.mark_delivered(id, &ids);
                        }
                        {
                            let mut model = self.model();
                            let rt = model.rt(id);
                            rt.turn_messages.extend(ids.iter().cloned());
                            rt.turn_openers.extend(ids.iter().cloned());
                        }
                        if kind != Some(RuntimeKind::Shell) {
                            self.model().rt(id).turn_active = true;
                            self.update_agent(id, |a| {
                                a.status = AgentStatus::Working;
                                a.activity.clear();
                            });
                        }
                        tokio::time::sleep(DELIVERY_GAP).await;
                    }
                    Err(e) => self.emit(
                        NewEvent::new(
                            "daemon",
                            "agent.prompt_failed",
                            format!("could not submit the first prompt to {id}: {e:#}"),
                        )
                        .agent(id)
                        .warn(),
                    ),
                },
                Err(e) => tracing::warn!("{}", e.message),
            }
        }
        let turn_active = self.model().rt(id).turn_active;
        if !turn_active {
            let status = self.resting_status(id);
            self.update_agent(id, |a| {
                if a.status == AgentStatus::Spawning {
                    a.status = status;
                    a.activity.clear();
                }
            });
        }
        self.request_delivery(id);
        self.replan();
    }

    /// Check a starting session for a dialog, and resume start-up once a
    /// dialog that was blocking it is gone.
    pub(crate) async fn check_startup_dialog(&self, id: &str) {
        let (ready, was_blocked) = {
            let mut model = self.model();
            let was_blocked = model
                .agent(id)
                .is_some_and(|a| a.block_kind.as_deref() == Some("dialog"));
            (model.rt(id).ready, was_blocked)
        };
        if ready && !was_blocked {
            return;
        }
        let blocked = self.dialog_blocks(id).await;
        if was_blocked && !blocked {
            // The dialog was answered; pick up where start-up left off.
            let (has_prompt, turn_active) = {
                let mut model = self.model();
                let rt = model.rt(id);
                (rt.pending_prompt.is_some(), rt.turn_active)
            };
            if has_prompt {
                self.on_signal(id, Signal::Ready).await;
            } else if turn_active {
                self.update_agent(id, |a| {
                    if a.status == AgentStatus::Spawning {
                        a.status = AgentStatus::Working;
                        a.activity.clear();
                    }
                });
            }
        }
    }

    /// Dismiss a menu on `screen` that needs no answer (Codex's "extra
    /// thought" notice), so it holds no keys and blocks nothing. Returns
    /// whether there was one.
    pub(crate) async fn dismiss_notice(&self, id: &str, screen: &str) -> bool {
        let Some(agent) = self.agent(id) else {
            return false;
        };
        let Some(notice) = runtime_for(agent.runtime)
            .detect_dialog(screen)
            .filter(|d| d.kind == "notice")
        else {
            return false;
        };
        let Ok(procs) = self.procs() else {
            return false;
        };
        if procs.send_keys(id, &notice.accept_keys).await.is_ok() {
            self.emit(
                NewEvent::new(
                    "daemon",
                    "agent.notice_dismissed",
                    format!("{id}: dismissed — {}", notice.description),
                )
                .agent(id),
            );
        }
        true
    }

    /// Look for a blocking dialog on the agent's screen. Returns true when the
    /// session must not receive input yet.
    async fn dialog_blocks(&self, id: &str) -> bool {
        // One dialog handler per agent at a time: two of them answering the
        // same dialog would press its keys twice and pick the wrong option.
        let lock = self.delivery_lock(&format!("dialog:{id}"));
        let _guard = lock.lock().await;
        let Some(agent) = self.agent(id) else {
            return true;
        };
        let Ok(procs) = self.procs() else { return true };
        let runtime = runtime_for(agent.runtime);
        let auto_trust = self
            .model()
            .local
            .get(id)
            .map(|l| l.settings.auto_trust)
            .unwrap_or(false);
        for attempt in 0..4 {
            let screen = procs.visible(id, 80).await.unwrap_or_default();
            let Some(dialog) = runtime.detect_dialog(&screen) else {
                let was_reported = std::mem::take(&mut self.model().rt(id).dialog_reported);
                if was_reported {
                    self.update_agent(id, |a| {
                        if a.block_kind.as_deref() == Some("dialog") {
                            a.block_kind = None;
                            a.status = AgentStatus::Spawning;
                            a.activity = "starting".into();
                        }
                    });
                }
                return false;
            };
            match dialog.accept_keys.as_slice() {
                keys if dialog.kind == "notice" && !keys.is_empty() && attempt < 3 => {
                    if attempt == 0 {
                        self.emit(
                            NewEvent::new(
                                "daemon",
                                "agent.notice_dismissed",
                                format!("{id}: dismissed — {}", dialog.description),
                            )
                            .agent(id),
                        );
                    }
                    for key in keys {
                        let _ = procs.send_keys(id, &[key]).await;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                }
                keys if auto_trust && dialog.kind == "trust" && !keys.is_empty() && attempt < 3 => {
                    if attempt == 0 {
                        self.emit(
                            NewEvent::new(
                                "daemon",
                                "agent.dialog_accepted",
                                format!(
                                    "{id}: accepted the workspace-trust dialog (auto_trust is on)"
                                ),
                            )
                            .agent(id),
                        );
                    }
                    for key in keys {
                        let _ = procs.send_keys(id, &[key]).await;
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                }
                _ => {
                    let already = std::mem::replace(&mut self.model().rt(id).dialog_reported, true);
                    if !already {
                        self.update_agent(id, |a| {
                            a.status = AgentStatus::Blocked;
                            a.block_kind = Some("dialog".into());
                            a.activity = dialog.description.clone();
                        });
                        self.emit(
                            NewEvent::new(
                                "daemon",
                                "agent.dialog",
                                format!(
                                    "{id}: {}. Answer it in the agent's terminal, or set \
defaults.auto_trust: true in the team file.",
                                    dialog.description
                                ),
                            )
                            .agent(id)
                            .warn(),
                        );
                    }
                    return true;
                }
            }
        }
        true
    }

    fn refresh_usage(&self, id: &str, transcript: String) {
        let Some(agent) = self.agent(id) else { return };
        let runtime = runtime_for(agent.runtime);
        let daemon = self.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            if let Some(usage) = runtime.usage(std::path::Path::new(&transcript)) {
                daemon.update_agent(&id, |a| a.usage = Some(usage));
            }
        });
    }

    // ---- process events -------------------------------------------------

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
    async fn requeue_if_sessionless(&self, id: &str) {
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
    async fn requeue_forgotten_work(&self, id: &str) {
        let (task, reviews) = {
            let model = self.model();
            (
                active_task(&model.tasks, id).map(|t| t.id.clone()),
                model
                    .tasks
                    .iter()
                    .filter(|t| t.status == TaskStatus::Review && t.reviewer.as_deref() == Some(id))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        if let Some(task) = task {
            self.update_task(&task, |t| {
                t.status = TaskStatus::Ready;
                t.started_at = None;
                t.note = Some("re-dispatched after the agent restarted".into());
            });
        }
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

    // ---- operator actions ----------------------------------------------

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
            rt.expected_exit = procs.is_alive(id);
            rt.ready = false;
            rt.turn_active = false;
            rt.pending_prompt = None;
        }
        procs.kill(id).await.map_err(internal)?;
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
        if procs.is_alive(id) {
            self.model().rt(id).expected_exit = true;
            procs.kill(id).await.map_err(internal)?;
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

    // ---- time-based checks ---------------------------------------------

    pub(crate) async fn housekeeping(&self) {
        let now = now_ms();
        let Some(limits) = self.model().session.team.as_ref().map(|t| t.limits.clone()) else {
            return;
        };
        self.limit_housekeeping().await;
        self.context_housekeeping().await;
        self.transcript_housekeeping().await;
        self.temporary_housekeeping().await;

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
                    if now - started > (secs as i64) * 1000 {
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

    // ---- recovery -------------------------------------------------------

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
            } || super::delivery::wakes(
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

    /// The id the scheduler signs task messages with.
    pub(crate) fn dispatcher_for(&self, agent: &str) -> &'static str {
        if agent == ORCHESTRATOR_ID {
            "scheduler"
        } else {
            ORCHESTRATOR_ID
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::LaunchSpec;
    use crate::process::Spawned;
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
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
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
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
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
