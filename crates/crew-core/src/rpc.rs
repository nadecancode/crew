//! Wire protocol: newline-delimited JSON over the daemon's Unix socket.
//!
//! A client sends `Request` lines (`{"id":1,"method":"send","params":{..}}`)
//! and receives `ServerFrame` lines: either the response to a request
//! (`{"id":1,"result":..}` / `{"id":1,"error":..}`) or, after `subscribe`,
//! unsolicited pushes (`{"push":{..}}`).
//!
//! The same calls back the MCP tools, `crew ctl` and the dashboard, so adding
//! another front end never needs a protocol change.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{AgentSpec, TaskSpec, TeamConfig};
use crate::types::{
    Agent, AgentStatus, Artifact, Event, InboxEntry, Message, MessageType, RuntimeKind, Task,
    WebEventKind, WorkspaceState,
};

/// Bump for new/renamed enum values, removed/renamed fields or changed meanings.
/// New optional fields with `serde(default)` do not require a bump.
/// Peers require an exact match; restart running teams after upgrading.
pub const PROTOCOL_VERSION: u32 = 6;

/// The dashboard server closes a page's workspace connection with this when
/// the daemon's side of it ended. It says nothing about the daemon: the
/// listing does (notes/web-multi.md §multi-transport).
pub const CLOSE_WORKSPACE_ENDED: u16 = 4001;
/// The dashboard server closes a page's workspace connection with this when
/// the workspace was taken out of the dashboard (`crew web --remove`).
pub const CLOSE_WORKSPACE_REMOVED: u16 = 4002;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The human at the CLI or dashboard.
    Operator,
    /// A process acting as one specific agent (MCP bridge, `crew ctl`, hooks).
    Agent,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Hello {
    pub role: Role,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    /// Free-text client name for the daemon log (`tui`, `mcp`, `cli`, ...).
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub protocol: u32,
    /// Set by the dashboard server on a connection it opens for a browser:
    /// the daemon then treats the connection as a web session (W7).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub web: bool,
    /// From a browser to the dashboard server: the workspace this
    /// connection is for. Absent: the index connection. A daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl std::fmt::Debug for Hello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hello")
            .field("role", &self.role)
            .field("agent_id", &self.agent_id)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("client", &self.client)
            .field("protocol", &self.protocol)
            .field("web", &self.web)
            .field("workspace", &self.workspace)
            .finish()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SendParams {
    /// Recipient agent id, or `*` to broadcast.
    pub to: String,
    #[serde(rename = "type", default = "default_message_type")]
    pub kind: MessageType,
    pub body: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub metadata: Value,
    /// Client-chosen message id. Re-sending the same id is a no-op, which
    /// makes retries safe.
    #[serde(default)]
    pub id: Option<String>,
}

fn default_message_type() -> MessageType {
    MessageType::Message
}

/// Every operation the daemon understands.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Call {
    #[serde(deserialize_with = "empty_params")]
    Ping,
    Hello(Hello),
    /// Full current state; `messages`/`events` bound the history included.
    Snapshot {
        #[serde(default)]
        messages: Option<usize>,
        #[serde(default)]
        events: Option<usize>,
    },
    /// Start receiving pushes on this connection.
    #[serde(deserialize_with = "empty_params")]
    Subscribe,
    /// Launch a team in this daemon's workspace.
    LoadTeam {
        config: Box<TeamConfig>,
        /// herdr API socket of the invoking terminal, when run inside herdr.
        #[serde(default)]
        herdr_socket: Option<String>,
    },
    Shutdown {
        #[serde(default = "default_true")]
        stop_agents: bool,
    },

    #[serde(deserialize_with = "empty_params")]
    AgentsList,
    AgentAdd {
        spec: AgentSpec,
    },
    AgentKill {
        id: String,
    },
    /// Take an agent off the team: stop it and return its open tasks to the
    /// pool (operator or orchestrator).
    AgentRemove {
        id: String,
    },
    AgentRestart {
        id: String,
    },
    /// Restart an agent in a new session: its old one is not resumed (a
    /// session whose every turn the provider refuses stays refused).
    /// Operator only. A separate call, not a flag on AgentRestart, so an
    /// older daemon refuses it rather than resuming.
    AgentRestartFresh {
        id: String,
    },
    /// Put an agent on another model, with `note` as its first word
    /// (operator or orchestrator). On the same CLI its session is restarted
    /// on the model, resumed; a model of another CLI (or `runtime`) moves
    /// the agent to that CLI in a fresh session, handed off. An empty
    /// `model` with a `runtime` is that CLI's own default model.
    AgentSetModel {
        id: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        runtime: Option<RuntimeKind>,
        #[serde(default)]
        note: Option<String>,
    },
    /// The models an agent can be put on from this machine, for the
    /// dashboard's picker (operator).
    #[serde(deserialize_with = "empty_params")]
    ModelsList,
    /// The accounts' usage windows, which models are out until they reset
    /// and what to take instead, as text (orchestrator or operator).
    #[serde(deserialize_with = "empty_params")]
    UsageStatus,
    AgentPause {
        id: String,
    },
    /// Continue agents that stopped at a usage limit (all of them when
    /// `agents` is empty). `restart` starts their processes afresh, resuming
    /// their sessions, so a changed login or key takes effect.
    Resume {
        #[serde(default)]
        agents: Vec<String>,
        #[serde(default)]
        restart: bool,
    },
    AgentResume {
        id: String,
    },
    /// Queue a compaction of an agent's context: at its next turn boundary
    /// the runtime replaces the conversation so far with a summary. An
    /// agent may ask for its own (`id` omitted); the orchestrator and the
    /// operator for anyone's.
    AgentCompact {
        #[serde(default)]
        id: Option<String>,
        /// What the summary must keep.
        #[serde(default)]
        keep: Option<String>,
    },
    /// Rendered terminal contents of an agent: its last `lines` lines, or
    /// with `screen` only what the terminal shows now. The screen is quick
    /// to read on every backend; scrollback is not (herdr walks it slowly).
    AgentOutput {
        id: String,
        #[serde(default)]
        lines: Option<usize>,
        #[serde(default)]
        screen: bool,
        /// With the terminal's colours, as SGR escape codes in the text
        /// (implies `screen`).
        #[serde(default)]
        ansi: bool,
    },
    /// What an agent was told, wrote and did, read from the session file
    /// its runtime keeps (not from the terminal): the newest `limit` entries.
    AgentTranscript {
        id: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Type into an agent's terminal (operator only).
    AgentInput {
        id: String,
        #[serde(default)]
        text: Option<String>,
        /// Submit `text` as a prompt (paste + Enter) instead of typing it raw.
        #[serde(default)]
        submit: bool,
    },
    /// Bring the agent's herdr pane into focus (herdr backend).
    AgentFocus {
        id: String,
    },
    /// Called by an agent: launch a subagent of its own with a task.
    AgentSpawn {
        /// Optional id; generated from the parent's id when omitted.
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        role: String,
        task: String,
        #[serde(default)]
        runtime: Option<RuntimeKind>,
        #[serde(default)]
        model: Option<String>,
    },

    Send(SendParams),
    Inbox {
        /// Operator only: whose inbox to read.
        #[serde(default)]
        agent: Option<String>,
        #[serde(default = "default_true")]
        unread_only: bool,
        /// Acknowledge everything returned.
        #[serde(default = "default_true")]
        ack: bool,
        #[serde(default)]
        limit: Option<usize>,
    },
    Ack {
        ids: Vec<String>,
    },
    /// Operator only: answer a question an agent put to the operator. The
    /// answer goes to the agent that asked (and the member it asked for).
    Answer {
        /// Message id of the question.
        id: String,
        body: String,
    },
    /// Agent only: turn this connection into the agent's delivery channel.
    /// The daemon pushes `Deliver` frames here instead of typing into the
    /// agent's terminal.
    #[serde(deserialize_with = "empty_params")]
    Listen,
    /// Agent only: the given messages were injected into the session.
    Delivered {
        ids: Vec<String>,
    },
    /// Agent only: the API key the crew config assigns to this agent.
    #[serde(deserialize_with = "empty_params")]
    Credential,
    History {
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// One message by id: one the calling agent sent or received (the
    /// operator: any), so a message can be pointed to rather than repeated.
    MessageGet {
        id: String,
    },

    StatusUpdate {
        status: AgentStatus,
        #[serde(default)]
        note: Option<String>,
    },
    /// The agent will not do what its current turn asked of it (it goes
    /// against its guidelines or usage policies): crew hands that work to
    /// another model when the turn ends, or sets the agent aside.
    Decline {
        reason: String,
    },
    Progress {
        percent: u8,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        task_id: Option<String>,
    },

    #[serde(deserialize_with = "empty_params")]
    TasksList,
    /// One task's card: the task and the tasks it builds on. Without an
    /// id, the calling agent's task in progress.
    TaskGet {
        #[serde(default)]
        id: Option<String>,
    },
    TaskCreate {
        spec: TaskSpec,
    },
    /// Orchestrator only: revise a task's card. Its assignee is told.
    TaskEdit {
        id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
    },
    TaskAssign {
        id: String,
        agent: String,
    },
    TaskComplete {
        #[serde(default)]
        task_id: Option<String>,
        #[serde(default)]
        result: String,
        #[serde(default)]
        artifacts: Vec<Artifact>,
    },
    TaskFail {
        #[serde(default)]
        task_id: Option<String>,
        reason: String,
    },
    /// Put a failed or blocked task back in the queue.
    TaskRetry {
        id: String,
    },

    ReviewRequest {
        agent_id: String,
        artifact: String,
        #[serde(default)]
        task_id: Option<String>,
        #[serde(default)]
        note: Option<String>,
    },
    ReviewSubmit {
        task_id: String,
        approve: bool,
        #[serde(default)]
        comments: String,
    },
    /// Orchestrator only: record the final summary of the session.
    /// `decisions` are what it leaves for the operator to decide; each is
    /// put to the operator as an open question.
    SessionComplete {
        summary: String,
        #[serde(default)]
        decisions: Vec<String>,
    },

    GitCommit {
        message: String,
        #[serde(default)]
        agent: Option<String>,
    },
    GitDiff {
        /// Revision to diff against (default: uncommitted changes).
        #[serde(default)]
        against: Option<String>,
        #[serde(default)]
        agent: Option<String>,
    },
    GitCherryPick {
        commit: String,
        #[serde(default)]
        agent: Option<String>,
    },
    /// Merge another agent's branch into the caller's.
    GitMerge {
        from_agent: String,
        #[serde(default)]
        agent: Option<String>,
    },
    GitStatus {
        #[serde(default)]
        agent: Option<String>,
    },
    /// Merge every agent branch into the integration branch.
    #[serde(deserialize_with = "empty_params")]
    Integrate,

    /// Lifecycle signal from a runtime hook (`crew hook <event>`).
    Hook {
        event: String,
        #[serde(default)]
        payload: Value,
        /// `CREW_LAUNCH` of the process that ran the hook: the daemon drops
        /// a hook of a launch that is not the agent's current one. Only
        /// hooks that carry it are told apart (one without it, from an
        /// older crew or run outside a launch, is taken as before); it
        /// correlates a hook with its launch and authenticates nothing.
        /// Refusing hooks without it would need a protocol bump.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        launch: Option<String>,
    },
    Events {
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },

    /// Turn this connection into a raw terminal attachment (pty backend).
    Attach {
        agent: String,
        rows: u16,
        cols: u16,
    },
    AttachInput {
        /// Base64 of the raw bytes typed.
        data: String,
    },
    AttachResize {
        rows: u16,
        cols: u16,
    },

    /// Operator, on the dashboard server's control socket: join a workspace
    /// (`workspace`: its state directory), every running session (`all`),
    /// or the memberships of a server being replaced (`restore`: state
    /// directories); serve the dashboard if it is not served yet; make a
    /// one-time pairing code for one browser. A daemon answers `invalid`.
    WebOpen {
        #[serde(default)]
        port: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        all: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restore: Option<Vec<String>>,
    },
    /// Operator, on the control socket: with `workspace` (its id or state
    /// directory), take it out of the dashboard; without, stop the server.
    /// A daemon answers `invalid`.
    WebClose {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    /// A browser, before `hello`: trade a pairing code for a session token.
    /// The dashboard server's; a daemon answers `invalid`.
    WebPair {
        code: String,
    },
    /// The control socket and a page's index connection: the dashboard's
    /// listing (`Workspaces`).
    #[serde(deserialize_with = "empty_params")]
    Workspaces,
    /// Operator, on the control socket: reconcile every joined workspace
    /// with the registry (a daemon says it started).
    #[serde(deserialize_with = "empty_params")]
    WebRescan,
    /// Operator: what a workspace switcher shows of this session. With
    /// `watch`, the connection then gets a `summary` push whenever the
    /// summary changed: the first change at once, then at most one per
    /// 250 ms.
    Summary {
        #[serde(default)]
        watch: bool,
    },
    /// Operator on the socket, not web: this connection is the dashboard
    /// server's watch. `SessionInfo.web` is `url` until it ends.
    WebAttach {
        url: String,
    },
    /// On the watch: something about the dashboard the session's log should
    /// hold.
    WebEvent {
        kind: WebEventKind,
        #[serde(default)]
        count: u32,
    },
    /// Operator, either transport: the dashboard's preferences for this
    /// workspace. With `set` they are replaced first. Answers
    /// `{"prefs": {…}}`.
    WebPrefs {
        #[serde(default)]
        set: Option<Value>,
    },
}

fn default_true() -> bool {
    true
}

/// No-argument methods accept the usual empty JSON-RPC object as well as
/// omitted/null params. Keep unit variants and their serialized wire shape.
fn empty_params<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(()),
        Value::Object(params) if params.is_empty() => Ok(()),
        _ => Err(serde::de::Error::custom(
            "this RPC method takes no parameters (expected null or an empty object)",
        )),
    }
}

impl Call {
    /// Method name, for logging.
    pub fn method(&self) -> &'static str {
        match self {
            Call::Ping => "ping",
            Call::Hello(_) => "hello",
            Call::Snapshot { .. } => "snapshot",
            Call::Subscribe => "subscribe",
            Call::LoadTeam { .. } => "load_team",
            Call::Shutdown { .. } => "shutdown",
            Call::AgentsList => "agents_list",
            Call::AgentAdd { .. } => "agent_add",
            Call::AgentKill { .. } => "agent_kill",
            Call::AgentRemove { .. } => "agent_remove",
            Call::AgentRestart { .. } => "agent_restart",
            Call::AgentRestartFresh { .. } => "agent_restart_fresh",
            Call::AgentSetModel { .. } => "agent_set_model",
            Call::ModelsList => "models_list",
            Call::UsageStatus => "usage_status",
            Call::AgentPause { .. } => "agent_pause",
            Call::AgentResume { .. } => "agent_resume",
            Call::AgentCompact { .. } => "agent_compact",
            Call::Resume { .. } => "resume",
            Call::AgentOutput { .. } => "agent_output",
            Call::AgentTranscript { .. } => "agent_transcript",
            Call::AgentInput { .. } => "agent_input",
            Call::AgentFocus { .. } => "agent_focus",
            Call::AgentSpawn { .. } => "agent_spawn",
            Call::Listen => "listen",
            Call::Delivered { .. } => "delivered",
            Call::Credential => "credential",
            Call::Send(_) => "send",
            Call::Inbox { .. } => "inbox",
            Call::Ack { .. } => "ack",
            Call::Answer { .. } => "answer",
            Call::History { .. } => "history",
            Call::MessageGet { .. } => "message_get",
            Call::StatusUpdate { .. } => "status_update",
            Call::Decline { .. } => "decline",
            Call::Progress { .. } => "progress",
            Call::TasksList => "tasks_list",
            Call::TaskGet { .. } => "task_get",
            Call::TaskCreate { .. } => "task_create",
            Call::TaskEdit { .. } => "task_edit",
            Call::TaskAssign { .. } => "task_assign",
            Call::TaskComplete { .. } => "task_complete",
            Call::TaskFail { .. } => "task_fail",
            Call::TaskRetry { .. } => "task_retry",
            Call::ReviewRequest { .. } => "review_request",
            Call::ReviewSubmit { .. } => "review_submit",
            Call::SessionComplete { .. } => "session_complete",
            Call::GitCommit { .. } => "git_commit",
            Call::GitDiff { .. } => "git_diff",
            Call::GitCherryPick { .. } => "git_cherry_pick",
            Call::GitMerge { .. } => "git_merge",
            Call::GitStatus { .. } => "git_status",
            Call::Integrate => "integrate",
            Call::Hook { .. } => "hook",
            Call::Events { .. } => "events",
            Call::Attach { .. } => "attach",
            Call::AttachInput { .. } => "attach_input",
            Call::AttachResize { .. } => "attach_resize",
            Call::WebOpen { .. } => "web_open",
            Call::WebClose { .. } => "web_close",
            Call::WebPair { .. } => "web_pair",
            Call::WebPrefs { .. } => "web_prefs",
            Call::Workspaces => "workspaces",
            Call::WebRescan => "web_rescan",
            Call::Summary { .. } => "summary",
            Call::WebAttach { .. } => "web_attach",
            Call::WebEvent { .. } => "web_event",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub call: Call,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new("unauthorized", message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new("forbidden", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new("conflict", message)
    }

    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new("too_large", message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new("unavailable", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }
}

/// One line from the daemon.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ServerFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_push"
    )]
    pub push: Option<Push>,
}

fn deserialize_push<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Push>, D::Error> {
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    // Classify just the tag first. serde(other) alone rejects non-null data
    // for its unit fallback. Known kinds must still validate their full data.
    if let Some(kind) = value.get("kind").and_then(Value::as_str) {
        if matches!(
            serde_json::from_value::<Push>(serde_json::json!({"kind": kind})),
            Ok(Push::Unknown)
        ) {
            return Ok(Some(Push::Unknown));
        }
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

impl ServerFrame {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id: Some(id),
            result: Some(result),
            ..Self::default()
        }
    }

    pub fn err(id: u64, error: RpcError) -> Self {
        Self {
            id: Some(id),
            error: Some(error),
            ..Self::default()
        }
    }

    pub fn push(push: Push) -> Self {
        Self {
            push: Some(push),
            ..Self::default()
        }
    }
}

/// State changes streamed to subscribers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Push {
    Event(Event),
    Message(Message),
    Agent(Box<Agent>),
    AgentRemoved {
        id: String,
    },
    Task(Box<Task>),
    Session(Box<SessionInfo>),
    /// The subscriber fell behind and missed pushes; fetch a new snapshot.
    Resync,
    /// Messages for a listening agent, already phrased for its session.
    Deliver {
        ids: Vec<String>,
        text: String,
    },
    /// For a listening agent: compact the session at the next turn boundary.
    Compact {
        #[serde(default)]
        instructions: Option<String>,
    },
    /// Raw terminal output for an attached connection (base64).
    Pty {
        data: String,
    },
    /// The attached agent's process ended.
    PtyClosed,
    /// The daemon is shutting down; on a page's index connection, the
    /// dashboard server.
    Shutdown,
    /// On a `summary {watch: true}` connection: the summary changed.
    Summary(WorkspaceSummary),
    /// On a page's index connection: the dashboard's listing changed.
    Workspaces(Workspaces),
    /// An additive push the peer understands but this client does not.
    /// Produced only while deserializing; never sent by the daemon.
    #[serde(other)]
    Unknown,
}

