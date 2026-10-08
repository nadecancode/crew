//! Operator-only isolated answers. No body is placed in ordinary pushes or receipts.

use serde::{Deserialize, Serialize};

use crate::features::{Id, Label, List, PageRequest, Text, Timestamp, Wide};
use crate::usage_metrics::{CostEstimate, UsageCounters};
use crate::RuntimeKind;

pub const MAX_QUESTION_BYTES: usize = 8 * 1024;
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;
pub const MAX_TARGETS: usize = 64;
pub const MAX_PENDING: usize = 128;
pub const MAX_RUNNING: usize = 4;
pub const MAX_RETAINED: usize = 50;
pub const MAX_MEMORY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RECEIPTS: usize = 65_536;
pub const MAX_RECEIPT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideTarget {
    pub agent_id: String,
    pub incarnation: Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsideUnavailableReason {
    ShellMember,
    UnsupportedRuntime,
    UnsupportedVersion,
    NoSession,
    UnreadableSession,
    UsageLimit,
    MemberChanged,
    MemberRemoved,
    IsolationUnavailable,
    PrefixUnavailable,
    CredentialsUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AsideAvailability {
    Available,
    Unavailable { reason: AsideUnavailableReason },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideMember {
    pub target: AsideTarget,
    pub runtime: RuntimeKind,
    pub model: Option<Label>,
    pub availability: AsideAvailability,
    pub resets_at: Option<Timestamp>,
    pub last_turn_at: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsideStatus {
    Waiting,
    Answering,
    Answered,
    Failed,
    Cancelled,
    NotAvailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum AsideAnswerReason {
    Unavailable(AsideUnavailableReason),
    Timeout,
    RuntimeFailed,
    Cancelled,
    Removed,
    DaemonStopped,
    RunReset,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideUsage {
    pub counters: UsageCounters,
    pub cost: Option<CostEstimate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideAnswerMeta {
    pub target: AsideTarget,
    pub runtime: RuntimeKind,
    pub model: Option<Label>,
    pub status: AsideStatus,
    pub reason: Option<AsideAnswerReason>,
    pub resets_at: Option<Timestamp>,
    pub context_at: Option<Timestamp>,
    pub context_approximate: bool,
    pub last_turn_at: Option<Timestamp>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub prefix_reproduced: bool,
    pub truncated: bool,
    pub usage: Option<AsideUsage>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideCounts {
    pub total: Wide,
    pub waiting: Wide,
    pub answering: Wide,
    pub answered: Wide,
    pub failed: Wide,
    pub cancelled: Wide,
    pub not_available: Wide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideSummary {
    pub id: Id,
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub asked_at: Timestamp,
    pub question_preview: Text<512>,
    pub question_truncated: bool,
    pub answer_counts: AsideCounts,
    /// Server aggregate, including observed failed/cancelled spend and its coverage.
    pub total_cost: Option<CostEstimate>,
    pub answers: List<AsideAnswerMeta, MAX_TARGETS>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideDetail {
    pub summary_id: Id,
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub question: Text<MAX_QUESTION_BYTES>,
    pub answer_meta: Option<AsideAnswerMeta>,
    /// Only a settled answer. Never a streamed partial body.
    pub answer_text: Option<Text<MAX_ANSWER_BYTES>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsideGoneCause {
    Removed,
    Evicted,
    DaemonRestart,
    RunReset,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AsideGetResult {
    Available {
        detail: Box<AsideDetail>,
    },
    Gone {
        id: Id,
        workspace_run_id: Id,
        aside_epoch: Id,
        revision: Wide,
        cause: AsideGoneCause,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AsideAskResult {
    Accepted {
        summary: Box<AsideSummary>,
        duplicate: bool,
    },
    Gone {
        id: Id,
        workspace_run_id: Id,
        aside_epoch: Id,
        revision: Wide,
        cause: AsideGoneCause,
    },
}

/// Typed RpcError data. Refusal is whole-request and keeps the composer's draft.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AsideAdmissionFailure {
    QuestionLimit { max_bytes: u32 },
    TargetLimit { max_targets: u16 },
    PendingLimit { max_pending: u16 },
    MemoryLimit { max_bytes: Wide },
    ReceiptLimit { max_receipts: Wide, max_bytes: Wide },
    EmptyQuestion,
    EmptyTargets,
    DuplicateTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsideAsk {
    pub workspace_run_id: Id,
    pub request_id: Id,
    pub question: Text<MAX_QUESTION_BYTES>,
    pub targets: Option<List<AsideTarget, MAX_TARGETS>>,
}

impl AsideAsk {
    pub fn validate(&self) -> Result<(), AsideAdmissionFailure> {
        if self.question.trim().is_empty() {
            return Err(AsideAdmissionFailure::EmptyQuestion);
        }
        if let Some(targets) = &self.targets {
            if targets.is_empty() {
                return Err(AsideAdmissionFailure::EmptyTargets);
            }
            for (i, target) in targets.iter().enumerate() {
                if targets[..i].iter().any(|t| t.agent_id == target.agent_id) {
                    return Err(AsideAdmissionFailure::DuplicateTarget);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsideList {
    pub workspace_run_id: Id,
    pub page: PageRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsideGet {
    pub workspace_run_id: Id,
    pub id: Id,
    pub agent: Option<AsideTarget>,
}

pub type AsideCancel = AsideGet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsideRemove {
    pub workspace_run_id: Id,
    pub id: Id,
}

/// Cancel/remove acknowledgement. `changed=false` does not relabel successful
/// answers as cancelled; clients reconcile through the revision/list/detail path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideMutationResult {
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub id: Id,
    pub changed: bool,
    pub gone: Option<AsideGoneCause>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideListResult {
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub rows: List<AsideSummary, MAX_RETAINED>,
    pub next_cursor: Option<Id>,
    pub evicted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideMembers {
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub total: Wide,
    pub rows: List<AsideMember, 200>,
    pub next_cursor: Option<Id>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideWatchResult {
    pub watch_id: Id,
    pub initial: AsideListResult,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideGone {
    pub id: Id,
    pub cause: AsideGoneCause,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideChanged {
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub watch_id: Id,
    pub from_revision: Wide,
    pub to_revision: Wide,
    pub changed_ids: List<Id, 200>,
    pub gone: List<AsideGone, 200>,
    pub availability_changed: bool,
    pub resync_required: bool,
}

/// Persistence-only metadata. Never reconstruct a question/body from this record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsideReceipt {
    pub workspace_run_id: Id,
    pub request_id: Id,
    pub aside_id: Id,
    pub created_at: Timestamp,
    pub outcome: AsideReceiptOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsideReceiptOutcome {
    Admitted,
    Removed,
    Evicted,
    DaemonRestart,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideAsked {
    pub workspace_run_id: Id,
    pub id: Id,
    pub agents: List<AsideTarget, MAX_TARGETS>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideAnswered {
    pub workspace_run_id: Id,
    pub id: Id,
    pub counts: AsideCounts,
}
