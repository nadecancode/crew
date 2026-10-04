//! The crew daemon: the long-running process that owns all state.
//!
//! It persists everything through the stores, keeps an in-memory model for
//! fast reads, runs the agents through a `ProcessManager`, moves messages
//! through the `MessageBus`, and pushes every change to subscribers. The
//! dashboard and CLI are plain clients; closing them never affects agents.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use anyhow::{bail, Context, Result};
use crew_core::config::TeamConfig;
use crew_core::paths::{self, DaemonInfo};
use crew_core::rpc::{Push, RpcError, SessionInfo, Snapshot};
use crew_core::util::now_ms;
use crew_core::{Agent, AgentStatus, EventLevel, Task};
use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc, watch, Notify};

use crate::agents::AgentSettings;
use crate::message_bus::{LocalBus, MessageBus};
use crate::persistence::{NewEvent, SqliteStore, Store};
use crate::process::{PaneMeta, ProcEvent, ProcessManager};

mod context;
mod delivery;
mod fallback;
mod gitops;
mod handlers;
mod limits;
mod orchestrate;
mod policy;
mod questions;
mod roster;
mod server;
mod subagents;
mod supervisor;
mod team;
mod turns;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Team-level state that must survive a daemon restart.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub(crate) struct SessionState {
    pub team: Option<TeamConfig>,
    pub backend: String,
    pub isolation: String,
    pub herdr_socket: Option<String>,
    pub herdr_workspace: Option<String>,
    pub team_started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub summary: Option<String>,
    /// When the founding team was settled: the orchestrator's first turn
    /// with a plan ended. Members it adds after that are temporary.
    #[serde(default)]
    pub founded_at: Option<i64>,
}

/// Per-agent state that is persisted but is not part of the public `Agent`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub(crate) struct AgentLocal {
    pub settings: AgentSettings,
    /// The declaration the agent was created from, kept so the roster can be
    /// written back out.
    #[serde(default)]
    pub spec: crew_core::config::AgentSpec,
    /// The runtime reported its session id, so the session can be resumed.
    pub session_confirmed: bool,
    pub tasks_completed: u32,
    /// The account refused the model's 1M-token context variant; it is
    /// not asked for again.
    #[serde(default)]
    pub no_long_context: bool,
    /// The session was started in its model's 1M-token context variant.
    #[serde(default)]
    pub long_context: bool,
    /// The agent moved to another CLI: what its new session is told first,
    /// until that session is confirmed.
    #[serde(default)]
    pub handoff: Option<String>,
    /// On a work style's fallback model: what it goes back to, and when.
    #[serde(default)]
    pub home: Option<policy::HomeModel>,
    /// The last turn of a Codex session file that crew has acted on.
    #[serde(default)]
    pub codex_turn: Option<String>,
    /// How often its model has refused a turn.
    #[serde(default)]
    pub refusals: u32,
}

