use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use crew_client::api::{
    ClientScope, ConnectionContext, ConnectionMode, Grant, GrantTier, PeerInfo, SnapshotMode,
};
use crew_protocol::native::*;
use crew_protocol::rpc::{HelloInfo, Role, Workspaces};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::queue::ChannelSink;
use crate::TransportEnd;

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    endpoint: EndpointId,
    epoch: HexBytes<16>,
    device: DeviceId,
    generation: u64,
    expiry: i64,
}

impl From<&GatewayAuthority> for Identity {
    fn from(authority: &GatewayAuthority) -> Self {
        Self {
            endpoint: authority.endpoint_id,
            epoch: authority.endpoint_epoch,
            device: authority.device_id,
            generation: authority.device_generation,
            expiry: authority.credential_expires_at,
        }
    }
}

struct Active {
    workspace: Option<WorkspaceId>,
    stop: watch::Sender<Option<TransportEnd>>,
    sink: ChannelSink,
    admitted: bool,
    valid: bool,
    grant_generation: Option<u64>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    retired: bool,
    identity: Option<Identity>,
    grants: BTreeMap<WorkspaceId, WorkspaceGrant>,
    listing_rev: Option<u64>,
    listing_grants: BTreeMap<WorkspaceId, WorkspaceGrant>,
    channels: BTreeMap<u64, Active>,
}

/// One profile/key's device and grant fence spans every session/channel. No
/// callback or socket operation runs while its short metadata mutex is held.
#[derive(Clone, Default)]
pub(super) struct Fence(Arc<Mutex<State>>);

impl Fence {
    pub fn register(
        &self,
        workspace: Option<WorkspaceId>,
        stop: watch::Sender<Option<TransportEnd>>,
        sink: ChannelSink,
    ) -> Result<u64, TransportEnd> {
        let mut state = self.0.lock().map_err(|_| TransportEnd::Stopped)?;
        if state.retired || state.channels.len() >= 64 {
            return Err(TransportEnd::Permission);
        }
        state.next_id = state.next_id.checked_add(1).ok_or(TransportEnd::Overflow)?;
        let id = state.next_id;
        state.channels.insert(
            id,
            Active {
                workspace,
                stop,
                sink,
                admitted: false,
                valid: true,
                grant_generation: None,
            },
        );
        Ok(id)
    }

    pub fn unregister(&self, id: u64) {
        if let Ok(mut state) = self.0.lock() {
            state.channels.remove(&id);
        }
    }

    pub fn admit(
        &self,
        id: u64,
        authority: &GatewayAuthority,
        grant: Option<&WorkspaceGrant>,
    ) -> Result<(), TransportEnd> {
        self.authority(id, authority)?;
        let victims = {
            let mut state = self.0.lock().map_err(|_| TransportEnd::Stopped)?;
            if state.retired {
                return Err(TransportEnd::Revoked);
            }
            let active = state.channels.get(&id).ok_or(TransportEnd::Closed)?;
            if !active.valid
                || active.stop.borrow().is_some()
                || state.identity.as_ref() != Some(&Identity::from(authority))
            {
                return Err(TransportEnd::Closed);
            }
            let conflict = grant.and_then(|grant| {
                state
                    .grants
                    .get(&grant.workspace_id)
                    .filter(|old| *old != grant)
                    .map(|old| old.grant_generation)
            });
            let victims = if let Some(previous) = conflict {
                let grant = grant.ok_or(TransportEnd::Malformed)?;
                if grant.grant_generation <= previous {
                    return Err(TransportEnd::GrantChanged);
                }
                // Fresh route/grant proof may beat the Index update. Fence old
                // channels before this Opened event, retaining this fresh open.
                // A secondary lane still fails driver's full-grant comparison.
                cancel_grant_channels(
                    &mut state,
                    grant.workspace_id,
                    Some(grant.grant_generation),
                    Some(id),
                )
            } else {
                Vec::new()
            };
            if let Some(grant) = grant {
                state.grants.insert(grant.workspace_id, grant.clone());
            }
            let active = state.channels.get_mut(&id).ok_or(TransportEnd::Closed)?;
            active.admitted = true;
            active.grant_generation = grant.map(|grant| grant.grant_generation);
            victims
        };
        notify(victims, TransportEnd::GrantChanged);
        Ok(())
    }

