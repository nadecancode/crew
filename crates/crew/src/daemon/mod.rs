//! The crew daemon: the long-running process that owns all state.
//!
//! It persists everything through the stores, keeps an in-memory model for
//! fast reads, runs the agents through a `ProcessManager`, moves messages
//! through the `MessageBus`, and pushes every change to subscribers. The
//! dashboard and CLI are plain clients; closing them never affects agents.

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
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
use crate::message_bus::{LocalBus, MessageBus, NewMessage, SendOutcome};
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
mod retire;
mod roster;
mod sandbox;
pub(crate) mod server;
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
    /// The version control backend the agents' workspaces were set up
    /// with (`jj` or `git`); empty when they share the checkout.
    #[serde(default)]
    pub vcs: String,
    /// Why it is git although jj is the default; empty otherwise.
    #[serde(default)]
    pub vcs_note: String,
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
    /// The agent's current launch (`CREW_LAUNCH` of the processes it
    /// started): kept across daemon restarts, so processes adopted then are
    /// still its own.
    #[serde(default)]
    pub launch: Option<String>,
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
    /// The provider's error the last turn ended in, until a turn ends well:
    /// an agent that goes quiet with one is reported with it, not as
    /// unresponsive.
    pub provider_error: Option<String>,
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
    /// A write the plan needed was refused, and this was said (its error).
    pub plan_refused: Option<String>,
    /// Ids of agents being created: taken, though not on the team yet.
    pub reserved: HashSet<String>,
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

    /// An agent has the id, or one is being created under it.
    pub fn id_taken(&self, id: &str) -> bool {
        self.agent(id).is_some() || self.reserved.contains(id)
    }
}

/// An id in `Model::reserved`, given back when this is dropped: once the
/// agent is on the team, or its creation failed. Create and drop it with
/// the model unlocked.
pub(crate) struct IdReservation {
    daemon: Daemon,
    id: String,
}

impl IdReservation {
    /// For an id the caller has just put in `Model::reserved`.
    pub(crate) fn new(daemon: &Daemon, id: &str) -> Self {
        Self {
            daemon: daemon.clone(),
            id: id.to_string(),
        }
    }
}