/// Per-agent state that only matters while the daemon is running.
#[derive(Debug, Default)]
pub(crate) struct AgentRuntimeState {
    /// The session accepts input.
    pub ready: bool,
    /// First prompt, waiting for the session to become ready, with the ids of
    /// the bus messages it contains.
    pub pending_prompt: Option<(String, Vec<String>)>,
    pub turn_active: bool,
    /// When its last turn ended, and when messages were last put into its
    /// session (0: not since the daemon started).
    pub turn_ended_at: i64,
    pub delivered_at: i64,
    /// What the runtime said when the turn just over ended in an error.
    pub turn_failure: Option<String>,
    /// The refused turn last dealt with, so it is dealt with once.
    pub refusal_seen: Option<String>,
    /// The bus messages put into the session since its last turn ended.
    pub turn_messages: Vec<String>,
    /// Of those, the ones given to it at rest, which started the turn: its
    /// final reply answers them.
    pub turn_openers: Vec<String>,
    /// The agent said, with `decline`, that it will not do what this turn
    /// asked.
    pub declined: Option<String>,
    /// A turn that was asked something ended without a tool call, and crew
    /// asked again: that message, and what the turn had been given.
    pub follow_up: Option<(String, Vec<String>)>,
    /// Status the agent set for itself during the current turn.
    pub self_status: Option<AgentStatus>,
    pub nudges: u32,
    pub idle_since: Option<i64>,
    /// We asked the process to stop, so its exit is not a crash.
    pub expected_exit: bool,
    pub transcript: Option<String>,
    pub spawned_at: i64,
    /// Messages pushed to the agent's listener and not yet confirmed.
    pub in_flight: HashMap<String, i64>,
    pub last_output_at: i64,
    pub last_activity_push: i64,
    pub dialog_reported: bool,
    pub dialog_check_pending: bool,
    /// Queueing through the runtime's own socket failed; use the terminal.
    pub queue_failed: bool,
    /// Resumes since the last completed turn that ran into the limit again.
    pub limit_attempts: u32,
    /// Do not read the screen for a limit message before this time.
    pub limit_grace_until: i64,
    pub limit_checked_at: i64,
    pub quota_checked_at: i64,
    /// The Codex session file, once found.
    pub rollout: Option<std::path::PathBuf>,
    /// The runtime itself reported how full the context window is (so the
    /// transcript need not be read for it).
    pub context_reported: bool,
    /// When the session's transcript was last read for what it runs on.
    pub facts_checked_at: i64,
    /// The first model the session reported: what a member on the
    /// runtime's default model started on.
    pub first_model: Option<String>,
    /// What the summary of a queued compaction must keep.
    pub compact_keep: Option<String>,
    /// When the session was last asked to compact (0: not yet).
    pub compact_sent_at: i64,
    pub compacted_at: i64,
    /// Where the Codex session file ended when a compaction was asked for.
    pub compact_offset: u64,
    /// The orchestrator was told that this window is getting full.
    pub context_warned: bool,
}

#[derive(Default)]
pub(crate) struct Model {
    pub session: SessionState,
    pub agents: Vec<Agent>,
    pub tasks: Vec<Task>,
    pub tokens: HashMap<String, String>,
    pub local: HashMap<String, AgentLocal>,
    pub rt: HashMap<String, AgentRuntimeState>,
    /// Usage windows of the accounts the team runs on, newest readings.
    pub quotas: Vec<crew_core::rpc::Quota>,
    /// The stall currently observed: its fingerprint, when it was first
    /// seen, and whether it has been reported.
    pub stall: Option<(String, i64, bool)>,
}

impl Model {
    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn rt(&mut self, id: &str) -> &mut AgentRuntimeState {
        self.rt.entry(id.to_string()).or_default()
    }

    pub fn orchestrator(&self) -> Option<&Agent> {
        self.agents.iter().find(|a| a.orchestrator)
    }
}

pub struct DaemonOptions {
    /// Directory the team works in; state lives in `<workspace>/.crew`.
    pub workspace: PathBuf,
    /// Path of the crew binary handed to agents (MCP bridge, hooks).
    pub crew_bin: PathBuf,
}