    fn authority(
        &self,
        registration: u64,
        authority: &GatewayAuthority,
    ) -> Result<(), TransportEnd> {
        let identity = Identity::from(authority);
        let changed = {
            let mut state = self.0.lock().map_err(|_| TransportEnd::Stopped)?;
            if state.retired {
                return Err(TransportEnd::Revoked);
            }
            if !state
                .channels
                .get(&registration)
                .is_some_and(|active| active.valid && active.stop.borrow().is_none())
            {
                return Err(TransportEnd::Closed);
            }
            let changed = state
                .identity
                .as_ref()
                .filter(|old| **old != identity)
                .map(|old| {
                    if old.epoch != identity.epoch {
                        TransportEnd::Closed
                    } else {
                        TransportEnd::Revoked
                    }
                });
            state.identity = Some(identity);
            if changed.is_some() {
                state.grants.clear();
                state.listing_rev = None;
                state.listing_grants.clear();
            }
            let victims = if changed.is_some() {
                cancel_channels(&mut state, None, None)
            } else {
                Vec::new()
            };
            (changed, victims)
        };
        if let (Some(reason), victims) = changed {
            notify(victims, reason);
            return Err(reason);
        }
        Ok(())
    }

    pub fn admitted(&self, id: u64) -> bool {
        self.0.lock().is_ok_and(|state| {
            !state.retired
                && state.channels.get(&id).is_some_and(|channel| {
                    channel.valid && channel.admitted && channel.stop.borrow().is_none()
                })
        })
    }

    pub fn listing(
        &self,
        registration: u64,
        value: &GatewayListing<Workspaces>,
    ) -> Result<(), TransportEnd> {
        self.authority(registration, &value.authority)?;
        let grants: BTreeMap<_, _> = value
            .grants
            .iter()
            .map(|g| (g.workspace_id, g.clone()))
            .collect();
        let victims = {
            let mut state = self.0.lock().map_err(|_| TransportEnd::Stopped)?;
            if !state
                .channels
                .get(&registration)
                .is_some_and(|channel| channel.valid)
                || state.identity.as_ref() != Some(&Identity::from(&value.authority))
            {
                return Err(TransportEnd::Closed);
            }
            if state.listing_rev.is_some_and(|rev| {
                value.listing.rev < rev
                    || (value.listing.rev == rev && grants != state.listing_grants)
            }) {
                return Err(TransportEnd::Malformed);
            }
            // A same-revision reply may arrive after a fresher workspace-open
            // proof. Validate it against the last listing, without rolling
            // that independently verified grant backward.
            if state.listing_rev == Some(value.listing.rev) {
                return Ok(());
            }
            if grants.iter().any(|(id, new)| {
                state
                    .grants
                    .get(id)
                    .is_some_and(|old| old.grant_generation == new.grant_generation && old != new)
            }) {
                return Err(TransportEnd::Malformed);
            }
            let changed: Vec<_> = state
                .grants
                .iter()
                .filter_map(|(id, old)| match grants.get(id) {
                    None => Some((*id, None)),
                    Some(new) if new.grant_generation > old.grant_generation => {
                        Some((*id, Some(new.grant_generation)))
                    }
                    _ => None,
                })
                .collect();
            for (id, grant) in &grants {
                if state
                    .grants
                    .get(id)
                    .is_none_or(|old| old.grant_generation <= grant.grant_generation)
                {
                    state.grants.insert(*id, grant.clone());
                }
            }
            state.grants.retain(|id, _| grants.contains_key(id));
            state.listing_grants = grants;
            state.listing_rev = Some(value.listing.rev);
            changed
                .into_iter()
                .flat_map(|(workspace, generation)| {
                    cancel_grant_channels(&mut state, workspace, generation, None)
                })
                .collect()
        };
        notify(victims, TransportEnd::GrantChanged);
        Ok(())
    }

