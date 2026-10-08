//! Portable client records and the shared engine interface.
//!
//! This module defines data and method signatures. It supplies no engine,
//! transport, action planner or projection implementation. Native hosts keep IO,
//! credentials, clocks and rendering outside this crate. Serialization of these
//! records is a client schema; protocol request encoding belongs to `commands`.

use std::fmt;

use crew_protocol::rpc::{
    GitOutcome, InboxResult, ModelsResult, OutputResult, Push, Quota, SendParams, SendResult,
    SessionInfo, Snapshot, TaskCard, TranscriptResult, Workspaces,
};
use crew_protocol::{
    Agent, ContextUsage, Event, EventLevel, Message, MessageType, RuntimeKind, Task, TaskStatus,
    Usage,
};
use serde::{Deserialize, Serialize};

use crate::CurrentWork;

pub type ClientRevision = u64;
pub type OperationId = u64;
pub type ConfirmationId = u64;
pub type TimerId = u64;
pub type AttachmentId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockSample {
    pub wall_ms: i64,
    pub monotonic_ms: u64,
}

/// Whole asynchronous identity. Missing daemon/listing facts remain absent.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClientScope {
    pub endpoint_id: String,
    pub workspace_id: String,
    pub generation: Option<u64>,
    pub daemon_pid: Option<u32>,
    pub daemon_started_at_ms: Option<i64>,
    pub attempt: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointStatus {
    Disconnected,
    Connecting,
    Authenticated,
    Reconnecting,
    SignedOut,
    Incompatible,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    NoneSelected,
    Connecting,
    Live,
    Reconnecting,
    Stopped,
    Unreachable,
    Incompatible,
    Removed,
    NeverJoined,
    AccessRevoked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionMode {
    LocalOperator,
    ScopedGateway,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotMode {
    OrderedConnection,
    BufferedOverlap,
}

/// Bounds are validated when a core instance is constructed, not inferred from
/// the collections received from a daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientLimits {
    pub messages: u32,
    pub events: u32,
    pub context_points: u32,
    pub buffered_pushes: u32,
    pub pending_operations: u32,
    pub page_size: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    pub limits: ClientLimits,
    pub snapshot_mode: SnapshotMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Entity {
    Workspace,
    Agent(String),
    Task(String),
    Message(String),
    Event(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntityKey {
    pub scope: ClientScope,
    pub entity: Entity,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub revision: ClientRevision,
    pub agents: Vec<String>,
    pub tasks: Vec<String>,
    pub removed: Vec<EntityKey>,
    pub session: bool,
    pub questions: bool,
    pub messages: bool,
    pub events: bool,
    pub usage: bool,
    pub availability: bool,
    pub pending: bool,
    pub reset: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantTier {
    Read,
    Control,
    Terminal,
}

/// Preflight/presentation authority supplied by an authenticated connector.
/// Server authorization remains authoritative for every request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub grant_id: String,
    pub tiers: Vec<GrantTier>,
    pub workspace_ids: Vec<String>,
    pub expires_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionContext {
    pub mode: ConnectionMode,
    pub grant: Grant,
    pub snapshot_mode: SnapshotMode,
    pub scope: ClientScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
    pub project: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientReason {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ClientInput {
    Bind {
        scope: ClientScope,
        mode: ConnectionMode,
        grant: Grant,
        snapshot_mode: SnapshotMode,
    },
    Unbind {
        scope: ClientScope,
        reason: ClientReason,
    },
    EndpointChanged {
        endpoint_id: String,
        attempt: u64,
        status: EndpointStatus,
    },
    Listing {
        endpoint_id: String,
        attempt: u64,
        listing: Workspaces,
    },
    SnapshotStarted {
        scope: ClientScope,
        operation_id: OperationId,
    },
    Snapshot {
        scope: ClientScope,
        operation_id: OperationId,
        snapshot: Box<Snapshot>,
    },
    Push {
        scope: ClientScope,
        push: Push,
    },
    OperationFinished {
        scope: ClientScope,
        operation_id: OperationId,
        outcome: OperationOutcome,
    },
    AttachmentEnded {
        scope: ClientScope,
        attachment_id: AttachmentId,
        reason: TerminalEnd,
    },
    Wake {
        timer_id: TimerId,
        monotonic_ms: u64,
    },
    Lifecycle {
        active: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Loading,
    Fresh,
    Stale,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarningFact {
    pub code: String,
    pub text: String,
    pub entity: Option<EntityKey>,
}

/// The warning code for a member whose toolbox needs attention. Inspectors
/// that show the Toolbox section leave it out of their header warnings.
pub const TOOLBOX_WARNING: &str = "toolbox";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionAvailability {
    pub action: ActionKind,
    pub available: bool,
    pub reason: Option<ClientReason>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentPresentation {
    pub agent: Agent,
    pub status_label: String,
    pub tone: Tone,
    pub current_work: Option<CurrentWork>,
    pub waiting_task_ids: Vec<String>,
    pub satellite_parent_id: Option<String>,
    pub subagent_ids: Vec<String>,
    pub warnings: Vec<WarningFact>,
    pub actions: Vec<ActionAvailability>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyState {
    pub id: String,
    pub status: Option<TaskStatus>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskPresentation {
    pub task: Task,
    pub dependencies: Vec<DependencyState>,
    pub layer: u32,
    pub in_cycle: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRelation {
    pub parent_id: String,
    pub child_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLayer {
    pub index: u32,
    pub task_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverviewData {
    pub session: Option<SessionInfo>,
    pub agent_count: u32,
    pub working_count: u32,
    pub task_count: u32,
    pub done_count: u32,
    pub question_count: u32,
    pub warnings: Vec<WarningFact>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TeamData {
    pub agents: Vec<AgentPresentation>,
    pub relations: Vec<AgentRelation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TasksData {
    pub tasks: Vec<TaskPresentation>,
    pub layers: Vec<TaskLayer>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentDetailData {
    pub agent: Option<AgentPresentation>,
    pub assigned_tasks: Vec<TaskPresentation>,
    pub review_tasks: Vec<TaskPresentation>,
    pub recent_messages: MessagePage,
    pub resources: Vec<ResourceState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskDetailData {
    pub task: Option<TaskPresentation>,
    pub card: ResourceState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum MessageEndpoints {
    All,
    Agent { id: String },
    Pair { first: String, second: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFilter {
    pub endpoints: MessageEndpoints,
    pub kind: Option<MessageType>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFilter {
    pub agent_id: Option<String>,
    pub level: Option<EventLevel>,
    pub text: String,
    pub vcs_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HistoryAnchor {
    Message {
        id: String,
        timestamp_ms: i64,
        ordinal: u64,
    },
    Event {
        seq: u64,
        ordinal: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub scope: ClientScope,
    pub revision: ClientRevision,
    pub anchor: HistoryAnchor,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessagePage {
    pub items: Vec<Message>,
    pub before: Option<HistoryCursor>,
    pub has_older_local: bool,
    pub may_have_older_remote: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub items: Vec<Event>,
    pub before: Option<HistoryCursor>,
    pub has_older_local: bool,
    pub may_have_older_remote: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPoint {
    pub at_ms: i64,
    pub percent: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentUsage {
    pub agent_id: String,
    pub usage: Option<Usage>,
    pub context: Option<ContextUsage>,
    pub samples: Vec<ContextPoint>,
    pub compactions: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuotaRow {
    pub quota: Quota,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeQuotaGroup {
    pub runtime: RuntimeKind,
    pub quotas: Vec<QuotaRow>,
    pub paused_agent_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsagePresentation {
    pub total: Option<Usage>,
    pub reporters: u32,
    pub eligible: u32,
    pub overflowed: bool,
    pub agents: Vec<AgentUsage>,
    pub groups: Vec<RuntimeQuotaGroup>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    TaskCard,
    Message,
    Event,
    Transcript,
    Diff,
    Output,
    Inbox,
    Models,
    History,
    Events,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ReadOptions {
    Default,
    Bounded {
        limit: u32,
    },
    Output {
        lines: u32,
        screen: bool,
        ansi: bool,
    },
    Diff {
        against: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceKey {
    pub entity: EntityKey,
    pub kind: ResourceKind,
    pub options: ReadOptions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceStatus {
    Idle,
    Loading,
    Ready,
    Empty,
    Unsupported,
    Failed,
    Stale,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ResourceValue {
    TaskCard(TaskCard),
    Message(Option<Message>),
    Event(Option<Event>),
    Transcript(TranscriptResult),
    Diff(GitOutcome),
    Output(OutputResult),
    Inbox(InboxResult),
    Models(ModelsResult),
    History(Vec<Message>),
    Events(Vec<Event>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceState {
    pub key: ResourceKey,
    pub status: ResourceStatus,
    pub request_sequence: u64,
    pub error: Option<ClientReason>,
    pub value: Option<ResourceValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRequest {
    pub key: ResourceKey,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ViewQuery {
    /// Read the planner's immutable preflight for this captured intent. The
    /// query allocates no operation, confirmation or timer identifiers.
    Availability {
        intent: Box<ActionIntent>,
    },
    Overview,
    Team,
    Tasks,
    AgentDetail {
        id: String,
    },
    TaskDetail {
        id: String,
    },
    Messages {
        before: Option<HistoryCursor>,
        limit: u32,
        filter: MessageFilter,
    },
    Events {
        before: Option<HistoryCursor>,
        limit: u32,
        filter: EventFilter,
    },
    Usage,
    Questions,
    Evidence {
        key: ResourceKey,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ViewPayload {
    /// The availability reason belongs to the query's captured intent scope.
    Availability(ActionAvailability),
    Overview(Box<OverviewData>),
    Team(Box<TeamData>),
    Tasks(Box<TasksData>),
    AgentDetail(Box<AgentDetailData>),
    TaskDetail(Box<TaskDetailData>),
    Messages(Box<MessagePage>),
    Events(Box<EventPage>),
    Usage(Box<UsagePresentation>),
    Questions(Vec<Message>),
    Evidence(Box<ResourceState>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewData {
    pub scope: Option<ClientScope>,
    pub revision: ClientRevision,
    pub synchronized_at_ms: Option<i64>,
    pub freshness: Freshness,
    pub payload: ViewPayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAction {
    Pause,
    Resume,
    Compact,
    Restart,
    RestartFresh,
    Kill,
    Focus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    SendMessage,
    Answer,
    Pause,
    Resume,
    Compact,
    Restart,
    RestartFresh,
    Kill,
    Focus,
    SetModel,
    AgentInput,
    ResumeLimited,
    StopTeam,
    Read,
    Attach,
    AttachInput,
    AttachResize,
    Detach,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ActionIntent {
    SendMessage {
        scope: ClientScope,
        to: String,
        body: String,
        message_id: String,
    },
    Answer {
        scope: ClientScope,
        question_id: String,
        body: String,
    },
    Agent {
        scope: ClientScope,
        id: String,
        action: AgentAction,
    },
    SetModel {
        scope: ClientScope,
        id: String,
        runtime: Option<RuntimeKind>,
        model: String,
        note: Option<String>,
    },
    AgentInput {
        scope: ClientScope,
        id: String,
        text: Option<String>,
        submit: bool,
    },
    ResumeLimited {
        scope: ClientScope,
        restart: bool,
    },
    StopTeam {
        scope: ClientScope,
    },
    Read {
        scope: ClientScope,
        resource: ResourceRequest,
    },
    Attach {
        scope: ClientScope,
        agent: String,
        rows: u16,
        cols: u16,
    },
    AttachInput {
        scope: ClientScope,
        attachment_id: AttachmentId,
        data: Vec<u8>,
    },
    AttachResize {
        scope: ClientScope,
        attachment_id: AttachmentId,
        rows: u16,
        cols: u16,
    },
    Detach {
        scope: ClientScope,
        attachment_id: AttachmentId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationKind {
    KillAgent,
    RestartFresh,
    RestartLimited,
    StopTeam,
}

// One catalogue defines both the closed command schema and its discriminant.
// Consumers use an exhaustive CommandKind match for their test constructors.
macro_rules! command_catalog {
    ($($variant:ident $({$($field:ident: $field_type:ty),* $(,)?})?
        $(($tuple_name:ident: $tuple_type:ty))? => $method:literal),* $(,)?) => {
        /// Internal application commands. This serde shape is not a wire
        /// request envelope; the command encoder supplies protocol 6 bytes.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "kind", content = "params", rename_all = "snake_case")]
        pub enum Command {
            $($variant $({$($field: $field_type),*})? $(($tuple_type))?),*
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum CommandKind { $($variant),* }

        impl CommandKind {
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            pub const fn method(self) -> &'static str {
                match self { $(Self::$variant => $method),* }
            }
        }

        impl Command {
            pub fn kind(&self) -> CommandKind {
                match self {
                    $(Self::$variant $({$($field: _),*})?
                        $(($tuple_name))? => CommandKind::$variant),*
                }
            }
        }
    };
}

command_catalog! {
    Ping => "ping",
    Snapshot { messages: Option<u32>, events: Option<u32> } => "snapshot",
    Subscribe => "subscribe",
    Workspaces => "workspaces",
    AgentsList => "agents_list",
    TasksList => "tasks_list",
    TaskGet { id: Option<String> } => "task_get",
    History { agent: Option<String>, limit: Option<u32> } => "history",
    MessageGet { id: String } => "message_get",
    Events { agent: Option<String>, limit: Option<u32> } => "events",
    // Native inbox reads never acknowledge or filter only unread messages.
    Inbox { agent: Option<String>, limit: Option<u32> } => "inbox",
    AgentOutput { id: String, lines: Option<u32>, screen: bool, ansi: bool } => "agent_output",
    AgentTranscript { id: String, limit: Option<u32> } => "agent_transcript",
    GitDiff { against: Option<String>, agent: Option<String> } => "git_diff",
    ModelsList => "models_list",
    Send(_params: SendParams) => "send",
    Answer { id: String, body: String } => "answer",
    AgentPause { id: String } => "agent_pause",
    AgentResume { id: String } => "agent_resume",
    AgentCompact { id: Option<String>, keep: Option<String> } => "agent_compact",
    AgentRestart { id: String } => "agent_restart",
    AgentRestartFresh { id: String } => "agent_restart_fresh",
    AgentKill { id: String } => "agent_kill",
    AgentSetModel { id: String, model: String, runtime: Option<RuntimeKind>, note: Option<String> } => "agent_set_model",
    AgentFocus { id: String } => "agent_focus",
    AgentInput { id: String, text: Option<String>, submit: bool } => "agent_input",
    Resume { agents: Vec<String>, restart: bool } => "resume",
    Shutdown { stop_agents: bool } => "shutdown",
    Attach { agent: String, rows: u16, cols: u16 } => "attach",
    AttachInput { data: String } => "attach_input",
    AttachResize { rows: u16, cols: u16 } => "attach_resize",
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ActionPlan {
    Unavailable {
        reason: ClientReason,
    },
    Confirm {
        confirmation_id: ConfirmationId,
        kind: ConfirmationKind,
        scope: ClientScope,
        target_ids: Vec<String>,
    },
    Dispatch {
        operation_id: OperationId,
    },
}

/// Typed successful results. A decoder must validate the method-specific wire
/// payload before constructing one of these records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CommandResult {
    Pong,
    Subscribed,
    Stopping,
    Ok,
    Attached { agent: String },
    Snapshot(Box<Snapshot>),
    Workspaces(Workspaces),
    Agents(Vec<Agent>),
    Tasks(Vec<Task>),
    TaskCard(Box<TaskCard>),
    Messages(Vec<Message>),
    Message(Box<Message>),
    Events(Vec<Event>),
    Inbox(InboxResult),
    Output(OutputResult),
    Transcript(TranscriptResult),
    Diff(Box<GitOutcome>),
    Models(ModelsResult),
    Sent(Box<SendResult>),
    Agent(Option<Box<Agent>>),
    Resumed { agents: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum OperationOutcome {
    Succeeded { result: CommandResult },
    Rejected { code: String, message: String },
    NotSent { reason: ClientReason },
    Canceled,
    OutcomeUnknown { reason: ClientReason },
    ClosedLocally { attachment_id: AttachmentId },
    SentUnacknowledged { attachment_id: AttachmentId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestLane {
    State,
    Read,
    Action,
    Terminal { attachment_id: AttachmentId },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ClientEffect {
    Request {
        scope: ClientScope,
        operation_id: OperationId,
        lane: RequestLane,
        command: Command,
    },
    Schedule {
        timer_id: TimerId,
        deadline_ms: u64,
    },
    CancelTimer {
        timer_id: TimerId,
    },
    Resynchronize {
        scope: ClientScope,
    },
    Notice {
        code: String,
        entity: Option<EntityKey>,
    },
    CloseAttachment {
        scope: ClientScope,
        operation_id: OperationId,
        attachment_id: AttachmentId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientUpdate {
    pub revision: ClientRevision,
    pub changes: ChangeSet,
    pub effects: Vec<ClientEffect>,
    /// Final notices emitted only by the core. Held identified-message retries
    /// have no notice until their sole replay completes or their wait expires.
    pub outcomes: Vec<OperationNotice>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanUpdate {
    pub plan: ActionPlan,
    pub update: ClientUpdate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ClientError {
    InvalidInput {
        field: String,
        reason: String,
    },
    DecodeFailure {
        message: String,
    },
    ProtocolMismatch {
        expected: u32,
        actual: Option<u32>,
        version: Option<String>,
    },
    TooLarge {
        actual: u64,
        limit: u64,
    },
    CursorExpired,
    StaleConfirmation,
    ClosedClient,
    InternalFailure {
        code: String,
    },
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field, reason } => write!(formatter, "{field}: {reason}"),
            Self::DecodeFailure { message } => write!(formatter, "invalid server data: {message}"),
            Self::ProtocolMismatch {
                expected, actual, ..
            } => {
                write!(
                    formatter,
                    "expected protocol {expected}, received {actual:?}"
                )
            }
            Self::TooLarge { actual, limit } => {
                write!(formatter, "input is {actual} bytes; limit is {limit}")
            }
            Self::CursorExpired => formatter.write_str("history cursor expired"),
            Self::StaleConfirmation => formatter.write_str("confirmation is no longer current"),
            Self::ClosedClient => formatter.write_str("client is closed"),
            Self::InternalFailure { code } => write!(formatter, "client failure: {code}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Interface implemented by the reducer's ClientCore, not by this schema module.
pub trait ClientApi {
    fn apply(
        &mut self,
        input: ClientInput,
        clock: ClockSample,
    ) -> Result<ClientUpdate, ClientError>;
    fn view(&self, query: ViewQuery, clock: ClockSample) -> Result<ViewData, ClientError>;
    fn plan(&mut self, intent: ActionIntent, clock: ClockSample)
        -> Result<PlanUpdate, ClientError>;
    fn confirm(
        &mut self,
        id: ConfirmationId,
        clock: ClockSample,
    ) -> Result<ClientUpdate, ClientError>;
    fn cancel_confirmation(&mut self, id: ConfirmationId) -> Result<ClientUpdate, ClientError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcomeKind {
    Success,
    Rejected,
    NotSent,
    Canceled,
    Unknown,
    ClosedLocally,
    SentUnacknowledged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationNotice {
    pub scope: ClientScope,
    pub operation_id: OperationId,
    pub attachment_id: Option<AttachmentId>,
    pub kind: OperationOutcomeKind,
    pub reason: Option<ClientReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientNotice {
    pub scope: Option<ClientScope>,
    pub revision: ClientRevision,
    pub changes: ChangeSet,
    pub endpoint: EndpointStatus,
    pub workspace: WorkspaceStatus,
    pub reason: Option<ClientReason>,
    pub outcomes: Vec<OperationNotice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum TerminalEnd {
    Normal,
    Detach,
    Disconnect,
    Revocation,
    Overflow,
    Unsupported,
    Failed { reason: ClientReason },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalChunk {
    pub scope: ClientScope,
    pub attachment_id: AttachmentId,
    pub data: Vec<u8>,
    pub ended: Option<TerminalEnd>,
}