/// Shared daemon state. Public only because `Daemon` derefs to it; every
/// field is crate-private.
pub struct Inner {
    pub(crate) workspace: PathBuf,
    pub(crate) state_dir: PathBuf,
    pub(crate) socket: PathBuf,
    pub(crate) crew_bin: PathBuf,
    pub(crate) store: Arc<dyn Store>,
    pub(crate) bus: Arc<dyn MessageBus>,
    pub(crate) push: broadcast::Sender<Push>,
    pub(crate) model: Mutex<Model>,
    pub(crate) procs: RwLock<Option<Arc<dyn ProcessManager>>>,
    pub(crate) proc_tx: mpsc::UnboundedSender<ProcEvent>,
    /// Wakes the scheduler loop.
    pub(crate) plan: Notify,
    pub(crate) shutdown: watch::Sender<bool>,
    /// One lock per agent so prompts are injected one at a time.
    pub(crate) delivery: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Agents whose runtime holds a delivery channel open on the socket.
    pub(crate) listeners: Mutex<HashMap<String, Listener>>,
    /// Helper processes agents depend on (Codex app-servers), by agent id.
    pub(crate) sidecars: Mutex<HashMap<String, SidecarProc>>,
    /// Git operations run one at a time.
    pub(crate) git_lock: tokio::sync::Mutex<()>,
    /// Planning and applying the plan is one critical section, so two
    /// planners can never act on the same snapshot.
    pub(crate) plan_lock: tokio::sync::Mutex<()>,
    pub(crate) next_conn: std::sync::atomic::AtomicU64,
    /// The team is being stopped: an agent whose start is still under way
    /// must not come up behind the stop.
    pub(crate) stopping_team: std::sync::atomic::AtomicBool,
    /// When Claude Code's usage cache was last looked at.
    pub(crate) usage_cache_checked_at: std::sync::atomic::AtomicI64,
    /// When Claude Code was last asked to fetch the account's usage, and
    /// whether it is being asked now.
    pub(crate) usage_refreshed_at: std::sync::atomic::AtomicI64,
    pub(crate) usage_refreshing: std::sync::atomic::AtomicBool,
    pub(crate) started_at: i64,
}

/// A running sidecar and the socket it serves.
pub(crate) struct SidecarProc {
    pub child: tokio::process::Child,
    pub socket: PathBuf,
}

/// A connection an agent's runtime keeps open to receive messages.
pub(crate) struct Listener {
    pub conn: u64,
    pub tx: mpsc::Sender<crew_core::rpc::ServerFrame>,
}

#[derive(Clone)]
pub struct Daemon(pub(crate) Arc<Inner>);

impl std::ops::Deref for Daemon {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

const META_SESSION: &str = "session";
/// The accounts' usage windows as last reported. They belong to the
/// accounts, not to a team, and a runtime only reports them now and then,
/// so they are kept across daemon restarts and team launches.
const META_QUOTAS: &str = "quotas";

fn local_key(agent_id: &str) -> String {
    format!("agent:{agent_id}")
}

impl Daemon {
    pub(crate) fn model(&self) -> MutexGuard<'_, Model> {
        self.model.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn push(&self, push: Push) {
        // No subscribers is normal (dashboard closed).
        let _ = self.push.send(push);
    }

    /// Record a structured event and stream it to subscribers.
    pub(crate) fn emit(&self, event: NewEvent) {
        match event.level {
            EventLevel::Error => tracing::error!(target: "crew", "{}: {}", event.event_type, event.message),
            EventLevel::Warn => tracing::warn!(target: "crew", "{}: {}", event.event_type, event.message),
            _ => tracing::info!(target: "crew", "{}: {}", event.event_type, event.message),
        }
        match self.store.append_event(event) {
            Ok(event) => self.push(Push::Event(event)),
            Err(e) => tracing::error!("persisting event failed: {e:#}"),
        }
    }

    pub(crate) fn procs(&self) -> Result<Arc<dyn ProcessManager>, RpcError> {
        self.procs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| RpcError::unavailable("no team is loaded"))
    }

    pub(crate) fn agent(&self, id: &str) -> Option<Agent> {
        self.model().agent(id).cloned()
    }

    pub(crate) fn save_session(&self) {
        let session = self.model().session.clone();
        match serde_json::to_string(&session) {
            Ok(json) => {
                if let Err(e) = self.store.set_meta(META_SESSION, &json) {
                    tracing::error!("persisting session failed: {e:#}");
                }
            }
            Err(e) => tracing::error!("encoding session failed: {e}"),
        }
        self.push(Push::Session(Box::new(self.session_info())));
    }

    /// Write the accounts' usage windows down, to survive a restart.
    pub(crate) fn save_quotas(&self) {
        let json = serde_json::to_string(&self.model().quotas);
        if let Ok(json) = json {
            if let Err(e) = self.store.set_meta(META_QUOTAS, &json) {
                tracing::warn!("saving the usage windows failed: {e}");
            }
        }
    }