    pub fn invalidate(&self, workspace: Option<WorkspaceId>, reason: TransportEnd, retire: bool) {
        let victims = {
            let Ok(mut state) = self.0.lock() else {
                return;
            };
            state.retired |= retire;
            cancel_channels(&mut state, workspace, None)
        };
        notify(victims, reason);
    }
}

type Victims = Vec<(watch::Sender<Option<TransportEnd>>, ChannelSink)>;

fn cancel_channels(
    state: &mut State,
    workspace: Option<WorkspaceId>,
    except: Option<u64>,
) -> Victims {
    state
        .channels
        .iter_mut()
        .filter(|(id, channel)| {
            Some(**id) != except && (workspace.is_none() || channel.workspace == workspace)
        })
        .map(|(_, channel)| {
            channel.valid = false;
            channel.admitted = false;
            (channel.stop.clone(), channel.sink.clone())
        })
        .collect()
}

fn notify(victims: Victims, reason: TransportEnd) {
    for (stop, sink) in victims {
        stop.send_replace(Some(reason));
        sink.end(reason);
    }
}

fn cancel_grant_channels(
    state: &mut State,
    workspace: WorkspaceId,
    generation: Option<u64>,
    except: Option<u64>,
) -> Victims {
    state
        .channels
        .iter_mut()
        .filter(|(id, channel)| {
            Some(**id) != except
                && channel.workspace == Some(workspace)
                && (generation.is_none()
                    || channel
                        .grant_generation
                        .is_some_and(|old| Some(old) != generation))
        })
        .map(|(_, channel)| {
            channel.valid = false;
            channel.admitted = false;
            (channel.stop.clone(), channel.sink.clone())
        })
        .collect()
}

pub(super) fn validate_authority(
    authority: &GatewayAuthority,
    endpoint: EndpointId,
) -> Result<(), TransportEnd> {
    if authority.endpoint_id != endpoint
        || authority.device_generation == 0
        || authority.server_time <= 0
        || authority.credential_expires_at <= authority.server_time
    {
        return Err(TransportEnd::Authentication);
    }
    Ok(())
}