impl Drop for IdReservation {
    fn drop(&mut self) {
        self.daemon.model().reserved.remove(&self.id);
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
    /// Computed once from the socket this daemon binds.
    socket_warning: String,
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
    /// Every sandbox started for an agent that is not known to be gone yet
    /// (design-b3.md §3.4, D2): nothing of the agent's next launch is
    /// written until its entries are gone.
    pub(crate) ledger: retire::Ledger,
    /// Each agent's launch lease (Daemon::launch_lease).
    pub(crate) launches: Mutex<HashMap<String, Arc<tokio::sync::RwLock<()>>>>,
    /// Each agent's hook lease (Daemon::on_hook, Daemon::begin_launch).
    pub(crate) hooks: Mutex<HashMap<String, Arc<tokio::sync::RwLock<()>>>>,
    /// How many stops of each agent have been asked for (Daemon::stops).
    pub(crate) stops: Mutex<HashMap<String, u64>>,
    /// The version control backend of the session (Daemon::vcs).
    pub(crate) vcs: RwLock<Option<Arc<dyn crate::vcs::Vcs>>>,
    /// The web dashboard's server (`crew web`).
    pub(crate) web: crate::web::Web,
    /// Numbers VCS operations for their events (`v<n>`).
    pub(crate) vcs_ops: std::sync::atomic::AtomicU64,
    /// Git operations run one at a time.
    pub(crate) git_lock: Arc<tokio::sync::Mutex<()>>,
    /// Planning and applying the plan is one critical section, so two
    /// planners can never act on the same snapshot.
    pub(crate) plan_lock: tokio::sync::Mutex<()>,
    /// Loading a team and stopping it are one at a time: a load checks that
    /// no team is running and then replaces the session, with nothing in
    /// between.
    pub(crate) team_lock: tokio::sync::Mutex<()>,
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
    /// Held for the daemon's lifetime: the workspace's single-instance lock.
    /// Connection tasks and other `Daemon` handles can outlive `Running`, so
    /// the lock lasts as long as any of them.
    _instance_lock: std::fs::File,
    /// The operator's credential for this run (design-b3.md §2.1): here and in
    /// the 0600 token file next to the socket, nowhere else (not the database,
    /// logs, events, pushes or agent environments).
    pub(crate) operator_token: String,
    /// `<workspace>/.crew`, held open from the start (design-b3.md §3.4, D2).
    /// Agent directories are reached from it one component at a time, never
    /// through a path an agent could have replaced with a link.
    pub(crate) state_root: OwnedFd,
}

/// A running sidecar and the socket it serves.
pub(crate) struct SidecarProc {
    pub child: tokio::process::Child,
    pub socket: PathBuf,
    /// What the sidecar was started with: the agent's variables, which a
    /// sandboxed queue command gets too.
    pub env: Vec<(String, String)>,
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
            EventLevel::Error => {
                tracing::error!(target: "crew", "{}: {}", event.event_type, event.message)
            }
            EventLevel::Warn => {
                tracing::warn!(target: "crew", "{}: {}", event.event_type, event.message)
            }
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
        if before.status != after.status
            || before.progress != after.progress
            || before.task != after.task
        {
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

    /// Apply a change to a task, or `None` if there is no such task or the
    /// change could not be stored (logged; nothing changed then).
    pub(crate) fn update_task(&self, id: &str, f: impl FnOnce(&mut Task)) -> Option<Task> {
        match self.try_update_task(id, f) {
            Ok(task) => task,
            Err(e) => {
                tracing::error!("{}", e.message);
                None
            }
        }
    }

    /// Apply a change to a task: it is stored first, and only then made
    /// visible, so nothing is reported that the database does not hold.
    /// Saving under the model lock also keeps an older snapshot from
    /// overwriting a newer one.
    pub(crate) fn try_update_task(
        &self,
        id: &str,
        f: impl FnOnce(&mut Task),
    ) -> Result<Option<Task>, RpcError> {
        let after = {
            let mut model = self.model();
            let Some(task) = model.tasks.iter_mut().find(|t| t.id == id) else {
                return Ok(None);
            };
            let mut after = task.clone();
            f(&mut after);
            if after == *task {
                return Ok(Some(after));
            }
            self.store
                .save_task(&after)
                .map_err(|e| RpcError::internal(format!("persisting task {id} failed: {e:#}")))?;
            *task = after.clone();
            after
        };
        self.push(Push::Task(Box::new(after.clone())));
        Ok(Some(after))
    }

    /// try_update_task, with `message` (for `recipients`) stored in the
    /// same write as the change: both are, or neither is, and nothing is
    /// published unless they are. As try_update_task's, the change is made
    /// to the task as it is now, all under the model lock, so a change of
    /// it made meanwhile by any writer is kept. `f` refusing (Err) changes
    /// nothing.
    pub(crate) fn try_update_task_telling(
        &self,
        id: &str,
        f: impl FnOnce(&mut Task) -> Result<(), RpcError>,
        message: NewMessage,
        recipients: Vec<String>,
    ) -> Result<Option<(Task, SendOutcome)>, RpcError> {
        let (after, outcome) = {
            let mut model = self.model();
            let Some(task) = model.tasks.iter_mut().find(|t| t.id == id) else {
                return Ok(None);
            };
            let mut after = task.clone();
            f(&mut after)?;
            let outcome = self.bus.send_with_task(message, recipients, &after)?;
            *task = after.clone();
            (after, outcome)
        };
        self.push(Push::Task(Box::new(after.clone())));
        Ok(Some((after, outcome)))
    }

    /// Recompute inbox/outbox counters for the given agents.
    pub(crate) fn refresh_counts(&self, ids: &[String]) {
        for id in ids {
            let Ok(counts) = self.bus.counts(id) else {
                continue;
            };
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
            objective: s
                .team
                .as_ref()
                .map(|t| t.objective.clone())
                .unwrap_or_default(),
            style: s.team.as_ref().and_then(|t| t.style.clone()),
            quotas: model.quotas.clone(),
            workspace: self.workspace.to_string_lossy().into_owned(),
            state_dir: self.state_dir.to_string_lossy().into_owned(),
            backend: s.backend.clone(),
            isolation: s.isolation.clone(),
            sandbox: s
                .team
                .as_ref()
                .map(|team| match team.workspace.sandbox.mode {
                    crew_core::config::SandboxMode::Off => "off",
                    crew_core::config::SandboxMode::Required => "required",
                })
                .unwrap_or_default()
                .to_string(),
            vcs: s.vcs.clone(),
            vcs_note: s.vcs_note.clone(),
            web: self.web.url(),
            socket_warning: self.socket_warning.clone(),
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

    /// Make an agent's directories beneath the held state directory: `<A>`,
    /// which only the daemon writes, and `<A>/rw/home`, which the agent
    /// writes (design-b3.md §3.2). A link anywhere on the way is refused, so
    /// an agent that replaced `rw` cannot make the daemon write elsewhere.
    /// Returns `<A>`.
    pub(crate) fn agent_root(&self, agent_id: &str) -> Result<OwnedFd> {
        let agents = private_subdir(self.state_root.as_fd(), "agents")?;
        let root = private_subdir(agents.as_fd(), agent_id)?;
        let rw = private_subdir(root.as_fd(), "rw")?;
        private_subdir(rw.as_fd(), "home")?;
        Ok(root)
    }

    /// How to reach the files an agent's CLI keeps (transcripts, rollouts): the
    /// operator's own directories, or for a sandboxed agent its private tree,
    /// opened through its root descriptor.
    pub(crate) fn agent_files(&self, agent_id: &str) -> Result<crate::agents::files::AgentFiles> {
        if !self.sandboxed() {
            return Ok(crate::agents::files::AgentFiles::Operator);
        }
        // Only existing directories are opened: reading never makes `rw/home`
        // under an agent that is running (`agent_root` is for the launch).
        crate::agents::files::AgentFiles::open_existing(
            self.state_root.as_fd(),
            self.agent_dir(agent_id),
            agent_id,
        )
    }

    pub(crate) fn worktrees_dir(&self) -> PathBuf {
        self.state_dir.join("worktrees")
    }

    /// The version control backend of the session: the one `load_team`
    /// selected, or after a restart the one the session was set up with.
    pub(crate) fn vcs(&self) -> Arc<dyn crate::vcs::Vcs> {
        if let Some(vcs) = self.vcs.read().unwrap_or_else(|e| e.into_inner()).clone() {
            return vcs;
        }
        let kind = self.model().session.vcs.clone();
        let restored = crate::vcs::restore(&kind, &self.workspace, &self.state_dir);
        self.vcs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(restored)
            .clone()
    }

    /// Make `vcs` the session's backend.
    pub(crate) fn set_vcs(&self, vcs: Arc<dyn crate::vcs::Vcs>) {
        *self.vcs.write().unwrap_or_else(|e| e.into_inner()) = Some(vcs);
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

/// Take the workspace's single-instance lock (`daemon.lock` in the state
/// dir), or `None` if another daemon holds it. The kernel releases it when
/// the daemon exits, however it exits.
fn lock_instance(state_dir: &Path) -> Result<Option<std::fs::File>> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = state_dir.join("daemon.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    // SAFETY: flock on a descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(None);
    }
    Err(err).with_context(|| format!("locking {}", path.display()))
}

/// The state directory, opened once (not through a link) for `Inner::state_root`.
fn open_state_root(state_dir: &Path) -> Result<OwnedFd> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(state_dir)
        .with_context(|| format!("opening {}", state_dir.display()))?;
    Ok(dir.into())
}

/// `name` in `dir`, made `0700` if it is missing, and opened only if it is a
/// directory and not a link.
fn private_subdir(dir: BorrowedFd<'_>, name: &str) -> Result<OwnedFd> {
    let c_name = std::ffi::CString::new(name)?;
    // SAFETY: `dir` is a live descriptor and `c_name` is NUL-terminated.
    if unsafe { libc::mkdirat(dir.as_raw_fd(), c_name.as_ptr(), 0o700) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(err).with_context(|| format!("creating {name}"));
        }
    }
    paths::open_dir_beneath(dir, Path::new(name))
        .with_context(|| format!("{name} is not a directory of the daemon's"))
}

/// A daemon that has bound its socket and is ready to serve.
pub struct Running {
    pub daemon: Daemon,
    listener: UnixListener,
    proc_rx: mpsc::UnboundedReceiver<ProcEvent>,
    published: Published,
}

/// What this start published under the workspace lock (B7): its operator
/// token, its socket once bound, its registration once written. Withdrawn
/// in the reverse order at shutdown, or on drop when the start fails. It
/// holds its own descriptor of the lock (a clone sharing the same flock),
/// released only after the withdrawal (a field drops after `Drop::drop`
/// runs), so the daemon that wins the lock next never loses its files to
/// this one. Only this run's token and registration are removed.
struct Published {
    state_dir: PathBuf,
    socket: PathBuf,
    token: String,
    bound: bool,
    /// This run's registration was being written: its `started_at`.
    registering: Option<i64>,
    /// ...and `write_daemon_info` finished, so the registry entry is ours too.
    registered: bool,
    _lock: std::fs::File,
}

impl Published {
    fn withdraw(&mut self) {
        if let Some(started_at) = self.registering.take() {
            if std::mem::take(&mut self.registered) {
                paths::remove_daemon_info(&self.state_dir);
            } else if paths::read_daemon_info(&self.state_dir)
                .is_some_and(|info| info.pid == std::process::id() && info.started_at == started_at)
            {
                // Half written: daemon.json is ours, the registry entry is not.
                let _ = std::fs::remove_file(paths::daemon_info_path(&self.state_dir));
            }
        }
        if std::mem::take(&mut self.bound) {
            let _ = std::fs::remove_file(&self.socket);
        }
        let ours = paths::read_operator_token(&self.socket)
            .ok()
            .flatten()
            .is_some_and(|token| crew_core::util::ct_eq(&token, &self.token));
        if ours {
            let _ = std::fs::remove_file(paths::operator_token_path(&self.socket));
        }
    }
}

impl Drop for Published {
    fn drop(&mut self) {
        self.withdraw();
    }
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
    let state_root = open_state_root(&state_dir)?;

    let socket = paths::socket_path(&state_dir);
    let socket_warning = paths::socket_warning(&socket);
    // Only the lock holder may check the registration, replace the socket
    // and open the database, so two starts close together cannot both win.
    let Some(lock) = lock_instance(&state_dir)? else {
        let pid = paths::read_daemon_info(&state_dir)
            .map(|info| format!(" (pid {})", info.pid))
            .unwrap_or_default();
        bail!(
            "a crew daemon{pid} is already running for {}",
            workspace.display()
        );
    };
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
    // A new operator token for every start that wins the lock, written before
    // the socket is bound: a client never finds a socket without its token.
    let token = crew_core::util::new_token();
    let mut published = Published {
        state_dir: state_dir.clone(),
        socket: socket.clone(),
        token: token.clone(),
        bound: false,
        registering: None,
        registered: false,
        _lock: lock.try_clone().context("sharing the workspace lock")?,
    };
    paths::write_private_file(&paths::operator_token_path(&socket), token.as_bytes())
        .context("writing the operator token")?;
    // Stale socket from a daemon that did not shut down cleanly.
    let _ = std::fs::remove_file(&socket);
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    published.bound = true;
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

    // What an earlier daemon started stays binding (design-b3.md §3.4, D2).
    let ledger = retire::Ledger::open(&state_dir.join("sandboxes"))
        .context("opening the sandbox retirement ledger")?;
    let daemon = Daemon(Arc::new(Inner {
        workspace: workspace.clone(),
        state_dir: state_dir.clone(),
        socket: socket.clone(),
        socket_warning,
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
        ledger,
        launches: Mutex::new(HashMap::new()),
        hooks: Mutex::new(HashMap::new()),
        stops: Mutex::new(HashMap::new()),
        vcs: RwLock::new(None),
        web: crate::web::Web::default(),
        vcs_ops: std::sync::atomic::AtomicU64::new(1),
        git_lock: Arc::new(tokio::sync::Mutex::new(())),
        plan_lock: tokio::sync::Mutex::new(()),
        team_lock: tokio::sync::Mutex::new(()),
        next_conn: std::sync::atomic::AtomicU64::new(1),
        stopping_team: std::sync::atomic::AtomicBool::new(false),
        usage_cache_checked_at: std::sync::atomic::AtomicI64::new(0),
        usage_refreshed_at: std::sync::atomic::AtomicI64::new(0),
        usage_refreshing: std::sync::atomic::AtomicBool::new(false),
        started_at,
        _instance_lock: lock,
        operator_token: token,
        state_root,
    }));

    // Before writing: a registration only half written is withdrawn too.
    published.registering = Some(started_at);
    paths::write_daemon_info(&DaemonInfo {
        project,
        workspace: workspace.to_string_lossy().into_owned(),
        state_dir: state_dir.to_string_lossy().into_owned(),
        socket: socket.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at,
        pid_start: paths::process_start(std::process::id()),
        boot_id: paths::boot_id(),
    })?;
    published.registered = true;

    daemon.emit(NewEvent::new(
        "daemon",
        "daemon.started",
        format!("daemon started (pid {})", std::process::id()),
    ));
    if !daemon.socket_warning.is_empty() {
        daemon
            .emit(NewEvent::new("daemon", "daemon.socket_fallback", &daemon.socket_warning).warn());
    }
    Ok(Running {
        daemon,
        listener,
        proc_rx,
        published,
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
            mut published,
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
                    daemon.web.tick(&daemon).await;
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

        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
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
        // The web server stops before the socket and the registration go:
        // its sessions, then its connections, then its listeners.
        daemon.web.stop(&daemon, None).await;
        accept_task.abort();
        events_task.abort();
        plan_task.abort();
        housekeeping_task.abort();
        published.withdraw();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::{MessageType, TaskStatus};

    #[tokio::test]
    async fn a_task_change_told_in_the_same_write_is_made_to_the_task_as_it_is_now() {
        let temp = tempfile::tempdir().unwrap();
        let running = start(DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
        })
        .await
        .unwrap();
        let daemon = running.daemon.clone();
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "t", "title": "as read", "description": "", "status": "review",
            "assigned_agent": "author", "created_at": 0
        }))
        .unwrap();
        daemon.model().tasks.push(task);
        let verdict = || {
            NewMessage::new(
                "critic",
                "author",
                MessageType::ReviewResult,
                "Approved: task t.",
            )
        };
        let recipients = || vec!["author".to_string()];
        let messages = |daemon: &Daemon| daemon.bus.history(None, 10).unwrap().len();

        // The change is made to the task as it is when it is stored.
        daemon.model().tasks[0].title = "edited meanwhile".into();
        let (told, _) = daemon
            .try_update_task_telling(
                "t",
                |t| {
                    t.status = TaskStatus::Done;
                    Ok(())
                },
                verdict(),
                recipients(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            (told.status, told.title.as_str()),
            (TaskStatus::Done, "edited meanwhile")
        );
        assert_eq!(
            daemon.store.load_tasks().unwrap()[0].title,
            "edited meanwhile"
        );
        assert_eq!(messages(&daemon), 1);

        // A change refused (here: no longer awaiting review) stores nothing
        // and tells nothing.
        let refused = daemon.try_update_task_telling(
            "t",
            |_| Err(RpcError::conflict("not awaiting review")),
            verdict(),
            recipients(),
        );
        assert_eq!(refused.unwrap_err().code, "conflict");
        assert_eq!(daemon.model().tasks[0].status, TaskStatus::Done);
        assert_eq!(messages(&daemon), 1);
    }

    #[tokio::test]
    async fn the_compaction_offset_is_measured_through_the_agents_files() {
        let temp = tempfile::tempdir().unwrap();
        let running = start(DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        let mut team = crew_core::config::TeamConfig::from_yaml(
            "project: compact-review\nobjective: compact review\n",
        )
        .unwrap();
        team.workspace.sandbox.mode = crew_core::config::SandboxMode::Required;
        daemon.model().session.team = Some(team);
        drop(daemon.agent_root("a").unwrap());
        let agent: crew_core::Agent = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "role": "review", "status": "working", "runtime": "codex",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0,
            "compact_pending": true
        }))
        .unwrap();
        daemon.model().agents.push(agent);
        *daemon.procs.write().unwrap() = Some(Arc::new(crate::process::pty::PtyManager::new(
            daemon.proc_tx.clone(),
        )));
        let rollout = daemon.agent_dir("a").join("rw/rollout.jsonl");
        std::fs::write(&rollout, "12345").unwrap();
        {
            let mut model = daemon.model();
            let rt = model.rt("a");
            rt.ready = true;
            rt.rollout = Some(rollout.clone());
        }
        daemon.try_compact("a").await;
        assert_eq!(daemon.model().rt("a").compact_offset, 5, "regular rollout");
        // The agent replaced its rollout by a link to a file outside its tree:
        // the offset comes from nothing the link points at.
        let outside = temp.path().join("outside-rollout");
        std::fs::write(&outside, "outside metadata must not supply this offset").unwrap();
        std::fs::remove_file(&rollout).unwrap();
        std::os::unix::fs::symlink(&outside, &rollout).unwrap();
        daemon.model().rt("a").compact_sent_at = 0;
        daemon.try_compact("a").await;
        assert_eq!(
            daemon.model().rt("a").compact_offset,
            0,
            "followed a linked rollout"
        );
    }

