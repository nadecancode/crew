//! A member's resolved toolbox and its immutable file snapshots.

use std::collections::{BTreeMap, BTreeSet};

use crew_core::toolbox::{Content, Fingerprint, ItemRef, Resolved};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// Claude preserves server insertion order from its JSON configuration. Keep
/// crew first without relying on serde_json's optional map-order feature.
pub fn claude_mcp_bytes(
    crew: &Value,
    servers: &BTreeMap<String, Value>,
) -> anyhow::Result<Vec<u8>> {
    use serde::ser::SerializeMap;
    struct Ordered<'a> {
        crew: &'a Value,
        servers: &'a BTreeMap<String, Value>,
    }
    impl Serialize for Ordered<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(self.servers.len() + 1))?;
            map.serialize_entry("crew", self.crew)?;
            for (name, value) in self.servers {
                map.serialize_entry(name, value)?;
            }
            map.end()
        }
    }
    #[derive(Serialize)]
    struct Config<'a> {
        #[serde(rename = "mcpServers")]
        servers: Ordered<'a>,
    }
    Ok(serde_json::to_vec_pretty(&Config {
        servers: Ordered { crew, servers },
    })?)
}

pub(crate) mod args;
pub(crate) mod builtin;
pub mod prepare;
mod sources;
pub mod store;

/// Credential-free inputs to the runtime adapter, prepared once for this
/// launch. Never recovered from disk in place of resolving current settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeToolbox {
    /// Verified executable for the opt-in output skill; not part of skill bytes.
    pub compact_helper: Option<PathBuf>,
    pub servers: Vec<String>,
    pub scrub_env: Vec<String>,
    pub inherit_memory: bool,
    pub claude_sources: String,
    pub claude_inherit_mcp: bool,
    pub claude_preferences: Value,
    pub claude_plugins: Vec<PathBuf>,
    pub claude_rebuild_prompt: bool,
    pub codex_overrides: Vec<(String, Value)>,
    pub codex_skill_roots: Vec<PathBuf>,
    pub codex_inherit_servers: BTreeSet<String>,
    /// Project names also reserve collision space before Codex trusts the project.
    pub codex_project_servers: BTreeSet<String>,
    pub pi_skills: Vec<PathBuf>,
    pub pi_extensions: Vec<PathBuf>,
    pub pi_approve: bool,
    pub pi_inherit_extensions: bool,
    pub pi_context_files: bool,
    /// Empty retains pi's built-in prompt while disabling SYSTEM.md discovery.
    pub pi_system_prompt: String,
}

impl RuntimeToolbox {
    pub(crate) fn bind_helpers(&self, spec: &mut crate::agents::LaunchSpec) {
        let bind = |env: &mut Vec<(String, String)>| {
            env.retain(|(name, _)| name != builtin::HELPER_ENV);
            // Empty also masks an inherited binding when the skill is unavailable.
            env.push((
                builtin::HELPER_ENV.into(),
                self.compact_helper
                    .as_deref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ));
        };
        bind(&mut spec.env);
        if let Some(sidecar) = &mut spec.sidecar {
            bind(&mut sidecar.env);
        }
    }
}

/// Increment when a runtime mapping changes the generated toolbox semantics.
pub const MAPPING_VERSION: u32 = 2;

/// What a member was started with. References remain references: credentials
/// are resolved separately in the daemon and never enter this persisted value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveToolbox {
    pub resolved: Resolved,
    pub content: BTreeMap<ItemRef, Content>,
    pub files: BTreeMap<ItemRef, store::Snapshot>,
    /// Immutable copies of daemon-pinned MCP catalogs. Inheriting this map
    /// freezes the catalog even if the parent's launch later replaces its
    /// mutable `toolbox/mcp/<name>.json` file.
    #[serde(default)]
    pub mcp_snapshots: BTreeMap<String, store::Snapshot>,
    /// Servers whose mapping and references permit preflight or session use.
    #[serde(default)]
    pub servers: BTreeSet<String>,
    pub fingerprint: Option<Fingerprint>,
    pub definition_digest: Option<String>,
    #[serde(default)]
    pub scrub_env: BTreeSet<String>,
    /// Provenance only. Catalogs are read through `mcp_snapshots`, never by
    /// reopening this member's mutable pin during a child's launch.
    #[serde(default)]
    pub snapshot_owner: Option<String>,
    /// An explicit `resume --restart` or fresh-session restart refreshes
    /// upstream definition snapshots, except for inherited frozen catalogs.
    #[serde(default)]
    pub refresh: bool,
}

