//! Authoritative scoped domain state. No transport, UI, clock or credentials.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crew_protocol::rpc::{Push, SessionInfo, Snapshot, Workspaces};
use crew_protocol::{Agent, Task, WorkspaceState, AGENT_ERROR_EVENT, AGENT_REFUSED_EVENT};

use crate::api::*;
use crate::history::{EventHistory, MessageHistory};
use crate::questions::Questions;
use crate::sort_agents;

#[derive(Clone, Debug)]
struct SnapshotFlight {
    operation_id: OperationId,
    queued: Vec<Push>,
}

#[derive(Clone, Debug)]
struct PendingRead {
    key: ResourceKey,
    sequence: u64,
}

#[derive(Debug, Default)]
pub(crate) struct StateUpdate {
    pub changes: ChangeSet,
    pub effects: Vec<ClientEffect>,
    pub resynchronized: bool,
}

/// Borrowed by projections and action planning; it contains daemon records,
/// never a second presentation model.
#[derive(Debug)]
pub(crate) struct ClientState {
    pub config: ClientConfig,
    pub revision: u64,
    pub connection: Option<ConnectionContext>,
    // Identity/authority of the last completed barrier, used only to detect
    // abandonment across provisional reconnects. Planning uses connection.
    pub verified_binding: Option<ConnectionContext>,
    pub scope: Option<ClientScope>,
    pub endpoint: EndpointStatus,
    pub workspace: WorkspaceStatus,
    pub reason: Option<ClientReason>,
    pub listing: Option<Workspaces>,
    endpoint_id: Option<String>,
    endpoint_attempt: u64,
    workspace_attempt: u64,
    binding_endpoint_attempt: u64,
    last_binding: Option<ConnectionContext>,
    seen_workspace: bool,
    pub loaded: bool,
    pub synchronized_at_ms: Option<i64>,
    clock_offset_ms: i128,
    synchronized: bool,
    pub active: bool,
    pub session: Option<SessionInfo>,
    pub agents: Vec<Agent>,
    pub tasks: Vec<Task>,
    pub messages: MessageHistory,
    pub events: EventHistory,
    pub questions: Questions,
    pub usage_samples: BTreeMap<String, VecDeque<ContextPoint>>,
    sample_monotonic_ms: BTreeMap<String, u64>,
    pub resources: HashMap<ResourceKey, ResourceState>,
    resource_lru: VecDeque<ResourceKey>,
    pending_reads: BTreeMap<OperationId, PendingRead>,
    snapshot: Option<SnapshotFlight>,
}

impl ClientState {
    pub(crate) fn new(config: ClientConfig) -> Result<Self, ClientError> {
        crate::policy::validate_limits(config.limits)?;
        Ok(Self {
            config,
            revision: 0,
            connection: None,
            verified_binding: None,
            scope: None,
            endpoint: EndpointStatus::Disconnected,
            workspace: WorkspaceStatus::NoneSelected,
            reason: None,
            listing: None,
            endpoint_id: None,
            endpoint_attempt: 0,
            workspace_attempt: 0,
            binding_endpoint_attempt: 0,
            last_binding: None,
            seen_workspace: false,
            loaded: false,
            synchronized_at_ms: None,
            clock_offset_ms: 0,
            synchronized: false,
            active: true,
            session: None,
            agents: Vec::new(),
            tasks: Vec::new(),
            messages: MessageHistory::new(config.limits.messages as usize),
            events: EventHistory::new(config.limits.events as usize),
            questions: Questions::default(),
            usage_samples: BTreeMap::new(),
            sample_monotonic_ms: BTreeMap::new(),
            resources: HashMap::new(),
            resource_lru: VecDeque::new(),
            pending_reads: BTreeMap::new(),
            snapshot: None,
        })
    }

    pub(crate) fn matches(&self, scope: &ClientScope) -> bool {
        scope.attempt == self.workspace_attempt
            && self.binding_endpoint_attempt == self.endpoint_attempt
            && self.endpoint_id.as_ref() == Some(&scope.endpoint_id)
            && self
                .connection
                .as_ref()
                .is_some_and(|connection| &connection.scope == scope)
    }

    pub(crate) fn stale_bind(&self, scope: &ClientScope) -> bool {
        scope.attempt < self.workspace_attempt || scope.attempt < self.endpoint_attempt
    }

    pub(crate) fn accepts_endpoint_event(&self, id: &str, attempt: u64) -> bool {
        attempt >= self.endpoint_attempt
            && (self.endpoint_id.as_deref().is_none_or(|old| old == id)
                || (self.connection.is_none()
                    && attempt > self.endpoint_attempt
                    && attempt > self.workspace_attempt))
    }

    pub(crate) fn accepts_unbind(&self, scope: &ClientScope, reason: &ClientReason) -> bool {
        self.matches(scope)
            || (self.connection.is_none()
                && !matches!(reason.code.as_str(), "closed" | "canonical_scope")
                && self.scope.as_ref().is_some_and(|old| {
                    old.endpoint_id == scope.endpoint_id
                        && old.attempt == scope.attempt
                        && old.workspace_id == scope.workspace_id
                        && old.generation == scope.generation
                        && old.daemon_pid == scope.daemon_pid
                        && old.daemon_started_at_ms == scope.daemon_started_at_ms
                }))
    }

    pub(crate) fn validate_active_bind(
        &self,
        scope: &ClientScope,
        mode: ConnectionMode,
        grant: &Grant,
        snapshot_mode: SnapshotMode,
    ) -> Result<(), ClientError> {
        validate_bind(scope)?;
        if let Some(old) = &self.connection {
            if old.scope.attempt == scope.attempt
                && (old.scope != *scope
                    || old.mode != mode
                    || old.grant != *grant
                    || old.snapshot_mode != snapshot_mode)
            {
                return Err(ClientError::InvalidInput {
                    field: "scope.attempt".into(),
                    reason: "an active binding change requires a fresh attempt".into(),
                });
            }
        } else if let Some(old) = &self.last_binding {
            if old.scope.attempt == scope.attempt {
                let authority_matches = old.mode == ConnectionMode::LocalOperator
                    && mode == old.mode
                    && old.snapshot_mode == snapshot_mode
                    && old.scope.endpoint_id == scope.endpoint_id
                    && old.grant.grant_id == grant.grant_id
                    && old.grant.tiers == grant.tiers
                    && old.grant.expires_at_ms == grant.expires_at_ms
                    && (old.grant.workspace_ids == grant.workspace_ids
                        || (old.grant.workspace_ids == [old.scope.workspace_id.clone()]
                            && grant.workspace_ids == [scope.workspace_id.clone()]));
                let proof_enriched = old
                    .scope
                    .generation
                    .is_none_or(|v| scope.generation == Some(v))
                    && old
                        .scope
                        .daemon_pid
                        .is_none_or(|v| scope.daemon_pid == Some(v))
                    && old
                        .scope
                        .daemon_started_at_ms
                        .is_none_or(|v| scope.daemon_started_at_ms == Some(v));
                if self
                    .reason
                    .as_ref()
                    .is_none_or(|reason| reason.code != "canonical_scope")
                    || !authority_matches
                    || !proof_enriched
                    || self.loaded
                {
                    return Err(ClientError::InvalidInput {
                        field: "scope.attempt".into(),
                        reason:
                            "a retired attempt can only refine a pre-live local canonical binding"
                                .into(),
                    });
                }
            }
        }
        Ok(())
    }

    fn listing_allows_data(&self) -> bool {
        let Some(connection) = &self.connection else {
            return false;
        };
        if connection.mode == ConnectionMode::LocalOperator {
            return true;
        }
        let Some(listing) = &self.listing else {
            return true;
        };
        listing.joined.iter().any(|entry| {
            entry.id == connection.scope.workspace_id
                && entry.state == WorkspaceState::Running
                && connection
                    .scope
                    .generation
                    .is_none_or(|generation| generation == entry.generation)
                && entry.daemon.is_none_or(|daemon| {
                    connection
                        .scope
                        .daemon_pid
                        .is_none_or(|pid| pid == daemon.pid)
                        && connection
                            .scope
                            .daemon_started_at_ms
                            .is_none_or(|at| at == daemon.started_at)
                })
        })
    }

    pub(crate) fn live(&self) -> bool {
        self.active
            && self.endpoint == EndpointStatus::Authenticated
            && self.workspace == WorkspaceStatus::Live
            && self.synchronized
    }