    pub(crate) fn save_local(&self, agent_id: &str) {
        let local = self.model().local.get(agent_id).cloned();
        if let Some(local) = local {
            if let Ok(json) = serde_json::to_string(&local) {
                if let Err(e) = self.store.set_meta(&local_key(agent_id), &json) {
                    tracing::error!("persisting agent settings failed: {e:#}");
                }
            }
        }
    }

    /// Apply a change to an agent, then persist and publish it if anything
    /// actually changed. Status transitions are logged as events.
    pub(crate) fn update_agent(&self, id: &str, f: impl FnOnce(&mut Agent)) -> Option<Agent> {
        let (before, after) = {
            let mut model = self.model();
            let agent = model.agents.iter_mut().find(|a| a.id == id)?;
            let before = agent.clone();
            f(agent);
            (before, agent.clone())
        };
        if before == after {
            return Some(after);
        }
        if let Err(e) = self.store.save_agent(&after) {
            tracing::error!("persisting agent {id} failed: {e:#}");
        }
        self.push(Push::Agent(Box::new(after.clone())));
        if before.status != after.status {
            let mut event = NewEvent::new(
                "daemon",
                "agent.status",
                match (&after.status, &after.error) {
                    // Say why: a failure nobody can explain is the worst kind.
                    (AgentStatus::Failed, Some(error)) => {
                        format!("{id}: {} → {} ({error})", before.status, after.status)
                    }
                    _ => format!("{id}: {} → {}", before.status, after.status),
                },
            )
            .agent(id);
            event = match after.status {
                AgentStatus::Failed => event.error(),
                AgentStatus::Blocked => event.warn(),
                _ => event,
            };
            self.emit(event);
        }
        if before.status != after.status || before.progress != after.progress || before.task != after.task {
            self.publish_meta(&after);
        }
        Some(after)
    }

    /// Mirror role/status/progress into the backend's own UI (the herdr panel).
    fn publish_meta(&self, agent: &Agent) {
        let Ok(procs) = self.procs() else { return };
        if procs.name() != "herdr" {
            return;
        }
        let meta = PaneMeta {
            role: agent.role.clone(),
            task: agent.task.clone(),
            status: agent.status.to_string(),
            progress: agent.progress,
        };
        let id = agent.id.clone();
        tokio::spawn(async move {
            if let Err(e) = procs.set_meta(&id, &meta).await {
                tracing::debug!("herdr metadata for {id}: {e:#}");
            }
        });
    }

    pub(crate) fn update_task(&self, id: &str, f: impl FnOnce(&mut Task)) -> Option<Task> {
        let (before, after) = {
            let mut model = self.model();
            let task = model.tasks.iter_mut().find(|t| t.id == id)?;
            let before = task.clone();
            f(task);
            (before, task.clone())
        };
        if before == after {
            return Some(after);
        }
        if let Err(e) = self.store.save_task(&after) {
            tracing::error!("persisting task {id} failed: {e:#}");
        }
        self.push(Push::Task(Box::new(after.clone())));
        Some(after)
    }

    /// Recompute inbox/outbox counters for the given agents.
    pub(crate) fn refresh_counts(&self, ids: &[String]) {
        for id in ids {
            let Ok(counts) = self.bus.counts(id) else { continue };
            self.update_agent(id, |a| {
                a.inbox_unread = counts.unread;
                a.inbox_total = counts.inbox_total;
                a.outbox_total = counts.outbox_total;
            });
        }
    }

