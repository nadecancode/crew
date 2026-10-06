use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{bail, Result};
use crew_core::rpc::{HelloInfo, Workspaces};
use crew_protocol::native::{
    Capability, DeviceId, EnrollmentId, GatewayAuthority, GatewayListing, HexBytes, PublicKey,
    PublicProfile, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch, Semaphore};
use tokio::task::JoinHandle;

use super::auth::Enrollment;
use super::limits::Attempts;
use super::store::{Device, GrantRecord, Membership, Store};
use super::tls::Identity;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub endpoint: String,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub name: String,
    pub runtime: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantSelection {
    pub workspace: PathBuf,
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub terminal: bool,
}

pub(crate) struct Connection {
    pub id: u64,
    pub cancel: watch::Sender<bool>,
    pub close_code: std::sync::atomic::AtomicU16,
    pub done: watch::Sender<bool>,
    pub task: Mutex<Option<JoinHandle<()>>>,
    pub approval: Mutex<Option<oneshot::Sender<DeviceId>>>,
}

pub(crate) struct ConnectionEntry {
    pub connection: Arc<Connection>,
    pub device: Option<DeviceId>,
    pub workspace: Option<WorkspaceId>,
    pub source: std::net::IpAddr,
    pub pending: usize,
}

pub(crate) struct State {
    pub store: Store,
    pub enrollment: Enrollment,
    pub connections: BTreeMap<u64, ConnectionEntry>,
    pub next_connection: u64,
    pub stopping: bool,
    pub visible: BTreeMap<
        DeviceId,
        (
            u64,
            Vec<crew_core::rpc::WorkspaceEntry>,
            Vec<crew_protocol::native::WorkspaceGrant>,
        ),
    >,
}

pub struct Gateway {
    pub(crate) startup_lock: Mutex<Option<std::fs::File>>,
    pub(crate) config: Config,
    pub(crate) identity: Identity,
    pub(crate) epoch: HexBytes<16>,
    pub(crate) state: Mutex<State>,
    pub(crate) changed: watch::Sender<u64>,
    pub(crate) stopping: watch::Sender<bool>,
    pub(crate) unauthenticated: Arc<Semaphore>,
    pub(crate) authenticated: Arc<Semaphore>,
    pub(crate) attempts: Mutex<Attempts>,
    pub(crate) enroll_attempts: Mutex<Attempts>,
    pub(crate) members: Mutex<
        BTreeMap<
            WorkspaceId,
            (
                crew_core::rpc::WorkspaceEntry,
                Option<crew_core::paths::DaemonInfo>,
            ),
        >,
    >,
}

impl Gateway {
    pub fn open(config: Config) -> Result<Arc<Self>> {
        crew_core::client::refuse_in_agent_context()?;
        let host = super::tls::validate_endpoint(config.listen, &config.endpoint)?;
        if !crew_protocol::native::valid_label(&config.name) {
            bail!("invalid native endpoint name");
        }
        let startup_lock = super::run::acquire_lock(&config.runtime)?;
        let store = Store::open(&config.runtime)?;
        let identity = Identity::load(&store.root, &config.certificate, &config.private_key, host)?;
        let (changed, _) = watch::channel(0);
        let (stopping, _) = watch::channel(false);
        Ok(Arc::new(Self {
            startup_lock: Mutex::new(Some(startup_lock)),
            config,
            identity,
            epoch: super::fresh()?,
            state: Mutex::new(State {
                store,
                enrollment: Enrollment::default(),
                connections: BTreeMap::new(),
                next_connection: 0,
                stopping: false,
                visible: BTreeMap::new(),
            }),
            changed,
            stopping,
            unauthenticated: Arc::new(Semaphore::new(32)),
            authenticated: Arc::new(Semaphore::new(128)),
            attempts: Mutex::new(Attempts::new(10, 100)),
            enroll_attempts: Mutex::new(Attempts::new(5, 30)),
            members: Mutex::new(BTreeMap::new()),
        }))
    }

    pub fn profile(&self) -> PublicProfile {
        PublicProfile {
            endpoint_id: lock(&self.state).store.registry.endpoint_id,
            name: self.config.name.clone(),
            url: self.config.endpoint.clone(),
            tls_leaf_sha256: self.identity.leaf_hash,
            trust_pem: self.identity.trust_pem.clone(),
        }
    }

    pub(crate) fn notify(&self) {
        self.changed.send_modify(|rev| *rev = rev.saturating_add(1));
    }

