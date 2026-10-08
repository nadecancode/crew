//! One driver for native hosts. Command encoding is supplied by the shared
//! client codec, never duplicated by a platform adapter.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use base64::Engine as _;
use crew_client::api::*;
use crew_client::policy::{method_policy, validate_limits, NATIVE_LIMITS};
use crew_protocol::rpc::{Push, ServerFrame, Snapshot, Workspaces, PROTOCOL_VERSION};
use serde::Deserialize;

use crate::framing::{validate_record, FrameError, MAX_BODY_BYTES, MAX_FRAME_BYTES};
use crate::{
    ChannelId, ChannelPurpose, Clock, HostEffect, HostEvent, HostUpdate, SendDisposition,
    SystemClock, TransportEnd,
};

/// The real implementation delegates to Command::encode/decode_result. This
/// port also lets correlation tests exercise the driver independently of the
/// action implementation, with real protocol types in their test codec.
pub trait CommandCodec: Send + Sync {
    fn encode(&self, command: &Command, request_id: u64) -> Result<String, ClientError>;
    fn decode_result(
        &self,
        command: &Command,
        value: serde_json::Value,
    ) -> Result<CommandResult, ClientError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Protocol6Codec;

impl CommandCodec for Protocol6Codec {
    fn encode(&self, command: &Command, request_id: u64) -> Result<String, ClientError> {
        command.encode(request_id)
    }
    fn decode_result(
        &self,
        command: &Command,
        value: serde_json::Value,
    ) -> Result<CommandResult, ClientError> {
        command.decode_result(value)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DriverConfig {
    pub max_channels: usize,
    pub max_pending: usize,
    pub max_channel_pending: usize,
    pub snapshot_messages: u32,
    pub snapshot_events: u32,
    pub reconnect: bool,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            max_channels: 8,
            max_pending: NATIVE_LIMITS.pending_operations as usize,
            max_channel_pending: 16,
            snapshot_messages: 300,
            snapshot_events: 1000,
            reconnect: true,
        }
    }
}

impl DriverConfig {
    fn validate(self) -> Result<Self, ClientError> {
        let pending_operations =
            u32::try_from(self.max_pending).map_err(|_| ClientError::InvalidInput {
                field: "pending_operations".into(),
                reason: "invalid native pending request bound".into(),
            })?;
        validate_limits(ClientLimits {
            pending_operations,
            ..NATIVE_LIMITS
        })?;
        if !(1..=64).contains(&self.max_channels)
            || !(1..=16).contains(&self.max_channel_pending)
            || self.snapshot_messages > 2000
            || self.snapshot_events > 5000
        {
            return Err(ClientError::InvalidInput {
                field: "driver_limits".into(),
                reason: "invalid native transport bounds".into(),
            });
        }
        Ok(self)
    }
}

pub struct DriverPlan {
    pub plan: ActionPlan,
    pub update: HostUpdate,
}

#[derive(Clone)]
struct Selection {
    profile_id: String,
    endpoint_id: String,
    workspace_hint: Option<String>,
    pinned_workspace: Option<String>,
    /// The endpoint authenticates a separate index channel (scoped gateway).
    /// Its device authority and listing outlive any one workspace route, so
    /// every reconnect reopens it beside the selected workspace.
    indexed: bool,
}

struct Channel {
    purpose: ChannelPurpose,
    attempt: u64,
    expected: Option<ClientScope>,
    context: Option<ConnectionContext>,
    ready: bool,
    features: crew_client::features::FeatureAvailability,
    next_request: u64,
    waiting: VecDeque<u64>,
    open_timer: Option<u64>,
    // Successful terminal notifications never receive replies. All IDs in
    // this interval belong to this channel's one immutable attachment, so
    // late errors can be handled without retaining one entry per keystroke.
    unacked_first: Option<u64>,
    unacked_last: u64,
    terminal_live: bool,
    buffered_terminal: VecDeque<Vec<u8>>,
    terminal_bytes: usize,
}

enum PendingKind {
    Discover {
        expected: Option<ClientScope>,
    },
    Subscribe {
        snapshot_id: u64,
    },
    Snapshot {
        snapshot_id: u64,
    },
    VerifyLane {
        expected: ClientScope,
    },
    Listing,
    Operation {
        scope: ClientScope,
        operation_id: u64,
        lane: RequestLane,
    },
}

struct Pending {
    command: Command,
    kind: PendingKind,
    disposition: SendDisposition,
    timer: u64,
    text: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimerOwner {
    Open(ChannelId),
    Request(ChannelId, u64),
    Core(u64),
    Reconnect,
}

#[derive(Clone, Copy)]
struct TimerKey {
    owner: TimerOwner,
    attempt: u64,
    deadline_ms: u64,
}

#[derive(Default)]
struct Batch {
    changes: ChangeSet,
    effects: Vec<HostEffect>,
    outcomes: Vec<OperationNotice>,
    terminal: Vec<TerminalChunk>,
    core_effects: VecDeque<ClientEffect>,
    reason: Option<ClientReason>,
}

/// Calls are serialized by the Rust service. This object performs no IO and
/// invokes no observer; every async event is fenced by channel and full scope.
pub struct SessionDriver<C, W> {
    core: C,
    codec: W,
    clock: Arc<dyn Clock>,
    config: DriverConfig,
    revision: u64,
    next_channel: u64,
    next_attempt: u64,
    next_timer: u64,
    next_snapshot: u64,
    channels: BTreeMap<ChannelId, Channel>,
    pending: BTreeMap<(ChannelId, u64), Pending>,
    timers: BTreeMap<u64, TimerKey>,
    selection: Option<Selection>,
    binding: Option<ConnectionContext>,
    visible_scope: Option<ClientScope>,
    root: Option<ChannelId>,
    index: Option<ChannelId>,
    index_attempt: Option<u64>,
    listing: Option<Workspaces>,
    endpoint: EndpointStatus,
    workspace: WorkspaceStatus,
    live: bool,
    active: bool,
    disposed: bool,
    reconnect_count: u32,
}

impl<C: ClientApi, W: CommandCodec> SessionDriver<C, W> {
    pub fn new(core: C, codec: W, config: DriverConfig) -> Result<Self, ClientError> {
        Self::with_clock(core, codec, config, Arc::new(SystemClock::default()))
    }

    pub fn with_clock(
        core: C,
        codec: W,
        config: DriverConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ClientError> {
        Ok(Self {
            core,
            codec,
            clock,
            config: config.validate()?,
            revision: 0,
            next_channel: 1,
            next_attempt: 1,
            next_timer: 1,
            next_snapshot: 1 << 63,
            channels: BTreeMap::new(),
            pending: BTreeMap::new(),
            timers: BTreeMap::new(),
            selection: None,
            binding: None,
            visible_scope: None,
            root: None,
            index: None,
            index_attempt: None,
            listing: None,
            endpoint: EndpointStatus::Disconnected,
            workspace: WorkspaceStatus::NoneSelected,
            live: false,
            active: true,
            disposed: false,
            reconnect_count: 0,
        })
    }

    pub fn query(&self, query: ViewQuery) -> Result<ViewData, ClientError> {
        self.ensure_open()?;
        self.core.view(query, self.clock.sample())
    }

    pub fn plan(&mut self, intent: ActionIntent) -> Result<DriverPlan, ClientError> {
        self.ensure_open()?;
        let clock = self.clock.sample();
        let planned = self.core.plan(intent, clock)?;
        let mut batch = Batch::default();
        self.absorb(planned.update, &mut batch)?;
        let update = self.finish(batch, clock)?;
        Ok(DriverPlan {
            plan: planned.plan,
            update,
        })
    }

    pub fn confirm(&mut self, id: u64) -> Result<HostUpdate, ClientError> {
        self.ensure_open()?;
        let clock = self.clock.sample();
        let update = self.core.confirm(id, clock)?;
        let mut batch = Batch::default();
        self.absorb(update, &mut batch)?;
        self.finish(batch, clock)
    }

    pub fn cancel_confirmation(&mut self, id: u64) -> Result<HostUpdate, ClientError> {
        self.ensure_open()?;
        let update = self.core.cancel_confirmation(id)?;
        let mut batch = Batch::default();
        self.absorb(update, &mut batch)?;
        self.finish(batch, self.clock.sample())
    }

    /// A configured profile chooses transport, not authority. No grant is
    /// supplied until its connector reports verified authentication.
    pub fn connect(
        &mut self,
        profile_id: String,
        endpoint_id: String,
        workspace_hint: Option<String>,
    ) -> Result<HostUpdate, ClientError> {
        self.ensure_open()?;
        if profile_id.is_empty()
            || profile_id.len() > 128
            || endpoint_id.is_empty()
            || endpoint_id.len() > 128
            || workspace_hint
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 4096)
        {
            return Err(ClientError::InvalidInput {
                field: "profile".into(),
                reason: "invalid connection profile identity".into(),
            });
        }
        let clock = self.clock.sample();
        let mut batch = Batch::default();
        self.retire_all(TransportEnd::Stopped, false, &mut batch, clock)?;
        self.selection = Some(Selection {
            profile_id,
            endpoint_id,
            workspace_hint,
            pinned_workspace: None,
            indexed: false,
        });
        self.listing = None;
        self.reconnect_count = 0;
        self.start_selection(&mut batch, clock)?;
        self.finish(batch, clock)
    }

