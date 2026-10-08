//! Epic metadata and coherent board projections. These records do not schedule work.

use serde::{Deserialize, Serialize};

use crate::features::{Content, Id, Label, List, RunScope, Timestamp, Wide};
use crate::rpc::TaskCard;
use crate::Task;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpicStatus {
    Planned,
    Active,
    Blocked,
    Done,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicCounts {
    pub total: Wide,
    pub queued: Wide,
    pub ready: Wide,
    pub working: Wide,
    pub waiting: Wide,
    pub blocked: Wide,
    pub review: Wide,
    pub done: Wide,
    pub failed: Wide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicAgentCounts {
    pub agent_id: String,
    pub assigned: Wide,
    pub unfinished_assigned: Wide,
    pub review: Wide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicReport {
    pub summary: Content,
    pub reported_at: Timestamp,
    pub covered_revision: Wide,
    pub decision_message_ids: List<Id, 64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicReportSummary {
    pub reported_at: Timestamp,
    pub covered_revision: Wide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Epic {
    pub id: String,
    pub title: Label,
    pub objective: Content,
    pub source_message_id: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub revision: Wide,
    pub status: EpicStatus,
    pub counts: EpicCounts,
    pub report: Option<EpicReport>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicSummary {
    pub id: String,
    pub title: Label,
    pub source_message_id: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub last_done_at: Option<Timestamp>,
    /// Work revision; report publication does not increment it.
    pub revision: Wide,
    pub status: EpicStatus,
    pub counts: EpicCounts,
    pub agents: Vec<EpicAgentCounts>,
    pub report: Option<EpicReportSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicBoard {
    pub generation: Id,
    pub revision: Wide,
    pub epics: Vec<EpicSummary>,
    pub no_epic: EpicCounts,
    pub focus_epic: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpicBoardChanged {
    pub workspace_run_id: Id,
    pub from_revision: Wide,
    pub to_revision: Wide,
    pub board: EpicBoard,
    pub tasks: Vec<Task>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum EpicFilter {
    #[default]
    All,
    Id(String),
    NoEpic,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum EpicMembershipPatch {
    #[default]
    Keep,
    Set(String),
    Clear,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpicCreate {
    pub workspace_run_id: Id,
    pub id: String,
    pub title: Label,
    pub objective: Content,
    pub source_message_id: Option<Id>,
}

impl EpicCreate {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_id(&self.id)?;
        validate_title(&self.title)
    }
}

pub fn validate_id(id: &str) -> Result<(), &'static str> {
    if id.len() > 32
        || !id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        Err("epic id must match [a-z][a-z0-9_-]{0,31}")
    } else {
        Ok(())
    }
}

pub fn validate_title(title: &str) -> Result<(), &'static str> {
    if title.is_empty() || title.trim() != title || title.chars().count() > 80 {
        Err("epic title must be trimmed, nonempty and at most 80 characters")
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpicEdit {
    pub workspace_run_id: Id,
    pub id: String,
    pub title: Option<Label>,
    pub objective: Option<Content>,
    pub expected_revision: Option<Wide>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpicGet {
    pub workspace_run_id: Id,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicDetail {
    pub scope: RunScope,
    pub board_revision: Wide,
    pub epic: Epic,
    pub agents: Vec<EpicAgentCounts>,
    pub task_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasksQuery {
    pub workspace_run_id: Id,
    pub epic: EpicFilter,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TasksQueryResult {
    pub workspace_run_id: Id,
    pub board_revision: Wide,
    pub tasks: Vec<Task>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGetScoped {
    pub workspace_run_id: Id,
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopedTaskCard {
    pub workspace_run_id: Id,
    pub board_revision: Wide,
    pub card: TaskCard,
    pub epic: Option<EpicSummary>,
}

pub type EpicCreateResult = EpicDetail;
pub type EpicEditResult = EpicDetail;
pub type TaskCreateScopedResult = ScopedTaskCard;
pub type TaskReviseResult = ScopedTaskCard;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRevise {
    pub workspace_run_id: Id,
    pub id: String,
    pub title: Option<Label>,
    pub description: Option<Content>,
    pub epic: EpicMembershipPatch,
    pub expected_board_revision: Option<Wide>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpicComplete {
    pub workspace_run_id: Id,
    pub id: String,
    pub summary: Content,
    pub decisions: List<Content, 64>,
    pub expected_revision: Option<Wide>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicCompleteResult {
    pub workspace_run_id: Id,
    pub board_revision: Wide,
    pub epic: EpicSummary,
    pub report: EpicReport,
}

/// Data for epic.created/updated/closed/reopened/reported. Bodies stay in detail reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicEventData {
    pub workspace_run_id: Id,
    pub epic_id: String,
    pub revision: Wide,
    pub before: Option<EpicSummary>,
    pub after: EpicSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEpicChanged {
    pub workspace_run_id: Id,
    pub task_id: String,
    pub board_revision: Wide,
    pub before: Option<String>,
    pub after: Option<String>,
}
