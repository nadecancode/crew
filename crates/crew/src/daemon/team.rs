//! Launching a team from its declarative configuration.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use crew_core::config::{AgentSpec, BackendChoice, Isolation, SandboxMode, TeamConfig};
use crew_core::paths::{self, DaemonInfo};
use crew_core::rpc::RpcError;
use crew_core::util::now_ms;
use crew_core::ORCHESTRATOR_ID;

use super::handlers::internal;
use super::orchestrate::check_task_message_size;
use super::{Daemon, Model, SessionState};
use crate::git;
use crate::persistence::NewEvent;
use crate::process::herdr::{self, Herdr, HerdrManager};
use crate::process::pty::PtyManager;
use crate::process::ProcessManager;
use crate::vcs;

impl Daemon {
    /// Construct the process backend for a session.
    pub(crate) fn build_backend(
        &self,
        backend: &str,
        team: &TeamConfig,
        herdr_socket: Option<&str>,
        herdr_workspace: Option<String>,
    ) -> Result<Arc<dyn ProcessManager>> {
        match backend {
            "herdr" => {
                let socket =
                    herdr_socket.ok_or_else(|| anyhow!("the herdr socket path is unknown"))?;
                let label = team
                    .herdr
                    .workspace_label
                    .clone()
                    .unwrap_or_else(|| format!("crew:{}", team.project));
                Ok(Arc::new(Herdr(Arc::new(HerdrManager::new(
                    Path::new(socket),
                    &label,
                    team.herdr.layout,
                    herdr_workspace,
                    self.proc_tx.clone(),
                )))))
            }
            "pty" => Ok(Arc::new(PtyManager::new(self.proc_tx.clone()))),
            other => bail!("unknown backend '{other}'"),
        }
    }