    pub fn select_workspace(&mut self, id: String) -> Result<HostUpdate, ClientError> {
        self.ensure_open()?;
        let clock = self.clock.sample();
        let mut batch = Batch::default();
        let entry = self
            .listing
            .as_ref()
            .and_then(|listing| listing.joined.iter().find(|entry| entry.id == id))
            .cloned()
            .ok_or_else(|| ClientError::InvalidInput {
                field: "workspace".into(),
                reason: "workspace access is unavailable".into(),
            })?;
        self.retire_workspace(TransportEnd::Stopped, &mut batch, clock)?;
        let selection = self.selection.as_mut().ok_or(ClientError::ClosedClient)?;
        selection.workspace_hint = Some(id.clone());
        selection.pinned_workspace = Some(id.clone());
        let attempt = allocate(&mut self.next_attempt)?;
        let scope = ClientScope {
            endpoint_id: selection.endpoint_id.clone(),
            workspace_id: id,
            generation: Some(entry.generation),
            daemon_pid: entry.daemon.map(|value| value.pid),
            daemon_started_at_ms: entry.daemon.map(|value| value.started_at),
            attempt,
        };
        self.workspace = WorkspaceStatus::Connecting;
        batch.changes.availability = true;
        let channel = self.open_channel(
            ChannelPurpose::Workspace,
            attempt,
            Some(scope),
            &mut batch,
            clock,
        )?;
        self.root = Some(channel);
        self.finish(batch, clock)
    }

    pub fn reconnect(&mut self) -> Result<HostUpdate, ClientError> {
        self.ensure_open()?;
        let clock = self.clock.sample();
        let mut batch = Batch::default();
        self.retire_all(TransportEnd::Closed, false, &mut batch, clock)?;
        self.start_selection(&mut batch, clock)?;
        self.finish(batch, clock)
    }

    pub fn disconnect(&mut self) -> Result<HostUpdate, ClientError> {
        self.ensure_open()?;
        let clock = self.clock.sample();
        let mut batch = Batch::default();
        self.retire_all(TransportEnd::Stopped, false, &mut batch, clock)?;
        self.endpoint = EndpointStatus::Disconnected;
        self.workspace = if self.visible_scope.is_some() {
            WorkspaceStatus::Unreachable
        } else {
            WorkspaceStatus::NoneSelected
        };
        self.finish(batch, clock)
    }

    /// Dispose closes resources only. It never dispatches Shutdown/StopTeam.
    pub fn close(&mut self) -> Result<HostUpdate, ClientError> {
        if self.disposed {
            return Ok(self.empty_update());
        }
        let update = self.disconnect()?;
        self.disposed = true;
        Ok(update)
    }

    pub fn handle(&mut self, event: HostEvent) -> Result<HostUpdate, ClientError> {
        if self.disposed {
            return Ok(self.empty_update());
        }
        let clock = self.clock.sample();
        let mut batch = Batch::default();
        match event {
            HostEvent::EndpointOpened {
                channel_id,
                endpoint_id,
                attempt,
            } => {
                if self.channels.get(&channel_id).is_some_and(|channel| {
                    channel.purpose == ChannelPurpose::Index && channel.attempt == attempt
                }) && self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.endpoint_id == endpoint_id)
                {
                    self.cancel_open_timer(channel_id, &mut batch);
                    self.index = Some(channel_id);
                    self.index_attempt = Some(attempt);
                    if let Some(selection) = &mut self.selection {
                        selection.indexed = true;
                    }
                    self.endpoint = EndpointStatus::Authenticated;
                    self.input(
                        ClientInput::EndpointChanged {
                            endpoint_id,
                            attempt,
                            status: self.endpoint,
                        },
                        clock,
                        &mut batch,
                    )?;
                    self.request(
                        channel_id,
                        Command::Workspaces,
                        PendingKind::Listing,
                        false,
                        &mut batch,
                        clock,
                    )?;
                }
            }
            HostEvent::WorkspaceOpened {
                channel_id,
                context,
                peer,
            } => self.opened(channel_id, context, peer, &mut batch, clock)?,
            HostEvent::Listing {
                channel_id,
                request_id,
                listing,
            } => self.receive_listing(channel_id, request_id, listing, &mut batch, clock)?,
            HostEvent::Frame { channel_id, text } => {
                self.frame(channel_id, text, &mut batch, clock)?
            }
            HostEvent::SendProgress {
                channel_id,
                request_id,
                disposition,
            } => self.sent(channel_id, request_id, disposition, &mut batch, clock)?,
            HostEvent::Ended { channel_id, reason } => {
                self.end_channel(channel_id, reason, true, &mut batch, clock)?
            }
            HostEvent::Wake { timer_id } => self.wake(timer_id, &mut batch, clock)?,
            HostEvent::Lifecycle { active } => {
                self.active = active;
                self.input(ClientInput::Lifecycle { active }, clock, &mut batch)?;
                if active {
                    if self.channels.is_empty() && self.selection.is_some() {
                        self.start_selection(&mut batch, clock)?;
                    }
                } else {
                    self.retire_all(TransportEnd::Closed, false, &mut batch, clock)?;
                    self.endpoint = EndpointStatus::Disconnected;
                }
            }
        }
        self.finish(batch, clock)
    }

    pub fn notice(&self) -> ClientNotice {
        self.empty_update().notice
    }
    pub fn connection_context(&self) -> Option<ConnectionContext> {
        self.binding.clone()
    }

    pub fn workspaces(&self) -> Option<Workspaces> {
        self.listing.clone()
    }

    /// Backpressure in the typed observer is a real disconnect. Preserve final
    /// operation outcomes, fence IO, and require explicit fresh synchronization.
    pub fn observer_overflow(&mut self) -> Result<HostUpdate, ClientError> {
        let clock = self.clock.sample();
        let mut batch = Batch::default();
        self.retire_all(TransportEnd::Overflow, false, &mut batch, clock)?;
        self.endpoint = EndpointStatus::Reconnecting;
        self.workspace = WorkspaceStatus::Reconnecting;
        batch.changes.availability = true;
        batch.changes.reset = true;
        batch.reason = Some(TransportEnd::Overflow.reason());
        self.finish(batch, clock)
    }
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn timer_count(&self) -> usize {
        self.timers.len()
    }

    #[cfg(feature = "runtime")]
    pub(crate) fn effect_is_current(&self, effect: &HostEffect) -> bool {
        match effect {
            HostEffect::Open {
                channel_id,
                attempt,
                ..
            } => self
                .channels
                .get(channel_id)
                .is_some_and(|channel| channel.attempt == *attempt),
            HostEffect::Send {
                channel_id,
                request_id,
                ..
            } => {
                self.channels.contains_key(channel_id)
                    && self.pending.contains_key(&(*channel_id, *request_id))
            }
            HostEffect::Schedule {
                timer_id,
                deadline_ms,
            } => self
                .timers
                .get(timer_id)
                .is_some_and(|timer| timer.deadline_ms == *deadline_ms),
            HostEffect::Close { .. } | HostEffect::CancelTimer { .. } => true,
        }
    }

    #[cfg(feature = "runtime")]
    pub(crate) fn state_failed(&mut self) -> HostUpdate {
        // A failed core cannot classify final operations. Do not synthesize
        // notices on its behalf or keep accepting IO into a broken instance.
        let mut effects: Vec<_> = self
            .channels
            .keys()
            .map(|id| HostEffect::Close { channel_id: *id })
            .collect();
        effects.extend(
            self.timers
                .keys()
                .map(|id| HostEffect::CancelTimer { timer_id: *id }),
        );
        self.channels.clear();
        self.pending.clear();
        self.timers.clear();
        self.binding = None;
        self.root = None;
        self.index = None;
        self.index_attempt = None;
        self.listing = None;
        self.live = false;
        self.disposed = true;
        self.endpoint = EndpointStatus::Stopped;
        self.workspace = WorkspaceStatus::Unreachable;
        let mut update = self.empty_update();
        update.effects = effects;
        update.notice.changes.reset = true;
        update.notice.changes.availability = true;
        update.notice.reason = Some(ClientReason {
            details: None,
            code: "client_state".into(),
            message: "The client could not update its state. Reconnect with a new client.".into(),
        });
        update
    }

    fn ensure_open(&self) -> Result<(), ClientError> {
        if self.disposed {
            Err(ClientError::ClosedClient)
        } else {
            Ok(())
        }
    }

    fn start_selection(
        &mut self,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        if !self.active {
            return Ok(());
        }
        let selection = self.selection.clone().ok_or(ClientError::ClosedClient)?;
        let mut attempt = allocate(&mut self.next_attempt)?;
        if self.index.is_none() {
            self.endpoint = EndpointStatus::Connecting;
            self.input(
                ClientInput::EndpointChanged {
                    endpoint_id: selection.endpoint_id.clone(),
                    attempt,
                    status: self.endpoint,
                },
                clock,
                batch,
            )?;
            if selection.indexed && selection.workspace_hint.is_some() {
                // Reopen the endpoint index first, as on the initial connect,
                // then the selected workspace with its own later attempt.
                self.index =
                    Some(self.open_channel(ChannelPurpose::Index, attempt, None, batch, clock)?);
                attempt = allocate(&mut self.next_attempt)?;
            }
        }
        if selection.workspace_hint.is_some() {
            self.workspace = WorkspaceStatus::Connecting;
            let expected = selection.pinned_workspace.map(|workspace_id| ClientScope {
                endpoint_id: selection.endpoint_id,
                workspace_id,
                generation: self
                    .visible_scope
                    .as_ref()
                    .and_then(|scope| scope.generation),
                daemon_pid: None,
                daemon_started_at_ms: None,
                attempt,
            });
            self.root = Some(self.open_channel(
                ChannelPurpose::Workspace,
                attempt,
                expected,
                batch,
                clock,
            )?);
        } else {
            self.workspace = WorkspaceStatus::NoneSelected;
            self.index =
                Some(self.open_channel(ChannelPurpose::Index, attempt, None, batch, clock)?);
        }
        Ok(())
    }