    pub(crate) fn allocate_connection(&self, source: std::net::IpAddr) -> Option<Arc<Connection>> {
        if !lock(&self.attempts).allowed(source) {
            return None;
        }
        let mut state = lock(&self.state);
        if state.stopping
            || state
                .connections
                .values()
                .filter(|c| c.device.is_none() && c.source == source)
                .count()
                >= 4
        {
            return None;
        }
        state.next_connection = state.next_connection.checked_add(1)?;
        let id = state.next_connection;
        let connection = Arc::new(Connection {
            id,
            cancel: watch::channel(false).0,
            close_code: std::sync::atomic::AtomicU16::new(4101),
            done: watch::channel(false).0,
            task: Mutex::new(None),
            approval: Mutex::new(None),
        });
        state.connections.insert(
            id,
            ConnectionEntry {
                connection: connection.clone(),
                device: None,
                workspace: None,
                source,
                pending: 0,
            },
        );
        Some(connection)
    }

    pub(crate) fn finish_connection(&self, connection: &Connection) {
        let mut state = lock(&self.state);
        if let Some(entry) = state.connections.remove(&connection.id) {
            if entry.device.is_none() && !state.stopping {
                lock(&self.attempts).admit(entry.source);
            }
        }
        state.enrollment.remove_connection(connection.id);
        connection.done.send_replace(true);
    }

    pub(crate) fn bind(
        &self,
        connection: u64,
        key: PublicKey,
        workspace: Option<WorkspaceId>,
    ) -> Option<Device> {
        let mut state = lock(&self.state);
        if state.stopping {
            return None;
        }
        let device = state.store.registry.by_key(key)?.clone();
        if !device.valid_at(super::now_ms())
            || workspace.is_some_and(|w| !device.grants.contains_key(&w))
            || state
                .connections
                .values()
                .filter(|c| c.device == Some(device.id))
                .count()
                >= 8
        {
            return None;
        }
        let entry = state.connections.get_mut(&connection)?;
        if *entry.connection.cancel.borrow() {
            return None;
        }
        entry.device = Some(device.id);
        entry.workspace = workspace;
        Some(device)
    }

    pub(crate) fn current(&self, device: &Device, workspace: Option<WorkspaceId>) -> bool {
        let state = lock(&self.state);
        if state.stopping {
            return false;
        }
        state
            .store
            .registry
            .devices
            .get(&device.id)
            .is_some_and(|current| {
                current.valid_at(super::now_ms())
                    && current.generation == device.generation
                    && workspace.is_none_or(|w| {
                        current
                            .grants
                            .get(&w)
                            .zip(device.grants.get(&w))
                            .is_some_and(|(a, b)| a.generation == b.generation)
                    })
            })
    }

    pub(crate) fn acquire_pending(&self, connection: u64) -> bool {
        let mut state = lock(&self.state);
        let Some(device) = state.connections.get(&connection).and_then(|c| c.device) else {
            return false;
        };
        if state.stopping
            || !state
                .store
                .registry
                .devices
                .get(&device)
                .is_some_and(|d| d.valid_at(super::now_ms()))
        {
            return false;
        }
        let total = state
            .connections
            .values()
            .filter(|c| c.device == Some(device))
            .map(|c| c.pending)
            .sum::<usize>();
        let Some(entry) = state.connections.get_mut(&connection) else {
            return false;
        };
        if total >= 64 || entry.pending >= 16 || *entry.connection.cancel.borrow() {
            return false;
        }
        entry.pending += 1;
        true
    }

    /// Read the operator-selected route together with its live device grant.
    /// Cached index entries do not establish a channel's daemon incarnation.
    pub(crate) fn route_membership(
        &self,
        device: &Device,
        workspace: WorkspaceId,
    ) -> Option<Membership> {
        let state = lock(&self.state);
        let current = state.store.registry.devices.get(&device.id)?;
        if state.stopping
            || !current.valid_at(super::now_ms())
            || current.generation != device.generation
            || current
                .grants
                .get(&workspace)
                .zip(device.grants.get(&workspace))
                .is_none_or(|(a, b)| a.generation != b.generation)
        {
            return None;
        }
        state.store.registry.memberships.get(&workspace).cloned()
    }

    pub(crate) fn release_pending(&self, connection: u64) {
        if let Some(entry) = lock(&self.state).connections.get_mut(&connection) {
            entry.pending = entry.pending.saturating_sub(1);
        }
    }