/// How much of one of an account's usage windows is spent, as the runtime
/// last reported it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Quota {
    pub runtime: RuntimeKind,
    /// `5h`, `week`, `spend`, or a window length such as `30d`.
    pub window: String,
    /// 0 to 100 (more on an exceeded spend limit).
    pub percent_used: f32,
    #[serde(default)]
    pub resets_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SessionInfo {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub objective: String,
    /// The work style the team was launched from, if any.
    #[serde(default)]
    pub style: Option<String>,
    /// Usage windows of the accounts the team runs on, where known.
    #[serde(default)]
    pub quotas: Vec<Quota>,
    pub workspace: String,
    pub state_dir: String,
    /// `pty`, `herdr`, or empty before a team is loaded.
    #[serde(default)]
    pub backend: String,
    /// `worktree` or `shared`.
    #[serde(default)]
    pub isolation: String,
    /// `off`, `required`, or empty before a team is loaded or from older peers.
    #[serde(default)]
    pub sandbox: String,
    /// The version control tool agents work with: `jj`, `git`, or empty when
    /// they share the checkout, before a team is loaded, or from older peers.
    #[serde(default)]
    pub vcs: String,
    /// Why `vcs` is git although jj is the default (the fallback's notice);
    /// empty otherwise.
    #[serde(default)]
    pub vcs_note: String,
    /// The web dashboard's address while it is served
    /// (`http://<label>.localhost:<port>/`), else empty. Never a code or a
    /// token.
    #[serde(default)]
    pub web: String,
    /// The daemon's startup warning when its bound socket uses the private
    /// /tmp fallback; empty for the preferred runtime directory or older peers.
    #[serde(default)]
    pub socket_warning: String,
    /// The daemon's warning when it could not keep a copy of its binary to
    /// hand its agents (a rebuild while the team runs breaks their tools);
    /// empty when it did, or from older peers.
    #[serde(default)]
    pub binary_warning: String,
    pub daemon_started_at: i64,
    #[serde(default)]
    pub team_started_at: Option<i64>,
    /// Set when every task finished.
    #[serde(default)]
    pub completed_at: Option<i64>,
    /// The orchestrator's final summary.
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub herdr_workspace: Option<String>,
    pub daemon_pid: u32,
    pub version: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Snapshot {
    pub session: SessionInfo,
    pub agents: Vec<Agent>,
    pub tasks: Vec<Task>,
    /// Oldest first.
    pub messages: Vec<Message>,
    /// Oldest first.
    pub events: Vec<Event>,
    /// Questions agents put to the operator that nobody answered yet,
    /// oldest first.
    #[serde(default)]
    pub questions: Vec<Message>,
    pub server_time: i64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct HelloInfo {
    pub role: Role,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
    /// On the web transport: the secret of the session whose token was
    /// presented; its SHA-256 is that session's verifier. Absent on the
    /// socket, and on any `hello` that failed. `Debug` leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_proof: Option<String>,
}

impl std::fmt::Debug for HelloInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelloInfo")
            .field("role", &self.role)
            .field("agent_id", &self.agent_id)
            .field("project", &self.project)
            .field("version", &self.version)
            .field("protocol", &self.protocol)
            .field("pid", &self.pid)
            .field("web_proof", &self.web_proof.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// What `web_open` answers. `Debug` leaves `pair_url` out.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct WebInfo {
    /// `http://<label>.localhost:<port>/`: this server's origin.
    pub url: String,
    pub port: u16,
    /// `url` with `#pair=<code>`: opens a browser session once.
    pub pair_url: String,
    pub expires_in_secs: u32,
    /// This call started the server (it was not running).
    pub started: bool,
    /// The id of the workspace this call was made for; absent for `--all`
    /// run outside a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// `url` with `#/w/<workspace>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_url: Option<String>,
    /// The ids this call joined (not those that were joined already).
    #[serde(default)]
    pub joined: Vec<String>,
}

impl std::fmt::Debug for WebInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebInfo")
            .field("url", &self.url)
            .field("port", &self.port)
            .field("pair_url", &"<redacted>")
            .field("expires_in_secs", &self.expires_in_secs)
            .field("started", &self.started)
            .field("workspace", &self.workspace)
            .field("workspace_url", &self.workspace_url)
            .field("joined", &self.joined)
            .finish()
    }
}

