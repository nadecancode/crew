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

pub mod prepare;
mod sources;
pub mod store;

/// Credential-free inputs to the runtime adapter, prepared once for this
/// launch. Never recovered from disk in place of resolving current settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeToolbox {
    pub servers: Vec<String>,
    pub scrub_env: Vec<String>,
    pub inherit_memory: bool,
    pub claude_sources: String,
    pub claude_inherit_mcp: bool,
    pub claude_preferences: Value,
    pub claude_plugins: Vec<PathBuf>,
    pub codex_overrides: Vec<(String, Value)>,
    pub codex_skill_roots: Vec<PathBuf>,
    pub codex_inherit_servers: BTreeSet<String>,
    pub pi_skills: Vec<PathBuf>,
    pub pi_extensions: Vec<PathBuf>,
    pub pi_approve: bool,
    pub pi_inherit_extensions: bool,
    pub pi_context_files: bool,
    /// Empty retains pi's built-in prompt while disabling SYSTEM.md discovery.
    pub pi_system_prompt: String,
}

/// Increment when a runtime mapping changes the generated toolbox semantics.
pub const MAPPING_VERSION: u32 = 1;

/// What a member was started with. References remain references: credentials
/// are resolved separately in the daemon and never enter this persisted value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveToolbox {
    pub resolved: Resolved,
    pub content: BTreeMap<ItemRef, Content>,
    pub files: BTreeMap<ItemRef, store::Snapshot>,
    /// Servers whose mapping and references permit preflight or session use.
    #[serde(default)]
    pub servers: BTreeSet<String>,
    pub fingerprint: Option<Fingerprint>,
    pub definition_digest: Option<String>,
    /// A fresh-session restart refreshes the upstream definition snapshots.
    #[serde(default)]
    pub refresh: bool,
}