    pub(crate) fn server_now(&self, clock: ClockSample) -> i64 {
        (i128::from(clock.wall_ms) + self.clock_offset_ms)
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    pub(crate) fn freshness(&self) -> Freshness {
        if self.live() {
            Freshness::Fresh
        } else if self.loaded {
            Freshness::Stale
        } else if self.snapshot.is_some() || self.workspace == WorkspaceStatus::Connecting {
            Freshness::Loading
        } else {
            Freshness::Unavailable
        }
    }

    fn next_revision(&self) -> Result<u64, ClientError> {
        self.revision
            .checked_add(1)
            .ok_or_else(|| ClientError::InternalFailure {
                code: "revision_overflow".into(),
            })
    }

    // Once the revision space is exhausted, mutation is a terminal error.
    // Check before calling an action hook, which also mutates pending state.
    pub(crate) fn ensure_writable(&self) -> Result<(), ClientError> {
        self.next_revision().map(|_| ())
    }

    fn clear_domain(&mut self, revision: u64) -> Result<(), ClientError> {
        self.messages.reset(Vec::new(), revision)?;
        self.events.reset(Vec::new(), revision)?;
        self.session = None;
        self.agents.clear();
        self.tasks.clear();
        self.questions.clear();
        self.usage_samples.clear();
        self.sample_monotonic_ms.clear();
        self.resources.clear();
        self.resource_lru.clear();
        self.pending_reads.clear();
        self.loaded = false;
        self.synchronized = false;
        self.synchronized_at_ms = None;
        self.clock_offset_ms = 0;
        self.snapshot = None;
        Ok(())
    }

    fn mark_disconnected(&mut self, update: &mut StateUpdate) {
        self.synchronized = false;
        self.snapshot = None;
        self.pending_reads.clear();
        for state in self.resources.values_mut() {
            if state.status == ResourceStatus::Loading || state.value.is_some() {
                state.status = ResourceStatus::Stale;
            }
        }
        let retained: Vec<_> = self.resources.keys().cloned().collect();
        for key in retained {
            self.touch_resource(&key);
        }
        self.trim_resources();
        update.changes.availability = true;
        update.changes.pending = true;
    }

    fn evaluate_workspace(&mut self) {
        let Some(scope) = &self.scope else {
            return;
        };
        if self.connection.is_none() && self.workspace == WorkspaceStatus::AccessRevoked {
            return;
        }
        if self
            .last_binding
            .as_ref()
            .is_some_and(|binding| binding.mode == ConnectionMode::ScopedGateway)
        {
            if let Some(listing) = &self.listing {
                let entry = listing
                    .joined
                    .iter()
                    .find(|entry| entry.id == scope.workspace_id);
                match entry {
                    None => {
                        self.workspace = if self.seen_workspace {
                            WorkspaceStatus::Removed
                        } else {
                            WorkspaceStatus::NeverJoined
                        };
                        self.reason = None;
                        return;
                    }
                    Some(entry) => {
                        self.seen_workspace = true;
                        match entry.state {
                            WorkspaceState::Stopped => self.workspace = WorkspaceStatus::Stopped,
                            WorkspaceState::Unreachable => {
                                self.workspace = WorkspaceStatus::Unreachable
                            }
                            WorkspaceState::Incompatible => {
                                self.workspace = WorkspaceStatus::Incompatible
                            }
                            WorkspaceState::Running => {
                                if entry.generation != scope.generation.unwrap_or(entry.generation)
                                    || entry.daemon.is_some_and(|daemon| {
                                        scope.daemon_pid.is_some_and(|pid| pid != daemon.pid)
                                            || scope
                                                .daemon_started_at_ms
                                                .is_some_and(|at| at != daemon.started_at)
                                    })
                                {
                                    self.synchronized = false;
                                }
                                self.workspace = if self.live_channel() {
                                    WorkspaceStatus::Live
                                } else if self.loaded {
                                    WorkspaceStatus::Reconnecting
                                } else {
                                    WorkspaceStatus::Connecting
                                };
                            }
                        }
                        self.reason = (!entry.detail.is_empty()).then(|| ClientReason {
                            code: entry.state.as_str().into(),
                            message: entry.detail.clone(),
                        });
                        return;
                    }
                }
            }
        }
        self.workspace = if self.live_channel() {
            WorkspaceStatus::Live
        } else if self.loaded {
            WorkspaceStatus::Reconnecting
        } else {
            WorkspaceStatus::Connecting
        };
    }

    fn live_channel(&self) -> bool {
        self.connection.is_some()
            && self.active
            && self.endpoint == EndpointStatus::Authenticated
            && self.synchronized
    }

    pub(crate) fn listing_abandons(&self, scope: &ClientScope) -> bool {
        if self
            .last_binding
            .as_ref()
            .is_none_or(|binding| binding.mode != ConnectionMode::ScopedGateway)
        {
            return false;
        }
        let Some(listing) = &self.listing else {
            return false;
        };
        let Some(entry) = listing
            .joined
            .iter()
            .find(|entry| entry.id == scope.workspace_id)
        else {
            return true;
        };
        matches!(
            entry.state,
            WorkspaceState::Stopped | WorkspaceState::Incompatible
        ) || (entry.state == WorkspaceState::Running
            && (scope
                .generation
                .is_some_and(|generation| generation != entry.generation)
                || entry.daemon.is_some_and(|daemon| {
                    scope.daemon_pid.is_some_and(|pid| pid != daemon.pid)
                        || scope
                            .daemon_started_at_ms
                            .is_some_and(|at| at != daemon.started_at)
                })))
    }

    pub(crate) fn apply_data(
        &mut self,
        input: ClientInput,
        clock: ClockSample,
    ) -> Result<StateUpdate, ClientError> {
        let mut update = StateUpdate::default();
        match input {
            ClientInput::Bind {
                scope,
                mode,
                grant,
                snapshot_mode,
            } => {
                if self.stale_bind(&scope) {
                    return Ok(update);
                }
                self.validate_active_bind(&scope, mode, &grant, snapshot_mode)?;
                if self.connection.as_ref().is_some_and(|c| {
                    c.scope == scope
                        && c.mode == mode
                        && c.grant == grant
                        && c.snapshot_mode == snapshot_mode
                }) {
                    return Ok(update);
                }
                let revision = self.next_revision()?;
                let same_workspace = self.scope.as_ref().is_some_and(|old| {
                    old.endpoint_id == scope.endpoint_id && old.workspace_id == scope.workspace_id
                });
                self.clear_domain(revision)?;
                if !same_workspace {
                    self.seen_workspace = false;
                }
                if self.endpoint_id.as_ref() != Some(&scope.endpoint_id) {
                    self.listing = None;
                }
                self.endpoint_id = Some(scope.endpoint_id.clone());
                if mode == ConnectionMode::LocalOperator {
                    self.endpoint_attempt = scope.attempt;
                }
                self.workspace_attempt = scope.attempt;
                self.binding_endpoint_attempt = self.endpoint_attempt;
                self.scope = Some(scope.clone());
                self.endpoint = EndpointStatus::Authenticated;
                self.connection = Some(ConnectionContext {
                    mode,
                    grant,
                    snapshot_mode,
                    scope,
                });
                self.last_binding = self.connection.clone();
                self.reason = None;
                self.evaluate_workspace();
                update.changes.reset = true;
                update.changes.availability = true;
                update.changes.pending = true;
            }
            ClientInput::Unbind { scope, reason } => {
                if !self.accepts_unbind(&scope, &reason)
                    || (self.connection.is_none() && self.reason.as_ref() == Some(&reason))
                {
                    return Ok(update);
                }
                if reason.code == "canonical_scope" && self.loaded {
                    return Err(ClientError::InvalidInput {
                        field: "scope".into(),
                        reason: "canonical refinement must precede the attempt's first snapshot"
                            .into(),
                    });
                }
                self.next_revision()?;
                self.mark_disconnected(&mut update);
                let revoked = matches!(
                    reason.code.as_str(),
                    "revoked" | "access_revoked" | "unauthorized" | "signed_out"
                );
                if revoked {
                    self.clear_domain(self.next_revision()?)?;
                    self.verified_binding = None;
                    self.workspace = WorkspaceStatus::AccessRevoked;
                    self.endpoint = EndpointStatus::SignedOut;
                    update.changes.reset = true;
                } else {
                    self.workspace = match reason.code.as_str() {
                        "shutdown" | "stopped" => WorkspaceStatus::Stopped,
                        "incompatible" | "protocol" => WorkspaceStatus::Incompatible,
                        "removed" => WorkspaceStatus::Removed,
                        _ if self.loaded => WorkspaceStatus::Reconnecting,
                        _ => WorkspaceStatus::Unreachable,
                    };
                }
                self.connection = None;
                self.reason = Some(reason);
            }
            ClientInput::EndpointChanged {
                endpoint_id,
                attempt,
                status,
            } => {
                if !self.accepts_endpoint_event(&endpoint_id, attempt) {
                    return Ok(update);
                }
                if self.endpoint_id.as_ref() == Some(&endpoint_id)
                    && attempt == self.endpoint_attempt
                    && status == self.endpoint
                {
                    return Ok(update);
                }
                self.next_revision()?;
                let changed_endpoint = self
                    .endpoint_id
                    .as_deref()
                    .is_some_and(|old| old != endpoint_id);
                let changed_attempt = attempt > self.endpoint_attempt;
                if changed_endpoint {
                    self.clear_domain(self.next_revision()?)?;
                    self.scope = None;
                    self.verified_binding = None;
                    self.last_binding = None;
                    self.seen_workspace = false;
                    self.workspace = WorkspaceStatus::NoneSelected;
                    self.reason = None;
                    update.changes.reset = true;
                }
                self.endpoint_id = Some(endpoint_id);
                self.endpoint_attempt = attempt;
                self.endpoint = status;
                if changed_attempt {
                    // Listing revisions belong to one endpoint incarnation.
                    // A restarted index can legitimately begin again at one.
                    self.listing = None;
                }
                if status != EndpointStatus::Authenticated || changed_attempt {
                    self.mark_disconnected(&mut update);
                    if status == EndpointStatus::SignedOut {
                        self.clear_domain(self.next_revision()?)?;
                        self.verified_binding = None;
                        self.workspace = WorkspaceStatus::AccessRevoked;
                        self.connection = None;
                        update.changes.reset = true;
                    } else if self.connection.is_some() && self.workspace == WorkspaceStatus::Live {
                        self.workspace = WorkspaceStatus::Reconnecting;
                    }
                } else {
                    self.evaluate_workspace();
                }
                update.changes.availability = true;
            }
            ClientInput::Listing {
                endpoint_id,
                attempt,
                listing,
            } => {
                if self.endpoint_id.as_ref() != Some(&endpoint_id)
                    || attempt != self.endpoint_attempt
                    || self
                        .listing
                        .as_ref()
                        .is_some_and(|old| listing.rev <= old.rev)
                {
                    return Ok(update);
                }
                self.next_revision()?;
                self.listing = Some(listing);
                let previous = self.workspace;
                self.evaluate_workspace();
                if (previous == WorkspaceStatus::Live && self.workspace != WorkspaceStatus::Live)
                    || (self.connection.is_some() && !self.listing_allows_data())
                {
                    self.mark_disconnected(&mut update);
                }
                update.changes.availability = true;
            }
            ClientInput::SnapshotStarted {
                scope,
                operation_id,
            } => {
                if !self.matches(&scope)
                    || !self.active
                    || self.endpoint != EndpointStatus::Authenticated
                    || !self.listing_allows_data()
                {
                    return Ok(update);
                }
                if self
                    .snapshot
                    .as_ref()
                    .is_some_and(|flight| flight.operation_id == operation_id)
                {
                    return Ok(update);
                }
                self.next_revision()?;
                self.mark_disconnected(&mut update);
                self.snapshot = Some(SnapshotFlight {
                    operation_id,
                    queued: Vec::new(),
                });
                self.evaluate_workspace();
                update.changes.availability = true;
            }
            ClientInput::Snapshot {
                scope,
                operation_id,
                snapshot,
            } => {
                if !self.matches(&scope)
                    || !self.listing_allows_data()
                    || self
                        .snapshot
                        .as_ref()
                        .is_none_or(|flight| flight.operation_id != operation_id)
                {
                    return Ok(update);
                }
                validate_snapshot(&snapshot)?;
                let revision = self.next_revision()?;
                let mut messages = self.messages.clone();
                messages.reset(snapshot.messages, revision)?;
                let mut events = self.events.clone();
                events.reset(snapshot.events, revision)?;
                let queued = self.snapshot.take().expect("matching snapshot").queued;
                self.messages = messages;
                self.events = events;
                self.clock_offset_ms = i128::from(snapshot.server_time) - i128::from(clock.wall_ms);
                self.session = Some(snapshot.session);
                self.agents = snapshot.agents;
                self.tasks = snapshot.tasks;
                sort_agents(&mut self.agents);
                self.questions.replace(snapshot.questions);
                self.loaded = true;
                self.synchronized = true;
                self.synchronized_at_ms = Some(clock.wall_ms);
                let present: std::collections::HashSet<_> =
                    self.agents.iter().map(|a| a.id.clone()).collect();
                self.usage_samples.retain(|id, _| present.contains(id));
                self.sample_monotonic_ms
                    .retain(|id, _| present.contains(id));
                for agent in &self.agents.clone() {
                    self.sample(agent, clock);
                }
                self.reason = None;
                self.evaluate_workspace();
                update.changes.reset = true;
                update.changes.session = true;
                update.changes.questions = true;
                update.changes.messages = true;
                update.changes.events = true;
                update.changes.usage = true;
                update.changes.availability = true;
                update.changes.agents = self.agents.iter().map(|a| a.id.clone()).collect();
                update.changes.tasks = self.tasks.iter().map(|t| t.id.clone()).collect();
                for push in queued {
                    self.apply_push(push, clock, &mut update)?;
                }
                update.resynchronized = self.live();
                if update.resynchronized {
                    self.verified_binding = self.connection.clone();
                }
            }
            ClientInput::Push { scope, push } => {
                if !self.matches(&scope) || !self.listing_allows_data() {
                    return Ok(update);
                }
                validate_push(&push)?;
                if matches!(push, Push::Shutdown) {
                    self.apply_push(push, clock, &mut update)?;
                    return Ok(update);
                }
                if let Some(flight) = &mut self.snapshot {
                    match self
                        .connection
                        .as_ref()
                        .expect("matching scope")
                        .snapshot_mode
                    {
                        SnapshotMode::OrderedConnection => return Ok(update),
                        SnapshotMode::BufferedOverlap => {
                            if flight.queued.len() >= self.config.limits.buffered_pushes as usize {
                                self.next_revision()?;
                                self.snapshot = None;
                                self.synchronized = false;
                                self.evaluate_workspace();
                                update.changes.availability = true;
                                update.effects.push(ClientEffect::Resynchronize { scope });
                            } else {
                                flight.queued.push(push);
                            }
                            return Ok(update);
                        }
                    }
                }
                if !self.synchronized {
                    return Ok(update);
                }
                self.apply_push(push, clock, &mut update)?;
            }
            ClientInput::Lifecycle { active } => {
                if self.active != active {
                    self.next_revision()?;
                    self.active = active;
                    if !active {
                        self.mark_disconnected(&mut update);
                    } else if let Some(scope) = &self.scope {
                        update.effects.push(ClientEffect::Resynchronize {
                            scope: scope.clone(),
                        });
                    }
                    self.evaluate_workspace();
                    update.changes.availability = true;
                }
            }
            ClientInput::OperationFinished { .. }
            | ClientInput::AttachmentEnded { .. }
            | ClientInput::Wake { .. } => {
                return Err(ClientError::InternalFailure {
                    code: "action_input_requires_orchestrator".into(),
                });
            }
        }
        Ok(update)
    }

    fn sample_due(&self, agent: &Agent, clock: ClockSample) -> bool {
        let Some(context) = &agent.context else {
            return false;
        };
        let last = self
            .usage_samples
            .get(&agent.id)
            .and_then(|samples| samples.back());
        let previous = self.sample_monotonic_ms.get(&agent.id).copied();
        !(last.is_some_and(|point| point.percent == context.percent)
            && previous.is_some_and(|at| clock.monotonic_ms.saturating_sub(at) < 1000))
    }

    fn sample(&mut self, agent: &Agent, clock: ClockSample) -> bool {
        if !self.sample_due(agent, clock) {
            return false;
        }
        let context = agent.context.as_ref().expect("context sample is present");
        let samples = self.usage_samples.entry(agent.id.clone()).or_default();
        samples.push_back(ContextPoint {
            at_ms: clock.wall_ms,
            percent: context.percent,
        });
        if samples.len() > self.config.limits.context_points as usize {
            samples.pop_front();
        }
        self.sample_monotonic_ms
            .insert(agent.id.clone(), clock.monotonic_ms);
        true
    }

    fn apply_push(
        &mut self,
        push: Push,
        clock: ClockSample,
        update: &mut StateUpdate,
    ) -> Result<(), ClientError> {
        match push {
            Push::Agent(agent) => {
                let agent = *agent;
                let index = self.agents.iter().position(|old| old.id == agent.id);
                if index.is_some_and(|index| self.agents[index] == agent) {
                    // The existing clients retain periodic context samples even
                    // when the authoritative agent record has not changed.
                    if self.sample_due(&agent, clock) {
                        self.next_revision()?;
                        update.changes.usage |= self.sample(&agent, clock);
                    }
                    return Ok(());
                }
                self.next_revision()?;
                let id = agent.id.clone();
                if index.is_some_and(|index| !same_agent_incarnation(&self.agents[index], &agent)) {
                    self.invalidate_agent_resources(&id);
                    update.changes.pending = true;
                }
                let sampled = self.sample(&agent, clock);
                match index {
                    Some(index) => self.agents[index] = agent,
                    None => self.agents.push(agent),
                }
                sort_agents(&mut self.agents);
                update.changes.agents.push(id);
                update.changes.usage |= sampled;
            }
            Push::AgentRemoved { id } => {
                if !self.agents.iter().any(|agent| agent.id == id) {
                    return Ok(());
                }
                self.next_revision()?;
                self.agents.retain(|agent| agent.id != id);
                sort_agents(&mut self.agents);
                self.usage_samples.remove(&id);
                self.sample_monotonic_ms.remove(&id);
                if let Some(scope) = &self.scope {
                    update.changes.removed.push(EntityKey {
                        scope: scope.clone(),
                        entity: Entity::Agent(id.clone()),
                    });
                }
                self.invalidate_agent_resources(&id);
                update.changes.usage = true;
                update.changes.pending = true;
            }
            Push::Task(task) => {
                let task = *task;
                let index = self.tasks.iter().position(|old| old.id == task.id);
                if index.is_some_and(|index| self.tasks[index] == task) {
                    return Ok(());
                }
                self.next_revision()?;
                update.changes.tasks.push(task.id.clone());
                match index {
                    Some(index) => self.tasks[index] = task,
                    None => self.tasks.push(task),
                }
            }
            Push::Message(message) => {
                if self
                    .messages
                    .items()
                    .rev()
                    .take(64)
                    .any(|old| old.id == message.id)
                {
                    return Ok(());
                }
                self.next_revision()?;
                self.messages.append(message.clone())?;
                update.changes.questions |= self.questions.on_message(&message);
                update.changes.messages = true;
            }
            Push::Event(event) => {
                validate_event(&event)?;
                if self
                    .events
                    .items()
                    .next_back()
                    .is_some_and(|last| event.seq != 0 && event.seq <= last.seq)
                {
                    return Ok(());
                }
                self.next_revision()?;
                if self.events.append(event.clone())? {
                    update.changes.events = true;
                    if matches!(
                        event.event_type.as_str(),
                        AGENT_ERROR_EVENT | AGENT_REFUSED_EVENT
                    ) {
                        let entity =
                            self.scope
                                .as_ref()
                                .zip(event.agent_id.as_ref())
                                .map(|(scope, id)| EntityKey {
                                    scope: scope.clone(),
                                    entity: Entity::Agent(id.clone()),
                                });
                        update.effects.push(ClientEffect::Notice {
                            code: event.event_type,
                            entity,
                        });
                    }
                }
            }
            Push::Session(session) => {
                if self.session.as_ref() != Some(session.as_ref()) {
                    self.next_revision()?;
                    self.session = Some(*session);
                    update.changes.session = true;
                }
            }
            Push::Resync => {
                self.next_revision()?;
                self.synchronized = false;
                self.snapshot = None;
                self.evaluate_workspace();
                update.changes.availability = true;
                if let Some(scope) = &self.scope {
                    update.effects.push(ClientEffect::Resynchronize {
                        scope: scope.clone(),
                    });
                }
            }
            Push::Shutdown => {
                self.next_revision()?;
                self.mark_disconnected(update);
                self.workspace = WorkspaceStatus::Stopped;
            }
            Push::Unknown
            // This client does not negotiate feature streams until P4 lands.
            | Push::EpicBoardChanged(_)
            | Push::Aside(_)
            | Push::DocsChanged(_)
            | Push::UsageChanged(_)
            | Push::Deliver { .. }
            | Push::Compact { .. }
            | Push::Pty { .. }
            | Push::PtyClosed
            | Push::Summary(_)
            | Push::Workspaces(_) => {}
        }
        Ok(())
    }

    fn invalidate_agent_resources(&mut self, id: &str) {
        let matches =
            |key: &ResourceKey| matches!(&key.entity.entity, Entity::Agent(agent) if agent == id);
        self.resources.retain(|key, _| !matches(key));
        self.resource_lru.retain(|key| !matches(key));
        self.pending_reads
            .retain(|_, pending| !matches(&pending.key));
    }

    pub(crate) fn begin_read(
        &mut self,
        operation_id: OperationId,
        key: ResourceKey,
    ) -> Result<(), ClientError> {
        if !self.matches(&key.entity.scope) {
            return Ok(());
        }
        if self.pending_reads.len() >= self.config.limits.pending_operations as usize {
            return Err(ClientError::InvalidInput {
                field: "pending_operations".into(),
                reason: "pending read limit reached".into(),
            });
        }
        let sequence = self
            .resources
            .get(&key)
            .map_or(Some(1), |old| old.request_sequence.checked_add(1))
            .ok_or_else(|| ClientError::InternalFailure {
                code: "resource_sequence_overflow".into(),
            })?;
        let value = self.resources.get(&key).and_then(|old| old.value.clone());
        self.resources.insert(
            key.clone(),
            ResourceState {
                key: key.clone(),
                status: ResourceStatus::Loading,
                request_sequence: sequence,
                error: None,
                value,
            },
        );
        self.resource_lru.retain(|old| old != &key);
        self.pending_reads
            .insert(operation_id, PendingRead { key, sequence });
        Ok(())
    }

    pub(crate) fn complete_read(
        &mut self,
        operation_id: OperationId,
        outcome: &OperationOutcome,
    ) -> Result<bool, ClientError> {
        let Some(pending) = self.pending_reads.remove(&operation_id) else {
            return Ok(false);
        };
        if !self.matches(&pending.key.entity.scope) {
            return Ok(false);
        }
        let Some(resource) = self.resources.get(&pending.key) else {
            return Ok(false);
        };
        if resource.request_sequence != pending.sequence {
            return Ok(false);
        }
        let (status, value, error) = match outcome {
            OperationOutcome::Succeeded { result } => match resource_value(&pending.key, result) {
                Some(value) => {
                    let status = resource_status(&value);
                    (status, Some(value), None)
                }
                None => (
                    ResourceStatus::Failed,
                    None,
                    Some(ClientReason {
                        code: "decode".into(),
                        message: "reply does not match requested resource".into(),
                    }),
                ),
            },
            OperationOutcome::Rejected { code, message } => (
                if code == "unsupported" {
                    ResourceStatus::Unsupported
                } else {
                    ResourceStatus::Failed
                },
                None,
                Some(ClientReason {
                    code: code.clone(),
                    message: message.clone(),
                }),
            ),
            OperationOutcome::NotSent { reason } | OperationOutcome::OutcomeUnknown { reason } => {
                (ResourceStatus::Failed, None, Some(reason.clone()))
            }
            OperationOutcome::Canceled => (ResourceStatus::Stale, None, None),
            OperationOutcome::ClosedLocally { .. }
            | OperationOutcome::SentUnacknowledged { .. } => (
                ResourceStatus::Failed,
                None,
                Some(ClientReason {
                    code: "invalid_read_outcome".into(),
                    message: "attachment closure is not a read result".into(),
                }),
            ),
        };
        if let Some(value) = &value {
            match value {
                ResourceValue::History(older) => {
                    self.messages.prepend(older.clone())?;
                    if let (Entity::Workspace, ReadOptions::Bounded { limit }) =
                        (&pending.key.entity.entity, &pending.key.options)
                    {
                        self.messages.refine_remote_older(older, *limit);
                    }
                }
                ResourceValue::Events(older) => {
                    self.events.prepend(older.clone())?;
                    if let (Entity::Workspace, ReadOptions::Bounded { limit }) =
                        (&pending.key.entity.entity, &pending.key.options)
                    {
                        // Legacy zero sequences lack a server identity; a
                        // matching retained value does not prove that all
                        // identical arrivals fit in the local ring.
                        if older.iter().all(|event| event.seq != 0) {
                            self.events.refine_remote_older(older, *limit);
                        }
                    }
                }
                _ => {}
            }
        }
        self.resources.insert(
            pending.key.clone(),
            ResourceState {
                key: pending.key.clone(),
                status,
                request_sequence: pending.sequence,
                error,
                value,
            },
        );
        self.touch_resource(&pending.key);
        self.trim_resources();
        Ok(true)
    }

    fn touch_resource(&mut self, key: &ResourceKey) {
        self.resource_lru.retain(|old| old != key);
        if self
            .resources
            .get(key)
            .is_some_and(|state| state.status != ResourceStatus::Loading)
        {
            self.resource_lru.push_back(key.clone());
        }
    }

    fn trim_resources(&mut self) {
        while self.resource_lru.len() > self.config.limits.pending_operations as usize {
            let Some(key) = self.resource_lru.pop_front() else {
                break;
            };
            if self
                .resources
                .get(&key)
                .is_some_and(|state| state.status == ResourceStatus::Loading)
            {
                continue;
            }
            self.resources.remove(&key);
            // Old captured sequences must not alias a new cache entry after eviction.
            self.pending_reads.retain(|_, pending| pending.key != key);
        }
    }

    pub(crate) fn current_resource(&self, key: &ResourceKey) -> ResourceState {
        self.resources
            .get(key)
            .cloned()
            .unwrap_or_else(|| ResourceState {
                key: key.clone(),
                status: ResourceStatus::Idle,
                request_sequence: 0,
                error: None,
                value: None,
            })
    }

    pub(crate) fn commit(&mut self, changes: &mut ChangeSet) -> Result<u64, ClientError> {
        if changes_visible(changes) {
            self.revision = self.next_revision()?;
        }
        changes.revision = self.revision;
        changes.agents.sort();
        changes.agents.dedup();
        changes.tasks.sort();
        changes.tasks.dedup();
        Ok(self.revision)
    }
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<(), ClientError> {
    let mut ids = std::collections::HashSet::new();
    if snapshot.agents.iter().any(|agent| !ids.insert(&agent.id)) {
        return Err(ClientError::InvalidInput {
            field: "snapshot.agents".into(),
            reason: "duplicate agent identity".into(),
        });
    }
    ids.clear();
    if snapshot.tasks.iter().any(|task| !ids.insert(&task.id)) {
        return Err(ClientError::InvalidInput {
            field: "snapshot.tasks".into(),
            reason: "duplicate task identity".into(),
        });
    }
    for event in &snapshot.events {
        validate_event(event)?;
    }
    Ok(())
}

fn same_agent_incarnation(old: &Agent, new: &Agent) -> bool {
    old.runtime == new.runtime
        && old.pid == new.pid
        && old.session_id == new.session_id
        && old.started_at == new.started_at
        && old.backend == new.backend
        && old.backend_ref == new.backend_ref
        && old.workdir == new.workdir
}

pub(crate) fn validate_bind(scope: &ClientScope) -> Result<(), ClientError> {
    if scope.endpoint_id.is_empty() || scope.workspace_id.is_empty() {
        return Err(ClientError::InvalidInput {
            field: "scope".into(),
            reason: "endpoint and workspace must be named".into(),
        });
    }
    Ok(())
}

fn validate_push(push: &Push) -> Result<(), ClientError> {
    if let Push::Event(event) = push {
        validate_event(event)?;
    }
    Ok(())
}

fn validate_event(event: &crew_protocol::Event) -> Result<(), ClientError> {
    if event.seq < 0 {
        return Err(ClientError::InvalidInput {
            field: "event.seq".into(),
            reason: "must be nonnegative".into(),
        });
    }
    Ok(())
}

fn resource_value(key: &ResourceKey, result: &CommandResult) -> Option<ResourceValue> {
    match (key.kind, result) {
        (ResourceKind::TaskCard, CommandResult::TaskCard(card)) if matches!(&key.entity.entity, Entity::Task(id) if id == &card.task.id) => {
            Some(ResourceValue::TaskCard(*card.clone()))
        }
        (ResourceKind::Message, CommandResult::Message(message)) if matches!(&key.entity.entity, Entity::Message(id) if id == &message.id) => {
            Some(ResourceValue::Message(Some(*message.clone())))
        }
        (ResourceKind::Transcript, CommandResult::Transcript(value)) => {
            Some(ResourceValue::Transcript(value.clone()))
        }
        (ResourceKind::Diff, CommandResult::Diff(value)) => {
            Some(ResourceValue::Diff(*value.clone()))
        }
        (ResourceKind::Output, CommandResult::Output(value)) => {
            Some(ResourceValue::Output(value.clone()))
        }
        (ResourceKind::Inbox, CommandResult::Inbox(value)) => {
            Some(ResourceValue::Inbox(value.clone()))
        }
        (ResourceKind::Models, CommandResult::Models(value)) => {
            Some(ResourceValue::Models(value.clone()))
        }
        (ResourceKind::History, CommandResult::Messages(value)) => {
            Some(ResourceValue::History(value.clone()))
        }
        (ResourceKind::Events, CommandResult::Events(value))
            if value.iter().all(|event| event.seq >= 0) =>
        {
            Some(ResourceValue::Events(value.clone()))
        }
        _ => None,
    }
}

fn resource_status(value: &ResourceValue) -> ResourceStatus {
    let empty = match value {
        ResourceValue::Message(value) => value.is_none(),
        ResourceValue::Event(value) => value.is_none(),
        ResourceValue::Transcript(value) => {
            if !value.available {
                return ResourceStatus::Unsupported;
            }
            value.entries.is_empty()
        }
        ResourceValue::Output(value) => value.text.is_empty(),
        ResourceValue::Inbox(value) => value.messages.is_empty(),
        ResourceValue::Models(value) => value.models.is_empty(),
        ResourceValue::History(value) => value.is_empty(),
        ResourceValue::Events(value) => value.is_empty(),
        ResourceValue::Diff(value) => {
            if !value.ok {
                return ResourceStatus::Failed;
            }
            value.text.is_empty()
        }
        ResourceValue::TaskCard(_) => false,
    };
    if empty {
        ResourceStatus::Empty
    } else {
        ResourceStatus::Ready
    }
}

pub(crate) fn changes_visible(changes: &ChangeSet) -> bool {
    changes.reset
        || changes.session
        || changes.questions
        || changes.messages
        || changes.events
        || changes.usage
        || changes.availability
        || changes.pending
        || !changes.agents.is_empty()
        || !changes.tasks.is_empty()
        || !changes.removed.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_protocol::rpc::ServerFrame;
    use serde_json::{json, Value};

    fn config(mode: SnapshotMode) -> ClientConfig {
        ClientConfig {
            limits: ClientLimits {
                messages: 10,
                events: 10,
                context_points: 3,
                buffered_pushes: 2,
                pending_operations: 4,
                page_size: 10,
            },
            snapshot_mode: mode,
        }
    }
    fn scope() -> ClientScope {
        ClientScope {
            endpoint_id: "local:e".into(),
            workspace_id: "w".into(),
            generation: None,
            daemon_pid: Some(1),
            daemon_started_at_ms: Some(10),
            attempt: 1,
        }
    }
    fn clock(at: i64) -> ClockSample {
        ClockSample {
            wall_ms: at,
            monotonic_ms: at.max(0) as u64,
        }
    }
    fn snapshot() -> Snapshot {
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/snapshot.json")).unwrap()
    }
    fn apply(state: &mut ClientState, input: ClientInput, at: i64) -> StateUpdate {
        let mut update = state.apply_data(input, clock(at)).unwrap();
        state.commit(&mut update.changes).unwrap();
        update
    }
    fn bind(state: &mut ClientState, scope: ClientScope, mode: SnapshotMode) {
        apply(
            state,
            ClientInput::Bind {
                scope: scope.clone(),
                mode: ConnectionMode::LocalOperator,
                grant: Grant {
                    grant_id: "g".into(),
                    tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
                    workspace_ids: vec![scope.workspace_id.clone()],
                    expires_at_ms: None,
                },
                snapshot_mode: mode,
            },
            0,
        );
    }
    fn begin(state: &mut ClientState, scope: ClientScope, id: u64) {
        apply(
            state,
            ClientInput::SnapshotStarted {
                scope,
                operation_id: id,
            },
            0,
        );
    }
    fn load(state: &mut ClientState, scope: ClientScope, id: u64, snapshot: Snapshot, at: i64) {
        apply(
            state,
            ClientInput::Snapshot {
                scope,
                operation_id: id,
                snapshot: Box::new(snapshot),
            },
            at,
        );
    }

    #[test]
    fn both_snapshot_modes_preserve_their_actual_ordering_guarantee() {
        for mode in [
            SnapshotMode::OrderedConnection,
            SnapshotMode::BufferedOverlap,
        ] {
            let mut state = ClientState::new(config(mode)).unwrap();
            bind(&mut state, scope(), mode);
            begin(&mut state, scope(), 100);
            let snap = snapshot();
            let mut pushed = snap.tasks[0].clone();
            pushed.title = "queued push".into();
            apply(
                &mut state,
                ClientInput::Push {
                    scope: scope(),
                    push: Push::Task(Box::new(pushed.clone())),
                },
                1,
            );
            assert!(state.tasks.is_empty());
            load(&mut state, scope(), 100, snap.clone(), 2);
            assert_eq!(
                state.tasks[0].title,
                if mode == SnapshotMode::BufferedOverlap {
                    "queued push"
                } else {
                    snap.tasks[0].title.as_str()
                }
            );
            assert!(state.live());
        }
    }

    #[test]
    fn stale_snapshots_pushes_attempts_and_workspace_scopes_do_not_mutate() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        let old = scope();
        let mut new = old.clone();
        new.attempt = 2;
        bind(&mut state, new.clone(), mode);
        begin(&mut state, new.clone(), 2);
        load(&mut state, new.clone(), 2, snapshot(), 1);
        let revision = state.revision;
        let mut wrong = new.clone();
        wrong.workspace_id = "other".into();
        for stale in [old.clone(), wrong] {
            assert!(!changes_visible(
                &apply(
                    &mut state,
                    ClientInput::Push {
                        scope: stale.clone(),
                        push: Push::AgentRemoved {
                            id: "claude-1".into()
                        }
                    },
                    2
                )
                .changes
            ));
            assert!(!changes_visible(
                &apply(
                    &mut state,
                    ClientInput::Snapshot {
                        scope: stale,
                        operation_id: 1,
                        snapshot: Box::new(Snapshot::default())
                    },
                    2
                )
                .changes
            ));
        }
        bind(&mut state, old, mode);
        assert_eq!(state.revision, revision);
        assert_eq!(state.scope, Some(new));
    }

    #[test]
    fn buffered_overflow_requests_resynchronization_and_rejects_its_old_snapshot() {
        let mode = SnapshotMode::BufferedOverlap;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 7);
        let task = snapshot().tasks.remove(0);
        for _ in 0..2 {
            assert!(apply(
                &mut state,
                ClientInput::Push {
                    scope: scope(),
                    push: Push::Task(Box::new(task.clone()))
                },
                1
            )
            .effects
            .is_empty());
        }
        let overflow = apply(
            &mut state,
            ClientInput::Push {
                scope: scope(),
                push: Push::Task(Box::new(task)),
            },
            1,
        );
        assert_eq!(
            overflow.effects,
            [ClientEffect::Resynchronize { scope: scope() }]
        );
        load(&mut state, scope(), 7, snapshot(), 2);
        assert!(!state.loaded);
        assert!(!state.live());
        begin(&mut state, scope(), 8);
        load(&mut state, scope(), 8, snapshot(), 3);
        assert!(state.live());
    }