    pub(crate) fn session_info(&self) -> SessionInfo {
        let model = self.model();
        let s = &model.session;
        SessionInfo {
            project: s.team.as_ref().map(|t| t.project.clone()),
            objective: s.team.as_ref().map(|t| t.objective.clone()).unwrap_or_default(),
            style: s.team.as_ref().and_then(|t| t.style.clone()),
            quotas: model.quotas.clone(),
            workspace: self.workspace.to_string_lossy().into_owned(),
            state_dir: self.state_dir.to_string_lossy().into_owned(),
            backend: s.backend.clone(),
            isolation: s.isolation.clone(),
            daemon_started_at: self.started_at,
            team_started_at: s.team_started_at,
            completed_at: s.completed_at,
            summary: s.summary.clone(),
            herdr_workspace: s.herdr_workspace.clone(),
            daemon_pid: std::process::id(),
            version: VERSION.to_string(),
        }
    }

    pub(crate) fn snapshot(&self, messages: usize, events: usize) -> Result<Snapshot, RpcError> {
        let session = self.session_info();
        let (agents, tasks) = {
            let model = self.model();
            (model.agents.clone(), model.tasks.clone())
        };
        Ok(Snapshot {
            session,
            agents,
            tasks,
            messages: self
                .bus
                .history(None, messages)
                .map_err(|e| RpcError::internal(e.to_string()))?,
            events: self
                .store
                .recent_events(None, events)
                .map_err(|e| RpcError::internal(e.to_string()))?,
            questions: self.open_questions()?,
            server_time: now_ms(),
        })
    }

    pub(crate) fn agent_dir(&self, agent_id: &str) -> PathBuf {
        self.state_dir.join("agents").join(agent_id)
    }

    pub(crate) fn worktrees_dir(&self) -> PathBuf {
        self.state_dir.join("worktrees")
    }

    /// Ask the scheduler loop to re-plan.
    pub(crate) fn replan(&self) {
        self.plan.notify_one();
    }
}

fn load_model(store: &dyn Store) -> Result<Model> {
    let mut model = Model {
        agents: store.load_agents()?,
        tasks: store.load_tasks()?,
        tokens: store.load_tokens()?,
        ..Model::default()
    };
    if let Some(json) = store.get_meta(META_SESSION)? {
        model.session = serde_json::from_str(&json).context("decoding persisted session")?;
    }
    if let Some(json) = store.get_meta(META_QUOTAS)? {
        if let Ok(quotas) = serde_json::from_str::<Vec<crew_core::rpc::Quota>>(&json) {
            // A window that has reset since is no longer what was read.
            let now = now_ms();
            model.quotas = quotas
                .into_iter()
                .filter(|q| q.resets_at.is_none_or(|at| at > now))
                .collect();
        }
    }
    for agent in &model.agents {
        if let Some(json) = store.get_meta(&local_key(&agent.id))? {
            if let Ok(local) = serde_json::from_str(&json) {
                model.local.insert(agent.id.clone(), local);
            }
        }
    }
    Ok(model)
}

/// A daemon that has bound its socket and is ready to serve.
pub struct Running {
    pub daemon: Daemon,
    listener: UnixListener,
    proc_rx: mpsc::UnboundedReceiver<ProcEvent>,
}

