//! Adapters for the few records whose public representation differs from Rust.

use crew_client::api as client;
use crew_protocol::{rpc, types};
use serde_json::Value;

use super::shared::*;

/// JSON null is absence; other metadata is retained as deterministic JSON text.
fn metadata_json(value: Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // serde_json's default Map is ordered. Rebuild object keys explicitly so
    // this representation stays canonical if another dependency enables
    // `preserve_order` in a future integrated graph.
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(object) => {
                let sorted: std::collections::BTreeMap<_, _> = object.into_iter().collect();
                Value::Object(
                    sorted
                        .into_iter()
                        .map(|(key, value)| (key, canonical(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            value => value,
        }
    }
    Some(canonical(value).to_string())
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: MessageType,
    pub body: String,
    pub timestamp: i64,
    pub correlation_id: Option<String>,
    pub reply_to: Option<String>,
    pub metadata_json: Option<String>,
}

impl From<types::Message> for Message {
    fn from(value: types::Message) -> Self {
        let types::Message {
            id,
            from,
            to,
            kind,
            body,
            timestamp,
            correlation_id,
            reply_to,
            metadata,
        } = value;
        Self {
            id,
            from,
            to,
            kind,
            body,
            timestamp,
            correlation_id,
            reply_to,
            metadata_json: metadata_json(metadata),
        }
    }
}

/// Presentation facts from the same protocol helpers used by Rust clients.
/// These facts do not confer permission to answer the question.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct QuestionInfo {
    pub is_operator_question: bool,
    pub asker: String,
    pub asked_by: Option<String>,
    pub options: Vec<String>,
}

#[uniffi::export]
pub fn message_question_info(message: Message) -> Result<QuestionInfo, ClientError> {
    let metadata = match message.metadata_json {
        None => Value::Null,
        Some(text) => {
            if text.len() > types::MAX_FRAME_BYTES {
                return Err(ClientError::TooLarge {
                    actual: text.len() as u64,
                    limit: types::MAX_FRAME_BYTES as u64,
                });
            }
            serde_json::from_str(&text).map_err(|_| ClientError::DecodeFailure {
                message: "invalid message metadata".into(),
            })?
        }
    };
    let source = types::Message {
        id: message.id,
        from: message.from,
        to: message.to,
        kind: message.kind,
        body: message.body,
        timestamp: message.timestamp,
        correlation_id: message.correlation_id,
        reply_to: message.reply_to,
        metadata,
    };
    Ok(QuestionInfo {
        is_operator_question: source.is_operator_question(),
        asker: source.asker(),
        asked_by: source.asked_by().map(str::to_owned),
        options: source.options(),
    })
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Event {
    pub seq: i64,
    pub timestamp: i64,
    pub source: String,
    pub event_type: String,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub message: String,
    pub level: EventLevel,
    pub data_json: Option<String>,
}

impl From<types::Event> for Event {
    fn from(value: types::Event) -> Self {
        let types::Event {
            seq,
            timestamp,
            source,
            event_type,
            agent_id,
            task_id,
            message,
            level,
            data,
        } = value;
        Self {
            seq,
            timestamp,
            source,
            event_type,
            agent_id,
            task_id,
            message,
            level,
            data_json: metadata_json(data),
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct InboxEntry {
    pub message: Message,
    pub delivered_at: Option<i64>,
    pub acked_at: Option<i64>,
}

impl From<types::InboxEntry> for InboxEntry {
    fn from(value: types::InboxEntry) -> Self {
        let types::InboxEntry {
            message,
            delivered_at,
            acked_at,
        } = value;
        Self {
            message: message.into(),
            delivered_at,
            acked_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct InboxResult {
    pub messages: Vec<InboxEntry>,
}

impl From<rpc::InboxResult> for InboxResult {
    fn from(value: rpc::InboxResult) -> Self {
        let rpc::InboxResult { messages } = value;
        Self {
            messages: messages.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct CommitInfo {
    pub sha: String,
    pub short: String,
    pub branch: String,
    pub summary: String,
    pub files_changed: u64,
    pub change_id: Option<String>,
}

impl From<rpc::CommitInfo> for CommitInfo {
    fn from(value: rpc::CommitInfo) -> Self {
        let rpc::CommitInfo {
            sha,
            short,
            branch,
            summary,
            files_changed,
            change_id,
        } = value;
        // Native targets use at most 64-bit pointers. Widen the host-sized
        // count at the ABI boundary without exposing usize to Swift.
        const {
            assert!(std::mem::size_of::<usize>() <= std::mem::size_of::<u64>());
        }
        Self {
            sha,
            short,
            branch,
            summary,
            files_changed: files_changed as u64,
            change_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct GitOutcome {
    pub ok: bool,
    pub commit: Option<CommitInfo>,
    pub conflict: Option<ConflictReport>,
    pub text: String,
}

impl From<rpc::GitOutcome> for GitOutcome {
    fn from(value: rpc::GitOutcome) -> Self {
        let rpc::GitOutcome {
            ok,
            commit,
            conflict,
            text,
        } = value;
        Self {
            ok,
            commit: commit.map(Into::into),
            conflict,
            text,
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MessagePage {
    pub items: Vec<Message>,
    pub before: Option<HistoryCursor>,
    pub has_older_local: bool,
    pub may_have_older_remote: bool,
}

impl From<client::MessagePage> for MessagePage {
    fn from(value: client::MessagePage) -> Self {
        let client::MessagePage {
            items,
            before,
            has_older_local,
            may_have_older_remote,
        } = value;
        Self {
            items: items.into_iter().map(Into::into).collect(),
            before,
            has_older_local,
            may_have_older_remote,
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct EventPage {
    pub items: Vec<Event>,
    pub before: Option<HistoryCursor>,
    pub has_older_local: bool,
    pub may_have_older_remote: bool,
}

impl From<client::EventPage> for EventPage {
    fn from(value: client::EventPage) -> Self {
        let client::EventPage {
            items,
            before,
            has_older_local,
            may_have_older_remote,
        } = value;
        Self {
            items: items.into_iter().map(Into::into).collect(),
            before,
            has_older_local,
            may_have_older_remote,
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
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

impl From<client::ResourceValue> for ResourceValue {
    fn from(value: client::ResourceValue) -> Self {
        match value {
            client::ResourceValue::TaskCard(value) => Self::TaskCard(value),
            client::ResourceValue::Message(value) => Self::Message(value.map(Into::into)),
            client::ResourceValue::Event(value) => Self::Event(value.map(Into::into)),
            client::ResourceValue::Transcript(value) => Self::Transcript(value),
            client::ResourceValue::Diff(value) => Self::Diff(value.into()),
            client::ResourceValue::Output(value) => Self::Output(value),
            client::ResourceValue::Inbox(value) => Self::Inbox(value.into()),
            client::ResourceValue::Models(value) => Self::Models(value),
            client::ResourceValue::History(values) => {
                Self::History(values.into_iter().map(Into::into).collect())
            }
            client::ResourceValue::Events(values) => {
                Self::Events(values.into_iter().map(Into::into).collect())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct ResourceState {
    pub key: ResourceKey,
    pub status: ResourceStatus,
    pub request_sequence: u64,
    pub error: Option<ClientReason>,
    pub value: Option<ResourceValue>,
}

impl From<client::ResourceState> for ResourceState {
    fn from(value: client::ResourceState) -> Self {
        let client::ResourceState {
            key,
            status,
            request_sequence,
            error,
            value,
        } = value;
        Self {
            key,
            status,
            request_sequence,
            error,
            value: value.map(Into::into),
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct AgentDetailData {
    pub agent: Option<AgentPresentation>,
    pub assigned_tasks: Vec<TaskPresentation>,
    pub review_tasks: Vec<TaskPresentation>,
    pub recent_messages: MessagePage,
    pub resources: Vec<ResourceState>,
}

impl From<client::AgentDetailData> for AgentDetailData {
    fn from(value: client::AgentDetailData) -> Self {
        let client::AgentDetailData {
            agent,
            assigned_tasks,
            review_tasks,
            recent_messages,
            resources,
        } = value;
        Self {
            agent,
            assigned_tasks,
            review_tasks,
            recent_messages: recent_messages.into(),
            resources: resources.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct TaskDetailData {
    pub task: Option<TaskPresentation>,
    pub card: ResourceState,
}

impl From<client::TaskDetailData> for TaskDetailData {
    fn from(value: client::TaskDetailData) -> Self {
        let client::TaskDetailData { task, card } = value;
        Self {
            task,
            card: card.into(),
        }
    }
}

// Keep Rust's large view values behind boxes. UniFFI lowers each box as its
// underlying value; Swift sees a value-type alias, never a pointer or handle.
macro_rules! boxed_value {
    ($name:ident, $value:ty) => {
        pub type $name = Box<$value>;
        uniffi::custom_type!($name, $value, {
            remote,
            lower: |value| *value,
            try_lift: |value| Ok(Box::new(value)),
        });
    };
}

boxed_value!(BoxedOverview, OverviewData);
boxed_value!(BoxedTeam, TeamData);
boxed_value!(BoxedTasks, TasksData);
boxed_value!(BoxedAgentDetail, AgentDetailData);
boxed_value!(BoxedTaskDetail, TaskDetailData);
boxed_value!(BoxedMessages, MessagePage);
boxed_value!(BoxedEvents, EventPage);
boxed_value!(BoxedUsage, UsagePresentation);
boxed_value!(BoxedEvidence, ResourceState);
// Availability queries keep Rust's boxed intent. Swift passes the complete
// ActionIntent it would submit; answering the query allocates nothing.
boxed_value!(BoxedIntent, ActionIntent);

#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum ViewPayload {
    Availability(ActionAvailability),
    Overview(BoxedOverview),
    Team(BoxedTeam),
    Tasks(BoxedTasks),
    AgentDetail(BoxedAgentDetail),
    TaskDetail(BoxedTaskDetail),
    Messages(BoxedMessages),
    Events(BoxedEvents),
    Usage(BoxedUsage),
    Questions(Vec<Message>),
    Evidence(BoxedEvidence),
}

impl From<client::ViewPayload> for ViewPayload {
    fn from(value: client::ViewPayload) -> Self {
        match value {
            client::ViewPayload::Availability(value) => Self::Availability(value),
            client::ViewPayload::Overview(value) => Self::Overview(value),
            client::ViewPayload::Team(value) => Self::Team(value),
            client::ViewPayload::Tasks(value) => Self::Tasks(value),
            client::ViewPayload::AgentDetail(value) => Self::AgentDetail(Box::new((*value).into())),
            client::ViewPayload::TaskDetail(value) => Self::TaskDetail(Box::new((*value).into())),
            client::ViewPayload::Messages(value) => Self::Messages(Box::new((*value).into())),
            client::ViewPayload::Events(value) => Self::Events(Box::new((*value).into())),
            client::ViewPayload::Usage(value) => Self::Usage(value),
            client::ViewPayload::Questions(value) => {
                Self::Questions(value.into_iter().map(Into::into).collect())
            }
            client::ViewPayload::Evidence(value) => Self::Evidence(Box::new((*value).into())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct ViewData {
    pub scope: Option<ClientScope>,
    pub revision: u64,
    pub synchronized_at_ms: Option<i64>,
    pub freshness: Freshness,
    pub payload: ViewPayload,
}

impl From<client::ViewData> for ViewData {
    fn from(value: client::ViewData) -> Self {
        let client::ViewData {
            scope,
            revision,
            synchronized_at_ms,
            freshness,
            payload,
        } = value;
        Self {
            scope,
            revision,
            synchronized_at_ms,
            freshness,
            payload: payload.into(),
        }
    }
}

/// A service has already dispatched the Rust effects before returning this
/// decision to a native view. No effect or raw protocol command crosses FFI.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct PlanUpdate {
    pub plan: ActionPlan,
    pub revision: u64,
    pub changes: ChangeSet,
}
