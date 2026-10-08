//! A member's resolved toolbox and its immutable file snapshots.

use std::collections::{BTreeMap, BTreeSet};

use crew_core::toolbox::{Content, Fingerprint, ItemRef, Resolved};
use serde::{Deserialize, Serialize};

pub mod store;

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