/// Bind the socket, open the database and recover persisted state.
pub async fn start(options: DaemonOptions) -> Result<Running> {
    let workspace = options
        .workspace
        .canonicalize()
        .with_context(|| format!("resolving workspace {}", options.workspace.display()))?;
    let state_dir = paths::state_dir(&workspace);
    paths::ensure_private_dir(&state_dir)?;
    paths::ensure_private_dir(&state_dir.join("agents"))?;
    paths::ensure_private_dir(&paths::runtime_dir())?;

    let socket = paths::socket_path(&state_dir);
    if let Some(info) = paths::read_daemon_info(&state_dir) {
        if paths::pid_alive(info.pid)
            && info.pid != std::process::id()
            && std::os::unix::net::UnixStream::connect(&info.socket).is_ok()
        {
            bail!(
                "a crew daemon (pid {}) is already running for {}",
                info.pid,
                workspace.display()
            );
        }
    }
    // Stale socket from a daemon that did not shut down cleanly.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("binding {}", socket.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    }

    let store: Arc<SqliteStore> = Arc::new(SqliteStore::open(&state_dir.join("crew.db"))?);
    let model = load_model(store.as_ref())?;
    let bus = Arc::new(LocalBus::new(store.clone()));
    if let Some(team) = &model.session.team {
        bus.set_max_bytes(team.limits.max_message_bytes);
    }

    let (push, _) = broadcast::channel(4096);
    let (proc_tx, proc_rx) = mpsc::unbounded_channel();
    let (shutdown, _) = watch::channel(false);
    let started_at = now_ms();
    let project = model.session.team.as_ref().map(|t| t.project.clone());

    let daemon = Daemon(Arc::new(Inner {
        workspace: workspace.clone(),
        state_dir: state_dir.clone(),
        socket: socket.clone(),
        crew_bin: options.crew_bin,
        store,
        bus,
        push,
        model: Mutex::new(model),
        procs: RwLock::new(None),
        proc_tx,
        plan: Notify::new(),
        shutdown,
        delivery: Mutex::new(HashMap::new()),
        listeners: Mutex::new(HashMap::new()),
        sidecars: Mutex::new(HashMap::new()),
        git_lock: tokio::sync::Mutex::new(()),
        plan_lock: tokio::sync::Mutex::new(()),
        next_conn: std::sync::atomic::AtomicU64::new(1),
        stopping_team: std::sync::atomic::AtomicBool::new(false),
        usage_cache_checked_at: std::sync::atomic::AtomicI64::new(0),
        usage_refreshed_at: std::sync::atomic::AtomicI64::new(0),
        usage_refreshing: std::sync::atomic::AtomicBool::new(false),
        started_at,
    }));

    paths::write_daemon_info(&DaemonInfo {
        project,
        workspace: workspace.to_string_lossy().into_owned(),
        state_dir: state_dir.to_string_lossy().into_owned(),
        socket: socket.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at,
    })?;

    daemon.emit(NewEvent::new(
        "daemon",
        "daemon.started",
        format!("daemon started (pid {})", std::process::id()),
    ));
    Ok(Running {
        daemon,
        listener,
        proc_rx,
    })
}

impl Running {
    pub fn socket(&self) -> &Path {
        &self.daemon.socket
    }

    /// Serve until asked to shut down.
    pub async fn serve(self) -> Result<()> {
        let Running {
            daemon,
            listener,
            mut proc_rx,
        } = self;
        let mut stop = daemon.shutdown.subscribe();

        // Process events (exits, liveness ticks, backend hints).
        let events_task = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                while let Some(event) = proc_rx.recv().await {
                    daemon.on_proc_event(event).await;
                }
            })
        };
        // Scheduler: re-plan whenever something asks for it.
        let plan_task = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                loop {
                    daemon.plan.notified().await;
                    daemon.apply_plan().await;
                }
            })
        };
        // Time-based conditions: idle reminders, timeouts, unresponsive agents.
        let housekeeping_task = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tick.tick().await;
                    daemon.housekeeping().await;
                    // Re-plan on the tick too: a stall is only reported once
                    // it has lasted a while.
                    daemon.replan();
                }
            })
        };

        daemon.recover().await;

        let accept_task = {
            let daemon = daemon.clone();
            tokio::spawn(async move { server::accept_loop(daemon, listener).await })
        };

        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        tokio::select! {
            _ = stop.changed() => {}
            _ = sigterm.recv() => daemon.emit(NewEvent::new("daemon", "daemon.signal", "received SIGTERM")),
            _ = sigint.recv() => daemon.emit(NewEvent::new("daemon", "daemon.signal", "received SIGINT")),
        }

        daemon.emit(NewEvent::new("daemon", "daemon.stopped", "daemon stopping"));
        daemon.push(Push::Shutdown);
        // Give writers a moment to flush the final frames.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        accept_task.abort();
        events_task.abort();
        plan_task.abort();
        housekeeping_task.abort();
        let _ = std::fs::remove_file(&daemon.socket);
        paths::remove_daemon_info(&daemon.state_dir);
        Ok(())
    }
}
