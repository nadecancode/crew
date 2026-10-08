//! Host RPC calls and request envelopes for the Crew daemon.
//!
//! Configuration-free protocol records retain their existing public paths
//! through reexports from `crew-protocol`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{AgentSpec, TaskSpec, TeamConfig};
use crate::types::{AgentStatus, Artifact, RuntimeKind, WebEventKind};

pub use crew_protocol::rpc::*;

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
        spec: Box<AgentSpec>,
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
        /// Bundles by name: the subagent gets only these, each item equal to
        /// one in the parent's active toolbox. Left out: a copy of the
        /// parent's active toolbox.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        toolbox: Option<Vec<String>>,
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
    /// Agent only: how `crew toolbox mcp <name>` reaches one MCP server of
    /// the caller's active toolbox, its references resolved. Like
    /// `Credential`, the caller is the authenticated agent, never a
    /// parameter. Answered with a [`ToolboxServerLaunch`].
    ToolboxServer {
        name: String,
        /// Asked by the preflight before the session starts, not by the
        /// session's proxy.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        preflight: bool,
    },
    /// Agent only: what `crew toolbox mcp <name>` saw of its server after
    /// the session started.
    ToolboxReport {
        name: String,
        kind: ToolboxReportKind,
        #[serde(default)]
        detail: String,
    },
    /// Members' toolboxes as they show them (operator or orchestrator):
    /// every member when `agent` is left out. With `runtime`, how each
    /// member's configuration would map onto that runtime at its next
    /// launch, never the toolbox its session runs with.
    Toolbox {
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        runtime: Option<RuntimeKind>,
    },
    /// Operator: resolve every member's toolbox in `config` (a team file the
    /// CLI loaded and validated) against the daemon's environment and
    /// configuration, without starting anything. Answered with the members'
    /// toolboxes, every item Planned or why it would not load.
    ToolboxCheck {
        config: Box<TeamConfig>,
    },
    /// Operator: take the `toolboxes` and `toolbox` fields of `config` into the
    /// running team. Running sessions keep theirs until a restart. Answered
    /// with a [`ToolboxReload`].
    ToolboxReload {
        config: Box<TeamConfig>,
    },
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
        #[serde(default)]
        task_id: Option<String>,
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
            Call::ToolboxServer { .. } => "toolbox_server",
            Call::ToolboxReport { .. } => "toolbox_report",
            Call::Toolbox { .. } => "toolbox",
            Call::ToolboxCheck { .. } => "toolbox_check",
            Call::ToolboxReload { .. } => "toolbox_reload",
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

/// The answer to [`Call::ToolboxServer`]: one toolbox MCP server, ready to
/// start or connect to. It carries resolved secret values: it lives in the
/// memory of `crew toolbox mcp` only, and its `Debug` leaves them out.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ToolboxServerLaunch {
    pub name: String,
    pub upstream: Upstream,
    /// Exact values replaced in the server's responses by `[redacted $NAME]`.
    #[serde(default)]
    pub redact: Vec<Redaction>,
    /// The member's secret names (reference and source names): removed from
    /// the environment the server inherits, whatever it was given.
    #[serde(default)]
    pub scrub: Vec<String>,
    /// SHA-256 (hex) of the item's canonical definition: a pinned snapshot
    /// taken for another definition is replaced.
    pub definition: String,
    /// Where the server's pinned tool-definition snapshot lives
    /// (`<agent dir>/toolbox/mcp/<name>.json`).
    pub snapshot: String,
    /// Take a new snapshot even when the definition is unchanged
    /// (`crew resume --restart`).
    #[serde(default)]
    pub refresh: bool,
}

impl std::fmt::Debug for ToolboxServerLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolboxServerLaunch")
            .field("name", &self.name)
            .field("upstream", &self.upstream)
            .field(
                "redact",
                &self.redact.iter().map(|r| &r.name).collect::<Vec<_>>(),
            )
            .field("scrub", &self.scrub)
            .field("definition", &self.definition)
            .field("snapshot", &self.snapshot)
            .field("refresh", &self.refresh)
            .finish()
    }
}

/// How a toolbox MCP server is reached.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Upstream {
    /// A local server: started as a child of `crew toolbox mcp`.
    Stdio {
        command: Vec<String>,
        /// Absolute.
        #[serde(default)]
        cwd: Option<String>,
        /// Added to the scrubbed environment, values resolved.
        #[serde(default)]
        env: std::collections::BTreeMap<String, String>,
    },
    /// A remote server, bridged from stdio.
    Http {
        url: String,
        #[serde(default)]
        transport: crate::toolbox::McpTransport,
        /// Values resolved.
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
    },
}

impl std::fmt::Debug for Upstream {
    /// Names only: the values may be secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Upstream::Stdio { command, cwd, env } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("cwd", cwd)
                .field("env", &env.keys().collect::<Vec<_>>())
                .finish(),
            Upstream::Http {
                url,
                transport,
                headers,
            } => f
                .debug_struct("Http")
                .field("url", url)
                .field("transport", transport)
                .field("headers", &headers.keys().collect::<Vec<_>>())
                .finish(),
        }
    }
}

/// A resolved value and the reference name it is shown as.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Redaction {
    pub name: String,
    pub value: String,
}

/// What `crew toolbox mcp` tells the daemon about its server in session.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolboxReportKind {
    /// The server exited or its connection failed: Failed in session.
    Failed,
    /// A listing differs from the pinned snapshot; the snapshot is kept.
    ListingDiffers,
    /// The server lists tools the pinned snapshot does not expose.
    NewTools,
}

/// The answer to [`Call::ToolboxReload`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ToolboxReload {
    /// Members whose toolbox a restart would change.
    #[serde(default)]
    pub pending: Vec<PendingToolbox>,
    /// Team-file fields other than toolboxes that differ from the running
    /// team: reported, not applied.
    #[serde(default)]
    pub ignored: Vec<String>,
}

/// One member's toolbox, as [`Call::Toolbox`] and [`Call::ToolboxCheck`]
/// answer it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MemberToolbox {
    pub agent: String,
    /// The runtime the toolbox is mapped onto.
    pub runtime: RuntimeKind,
    pub toolbox: crate::types::AgentToolbox,
    /// Why a launch with this toolbox would be refused (`crew toolbox check`
    /// exits non-zero when any member has one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PendingToolbox {
    pub agent: String,
    /// The active fingerprint, if the member has run.
    #[serde(default)]
    pub from: Option<String>,
    pub to: String,
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use crate::types::{EventLevel, MessageType, TaskStatus, TranscriptKind, WorkspaceState};
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
            Push::Deliver { ids: vec![], text: String::new(), immediate: false, batch_id: None },
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
