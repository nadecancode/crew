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
};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The human at the CLI or dashboard.
    Operator,
    /// A process acting as one specific agent (MCP bridge, `crew ctl`, hooks).
    Agent,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
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
    ModelsList,
    /// The accounts' usage windows, which models are out until they reset
    /// and what to take instead, as text (orchestrator or operator).
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
    Listen,
    /// Agent only: the given messages were injected into the session.
    Delivered {
        ids: Vec<String>,
    },
    /// Agent only: the API key the crew config assigns to this agent.
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
    Integrate,

    /// Lifecycle signal from a runtime hook (`crew hook <event>`).
    Hook {
        event: String,
        #[serde(default)]
        payload: Value,
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
}

fn default_true() -> bool {
    true
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<Push>,
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
    AgentRemoved { id: String },
    Task(Box<Task>),
    Session(Box<SessionInfo>),
    /// The subscriber fell behind and missed pushes; fetch a new snapshot.
    Resync,
    /// Messages for a listening agent, already phrased for its session.
    Deliver { ids: Vec<String>, text: String },
    /// For a listening agent: compact the session at the next turn boundary.
    Compact {
        #[serde(default)]
        instructions: Option<String>,
    },
    /// Raw terminal output for an attached connection (base64).
    Pty { data: String },
    /// The attached agent's process ended.
    PtyClosed,
    /// The daemon is shutting down.
    Shutdown,
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HelloInfo {
    pub role: Role,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
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