    fn open_channel(
        &mut self,
        purpose: ChannelPurpose,
        attempt: u64,
        expected: Option<ClientScope>,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<ChannelId, ClientError> {
        if self.channels.len() >= self.config.max_channels {
            return Err(capacity());
        }
        let selection = self.selection.clone().ok_or(ClientError::ClosedClient)?;
        let id = allocate(&mut self.next_channel)?;
        let timer = self.schedule(
            TimerOwner::Open(id),
            attempt,
            clock.monotonic_ms.saturating_add(3000),
            batch,
        )?;
        self.channels.insert(
            id,
            Channel {
                purpose,
                attempt,
                expected: expected.clone(),
                context: None,
                ready: false,
                features: Default::default(),
                next_request: 1,
                waiting: VecDeque::new(),
                open_timer: Some(timer),
                unacked_first: None,
                unacked_last: 0,
                terminal_live: false,
                buffered_terminal: VecDeque::new(),
                terminal_bytes: 0,
            },
        );
        batch.effects.push(HostEffect::Open {
            channel_id: id,
            profile_id: selection.profile_id,
            endpoint_id: selection.endpoint_id,
            attempt,
            purpose,
            expected_scope: expected,
        });
        Ok(id)
    }

    fn opened(
        &mut self,
        id: ChannelId,
        context: ConnectionContext,
        peer: PeerInfo,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        let Some(channel) = self.channels.get(&id) else {
            return Ok(());
        };
        if let Some(existing) = &channel.context {
            if existing == &context {
                return Ok(());
            }
            return self.end_channel(id, TransportEnd::GrantChanged, false, batch, clock);
        }
        if context.scope.attempt != channel.attempt
            || context.scope.endpoint_id
                != self
                    .selection
                    .as_ref()
                    .map_or("", |value| &value.endpoint_id)
            || peer.protocol != PROTOCOL_VERSION
            || peer.pid == 0
            || context.scope.daemon_pid != Some(peer.pid)
            || context.scope.workspace_id.is_empty()
            || context.scope.workspace_id.len() > 4096
            || context.snapshot_mode != SnapshotMode::OrderedConnection
            || !context.grant.tiers.contains(&GrantTier::Read)
            || !context
                .grant
                .workspace_ids
                .contains(&context.scope.workspace_id)
            || context.grant.grant_id.is_empty()
            || context.grant.grant_id.len() > 512
        {
            return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
        }
        let purpose = channel.purpose;
        let expected = channel.expected.clone();
        self.cancel_open_timer(id, batch);
        if let Some(channel) = self.channels.get_mut(&id) {
            channel.context = Some(context.clone());
        }
        if purpose == ChannelPurpose::Workspace {
            if expected.as_ref().is_some_and(|scope| {
                scope.workspace_id != context.scope.workspace_id
                    || scope.generation != context.scope.generation
            }) {
                return self.end_channel(id, TransportEnd::GrantChanged, false, batch, clock);
            }
            if expected.as_ref().is_some_and(|scope| {
                scope.daemon_pid.is_some() && scope.daemon_pid != context.scope.daemon_pid
            }) {
                return self.end_channel(id, TransportEnd::Closed, true, batch, clock);
            }
            if context.mode == ConnectionMode::ScopedGateway {
                if let Some(selection) = &mut self.selection {
                    selection.indexed = true;
                }
            }
            self.binding = Some(context.clone());
            self.visible_scope = Some(context.scope.clone());
            self.endpoint = EndpointStatus::Authenticated;
            self.workspace = WorkspaceStatus::Connecting;
            self.live = false;
            self.bind(&context, batch, clock)?;
            self.discover(id, None, batch, clock)?;
        } else if let Some(expected) = expected {
            let Some(binding) = &self.binding else {
                return self.end_channel(id, TransportEnd::Closed, false, batch, clock);
            };
            if expected != binding.scope
                || context.mode != binding.mode
                || context.grant != binding.grant
                || context.scope.daemon_pid != expected.daemon_pid
                || context.scope.workspace_id != expected.workspace_id
            {
                return self.end_channel(id, TransportEnd::GrantChanged, false, batch, clock);
            }
            if matches!(purpose, ChannelPurpose::Terminal { .. }) {
                self.verify_lane(id, expected, batch, clock)?;
            } else {
                self.discover(id, Some(expected), batch, clock)?;
            }
        } else {
            return self.end_channel(id, TransportEnd::Malformed, false, batch, clock);
        }
        Ok(())
    }

    fn discover(
        &mut self,
        id: ChannelId,
        expected: Option<ClientScope>,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        self.request(
            id,
            Command::FeaturesNegotiate(crew_protocol::features::FeaturesNegotiate {
                versions: crew_client::features::requested_versions(),
            }),
            PendingKind::Discover { expected },
            false,
            batch,
            clock,
        )?;
        Ok(())
    }

    fn discovered(
        &mut self,
        id: ChannelId,
        expected: Option<ClientScope>,
        availability: crew_client::features::FeatureAvailability,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        if availability.session.as_ref().is_some_and(|session| {
            self.channels.iter().any(|(other, c)| {
                *other != id
                    && c.features
                        .session
                        .as_ref()
                        .is_some_and(|s| s.connection_epoch == session.connection_epoch)
            })
        }) {
            return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
        }
        self.channels
            .get_mut(&id)
            .ok_or(ClientError::ClosedClient)?
            .features = availability;
        if let Some(expected) = expected {
            return self.verify_lane(id, expected, batch, clock);
        }
        let context = self
            .channels
            .get(&id)
            .and_then(|c| c.context.clone())
            .ok_or(ClientError::ClosedClient)?;
        let snapshot_id = allocate(&mut self.next_snapshot)?;
        self.input(
            ClientInput::SnapshotStarted {
                scope: context.scope,
                operation_id: snapshot_id,
            },
            clock,
            batch,
        )?;
        self.request(
            id,
            Command::Subscribe,
            PendingKind::Subscribe { snapshot_id },
            false,
            batch,
            clock,
        )?;
        Ok(())
    }

    fn verify_lane(
        &mut self,
        id: ChannelId,
        expected: ClientScope,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        let context = self
            .channels
            .get(&id)
            .and_then(|c| c.context.clone())
            .ok_or(ClientError::ClosedClient)?;
        // Discovery precedes Snapshot, but no operation leaves until this existing
        // full local identity check (including startup time) has succeeded.
        if context.mode == ConnectionMode::LocalOperator {
            self.request(
                id,
                Command::Snapshot {
                    messages: Some(0),
                    events: Some(0),
                },
                PendingKind::VerifyLane { expected },
                false,
                batch,
                clock,
            )?;
        } else if context.scope == expected {
            self.lane_ready(id, expected, batch, clock)?;
        } else {
            self.end_channel(id, TransportEnd::GrantChanged, false, batch, clock)?;
        }
        Ok(())
    }

    fn bind(
        &mut self,
        context: &ConnectionContext,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        self.input(
            ClientInput::Bind {
                scope: context.scope.clone(),
                mode: context.mode,
                grant: context.grant.clone(),
                snapshot_mode: context.snapshot_mode,
            },
            clock,
            batch,
        )
    }

    fn lane_ready(
        &mut self,
        id: ChannelId,
        scope: ClientScope,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        let Some(channel) = self.channels.get_mut(&id) else {
            return Ok(());
        };
        channel.ready = true;
        if let Some(context) = &mut channel.context {
            context.scope = scope;
        }
        let waiting = std::mem::take(&mut channel.waiting);
        for request in waiting {
            let supported = self
                .pending
                .get(&(id, request))
                .and_then(|p| p.command.required_feature())
                .is_none_or(|f| self.channels[&id].features.supports(f));
            if !supported {
                if let Some(pending) = self.take_pending(id, request, batch) {
                    if let PendingKind::Operation {
                        scope,
                        operation_id,
                        ..
                    } = pending.kind
                    {
                        self.not_sent(scope, operation_id, "unsupported_feature", batch, clock)?;
                    }
                }
                continue;
            }
            if let Some(pending) = self.pending.get_mut(&(id, request)) {
                if let Some(text) = pending.text.take() {
                    batch.effects.push(HostEffect::Send {
                        channel_id: id,
                        request_id: request,
                        text,
                    });
                }
            }
        }
        Ok(())
    }

    fn request(
        &mut self,
        channel_id: ChannelId,
        command: Command,
        kind: PendingKind,
        wait_ready: bool,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<u64, ClientError> {
        let channel_pending = self
            .pending
            .keys()
            .filter(|(id, _)| *id == channel_id)
            .count();
        if self.pending.len() >= self.config.max_pending
            || channel_pending >= self.config.max_channel_pending
        {
            return Err(capacity());
        }
        let channel = self
            .channels
            .get_mut(&channel_id)
            .ok_or(ClientError::ClosedClient)?;
        let request_id = allocate(&mut channel.next_request)?;
        let text = self.codec.encode(&command, request_id)?;
        validate_record(&text).map_err(|_| ClientError::TooLarge {
            actual: text.len() as u64,
            limit: MAX_FRAME_BYTES as u64,
        })?;
        let attempt = channel.attempt;
        let waiting = wait_ready && !channel.ready;
        let deadline = clock
            .monotonic_ms
            .saturating_add(method_policy(command.kind()).timeout_ms);
        let timer = self.schedule(
            TimerOwner::Request(channel_id, request_id),
            attempt,
            deadline,
            batch,
        )?;
        self.pending.insert(
            (channel_id, request_id),
            Pending {
                command,
                kind,
                disposition: SendDisposition::NeverWritten,
                timer,
                text: if waiting { Some(text.clone()) } else { None },
            },
        );
        if waiting {
            self.channels
                .get_mut(&channel_id)
                .ok_or(ClientError::ClosedClient)?
                .waiting
                .push_back(request_id);
        } else {
            batch.effects.push(HostEffect::Send {
                channel_id,
                request_id,
                text,
            });
        }
        Ok(request_id)
    }

    fn frame(
        &mut self,
        id: ChannelId,
        text: String,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        if !self.channels.contains_key(&id) {
            return Ok(());
        }
        if let Err(error) = validate_record(&text) {
            let reason = if error == FrameError::Oversized {
                TransportEnd::Oversized
            } else {
                TransportEnd::Malformed
            };
            return self.end_channel(id, reason, true, batch, clock);
        }
        let frame: ServerFrame = match serde_json::from_str(&text) {
            Ok(frame) => frame,
            Err(_) => return self.end_channel(id, TransportEnd::Malformed, true, batch, clock),
        };
        // Option<Value> loses explicit JSON null. Preserve key presence to
        // distinguish a nullable result from an absent/ambiguous response.
        // The direct struct decode above still rejects duplicate known keys.
        let envelope: EnvelopeShape = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) => return self.end_channel(id, TransportEnd::Malformed, true, batch, clock),
        };
        let has_result = envelope.result;
        let has_error = envelope.error;
        if let Some(push) = frame.push {
            if envelope.id || has_result || has_error {
                return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
            }
            return self.push(id, push, batch, clock);
        }
        let Some(request_id) = frame.id else {
            return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
        };
        if envelope.push || has_result == has_error || (has_error && frame.error.is_none()) {
            return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
        }
        // Protocol 6 has no successful acknowledgement for these operations.
        // A fabricated success must not consume the pending write or invoke
        // the shared decoder's intentionally unsupported result branch.
        if has_result
            && self
                .pending
                .get(&(id, request_id))
                .is_some_and(|pending| unacknowledged(&pending.command))
        {
            return Ok(());
        }
        let Some(pending) = self.take_pending(id, request_id, batch) else {
            // A terminal notification already completed locally. A late error
            // makes its fixed attachment unusable; never emit a second outcome.
            let is_unacked = self.channels.get(&id).is_some_and(|channel| {
                channel
                    .unacked_first
                    .is_some_and(|first| request_id >= first && request_id <= channel.unacked_last)
            });
            if is_unacked {
                if let Some(error) = frame.error {
                    return self.end_terminal(
                        id,
                        TerminalEnd::Failed {
                            reason: safe_error(error.code, error.message),
                        },
                        batch,
                        clock,
                    );
                }
            }
            // Duplicate/late results cannot finish a new operation.
            return Ok(());
        };
        if let Some(error) = frame.error {
            if let PendingKind::Discover { expected } = pending.kind {
                if matches!(
                    error.code.as_str(),
                    "unauthorized" | "authentication" | "revoked"
                ) {
                    return self.end_channel(id, TransportEnd::Authentication, true, batch, clock);
                }
                if error.code == "protocol" {
                    return self.end_channel(
                        id,
                        TransportEnd::ProtocolMismatch,
                        false,
                        batch,
                        clock,
                    );
                }
                // Old decoders and old native intermediaries use invalid/forbidden.
                // No mutation is retried or translated; only legacy views remain.
                return self.discovered(
                    id,
                    expected,
                    crew_client::features::FeatureAvailability {
                        session: None,
                        reason: Some(ClientReason {
                            details: None,
                            code: error.code,
                            message: "Feature discovery is unavailable on this connection.".into(),
                        }),
                    },
                    batch,
                    clock,
                );
            }
            let details = if matches!(pending.command, Command::AsideAsk(_)) {
                error
                    .data
                    .and_then(|data| serde_json::from_value(data).ok())
                    .map(crew_client::features::FeatureRejection::AsideAdmission)
            } else {
                None
            };
            let mut reason = safe_error(error.code, error.message);
            reason.details = details;
            if let PendingKind::Operation {
                scope,
                operation_id,
                lane,
            } = pending.kind
            {
                self.complete(
                    scope,
                    operation_id,
                    OperationOutcome::Rejected {
                        code: reason.code.clone(),
                        message: reason.message.clone(),
                        details: reason.details.clone(),
                    },
                    batch,
                    clock,
                )?;
                if matches!(lane, RequestLane::Terminal { .. }) {
                    self.end_terminal(id, TerminalEnd::Failed { reason }, batch, clock)?;
                } else if self
                    .channels
                    .get(&id)
                    .is_some_and(|channel| channel.purpose == ChannelPurpose::Action)
                {
                    self.end_channel(id, TransportEnd::Stopped, false, batch, clock)?;
                }
            } else {
                self.end_channel(id, TransportEnd::Malformed, true, batch, clock)?;
            }
            return Ok(());
        }
        let decoded = self.codec.decode_result(
            &pending.command,
            frame.result.unwrap_or(serde_json::Value::Null),
        );
        let result = match decoded {
            Ok(result) => result,
            Err(_) => {
                self.fail_pending(pending, TransportEnd::Malformed, batch, clock)?;
                return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
            }
        };
        match pending.kind {
            PendingKind::Discover { expected } => {
                let CommandResult::Feature(crew_client::features::FeatureResponse::FeatureSession(
                    session,
                )) = result
                else {
                    return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
                };
                if !crew_client::features::valid_negotiation(&session) {
                    return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
                }
                self.discovered(
                    id,
                    expected,
                    crew_client::features::FeatureAvailability {
                        session: Some(*session),
                        reason: None,
                    },
                    batch,
                    clock,
                )?;
            }
            PendingKind::Subscribe { snapshot_id } => {
                if result != CommandResult::Subscribed {
                    return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
                }
                self.request(
                    id,
                    Command::Snapshot {
                        messages: Some(self.config.snapshot_messages),
                        events: Some(self.config.snapshot_events),
                    },
                    PendingKind::Snapshot { snapshot_id },
                    false,
                    batch,
                    clock,
                )?;
            }
            PendingKind::Snapshot { snapshot_id } => {
                let CommandResult::Snapshot(snapshot) = result else {
                    return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
                };
                self.snapshot(id, snapshot_id, snapshot, batch, clock)?;
            }
            PendingKind::VerifyLane { expected } => {
                let CommandResult::Snapshot(snapshot) = result else {
                    return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
                };
                if snapshot.session.workspace != expected.workspace_id
                    || Some(snapshot.session.daemon_pid) != expected.daemon_pid
                    || Some(snapshot.session.daemon_started_at) != expected.daemon_started_at_ms
                {
                    self.end_channel(id, TransportEnd::GrantChanged, false, batch, clock)?;
                    self.resynchronize(batch, clock)?;
                } else {
                    self.lane_ready(id, expected, batch, clock)?;
                }
            }
            PendingKind::Listing => {
                let CommandResult::Workspaces(listing) = result else {
                    return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
                };
                self.receive_listing(id, None, listing, batch, clock)?;
            }
            PendingKind::Operation {
                scope,
                operation_id,
                lane,
            } => {
                let terminal = matches!(lane, RequestLane::Terminal { .. });
                if let Command::Attach { agent, .. } = &pending.command {
                    if !matches!(&result, CommandResult::Attached { agent: attached } if attached == agent)
                    {
                        self.complete(
                            scope,
                            operation_id,
                            OperationOutcome::OutcomeUnknown {
                                reason: TransportEnd::Malformed.reason(),
                            },
                            batch,
                            clock,
                        )?;
                        return self.end_terminal(
                            id,
                            TerminalEnd::Failed {
                                reason: TransportEnd::Malformed.reason(),
                            },
                            batch,
                            clock,
                        );
                    }
                    if let Some(channel) = self.channels.get_mut(&id) {
                        channel.terminal_live = true;
                    }
                }
                let result = match result {
                    CommandResult::Feature(response) => {
                        let Some(context) = self.channels[&id].features.session.clone() else {
                            return self.end_channel(
                                id,
                                TransportEnd::Malformed,
                                true,
                                batch,
                                clock,
                            );
                        };
                        CommandResult::FeatureScoped { context, response }
                    }
                    other => other,
                };
                if let (
                    Command::AsideRemove(q),
                    CommandResult::FeatureScoped {
                        response: crew_client::features::FeatureResponse::AsideMutation(r),
                        ..
                    },
                ) = (&pending.command, &result)
                {
                    if q.id == r.id && q.workspace_run_id == r.workspace_run_id {
                        self.purge_aside_bytes(&r.workspace_run_id, &r.id);
                    }
                }
                self.complete(
                    scope.clone(),
                    operation_id,
                    OperationOutcome::Succeeded { result },
                    batch,
                    clock,
                )?;
                if terminal {
                    let channel = self
                        .channels
                        .get_mut(&id)
                        .ok_or(ClientError::ClosedClient)?;
                    if let ChannelPurpose::Terminal { attachment_id } = channel.purpose {
                        let chunks = std::mem::take(&mut channel.buffered_terminal);
                        channel.terminal_bytes = 0;
                        for data in chunks {
                            terminal_chunks(batch, &scope, attachment_id, data, None);
                        }
                    }
                } else if self
                    .channels
                    .get(&id)
                    .is_some_and(|channel| channel.purpose == ChannelPurpose::Action)
                {
                    self.end_channel(id, TransportEnd::Stopped, false, batch, clock)?;
                }
            }
        }
        Ok(())
    }

