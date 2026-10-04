//! Launching an agent (old sandboxes retired, home seeded, sidecar, process) and stopping it.

use anyhow::{anyhow, Result};
use crew_core::paths::{write_private_file, Identity};
use crew_core::util::now_ms;
use crew_core::{AgentStatus, RuntimeKind};

use super::{chosen_session_id, Start};
use crate::agents::prompt::{self, TeamView};
use crate::agents::{runtime_for, LaunchContext, Signal};
use crate::daemon::sandbox::Run;
use crate::daemon::{AgentRuntimeState, Daemon};
use crate::persistence::NewEvent;
use crate::process::ProcessManager;

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

impl Daemon {
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
    pub(super) fn stop_requested(&self, id: &str) -> u64 {
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
        let launch_id = self.begin_launch(id).await;
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
            vcs: self.vcs().kind(),
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
        // The copy of crew it is handed is still the one made, and is
        // recorded with it: a process of another build's is not adopted
        // after a daemon restart (daemon::binary).
        let daemon = self.clone();
        tokio::task::spawn_blocking(move || daemon.check_binary()).await??;
        {
            let mut model = self.model();
            if let Some(local) = model.local.get_mut(id) {
                local.long_context = long_context;
                local.binary = self.binary.name().map(str::to_string);
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
        // Before the sandboxes are made: they pass on what the launch's
        // environment names.
        spec.env.push(("CREW_LAUNCH".into(), launch_id.clone()));
        if worktrees && self.vcs().kind() == crate::vcs::Kind::Jj {
            // jj run by the agent itself records its name, as the crew tools do.
            spec.env.push(("JJ_USER".into(), id.to_string()));
            spec.env
                .push(("JJ_EMAIL".into(), format!("{id}@crew.local")));
        }
        if let Some(sidecar) = spec.sidecar.as_mut() {
            sidecar.env.push(("CREW_LAUNCH".into(), launch_id.clone()));
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
                    let wrapper = super::super::retire::Proc { pid, start };
                    super::super::retire::discover(wrapper, super::super::retire::DISCOVER_WITHIN)
                        .await
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
        let mut cmd = super::super::sandbox::command(marked.as_ref(), &argv, &sidecar.env)?;
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
            let found = match super::super::retire::Proc::of(pid) {
                Some(wrapper) => {
                    super::super::retire::discover(wrapper, super::super::retire::DISCOVER_WITHIN)
                        .await
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
                        super::super::SidecarProc {
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
    pub(in crate::daemon) async fn retire_sandboxes(
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
}