    #[test]
    fn revoke_purges_evidence_questions_and_domain_but_plain_disconnect_retains_stale_data() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        apply(
            &mut state,
            ClientInput::Unbind {
                scope: scope(),
                reason: ClientReason {
                    code: "closed".into(),
                    message: "connection closed".into(),
                },
            },
            1,
        );
        assert!(state.loaded);
        assert_eq!(state.freshness(), Freshness::Stale);
        assert!(!state.agents.is_empty());
        let mut new = scope();
        new.attempt = 2;
        bind(&mut state, new.clone(), mode);
        begin(&mut state, new.clone(), 2);
        load(&mut state, new.clone(), 2, snapshot(), 2);
        apply(
            &mut state,
            ClientInput::Unbind {
                scope: new,
                reason: ClientReason {
                    code: "revoked".into(),
                    message: "access revoked".into(),
                },
            },
            3,
        );
        assert_eq!(state.workspace, WorkspaceStatus::AccessRevoked);
        assert!(state.agents.is_empty());
        assert!(state.questions.open.is_empty());
        assert!(state.resources.is_empty());
        assert!(!state.loaded);
    }

    #[test]
    fn resource_sequence_and_scope_suppress_old_completions() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        let key = ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Agent("claude-1".into()),
            },
            kind: ResourceKind::Output,
            options: ReadOptions::Output {
                lines: 300,
                screen: false,
                ansi: false,
            },
        };
        state.begin_read(10, key.clone()).unwrap();
        state.begin_read(11, key.clone()).unwrap();
        let out = |text: &str| OperationOutcome::Succeeded {
            result: CommandResult::Output(crew_protocol::rpc::OutputResult {
                text: text.into(),
                alive: true,
            }),
        };
        assert!(!state.complete_read(10, &out("old")).unwrap());
        assert!(state.complete_read(11, &out("new")).unwrap());
        assert_eq!(
            state.current_resource(&key).value,
            Some(ResourceValue::Output(crew_protocol::rpc::OutputResult {
                text: "new".into(),
                alive: true
            }))
        );
        state.begin_read(12, key.clone()).unwrap();
        let mut new = scope();
        new.workspace_id = "different".into();
        new.attempt = 2;
        bind(&mut state, new, mode);
        assert!(!state.complete_read(12, &out("wrong workspace")).unwrap());
        assert!(state.resources.is_empty());
    }

    #[test]
    fn resource_lru_is_bounded_without_evicting_loading_or_aliasing_old_sequences() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        let key = |lines| ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Agent("claude-1".into()),
            },
            kind: ResourceKind::Output,
            options: ReadOptions::Output {
                lines,
                screen: false,
                ansi: false,
            },
        };
        let outcome = OperationOutcome::Succeeded {
            result: CommandResult::Output(crew_protocol::rpc::OutputResult {
                text: "read".into(),
                alive: true,
            }),
        };
        state.begin_read(100, key(1)).unwrap();
        for id in 2..=6 {
            state.begin_read(id, key(id as u32)).unwrap();
            state.complete_read(id, &outcome).unwrap();
        }
        assert_eq!(state.resources.len(), 5);
        assert_eq!(
            state.current_resource(&key(1)).status,
            ResourceStatus::Loading
        );
        assert_eq!(state.current_resource(&key(2)).status, ResourceStatus::Idle);
        state.complete_read(100, &outcome).unwrap();
        assert_eq!(state.resources.len(), 4);
        state.begin_read(200, key(10)).unwrap();
        state.begin_read(201, key(10)).unwrap();
        state.complete_read(201, &outcome).unwrap();
        for id in 11..=15 {
            state.begin_read(id, key(id as u32)).unwrap();
            state.complete_read(id, &outcome).unwrap();
        }
        state.begin_read(300, key(10)).unwrap();
        assert!(
            !state.complete_read(200, &outcome).unwrap(),
            "eviction removed the old capture before sequence reuse"
        );
        assert_eq!(
            state.current_resource(&key(10)).status,
            ResourceStatus::Loading
        );
    }

    #[test]
    fn a_typed_reply_with_the_wrong_entity_or_invalid_events_does_not_populate_evidence() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        let key = ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Task("requested-task".into()),
            },
            kind: ResourceKind::TaskCard,
            options: ReadOptions::Default,
        };
        state.begin_read(9, key.clone()).unwrap();
        let wrong = OperationOutcome::Succeeded {
            result: CommandResult::TaskCard(Box::new(crew_protocol::rpc::TaskCard {
                task: snapshot().tasks.remove(0),
                dependencies: vec![],
            })),
        };
        assert!(state.complete_read(9, &wrong).unwrap());
        assert_eq!(state.current_resource(&key).status, ResourceStatus::Failed);
        assert!(state.current_resource(&key).value.is_none());
        let key = ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Workspace,
            },
            kind: ResourceKind::Events,
            options: ReadOptions::Bounded { limit: 200 },
        };
        state.begin_read(10, key.clone()).unwrap();
        let before = state.events.items().cloned().collect::<Vec<_>>();
        let mut invalid = before[0].clone();
        invalid.seq = -1;
        assert!(state
            .complete_read(
                10,
                &OperationOutcome::Succeeded {
                    result: CommandResult::Events(vec![invalid])
                }
            )
            .unwrap());
        assert_eq!(state.current_resource(&key).status, ResourceStatus::Failed);
        assert_eq!(state.events.items().cloned().collect::<Vec<_>>(), before);
    }

    #[test]
    fn filtered_history_reads_cannot_claim_workspace_wide_remote_exhaustion() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        let mut key = ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Agent("claude-1".into()),
            },
            kind: ResourceKind::History,
            options: ReadOptions::Bounded { limit: 300 },
        };
        let empty = OperationOutcome::Succeeded {
            result: CommandResult::Messages(vec![]),
        };
        state.begin_read(1, key.clone()).unwrap();
        state.complete_read(1, &empty).unwrap();
        assert!(state.messages.may_have_older_remote);
        key.entity.entity = Entity::Workspace;
        state.begin_read(2, key).unwrap();
        state.complete_read(2, &empty).unwrap();
        assert!(!state.messages.may_have_older_remote);
    }

    #[test]
    fn malformed_snapshot_is_atomic_and_shutdown_is_not_hidden_by_a_barrier() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        let original = state.tasks.clone();
        let revision = state.revision;
        begin(&mut state, scope(), 2);
        let mut bad = snapshot();
        bad.tasks.push(bad.tasks[0].clone());
        assert!(state
            .apply_data(
                ClientInput::Snapshot {
                    scope: scope(),
                    operation_id: 2,
                    snapshot: Box::new(bad)
                },
                clock(1)
            )
            .is_err());
        assert_eq!(state.tasks, original);
        assert_eq!(state.revision, revision + 1);
        apply(
            &mut state,
            ClientInput::Push {
                scope: scope(),
                push: Push::Shutdown,
            },
            2,
        );
        assert_eq!(state.workspace, WorkspaceStatus::Stopped);
        assert!(!state.live());
    }

    #[test]
    fn invalid_limits_and_revision_overflow_fail_explicitly() {
        let mut invalid = config(SnapshotMode::OrderedConnection);
        invalid.limits.messages = 0;
        assert!(ClientState::new(invalid).is_err());
        let mut state = ClientState::new(config(SnapshotMode::OrderedConnection)).unwrap();
        state.revision = u64::MAX;
        assert_eq!(
            state
                .apply_data(ClientInput::Lifecycle { active: false }, clock(0))
                .unwrap_err(),
            ClientError::InternalFailure {
                code: "revision_overflow".into()
            }
        );
        assert!(state.active);
    }

    #[test]
    fn a_new_endpoint_attempt_cannot_be_revived_by_the_old_bound_snapshot() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        apply(
            &mut state,
            ClientInput::EndpointChanged {
                endpoint_id: scope().endpoint_id,
                attempt: 2,
                status: EndpointStatus::Authenticated,
            },
            1,
        );
        let revision = state.revision;
        begin(&mut state, scope(), 2);
        load(&mut state, scope(), 2, Snapshot::default(), 2);
        assert_eq!(state.revision, revision);
        assert!(!state.live());
        assert!(!state.agents.is_empty());
        let mut fresh = scope();
        fresh.attempt = 2;
        bind(&mut state, fresh.clone(), mode);
        begin(&mut state, fresh.clone(), 3);
        load(&mut state, fresh, 3, snapshot(), 3);
        assert!(state.live());
    }

    #[test]
    fn a_gateway_listing_rejects_old_incarnations_before_and_after_binding() {
        use crew_protocol::rpc::{DaemonRef, WorkspaceEntry};
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        let mut remote = scope();
        remote.endpoint_id = "gateway:e".into();
        remote.generation = Some(1);
        apply(
            &mut state,
            ClientInput::EndpointChanged {
                endpoint_id: remote.endpoint_id.clone(),
                attempt: 1,
                status: EndpointStatus::Authenticated,
            },
            0,
        );
        let entry = WorkspaceEntry {
            id: remote.workspace_id.clone(),
            generation: 1,
            daemon: Some(DaemonRef {
                pid: 1,
                started_at: 10,
            }),
            project: None,
            workspace: "/work/w".into(),
            state: WorkspaceState::Running,
            detail: String::new(),
            summary: None,
            joined_at: 0,
        };
        let listing = Workspaces {
            rev: 1,
            joined: vec![entry.clone()],
            others: vec![],
        };
        apply(
            &mut state,
            ClientInput::Listing {
                endpoint_id: remote.endpoint_id.clone(),
                attempt: 1,
                listing,
            },
            0,
        );
        apply(
            &mut state,
            ClientInput::Bind {
                scope: remote.clone(),
                mode: ConnectionMode::ScopedGateway,
                grant: Grant {
                    grant_id: "g".into(),
                    tiers: vec![GrantTier::Read],
                    workspace_ids: vec!["w".into()],
                    expires_at_ms: None,
                },
                snapshot_mode: mode,
            },
            0,
        );
        assert!(
            state.listing.is_some(),
            "binding preserves current endpoint facts"
        );
        begin(&mut state, remote.clone(), 1);
        load(&mut state, remote.clone(), 1, snapshot(), 0);
        assert!(state.live());
        let changed = WorkspaceEntry {
            generation: 2,
            ..entry
        };
        apply(
            &mut state,
            ClientInput::Listing {
                endpoint_id: remote.endpoint_id.clone(),
                attempt: 1,
                listing: Workspaces {
                    rev: 2,
                    joined: vec![changed],
                    others: vec![],
                },
            },
            1,
        );
        let revision = state.revision;
        begin(&mut state, remote.clone(), 2);
        load(&mut state, remote.clone(), 2, Snapshot::default(), 2);
        apply(
            &mut state,
            ClientInput::Push {
                scope: remote,
                push: Push::AgentRemoved {
                    id: "claude-1".into(),
                },
            },
            2,
        );
        assert_eq!(state.revision, revision);
        assert!(!state.live());
        assert!(state.agents.iter().any(|agent| agent.id == "claude-1"));
    }

    #[test]
    fn gateway_availability_uses_listing_facts_and_distinguishes_removed_from_never_joined() {
        use crew_protocol::rpc::WorkspaceEntry;
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        apply(
            &mut state,
            ClientInput::EndpointChanged {
                endpoint_id: scope().endpoint_id,
                attempt: 1,
                status: EndpointStatus::Authenticated,
            },
            0,
        );
        apply(
            &mut state,
            ClientInput::Bind {
                scope: scope(),
                mode: ConnectionMode::ScopedGateway,
                grant: Grant {
                    grant_id: "g".into(),
                    tiers: vec![GrantTier::Read],
                    workspace_ids: vec!["w".into()],
                    expires_at_ms: None,
                },
                snapshot_mode: mode,
            },
            0,
        );
        apply(
            &mut state,
            ClientInput::Listing {
                endpoint_id: scope().endpoint_id,
                attempt: 1,
                listing: Workspaces {
                    rev: 1,
                    ..Workspaces::default()
                },
            },
            0,
        );
        assert_eq!(state.workspace, WorkspaceStatus::NeverJoined);
        let mut entry = WorkspaceEntry {
            id: "w".into(),
            generation: 1,
            daemon: None,
            project: None,
            workspace: "/work/w".into(),
            state: WorkspaceState::Running,
            detail: String::new(),
            summary: None,
            joined_at: 0,
        };
        for (index, (fact, expected)) in [
            (WorkspaceState::Stopped, WorkspaceStatus::Stopped),
            (WorkspaceState::Unreachable, WorkspaceStatus::Unreachable),
            (WorkspaceState::Incompatible, WorkspaceStatus::Incompatible),
            (WorkspaceState::Running, WorkspaceStatus::Connecting),
        ]
        .into_iter()
        .enumerate()
        {
            entry.state = fact;
            entry.detail = "source detail".into();
            apply(
                &mut state,
                ClientInput::Listing {
                    endpoint_id: scope().endpoint_id,
                    attempt: 1,
                    listing: Workspaces {
                        rev: index as u64 + 2,
                        joined: vec![entry.clone()],
                        others: vec![],
                    },
                },
                1,
            );
            assert_eq!(state.workspace, expected);
            assert_eq!(state.reason.as_ref().unwrap().message, "source detail");
        }
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 2);
        assert_eq!(state.workspace, WorkspaceStatus::Live);
        apply(
            &mut state,
            ClientInput::Listing {
                endpoint_id: scope().endpoint_id,
                attempt: 1,
                listing: Workspaces {
                    rev: 6,
                    ..Workspaces::default()
                },
            },
            3,
        );
        assert_eq!(state.workspace, WorkspaceStatus::Removed);
        assert_eq!(state.freshness(), Freshness::Stale);
    }

    #[test]
    fn gateway_index_and_workspace_attempts_remain_independent_but_endpoint_reconnect_fences_both()
    {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        apply(
            &mut state,
            ClientInput::EndpointChanged {
                endpoint_id: scope().endpoint_id,
                attempt: 1,
                status: EndpointStatus::Authenticated,
            },
            0,
        );
        let mut workspace = scope();
        workspace.attempt = 2;
        apply(
            &mut state,
            ClientInput::Bind {
                scope: workspace.clone(),
                mode: ConnectionMode::ScopedGateway,
                grant: Grant {
                    grant_id: "g".into(),
                    tiers: vec![GrantTier::Read],
                    workspace_ids: vec!["w".into()],
                    expires_at_ms: None,
                },
                snapshot_mode: mode,
            },
            0,
        );
        begin(&mut state, workspace.clone(), 1);
        load(&mut state, workspace.clone(), 1, snapshot(), 0);
        assert!(state.live());
        apply(
            &mut state,
            ClientInput::Listing {
                endpoint_id: scope().endpoint_id,
                attempt: 1,
                listing: Workspaces {
                    rev: 1,
                    ..Workspaces::default()
                },
            },
            1,
        );
        assert_eq!(
            state.listing.as_ref().unwrap().rev,
            1,
            "live index was not promoted to workspace attempt2"
        );
        assert_eq!(state.workspace, WorkspaceStatus::NeverJoined);
        apply(
            &mut state,
            ClientInput::EndpointChanged {
                endpoint_id: scope().endpoint_id,
                attempt: 3,
                status: EndpointStatus::Authenticated,
            },
            2,
        );
        let revision = state.revision;
        begin(&mut state, workspace.clone(), 2);
        load(&mut state, workspace, 2, Snapshot::default(), 2);
        apply(
            &mut state,
            ClientInput::Listing {
                endpoint_id: scope().endpoint_id,
                attempt: 1,
                listing: Workspaces {
                    rev: 99,
                    ..Workspaces::default()
                },
            },
            2,
        );
        assert_eq!(state.revision, revision);
        assert!(state.listing.is_none(), "old endpoint listing was fenced");
        assert!(!state.live());
        apply(
            &mut state,
            ClientInput::Listing {
                endpoint_id: scope().endpoint_id,
                attempt: 3,
                listing: Workspaces {
                    rev: 1,
                    ..Workspaces::default()
                },
            },
            3,
        );
        assert_eq!(
            state.listing.as_ref().unwrap().rev,
            1,
            "new index revision restarted safely"
        );
    }

    #[test]
    fn inactive_lifecycle_retires_the_barrier_and_resumes_only_after_new_synchronization() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        apply(&mut state, ClientInput::Lifecycle { active: false }, 1);
        assert!(!state.live());
        assert_eq!(state.freshness(), Freshness::Stale);
        let revision = state.revision;
        begin(&mut state, scope(), 2);
        load(&mut state, scope(), 2, Snapshot::default(), 2);
        assert_eq!(state.revision, revision);
        let resumed = apply(&mut state, ClientInput::Lifecycle { active: true }, 3);
        assert_eq!(
            resumed.effects,
            [ClientEffect::Resynchronize { scope: scope() }]
        );
        assert!(!state.live());
        begin(&mut state, scope(), 3);
        load(&mut state, scope(), 3, snapshot(), 4);
        assert!(state.live());
    }

    #[test]
    fn negative_event_inputs_fail_before_queueing_or_replacing_domain_state() {
        let mode = SnapshotMode::BufferedOverlap;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        let mut event = state.events.items().next().unwrap().clone();
        event.seq = -1;
        let before = state.events.items().cloned().collect::<Vec<_>>();
        for id in [None, Some(2)] {
            if let Some(id) = id {
                begin(&mut state, scope(), id);
            }
            let revision = state.revision;
            assert!(state
                .apply_data(
                    ClientInput::Push {
                        scope: scope(),
                        push: Push::Event(event.clone())
                    },
                    clock(1)
                )
                .is_err());
            assert_eq!(state.revision, revision);
            assert_eq!(state.events.items().cloned().collect::<Vec<_>>(), before);
        }
        load(&mut state, scope(), 2, snapshot(), 2);
        assert!(state.live());
    }

    #[test]
    fn identical_agent_reports_sample_usage_at_the_original_bounded_cadence() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        let mut snap = snapshot();
        snap.agents[0].context = Some(crew_protocol::ContextUsage::new(10, 100, 0));
        let agent = snap.agents[0].clone();
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snap, 0);
        let initial = state.revision;
        for at in [500, 1000, 2000, 3000] {
            apply(
                &mut state,
                ClientInput::Push {
                    scope: scope(),
                    push: Push::Agent(Box::new(agent.clone())),
                },
                at,
            );
        }
        assert_eq!(state.revision, initial + 3);
        assert_eq!(
            state.usage_samples[&agent.id]
                .iter()
                .map(|point| point.at_ms)
                .collect::<Vec<_>>(),
            [1000, 2000, 3000]
        );
    }

    #[test]
    fn a_read_for_a_replaced_agent_cannot_populate_the_new_agents_cache() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        load(&mut state, scope(), 1, snapshot(), 0);
        let mut agent = state.agents[0].clone();
        let key = ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Agent(agent.id.clone()),
            },
            kind: ResourceKind::Output,
            options: ReadOptions::Output {
                lines: 300,
                screen: false,
                ansi: false,
            },
        };
        state.begin_read(7, key.clone()).unwrap();
        agent.pid = Some(9999);
        apply(
            &mut state,
            ClientInput::Push {
                scope: scope(),
                push: Push::Agent(Box::new(agent)),
            },
            1,
        );
        assert!(!state
            .complete_read(
                7,
                &OperationOutcome::Succeeded {
                    result: CommandResult::Output(crew_protocol::rpc::OutputResult {
                        text: "old process".into(),
                        alive: true
                    })
                }
            )
            .unwrap());
        assert_eq!(state.current_resource(&key).status, ResourceStatus::Idle);
    }

    #[test]
    fn clock_offset_preserves_extreme_samples_without_panicking_or_losing_the_snapshot_time() {
        let mode = SnapshotMode::OrderedConnection;
        let mut state = ClientState::new(config(mode)).unwrap();
        bind(&mut state, scope(), mode);
        begin(&mut state, scope(), 1);
        let mut snap = snapshot();
        snap.server_time = i64::MAX;
        load(&mut state, scope(), 1, snap, i64::MIN);
        assert_eq!(state.server_now(clock(i64::MIN)), i64::MAX);
        assert_eq!(state.server_now(clock(i64::MAX)), i64::MAX);
    }

    #[test]
    fn all_five_reviewed_state_cases_drive_real_reduction() {
        for text in [
            include_str!("../../../fixtures/native/state/work-choice.case.json"),
            include_str!("../../../fixtures/native/state/parked-resumption.case.json"),
            include_str!("../../../fixtures/native/state/completed-ties.case.json"),
            include_str!("../../../fixtures/native/state/optional-values.case.json"),
            include_str!("../../../fixtures/native/state/snapshot-replacement.case.json"),
        ] {
            let case: Value = serde_json::from_str(text).unwrap();
            let mode = SnapshotMode::BufferedOverlap;
            let mut state = ClientState::new(config(mode)).unwrap();
            bind(&mut state, scope(), mode);
            for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let at = step["at_ms"].as_i64().unwrap();
                if let Some(snap) = step.get("snapshot") {
                    begin(&mut state, scope(), index as u64 + 1);
                    load(
                        &mut state,
                        scope(),
                        index as u64 + 1,
                        serde_json::from_value(snap.clone()).unwrap(),
                        at,
                    );
                } else {
                    let frame: ServerFrame = serde_json::from_value(step["frame"].clone()).unwrap();
                    apply(
                        &mut state,
                        ClientInput::Push {
                            scope: scope(),
                            push: frame.push.unwrap(),
                        },
                        at,
                    );
                }
                let mut agent_ids: Vec<_> = state.agents.iter().map(|a| &a.id).collect();
                agent_ids.sort();
                let agents: BTreeMap<_, _> = state.agents.iter().map(|a| (&a.id, a)).collect();
                let tasks: BTreeMap<_, _> = state.tasks.iter().map(|t| (&t.id, t)).collect();
                let messages: Vec<_> = state.messages.items().collect();
                let events: Vec<_> = state.events.items().collect();
                let work: BTreeMap<_, _> = state
                    .agents
                    .iter()
                    .map(|a| {
                        let chosen =
                            crate::current_work(&state.tasks, &a.id, state.server_now(clock(at)));
                        (a.id.clone(), chosen)
                    })
                    .collect();
                let actual = json!({ "agent_ids":agent_ids, "agents":agents, "task_ids":state.tasks.iter().map(|t| &t.id).collect::<Vec<_>>(), "tasks":tasks, "messages":messages, "message_ids":messages.iter().map(|m| &m.id).collect::<Vec<_>>(), "events":events, "event_seqs":events.iter().map(|e| e.seq).collect::<Vec<_>>(), "question_ids":state.questions.visible().map(|q| &q.id).collect::<Vec<_>>(), "work":work, "clock_offset_ms":state.clock_offset_ms });
                let mut expected = step["expect"].clone();
                expected
                    .as_object_mut()
                    .unwrap()
                    .remove("selection_validity");
                subset(&actual, &expected, &format!("{}/{index}", case["id"]));
            }
        }
    }

    fn subset(actual: &Value, expected: &Value, path: &str) {
        match expected {
            Value::Object(fields) => {
                for (key, expected) in fields {
                    subset(
                        actual
                            .get(key)
                            .unwrap_or_else(|| panic!("{path}.{key}: missing")),
                        expected,
                        &format!("{path}.{key}"),
                    );
                }
            }
            Value::Array(items) => {
                let values = actual.as_array().unwrap();
                assert_eq!(values.len(), items.len(), "{path}");
                for (index, (actual, expected)) in values.iter().zip(items).enumerate() {
                    subset(actual, expected, &format!("{path}[{index}]"));
                }
            }
            _ => assert_eq!(actual, expected, "{path}"),
        }
    }
}