    pub(crate) async fn load_team(
        &self,
        config: TeamConfig,
        herdr_socket: Option<String>,
    ) -> Result<(), RpcError> {
        config
            .validate()
            .map_err(|e| RpcError::invalid(e.to_string()))?;
        for spec in config.all_tasks() {
            crate::tasks::check_reviewer(&spec).map_err(RpcError::invalid)?;
            let task = crate::tasks::task_from_spec(&spec, 0);
            check_task_message_size(&task, config.limits.max_message_bytes)?;
        }

        // One team per workspace at a time. The lock is held until the new
        // team is in place, so a second load sees it.
        let _lifecycle = self.team_lock.lock().await;
        {
            let model = self.model();
            if let Some(team) = &model.session.team {
                let procs = self.procs.read().unwrap_or_else(|e| e.into_inner()).clone();
                // An agent that is still starting counts: its process may
                // not exist yet.
                let alive = model.agents.iter().any(|a| {
                    a.status == crew_core::AgentStatus::Spawning
                        || procs.as_ref().is_some_and(|p| p.is_alive(&a.id))
                });
                if alive {
                    return Err(RpcError::conflict(format!(
                        "team '{}' is already running in this workspace; use `crew attach`, or \
`crew stop` first",
                        team.project
                    )));
                }
            }
        }

        // Workspace isolation, and the version control that provides it.
        let isolated = {
            let (workspace, state_dir) = (self.workspace.clone(), self.state_dir.clone());
            let workspace_config = config.workspace.clone();
            tokio::task::spawn_blocking(move || isolate(&workspace, &state_dir, &workspace_config))
                .await
                .map_err(internal)?
                .map_err(|e| RpcError::invalid(format!("{e:#}")))?
        };
        let Isolated {
            selection,
            shared_note: note,
        } = isolated;
        let worktrees = selection.is_some();
        let vcs_kind = selection
            .as_ref()
            .map(|s| s.vcs.kind().as_str())
            .unwrap_or_default();

        // Process backend.
        let herdr_socket = herdr_socket.filter(|s| !s.is_empty());
        let backend = match config.backend {
            BackendChoice::Pty => "pty",
            BackendChoice::Herdr => {
                let socket = herdr_socket.as_deref().ok_or_else(|| {
                    RpcError::invalid(
                        "backend: herdr needs to be started from inside a herdr pane (HERDR_SOCKET_PATH)",
                    )
                })?;
                if !herdr::reachable(Path::new(socket)).await {
                    return Err(RpcError::unavailable(format!(
                        "herdr is not reachable at {socket}"
                    )));
                }
                "herdr"
            }
            BackendChoice::Auto => match herdr_socket.as_deref() {
                Some(socket) if herdr::reachable(Path::new(socket)).await => "herdr",
                _ => "pty",
            },
        };
        self.refuse_unsandboxable(&config.workspace.sandbox, backend, worktrees)
            .await
            .map_err(RpcError::invalid)?;
        let procs = self
            .build_backend(backend, &config, herdr_socket.as_deref(), None)
            .map_err(internal)?;

        // Start from a clean slate; the event log is kept.
        let previous_run = {
            let mut model = self.model();
            let previous_run = self.run_context().map_err(internal)?;
            self.feature_db
                .clear_session(&previous_run.workspace_run_id, |tx| {
                    if let Some(projection) = self.feature_services.task_projection() {
                        projection.clear_run(tx, &previous_run.workspace_run_id)?;
                    }
                    Ok(())
                })
                .map_err(internal)?;
            let quotas = std::mem::take(&mut model.quotas);
            *model = Model {
                quotas,
                session: SessionState {
                    team: Some(config.clone()),
                    backend: backend.to_string(),
                    isolation: if worktrees { "worktree" } else { "shared" }.to_string(),
                    herdr_socket: herdr_socket.clone(),
                    herdr_workspace: None,
                    team_started_at: Some(now_ms()),
                    completed_at: None,
                    summary: None,
                    founded_at: None,
                    vcs: vcs_kind.to_string(),
                    vcs_note: selection
                        .as_ref()
                        .filter(|s| s.fallback)
                        .map(|s| s.note.clone())
                        .unwrap_or_default(),
                    workflow_runs: Vec::new(),
                    workflow_queue: Vec::new(),
                },
                ..Model::default()
            };
            previous_run
        };
        self.feature_services.run_archived(&previous_run);
        // The usage windows are the accounts', not the old team's.
        self.save_quotas();
        self.set_vcs(match &selection {
            Some(selection) => selection.vcs.clone(),
            None => Arc::new(vcs::Git::new(&self.workspace)),
        });
        *self.procs.write().unwrap_or_else(|e| e.into_inner()) = Some(procs);
        self.bus.set_max_bytes(config.limits.max_message_bytes);
        self.save_session();
        // Make the project name visible to `crew attach --project`.
        let _ = paths::write_daemon_info(&DaemonInfo {
            project: Some(config.project.clone()),
            workspace: self.workspace.to_string_lossy().into_owned(),
            state_dir: self.state_dir.to_string_lossy().into_owned(),
            socket: self.socket.to_string_lossy().into_owned(),
            pid: std::process::id(),
            started_at: self.started_at,
            pid_start: paths::process_start(std::process::id()),
            boot_id: paths::boot_id(),
        });
        self.emit(NewEvent::new(
            "daemon",
            "session.started",
            format!(
                "team '{}' starting: {} agent(s), backend {backend}, isolation {}{}",
                config.project,
                config.agents.len(),
                if worktrees { "worktree" } else { "shared" },
                if worktrees {
                    format!(", vcs {vcs_kind}")
                } else {
                    String::new()
                }
            ),
        ));
        if let Some(note) = note {
            self.emit(NewEvent::new("daemon", "session.isolation", note).warn());
        }
        if let Some(selection) = &selection {
            let event = NewEvent::new("daemon", "session.vcs", &selection.note);
            self.emit(if selection.fallback {
                event.warn()
            } else {
                event
            });
        }
        if config.workspace.sandbox.mode == SandboxMode::Off {
            // An explicit opt-out, visible (design-b3.md §3.6).
            self.emit(
                NewEvent::new(
                    "daemon",
                    "session.sandbox",
                    "agents run without a sandbox (workspace.sandbox.mode: off): they can read \
and write what you can and use your credentials; set workspace.sandbox.mode: required to confine \
them",
                )
                .warn(),
            );
        }

        // Agent records (worktrees, tokens) are created before any process
        // starts, so every briefing can list the full team.
        let mut specs: Vec<(AgentSpec, bool)> = Vec::new();
        if config.orchestrator.agent {
            specs.push((
                AgentSpec {
                    id: ORCHESTRATOR_ID.to_string(),
                    role: "orchestrator".to_string(),
                    runtime: config.orchestrator.runtime,
                    model: config.orchestrator.model.clone(),
                    command: config.orchestrator.command.clone(),
                    toolbox: config.orchestrator.toolbox.clone(),
                    ..AgentSpec::default()
                },
                true,
            ));
        }
        specs.extend(config.agents.iter().cloned().map(|a| (a, false)));
        let daemon = self.clone();
        let team = config.clone();
        let backend_name = backend.to_string();
        let ids = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let mut ids = Vec::new();
            for (spec, orchestrator) in &specs {
                let agent = daemon.create_agent(
                    spec,
                    &team,
                    *orchestrator,
                    worktrees,
                    &backend_name,
                    None,
                )?;
                ids.push(agent.id);
            }
            Ok(ids)
        })
        .await
        .map_err(internal)?
        .map_err(|e| RpcError::internal(format!("creating agents: {e:#}")))?;

        for spec in config.all_tasks() {
            self.create_task(spec)?;
        }
        self.persist_roster();

        // Plan before starting anything: each agent's first prompt is then its
        // task, not a "stand by".
        self.apply_plan().await;
        for id in ids {
            self.spawn_in_background(&id, false, None);
        }
        Ok(())
    }
}

