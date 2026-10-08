//! Foreign declarations for portable records with unchanged field types.
//!
//! UniFFI remote declarations implement conversion on the actual Rust types.
//! Struct construction and exhaustive enum matches catch upstream field drift.
//! Opaque JSON and boxed view payloads are adapted separately in `views`.

use super::views::BoxedIntent;
pub use crew_client::api::{
    ActionAvailability, ActionIntent, ActionKind, ActionPlan, AgentAction, AgentPresentation,
    AgentRelation, AgentUsage, ChangeSet, ClientConfig, ClientError, ClientLimits, ClientNotice,
    ClientReason, ClientScope, ClockSample, ConfirmationKind, ConnectionContext, ConnectionMode,
    ContextPoint, DependencyState, EndpointStatus, Entity, EntityKey, EventFilter, Freshness,
    Grant, GrantTier, HistoryAnchor, HistoryCursor, MessageEndpoints, MessageFilter,
    OperationNotice, OperationOutcomeKind, OverviewData, PeerInfo, QuotaRow, ReadOptions,
    ResourceKey, ResourceKind, ResourceRequest, ResourceStatus, RuntimeQuotaGroup, SnapshotMode,
    TaskLayer, TaskPresentation, TasksData, TeamData, TerminalChunk, TerminalEnd, Tone,
    UsagePresentation, ViewQuery, WarningFact, WorkspaceStatus,
};
use crew_client::api::{AttachmentId, ClientRevision, ConfirmationId, OperationId};
pub use crew_client::CurrentWork;
pub use crew_protocol::rpc::{
    ConflictReport, DaemonRef, ModelChoice, ModelsResult, OtherSession, OutputResult, Quota,
    SessionInfo, TaskCard, TranscriptResult, WorkspaceEntry, WorkspaceSummary, Workspaces,
};
pub use crew_protocol::{
    Agent, AgentStatus, AgentToolbox, Artifact, ContextUsage, EventLevel, MessageType,
    ModelFallback, RuntimeKind, Task, TaskStatus, ToolboxItem, ToolboxItemCode, ToolboxItemKind,
    ToolboxItemStatus, ToolboxSource, ToolboxSourceCode, ToolboxSourceOrigin, ToolboxSourceStatus,
    TranscriptEntry, TranscriptKind, Usage, UsageLimit, WorkspaceState,
};

#[uniffi::remote(Enum)]
pub enum AgentStatus {
    Spawning,
    Idle,
    Working,
    Waiting,
    Blocked,
    Done,
    Failed,
    Stopped,
}

#[uniffi::remote(Enum)]
pub enum TaskStatus {
    Queued,
    Ready,
    Working,
    Waiting,
    Blocked,
    Review,
    Done,
    Failed,
}

#[uniffi::remote(Enum)]
pub enum RuntimeKind {
    Claude,
    Codex,
    Pi,
    Shell,
}

#[uniffi::remote(Enum)]
pub enum MessageType {
    Task,
    Message,
    Question,
    Answer,
    Status,
    ReviewRequest,
    ReviewResult,
    Artifact,
    Error,
}

#[uniffi::remote(Enum)]
pub enum EventLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[uniffi::remote(Enum)]
pub enum TranscriptKind {
    Prompt,
    Reply,
    Tool,
    Note,
}

#[uniffi::remote(Enum)]
pub enum WorkspaceState {
    Running,
    Stopped,
    Incompatible,
    Unreachable,
}

#[uniffi::remote(Record)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub turns: u64,
}

#[uniffi::remote(Record)]
pub struct ContextUsage {
    /// Tokens the last model request was answered over.
    pub tokens: u64,
    /// The model's context window, in tokens.
    pub window: u64,
    /// `tokens` over `window`, 0 to 100.
    pub percent: u8,
    pub updated_at: i64,
}

#[uniffi::remote(Record)]
pub struct ModelFallback {
    /// The model the member was given, by its proper name (`Fable 5.1`).
    pub from: String,
    /// What the runtime said about the switch, when it said; else empty.
    #[serde(default)]
    pub note: String,
    pub since: i64,
    /// crew moved it, as the work style's `model_fallbacks` say, and moves
    /// it back once the window resets: nobody needs to decide anything.
    #[serde(default)]
    pub policy: bool,
}