/// What the `workspaces` call answers and the `workspaces` push carries:
/// the whole listing, each time (notes/web-multi.md §multi-join).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Workspaces {
    /// The revision of this listing: greater than that of every listing the
    /// server made before it. A page applies only a greater one.
    pub rev: u64,
    /// Joined, in the order they were joined.
    pub joined: Vec<WorkspaceEntry>,
    /// Running sessions of this user that are not joined: names only.
    #[serde(default)]
    pub others: Vec<OtherSession>,
}

/// A joined workspace, as the dashboard server knows it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceEntry {
    /// 16 hex digits: the stem of its daemon's socket and registry entry.
    pub id: String,
    /// The membership's generation: greater after the workspace was taken
    /// out and joined again.
    pub generation: u64,
    /// The daemon the server's watch is on. Absent unless `running`.
    #[serde(default)]
    pub daemon: Option<DaemonRef>,
    #[serde(default)]
    pub project: Option<String>,
    /// The workspace directory.
    pub workspace: String,
    pub state: WorkspaceState,
    /// Why, for `incompatible` and `unreachable`; else empty.
    #[serde(default)]
    pub detail: String,
    /// The last summary its daemon gave; kept while it is stopped.
    #[serde(default)]
    pub summary: Option<WorkspaceSummary>,
    pub joined_at: i64,
}

