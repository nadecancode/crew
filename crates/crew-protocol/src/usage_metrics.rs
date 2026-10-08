//! Native usage evidence and server-computed metrics. Missing counters stay null.
//! No prompt, reasoning, tool-result, credential or native filesystem path belongs here.

use serde::{Deserialize, Serialize};

use crate::features::{Fraction, Id, Label, List, PageRequest, Timestamp, Wide};
use crate::RuntimeKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageReason {
    ZeroInput,
    NoCoveredInput,
    NotReported,
    InvalidCounter,
    UnknownTime,
    UnknownModel,
    UnknownProvider,
    UnknownAccount,
    UnknownAttribution,
    MultipleTasks,
    AmbiguousBoundary,
    NativeBatch,
    MissingRequestIdentity,
    MissingTurnIdentity,
    SourceGap,
    SourceBacklog,
    UnsupportedRuntime,
    UnsupportedSidechain,
    ParserLimit,
    RetentionLimit,
    IdentityLost,
    CorrectionOutsideHorizon,
    CompletionHistoryIncomplete,
    NoCompletions,
    NoEligibleCost,
    NoPrice,
    NotTracked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageExclusion {
    pub reason: CoverageReason,
    pub observations: Wide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceGap {
    pub source_id: Id,
    pub reason: CoverageReason,
    pub from: Option<Timestamp>,
    pub to: Option<Timestamp>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricCoverage {
    pub eligible_observations: Wide,
    pub observations_seen: Wide,
    pub exclusions: List<CoverageExclusion, 32>,
    pub known_requests: Wide,
    pub request_cardinality_complete: bool,
    pub reporting_agents: Wide,
    pub expected_agents: Option<Wide>,
    pub source_gaps: List<SourceGap, 64>,
    pub source_gaps_total: Option<Wide>,
    pub source_backlog_files: Option<Wide>,
    pub retained_from: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricState {
    Complete,
    Partial,
    #[default]
    Unavailable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterMetric {
    pub known_sum: Option<Wide>,
    pub state: MetricState,
    pub reason: Option<CoverageReason>,
    pub coverage: MetricCoverage,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RatioMetric {
    pub numerator: Option<Wide>,
    pub denominator: Option<Wide>,
    /// Display only. Exact operands and eligibility remain authoritative.
    pub value: Option<Fraction>,
    /// Priced write-share uncertainty can have different exact endpoint operands.
    pub lower: Option<RatioBound>,
    pub upper: Option<RatioBound>,
    pub state: MetricState,
    pub reason: Option<CoverageReason>,
    pub coverage: MetricCoverage,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RatioBound {
    pub numerator: Wide,
    pub denominator: Wide,
    pub value: Fraction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostBasis {
    StandardApiEquivalent,
    RuntimeReported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstimateState {
    Finite,
    Partial,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub price_book_id: Option<Id>,
    pub prices_checked_at: Option<Timestamp>,
    pub basis: CostBasis,
    pub lower_pico_usd: Option<Wide>,
    pub upper_pico_usd: Option<Wide>,
    pub state: EstimateState,
    pub reason: Option<CoverageReason>,
    pub assumptions: List<Label, 32>,
    pub excluded_models: List<Label, 64>,
    pub coverage: MetricCoverage,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCounters {
    pub input_total: Option<Wide>,
    pub input_uncached: Option<Wide>,
    pub cache_read: Option<Wide>,
    pub cache_write: Option<Wide>,
    pub cache_write_5m: Option<Wide>,
    pub cache_write_1h: Option<Wide>,
    pub output: Option<Wide>,
    pub total_tokens: Option<Wide>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub input_total: CounterMetric,
    pub input_uncached: CounterMetric,
    pub cache_read: CounterMetric,
    pub cache_write: CounterMetric,
    pub cache_write_5m: CounterMetric,
    pub cache_write_1h: CounterMetric,
    pub output: CounterMetric,
    pub total_tokens: CounterMetric,
    pub hit_rate: RatioMetric,
    pub write_share: RatioMetric,
    pub estimate: CostEstimate,
    pub requests_observed: CounterMetric,
    pub turns_observed: CounterMetric,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    Live,
    Aside,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum UsageTaskFilter {
    All,
    Id(String),
    NoTask,
    NotAttributed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum UsageEpicFilter {
    All,
    Id(String),
    NoEpic,
    NoTask,
    NotAttributed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageScope {
    pub workspace_run_id: Id,
    pub kind: UsageKind,
    pub agent_incarnation: Option<Id>,
    pub task: UsageTaskFilter,
    pub epic: UsageEpicFilter,
    pub model: Option<Label>,
    pub account_id: Option<Id>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageDimension {
    Agent,
    Task,
    Epic,
    Model,
    Account,
    Runtime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageQuery {
    pub scope: UsageScope,
    pub from_ms: Option<Timestamp>,
    pub to_ms: Option<Timestamp>,
    pub group_by: Option<UsageDimension>,
    pub bucket_seconds: Option<u32>,
    pub price_book_id: Option<Id>,
    pub page: PageRequest,
}

impl UsageQuery {
    pub fn validate(&self, max_rows: u16) -> Result<(), &'static str> {
        self.page.validate(max_rows)?;
        if self.from_ms.zip(self.to_ms).is_some_and(|(a, b)| a >= b) {
            return Err("usage range must be nonempty and increasing");
        }
        if self.bucket_seconds.is_some_and(|n| n == 0 || n > 86400) {
            return Err("usage bucket must be 1..86400 seconds");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EpicAttribution {
    Epic { id: String },
    NoEpic,
    Unknown { reason: CoverageReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Work,
    Orchestration,
    Maintenance,
    Keepalive,
    Aside,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkAttribution {
    Task { id: String, epic: EpicAttribution },
    NoTask { activity: ActivityKind },
    Unknown { reason: CoverageReason },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountAttribution {
    Known {
        id: Id,
        assignment_generation: Wide,
        interval_id: Id,
    },
    Unknown {
        reason: CoverageReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageBinding {
    pub workspace_run_id: Id,
    pub source_id: Id,
    pub kind: UsageKind,
    pub agent_id: String,
    pub incarnation: Id,
    pub launch_id: Id,
    pub native_session_id: Option<Id>,
    pub native_turn_id: Option<Id>,
    pub aside_execution_id: Option<Id>,
    pub runtime: RuntimeKind,
    pub cli_version: Option<Label>,
    pub model: Option<Label>,
    pub provider: Option<Label>,
    pub work: WorkAttribution,
    pub account: AccountAttribution,
    pub board_revision: Option<Wide>,
    pub event_seq: Option<Wide>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationUnit {
    Request,
    CounterDelta,
    /// Diagnostic evidence only; never eligible or priced without a valid revision.
    Unusable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBasis {
    Native,
    CorrelatedTurn,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageObservation {
    pub id: Id,
    pub revision: Wide,
    pub unit: ObservationUnit,
    pub binding: UsageBinding,
    pub time_basis: TimeBasis,
    pub native_time: Option<Timestamp>,
    pub observed_at: Timestamp,
    pub counters: UsageCounters,
    pub validity: List<CoverageReason, 32>,
    pub diagnostic_codes: List<Label, 32>,
    pub event_refs: List<Wide, 32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionCohort {
    pub first_completions: Wide,
    pub membership_complete: bool,
    pub tasks_with_observed_cost: Wide,
    pub membership_coverage: MetricCoverage,
    pub cost_per_completed_task: CostEstimate,
    pub includes_earlier_spend: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingState {
    Tracking,
    CatchingUp,
    Partial,
    Paused,
    Closed,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub revision: Wide,
    /// Fully resolved defaults, including the selected price book.
    pub query: UsageQuery,
    pub as_of: Timestamp,
    pub tracking_since: Option<Timestamp>,
    pub tracking_state: TrackingState,
    pub totals: UsageTotals,
    pub attribution_coverage: MetricCoverage,
    pub completion_cohort: Option<CompletionCohort>,
    /// Aside scope only: distinct executions with proven newly observed usage,
    /// including explicit zero and failed/cancelled spend, excluding copied history.
    /// Unknown execution membership is unavailable, never a guessed zero. Incomplete
    /// spend coverage is separate from a known execution count.
    pub executions_observed: Option<CounterMetric>,
    pub execution_statuses: Option<ExecutionStatusCounts>,
    pub limitations: List<CoverageReason, 32>,
}

/// Status partition of the executions_observed cohort, not all currently retained asides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionStatusCounts {
    pub answered: Wide,
    pub failed: Wide,
    pub cancelled: Wide,
    pub still_answering: Wide,
    pub unknown: Wide,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageBucket {
    pub from_ms: Timestamp,
    pub to_ms: Timestamp,
    pub totals: UsageTotals,
    pub category_split_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UsageGroupKey {
    Agent {
        id: String,
        incarnation: Id,
    },
    Task {
        id: String,
    },
    Epic {
        id: String,
    },
    Model {
        model: Label,
        provider: Option<Label>,
    },
    Account {
        id: Id,
    },
    Runtime {
        runtime: RuntimeKind,
    },
    NoEpic,
    NoTask,
    NotAttributed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageBreakdownRow {
    pub key: UsageGroupKey,
    pub totals: UsageTotals,
    pub activities: List<ActivityKind, 8>,
    pub reasons: List<CoverageExclusion, 32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsagePage<T, const MAX: usize> {
    pub revision: Wide,
    pub query: UsageQuery,
    pub rows: List<T, MAX>,
    pub next_cursor: Option<Id>,
}

pub type UsageSeries = UsagePage<UsageBucket, 720>;
pub type UsageBreakdown = UsagePage<UsageBreakdownRow, 200>;
pub type UsageObservations = UsagePage<UsageObservation, 200>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRun {
    pub workspace_run_id: Id,
    pub started_at: Option<Timestamp>,
    pub tracking_since: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub state: TrackingState,
    pub coverage: MetricCoverage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRuns {
    pub rows: List<UsageRun, 200>,
    pub next_cursor: Option<Id>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceRow {
    pub provider: Label,
    pub model: Label,
    /// Exclusive input threshold; None is the ordinary context tier.
    pub above_input_tokens: Option<Wide>,
    /// Tokens * this wire rate / 1,000,000 gives pico-USD; no client repricing.
    pub input_pico_usd_per_million: Wide,
    pub output_pico_usd_per_million: Wide,
    pub cache_read_pico_usd_per_million: Option<Wide>,
    pub cache_write_pico_usd_per_million: Option<Wide>,
    pub cache_write_5m_pico_usd_per_million: Option<Wide>,
    pub cache_write_1h_pico_usd_per_million: Option<Wide>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBook {
    pub id: Id,
    pub checked_at: Timestamp,
    pub basis: CostBasis,
    pub rows: List<PriceRow, 200>,
    pub sources: List<crate::features::Text<2048>, 16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagePrices {
    pub price_book_id: Option<Id>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageWatchResult {
    pub epoch: Id,
    pub watch_id: Id,
    pub initial: UsageSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageChanged {
    pub workspace_run_id: Id,
    pub epoch: Id,
    pub watch_id: Id,
    pub from_revision: Wide,
    pub to_revision: Wide,
    pub resync_required: bool,
}