#[uniffi::remote(Record)]
pub struct UsageLimit {
    /// `usage` (a plan or rate limit: wait for the reset, or switch
    /// account), `credits` (out of credits or over a spend cap) or `auth`
    /// (the login no longer works).
    pub kind: String,
    /// What the runtime said.
    pub message: String,
    pub since: i64,
    /// When the runtime says the limit resets, if it said.
    #[serde(default)]
    pub resets_at: Option<i64>,
    /// When crew resumes the agent by itself; `None` waits for `crew resume`.
    #[serde(default)]
    pub retry_at: Option<i64>,
    /// Resume attempts that ran into the limit again.
    #[serde(default)]
    pub attempts: u32,
}

#[uniffi::remote(Record)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub role: String,
    /// Title of the task the agent is currently assigned, if any.
    #[serde(default)]
    pub task: Option<String>,
    pub status: AgentStatus,
    /// Free-text description of what the agent is doing right now.
    #[serde(default)]
    pub activity: String,
    pub runtime: RuntimeKind,
    #[serde(default)]
    pub model: Option<String>,
    pub workdir: String,
    #[serde(default)]
    pub branch: Option<String>,
    /// Process backend hosting the agent: `pty` or `herdr`.
    pub backend: String,
    /// Backend-specific handle (the herdr pane id).
    #[serde(default)]
    pub backend_ref: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    /// The runtime's own session id (used to resume after a crash).
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub orchestrator: bool,
    /// The agent that launched this one, for dynamically spawned subagents.
    #[serde(default)]
    pub parent: Option<String>,
    /// Added by the orchestrator once the founding team was settled: it is
    /// not saved with the roster, and leaves the team when its work is done.
    #[serde(default)]
    pub temporary: bool,
    /// In-session subagents (e.g. Claude Code's Task tool) currently running.
    #[serde(default)]
    pub subagents: u32,
    /// How bus messages reach the session: `socket` (pushed over the daemon's
    /// Unix socket into the runtime) or `terminal` (typed into its terminal).
    #[serde(default)]
    pub delivery: String,
    #[serde(default)]
    pub started_at: Option<i64>,
    pub last_activity_at: i64,
    #[serde(default)]
    pub progress: Option<u8>,
    #[serde(default)]
    pub paused: bool,
    /// Why the agent is blocked: `permission`, `dialog`, `self`,
    /// `unresponsive`, `usage_limit`, `refused` (its model declined the
    /// turn's content), or `provider_error` (the runtime could not finish the turn).
    #[serde(default)]
    pub block_kind: Option<String>,
    /// Set while the agent waits for a usage limit to lift.
    #[serde(default)]
    pub limit: Option<UsageLimit>,
    /// How full the session's context window is, when the runtime says.
    #[serde(default)]
    pub context: Option<ContextUsage>,
    /// A compaction of the session was asked for and has not happened yet.
    #[serde(default)]
    pub compact_pending: bool,
    /// How often the session's context has been compacted.
    #[serde(default)]
    pub compactions: u32,
    /// The model the session says it runs on (the runtime's own id for it).
    #[serde(default)]
    pub running_model: Option<String>,
    /// The model's proper name, for display: `Opus 5.5`, `GPT 6 Astra`.
    #[serde(default)]
    pub model_name: Option<String>,
    /// The reasoning effort the session runs at (`low` … `max`), when the
    /// runtime says.
    #[serde(default)]
    pub effort: Option<String>,
    /// Set while the session runs on a model other than the one the member
    /// was given.
    #[serde(default)]
    pub fallback: Option<ModelFallback>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub restarts: u32,
    #[serde(default)]
    pub inbox_unread: u32,
    #[serde(default)]
    pub inbox_total: u32,
    #[serde(default)]
    pub outbox_total: u32,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub error: Option<String>,
    /// The member's effective toolbox, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolbox: Option<AgentToolbox>,
}

/// A member's effective toolbox as every dashboard shows it. Secret values
/// are never in it.
#[uniffi::remote(Record)]
pub struct AgentToolbox {
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_bytes: Option<u64>,
    #[serde(default)]
    pub items: Vec<ToolboxItem>,
    #[serde(default)]
    pub sources: Vec<ToolboxSource>,
}

