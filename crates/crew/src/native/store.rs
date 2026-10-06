//! Native authority lives beneath the runtime root already hidden from agents.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use crew_core::paths;
use crew_protocol::native::{
    Capability, DeviceId, EndpointId, PublicKey, WorkspaceGrant, WorkspaceId,
};
use serde::{Deserialize, Serialize};

use super::policy;

const MAX_REGISTRY_BYTES: usize = 1024 * 1024;

// Native memberships remain selected after grants are removed. Their identity
// is immutable for this registry's lifetime; grant generations change separately.
pub(crate) const MEMBERSHIP_GENERATION: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Membership {
    pub id: WorkspaceId,
    pub state_dir: String,
    pub workspace: String,
    pub joined_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantRecord {
    pub generation: u64,
    pub capabilities: Vec<Capability>,
}

impl GrantRecord {
    pub fn view(&self, workspace_id: WorkspaceId) -> WorkspaceGrant {
        WorkspaceGrant {
            workspace_id,
            grant_generation: self.generation,
            capabilities: self.capabilities.clone(),
            allowed_actions: policy::allowed_actions(&self.capabilities),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Device {
    pub id: DeviceId,
    pub public_key: PublicKey,
    pub label: String,
    pub generation: u64,
    pub grant_counter: u64,
    pub issued_at: i64,
    pub expires_at: i64,
    pub active: bool,
    pub grants: BTreeMap<WorkspaceId, GrantRecord>,
}

impl Device {
    pub fn valid_at(&self, now: i64) -> bool {
        self.active && now >= self.issued_at && now < self.expires_at
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registry {
    pub format: u32,
    pub endpoint_id: EndpointId,
    pub devices: BTreeMap<DeviceId, Device>,
    pub memberships: BTreeMap<WorkspaceId, Membership>,
}

impl Registry {
    fn validate(&self) -> Result<()> {
        if self.format != 1 || self.devices.len() > 32 || self.memberships.len() > 32 {
            bail!("invalid native authority registry");
        }
        let mut keys = BTreeSet::new();
        for (id, device) in &self.devices {
            if *id != device.id
                || device.label.is_empty()
                || device.label.chars().count() > 64
                || device.label.chars().any(char::is_control)
                || device.generation == 0
                || device.grant_counter == 0
                || device.expires_at <= device.issued_at
                || device.grants.len() > 32
                || !keys.insert(device.public_key)
            {
                bail!("invalid native device record");
            }
            for (workspace, grant) in &device.grants {
                if !self.memberships.contains_key(workspace)
                    || grant.generation == 0
                    || grant.capabilities.is_empty()
                    || grant.capabilities.len() > 3
                    || grant.capabilities[0] != Capability::Read
                    || !grant.capabilities.windows(2).all(|p| p[0] < p[1])
                {
                    bail!("invalid native workspace grant");
                }
            }
        }
        for (id, member) in &self.memberships {
            if *id != member.id
                || member.state_dir.len() > 4096
                || member.workspace.len() > 4096
                || !Path::new(&member.state_dir).is_absolute()
                || !Path::new(&member.workspace).is_absolute()
                || paths::session_id(Path::new(&member.state_dir)) != id.to_hex()
                || paths::state_dir(Path::new(&member.workspace)) != Path::new(&member.state_dir)
            {
                bail!("invalid native membership record");
            }
        }
        Ok(())
    }

    pub fn by_key(&self, key: PublicKey) -> Option<&Device> {
        self.devices.values().find(|d| d.public_key == key)
    }
}

pub(crate) struct Store {
    pub root: PathBuf,
    pub registry: Registry,
    // Keep a pinned directory descriptor for reads/durability checks.
    directory: File,
}

impl Store {
    pub fn open(runtime: &Path) -> Result<Self> {
        paths::ensure_private_dir(runtime)?;
        let root = runtime.join("native");
        paths::ensure_private_dir(&root)?;
        let directory = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&root)?;
        let registry = match read_private(&root, "devices.json", MAX_REGISTRY_BYTES) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid native authority registry"))?,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Registry {
                    format: 1,
                    endpoint_id: super::fresh()?,
                    devices: BTreeMap::new(),
                    memberships: BTreeMap::new(),
                }
            }
            Err(error) => return Err(error),
        };
        let mut store = Self {
            root,
            registry,
            directory,
        };
        store.registry.validate()?;
        if !store.root.join("devices.json").exists() {
            store.commit(store.registry.clone())?;
        }
        Ok(store)
    }

    pub fn commit(&mut self, next: Registry) -> Result<()> {
        next.validate()?;
        let bytes = serde_json::to_vec(&next)?;
        if bytes.len() > MAX_REGISTRY_BYTES {
            bail!("native registry capacity exceeded");
        }
        paths::write_private_file(&self.root.join("devices.json"), &bytes)?;
        // Atomic replacement is not durable until the directory entry is synced.
        self.directory
            .sync_all()
            .context("syncing native authority registry")?;
        self.registry = next;
        Ok(())
    }

    pub fn membership(workspace: &Path, now: i64) -> Result<Membership> {
        let workspace = workspace
            .canonicalize()
            .context("resolving selected workspace")?;
        let state = paths::state_dir(&workspace);
        if !state.is_dir() {
            bail!("selected workspace has no crew state directory");
        }
        Ok(Membership {
            id: WorkspaceId::from_hex(&paths::session_id(&state)).map_err(anyhow::Error::msg)?,
            state_dir: state.to_string_lossy().into_owned(),
            workspace: workspace.to_string_lossy().into_owned(),
            joined_at: now,
        })
    }

    pub fn add_membership(next: &mut Registry, member: Membership) -> Result<()> {
        if let Some(existing) = next.memberships.get(&member.id) {
            if existing.state_dir != member.state_dir {
                bail!("native workspace id collision");
            }
            return Ok(());
        }
        if next.memberships.len() >= 32 {
            bail!("native workspace capacity exceeded");
        }
        next.memberships.insert(member.id, member);
        Ok(())
    }
}

pub(crate) fn read_private(root: &Path, name: &str, max: usize) -> Result<Vec<u8>> {
    let directory = File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?;
    if directory.metadata()?.uid() != paths::uid()
        || directory.metadata()?.permissions().mode() & 0o077 != 0
    {
        bail!("native private directory must belong to this user with mode 0700");
    }
    let file = paths::open_beneath(&directory, Path::new(name))?;
    let metadata = file.metadata()?;
    if metadata.uid() != paths::uid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > max as u64
    {
        bail!("native private file must be a bounded same-user regular file with mode 0600 and one link");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        bail!("native private file exceeds its limit");
    }
    Ok(bytes)
}

pub(crate) fn read_tls_key(root: &Path, path: &Path) -> Result<Vec<u8>> {
    // Do not read or canonicalize an external private key before this check.
    let relative = path.strip_prefix(root).map_err(|_| {
        anyhow::anyhow!("TLS private key must be beneath the protected native runtime directory")
    })?;
    if relative.components().count() != 1 || relative.file_name().is_none() {
        bail!("TLS private key must be a direct file in the protected native runtime directory");
    }
    let name = relative
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid TLS key path"))?;
    read_private(root, name, 64 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_protocol::native::HexBytes;
    use std::fs;

    #[test]
    fn registry_is_private_durable_and_independent_of_external_state_home() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::open(tmp.path()).unwrap();
        let id = store.registry.endpoint_id;
        let device_id = HexBytes([3; 16]);
        let mut next = store.registry.clone();
        next.devices.insert(
            device_id,
            Device {
                id: device_id,
                public_key: HexBytes([4; 32]),
                label: "Phone".into(),
                generation: 1,
                grant_counter: 1,
                issued_at: 1,
                expires_at: 2,
                active: false,
                grants: BTreeMap::new(),
            },
        );
        store.commit(next).unwrap();
        drop(store);
        let store = Store::open(tmp.path()).unwrap();
        assert_eq!(store.registry.endpoint_id, id);
        assert!(!store.registry.devices[&device_id].active);
        assert_eq!(
            fs::metadata(store.root.join("devices.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(store.root.starts_with(tmp.path()));
    }

    #[test]
    fn external_linked_and_symlinked_tls_keys_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let external = tmp.path().join("external.key");
        paths::write_private_file(&external, b"synthetic-private-key").unwrap();
        assert!(read_tls_key(&store.root, &external).is_err());
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native/key.pem");
        assert!(fixture.is_file());
        assert!(read_tls_key(&store.root, &fixture).is_err());
        fs::hard_link(&external, store.root.join("linked.key")).unwrap();
        assert!(read_tls_key(&store.root, &store.root.join("linked.key")).is_err());
        std::os::unix::fs::symlink(&external, store.root.join("symlink.key")).unwrap();
        assert!(read_tls_key(&store.root, &store.root.join("symlink.key")).is_err());
        paths::write_private_file(&store.root.join("key.pem"), b"private-key").unwrap();
        assert_eq!(
            read_tls_key(&store.root, &store.root.join("key.pem")).unwrap(),
            b"private-key"
        );
    }
}