/// Restore the inherited catalog, or reuse the child's identical existing
/// pin. The content store verifies the snapshot hash and refuses links; no
/// mutable file in the parent member's directory participates in this read.
pub(crate) fn restore_inherited_mcp(
    store: &store::Store,
    agent_dir: &std::path::Path,
    name: &str,
    definition: &str,
    inherited: &ActiveToolbox,
) -> anyhow::Result<crate::mcp::proxy::PinOutcome> {
    use anyhow::{anyhow, bail};
    use std::path::Path;
    let snapshot = inherited.mcp_snapshots.get(name).ok_or_else(|| {
        anyhow!(
        "inherited MCP catalog {name} is not frozen; restart the parent before spawning a new child"
    )
    })?;
    let files = store.files(snapshot)?;
    let bytes = files
        .get(Path::new(""))
        .ok_or_else(|| anyhow!("inherited MCP catalog {name} is not a file"))?;
    let listing: Value = serde_json::from_slice(&bytes.0)?;
    if listing.get("definition").and_then(Value::as_str) != Some(definition) {
        bail!("inherited MCP snapshot does not match {name}'s definition");
    }
    let pinned = crate::mcp::proxy::pin(agent_dir, name, definition, &listing, false)?;
    if pinned.outcome == crate::mcp::proxy::PinOutcome::KeptDiffers {
        // The inherited selection, not a stray old child pin, is authoritative.
        return Ok(crate::mcp::proxy::pin(agent_dir, name, definition, &listing, true)?.outcome);
    }
    Ok(pinned.outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::proxy::{pin, snapshot_path, PinOutcome};
    use serde_json::json;
    use std::path::Path;

    struct Fixture(tempfile::TempDir);

    impl Drop for Fixture {
        fn drop(&mut self) {
            fn writable(path: &Path) {
                use std::os::unix::fs::PermissionsExt;
                if !std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                    return;
                }
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
                if let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.flatten() {
                        writable(&entry.path());
                    }
                }
            }
            writable(self.0.path());
        }
    }

    #[test]
    fn inherited_catalog_survives_parent_replacement_before_first_launch_and_child_resume() {
        let fixture = Fixture(tempfile::tempdir().unwrap());
        let root = fixture.0.path();
        let parent = root.join("parent");
        let child = root.join("child");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        let store = store::Store::open(&root.join("state")).unwrap();
        let definition = "a".repeat(64);
        let listing = |name| json!({"initialize":{"capabilities":{"tools":{}},"instructions":"Frozen instructions"},"tools":[{"name":name,"inputSchema":{"type":"object"}}]});
        pin(&parent, "docs", &definition, &listing("old_tool"), true).unwrap();
        let mut active = ActiveToolbox {
            snapshot_owner: Some("parent".into()),
            ..ActiveToolbox::default()
        };
        active.mcp_snapshots.insert(
            "docs".into(),
            store.snapshot(&snapshot_path(&parent, "docs")).unwrap(),
        );
        // Capture the selection, including persistence/recovery, before the
        // parent changes either its catalog or its server definition.
        let inherited: ActiveToolbox =
            serde_json::from_slice(&serde_json::to_vec(&active).unwrap()).unwrap();
        pin(&parent, "docs", &definition, &listing("new_tool"), true).unwrap();
        assert_eq!(
            restore_inherited_mcp(&store, &child, "docs", &definition, &inherited).unwrap(),
            PinOutcome::Written
        );
        let old = std::fs::read(snapshot_path(&child, "docs")).unwrap();
        assert!(String::from_utf8_lossy(&old).contains("old_tool"));
        assert!(!String::from_utf8_lossy(&old).contains("new_tool"));
        // A new upstream listing during the child's preflight also keeps
        // the inherited pin rather than expanding its catalog.
        assert_eq!(
            pin(&child, "docs", &definition, &listing("new_tool"), false)
                .unwrap()
                .outcome,
            PinOutcome::KeptDiffers
        );
        pin(
            &parent,
            "docs",
            &"b".repeat(64),
            &listing("different_server"),
            true,
        )
        .unwrap();
        assert_eq!(
            restore_inherited_mcp(&store, &child, "docs", &definition, &inherited).unwrap(),
            PinOutcome::Kept
        );
        assert_eq!(std::fs::read(snapshot_path(&child, "docs")).unwrap(), old);
        // The parent can even be removed; a child's crash/resume does not
        // depend on retaining that member's mutable directory.
        std::fs::remove_file(snapshot_path(&parent, "docs")).unwrap();
        assert_eq!(
            restore_inherited_mcp(&store, &child, "docs", &definition, &inherited).unwrap(),
            PinOutcome::Kept
        );
        assert_eq!(std::fs::read(snapshot_path(&child, "docs")).unwrap(), old);
    }
}