#[uniffi::remote(Record)]
pub struct ToolboxItem {
    pub kind: ToolboxItemKind,
    pub name: String,
    pub origin: String,
    #[serde(default)]
    pub required: bool,
    pub status: ToolboxItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[uniffi::remote(Enum)]
pub enum ToolboxItemKind {
    Mcp,
    Skills,
    Plugins,
    Subagents,
    Hooks,
    Unknown,
}

#[uniffi::remote(Record)]
pub struct ToolboxItemStatus {
    pub code: ToolboxItemCode,
    pub text: String,
}

#[uniffi::remote(Enum)]
pub enum ToolboxItemCode {
    Planned,
    Active,
    NotAvailable,
    NotApplicable,
    Needs,
    Blocked,
    Shadowed,
    FailedPreflight,
    FailedInSession,
    Unknown,
}

#[uniffi::remote(Record)]
pub struct ToolboxSource {
    pub origin: ToolboxSourceOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub label: String,
    pub status: ToolboxSourceStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[uniffi::remote(Enum)]
pub enum ToolboxSourceOrigin {
    Operator,
    Project,
    System,
    Managed,
    Unknown,
}

#[uniffi::remote(Record)]
pub struct ToolboxSourceStatus {
    pub code: ToolboxSourceCode,
    pub text: String,
}

#[uniffi::remote(Enum)]
pub enum ToolboxSourceCode {
    NotUsed,
    Used,
    Disabled,
    NotAvailableSeparately,
    CannotBeExcluded,
    Unknown,
}

#[uniffi::remote(Record)]
pub struct TranscriptEntry {
    pub kind: TranscriptKind,
    #[serde(default)]
    pub at: Option<i64>,
    pub text: String,
}

#[uniffi::remote(Record)]
pub struct Artifact {
    /// `commit`, `file`, `branch`, `note`, ...
    pub kind: String,
    /// The commit sha, path or text.
    pub value: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub created_at: i64,
}

#[uniffi::remote(Record)]
pub struct Task {
    pub epic: Option<String>,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub assigned_agent: Option<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub created_at: i64,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// Agent asked to review the result before the task counts as done.
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub progress: Option<u8>,
    /// Latest progress note or the reason the task is blocked/failed.
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[uniffi::remote(Record)]
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

#[uniffi::remote(Record)]
pub struct SessionInfo {
    pub workspace_run_id: Option<String>,
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

#[uniffi::remote(Record)]
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

#[uniffi::remote(Record)]
pub struct DaemonRef {
    pub pid: u32,
    pub started_at: i64,
}

#[uniffi::remote(Record)]
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

#[uniffi::remote(Record)]
pub struct OtherSession {
    pub id: String,
    #[serde(default)]
    pub project: Option<String>,
    pub workspace: String,
}

#[uniffi::remote(Record)]
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

#[uniffi::remote(Record)]
pub struct TaskCard {
    pub task: Task,
    #[serde(default)]
    pub dependencies: Vec<Task>,
}

#[uniffi::remote(Record)]
pub struct ConflictReport {
    /// `cherry-pick` or `merge`.
    pub operation: String,
    pub files: Vec<String>,
    pub detail: String,
}

#[uniffi::remote(Record)]
pub struct OutputResult {
    pub text: String,
    #[serde(default)]
    pub alive: bool,
}

#[uniffi::remote(Record)]
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

#[uniffi::remote(Record)]
pub struct ModelsResult {
    /// Claude Code's, then Codex's, then pi's, each in the catalog's order.
    pub models: Vec<ModelChoice>,
}

#[uniffi::remote(Record)]
pub struct TranscriptResult {
    /// Oldest first.
    pub entries: Vec<crew_protocol::types::TranscriptEntry>,
    /// The runtime keeps a session file and it was found.
    #[serde(default)]
    pub available: bool,
    /// The session is longer than what is returned.
    #[serde(default)]
    pub truncated: bool,
}

#[uniffi::remote(Record)]
pub struct CurrentWork {
    pub task_id: String,
    pub done: bool,
    pub elapsed_ms: Option<i64>,
}

#[uniffi::remote(Record)]
pub struct ClockSample {
    pub wall_ms: i64,
    pub monotonic_ms: u64,
}

#[uniffi::remote(Record)]
pub struct ClientScope {
    pub endpoint_id: String,
    pub workspace_id: String,
    pub generation: Option<u64>,
    pub daemon_pid: Option<u32>,
    pub daemon_started_at_ms: Option<i64>,
    pub attempt: u64,
}

#[uniffi::remote(Enum)]
pub enum EndpointStatus {
    Disconnected,
    Connecting,
    Authenticated,
    Reconnecting,
    SignedOut,
    Incompatible,
    Stopped,
}

#[uniffi::remote(Enum)]
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

#[uniffi::remote(Enum)]
pub enum ConnectionMode {
    LocalOperator,
    ScopedGateway,
}

#[uniffi::remote(Enum)]
pub enum SnapshotMode {
    OrderedConnection,
    BufferedOverlap,
}

#[uniffi::remote(Record)]
pub struct ClientLimits {
    pub messages: u32,
    pub events: u32,
    pub context_points: u32,
    pub buffered_pushes: u32,
    pub pending_operations: u32,
    pub page_size: u32,
}

#[uniffi::remote(Record)]
pub struct ClientConfig {
    pub limits: ClientLimits,
    pub snapshot_mode: SnapshotMode,
}

#[uniffi::remote(Enum)]
pub enum Entity {
    Workspace,
    Agent(String),
    Task(String),
    Message(String),
    Event(u64),
}

#[uniffi::remote(Record)]
pub struct EntityKey {
    pub scope: ClientScope,
    pub entity: Entity,
}

#[uniffi::remote(Record)]
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

#[uniffi::remote(Enum)]
pub enum GrantTier {
    Read,
    Control,
    Terminal,
}

#[uniffi::remote(Record)]
pub struct Grant {
    pub grant_id: String,
    pub tiers: Vec<GrantTier>,
    pub workspace_ids: Vec<String>,
    pub expires_at_ms: Option<i64>,
}

#[uniffi::remote(Record)]
pub struct ConnectionContext {
    pub mode: ConnectionMode,
    pub grant: Grant,
    pub snapshot_mode: SnapshotMode,
    pub scope: ClientScope,
}

#[uniffi::remote(Record)]
pub struct PeerInfo {
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
    pub project: Option<String>,
}

#[uniffi::remote(Record)]
pub struct ClientReason {
    pub details: Option<crew_client::features::FeatureRejection>,
    pub code: String,
    pub message: String,
}

#[uniffi::remote(Enum)]
pub enum Freshness {
    Loading,
    Fresh,
    Stale,
    Unavailable,
}

#[uniffi::remote(Enum)]
pub enum Tone {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

#[uniffi::remote(Record)]
pub struct WarningFact {
    pub code: String,
    pub text: String,
    pub entity: Option<EntityKey>,
}

#[uniffi::remote(Record)]
pub struct ActionAvailability {
    pub action: ActionKind,
    pub available: bool,
    pub reason: Option<ClientReason>,
}

#[uniffi::remote(Record)]
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

#[uniffi::remote(Record)]
pub struct DependencyState {
    pub id: String,
    pub status: Option<TaskStatus>,
}

#[uniffi::remote(Record)]
pub struct TaskPresentation {
    pub task: Task,
    pub dependencies: Vec<DependencyState>,
    pub layer: u32,
    pub in_cycle: bool,
}

#[uniffi::remote(Record)]
pub struct AgentRelation {
    pub parent_id: String,
    pub child_id: String,
}

#[uniffi::remote(Record)]
pub struct TaskLayer {
    pub index: u32,
    pub task_ids: Vec<String>,
}

#[uniffi::remote(Record)]
pub struct OverviewData {
    pub session: Option<SessionInfo>,
    pub agent_count: u32,
    pub working_count: u32,
    pub task_count: u32,
    pub done_count: u32,
    pub question_count: u32,
    pub warnings: Vec<WarningFact>,
}

#[uniffi::remote(Record)]
pub struct TeamData {
    pub agents: Vec<AgentPresentation>,
    pub relations: Vec<AgentRelation>,
}

#[uniffi::remote(Record)]
pub struct TasksData {
    pub tasks: Vec<TaskPresentation>,
    pub layers: Vec<TaskLayer>,
    pub epics: Option<crew_protocol::epics::EpicBoard>,
}

#[uniffi::remote(Enum)]
pub enum MessageEndpoints {
    All,
    Agent { id: String },
    Pair { first: String, second: String },
}

#[uniffi::remote(Record)]
pub struct MessageFilter {
    pub endpoints: MessageEndpoints,
    pub kind: Option<MessageType>,
    pub text: String,
}

#[uniffi::remote(Record)]
pub struct EventFilter {
    pub agent_id: Option<String>,
    pub level: Option<EventLevel>,
    pub text: String,
    pub vcs_only: bool,
}

#[uniffi::remote(Enum)]
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

#[uniffi::remote(Record)]
pub struct HistoryCursor {
    pub scope: ClientScope,
    pub revision: ClientRevision,
    pub anchor: HistoryAnchor,
}

#[uniffi::remote(Record)]
pub struct ContextPoint {
    pub at_ms: i64,
    pub percent: u8,
}

#[uniffi::remote(Record)]
pub struct AgentUsage {
    pub agent_id: String,
    pub usage: Option<Usage>,
    pub context: Option<ContextUsage>,
    pub samples: Vec<ContextPoint>,
    pub compactions: u32,
}

#[uniffi::remote(Record)]
pub struct QuotaRow {
    pub quota: Quota,
    pub stale: bool,
}

#[uniffi::remote(Record)]
pub struct RuntimeQuotaGroup {
    pub runtime: RuntimeKind,
    pub quotas: Vec<QuotaRow>,
    pub paused_agent_ids: Vec<String>,
}

#[uniffi::remote(Record)]
pub struct UsagePresentation {
    pub total: Option<Usage>,
    pub reporters: u32,
    pub eligible: u32,
    pub overflowed: bool,
    pub agents: Vec<AgentUsage>,
    pub groups: Vec<RuntimeQuotaGroup>,
}

#[uniffi::remote(Enum)]
pub enum ResourceKind {
    Feature(crew_client::features::FeatureResourceKind),
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

#[uniffi::remote(Enum)]
pub enum ReadOptions {
    Feature(super::features::BoxedFeatureRead),
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

#[uniffi::remote(Record)]
pub struct ResourceKey {
    pub entity: EntityKey,
    pub kind: ResourceKind,
    pub options: ReadOptions,
}

#[uniffi::remote(Enum)]
pub enum ResourceStatus {
    Idle,
    Loading,
    Ready,
    Empty,
    Unsupported,
    Failed,
    Stale,
}

#[uniffi::remote(Record)]
pub struct ResourceRequest {
    pub key: ResourceKey,
}

#[uniffi::remote(Enum)]
pub enum ViewQuery {
    Features,
    FeatureWatches {
        feature: crew_protocol::features::Feature,
    },
    Epic {
        key: ResourceKey,
    },
    Asides {
        key: ResourceKey,
    },
    Docs {
        key: ResourceKey,
    },
    UsageMetrics {
        key: ResourceKey,
    },
    /// Read the planner's immutable preflight for this captured intent. The
    /// query allocates no operation, confirmation or timer identifiers.
    Availability {
        intent: BoxedIntent,
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

#[uniffi::remote(Enum)]
pub enum AgentAction {
    Pause,
    Resume,
    Compact,
    Restart,
    RestartFresh,
    Kill,
    Focus,
}

#[uniffi::remote(Enum)]
pub enum ActionKind {
    Feature,
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

#[uniffi::remote(Enum)]
pub enum ActionIntent {
    Feature {
        scope: ClientScope,
        action: crew_client::features::FeatureAction,
    },
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

#[uniffi::remote(Enum)]
pub enum ConfirmationKind {
    KillAgent,
    RestartFresh,
    RestartLimited,
    StopTeam,
}

#[uniffi::remote(Enum)]
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

#[uniffi::remote(Enum)]
pub enum OperationOutcomeKind {
    Success,
    Rejected,
    NotSent,
    Canceled,
    Unknown,
    ClosedLocally,
    SentUnacknowledged,
}

#[uniffi::remote(Record)]
pub struct OperationNotice {
    pub scope: ClientScope,
    pub operation_id: OperationId,
    pub attachment_id: Option<AttachmentId>,
    pub kind: OperationOutcomeKind,
    pub reason: Option<ClientReason>,
}

#[uniffi::remote(Record)]
pub struct ClientNotice {
    pub scope: Option<ClientScope>,
    pub revision: ClientRevision,
    pub changes: ChangeSet,
    pub endpoint: EndpointStatus,
    pub workspace: WorkspaceStatus,
    pub reason: Option<ClientReason>,
    pub outcomes: Vec<OperationNotice>,
}

#[uniffi::remote(Enum)]
pub enum TerminalEnd {
    Normal,
    Detach,
    Disconnect,
    Revocation,
    Overflow,
    Unsupported,
    Failed { reason: ClientReason },
}

#[uniffi::remote(Record)]
pub struct TerminalChunk {
    pub scope: ClientScope,
    pub attachment_id: AttachmentId,
    pub data: Vec<u8>,
    pub ended: Option<TerminalEnd>,
}

#[uniffi::remote(Error)]
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