pub(super) fn validate_grant(grant: &WorkspaceGrant) -> Result<(), TransportEnd> {
    if grant.grant_generation == 0
        || !grant.capabilities.contains(&Capability::Read)
        || grant.capabilities.len() > 3
        || grant.allowed_actions.len() > 31
    {
        return Err(TransportEnd::Malformed);
    }
    let capabilities: BTreeSet<_> = grant.capabilities.iter().collect();
    let actions: BTreeSet<_> = grant.allowed_actions.iter().collect();
    if capabilities.len() != grant.capabilities.len()
        || actions.len() != grant.allowed_actions.len()
        || grant.allowed_actions.iter().any(|action| {
            action.is_empty()
                || action.len() > 64
                || !action.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
    {
        return Err(TransportEnd::Malformed);
    }
    Ok(())
}

pub(super) fn validate_listing(
    value: &GatewayListing<Workspaces>,
    endpoint: EndpointId,
) -> Result<(), TransportEnd> {
    validate_authority(&value.authority, endpoint)?;
    if value.listing.rev == 0
        || !value.listing.others.is_empty()
        || value.listing.joined.len() > 128
        || value.grants.len() != value.listing.joined.len()
    {
        return Err(TransportEnd::Malformed);
    }
    let mut grants = BTreeSet::new();
    for grant in &value.grants {
        validate_grant(grant)?;
        if !grants.insert(grant.workspace_id) {
            return Err(TransportEnd::Malformed);
        }
    }
    let mut entries = BTreeSet::new();
    for entry in &value.listing.joined {
        let id = WorkspaceId::from_hex(&entry.id).map_err(|_| TransportEnd::Malformed)?;
        if entry.generation == 0 || !grants.contains(&id) || !entries.insert(id) {
            return Err(TransportEnd::Malformed);
        }
    }
    Ok(())
}

/// The returned scope takes daemon identity and generation only from route.
/// Expected/listed values are comparison constraints, never replacements.
pub(super) fn normalize(
    opened: &GatewayOpened<HelloInfo>,
    endpoint: EndpointId,
    purpose: ChannelPurpose,
    workspace: Option<WorkspaceId>,
    expected: Option<&ClientScope>,
    attempt: u64,
) -> Result<Option<(ConnectionContext, PeerInfo)>, TransportEnd> {
    validate_authority(&opened.authority, endpoint)?;
    if opened.native_protocol != NATIVE_PROTOCOL || opened.protocol != DAEMON_PROTOCOL {
        return Err(TransportEnd::ProtocolMismatch);
    }
    if opened.purpose != purpose || !purpose.accepts_workspace(workspace) {
        return Err(TransportEnd::Malformed);
    }
    if purpose == ChannelPurpose::Index {
        return if opened.route.is_none() && opened.grant.is_none() && opened.hello.is_none() {
            Ok(None)
        } else {
            Err(TransportEnd::Malformed)
        };
    }
    let route = opened.route.as_ref().ok_or(TransportEnd::Malformed)?;
    let grant = opened.grant.as_ref().ok_or(TransportEnd::Malformed)?;
    let hello = opened.hello.as_ref().ok_or(TransportEnd::Malformed)?;
    validate_grant(grant)?;
    if Some(route.workspace_id) != workspace
        || grant.workspace_id != route.workspace_id
        || route.workspace_generation == 0
        || route.daemon.pid == 0
        || route.daemon.started_at <= 0
        || hello.protocol != DAEMON_PROTOCOL
        || hello.role != Role::Operator
        || hello.agent_id.is_some()
        || hello.web_proof.is_some()
        || hello.pid != route.daemon.pid
    {
        return Err(TransportEnd::Malformed);
    }
    let scope = ClientScope {
        endpoint_id: endpoint.to_hex(),
        workspace_id: route.workspace_id.to_hex(),
        generation: Some(route.workspace_generation),
        daemon_pid: Some(route.daemon.pid),
        daemon_started_at_ms: Some(route.daemon.started_at),
        attempt,
    };
    if expected.is_some_and(|old| {
        old.endpoint_id != scope.endpoint_id
            || old.workspace_id != scope.workspace_id
            || old.attempt != scope.attempt
            || old
                .generation
                .is_some_and(|value| Some(value) != scope.generation)
            || old
                .daemon_pid
                .is_some_and(|value| Some(value) != scope.daemon_pid)
            || old
                .daemon_started_at_ms
                .is_some_and(|value| Some(value) != scope.daemon_started_at_ms)
    }) {
        return Err(TransportEnd::GrantChanged);
    }
    if purpose == ChannelPurpose::Terminal && !grant.capabilities.contains(&Capability::Terminal) {
        return Err(TransportEnd::Permission);
    }
    // server_time/attempt are intentionally excluded so an unchanged authority
    // has a stable identity across freshly authenticated message-retry lanes.
    let identity = serde_json::to_vec(&(
        opened.authority.endpoint_id,
        opened.authority.endpoint_epoch,
        opened.authority.device_id,
        opened.authority.device_generation,
        opened.authority.credential_expires_at,
        grant,
        route,
    ))
    .map_err(|_| TransportEnd::Malformed)?;
    let grant_id = format!(
        "gateway:{}",
        HexBytes::<32>(Sha256::digest(identity).into()).to_hex()
    );
    let context = ConnectionContext {
        scope,
        mode: ConnectionMode::ScopedGateway,
        snapshot_mode: SnapshotMode::OrderedConnection,
        grant: Grant {
            grant_id,
            tiers: grant
                .capabilities
                .iter()
                .map(|c| match c {
                    Capability::Read => GrantTier::Read,
                    Capability::Control => GrantTier::Control,
                    Capability::Terminal => GrantTier::Terminal,
                })
                .collect(),
            workspace_ids: vec![route.workspace_id.to_hex()],
            expires_at_ms: Some(opened.authority.credential_expires_at),
        },
    };
    let peer = PeerInfo {
        version: hello.version.clone(),
        protocol: hello.protocol,
        pid: hello.pid,
        project: hello.project.clone(),
    };
    Ok(Some((context, peer)))
}