/// One run of a workspace's daemon, as the registry publishes it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DaemonRef {
    pub pid: u32,
    pub started_at: i64,
}

/// A running session of this user that is not joined.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OtherSession {
    pub id: String,
    #[serde(default)]
    pub project: Option<String>,
    pub workspace: String,
}

/// What a workspace switcher shows of a session (`summary`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct WorkspaceSummary {
    /// Grows with every change of the summary, for the daemon's life.
    pub rev: u64,
    /// The team's name once one is loaded.
    #[serde(default)]
    pub project: Option<String>,
    pub agents: u32,
    /// Agents whose status is `working`.
    pub working: u32,
    /// Agents that cannot go on by themselves: `blocked` (whatever the
    /// kind) or `failed`.
    pub attention: u32,
    /// Questions put to the operator that nobody answered yet.
    pub questions: u32,
    pub tasks: u32,
    pub tasks_done: u32,
}

/// What `web_pair` answers. `Debug` leaves the token out.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct WebPaired {
    /// What the page presents in `hello`.
    pub token: String,
    /// SHA-256, in hex, of the secret the server shows in every `hello`
    /// reply of this session. Not a secret itself.
    pub verifier: String,
}

impl std::fmt::Debug for WebPaired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebPaired")
            .field("token", &"<redacted>")
            .field("verifier", &self.verifier)
            .finish()
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use crate::types::{EventLevel, TaskStatus, TranscriptKind};
    use serde_json::json;

    #[test]
    fn unit_rpc_methods_accept_empty_params_without_changing_their_wire_shape() {
        for call in [
            Call::Ping,
            Call::Subscribe,
            Call::AgentsList,
            Call::ModelsList,
            Call::UsageStatus,
            Call::Listen,
            Call::Credential,
            Call::TasksList,
            Call::Integrate,
            Call::Workspaces,
            Call::WebRescan,
        ] {
            let canonical = serde_json::to_value(&call).unwrap();
            assert!(canonical.get("params").is_none());
            for params in [Value::Null, json!({})] {
                let wire = json!({"id":7, "method":call.method(), "params":params});
                let request: Request = serde_json::from_value(wire.clone())
                    .unwrap_or_else(|error| panic!("{}: {error}", call.method()));
                assert_eq!(request.id, 7);
                assert_eq!(request.call, call);
                assert_eq!(serde_json::to_value(&request.call).unwrap(), canonical);
                let direct: Call = serde_json::from_value(wire).unwrap();
                assert_eq!(direct, call);
            }
            assert_eq!(serde_json::from_value::<Call>(canonical).unwrap(), call);
            for params in [
                json!({"unexpected":true}),
                json!([]),
                json!([null]),
                json!(false),
                json!(0),
                json!(""),
            ] {
                assert!(
                    serde_json::from_value::<Request>(
                        json!({"id":7, "method":call.method(), "params":params})
                    )
                    .is_err(),
                    "{} accepted nonempty params",
                    call.method()
                );
            }
        }
        for method in ["hello", "agent_restart", "agent_restart_fresh", "load_team"] {
            assert!(
                serde_json::from_value::<Request>(json!({"id":7,"method":method,"params":{}}))
                    .is_err(),
                "{method} lost required arguments"
            );
        }
        assert!(
            serde_json::from_value::<Request>(json!({"id":7,"method":"unknown","params":{}}))
                .is_err()
        );
        // The content may arrive before the tag in a JSON object.
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"params":{},"id":7,"method":"ping"}"#)
                .unwrap()
                .call,
            Call::Ping
        );
    }

    #[test]
    fn legacy_snapshots_default_sandbox_to_unknown_and_preserve_reported_modes() {
        let legacy = json!({
            "session":{"workspace":"/ws", "state_dir":"/ws/.crew", "daemon_started_at":0,
                "daemon_pid":1, "version":"old"},
            "agents":[], "tasks":[], "messages":[], "events":[], "server_time":0,
        });
        let snapshot: Snapshot = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(snapshot.session.sandbox, "");
        for mode in ["off", "required", "future"] {
            let mut wire = legacy.clone();
            wire["session"]["sandbox"] = json!(mode);
            let snapshot: Snapshot = serde_json::from_value(wire).unwrap();
            assert_eq!(snapshot.session.sandbox, mode);
            assert_eq!(
                serde_json::to_value(snapshot).unwrap()["session"]["sandbox"],
                mode
            );
        }
    }

    #[test]
    fn legacy_snapshots_without_a_socket_warning_remain_readable() {
        let legacy = json!({
            "session":{"workspace":"/ws", "state_dir":"/ws/.crew", "daemon_started_at":0,
                "daemon_pid":1, "version":"old"},
            "agents":[], "tasks":[], "messages":[], "events":[], "server_time":0,
        });
        let snapshot: Snapshot = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            serde_json::to_value(snapshot).unwrap()["session"]["socket_warning"],
            ""
        );
    }

    #[test]
    fn provider_events_and_legacy_agents_keep_the_snapshot_wire_shape() {
        let mut wire = json!({
            "session":{"workspace":"/ws", "state_dir":"/ws/.crew", "daemon_started_at":0,
                "daemon_pid":1, "version":"old"},
            "agents":[{"id":"a", "name":"a", "role":"worker", "status":"blocked",
                "runtime":"shell", "workdir":"/ws", "backend":"pty", "last_activity_at":0}],
            "tasks":[], "messages":[], "events":[], "server_time":0,
        });
        let old: Snapshot = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(old.agents[0].error, None);
        assert_eq!(old.agents[0].block_kind, None);
        for (event_type, block_kind) in [
            (crate::AGENT_ERROR_EVENT, "provider_error"),
            (crate::AGENT_REFUSED_EVENT, "refused"),
        ] {
            wire["agents"][0]["block_kind"] = json!(block_kind);
            wire["agents"][0]["error"] = json!("provider said: rate limit");
            wire["events"] = json!([{"seq":1, "timestamp":0, "source":"daemon",
                "event_type":event_type, "agent_id":"a", "message":"provider said: rate limit",
                "level":"error"}]);
            let snapshot: Snapshot = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(snapshot.events[0].event_type, event_type);
            assert_eq!(snapshot.events[0].level, EventLevel::Error);
            assert_eq!(
                snapshot.agents[0].error.as_deref(),
                Some("provider said: rate limit")
            );
            assert_eq!(snapshot.agents[0].block_kind.as_deref(), Some(block_kind));
        }
    }

    fn enum_values<T: Serialize>(values: &[T]) -> Value {
        serde_json::to_value(values).unwrap()
    }

    #[test]
    fn wire_enum_manifest_requires_a_protocol_bump() {
        // These matches intentionally require updates when variants are added.
        let role_tag = |role: Role| match role {
            Role::Operator => "operator",
            Role::Agent => "agent",
        };
        // Adding a variant also requires an instance in `pushes` and its kind
        // in the pinned version list below, together with a protocol bump.
        let push_tag = |push: &Push| match push {
            Push::Event(_) => "event",
            Push::Message(_) => "message",
            Push::Agent(_) => "agent",
            Push::AgentRemoved { .. } => "agent_removed",
            Push::Task(_) => "task",
            Push::Session(_) => "session",
            Push::Resync => "resync",
            Push::Deliver { .. } => "deliver",
            Push::Compact { .. } => "compact",
            Push::Pty { .. } => "pty",
            Push::PtyClosed => "pty_closed",
            Push::Shutdown => "shutdown",
            Push::Summary(_) => "summary",
            Push::Workspaces(_) => "workspaces",
            Push::Unknown => "unknown",
        };
        let pushes = vec![
            Push::Event(serde_json::from_value(json!({"seq":0,"timestamp":0,"source":"","event_type":"","message":"","level":"info"})).unwrap()),
            Push::Message(serde_json::from_value(json!({"id":"m","from":"a","to":"b","type":"message","body":"","timestamp":0})).unwrap()),
            Push::Agent(Box::new(serde_json::from_value(json!({"id":"a","name":"a","role":"","status":"idle","runtime":"shell","workdir":"","backend":"pty","last_activity_at":0})).unwrap())),
            Push::AgentRemoved { id: "a".into() },
            Push::Task(Box::new(serde_json::from_value(json!({"id":"t","title":"","description":"","status":"queued","created_at":0})).unwrap())),
            Push::Session(Box::default()), Push::Resync,
            Push::Deliver { ids: vec![], text: String::new() },
            Push::Compact { instructions: None }, Push::Pty { data: String::new() },
            Push::PtyClosed, Push::Shutdown,
            Push::Summary(WorkspaceSummary::default()), Push::Workspaces(Workspaces::default()),
            Push::Unknown,
        ];
        let push_kinds: Vec<_> = pushes
            .iter()
            .map(|push| {
                let tag = serde_json::to_value(push).unwrap()["kind"].clone();
                assert_eq!(
                    tag,
                    push_tag(push),
                    "update the manifest and bump PROTOCOL_VERSION"
                );
                tag
            })
            .collect();
        let roles = [Role::Operator, Role::Agent];
        for role in roles {
            assert_eq!(serde_json::to_value(role).unwrap(), role_tag(role));
        }
        let actual = json!({
            "agent_status": enum_values(AgentStatus::ALL), "task_status": enum_values(TaskStatus::ALL),
            "message_type": enum_values(MessageType::ALL), "runtime_kind": enum_values(RuntimeKind::ALL),
            "event_level": enum_values(EventLevel::ALL), "transcript_kind": enum_values(TranscriptKind::ALL),
            "role": enum_values(&roles), "push": push_kinds,
            "workspace_state": enum_values(WorkspaceState::ALL),
            "web_event_kind": enum_values(WebEventKind::ALL),
        });
        let version_6 = json!({
            "agent_status": ["spawning","idle","working","waiting","blocked","done","failed","stopped"],
            "task_status": ["queued","ready","working","waiting","blocked","review","done","failed"],
            "message_type": ["task","message","question","answer","status","review_request","review_result","artifact","error"],
            "runtime_kind": ["claude","codex","pi","shell"], "event_level": ["debug","info","warn","error"],
            "transcript_kind": ["prompt","reply","tool","note"], "role": ["operator","agent"],
            "push": ["event","message","agent","agent_removed","task","session","resync","deliver","compact","pty","pty_closed","shutdown","summary","workspaces","unknown"],
            "workspace_state": ["running","stopped","incompatible","unreachable"],
            "web_event_kind": ["paired","auth_failed"],
        });
        assert_eq!(
            (PROTOCOL_VERSION, actual),
            (6, version_6),
            "bump PROTOCOL_VERSION and update this list"
        );
    }

    #[test]
    fn the_web_calls_round_trip_and_their_secrets_stay_out_of_debug() {
        for (call, wire) in [
            (
                Call::WebOpen {
                    port: Some(23417),
                    workspace: Some("/w/payments/.crew".into()),
                    all: false,
                    restore: None,
                },
                json!({"method":"web_open","params":{"port":23417,"workspace":"/w/payments/.crew"}}),
            ),
            (
                Call::WebOpen {
                    port: None,
                    workspace: None,
                    all: true,
                    restore: None,
                },
                json!({"method":"web_open","params":{"port":null,"all":true}}),
            ),
            (
                Call::WebOpen {
                    port: None,
                    workspace: None,
                    all: false,
                    restore: Some(vec!["/w/a/.crew".into(), "/w/b/.crew".into()]),
                },
                json!({"method":"web_open","params":{"port":null,"restore":["/w/a/.crew","/w/b/.crew"]}}),
            ),
            (
                Call::WebClose { workspace: None },
                json!({"method":"web_close","params":{}}),
            ),
            (
                Call::WebClose {
                    workspace: Some("946d4617aa702870".into()),
                },
                json!({"method":"web_close","params":{"workspace":"946d4617aa702870"}}),
            ),
            (
                Call::Summary { watch: true },
                json!({"method":"summary","params":{"watch":true}}),
            ),
            (
                Call::WebAttach {
                    url: "http://c7f3.localhost:23417/".into(),
                },
                json!({"method":"web_attach","params":{"url":"http://c7f3.localhost:23417/"}}),
            ),
            (
                Call::WebEvent {
                    kind: WebEventKind::AuthFailed,
                    count: 7,
                },
                json!({"method":"web_event","params":{"kind":"auth_failed","count":7}}),
            ),
            (
                Call::WebPair {
                    code: "c0de".into(),
                },
                json!({"method":"web_pair","params":{"code":"c0de"}}),
            ),
            (
                Call::WebPrefs {
                    set: Some(json!({"keys": false})),
                },
                json!({"method":"web_prefs","params":{"set":{"keys":false}}}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&call).unwrap(), wire);
            assert_eq!(serde_json::from_value::<Call>(wire.clone()).unwrap(), call);
            assert_eq!(wire["method"], call.method());
        }
        // Optional parameters may be left out.
        let open: Call = serde_json::from_value(json!({"method":"web_open","params":{}})).unwrap();
        assert_eq!(
            open,
            Call::WebOpen {
                port: None,
                workspace: None,
                all: false,
                restore: None
            }
        );
        let close: Call =
            serde_json::from_value(json!({"method":"web_close","params":{}})).unwrap();
        assert_eq!(close, Call::WebClose { workspace: None });
        let summary: Call =
            serde_json::from_value(json!({"method":"summary","params":{}})).unwrap();
        assert_eq!(summary, Call::Summary { watch: false });
        let prefs: Call =
            serde_json::from_value(json!({"method":"web_prefs","params":{}})).unwrap();
        assert_eq!(prefs, Call::WebPrefs { set: None });

        let info = WebInfo {
            url: "http://c7f3.localhost:23417/".into(),
            port: 23417,
            pair_url: "http://c7f3.localhost:23417/#pair=SECRETCODE".into(),
            expires_in_secs: 120,
            started: true,
            workspace: Some("946d4617aa702870".into()),
            workspace_url: Some("http://c7f3.localhost:23417/#/w/946d4617aa702870/".into()),
            joined: vec!["946d4617aa702870".into()],
        };
        let paired = WebPaired {
            token: "SECRETTOKEN".into(),
            verifier: "abc123".into(),
        };
        let hello = HelloInfo {
            role: Role::Operator,
            agent_id: None,
            project: None,
            version: "0".into(),
            protocol: PROTOCOL_VERSION,
            pid: 1,
            web_proof: Some("SECRETPROOF".into()),
        };
        for printed in [
            format!("{info:?}"),
            format!("{paired:?}"),
            format!("{hello:?}"),
        ] {
            assert!(!printed.contains("SECRET"), "{printed}");
        }
        assert!(format!("{paired:?}").contains("abc123"));
        assert_eq!(
            serde_json::to_value(&hello).unwrap()["web_proof"],
            "SECRETPROOF"
        );
        // Without a proof the field is not on the wire, and a reply without
        // it still reads.
        let plain = HelloInfo {
            web_proof: None,
            ..hello
        };
        let wire = serde_json::to_value(&plain).unwrap();
        assert!(wire.get("web_proof").is_none());
        assert_eq!(serde_json::from_value::<HelloInfo>(wire).unwrap(), plain);
    }

    #[test]
    fn a_session_without_a_web_address_still_reads() {
        let legacy = json!({"workspace":"/ws", "state_dir":"/ws/.crew", "daemon_started_at":0,
            "daemon_pid":1, "version":"old"});
        let session: SessionInfo = serde_json::from_value(legacy).unwrap();
        assert_eq!(session.web, "");
    }

    #[test]
    fn a_hello_says_web_and_workspace_only_when_it_has_them() {
        let plain =
            json!({"role":"operator","agent_id":null,"token":"t","client":"cli","protocol":6});
        let hello: Hello = serde_json::from_value(plain.clone()).unwrap();
        assert!(!hello.web);
        assert_eq!(hello.workspace, None);
        assert_eq!(serde_json::to_value(&hello).unwrap(), plain);
        let server = json!({"role":"operator","agent_id":null,"token":"t","client":"web","protocol":6,"web":true});
        let hello: Hello = serde_json::from_value(server.clone()).unwrap();
        assert!(hello.web);
        assert_eq!(serde_json::to_value(&hello).unwrap(), server);
        let page = json!({"role":"operator","agent_id":null,"token":"t","client":"web","protocol":6,
            "workspace":"946d4617aa702870"});
        let hello: Hello = serde_json::from_value(page.clone()).unwrap();
        assert_eq!(hello.workspace.as_deref(), Some("946d4617aa702870"));
        assert_eq!(serde_json::to_value(&hello).unwrap(), page);
        // `WebInfo` as protocol 5 wrote it still reads.
        let old = json!({"url":"http://c7f3.localhost:23417/","port":23417,
            "pair_url":"http://c7f3.localhost:23417/#pair=x","expires_in_secs":120,"started":true});
        let info: WebInfo = serde_json::from_value(old).unwrap();
        assert_eq!((info.workspace, info.joined.len()), (None, 0));
    }

    #[test]
    fn a_listing_round_trips_with_every_state() {
        let listing = Workspaces {
            rev: 7,
            joined: WorkspaceState::ALL
                .iter()
                .enumerate()
                .map(|(i, state)| WorkspaceEntry {
                    id: format!("{i:016x}"),
                    generation: i as u64 + 1,
                    daemon: (*state == WorkspaceState::Running).then_some(DaemonRef {
                        pid: 4242,
                        started_at: 1_791_000_000_000,
                    }),
                    project: Some(format!("p{i}")),
                    workspace: format!("/w/{i}"),
                    state: *state,
                    detail: String::new(),
                    summary: Some(WorkspaceSummary {
                        rev: 3,
                        agents: 2,
                        ..WorkspaceSummary::default()
                    }),
                    joined_at: 1,
                })
                .collect(),
            others: vec![OtherSession {
                id: "0b54c1d2e3f4a596".into(),
                project: None,
                workspace: "/w/docs".into(),
            }],
        };
        let wire = serde_json::to_value(Push::Workspaces(listing.clone())).unwrap();
        assert_eq!(wire["kind"], "workspaces");
        assert_eq!(wire["data"]["joined"][2]["state"], "incompatible");
        assert_eq!(
            serde_json::from_value::<Push>(wire).unwrap(),
            Push::Workspaces(listing)
        );
        let summary = serde_json::to_value(Push::Summary(WorkspaceSummary::default())).unwrap();
        assert_eq!(summary["kind"], "summary");
    }

    #[test]
    fn a_fresh_restart_is_its_own_method_not_a_flag_on_restart() {
        let fresh = Call::AgentRestartFresh { id: "a".into() };
        let wire = serde_json::to_value(&fresh).unwrap();
        assert_eq!(
            wire,
            json!({"method":"agent_restart_fresh","params":{"id":"a"}})
        );
        assert_eq!(fresh.method(), "agent_restart_fresh");
        assert!(matches!(
            serde_json::from_value(wire).unwrap(),
            Call::AgentRestartFresh { id } if id == "a"
        ));
        let plain: Call =
            serde_json::from_value(json!({"method":"agent_restart","params":{"id":"a"}})).unwrap();
        assert!(matches!(plain, Call::AgentRestart { id } if id == "a"));
    }

    #[test]
    fn a_hook_says_its_launch_only_when_it_has_one() {
        // As an older crew sends it: no launch.
        let old: Call =
            serde_json::from_value(json!({"method":"hook","params":{"event":"stop"}})).unwrap();
        assert_eq!(
            old,
            Call::Hook {
                event: "stop".into(),
                payload: Value::Null,
                launch: None,
            }
        );
        // Without a launch the frame is the old one.
        assert_eq!(
            serde_json::to_value(&old).unwrap(),
            json!({"method":"hook","params":{"event":"stop","payload":null}})
        );
        let marked = Call::Hook {
            event: "stop".into(),
            payload: json!({"session_id":"s"}),
            launch: Some("l-1".into()),
        };
        let wire = serde_json::to_value(&marked).unwrap();
        assert_eq!(wire["params"]["launch"], "l-1");
        assert_eq!(serde_json::from_value::<Call>(wire).unwrap(), marked);
    }

    #[test]
    fn future_push_payloads_do_not_break_frames() {
        for data in [json!({"x": 1}), json!([1, 2]), json!("future"), json!(null)] {
            let frame: ServerFrame =
                serde_json::from_value(json!({"push": {"kind": "future", "data": data}})).unwrap();
            assert_eq!(
                serde_json::to_value(frame.push.unwrap()).unwrap(),
                json!({"kind": "unknown"})
            );
        }
        for push in [
            json!({"kind": "deliver", "data": {"ids": 1}}),
            json!({"kind": "agent_removed", "data": {}}),
            json!({"data": 1}),
        ] {
            assert!(serde_json::from_value::<ServerFrame>(json!({"push": push})).is_err());
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SendResult {
    pub message: Message,
    /// The id was seen before; nothing new was sent.
    pub duplicate: bool,
    pub recipients: Vec<String>,
}

/// A task as a card on the board: the task, and the tasks it depends on
/// (their results and artifacts are what it builds on).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TaskCard {
    pub task: Task,
    #[serde(default)]
    pub dependencies: Vec<Task>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct InboxResult {
    pub messages: Vec<InboxEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct CommitInfo {
    pub sha: String,
    pub short: String,
    pub branch: String,
    pub summary: String,
    pub files_changed: usize,
    /// jj's change id of the commit; None under git.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ConflictReport {
    /// `cherry-pick` or `merge`.
    pub operation: String,
    pub files: Vec<String>,
    pub detail: String,
}

/// Outcome of a git operation that can hit a conflict.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct GitOutcome {
    pub ok: bool,
    #[serde(default)]
    pub commit: Option<CommitInfo>,
    #[serde(default)]
    pub conflict: Option<ConflictReport>,
    #[serde(default)]
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct OutputResult {
    pub text: String,
    #[serde(default)]
    pub alive: bool,
}

/// One model an agent can be put on: what `models_list` answers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    /// The CLI that runs it.
    pub runtime: RuntimeKind,
    /// The name to ask for (`opus`, `gpt-6-astra`, `anthropic/claude-…`);
    /// empty for the CLI's own default model.
    pub model: String,
    /// Its proper name, for display.
    pub name: String,
    /// `frontier`, `strong`, `balanced` or `fast`, when known.
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub price: String,
    /// The vendor's one-line description.
    #[serde(default)]
    pub summary: String,
    /// Why it cannot be launched from this machine; `None` when it can.
    #[serde(default)]
    pub unavailable: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ModelsResult {
    /// Claude Code's, then Codex's, then pi's, each in the catalog's order.
    pub models: Vec<ModelChoice>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct TranscriptResult {
    /// Oldest first.
    pub entries: Vec<crate::types::TranscriptEntry>,
    /// The runtime keeps a session file and it was found.
    #[serde(default)]
    pub available: bool,
    /// The session is longer than what is returned.
    #[serde(default)]
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format() {
        let req = Request {
            id: 7,
            call: Call::Send(SendParams {
                to: "claude-5".into(),
                body: "hello".into(),
                ..SendParams::default()
            }),
        };
        let v: Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "send");
        assert_eq!(v["params"]["to"], "claude-5");
        assert_eq!(v["params"]["type"], "message");
        let back: Request = serde_json::from_value(v).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn unit_calls_need_no_params() {
        let req: Request = serde_json::from_str(r#"{"id":1,"method":"ping"}"#).unwrap();
        assert_eq!(req.call, Call::Ping);
        let req: Request =
            serde_json::from_str(r#"{"id":2,"method":"inbox","params":{}}"#).unwrap();
        assert_eq!(
            req.call,
            Call::Inbox {
                agent: None,
                unread_only: true,
                ack: true,
                limit: None,
            }
        );
    }

    #[test]
    fn frames_round_trip() {
        let f = ServerFrame::push(Push::AgentRemoved { id: "a".into() });
        let s = serde_json::to_string(&f).unwrap();
        assert_eq!(s, r#"{"push":{"kind":"agent_removed","data":{"id":"a"}}}"#);
        let back: ServerFrame = serde_json::from_str(&s).unwrap();
        assert_eq!(back.push, f.push);

        let e = ServerFrame::err(3, RpcError::forbidden("no"));
        let back: ServerFrame = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back.error.unwrap().code, "forbidden");
    }
}