/// How the agents of a team are kept apart.
struct Isolated {
    /// The version control backend that gives each agent a workspace of its
    /// own; None when they share the checkout.
    selection: Option<vcs::Selection>,
    /// Why they share it although isolation was not turned off.
    shared_note: Option<String>,
}

/// Decide the team's isolation and make its store ready: create the
/// repository when asked to (`workspace.init`), choose jj or git
/// (`workspace.vcs`) and prepare it. Blocking.
fn isolate(
    workspace: &Path,
    state_dir: &Path,
    config: &crew_core::config::WorkspaceConfig,
) -> Result<Isolated> {
    // The workspace must be the root of its own repository; sitting inside
    // some other repository does not count. A jj repository of its own is
    // used as it is.
    let jj_root = vcs::is_jj_root(workspace);
    if config.init && !jj_root && !(git::is_root(workspace) && git::has_commits(workspace)) {
        git::init_repo(workspace)?;
    }
    let shared = |note: Option<&str>| Isolated {
        selection: None,
        shared_note: note.map(str::to_string),
    };
    if config.isolation == Isolation::Shared {
        if git::is_root(workspace) {
            git::exclude_state_dir(workspace)?;
        }
        return Ok(shared(None));
    }
    let not_usable = || match config.isolation {
        Isolation::Worktree => Err(anyhow!(
            "isolation: worktree needs the workspace to be the root of a git repository with at \
least one commit (set workspace.init: true to create one)"
        )),
        _ => Ok(shared(Some(
            "the workspace is not the root of a git repository with commits, so agents share it \
without isolation",
        ))),
    };
    if !jj_root && !git::is_root(workspace) {
        return not_usable();
    }
    let selection = vcs::select(config.vcs, workspace, state_dir)?;
    if !selection.vcs.prepare(workspace)? {
        return not_usable();
    }
    Ok(Isolated {
        selection: Some(selection),
        shared_note: None,
    })
}