    fn snapshot(
        &mut self,
        id: ChannelId,
        snapshot_id: u64,
        snapshot: Box<Snapshot>,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        let Some(mut context) = self.binding.clone() else {
            return Ok(());
        };
        if self.root != Some(id)
            || snapshot.session.daemon_pid == 0
            || Some(snapshot.session.daemon_pid) != context.scope.daemon_pid
            || snapshot.session.workspace.is_empty()
            || snapshot.session.workspace.len() > 4096
        {
            return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
        }
        let previous = context.scope.clone();
        if previous
            .daemon_started_at_ms
            .is_some_and(|started| started != snapshot.session.daemon_started_at)
        {
            return self.end_channel(id, TransportEnd::Malformed, false, batch, clock);
        }
        if context.mode == ConnectionMode::LocalOperator {
            context.scope.workspace_id = snapshot.session.workspace.clone();
        }
        context.scope.daemon_started_at_ms = Some(snapshot.session.daemon_started_at);
        if self
            .selection
            .as_ref()
            .and_then(|selection| selection.pinned_workspace.as_ref())
            .is_some_and(|pinned| *pinned != context.scope.workspace_id)
        {
            return self.end_channel(id, TransportEnd::GrantChanged, false, batch, clock);
        }
        if self.channels.get(&id).is_some_and(|channel| channel.ready) && context.scope != previous
        {
            return self.end_channel(id, TransportEnd::GrantChanged, true, batch, clock);
        }
        if context.scope != previous {
            self.input(
                ClientInput::Unbind {
                    scope: previous,
                    reason: ClientReason {
                        details: None,
                        code: "canonical_scope".into(),
                        message: "Verified workspace identity established.".into(),
                    },
                },
                clock,
                batch,
            )?;
            if context.mode == ConnectionMode::LocalOperator {
                context.grant.workspace_ids = vec![context.scope.workspace_id.clone()];
            }
            self.bind(&context, batch, clock)?;
            self.input(
                ClientInput::SnapshotStarted {
                    scope: context.scope.clone(),
                    operation_id: snapshot_id,
                },
                clock,
                batch,
            )?;
        }
        if let Some(selection) = &mut self.selection {
            selection.pinned_workspace = Some(context.scope.workspace_id.clone());
        }
        if let Some(channel) = self.channels.get_mut(&id) {
            channel.context = Some(context.clone());
            channel.ready = true;
        }
        self.binding = Some(context.clone());
        self.visible_scope = Some(context.scope.clone());
        let mut availability = self.channels[&id].features.clone();
        if let Some(session) = &mut availability.session {
            session.workspace_run_id = match snapshot.session.workspace_run_id.as_ref() {
                Some(run) => match crew_protocol::features::Id::new(run.clone()) {
                    Ok(run) => Some(run),
                    Err(_) => {
                        return self.end_channel(id, TransportEnd::Malformed, true, batch, clock)
                    }
                },
                None => None,
            };
        }
        self.channels
            .get_mut(&id)
            .ok_or(ClientError::ClosedClient)?
            .features = availability.clone();
        self.input(
            ClientInput::Features {
                scope: context.scope.clone(),
                availability,
            },
            clock,
            batch,
        )?;
        self.input(
            ClientInput::Snapshot {
                scope: context.scope,
                operation_id: snapshot_id,
                snapshot,
            },
            clock,
            batch,
        )?;
        self.workspace = WorkspaceStatus::Live;
        self.live = true;
        self.reconnect_count = 0;
        batch.changes.availability = true;
        Ok(())
    }

