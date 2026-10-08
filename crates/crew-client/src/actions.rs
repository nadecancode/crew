//! Scoped, storage-independent action planning and bounded operation state.
//!
//! ConnectionContext comes from a trusted authenticated connector. Eligibility
//! is a local preflight; it never substitutes for daemon/gateway authorization.
//! The reducer owns domain/resource state and revision advancement.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD, Engine};
use crew_protocol::rpc::{SendParams, SessionInfo};
use crew_protocol::{Agent, Message, MessageType, RuntimeKind, Task, UsageLimit, BROADCAST};

use crate::policy::{self, ConfirmationRule, ReplayPolicy};
use crate::*;

/// The driver reserves the upper half for bootstrap operation IDs. Wire
/// request IDs, channel IDs and host timer handles have separate lifetimes.
pub const DRIVER_OPERATION_ID_START: u64 = 1 << 63;
pub const CORE_OPERATION_ID_MAX: u64 = DRIVER_OPERATION_ID_START - 1;

/// Borrowed current facts. No selected UI entity or credential lives here.
pub struct ActionContext<'a> {
    pub features: Option<&'a crate::features::FeatureAvailability>,
    pub feature_watches: &'a [crate::features::FeatureWatchState],
    pub connection: Option<&'a ConnectionContext>,
    pub endpoint: EndpointStatus,
    pub workspace: WorkspaceStatus,
    pub agents: &'a [Agent],
    pub tasks: &'a [Task],
    pub questions: &'a [Message],
    pub session: Option<&'a SessionInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReadStarted {
    pub operation_id: OperationId,
    pub key: ResourceKey,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompletedOperation {
    pub operation_id: OperationId,
    pub scope: ClientScope,
    pub command: Option<Command>,
    pub resource: Option<ResourceKey>,
    pub outcome: OperationOutcome,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionUpdate {
    pub changes: ChangeSet,
    pub effects: Vec<ClientEffect>,
    pub reads_started: Vec<ReadStarted>,
    pub completions: Vec<CompletedOperation>,
}

impl ActionUpdate {
    pub fn append(&mut self, mut other: Self) {
        self.changes.pending |= other.changes.pending;
        self.changes.availability |= other.changes.availability;
        self.changes.questions |= other.changes.questions;
        self.effects.append(&mut other.effects);
        self.reads_started.append(&mut other.reads_started);
        self.completions.append(&mut other.completions);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActionDecision {
    pub plan: ActionPlan,
    pub update: ActionUpdate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    State,
    Action,
    Read,
    Attach,
    Terminal(AttachmentId),
    Detach(AttachmentId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentFacts {
    id: String,
    runtime: RuntimeKind,
    backend: String,
    backend_ref: Option<String>,
    terminal: bool,
    pid: Option<u32>,
    started_at: Option<i64>,
    session_id: Option<String>,
    limit: Option<UsageLimit>,
}

impl AgentFacts {
    fn capture(agent: &Agent, limit: bool) -> Self {
        Self {
            id: agent.id.clone(),
            runtime: agent.runtime,
            backend: agent.backend.clone(),
            backend_ref: agent.backend_ref.clone(),
            terminal: policy::status_policy(agent.status).terminal(),
            pid: agent.pid,
            started_at: agent.started_at,
            session_id: agent.session_id.clone(),
            limit: limit.then(|| agent.limit.clone()).flatten(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Proposal {
    intent: ActionIntent,
    scope: ClientScope,
    grant: Grant,
    mode: ConnectionMode,
    command: Option<Command>,
    resource: Option<ResourceKey>,
    route: Route,
    targets: Vec<String>,
    facts: Vec<AgentFacts>,
    confirmation: Option<ConfirmationKind>,
}

#[derive(Clone, Debug)]
struct Pending {
    proposal: Proposal,
    retried: bool,
}

#[derive(Clone, Debug)]
struct Retry {
    pending: Pending,
    timer_id: TimerId,
    deadline: u64,
    reason: ClientReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachmentStatus {
    Pending,
    Live,
    Closing,
}

#[derive(Clone, Debug)]
struct Attachment {
    scope: ClientScope,
    agent: String,
    status: AttachmentStatus,
}

/// Pending operations, confirmations and retry waits share one bounded cap.
/// Live attachment count is independently bounded by the same configuration.
pub struct Actions {
    maximum: usize,
    next_operation: u64,
    next_confirmation: u64,
    next_timer: u64,
    pending: BTreeMap<OperationId, Pending>,
    confirmations: BTreeMap<ConfirmationId, Proposal>,
    retries: BTreeMap<OperationId, Retry>,
    attachments: BTreeMap<AttachmentId, Attachment>,
}

enum PlanFailure {
    Invalid(ClientError),
    Unavailable(ClientReason),
}
impl From<ClientError> for PlanFailure {
    fn from(error: ClientError) -> Self {
        Self::Invalid(error)
    }
}

fn reason(code: &str, message: impl Into<String>) -> ClientReason {
    ClientReason {
        details: None,
        code: code.into(),
        message: message.into(),
    }
}

fn unavailable(code: &str, message: impl Into<String>) -> PlanFailure {
    PlanFailure::Unavailable(reason(code, message))
}

fn invalid(field: &str, message: &str) -> ClientError {
    ClientError::InvalidInput {
        field: field.into(),
        reason: message.into(),
    }
}

fn body(text: &str, field: &str, reject_slash: bool) -> Result<(), ClientError> {
    if text.trim().is_empty() {
        return Err(invalid(field, "some text is required"));
    }
    if text.len() > policy::VALIDATION_LIMITS.message_bytes {
        return Err(ClientError::TooLarge {
            actual: text.len() as u64,
            limit: policy::VALIDATION_LIMITS.message_bytes as u64,
        });
    }
    if reject_slash {
        if let Some((name, _)) = policy::slash_command(text) {
            let message = if policy::command_policy(name).is_some() {
                "use the corresponding action; prefix a space to send command text"
            } else {
                "unknown slash command; prefix a space to send command text"
            };
            return Err(invalid(field, message));
        }
    }
    Ok(())
}

fn checked_id(id: &str) -> Result<(), ClientError> {
    if policy::is_valid_id(id) {
        Ok(())
    } else {
        Err(invalid("agent", "expected [a-z][a-z0-9_-]{0,31}"))
    }
}

fn connection<'a>(
    scope: &ClientScope,
    context: &'a ActionContext<'_>,
    clock: ClockSample,
    kind: Option<CommandKind>,
) -> Result<&'a ConnectionContext, PlanFailure> {
    let Some(connection) = context.connection else {
        return Err(unavailable("not_connected", "not connected to a workspace"));
    };
    if &connection.scope != scope {
        return Err(unavailable(
            "stale_scope",
            "workspace or connection changed",
        ));
    }
    if context.endpoint != EndpointStatus::Authenticated
        || context.workspace != WorkspaceStatus::Live
    {
        return Err(unavailable(
            "not_live",
            "workspace is not connected with current data",
        ));
    }
    if connection
        .grant
        .expires_at_ms
        .is_some_and(|expiry| clock.wall_ms >= expiry)
    {
        return Err(unavailable("grant_expired", "workspace authority expired"));
    }
    if connection.mode == ConnectionMode::ScopedGateway {
        if !connection.grant.workspace_ids.contains(&scope.workspace_id)
            || !connection.grant.tiers.contains(&GrantTier::Read)
        {
            return Err(unavailable(
                "workspace_not_granted",
                "workspace access is not granted",
            ));
        }
        if let Some(kind) = kind {
            let Some(tier) = policy::method_policy(kind).gateway.tier() else {
                return Err(unavailable(
                    "local_only",
                    "action requires the local operator connection",
                ));
            };
            if !connection.grant.tiers.contains(&tier) {
                return Err(unavailable(
                    "grant_required",
                    format!("{tier:?} authority is required"),
                ));
            }
        }
    }
    Ok(connection)
}

fn agent<'a>(id: &str, context: &'a ActionContext<'_>) -> Result<&'a Agent, PlanFailure> {
    checked_id(id)?;
    context
        .agents
        .iter()
        .find(|agent| agent.id == id)
        .ok_or_else(|| {
            unavailable(
                "agent_missing",
                format!("no agent '{id}' in this workspace"),
            )
        })
}

fn running(agent: &Agent) -> Result<(), PlanFailure> {
    if policy::status_policy(agent.status).terminal() {
        Err(unavailable(
            "agent_not_running",
            format!("{} is not running", agent.id),
        ))
    } else {
        Ok(())
    }
}

/// The immutable scope supplied by the caller, including for unavailable
/// intents. A view must retain this identity rather than substitute selection.
pub fn action_scope(intent: &ActionIntent) -> &ClientScope {
    match intent {
        ActionIntent::Feature { scope, .. } => scope,
        ActionIntent::SendMessage { scope, .. }
        | ActionIntent::Answer { scope, .. }
        | ActionIntent::Agent { scope, .. }
        | ActionIntent::SetModel { scope, .. }
        | ActionIntent::AgentInput { scope, .. }
        | ActionIntent::ResumeLimited { scope, .. }
        | ActionIntent::StopTeam { scope }
        | ActionIntent::Read { scope, .. }
        | ActionIntent::Attach { scope, .. }
        | ActionIntent::AttachInput { scope, .. }
        | ActionIntent::AttachResize { scope, .. }
        | ActionIntent::Detach { scope, .. } => scope,
    }
}

fn action_kind(intent: &ActionIntent) -> ActionKind {
    match intent {
        ActionIntent::Feature { .. } => ActionKind::Feature,
        ActionIntent::SendMessage { .. } => ActionKind::SendMessage,
        ActionIntent::Answer { .. } => ActionKind::Answer,
        ActionIntent::Agent { action, .. } => match action {
            AgentAction::Pause => ActionKind::Pause,
            AgentAction::Resume => ActionKind::Resume,
            AgentAction::Compact => ActionKind::Compact,
            AgentAction::Restart => ActionKind::Restart,
            AgentAction::RestartFresh => ActionKind::RestartFresh,
            AgentAction::Kill => ActionKind::Kill,
            AgentAction::Focus => ActionKind::Focus,
        },
        ActionIntent::SetModel { .. } => ActionKind::SetModel,
        ActionIntent::AgentInput { .. } => ActionKind::AgentInput,
        ActionIntent::ResumeLimited { .. } => ActionKind::ResumeLimited,
        ActionIntent::StopTeam { .. } => ActionKind::StopTeam,
        ActionIntent::Read { .. } => ActionKind::Read,
        ActionIntent::Attach { .. } => ActionKind::Attach,
        ActionIntent::AttachInput { .. } => ActionKind::AttachInput,
        ActionIntent::AttachResize { .. } => ActionKind::AttachResize,
        ActionIntent::Detach { .. } => ActionKind::Detach,
    }
}

/// Canonical effective options are shared with resource storage and queries.
/// Event evidence is local-only and produces no command.
pub fn canonical_resource_key(key: &ResourceKey) -> Result<ResourceKey, ClientError> {
    if let ResourceKind::Feature(kind) = key.kind {
        let ReadOptions::Feature(read) = &key.options else {
            return Err(invalid("resource.options", "expected typed feature read"));
        };
        if !matches!(key.entity.entity, Entity::Workspace) || read.kind() != kind {
            return Err(invalid(
                "resource",
                "feature resource identity differs from its request",
            ));
        }
        read.validate()?;
        return Ok(key.clone());
    }
    let mut key = key.clone();
    let valid_entity = match key.kind {
        ResourceKind::Feature(_) => unreachable!("validated above"),
        ResourceKind::TaskCard => matches!(key.entity.entity, Entity::Task(_)),
        ResourceKind::Message => matches!(key.entity.entity, Entity::Message(_)),
        ResourceKind::Event => matches!(key.entity.entity, Entity::Event(_)),
        ResourceKind::Transcript
        | ResourceKind::Diff
        | ResourceKind::Output
        | ResourceKind::Inbox => matches!(key.entity.entity, Entity::Agent(_)),
        ResourceKind::Models => matches!(key.entity.entity, Entity::Workspace),
        ResourceKind::History | ResourceKind::Events => {
            matches!(key.entity.entity, Entity::Workspace | Entity::Agent(_))
        }
    };
    if !valid_entity {
        return Err(invalid(
            "resource.entity",
            "entity does not match resource kind",
        ));
    }
    let bounded = match key.kind {
        ResourceKind::Transcript => Some((400, 2_000)),
        ResourceKind::Inbox => Some((100, 500)),
        ResourceKind::History => Some((300, 2_000)),
        ResourceKind::Events => Some((200, 5_000)),
        _ => None,
    };
    if let Some((default, maximum)) = bounded {
        let limit = match key.options {
            ReadOptions::Default => default,
            ReadOptions::Bounded { limit } => limit,
            _ => return Err(invalid("resource.options", "expected bounded read options")),
        };
        if limit == 0 || limit > maximum {
            return Err(invalid("resource.limit", &format!("expected 1–{maximum}")));
        }
        key.options = ReadOptions::Bounded { limit };
    } else {
        key.options = match (key.kind, key.options) {
            (ResourceKind::Output, ReadOptions::Default) => ReadOptions::Output {
                lines: 300,
                screen: false,
                ansi: false,
            },
            (
                ResourceKind::Output,
                ReadOptions::Output {
                    lines,
                    screen,
                    ansi,
                },
            ) if (1..=2_000).contains(&lines) => ReadOptions::Output {
                lines,
                screen: screen || ansi,
                ansi,
            },
            (ResourceKind::Diff, ReadOptions::Default) => ReadOptions::Diff { against: None },
            (ResourceKind::Diff, options @ ReadOptions::Diff { .. }) => options,
            (
                ResourceKind::TaskCard
                | ResourceKind::Message
                | ResourceKind::Event
                | ResourceKind::Models,
                ReadOptions::Default,
            ) => ReadOptions::Default,
            _ => {
                return Err(invalid(
                    "resource.options",
                    "options do not match resource kind or bounds",
                ))
            }
        };
    }
    Ok(key)
}

/// Build a read from a canonicalized key; never forwards UI-supplied method
/// strings or unsupported filter/ack parameters.
pub fn resource_command(key: &ResourceKey) -> Result<Option<Command>, ClientError> {
    let key = canonical_resource_key(key)?;
    if let ReadOptions::Feature(read) = &key.options {
        return Ok(Some(read.command()));
    }
    let id = match &key.entity.entity {
        Entity::Agent(id) | Entity::Task(id) | Entity::Message(id) => Some(id.clone()),
        _ => None,
    };
    let limit = match key.options {
        ReadOptions::Bounded { limit } => Some(limit),
        _ => None,
    };
    Ok(Some(match key.kind {
        ResourceKind::Feature(_) => unreachable!("handled above"),
        ResourceKind::TaskCard => Command::TaskGet { id },
        ResourceKind::Message => Command::MessageGet {
            id: id.expect("validated message entity"),
        },
        ResourceKind::Event => return Ok(None),
        ResourceKind::Transcript => Command::AgentTranscript {
            id: id.expect("validated agent entity"),
            limit,
        },
        ResourceKind::Diff => {
            let ReadOptions::Diff { against } = key.options else {
                unreachable!("canonical diff options")
            };
            Command::GitDiff { against, agent: id }
        }
        ResourceKind::Output => {
            let ReadOptions::Output {
                lines,
                screen,
                ansi,
            } = key.options
            else {
                unreachable!("canonical output options")
            };
            Command::AgentOutput {
                id: id.expect("validated agent entity"),
                lines: Some(lines),
                screen,
                ansi,
            }
        }
        ResourceKind::Inbox => Command::Inbox { agent: id, limit },
        ResourceKind::Models => Command::ModelsList,
        ResourceKind::History => Command::History { agent: id, limit },
        ResourceKind::Events => Command::Events { agent: id, limit },
    }))
}

impl Actions {
    pub fn new(max_pending_operations: u32) -> Result<Self, ClientError> {
        if max_pending_operations == 0
            || max_pending_operations > policy::NATIVE_LIMITS.pending_operations
        {
            return Err(invalid("pending_operations", "expected 1–64"));
        }
        Ok(Self {
            maximum: max_pending_operations as usize,
            next_operation: 1,
            next_confirmation: 1,
            next_timer: 1,
            pending: BTreeMap::new(),
            confirmations: BTreeMap::new(),
            retries: BTreeMap::new(),
            attachments: BTreeMap::new(),
        })
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len() + self.confirmations.len() + self.retries.len()
    }

    pub(crate) fn purge_aside(
        &mut self,
        run: &crew_protocol::features::Id,
        id: &crew_protocol::features::Id,
    ) {
        fn clear(
            proposal: &mut Proposal,
            run: &crew_protocol::features::Id,
            id: &crew_protocol::features::Id,
        ) {
            if let Some(Command::AsideAsk(q)) = &mut proposal.command {
                if &q.workspace_run_id == run && &q.request_id == id {
                    q.question = Default::default();
                }
            }
            if let ActionIntent::Feature {
                action: crate::features::FeatureAction::AskAside(q),
                ..
            } = &mut proposal.intent
            {
                if &q.workspace_run_id == run && &q.request_id == id {
                    q.question = Default::default();
                }
            }
        }
        for pending in self.pending.values_mut() {
            clear(&mut pending.proposal, run, id);
        }
        for proposal in self.confirmations.values_mut() {
            clear(proposal, run, id);
        }
        for retry in self.retries.values_mut() {
            clear(&mut retry.pending.proposal, run, id);
        }
    }

    pub fn pending_answer_ids(&self) -> impl Iterator<Item = &str> {
        self.pending
            .values()
            .filter_map(|pending| match pending.proposal.command.as_ref() {
                Some(Command::Answer { id, .. }) => Some(id.as_str()),
                _ => None,
            })
    }

    pub fn attachment_is_live(&self, scope: &ClientScope, id: AttachmentId) -> bool {
        self.attachments.get(&id).is_some_and(|attachment| {
            &attachment.scope == scope && attachment.status == AttachmentStatus::Live
        })
    }

    fn prepare(
        &self,
        intent: ActionIntent,
        context: &ActionContext<'_>,
        clock: ClockSample,
    ) -> Result<Proposal, PlanFailure> {
        let scope = action_scope(&intent).clone();
        // Check identity before validating or exposing facts about a stale target.
        let current = connection(&scope, context, clock, None)?;
        let mut targets = Vec::new();
        let mut facts = Vec::new();
        let mut resource = None;
        let mut route = Route::Action;
        let command = match &intent {
            ActionIntent::Feature { action, .. } => {
                action.validate()?;
                let command = action.command();
                let feature = command.required_feature().expect("feature action");
                if !context.features.is_some_and(|f| f.supports(feature)) {
                    return Err(unavailable(
                        "unsupported_feature",
                        "feature is unavailable on this connection",
                    ));
                }
                if action.current_run().is_some_and(|run| {
                    context.session.and_then(|s| s.workspace_run_id.as_deref())
                        != Some(run.as_str())
                }) {
                    return Err(unavailable(
                        "stale_scope",
                        "feature action belongs to another workspace run",
                    ));
                }
                if let crate::features::FeatureAction::UnwatchAsides(q)
                | crate::features::FeatureAction::UnwatchDocs(q)
                | crate::features::FeatureAction::UnwatchUsage(q) = action
                {
                    let epoch = context
                        .features
                        .and_then(|f| f.session.as_ref())
                        .map(|s| &s.connection_epoch);
                    if !context.feature_watches.iter().any(|w| {
                        w.feature == feature
                            && w.watch_id == q.watch_id
                            && Some(&w.connection_epoch) == epoch
                    }) {
                        return Err(unavailable(
                            "stale_scope",
                            "Watch belongs to another feature or connection.",
                        ));
                    }
                }
                let watch_limit = match action {
                    crate::features::FeatureAction::WatchAsides(_)
                    | crate::features::FeatureAction::WatchDocs(_) => Some(1),
                    crate::features::FeatureAction::WatchUsage(_) => Some(4),
                    _ => None,
                };
                if let Some(limit) = watch_limit {
                    let active = context
                        .feature_watches
                        .iter()
                        .filter(|w| w.feature == feature)
                        .count();
                    let pending = self
                        .pending
                        .values()
                        .filter(|p| {
                            p.proposal
                                .command
                                .as_ref()
                                .is_some_and(|c| c.kind() == command.kind())
                        })
                        .count();
                    if active + pending >= limit {
                        return Err(unavailable(
                            "watch_limit",
                            "Close an existing watch before opening another.",
                        ));
                    }
                }
                if action.observer() {
                    route = Route::State;
                }
                Some(command)
            }
            ActionIntent::SendMessage {
                to,
                body: text,
                message_id,
                ..
            } => {
                body(text, "body", true)?;
                if !policy::is_valid_message_id(message_id) {
                    return Err(invalid(
                        "message_id",
                        "expected 1–64 ASCII letters, digits, _, ., : or -",
                    )
                    .into());
                }
                if to != BROADCAST {
                    agent(to, context)?;
                }
                targets.push(to.clone());
                Some(Command::Send(SendParams {
                    to: to.clone(),
                    body: text.clone(),
                    id: Some(message_id.clone()),
                    kind: MessageType::Message,
                    ..SendParams::default()
                }))
            }
            ActionIntent::Answer {
                question_id,
                body: text,
                ..
            } => {
                body(text, "answer", true)?;
                let question = context
                    .questions
                    .iter()
                    .find(|question| question.id == *question_id && question.is_operator_question())
                    .ok_or_else(|| unavailable("question_missing", "question is no longer open"))?;
                agent(&question.from, context)?;
                targets.push(question_id.clone());
                Some(Command::Answer {
                    id: question_id.clone(),
                    body: text.clone(),
                })
            }
            ActionIntent::Agent { id, action, .. } => {
                let agent = agent(id, context)?;
                targets.push(id.clone());
                facts.push(AgentFacts::capture(agent, false));
                Some(match action {
                    AgentAction::Pause => {
                        running(agent)?;
                        Command::AgentPause { id: id.clone() }
                    }
                    AgentAction::Resume => {
                        running(agent)?;
                        Command::AgentResume { id: id.clone() }
                    }
                    AgentAction::Compact => {
                        running(agent)?;
                        if agent.runtime == RuntimeKind::Shell {
                            return Err(unavailable(
                                "compact_unsupported",
                                "this runtime does not support context compaction",
                            ));
                        }
                        Command::AgentCompact {
                            id: Some(id.clone()),
                            keep: None,
                        }
                    }
                    AgentAction::Restart => Command::AgentRestart { id: id.clone() },
                    AgentAction::RestartFresh => Command::AgentRestartFresh { id: id.clone() },
                    AgentAction::Kill => Command::AgentKill { id: id.clone() },
                    AgentAction::Focus => {
                        if agent.backend != "herdr" {
                            return Err(unavailable(
                                "focus_unsupported",
                                "pane focus requires the herdr backend",
                            ));
                        }
                        Command::AgentFocus { id: id.clone() }
                    }
                })
            }
            ActionIntent::SetModel {
                id,
                runtime,
                model,
                note,
                ..
            } => {
                agent(id, context)?;
                targets.push(id.clone());
                Some(Command::AgentSetModel {
                    id: id.clone(),
                    runtime: *runtime,
                    model: model.clone(),
                    note: note.clone(),
                })
            }
            ActionIntent::AgentInput {
                id, text, submit, ..
            } => {
                running(agent(id, context)?)?;
                // Raw terminal input can be empty; submitted prompts cannot.
                // Coalesced raw batches stay within the shared 64 KiB bound.
                // Whitespace and control characters remain valid raw input.
                if let Some(text) = text
                    .as_ref()
                    .filter(|text| !*submit && text.len() > policy::VALIDATION_LIMITS.message_bytes)
                {
                    return Err(ClientError::TooLarge {
                        actual: text.len() as u64,
                        limit: policy::VALIDATION_LIMITS.message_bytes as u64,
                    }
                    .into());
                }
                if *submit && text.as_deref().unwrap_or_default().trim().is_empty() {
                    return Err(invalid("text", "a submitted prompt needs some text").into());
                }
                targets.push(id.clone());
                Some(Command::AgentInput {
                    id: id.clone(),
                    text: text.clone(),
                    submit: *submit,
                })
            }
            ActionIntent::ResumeLimited { restart, .. } => {
                for agent in context
                    .agents
                    .iter()
                    .filter(|agent| agent.limit.is_some() && !agent.status.is_terminal())
                {
                    targets.push(agent.id.clone());
                    facts.push(AgentFacts::capture(agent, true));
                }
                if targets.is_empty() {
                    return Err(unavailable(
                        "no_limited_agents",
                        "no agent is paused at a usage limit",
                    ));
                }
                Some(Command::Resume {
                    agents: targets.clone(),
                    restart: *restart,
                })
            }
            ActionIntent::StopTeam { .. } => {
                for agent in context.agents {
                    targets.push(agent.id.clone());
                    facts.push(AgentFacts::capture(agent, false));
                }
                Some(Command::Shutdown { stop_agents: true })
            }
            ActionIntent::Read {
                resource: request, ..
            } => {
                if request.key.entity.scope != scope {
                    return Err(unavailable(
                        "stale_scope",
                        "evidence belongs to another workspace or connection",
                    ));
                }
                let key = canonical_resource_key(&request.key)?;
                if let ReadOptions::Feature(read) = &key.options {
                    let feature = read.command().required_feature().expect("feature read");
                    if !context.features.is_some_and(|f| f.supports(feature)) {
                        return Err(unavailable(
                            "unsupported_feature",
                            "feature is unavailable on this connection",
                        ));
                    }
                    if read.current_run().is_some_and(|run| {
                        context.session.and_then(|s| s.workspace_run_id.as_deref())
                            != Some(run.as_str())
                    }) {
                        return Err(unavailable(
                            "stale_scope",
                            "feature read belongs to another workspace run",
                        ));
                    }
                }
                match &key.entity.entity {
                    Entity::Agent(id) => {
                        agent(id, context)?;
                    }
                    Entity::Task(id) => {
                        if !policy::is_valid_id(id) {
                            return Err(invalid("task", "invalid task identifier").into());
                        }
                        if !context.tasks.iter().any(|task| task.id == *id) {
                            return Err(unavailable("task_missing", "task is no longer present"));
                        }
                    }
                    Entity::Message(id) => {
                        if !policy::is_valid_message_id(id) {
                            return Err(invalid("message", "invalid message identifier").into());
                        }
                    }
                    Entity::Event(_) => {
                        return Err(unavailable(
                            "local_evidence",
                            "event evidence is read from retained history",
                        ))
                    }
                    Entity::Workspace => {}
                }
                if let Entity::Agent(id) | Entity::Task(id) | Entity::Message(id) =
                    &key.entity.entity
                {
                    targets.push(id.clone());
                }
                let command = resource_command(&key)?;
                resource = Some(key);
                // A model catalogue is evidence but requires Control on an
                // Action-purpose channel. Resource lifecycle is independent
                // of transport lane; derive the lane from the live policy.
                route = if command.as_ref().is_some_and(|command| {
                    policy::method_policy(command.kind()).gateway
                        == policy::GatewayPermission::Control
                }) {
                    Route::Action
                } else {
                    Route::Read
                };
                command
            }
            ActionIntent::Attach {
                agent: id,
                rows,
                cols,
                ..
            } => {
                running(agent(id, context)?)?;
                if self.attachments.len() >= self.maximum {
                    return Err(unavailable(
                        "attachment_capacity",
                        "too many terminal attachments",
                    ));
                }
                targets.push(id.clone());
                route = Route::Attach;
                Some(Command::Attach {
                    agent: id.clone(),
                    rows: (*rows).clamp(5, 500),
                    cols: (*cols).clamp(20, 1_000),
                })
            }
            ActionIntent::AttachInput {
                attachment_id,
                data,
                ..
            } => {
                self.live_attachment(&scope, *attachment_id)?;
                if data.len() > policy::VALIDATION_LIMITS.frame_bytes / 4 * 3 {
                    return Err(ClientError::TooLarge {
                        actual: data.len() as u64,
                        limit: (policy::VALIDATION_LIMITS.frame_bytes / 4 * 3) as u64,
                    }
                    .into());
                }
                route = Route::Terminal(*attachment_id);
                Some(Command::AttachInput {
                    data: STANDARD.encode(data),
                })
            }
            ActionIntent::AttachResize {
                attachment_id,
                rows,
                cols,
                ..
            } => {
                self.live_attachment(&scope, *attachment_id)?;
                route = Route::Terminal(*attachment_id);
                Some(Command::AttachResize {
                    rows: (*rows).clamp(5, 500),
                    cols: (*cols).clamp(20, 1_000),
                })
            }
            ActionIntent::Detach { attachment_id, .. } => {
                self.live_attachment(&scope, *attachment_id)?;
                // Local closure remains available when Terminal was removed,
                // but still requires the captured current scope/live binding.
                route = Route::Detach(*attachment_id);
                None
            }
        };
        let confirmation = if let Some(command) = &command {
            connection(&scope, context, clock, Some(command.kind()))?;
            // Validate complete JSON size before reserving any operation.
            command.encode(u64::MAX)?;
            match policy::method_policy(command.kind()).confirmation {
                ConfirmationRule::Always(kind) => Some(kind),
                ConfirmationRule::RestartFlag(kind)
                    if matches!(command, Command::Resume { restart: true, .. }) =>
                {
                    Some(kind)
                }
                _ => None,
            }
        } else {
            None
        };
        facts.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Proposal {
            intent,
            scope,
            grant: current.grant.clone(),
            mode: current.mode,
            command,
            resource,
            route,
            targets,
            facts,
            confirmation,
        })
    }

    fn live_attachment(
        &self,
        scope: &ClientScope,
        id: AttachmentId,
    ) -> Result<&Attachment, PlanFailure> {
        self.attachments
            .get(&id)
            .filter(|attachment| {
                &attachment.scope == scope && attachment.status == AttachmentStatus::Live
            })
            .ok_or_else(|| {
                unavailable(
                    "attachment_unavailable",
                    "terminal attachment is unknown, pending or closed",
                )
            })
    }

    fn preflight(
        &self,
        intent: ActionIntent,
        context: &ActionContext<'_>,
        clock: ClockSample,
    ) -> Result<Proposal, PlanFailure> {
        let proposal = self.prepare(intent, context, clock)?;
        if self.pending_count() >= self.maximum {
            return Err(unavailable(
                "operation_capacity",
                "too many pending operations",
            ));
        }
        if self.duplicate(&proposal) {
            return Err(unavailable(
                "already_pending",
                "this action is already pending",
            ));
        }
        // Check exhaustion without allocating. Availability and execution must
        // agree even at the final operation or confirmation ID boundary.
        if proposal.confirmation.is_some() {
            check_counter(self.next_confirmation, "confirmation_id_exhausted")?;
        } else {
            check_counter(self.next_operation, "operation_id_exhausted")?;
        }
        Ok(proposal)
    }

    /// Exact eligibility for a complete captured intent at the supplied clock.
    /// Available means execution would dispatch or request confirmation in the
    /// same state. Invalid payloads return the same error as `plan`.
    ///
    /// This borrows operation state, retains nothing from the intent and creates
    /// no IDs, confirmations, timers, effects or notices. It is not a reservation:
    /// actual execution repeats the preflight against its current context.
    pub fn intent_availability(
        &self,
        intent: &ActionIntent,
        context: &ActionContext<'_>,
        clock: ClockSample,
    ) -> Result<ActionAvailability, ClientError> {
        let reason = match self.preflight(intent.clone(), context, clock) {
            Ok(_) => None,
            Err(PlanFailure::Unavailable(reason)) => Some(reason),
            Err(PlanFailure::Invalid(error)) => return Err(error),
        };
        Ok(ActionAvailability {
            action: action_kind(intent),
            available: reason.is_none(),
            reason,
        })
    }

    pub fn plan(
        &mut self,
        intent: ActionIntent,
        context: &ActionContext<'_>,
        clock: ClockSample,
    ) -> Result<ActionDecision, ClientError> {
        let proposal = match self.preflight(intent, context, clock) {
            Ok(proposal) => proposal,
            Err(PlanFailure::Invalid(error)) => return Err(error),
            Err(PlanFailure::Unavailable(reason)) => {
                return Ok(ActionDecision {
                    plan: ActionPlan::Unavailable { reason },
                    update: ActionUpdate::default(),
                })
            }
        };
        if let Some(kind) = proposal.confirmation {
            let id = allocate(&mut self.next_confirmation, "confirmation_id_exhausted")?;
            let plan = ActionPlan::Confirm {
                confirmation_id: id,
                kind,
                scope: proposal.scope.clone(),
                target_ids: proposal.targets.clone(),
            };
            self.confirmations.insert(id, proposal);
            Ok(ActionDecision {
                plan,
                update: pending_update(false),
            })
        } else {
            let (id, update) = self.dispatch(proposal, false)?;
            Ok(ActionDecision {
                plan: ActionPlan::Dispatch { operation_id: id },
                update,
            })
        }
    }

    fn duplicate(&self, proposal: &Proposal) -> bool {
        // Terminal traffic and independent reads never serialize behind a slow
        // action. Identified sends conflict by immutable message ID.
        let same = |other: &Proposal| match (&other.command, &proposal.command) {
            (Some(Command::Send(left)), Some(Command::Send(right))) => {
                other.scope.endpoint_id == proposal.scope.endpoint_id
                    && other.scope.workspace_id == proposal.scope.workspace_id
                    && left.id == right.id
            }
            (
                Some(Command::AgentInput { submit: left, .. }),
                Some(Command::AgentInput { submit: right, .. }),
            ) => {
                other.scope == proposal.scope
                    && *left
                    && *right
                    && other.targets == proposal.targets
            }
            (Some(left), Some(right))
                if other.scope == proposal.scope && proposal.route == Route::Action =>
            {
                left.kind() == right.kind() && other.targets == proposal.targets
            }
            _ => false,
        };
        self.pending.values().any(|pending| same(&pending.proposal))
            || self.confirmations.values().any(same)
            || self
                .retries
                .values()
                .any(|retry| same(&retry.pending.proposal))
    }

    fn dispatch(
        &mut self,
        proposal: Proposal,
        retried: bool,
    ) -> Result<(OperationId, ActionUpdate), ClientError> {
        let id = allocate(&mut self.next_operation, "operation_id_exhausted")?;
        let mut update = pending_update(matches!(proposal.command, Some(Command::Answer { .. })));
        let lane = match proposal.route {
            Route::State => RequestLane::State,
            Route::Read => match proposal.resource.as_ref().map(|r| &r.options) {
                Some(ReadOptions::Feature(read)) if read.lease.is_some() => RequestLane::Document {
                    lease: read.lease.clone().expect("checked lease"),
                },
                _ => RequestLane::Read,
            },
            Route::Action => RequestLane::Action,
            Route::Attach => {
                let Some(Command::Attach { agent, .. }) = &proposal.command else {
                    return Err(ClientError::InternalFailure {
                        code: "attach_command_missing".into(),
                    });
                };
                self.attachments.insert(
                    id,
                    Attachment {
                        scope: proposal.scope.clone(),
                        agent: agent.clone(),
                        status: AttachmentStatus::Pending,
                    },
                );
                RequestLane::Terminal { attachment_id: id }
            }
            Route::Terminal(attachment_id) => RequestLane::Terminal { attachment_id },
            Route::Detach(attachment_id) => {
                if let Some(attachment) = self.attachments.get_mut(&attachment_id) {
                    attachment.status = AttachmentStatus::Closing;
                }
                update.effects.push(ClientEffect::CloseAttachment {
                    scope: proposal.scope.clone(),
                    operation_id: id,
                    attachment_id,
                });
                self.pending.insert(id, Pending { proposal, retried });
                return Ok((id, update));
            }
        };
        if let Some(key) = &proposal.resource {
            update.reads_started.push(ReadStarted {
                operation_id: id,
                key: key.clone(),
            });
        }
        let Some(command) = &proposal.command else {
            return Err(ClientError::InternalFailure {
                code: "request_command_missing".into(),
            });
        };
        update.effects.push(ClientEffect::Request {
            scope: proposal.scope.clone(),
            operation_id: id,
            lane,
            command: command.clone(),
        });
        self.pending.insert(id, Pending { proposal, retried });
        Ok((id, update))
    }

    pub fn confirm(
        &mut self,
        id: ConfirmationId,
        context: &ActionContext<'_>,
        clock: ClockSample,
    ) -> Result<ActionUpdate, ClientError> {
        let Some(proposal) = self.confirmations.remove(&id) else {
            return Err(ClientError::StaleConfirmation);
        };
        let current = self.prepare(proposal.intent.clone(), context, clock);
        match current {
            Ok(current)
                if current.scope == proposal.scope
                    && current.grant == proposal.grant
                    && current.mode == proposal.mode
                    && current.facts == proposal.facts
                    && current.confirmation == proposal.confirmation =>
            {
                self.dispatch(proposal, false).map(|(_, update)| update)
            }
            _ => {
                let mut update = pending_update(false);
                update.effects.push(ClientEffect::Notice {
                    code: "stale_confirmation".into(),
                    entity: None,
                });
                Ok(update)
            }
        }
    }

    pub fn cancel_confirmation(&mut self, id: ConfirmationId) -> ActionUpdate {
        if self.confirmations.remove(&id).is_some() {
            pending_update(false)
        } else {
            ActionUpdate::default()
        }
    }

    /// Apply only an outcome for the exact pending scope/id. Old-attempt and
    /// already-completed outcomes cannot alter the current operation.
    pub fn finish(
        &mut self,
        scope: &ClientScope,
        id: OperationId,
        outcome: OperationOutcome,
        context: &ActionContext<'_>,
        clock: ClockSample,
    ) -> Result<ActionUpdate, ClientError> {
        if !self
            .pending
            .get(&id)
            .is_some_and(|pending| &pending.proposal.scope == scope)
        {
            return Ok(ActionUpdate::default());
        }
        if context
            .connection
            .is_some_and(|connection| &connection.scope != scope)
            || (context.connection.is_none()
                && matches!(
                    outcome,
                    OperationOutcome::Succeeded { .. }
                        | OperationOutcome::SentUnacknowledged { .. }
                ))
        {
            return Ok(ActionUpdate::default());
        }
        let pending = self.pending.remove(&id).expect("checked pending operation");
        self.complete(id, pending, outcome, clock)
    }

    fn complete(
        &mut self,
        id: OperationId,
        pending: Pending,
        mut outcome: OperationOutcome,
        clock: ClockSample,
    ) -> Result<ActionUpdate, ClientError> {
        let mut update = pending_update(matches!(
            pending.proposal.command,
            Some(Command::Answer { .. })
        ));
        match pending.proposal.route {
            Route::Attach => {
                let acknowledged = matches!(&outcome, OperationOutcome::Succeeded { result: CommandResult::Attached { agent } }
                    if pending.proposal.targets.first() == Some(agent));
                if acknowledged
                    && self.attachments.get(&id).is_some_and(|attachment| {
                        attachment.scope == pending.proposal.scope
                            && attachment.status == AttachmentStatus::Pending
                    })
                {
                    self.attachments
                        .get_mut(&id)
                        .expect("checked attachment")
                        .status = AttachmentStatus::Live;
                } else {
                    if acknowledged {
                        outcome = OperationOutcome::OutcomeUnknown {
                            reason: reason(
                                "attachment_ended",
                                "attachment ended before its acknowledgement",
                            ),
                        };
                    }
                    if let Some(attachment) = self.attachments.remove(&id) {
                        update.effects.push(ClientEffect::CloseAttachment {
                            scope: attachment.scope,
                            operation_id: allocate(
                                &mut self.next_operation,
                                "operation_id_exhausted",
                            )?,
                            attachment_id: id,
                        });
                    }
                }
            }
            Route::Detach(attachment_id) => {
                if !matches!(&outcome, OperationOutcome::ClosedLocally { attachment_id: ended } if *ended == attachment_id)
                {
                    // There is no daemon command, and therefore no successful
                    // daemon result that could complete a local detach.
                    if matches!(
                        outcome,
                        OperationOutcome::Succeeded { .. }
                            | OperationOutcome::SentUnacknowledged { .. }
                            | OperationOutcome::ClosedLocally { .. }
                    ) {
                        outcome = OperationOutcome::OutcomeUnknown {
                            reason: reason(
                                "invalid_local_completion",
                                "local terminal closure could not be verified",
                            ),
                        };
                    }
                }
                self.attachments.remove(&attachment_id);
            }
            Route::Terminal(attachment_id) => {
                if matches!(
                    outcome,
                    OperationOutcome::Succeeded { .. } | OperationOutcome::ClosedLocally { .. }
                ) || matches!(&outcome, OperationOutcome::SentUnacknowledged { attachment_id: written } if *written != attachment_id)
                {
                    outcome = OperationOutcome::OutcomeUnknown {
                        reason: reason(
                            "invalid_terminal_completion",
                            "terminal write completion could not be verified",
                        ),
                    };
                }
            }
            Route::Action | Route::Read | Route::State => {
                if matches!(
                    outcome,
                    OperationOutcome::SentUnacknowledged { .. }
                        | OperationOutcome::ClosedLocally { .. }
                ) {
                    outcome = OperationOutcome::OutcomeUnknown {
                        reason: reason("invalid_completion", "daemon reply was not received"),
                    };
                }
            }
        }
        if let OperationOutcome::OutcomeUnknown { reason: closed } = &outcome {
            let retryable = !pending.retried
                && closed.code == "closed"
                && pending.proposal.command.as_ref().is_some_and(|command| {
                    policy::method_policy(command.kind()).replay
                        == ReplayPolicy::IdentifiedSendOnClose
                })
                && pending.proposal.scope.daemon_pid.is_some()
                && pending.proposal.scope.daemon_started_at_ms.is_some()
                && pending
                    .proposal
                    .grant
                    .expires_at_ms
                    .is_none_or(|expiry| clock.wall_ms < expiry);
            if retryable {
                let timer_id = allocate(&mut self.next_timer, "timer_id_exhausted")?;
                let deadline = clock
                    .monotonic_ms
                    .saturating_add(policy::MESSAGE_RETRY_WAIT_MS);
                self.retries.insert(
                    id,
                    Retry {
                        pending,
                        timer_id,
                        deadline,
                        reason: closed.clone(),
                    },
                );
                update.effects.push(ClientEffect::Schedule {
                    timer_id,
                    deadline_ms: deadline,
                });
                // The logical submission remains pending. Its final notice is
                // emitted once, after replay or expiry, never on both attempts.
                return Ok(update);
            }
        }
        update.completions.push(completed(id, pending, outcome));
        Ok(update)
    }

    pub fn attachment_ended(&mut self, scope: &ClientScope, id: AttachmentId) -> ActionUpdate {
        if self
            .attachments
            .get(&id)
            .is_some_and(|attachment| &attachment.scope == scope)
        {
            self.attachments.remove(&id);
            ActionUpdate {
                changes: ChangeSet {
                    availability: true,
                    ..ChangeSet::default()
                },
                ..ActionUpdate::default()
            }
        } else {
            ActionUpdate::default()
        }
    }

    /// Retire all captured authority. A connection closure alone can preserve
    /// the one bounded identified-message wait; every other reason cancels it.
    pub fn retire(
        &mut self,
        scope: &ClientScope,
        why: ClientReason,
        clock: ClockSample,
    ) -> Result<ActionUpdate, ClientError> {
        let mut update = ActionUpdate::default();
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, pending)| &pending.proposal.scope == scope)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let pending = self
                .pending
                .remove(&id)
                .expect("captured pending operation");
            let outcome = if pending.proposal.route == Route::Read {
                OperationOutcome::Canceled
            } else {
                OperationOutcome::OutcomeUnknown {
                    reason: why.clone(),
                }
            };
            update.append(self.complete(id, pending, outcome, clock)?);
        }
        let before = self.confirmations.len();
        self.confirmations
            .retain(|_, proposal| &proposal.scope != scope);
        if self.confirmations.len() != before {
            update.append(pending_update(false));
        }
        if why.code != "closed" {
            let ids: Vec<_> = self
                .retries
                .iter()
                .filter(|(_, retry)| &retry.pending.proposal.scope == scope)
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                update.append(self.expire_retry(id, Some(why.clone())));
            }
        }
        let ids: Vec<_> = self
            .attachments
            .iter()
            .filter(|(_, attachment)| &attachment.scope == scope)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let attachment = self.attachments.remove(&id).expect("captured attachment");
            if attachment.status != AttachmentStatus::Closing {
                update.effects.push(ClientEffect::CloseAttachment {
                    scope: scope.clone(),
                    operation_id: allocate(&mut self.next_operation, "operation_id_exhausted")?,
                    attachment_id: id,
                });
            }
            update.changes.availability = true;
        }
        Ok(update)
    }

    /// Finalize held waits when the core has independently verified a trusted
    /// abandonment of this logical endpoint/workspace. This entry is not an
    /// asynchronous result guard: the caller validates current generation and
    /// incarnation before calling it. Verification-only normalization and
    /// ordinary connection closure must not call this entry.
    pub fn invalidate_retries(&mut self, scope: &ClientScope, why: ClientReason) -> ActionUpdate {
        let ids: Vec<_> = self
            .retries
            .iter()
            .filter(|(_, retry)| {
                let captured = &retry.pending.proposal.scope;
                captured.endpoint_id == scope.endpoint_id
                    && captured.workspace_id == scope.workspace_id
            })
            .map(|(id, _)| *id)
            .collect();
        let mut update = ActionUpdate::default();
        for id in ids {
            update.append(self.expire_retry(id, Some(why.clone())));
        }
        update
    }

    fn expire_retry(&mut self, id: OperationId, why: Option<ClientReason>) -> ActionUpdate {
        let Some(retry) = self.retries.remove(&id) else {
            return ActionUpdate::default();
        };
        let mut update = pending_update(false);
        update.effects.push(ClientEffect::CancelTimer {
            timer_id: retry.timer_id,
        });
        update.completions.push(completed(
            id,
            retry.pending,
            OperationOutcome::OutcomeUnknown {
                reason: why.unwrap_or(retry.reason),
            },
        ));
        update
    }

    pub fn wake(&mut self, timer_id: TimerId, clock: ClockSample) -> ActionUpdate {
        let id = self
            .retries
            .iter()
            .find(|(_, retry)| retry.timer_id == timer_id && clock.monotonic_ms >= retry.deadline)
            .map(|(id, _)| *id);
        id.map_or_else(ActionUpdate::default, |id| self.expire_retry(id, None))
    }

    /// Called only after the driver's authenticated barrier snapshot. A socket
    /// opening or a Bind alone cannot authorize an identified-message replay.
    pub fn resynchronized(
        &mut self,
        context: &ActionContext<'_>,
        clock: ClockSample,
    ) -> Result<ActionUpdate, ClientError> {
        let mut update = ActionUpdate::default();
        let ids: Vec<_> = self.retries.keys().copied().collect();
        for id in ids {
            let retry = self.retries.get(&id).expect("captured retry");
            if clock.monotonic_ms >= retry.deadline {
                update.append(self.expire_retry(id, None));
                continue;
            }
            let Some(current) = context.connection else {
                continue;
            };
            if context.endpoint != EndpointStatus::Authenticated
                || context.workspace != WorkspaceStatus::Live
            {
                continue;
            }
            let old = &retry.pending.proposal;
            if !same_incarnation(&old.scope, &current.scope)
                || current.grant != old.grant
                || current.mode != old.mode
            {
                update.append(self.expire_retry(
                    id,
                    Some(reason(
                        "retry_authority_changed",
                        "connection authority or daemon changed; message outcome remains unknown",
                    )),
                ));
                continue;
            }
            if current.scope.attempt <= old.scope.attempt {
                continue;
            }
            let mut intent = old.intent.clone();
            replace_scope(&mut intent, current.scope.clone());
            let proposal = match self.prepare(intent, context, clock) {
                Ok(proposal) if proposal.command == old.command => proposal,
                _ => {
                    update.append(self.expire_retry(
                        id,
                        Some(reason(
                            "retry_unavailable",
                            "message can no longer be retried in its original context",
                        )),
                    ));
                    continue;
                }
            };
            let retry = self.retries.remove(&id).expect("checked retry");
            update.append(pending_update(false));
            update.effects.push(ClientEffect::CancelTimer {
                timer_id: retry.timer_id,
            });
            update.effects.push(ClientEffect::Request {
                scope: proposal.scope.clone(),
                operation_id: id,
                lane: RequestLane::Action,
                command: proposal.command.clone().expect("identified send command"),
            });
            self.pending.insert(
                id,
                Pending {
                    proposal,
                    retried: true,
                },
            );
        }
        Ok(update)
    }

    /// Presentation uses these same scope/grant/target checks. Payload-specific
    /// validation still happens at plan, with the complete captured intent.
    /// `target` is an agent ID for presentation; question IDs and attachment
    /// IDs remain explicit in the corresponding intent when dispatched.
    pub fn availability(
        &self,
        kind: ActionKind,
        target: Option<&str>,
        context: &ActionContext<'_>,
        clock: ClockSample,
    ) -> ActionAvailability {
        let result = self.available(kind, target, context, clock);
        let reason = result.err().map(|failure| match failure {
            PlanFailure::Unavailable(reason) => reason,
            PlanFailure::Invalid(error) => reason("invalid_target", error.to_string()),
        });
        ActionAvailability {
            action: kind,
            available: reason.is_none(),
            reason,
        }
    }

    fn available(
        &self,
        kind: ActionKind,
        target: Option<&str>,
        context: &ActionContext<'_>,
        clock: ClockSample,
    ) -> Result<(), PlanFailure> {
        let Some(current) = context.connection else {
            return Err(unavailable("not_connected", "not connected to a workspace"));
        };
        let method = match kind {
            ActionKind::Feature => None,
            ActionKind::SendMessage => Some(CommandKind::Send),
            ActionKind::Answer => Some(CommandKind::Answer),
            ActionKind::Pause => Some(CommandKind::AgentPause),
            ActionKind::Resume => Some(CommandKind::AgentResume),
            ActionKind::Compact => Some(CommandKind::AgentCompact),
            ActionKind::Restart => Some(CommandKind::AgentRestart),
            ActionKind::RestartFresh => Some(CommandKind::AgentRestartFresh),
            ActionKind::Kill => Some(CommandKind::AgentKill),
            ActionKind::Focus => Some(CommandKind::AgentFocus),
            ActionKind::SetModel => Some(CommandKind::AgentSetModel),
            ActionKind::AgentInput => Some(CommandKind::AgentInput),
            ActionKind::ResumeLimited => Some(CommandKind::Resume),
            ActionKind::StopTeam => Some(CommandKind::Shutdown),
            ActionKind::Read => None,
            ActionKind::Attach => Some(CommandKind::Attach),
            ActionKind::AttachInput => Some(CommandKind::AttachInput),
            ActionKind::AttachResize => Some(CommandKind::AttachResize),
            ActionKind::Detach => None,
        };
        connection(&current.scope, context, clock, method)?;
        if self.pending_count() >= self.maximum {
            return Err(unavailable(
                "operation_capacity",
                "too many pending operations",
            ));
        }
        if kind == ActionKind::ResumeLimited
            && !context
                .agents
                .iter()
                .any(|agent| agent.limit.is_some() && !agent.status.is_terminal())
        {
            return Err(unavailable(
                "no_limited_agents",
                "no agent is paused at a usage limit",
            ));
        }
        if kind == ActionKind::Answer
            && !context.questions.iter().any(|question| {
                question.is_operator_question()
                    && context.agents.iter().any(|agent| agent.id == question.from)
                    && !self.pending_answer_ids().any(|id| id == question.id)
                    && target.is_none_or(|target| {
                        question.from == target || question.asked_by() == Some(target)
                    })
            })
        {
            return Err(unavailable(
                "question_missing",
                "no question is awaiting an answer",
            ));
        }
        if kind == ActionKind::SendMessage {
            if let Some(target) = target.filter(|target| *target != BROADCAST) {
                agent(target, context)?;
            }
        }
        if kind == ActionKind::Attach && self.attachments.len() >= self.maximum {
            return Err(unavailable(
                "attachment_capacity",
                "too many terminal attachments",
            ));
        }
        let target_required = matches!(
            kind,
            ActionKind::Pause
                | ActionKind::Resume
                | ActionKind::Compact
                | ActionKind::Restart
                | ActionKind::RestartFresh
                | ActionKind::Kill
                | ActionKind::Focus
                | ActionKind::SetModel
                | ActionKind::AgentInput
                | ActionKind::Attach
        );
        let selected = if target_required {
            Some(agent(
                target.ok_or_else(|| unavailable("agent_required", "name an agent"))?,
                context,
            )?)
        } else {
            None
        };
        if let Some(agent) = selected {
            if matches!(
                kind,
                ActionKind::Pause
                    | ActionKind::Resume
                    | ActionKind::Compact
                    | ActionKind::AgentInput
                    | ActionKind::Attach
            ) {
                running(agent)?;
            }
            if kind == ActionKind::Compact && agent.runtime == RuntimeKind::Shell {
                return Err(unavailable(
                    "compact_unsupported",
                    "this runtime does not support context compaction",
                ));
            }
            if kind == ActionKind::Focus && agent.backend != "herdr" {
                return Err(unavailable(
                    "focus_unsupported",
                    "pane focus requires the herdr backend",
                ));
            }
        }
        if matches!(
            kind,
            ActionKind::AttachInput | ActionKind::AttachResize | ActionKind::Detach
        ) && !self.attachments.values().any(|attachment| {
            attachment.scope == current.scope
                && attachment.status == AttachmentStatus::Live
                && target.is_none_or(|target| attachment.agent == target)
        }) {
            return Err(unavailable(
                "attachment_unavailable",
                "no live terminal attachment",
            ));
        }
        if let Some(method) = method.filter(|_| {
            !matches!(
                kind,
                ActionKind::SendMessage
                    | ActionKind::Answer
                    | ActionKind::AttachInput
                    | ActionKind::AttachResize
            )
        }) {
            let same = |proposal: &Proposal| {
                proposal.scope == current.scope
                    && proposal.command.as_ref().is_some_and(|command| {
                        command.kind() == method
                            // AgentInput availability describes the submitted
                            // prompt control. Raw streams plan full intents.
                            && !matches!(command, Command::AgentInput { submit: false, .. })
                    })
                    && target.is_none_or(|target| proposal.targets.iter().any(|id| id == target))
            };
            if self.pending.values().any(|pending| same(&pending.proposal))
                || self.confirmations.values().any(same)
            {
                return Err(unavailable(
                    "already_pending",
                    "this action is already pending",
                ));
            }
        }
        Ok(())
    }
}

fn completed(id: OperationId, pending: Pending, outcome: OperationOutcome) -> CompletedOperation {
    CompletedOperation {
        operation_id: id,
        scope: pending.proposal.scope,
        command: pending.proposal.command,
        resource: pending.proposal.resource,
        outcome,
    }
}

impl CompletedOperation {
    pub fn notice(&self) -> OperationNotice {
        let (kind, attachment_id, reason) = match &self.outcome {
            OperationOutcome::Succeeded {
                result: CommandResult::Attached { .. },
            } => (OperationOutcomeKind::Success, Some(self.operation_id), None),
            OperationOutcome::Succeeded { .. } => (OperationOutcomeKind::Success, None, None),
            OperationOutcome::Rejected {
                code,
                message,
                details,
            } => (
                OperationOutcomeKind::Rejected,
                None,
                Some(ClientReason {
                    code: code.clone(),
                    message: message.clone(),
                    details: details.clone(),
                }),
            ),
            OperationOutcome::NotSent { reason } => {
                (OperationOutcomeKind::NotSent, None, Some(reason.clone()))
            }
            OperationOutcome::Canceled => (OperationOutcomeKind::Canceled, None, None),
            OperationOutcome::OutcomeUnknown { reason } => {
                (OperationOutcomeKind::Unknown, None, Some(reason.clone()))
            }
            OperationOutcome::ClosedLocally { attachment_id } => (
                OperationOutcomeKind::ClosedLocally,
                Some(*attachment_id),
                None,
            ),
            OperationOutcome::SentUnacknowledged { attachment_id } => (
                OperationOutcomeKind::SentUnacknowledged,
                Some(*attachment_id),
                None,
            ),
        };
        OperationNotice {
            scope: self.scope.clone(),
            operation_id: self.operation_id,
            attachment_id,
            kind,
            reason,
        }
    }
}

fn same_incarnation(left: &ClientScope, right: &ClientScope) -> bool {
    left.endpoint_id == right.endpoint_id
        && left.workspace_id == right.workspace_id
        && left.generation == right.generation
        && left.daemon_pid.is_some()
        && left.daemon_pid == right.daemon_pid
        && left.daemon_started_at_ms.is_some()
        && left.daemon_started_at_ms == right.daemon_started_at_ms
}

fn replace_scope(intent: &mut ActionIntent, replacement: ClientScope) {
    let scope = match intent {
        ActionIntent::Feature { scope, .. } => scope,
        ActionIntent::SendMessage { scope, .. }
        | ActionIntent::Answer { scope, .. }
        | ActionIntent::Agent { scope, .. }
        | ActionIntent::SetModel { scope, .. }
        | ActionIntent::AgentInput { scope, .. }
        | ActionIntent::ResumeLimited { scope, .. }
        | ActionIntent::StopTeam { scope }
        | ActionIntent::Read { scope, .. }
        | ActionIntent::Attach { scope, .. }
        | ActionIntent::AttachInput { scope, .. }
        | ActionIntent::AttachResize { scope, .. }
        | ActionIntent::Detach { scope, .. } => scope,
    };
    *scope = replacement;
}

fn pending_update(questions: bool) -> ActionUpdate {
    ActionUpdate {
        changes: ChangeSet {
            pending: true,
            availability: true,
            questions,
            ..ChangeSet::default()
        },
        ..ActionUpdate::default()
    }
}

fn allocate(next: &mut u64, code: &str) -> Result<u64, ClientError> {
    check_counter(*next, code)?;
    let id = *next;
    *next = next
        .checked_add(1)
        .ok_or_else(|| ClientError::InternalFailure { code: code.into() })?;
    Ok(id)
}

fn check_counter(next: u64, code: &str) -> Result<(), ClientError> {
    if next > CORE_OPERATION_ID_MAX {
        return Err(ClientError::InternalFailure { code: code.into() });
    }
    Ok(())
}

#[cfg(test)]
#[path = "actions/availability_tests.rs"]
mod availability_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_exhaustion_never_crosses_driver_namespace_or_wraps() {
        let mut next = CORE_OPERATION_ID_MAX;
        assert_eq!(
            allocate(&mut next, "operation_id_exhausted").unwrap(),
            CORE_OPERATION_ID_MAX
        );
        assert_eq!(next, DRIVER_OPERATION_ID_START);
        assert_eq!(
            allocate(&mut next, "operation_id_exhausted"),
            Err(ClientError::InternalFailure {
                code: "operation_id_exhausted".into()
            })
        );
        assert_eq!(next, DRIVER_OPERATION_ID_START);
        next = u64::MAX;
        assert!(allocate(&mut next, "operation_id_exhausted").is_err());
        assert_eq!(next, u64::MAX);
    }
}