    pub(crate) async fn change_grant(
        &self,
        device_id: DeviceId,
        selection: GrantSelection,
    ) -> Result<()> {
        let member = Store::membership(&selection.workspace, super::now_ms())?;
        let workspace_id = member.id;
        let mut caps = vec![Capability::Read];
        if selection.control {
            caps.push(Capability::Control);
        }
        if selection.terminal {
            caps.push(Capability::Terminal);
        }
        let affected = {
            let mut state = lock(&self.state);
            let mut next = state.store.registry.clone();
            Store::add_membership(&mut next, member)?;
            let device = next
                .devices
                .get_mut(&device_id)
                .ok_or_else(|| anyhow::anyhow!("native device not found"))?;
            if !device.valid_at(super::now_ms()) {
                bail!("native device is inactive or expired");
            }
            device.grant_counter = device
                .grant_counter
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("native grant generation overflow"))?;
            device.grants.insert(
                workspace_id,
                GrantRecord {
                    generation: device.grant_counter,
                    capabilities: caps,
                },
            );
            self.commit(&mut state, next)?;
            let affected = state
                .connections
                .values()
                .filter(|c| c.device == Some(device_id) && c.workspace == Some(workspace_id))
                .map(|c| c.connection.clone())
                .collect::<Vec<_>>();
            for c in &affected {
                c.close_code
                    .store(4102, std::sync::atomic::Ordering::Relaxed);
                c.cancel.send_replace(true);
            }
            affected
        };
        self.notify();
        drain(affected).await
    }

    pub(crate) fn authority(&self, device: &Device) -> GatewayAuthority {
        GatewayAuthority {
            endpoint_id: lock(&self.state).store.registry.endpoint_id,
            endpoint_epoch: self.epoch,
            device_id: device.id,
            device_generation: device.generation,
            credential_expires_at: device.expires_at,
            server_time: super::now_ms(),
        }
    }

    pub(crate) fn open_enrollment(&self) -> Result<()> {
        let mut state = lock(&self.state);
        if state.stopping {
            bail!("native gateway is stopping");
        }
        state.enrollment.open();
        Ok(())
    }

    pub(crate) fn pending(&self) -> serde_json::Value {
        let mut state = lock(&self.state);
        state.enrollment.purge();
        serde_json::json!(state.enrollment.intents.values().map(|i| serde_json::json!({"enrollment_id":i.id,"comparison_code":i.comparison,"public_key_hash":super::auth::key_hash(i.public_key),"label":i.label,"expires_at":i.expires_at})).collect::<Vec<_>>())
    }

    pub(crate) fn approve(
        &self,
        id: EnrollmentId,
        code: HexBytes<8>,
        key_hash: HexBytes<32>,
        selections: Vec<GrantSelection>,
        lifetime_secs: u64,
    ) -> Result<DeviceId> {
        if selections.is_empty()
            || selections.len() > 32
            || lifetime_secs == 0
            || lifetime_secs > 30 * 86400
        {
            bail!("invalid native grant selection or lifetime");
        }
        let now = super::now_ms();
        let members = selections
            .into_iter()
            .map(|s| {
                let member = Store::membership(&s.workspace, now)?;
                let mut caps = vec![Capability::Read];
                if s.control {
                    caps.push(Capability::Control);
                }
                if s.terminal {
                    caps.push(Capability::Terminal);
                }
                Ok((member, caps))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut state = lock(&self.state);
        if state.stopping {
            bail!("native gateway is stopping");
        }
        let intent = state
            .enrollment
            .check(id, code, key_hash)
            .map_err(anyhow::Error::msg)?;
        let entry = state
            .connections
            .get(&intent.connection)
            .ok_or_else(|| anyhow::anyhow!("enrollment connection ended"))?;
        if *entry.connection.cancel.borrow() || lock(&entry.connection.approval).is_none() {
            bail!("enrollment connection ended");
        }
        if state.store.registry.devices.len() >= 32
            || state.store.registry.by_key(intent.public_key).is_some()
        {
            bail!("native device capacity or key already used");
        }
        let device_id = super::fresh()?;
        let mut next = state.store.registry.clone();
        let mut grants = BTreeMap::new();
        for (member, capabilities) in members {
            Store::add_membership(&mut next, member.clone())?;
            if grants
                .insert(
                    member.id,
                    GrantRecord {
                        generation: 1,
                        capabilities,
                    },
                )
                .is_some()
            {
                bail!("duplicate native workspace selection");
            }
        }
        next.devices.insert(
            device_id,
            Device {
                id: device_id,
                public_key: intent.public_key,
                label: intent.label,
                generation: 1,
                grant_counter: 1,
                issued_at: now,
                expires_at: now
                    .checked_add((lifetime_secs * 1000) as i64)
                    .ok_or_else(|| anyhow::anyhow!("native expiry overflow"))?,
                active: true,
                grants,
            },
        );
        self.commit(&mut state, next)?;
        let intent = state
            .enrollment
            .consume(id)
            .ok_or_else(|| anyhow::anyhow!("enrollment already consumed"))?;
        if let Some(entry) = state.connections.get(&intent.connection) {
            if let Some(approval) = lock(&entry.connection.approval).take() {
                let _ = approval.send(device_id);
            }
        }
        drop(state);
        self.notify();
        Ok(device_id)
    }

    fn commit(&self, state: &mut State, registry: super::store::Registry) -> Result<()> {
        if let Err(error) = state.store.commit(registry) {
            // A failed durable save must never leave an unconfirmed authority live.
            state.stopping = true;
            self.stopping.send_replace(true);
            for entry in state.connections.values() {
                entry.connection.cancel.send_replace(true);
            }
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn revoke(
        &self,
        device_id: DeviceId,
        workspace: Option<WorkspaceId>,
    ) -> Result<()> {
        let affected = {
            let mut state = lock(&self.state);
            let mut next = state.store.registry.clone();
            let device = next
                .devices
                .get_mut(&device_id)
                .ok_or_else(|| anyhow::anyhow!("native device not found"))?;
            if let Some(workspace) = workspace {
                device.grant_counter = device
                    .grant_counter
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("native grant generation overflow"))?;
                device
                    .grants
                    .remove(&workspace)
                    .ok_or_else(|| anyhow::anyhow!("native grant not found"))?;
            } else {
                device.active = false;
                device.generation = device
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("native generation overflow"))?;
            }
            self.commit(&mut state, next)?;
            let affected = state
                .connections
                .values()
                .filter(|c| {
                    c.device == Some(device_id) && (workspace.is_none() || c.workspace == workspace)
                })
                .map(|c| c.connection.clone())
                .collect::<Vec<_>>();
            for connection in &affected {
                connection.close_code.store(
                    if workspace.is_some() { 4102 } else { 4101 },
                    std::sync::atomic::Ordering::Relaxed,
                );
                connection.cancel.send_replace(true);
            }
            affected
        };
        self.notify();
        drain(affected).await
    }

    pub(crate) async fn stop(&self) -> Result<()> {
        let affected = {
            let mut state = lock(&self.state);
            state.stopping = true;
            state.enrollment.close();
            let affected = state
                .connections
                .values()
                .map(|c| c.connection.clone())
                .collect::<Vec<_>>();
            for c in &affected {
                c.close_code
                    .store(4103, std::sync::atomic::Ordering::Relaxed);
                c.cancel.send_replace(true);
            }
            affected
        };
        let result = drain(affected).await;
        self.stopping.send_replace(true);
        result
    }

    pub(crate) fn listing(&self, device: &Device) -> Option<GatewayListing<Workspaces>> {
        let mut state = lock(&self.state);
        let current = state.store.registry.devices.get(&device.id)?.clone();
        if !current.valid_at(super::now_ms()) || current.generation != device.generation {
            return None;
        }
        let members = lock(&self.members);
        let mut entries = Vec::new();
        let mut grants = Vec::new();
        for (id, grant) in &current.grants {
            if let Some((entry, _)) = members.get(id) {
                entries.push(entry.clone());
                grants.push(grant.view(*id));
            }
        }
        let cached = state
            .visible
            .entry(device.id)
            .or_insert((0, Vec::new(), Vec::new()));
        if cached.0 == 0 || cached.1 != entries || cached.2 != grants {
            cached.0 = cached.0.checked_add(1)?;
            cached.1 = entries.clone();
            cached.2 = grants.clone();
        }
        let rev = cached.0;
        drop(members);
        drop(state);
        Some(GatewayListing {
            authority: self.authority(&current),
            grants,
            listing: Workspaces {
                rev,
                joined: entries,
                others: Vec::new(),
            },
        })
    }
}

pub(crate) async fn drain(connections: Vec<Arc<Connection>>) -> Result<()> {
    let waits = connections.iter().map(|connection| async move {
        let mut done = connection.done.subscribe();
        if !*done.borrow() {
            let _ = done.wait_for(|v| *v).await;
        }
    });
    if tokio::time::timeout(
        Duration::from_secs(1),
        futures_util::future::join_all(waits),
    )
    .await
    .is_err()
    {
        let mut tasks = Vec::new();
        for connection in &connections {
            if !*connection.done.borrow() {
                if let Some(task) = lock(&connection.task).take() {
                    task.abort();
                    tasks.push(task);
                }
            }
        }
        tokio::time::timeout(
            Duration::from_secs(5),
            futures_util::future::join_all(tasks),
        )
        .await
        .map_err(|_| anyhow::anyhow!("native channel did not drain"))?;
        if connections.iter().any(|c| !*c.done.borrow()) {
            bail!("native channel did not drain");
        }
    }
    Ok(())
}

pub(crate) struct Finished {
    pub gateway: Arc<Gateway>,
    pub connection: Arc<Connection>,
}
impl Drop for Finished {
    fn drop(&mut self) {
        self.gateway.finish_connection(&self.connection);
    }
}

pub(crate) type NativeHello = crew_protocol::native::GatewayOpened<HelloInfo>;

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::rpc::WorkspaceEntry;
    use crew_core::WorkspaceState;

    #[tokio::test]
    async fn listing_waits_for_visible_members_before_publishing_grants() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        let native = runtime.join("native");
        crew_core::paths::ensure_private_dir(&native).unwrap();
        let key_path = native.join("key.pem");
        let certificate = native.join("cert.pem");
        crew_core::paths::write_private_file(
            &key_path,
            include_bytes!("../../tests/fixtures/native/key.pem"),
        )
        .unwrap();
        std::fs::write(
            &certificate,
            include_bytes!("../../tests/fixtures/native/cert.pem"),
        )
        .unwrap();
        // Opening validates the protected store/TLS files but binds no listener.
        let gateway = Gateway::open(Config {
            listen: "127.0.0.1:12345".parse().unwrap(),
            endpoint: "wss://127.0.0.1:12345/native/ws".into(),
            certificate,
            private_key: key_path,
            name: "Listing test".into(),
            runtime,
        })
        .unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".crew")).unwrap();
        let now = super::super::now_ms();
        let member = Store::membership(&workspace, now).unwrap();
        let device = Device {
            id: HexBytes([1; 16]),
            public_key: HexBytes([2; 32]),
            label: "Listing device".into(),
            generation: 1,
            grant_counter: 1,
            issued_at: now,
            expires_at: now + 60_000,
            active: true,
            grants: BTreeMap::new(),
        };
        {
            let mut state = lock(&gateway.state);
            let mut registry = state.store.registry.clone();
            registry.devices.insert(device.id, device.clone());
            state.store.commit(registry).unwrap();
        }
        let before = gateway.listing(&device).unwrap();
        assert!(before.grants.is_empty());
        assert!(before.listing.joined.is_empty());
        gateway
            .change_grant(
                device.id,
                GrantSelection {
                    workspace,
                    control: true,
                    terminal: false,
                },
            )
            .await
            .unwrap();
        let absent = gateway.listing(&device).unwrap();
        assert!(absent.grants.is_empty());
        assert!(absent.listing.joined.is_empty());
        assert_eq!(absent.listing.rev, before.listing.rev);
        assert_eq!(
            gateway.listing(&device).unwrap().listing.rev,
            absent.listing.rev
        );

        let entry = WorkspaceEntry {
            id: member.id.to_hex(),
            generation: super::super::store::MEMBERSHIP_GENERATION,
            daemon: None,
            project: None,
            workspace: member.workspace,
            state: WorkspaceState::Stopped,
            detail: String::new(),
            summary: None,
            joined_at: member.joined_at,
        };
        lock(&gateway.members).insert(member.id, (entry.clone(), None));
        let present = gateway.listing(&device).unwrap();
        assert_eq!(present.listing.joined, vec![entry.clone()]);
        assert_eq!(present.grants.len(), 1);
        assert_eq!(present.grants[0].workspace_id, member.id);
        assert_eq!(present.grants[0].grant_generation, 2);
        assert_eq!(present.listing.rev, absent.listing.rev + 1);
        let unchanged = gateway.listing(&device).unwrap();
        assert_eq!(unchanged.listing, present.listing);
        assert_eq!(unchanged.grants, present.grants);

        lock(&gateway.members).remove(&member.id);
        let disappeared = gateway.listing(&device).unwrap();
        assert!(disappeared.grants.is_empty());
        assert!(disappeared.listing.joined.is_empty());
        assert_eq!(disappeared.listing.rev, present.listing.rev + 1);
        lock(&gateway.members).insert(member.id, (entry.clone(), None));
        let returned = gateway.listing(&device).unwrap();
        assert_eq!(returned.listing.joined, vec![entry]);
        assert_eq!(returned.grants, present.grants);
        assert_eq!(returned.listing.rev, disappeared.listing.rev + 1);
        assert!(returned.listing.others.is_empty());
    }
}