    fn push(
        &mut self,
        id: ChannelId,
        push: Push,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        if push == Push::Unknown {
            return Ok(());
        }
        let Some(channel) = self.channels.get(&id) else {
            return Ok(());
        };
        let purpose = channel.purpose;
        if let ChannelPurpose::Terminal { attachment_id } = purpose {
            let scope = channel.expected.clone().ok_or(ClientError::ClosedClient)?;
            match push {
                Push::Pty { data } => {
                    if data.len() > MAX_FRAME_BYTES {
                        return self.end_terminal(id, TerminalEnd::Overflow, batch, clock);
                    }
                    let bytes = match base64::engine::general_purpose::STANDARD.decode(data) {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            return self.end_terminal(
                                id,
                                TerminalEnd::Failed {
                                    reason: TransportEnd::Malformed.reason(),
                                },
                                batch,
                                clock,
                            )
                        }
                    };
                    let channel = self
                        .channels
                        .get_mut(&id)
                        .ok_or(ClientError::ClosedClient)?;
                    if channel.terminal_live {
                        terminal_chunks(batch, &scope, attachment_id, bytes, None);
                    } else if channel.buffered_terminal.len() >= 256
                        || bytes.len()
                            > (4 * MAX_FRAME_BYTES).saturating_sub(channel.terminal_bytes)
                    {
                        return self.end_terminal(id, TerminalEnd::Overflow, batch, clock);
                    } else {
                        channel.terminal_bytes += bytes.len();
                        channel.buffered_terminal.push_back(bytes);
                    }
                }
                Push::PtyClosed => self.end_terminal(id, TerminalEnd::Normal, batch, clock)?,
                Push::Shutdown => self.end_terminal(id, TerminalEnd::Disconnect, batch, clock)?,
                _ => self.end_terminal(
                    id,
                    TerminalEnd::Failed {
                        reason: TransportEnd::Malformed.reason(),
                    },
                    batch,
                    clock,
                )?,
            }
            return Ok(());
        }
        if purpose == ChannelPurpose::Index {
            if let Push::Workspaces(listing) = push {
                return self.receive_listing(id, None, listing, batch, clock);
            }
            return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
        }
        if purpose != ChannelPurpose::Workspace
            || matches!(
                push,
                Push::Pty { .. }
                    | Push::PtyClosed
                    | Push::Deliver { .. }
                    | Push::Compact { .. }
                    | Push::Workspaces(_)
                    | Push::Summary(_)
            )
        {
            return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
        }
        let scope = channel
            .context
            .as_ref()
            .map(|context| context.scope.clone())
            .ok_or(ClientError::ClosedClient)?;
        let feature = match &push {
            Push::EpicBoardChanged(_) => Some(crew_protocol::features::Feature::Epics),
            Push::Aside(_) => Some(crew_protocol::features::Feature::Asides),
            Push::DocsChanged(_) => Some(crew_protocol::features::Feature::Docs),
            Push::UsageChanged(_) => Some(crew_protocol::features::Feature::UsageMetrics),
            _ => None,
        };
        if feature.is_some_and(|f| !channel.features.supports(f)) {
            return self.end_channel(id, TransportEnd::Malformed, true, batch, clock);
        }
        if let Push::Aside(change) = &push {
            let known = matches!(self.core.view(ViewQuery::FeatureWatches {feature:crew_protocol::features::Feature::Asides},clock),Ok(ViewData{payload:ViewPayload::FeatureWatches(ref watches),..}) if watches.iter().any(|w|w.watch_id==change.watch_id&&w.epoch==change.aside_epoch));
            if known {
                for gone in change
                    .gone
                    .iter()
                    .filter(|g| g.cause == crew_protocol::asides::AsideGoneCause::Removed)
                {
                    self.purge_aside_bytes(&change.workspace_run_id, &gone.id);
                }
            }
        }
        let shutdown = push == Push::Shutdown;
        self.input(ClientInput::Push { scope, push }, clock, batch)?;
        if shutdown {
            self.end_channel(id, TransportEnd::Stopped, false, batch, clock)?;
            self.workspace = WorkspaceStatus::Stopped;
        }
        Ok(())
    }

    fn purge_aside_bytes(
        &mut self,
        run: &crew_protocol::features::Id,
        id: &crew_protocol::features::Id,
    ) {
        for pending in self.pending.values_mut() {
            if let Command::AsideAsk(q) = &mut pending.command {
                if &q.workspace_run_id == run && &q.request_id == id {
                    q.question = Default::default();
                    pending.text = None;
                }
            }
        }
    }