    #[tokio::test]
    async fn the_daemons_agent_files_accessor_does_not_create_a_missing_home() {
        let temp = tempfile::tempdir().unwrap();
        let running = start(DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        let mut team = crew_core::config::TeamConfig::from_yaml(
            "project: reader-review\nobjective: reader review\n",
        )
        .unwrap();
        team.workspace.sandbox.mode = crew_core::config::SandboxMode::Required;
        daemon.model().session.team = Some(team);
        drop(daemon.agent_root("a").unwrap());
        let home = daemon.agent_dir("a").join("rw/home");
        std::fs::remove_dir(&home).unwrap();
        let files = daemon.agent_files("a").unwrap();
        assert!(
            !home.exists(),
            "a transcript reader created a missing private home"
        );
        let file = daemon.agent_dir("a").join("rw/session.jsonl");
        std::fs::write(&file, "data").unwrap();
        assert!(files.open_reported(&file).is_some());
    }

    #[test]
    fn socket_fallback_is_reported_from_the_bound_socket_and_emitted_once() {
        const PROBE: &str = "CREW_SOCKET_FALLBACK_PROBE";
        let Some(root) = std::env::var_os(PROBE) else {
            for case in ["xdg", "missing", "long"] {
                let root = tempfile::Builder::new()
                    .prefix("crew-sock")
                    .tempdir_in("/tmp")
                    .unwrap();
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child.args(["--exact", "daemon::tests::socket_fallback_is_reported_from_the_bound_socket_and_emitted_once", "--nocapture"])
                    .env(PROBE, root.path()).env("CREW_SOCKET_FALLBACK_CASE", case)
                    .env_remove("CREW_IDENTITY").env_remove("CREW_SOCKET");
                for (key, name) in [
                    ("HOME", "home"),
                    ("XDG_STATE_HOME", "state"),
                    ("XDG_CONFIG_HOME", "config"),
                    ("XDG_RUNTIME_DIR", "run"),
                ] {
                    let dir = root.path().join(name);
                    std::fs::create_dir(&dir).unwrap();
                    child.env(key, dir);
                }
                if case == "missing" {
                    child.env_remove("XDG_RUNTIME_DIR");
                }
                if case == "long" {
                    child.env("XDG_RUNTIME_DIR", root.path().join("x".repeat(108)));
                }
                let output = child
                    .env("CREW_GLOBAL_CONFIG", root.path().join("global.toml"))
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "case {case}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            }
            return;
        };
        let root = PathBuf::from(root);
        let workspace = root.join("ws");
        std::fs::create_dir(&workspace).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let running = runtime
            .block_on(start(DaemonOptions {
                workspace,
                crew_bin: std::env::current_exe().unwrap(),
            }))
            .unwrap();
        let info = serde_json::to_value(running.daemon.session_info()).unwrap();
        let warning = info["socket_warning"]
            .as_str()
            .expect("daemon must report its socket warning");
        let fallback = std::env::var("CREW_SOCKET_FALLBACK_CASE").unwrap() != "xdg";
        assert_eq!(!warning.is_empty(), fallback);
        assert_eq!(
            running.socket().parent().unwrap(),
            if fallback {
                PathBuf::from(format!("/tmp/crew-{}", paths::uid()))
            } else {
                root.join("run/crew")
            }
        );
        let events = running.daemon.store.recent_events(None, 100).unwrap();
        let warnings: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "daemon.socket_fallback")
            .collect();
        assert_eq!(warnings.len(), usize::from(fallback));
        if fallback {
            assert_eq!(warnings[0].message, warning);
            assert_eq!(warnings[0].source, "daemon");
            assert_eq!(warnings[0].level, EventLevel::Warn);
        }
        // Published::drop withdraws this run under the lock, including the
        // fallback files in the shared runtime directory. Leave it intact.
        let socket = running.socket().to_path_buf();
        let token = paths::operator_token_path(&socket);
        drop(running);
        assert!(!socket.exists(), "socket survived the unserved run");
        assert!(!token.exists(), "operator token survived the unserved run");
    }

    #[test]
    fn session_info_reports_the_loaded_team_sandbox_mode() {
        const PROBE: &str = "CREW_SESSION_SANDBOX_PROBE";
        let Some(root) = std::env::var_os(PROBE) else {
            let root = tempfile::Builder::new()
                .prefix("crew-sb")
                .tempdir_in("/tmp")
                .unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "daemon::tests::session_info_reports_the_loaded_team_sandbox_mode",
                    "--nocapture",
                ])
                .env(PROBE, root.path())
                .env_remove("CREW_IDENTITY")
                .env_remove("CREW_SOCKET");
            for (key, name) in [
                ("HOME", "home"),
                ("XDG_RUNTIME_DIR", "run"),
                ("XDG_STATE_HOME", "state"),
                ("XDG_CONFIG_HOME", "config"),
            ] {
                let dir = root.path().join(name);
                std::fs::create_dir(&dir).unwrap();
                child.env(key, dir);
            }
            let output = child
                .env("CREW_GLOBAL_CONFIG", root.path().join("config.toml"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let workspace = PathBuf::from(root).join("ws");
        std::fs::create_dir(&workspace).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let running = runtime
            .block_on(start(DaemonOptions {
                workspace,
                crew_bin: std::env::current_exe().unwrap(),
            }))
            .unwrap();
        let reported =
            || serde_json::to_value(running.daemon.session_info()).unwrap()["sandbox"].clone();
        assert_eq!(reported(), "", "no loaded team must not claim a sandbox");
        let mut team = TeamConfig::from_yaml("project: state-test\n").unwrap();
        for (mode, expected) in [
            (crew_core::config::SandboxMode::Off, "off"),
            (crew_core::config::SandboxMode::Required, "required"),
        ] {
            // Exercise the getter without launching a Required team before
            // the staged enforcement gate has been replaced.
            team.workspace.sandbox.mode = mode;
            running.daemon.model().session.team = Some(team.clone());
            assert_eq!(reported(), expected);
        }
        running.daemon.model().session.team = None;
        assert_eq!(reported(), "");
    }

    #[test]
    fn the_workspace_lock_is_held_until_everything_is_withdrawn() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        std::fs::create_dir(&state_dir).unwrap();
        let socket = dir.path().join("s.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let token = crew_core::util::new_token();
        paths::write_private_file(&paths::operator_token_path(&socket), token.as_bytes()).unwrap();
        let mut published = Published {
            state_dir: state_dir.clone(),
            socket: socket.clone(),
            token,
            bound: true,
            registering: None,
            registered: false,
            _lock: lock_instance(&state_dir).unwrap().unwrap(),
        };
        assert!(lock_instance(&state_dir).unwrap().is_none());

        published.withdraw();
        assert!(!socket.exists());
        assert!(!paths::operator_token_path(&socket).exists());
        // Still held: a daemon that starts now cannot have its socket or
        // token removed by this one's cleanup.
        assert!(lock_instance(&state_dir).unwrap().is_none());

        drop(published);
        // A test running alongside may have just forked a child (see
        // sandbox::test_support::in_child) that holds a copy of the lock's
        // descriptor; the lock goes when that child exits.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while lock_instance(&state_dir).unwrap().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the lock outlived the withdrawal"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
