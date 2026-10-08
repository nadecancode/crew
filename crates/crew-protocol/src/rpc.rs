//! Wire protocol: newline-delimited JSON over the daemon's Unix socket.
//!
//! A client sends `Request` lines (`{"id":1,"method":"send","params":{..}}`)
//! and receives `ServerFrame` lines: either the response to a request
//! (`{"id":1,"result":..}` / `{"id":1,"error":..}`) or, after `subscribe`,
//! unsolicited pushes (`{"push":{..}}`).
//!
//! The same calls back the MCP tools, `crew ctl` and the dashboard, so adding
//! another front end never needs a protocol change.
//!
//! Host configuration-bearing calls and their request envelope remain in
//! `crew-core`; its rpc module reexports these unchanged wire definitions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{
    Agent, Event, InboxEntry, Message, MessageType, RuntimeKind, Task, WorkspaceState,
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct RpcError {
    pub code: String,
    pub message: String,
    /// What a client can act on beyond the message, by code. `protocol`
    /// (a refused `hello`): the daemon's `version`, `protocol` and
    /// `workspace`. Kept loose, since a client reads it from a daemon of
    /// another protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
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
        /// Authenticated operator steering. Ordinary traffic waits for a turn
        /// boundary and is submitted through the native prompt queue.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        immediate: bool,
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
mod delivery_tests {
    use super::Push;
    use serde_json::json;

    #[test]
    fn ordinary_delivery_keeps_the_existing_wire_shape() {
        let old = json!({ "kind": "deliver", "data": { "ids": ["m-1"], "text": "hello" } });
        let push: Push = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(
            push,
            Push::Deliver {
                ids: vec!["m-1".into()],
                text: "hello".into(),
                immediate: false,
            }
        );
        assert_eq!(serde_json::to_value(push).unwrap(), old);
    }

    #[test]
    fn operator_steering_round_trips_as_explicit_metadata() {
        let push = Push::Deliver {
            ids: vec!["m-2".into()],
            text: "correction".into(),
            immediate: true,
        };
        let wire = serde_json::to_value(&push).unwrap();
        assert_eq!(wire["data"]["immediate"], true);
        assert_eq!(serde_json::from_value::<Push>(wire).unwrap(), push);
    }
}