    fn sent(
        &mut self,
        id: ChannelId,
        request_id: u64,
        disposition: SendDisposition,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        let Some(pending) = self.pending.get_mut(&(id, request_id)) else {
            return Ok(());
        };
        match disposition {
            SendDisposition::NeverWritten => {
                return self.end_channel(id, TransportEnd::Closed, true, batch, clock)
            }
            SendDisposition::WriteStarted => {
                if pending.disposition == SendDisposition::NeverWritten {
                    pending.disposition = disposition;
                }
            }
            SendDisposition::FullyWritten => {
                pending.disposition = disposition;
                if unacknowledged(&pending.command) {
                    let pending = self
                        .take_pending(id, request_id, batch)
                        .ok_or(ClientError::ClosedClient)?;
                    if let PendingKind::Operation {
                        scope,
                        operation_id,
                        lane: RequestLane::Terminal { attachment_id },
                    } = pending.kind
                    {
                        let channel = self
                            .channels
                            .get_mut(&id)
                            .ok_or(ClientError::ClosedClient)?;
                        channel.unacked_first.get_or_insert(request_id);
                        channel.unacked_last = channel.unacked_last.max(request_id);
                        self.complete(
                            scope,
                            operation_id,
                            OperationOutcome::SentUnacknowledged { attachment_id },
                            batch,
                            clock,
                        )?;
                    } else {
                        return self.end_channel(id, TransportEnd::Malformed, false, batch, clock);
                    }
                }
            }
        }
        Ok(())
    }

    fn receive_listing(
        &mut self,
        id: ChannelId,
        request_id: Option<u64>,
        listing: Workspaces,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        let Some(channel) = self.channels.get(&id) else {
            return Ok(());
        };
        if channel.purpose != ChannelPurpose::Index || self.index_attempt != Some(channel.attempt) {
            return Ok(());
        }
        let attempt = channel.attempt;
        if let Some(request_id) = request_id {
            self.take_pending(id, request_id, batch);
        }
        let endpoint_id = self
            .selection
            .as_ref()
            .ok_or(ClientError::ClosedClient)?
            .endpoint_id
            .clone();
        if let Some(binding) = &self.binding {
            if !listing.joined.iter().any(|entry| {
                entry.id == binding.scope.workspace_id
                    && Some(entry.generation) == binding.scope.generation
            }) {
                self.retire_workspace(TransportEnd::GrantChanged, batch, clock)?;
                self.workspace = WorkspaceStatus::AccessRevoked;
            }
        }
        self.listing = Some(listing.clone());
        self.input(
            ClientInput::Listing {
                endpoint_id,
                attempt,
                listing,
            },
            clock,
            batch,
        )
    }

    fn dispatch(
        &mut self,
        scope: ClientScope,
        operation_id: u64,
        lane: RequestLane,
        command: Command,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        if !self.live
            || !self.active
            || self
                .binding
                .as_ref()
                .is_none_or(|context| context.scope != scope)
        {
            return self.complete(
                scope,
                operation_id,
                OperationOutcome::NotSent {
                    reason: ClientReason {
                        details: None,
                        code: "not_live".into(),
                        message: "A fresh authenticated snapshot is required.".into(),
                    },
                },
                batch,
                clock,
            );
        }
        let chosen = match lane.clone() {
            RequestLane::Document { lease } => {
                let exact = self.channels.iter().find_map(|(id, c)| {
                    (c.ready
                        && matches!(c.purpose, ChannelPurpose::Workspace | ChannelPurpose::Read)
                        && c.context.as_ref().is_some_and(|ctx| ctx.scope == scope)
                        && c.features
                            .session
                            .as_ref()
                            .is_some_and(|f| f.connection_epoch == lease.connection_epoch)
                        && c.features.supports(crew_protocol::features::Feature::Docs))
                    .then_some(*id)
                });
                if exact.is_none() {
                    return self.not_sent(scope, operation_id, "snapshot_expired", batch, clock);
                }
                exact
            }
            RequestLane::State => self.root,
            RequestLane::Read => self.channels.iter().find_map(|(id, channel)| {
                (channel.purpose == ChannelPurpose::Read
                    && channel.expected.as_ref() == Some(&scope))
                .then_some(*id)
            }),
            RequestLane::Action => None,
            RequestLane::Terminal { attachment_id } => {
                let existing = self.channels.iter().find_map(|(id, channel)| {
                    (channel.purpose == ChannelPurpose::Terminal { attachment_id }
                        && channel.expected.as_ref() == Some(&scope))
                    .then_some(*id)
                });
                if matches!(command, Command::Attach { .. }) {
                    if operation_id != attachment_id || existing.is_some() {
                        return self.not_sent(
                            scope,
                            operation_id,
                            "invalid_attachment",
                            batch,
                            clock,
                        );
                    }
                    None
                } else {
                    if !unacknowledged(&command)
                        || existing.is_none_or(|id| !self.channels[&id].terminal_live)
                    {
                        return self.not_sent(
                            scope,
                            operation_id,
                            "attachment_unavailable",
                            batch,
                            clock,
                        );
                    }
                    existing
                }
            }
        };
        let purpose = match &lane {
            RequestLane::State => ChannelPurpose::Workspace,
            RequestLane::Read => ChannelPurpose::Read,
            RequestLane::Document { .. } => {
                self.channels[&chosen.expect("exact lease channel")].purpose
            }
            RequestLane::Action => ChannelPurpose::Action,
            RequestLane::Terminal { attachment_id } => ChannelPurpose::Terminal {
                attachment_id: *attachment_id,
            },
        };
        let channel_id = match chosen {
            Some(id) => id,
            None => {
                match self.open_channel(purpose, scope.attempt, Some(scope.clone()), batch, clock) {
                    Ok(id) => id,
                    Err(_) => {
                        return self.not_sent(
                            scope,
                            operation_id,
                            "transport_capacity",
                            batch,
                            clock,
                        )
                    }
                }
            }
        };
        if self.channels[&channel_id].ready
            && command
                .required_feature()
                .is_some_and(|f| !self.channels[&channel_id].features.supports(f))
        {
            return self.not_sent(scope, operation_id, "unsupported_feature", batch, clock);
        }
        let kind = PendingKind::Operation {
            scope: scope.clone(),
            operation_id,
            lane,
        };
        if self
            .request(channel_id, command, kind, true, batch, clock)
            .is_err()
        {
            self.not_sent(scope, operation_id, "transport_capacity", batch, clock)?;
            if matches!(
                purpose,
                ChannelPurpose::Action | ChannelPurpose::Terminal { .. }
            ) {
                self.end_channel(channel_id, TransportEnd::Stopped, false, batch, clock)?;
            }
        }
        Ok(())
    }

    fn not_sent(
        &mut self,
        scope: ClientScope,
        operation_id: u64,
        code: &str,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        self.complete(
            scope,
            operation_id,
            OperationOutcome::NotSent {
                reason: ClientReason {
                    details: None,
                    code: code.into(),
                    message: "The request was not sent.".into(),
                },
            },
            batch,
            clock,
        )
    }

    fn complete(
        &mut self,
        scope: ClientScope,
        operation_id: u64,
        outcome: OperationOutcome,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        // Old completions cannot alter a newer core instance or its draft.
        if self
            .binding
            .as_ref()
            .is_none_or(|context| context.scope != scope)
        {
            return Ok(());
        }
        // Core alone decides whether a wire outcome is final, held for a
        // permitted identified-message retry, or obsolete. It supplies the
        // final typed notice through ClientUpdate.outcomes.
        self.input(
            ClientInput::OperationFinished {
                scope,
                operation_id,
                outcome,
            },
            clock,
            batch,
        )
    }

    fn fail_pending(
        &mut self,
        pending: Pending,
        reason: TransportEnd,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        if let PendingKind::Operation {
            scope,
            operation_id,
            ..
        } = pending.kind
        {
            let outcome = if pending.disposition == SendDisposition::NeverWritten {
                OperationOutcome::NotSent {
                    reason: reason.reason(),
                }
            } else {
                OperationOutcome::OutcomeUnknown {
                    reason: reason.reason(),
                }
            };
            self.complete(scope, operation_id, outcome, batch, clock)?;
        }
        Ok(())
    }

    fn take_pending(
        &mut self,
        id: ChannelId,
        request_id: u64,
        batch: &mut Batch,
    ) -> Option<Pending> {
        let pending = self.pending.remove(&(id, request_id))?;
        self.cancel_timer(pending.timer, batch);
        if let Some(channel) = self.channels.get_mut(&id) {
            channel.waiting.retain(|request| *request != request_id);
        }
        Some(pending)
    }

    fn end_terminal(
        &mut self,
        id: ChannelId,
        reason: TerminalEnd,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        let Some(channel) = self.channels.get(&id) else {
            return Ok(());
        };
        let ChannelPurpose::Terminal { attachment_id } = channel.purpose else {
            return Ok(());
        };
        let scope = channel.expected.clone().ok_or(ClientError::ClosedClient)?;
        let requests: Vec<_> = self
            .pending
            .keys()
            .filter(|(channel, _)| *channel == id)
            .copied()
            .collect();
        for (_, request_id) in requests {
            if let Some(pending) = self.take_pending(id, request_id, batch) {
                self.fail_pending(pending, TransportEnd::Closed, batch, clock)?;
            }
        }
        self.cancel_open_timer(id, batch);
        self.channels.remove(&id);
        batch.effects.push(HostEffect::Close { channel_id: id });
        // Update core lifetime before exposing the end to a native renderer.
        self.input(
            ClientInput::AttachmentEnded {
                scope: scope.clone(),
                attachment_id,
                reason: reason.clone(),
            },
            clock,
            batch,
        )?;
        batch.terminal.push(TerminalChunk {
            scope,
            attachment_id,
            data: Vec::new(),
            ended: Some(reason),
        });
        Ok(())
    }

    fn end_channel(
        &mut self,
        id: ChannelId,
        reason: TransportEnd,
        reconnect: bool,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        let Some(channel) = self.channels.get(&id) else {
            return Ok(());
        };
        if matches!(channel.purpose, ChannelPurpose::Terminal { .. }) {
            let terminal_end = match reason {
                TransportEnd::Overflow => TerminalEnd::Overflow,
                TransportEnd::Revoked | TransportEnd::GrantChanged => TerminalEnd::Revocation,
                TransportEnd::Stopped => TerminalEnd::Detach,
                _ => TerminalEnd::Disconnect,
            };
            return self.end_terminal(id, terminal_end, batch, clock);
        }
        let root = self.root == Some(id);
        let index = self.index == Some(id);
        let attempt = channel.attempt;
        let failed_action = channel.purpose == ChannelPurpose::Action
            && reason == TransportEnd::Closed
            && self.pending.keys().any(|(channel, _)| *channel == id);
        let feature_connection = channel
            .context
            .as_ref()
            .zip(channel.features.session.as_ref())
            .map(|(context, features)| (context.scope.clone(), features.connection_epoch.clone()));
        let requests: Vec<_> = self
            .pending
            .keys()
            .filter(|(channel, _)| *channel == id)
            .copied()
            .collect();
        for (_, request_id) in requests {
            if let Some(pending) = self.take_pending(id, request_id, batch) {
                self.fail_pending(pending, reason, batch, clock)?;
            }
        }
        self.cancel_open_timer(id, batch);
        self.channels.remove(&id);
        batch.effects.push(HostEffect::Close { channel_id: id });
        if let Some((scope, connection_epoch)) = feature_connection {
            self.input(
                ClientInput::FeatureChannelClosed {
                    scope,
                    connection_epoch,
                },
                clock,
                batch,
            )?;
        }
        if failed_action {
            // The state lane can still be connected when a mutation lane
            // dies. A fresh state attempt/barrier establishes the only context
            // in which Core may authorize an identified-message retry.
            if let Some(root) = self.root {
                return self.end_channel(root, reason, true, batch, clock);
            }
        }
        // A scoped gateway refuses a workspace route generically when only that
        // workspace's grant is gone. Its separate index still decides whether
        // the device itself is authorized, so the route alone never signs out.
        let route_refused = self.index.is_some()
            && self.selection.as_ref().is_some_and(|value| value.indexed)
            && matches!(
                reason,
                TransportEnd::Authentication | TransportEnd::Permission
            );
        if root {
            self.root = None;
            self.retire_workspace(reason, batch, clock)?;
            self.workspace = match reason {
                TransportEnd::ProtocolMismatch => WorkspaceStatus::Incompatible,
                TransportEnd::Revoked | TransportEnd::GrantChanged => {
                    WorkspaceStatus::AccessRevoked
                }
                _ if route_refused => WorkspaceStatus::AccessRevoked,
                _ => WorkspaceStatus::Reconnecting,
            };
        }
        if index {
            self.index = None;
            self.index_attempt = None;
            if matches!(
                reason,
                TransportEnd::Revoked
                    | TransportEnd::GrantChanged
                    | TransportEnd::Authentication
                    | TransportEnd::CredentialLocked
                    | TransportEnd::CredentialUnavailable
                    | TransportEnd::TlsTrust
                    | TransportEnd::Permission
            ) {
                self.listing = None;
            }
            self.retire_workspace(reason, batch, clock)?;
        }
        if root || index {
            if self.index.is_none() {
                self.endpoint = match reason {
                    TransportEnd::ProtocolMismatch => EndpointStatus::Incompatible,
                    TransportEnd::Revoked
                    | TransportEnd::Authentication
                    | TransportEnd::MissingCredential
                    | TransportEnd::CredentialLocked
                    | TransportEnd::CredentialUnavailable
                    | TransportEnd::TlsTrust
                    | TransportEnd::Permission
                    | TransportEnd::AgentContext => EndpointStatus::SignedOut,
                    TransportEnd::Stopped => EndpointStatus::Disconnected,
                    _ => EndpointStatus::Reconnecting,
                };
                if let Some(selection) = &self.selection {
                    self.input(
                        ClientInput::EndpointChanged {
                            endpoint_id: selection.endpoint_id.clone(),
                            attempt,
                            status: self.endpoint,
                        },
                        clock,
                        batch,
                    )?;
                }
            }
            batch.reason = Some(reason.reason());
            if reconnect
                && self.config.reconnect
                && self.active
                && matches!(
                    reason,
                    TransportEnd::Closed
                        | TransportEnd::Timeout
                        | TransportEnd::Unreachable
                        | TransportEnd::Overflow
                        | TransportEnd::Malformed
                        | TransportEnd::Oversized
                )
            {
                self.reconnect_count = self.reconnect_count.saturating_add(1);
                let delay = 1000_u64
                    .saturating_mul(1_u64 << self.reconnect_count.min(5).saturating_sub(1))
                    .min(30_000);
                self.schedule(
                    TimerOwner::Reconnect,
                    attempt,
                    clock.monotonic_ms.saturating_add(delay),
                    batch,
                )?;
            }
        } else if matches!(
            reason,
            TransportEnd::Overflow
                | TransportEnd::Malformed
                | TransportEnd::Oversized
                | TransportEnd::GrantChanged
                | TransportEnd::Revoked
        ) {
            self.resynchronize(batch, clock)?;
        }
        Ok(())
    }

    fn retire_workspace(
        &mut self,
        reason: TransportEnd,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        // Remove root identity first, preventing recursive root retirement.
        self.root = None;
        let channels: Vec<_> = self
            .channels
            .iter()
            .filter_map(|(id, channel)| (channel.purpose != ChannelPurpose::Index).then_some(*id))
            .collect();
        for id in channels {
            self.end_channel(id, reason, false, batch, clock)?;
        }
        if let Some(binding) = self.binding.take() {
            self.input(
                ClientInput::Unbind {
                    scope: binding.scope,
                    reason: reason.reason(),
                },
                clock,
                batch,
            )?;
        }
        self.live = false;
        // Core cancels its policy timers explicitly. Held message retries must
        // still expire while unbound, using their original fixed deadline.
        Ok(())
    }

    fn retire_all(
        &mut self,
        reason: TransportEnd,
        reconnect: bool,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        self.root = None;
        self.index = None;
        self.index_attempt = None;
        self.retire_workspace(reason, batch, clock)?;
        let channels: Vec<_> = self.channels.keys().copied().collect();
        for id in channels {
            self.end_channel(id, reason, reconnect, batch, clock)?;
        }
        let timers: Vec<_> = self
            .timers
            .iter()
            .filter_map(|(id, timer)| {
                (reason != TransportEnd::Closed || !matches!(timer.owner, TimerOwner::Core(_)))
                    .then_some(*id)
            })
            .collect();
        for id in timers {
            self.cancel_timer(id, batch);
        }
        Ok(())
    }

    fn resynchronize(&mut self, batch: &mut Batch, clock: ClockSample) -> Result<(), ClientError> {
        let (Some(root), Some(context)) = (self.root, self.binding.clone()) else {
            return Ok(());
        };
        if self.pending.iter().any(|((id, _), pending)| {
            *id == root
                && matches!(
                    pending.kind,
                    PendingKind::Subscribe { .. } | PendingKind::Snapshot { .. }
                )
        }) {
            return Ok(());
        }
        self.live = false;
        self.workspace = WorkspaceStatus::Connecting;
        let snapshot_id = allocate(&mut self.next_snapshot)?;
        self.input(
            ClientInput::SnapshotStarted {
                scope: context.scope,
                operation_id: snapshot_id,
            },
            clock,
            batch,
        )?;
        self.request(
            root,
            Command::Snapshot {
                messages: Some(self.config.snapshot_messages),
                events: Some(self.config.snapshot_events),
            },
            PendingKind::Snapshot { snapshot_id },
            false,
            batch,
            clock,
        )?;
        Ok(())
    }

    fn close_attachment(
        &mut self,
        scope: ClientScope,
        operation_id: u64,
        attachment_id: u64,
        batch: &mut Batch,
        clock: ClockSample,
    ) -> Result<(), ClientError> {
        if self
            .binding
            .as_ref()
            .is_none_or(|binding| binding.scope != scope)
        {
            return Ok(());
        }
        let channel = self.channels.iter().find_map(|(id, channel)| {
            (channel.purpose == ChannelPurpose::Terminal { attachment_id }
                && channel.expected.as_ref() == Some(&scope))
            .then_some(*id)
        });
        if let Some(channel) = channel {
            self.end_terminal(channel, TerminalEnd::Detach, batch, clock)?;
            self.complete(
                scope,
                operation_id,
                OperationOutcome::ClosedLocally { attachment_id },
                batch,
                clock,
            )?;
        }
        // A duplicated close effect produces no second completion. Unknown or
        // closed *intents* are rejected by the real planner before dispatch.
        Ok(())
    }

    fn schedule(
        &mut self,
        owner: TimerOwner,
        attempt: u64,
        deadline_ms: u64,
        batch: &mut Batch,
    ) -> Result<u64, ClientError> {
        if self.timers.len() >= 256 {
            return Err(capacity());
        }
        let id = allocate(&mut self.next_timer)?;
        self.timers.insert(
            id,
            TimerKey {
                owner,
                attempt,
                deadline_ms,
            },
        );
        batch.effects.push(HostEffect::Schedule {
            timer_id: id,
            deadline_ms,
        });
        Ok(id)
    }

    fn cancel_timer(&mut self, id: u64, batch: &mut Batch) {
        if self.timers.remove(&id).is_some() {
            batch.effects.push(HostEffect::CancelTimer { timer_id: id });
        }
    }

    fn cancel_open_timer(&mut self, id: ChannelId, batch: &mut Batch) {
        if let Some(timer) = self
            .channels
            .get_mut(&id)
            .and_then(|channel| channel.open_timer.take())
        {
            self.cancel_timer(timer, batch);
        }
    }

    fn wake(&mut self, id: u64, batch: &mut Batch, clock: ClockSample) -> Result<(), ClientError> {
        let Some(timer) = self.timers.get(&id).copied() else {
            return Ok(());
        };
        if clock.monotonic_ms < timer.deadline_ms {
            batch.effects.push(HostEffect::Schedule {
                timer_id: id,
                deadline_ms: timer.deadline_ms,
            });
            return Ok(());
        }
        self.timers.remove(&id);
        match timer.owner {
            TimerOwner::Open(channel) => {
                if self
                    .channels
                    .get(&channel)
                    .is_some_and(|channel| channel.attempt == timer.attempt)
                {
                    self.end_channel(channel, TransportEnd::Timeout, true, batch, clock)?;
                }
            }
            TimerOwner::Request(channel, request) => {
                if self
                    .channels
                    .get(&channel)
                    .is_some_and(|channel| channel.attempt == timer.attempt)
                    && self.pending.contains_key(&(channel, request))
                {
                    self.end_channel(channel, TransportEnd::Timeout, true, batch, clock)?;
                }
            }
            TimerOwner::Core(timer_id) => {
                self.input(
                    ClientInput::Wake {
                        timer_id,
                        monotonic_ms: clock.monotonic_ms,
                    },
                    clock,
                    batch,
                )?;
            }
            TimerOwner::Reconnect => {
                if self.root.is_none() && (self.index.is_some() || self.channels.is_empty()) {
                    self.start_selection(batch, clock)?;
                }
            }
        }
        Ok(())
    }

    fn input(
        &mut self,
        input: ClientInput,
        clock: ClockSample,
        batch: &mut Batch,
    ) -> Result<(), ClientError> {
        let update = self.core.apply(input, clock)?;
        self.absorb(update, batch)
    }

    fn absorb(&mut self, update: ClientUpdate, batch: &mut Batch) -> Result<(), ClientError> {
        if update.revision < self.revision || update.changes.revision != update.revision {
            return Err(ClientError::InternalFailure {
                code: "revision_regression".into(),
            });
        }
        self.revision = update.revision;
        merge_changes(&mut batch.changes, update.changes);
        batch.outcomes.extend(update.outcomes);
        batch.core_effects.extend(update.effects);
        Ok(())
    }

    fn finish(&mut self, mut batch: Batch, clock: ClockSample) -> Result<HostUpdate, ClientError> {
        let mut processed = 0;
        while let Some(effect) = batch.core_effects.pop_front() {
            processed += 1;
            if processed > 1024 {
                return Err(ClientError::InternalFailure {
                    code: "effect_limit".into(),
                });
            }
            match effect {
                ClientEffect::Request {
                    scope,
                    operation_id,
                    lane,
                    command,
                } => self.dispatch(scope, operation_id, lane, command, &mut batch, clock)?,
                ClientEffect::Schedule {
                    timer_id,
                    deadline_ms,
                } => {
                    let attempt = self
                        .binding
                        .as_ref()
                        .map_or(0, |binding| binding.scope.attempt);
                    let old: Vec<_> = self
                        .timers
                        .iter()
                        .filter_map(|(id, timer)| {
                            (timer.owner == TimerOwner::Core(timer_id)).then_some(*id)
                        })
                        .collect();
                    for id in old {
                        self.cancel_timer(id, &mut batch);
                    }
                    self.schedule(TimerOwner::Core(timer_id), attempt, deadline_ms, &mut batch)?;
                }
                ClientEffect::CancelTimer { timer_id } => {
                    let old: Vec<_> = self
                        .timers
                        .iter()
                        .filter_map(|(id, timer)| {
                            (timer.owner == TimerOwner::Core(timer_id)).then_some(*id)
                        })
                        .collect();
                    for id in old {
                        self.cancel_timer(id, &mut batch);
                    }
                }
                ClientEffect::Resynchronize { scope } => {
                    if self
                        .binding
                        .as_ref()
                        .is_some_and(|binding| binding.scope == scope)
                    {
                        self.resynchronize(&mut batch, clock)?;
                    }
                }
                ClientEffect::Notice { code, .. } => {
                    batch.reason = Some(ClientReason {
                        details: None,
                        code,
                        message: "Client state changed.".into(),
                    });
                }
                ClientEffect::CloseAttachment {
                    scope,
                    operation_id,
                    attachment_id,
                } => {
                    self.close_attachment(scope, operation_id, attachment_id, &mut batch, clock)?
                }
            }
        }
        batch.changes.revision = self.revision;
        Ok(HostUpdate {
            notice: ClientNotice {
                scope: self.visible_scope.clone(),
                revision: self.revision,
                changes: batch.changes,
                endpoint: self.endpoint,
                workspace: self.workspace,
                reason: batch.reason,
                outcomes: batch.outcomes,
            },
            effects: batch.effects,
            terminal: batch.terminal,
        })
    }

    fn empty_update(&self) -> HostUpdate {
        HostUpdate {
            notice: ClientNotice {
                scope: self.visible_scope.clone(),
                revision: self.revision,
                changes: ChangeSet {
                    revision: self.revision,
                    ..ChangeSet::default()
                },
                endpoint: self.endpoint,
                workspace: self.workspace,
                reason: None,
                outcomes: Vec::new(),
            },
            effects: Vec::new(),
            terminal: Vec::new(),
        }
    }
}

fn allocate(next: &mut u64) -> Result<u64, ClientError> {
    let id = *next;
    *next = next
        .checked_add(1)
        .ok_or_else(|| ClientError::InternalFailure {
            code: "identity_exhausted".into(),
        })?;
    Ok(id)
}

// This second pass inspects envelope key presence without copying the payload.
// Explicit null differs from a missing field; IgnoredAny preserves that fact.
#[derive(Deserialize)]
struct EnvelopeShape {
    #[serde(default, deserialize_with = "present")]
    id: bool,
    #[serde(default, deserialize_with = "present")]
    result: bool,
    #[serde(default, deserialize_with = "present")]
    error: bool,
    #[serde(default, deserialize_with = "present")]
    push: bool,
}

fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Ok(true)
}

fn capacity() -> ClientError {
    ClientError::InternalFailure {
        code: "transport_capacity".into(),
    }
}

fn terminal_chunks(
    batch: &mut Batch,
    scope: &ClientScope,
    attachment_id: u64,
    data: Vec<u8>,
    ended: Option<TerminalEnd>,
) {
    if data.is_empty() {
        batch.terminal.push(TerminalChunk {
            scope: scope.clone(),
            attachment_id,
            data,
            ended,
        });
        return;
    }
    let count = data.len().div_ceil(MAX_BODY_BYTES);
    for (ordinal, bytes) in data.chunks(MAX_BODY_BYTES).enumerate() {
        batch.terminal.push(TerminalChunk {
            scope: scope.clone(),
            attachment_id,
            data: bytes.to_vec(),
            ended: if ordinal + 1 == count {
                ended.clone()
            } else {
                None
            },
        });
    }
}

fn unacknowledged(command: &Command) -> bool {
    matches!(
        command,
        Command::AttachInput { .. } | Command::AttachResize { .. }
    )
}

fn safe_error(code: String, message: String) -> ClientReason {
    let code = if code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        code
    } else {
        "server_error".into()
    };
    let message = if message.len() <= 4096
        && !message
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        message
    } else {
        "The host refused the request.".into()
    };
    ClientReason {
        details: None,
        code,
        message,
    }
}

fn merge_changes(target: &mut ChangeSet, source: ChangeSet) {
    target.revision = source.revision;
    for id in source.agents {
        if !target.agents.contains(&id) {
            target.agents.push(id);
        }
    }
    for id in source.tasks {
        if !target.tasks.contains(&id) {
            target.tasks.push(id);
        }
    }
    for id in source.removed {
        if !target.removed.contains(&id) {
            target.removed.push(id);
        }
    }
    target.session |= source.session;
    target.questions |= source.questions;
    target.messages |= source.messages;
    target.events |= source.events;
    target.usage |= source.usage;
    target.availability |= source.availability;
    target.pending |= source.pending;
    target.reset |= source.reset;
}
