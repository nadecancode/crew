//! Member toolboxes: the MCP servers, skills, plugins, subagents and hooks a
//! member's runtime loads beside crew's own, as a team file declares them
//! (`toolboxes`, `defaults.toolbox`, `orchestrator.toolbox`,
//! `agents[].toolbox`), and their resolution into one member's effective
//! toolbox. The contract is `notes/toolboxes.md` (§Spec).
//!
//! Everything here is pure: no file is read, no variable is looked up and no
//! runtime is consulted. The content hashes of file items, the runtime
//! mapping and the secret values come from the launcher.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Component, Path};

use anyhow::{anyhow, bail, Result};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::config::find_cycle;
use crate::types::RuntimeKind;
use crate::util::is_valid_id;

/// Most bundles a team (or style) may declare.
pub const MAX_BUNDLES: usize = 64;
/// Most items of one kind in one toolbox body.
pub const MAX_ITEMS: usize = 64;
/// Item names crew keeps for its own components, in every kind.
pub const RESERVED_NAMES: &[&str] = &["crew-bridge", "crew-toolbox"];
/// The MCP server name crew's coordination tools use.
pub const RESERVED_MCP: &str = "crew";
/// Variables that carry a member's model credentials or crew's own launch
/// settings. A toolbox reference may not read them: passing one on would
/// hand the model credential to a third-party server, and scrubbing it from
/// the session would break the member's login. The launcher adds the names
/// of the member's own endpoint configuration to this list.
pub const PROTECTED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "OPENAI_API_KEY",
    "CREW_CODEX_API_KEY",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "PI_CODING_AGENT_DIR",
    "CREW_IDENTITY",
];
/// Version of the canonical form; part of every fingerprint.
pub const CANONICAL_VERSION: u32 = 1;

const MAX_ARGS: usize = 256;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_NAME_BYTES: usize = 128;
const MAX_URL_BYTES: usize = 2048;

fn is_false(value: &bool) -> bool {
    !*value
}

/// What one level of a team file adds to a member's toolbox: a bundle under
/// `toolboxes`, `defaults.toolbox`, `orchestrator.toolbox` or an agent's
/// `toolbox`. Omitted and `{}` mean the same: no change at this level.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Toolbox {
    /// Bundles merged in, in the order listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<String, McpServer>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skills: BTreeMap<String, FileItem>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, FileItem>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subagents: BTreeMap<String, FileItem>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hooks: BTreeMap<String, Hook>,
    /// `<kind>.<name>` or `<kind>.*`: items removed from what earlier levels
    /// and this level's includes brought in. Not in bundles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Which runtime sources may load. Not in bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherit: Option<Inherit>,
    /// Start from nothing instead of `defaults.toolbox`. Members only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reset: bool,
    /// Every item required, and an inherited source the runtime cannot
    /// exclude stops the launch. Not in bundles.
    #[serde(default, skip_serializing_if = "is_false")]
    pub strict: bool,
}

/// An MCP server: local (`command`) or remote (`url`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    /// The program and its arguments, literal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// The server process's variables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, Reference>,
    /// Working directory, relative to the member's worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Remote servers only; `http` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<McpTransport>,
    /// Request headers of a remote server.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, Reference>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<RuntimeKind>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    #[default]
    Http,
    Sse,
}

impl McpTransport {
    fn as_str(self) -> &'static str {
        match self {
            McpTransport::Http => "http",
            McpTransport::Sse => "sse",
        }
    }
}

/// A skill directory (holding `SKILL.md`), a Claude Code plugin directory or
/// a subagent definition file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct FileItem {
    /// Absolute once the team file is loaded (see [`Toolbox::absolutize`]).
    pub path: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<RuntimeKind>,
}

/// A command run on a runtime event, alongside crew's own hooks.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<RuntimeKind>,
}

/// Where a variable or header value comes from. A team file never holds a
/// secret: `{ env: NAME }` names a variable resolved at launch, and
/// `{ value: "…" }` is a literal treated as public. A plain string is refused,
/// so every literal is deliberate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reference {
    Env {
        name: String,
        /// One `{}`, replaced by the value (`Bearer {}`).
        format: Option<String>,
    },
    Value(String),
}

impl Serialize for Reference {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Reference::Env { name, format } => {
                let mut map = serializer.serialize_map(Some(1 + usize::from(format.is_some())))?;
                map.serialize_entry("env", name)?;
                if let Some(format) = format {
                    map.serialize_entry("format", format)?;
                }
                map.end()
            }
            Reference::Value(value) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("value", value)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Reference {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ReferenceVisitor;

        impl<'de> Visitor<'de> for ReferenceVisitor {
            type Value = Reference;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("{ env: NAME } or { value: \"…\" }")
            }

            fn visit_str<E: de::Error>(self, _: &str) -> Result<Reference, E> {
                Err(E::custom(
                    "a plain string is not allowed here: write { env: NAME } for a secret \
or { value: \"…\" } for a public value",
                ))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Reference, A::Error> {
                let (mut env, mut format, mut value) = (None, None, None);
                while let Some(key) = map.next_key::<String>()? {
                    let slot = match key.as_str() {
                        "env" => &mut env,
                        "format" => &mut format,
                        "value" => &mut value,
                        other => {
                            return Err(de::Error::unknown_field(
                                other,
                                &["env", "format", "value"],
                            ))
                        }
                    };
                    if slot.is_some() {
                        return Err(de::Error::custom(format!("duplicate field `{key}`")));
                    }
                    *slot = Some(map.next_value::<String>()?);
                }
                match (env, format, value) {
                    (Some(name), format, None) => Ok(Reference::Env { name, format }),
                    (None, None, Some(value)) => Ok(Reference::Value(value)),
                    (None, Some(_), _) => Err(de::Error::custom("`format` needs `env`")),
                    (Some(_), _, Some(_)) => {
                        Err(de::Error::custom("give either `env` or `value`, not both"))
                    }
                    (None, None, None) => Err(de::Error::custom("expected `env` or `value`")),
                }
            }
        }

        deserializer.deserialize_any(ReferenceVisitor)
    }
}

/// Which runtime sources may load for a member.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Inherit {
    /// The operator's own configuration (user home, local settings).
    #[serde(default = "Selection::none")]
    pub operator: Selection,
    /// The repository's checked-in configuration. `all` means every kind
    /// except `mcp`: project MCP servers are declared under `mcp` instead.
    #[serde(default = "Selection::all")]
    pub project: Selection,
}

/// `none`, `all` or a list of kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    None,
    All,
    Kinds(BTreeSet<SourceKind>),
}

impl Selection {
    fn none() -> Self {
        Selection::None
    }

    fn all() -> Self {
        Selection::All
    }

    fn kinds(&self) -> BTreeSet<SourceKind> {
        match self {
            Selection::None => BTreeSet::new(),
            Selection::All => SourceKind::ALL.iter().copied().collect(),
            Selection::Kinds(kinds) => kinds.clone(),
        }
    }
}

impl Serialize for Selection {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Selection::None => serializer.serialize_str("none"),
            Selection::All => serializer.serialize_str("all"),
            Selection::Kinds(kinds) => {
                let mut seq = serializer.serialize_seq(Some(kinds.len()))?;
                for kind in kinds {
                    seq.serialize_element(kind)?;
                }
                seq.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Selection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SelectionVisitor;

        impl<'de> Visitor<'de> for SelectionVisitor {
            type Value = Selection;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("none, all or a list of kinds")
            }

            fn visit_str<E: de::Error>(self, word: &str) -> Result<Selection, E> {
                match word {
                    "none" => Ok(Selection::None),
                    "all" => Ok(Selection::All),
                    other => Err(E::custom(format!(
                        "'{other}' is not none, all or a list of kinds"
                    ))),
                }
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Selection, A::Error> {
                let mut kinds = BTreeSet::new();
                while let Some(kind) = seq.next_element::<SourceKind>()? {
                    kinds.insert(kind);
                }
                Ok(Selection::Kinds(kinds))
            }
        }

        deserializer.deserialize_any(SelectionVisitor)
    }
}

/// A kind of runtime source configuration.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Mcp,
    Skills,
    Plugins,
    Subagents,
    Hooks,
    /// The runtime's remaining settings files.
    Settings,
    /// Instruction files (`CLAUDE.md`, `AGENTS.md`, Codex
    /// `developer_instructions`).
    Memory,
}

impl SourceKind {
    pub const ALL: &'static [SourceKind] = &[
        SourceKind::Mcp,
        SourceKind::Skills,
        SourceKind::Plugins,
        SourceKind::Subagents,
        SourceKind::Hooks,
        SourceKind::Settings,
        SourceKind::Memory,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Mcp => "mcp",
            SourceKind::Skills => "skills",
            SourceKind::Plugins => "plugins",
            SourceKind::Subagents => "subagents",
            SourceKind::Hooks => "hooks",
            SourceKind::Settings => "settings",
            SourceKind::Memory => "memory",
        }
    }
}

/// The runtime sources one member may load, kind by kind.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    pub operator: BTreeSet<SourceKind>,
    pub project: BTreeSet<SourceKind>,
}

impl Sources {
    fn of(inherit: &Inherit) -> Self {
        let mut project = inherit.project.kinds();
        project.remove(&SourceKind::Mcp);
        Sources {
            operator: inherit.operator.kinds(),
            project,
        }
    }
}

impl Default for Sources {
    /// The built-in policy: none of the operator's configuration, all of the
    /// project's except its MCP servers.
    fn default() -> Self {
        Sources::of(&Inherit {
            operator: Selection::None,
            project: Selection::All,
        })
    }
}

/// The kind of a toolbox item, in canonical order.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Mcp,
    Skills,
    Plugins,
    Subagents,
    Hooks,
}

impl ItemKind {
    pub const ALL: &'static [ItemKind] = &[
        ItemKind::Mcp,
        ItemKind::Skills,
        ItemKind::Plugins,
        ItemKind::Subagents,
        ItemKind::Hooks,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Mcp => "mcp",
            ItemKind::Skills => "skills",
            ItemKind::Plugins => "plugins",
            ItemKind::Subagents => "subagents",
            ItemKind::Hooks => "hooks",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        ItemKind::ALL.iter().copied().find(|k| k.as_str() == word)
    }

    /// Kinds whose items are files snapshotted at launch.
    pub fn is_file(self) -> bool {
        matches!(
            self,
            ItemKind::Skills | ItemKind::Plugins | ItemKind::Subagents
        )
    }
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `<kind>.<name>`, ordered by kind (canonical order), then name (bytes).
/// It serializes as that string, so it can key a JSON map.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemRef {
    pub kind: ItemKind,
    pub name: String,
}

impl ItemRef {
    pub fn new(kind: ItemKind, name: &str) -> Self {
        ItemRef {
            kind,
            name: name.to_string(),
        }
    }
}

impl fmt::Display for ItemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.kind, self.name)
    }
}

impl std::str::FromStr for ItemRef {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        match Pattern::parse(text)? {
            Pattern::One(item) => Ok(item),
            Pattern::Kind(_) => bail!("'{text}' names a kind, not one item"),
        }
    }
}

impl Serialize for ItemRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ItemRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(de::Error::custom)
    }
}

/// One item's definition.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Item {
    Mcp(McpServer),
    File(FileItem),
    Hook(Hook),
}

impl Item {
    pub fn required(&self) -> bool {
        match self {
            Item::Mcp(m) => m.required,
            Item::File(f) => f.required,
            Item::Hook(h) => h.required,
        }
    }

    /// The runtimes the item applies on; empty means every one.
    pub fn runtimes(&self) -> &[RuntimeKind] {
        match self {
            Item::Mcp(m) => &m.runtimes,
            Item::File(f) => &f.runtimes,
            Item::Hook(h) => &h.runtimes,
        }
    }
}

/// Where a resolved item came from. It serializes as `default`,
/// `bundle:<name>` or `member`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Defined in `defaults.toolbox` itself.
    Default,
    /// Defined in this bundle (the innermost bundle that defines it).
    Bundle(String),
    /// Defined at the member's own level.
    Member,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Default => f.write_str("default"),
            Origin::Bundle(name) => write!(f, "bundle:{name}"),
            Origin::Member => f.write_str("member"),
        }
    }
}

impl Serialize for Origin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        match text.as_str() {
            "default" => Ok(Origin::Default),
            "member" => Ok(Origin::Member),
            other => match other.strip_prefix("bundle:") {
                Some(name) if is_valid_id(name) => Ok(Origin::Bundle(name.to_string())),
                _ => Err(de::Error::custom(format!(
                    "'{other}' is not default, member or bundle:<name>"
                ))),
            },
        }
    }
}

/// What a file item's entry in the canonical form records: the content hash
/// of its snapshot, or a fixed word for why there is none. The word never
/// carries error text, so the fingerprint stays stable while the item stays
/// in that state, and moves once the file appears or changes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    /// SHA-256 of the snapshot, 64 lowercase hex characters.
    Sha256(String),
    /// Never read: the item does not load on this runtime (Not applicable,
    /// Not available on <Runtime>). Edits to its files do not move the
    /// fingerprint.
    NotLoaded,
    /// The path does not exist (Failed preflight).
    Missing,
    /// The path is not the expected format (Failed preflight).
    Invalid,
    /// Larger than the snapshot limit (Failed preflight).
    TooLarge,
}

impl Content {
    fn canonical(&self) -> Result<String> {
        Ok(match self {
            Content::Sha256(hex) => {
                if hex.len() != 64
                    || !hex
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    bail!("'{hex}' is not 64 lowercase hex characters");
                }
                format!("sha256:{hex}")
            }
            Content::NotLoaded => "not-loaded".to_string(),
            Content::Missing => "unavailable:missing".to_string(),
            Content::Invalid => "unavailable:invalid".to_string(),
            Content::TooLarge => "unavailable:too-large".to_string(),
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolvedItem {
    pub item: Item,
    pub origin: Origin,
}

/// One member's effective toolbox before it is mapped onto a runtime.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Resolved {
    /// In canonical order.
    pub items: BTreeMap<ItemRef, ResolvedItem>,
    pub sources: Sources,
    pub strict: bool,
}

impl Toolbox {
    pub fn is_empty(&self) -> bool {
        *self == Toolbox::default()
    }

    /// The items this body defines itself, in canonical order.
    pub fn items(&self) -> Vec<(ItemRef, Item)> {
        let mut out = Vec::new();
        for (name, m) in &self.mcp {
            out.push((ItemRef::new(ItemKind::Mcp, name), Item::Mcp(m.clone())));
        }
        for (kind, map) in [
            (ItemKind::Skills, &self.skills),
            (ItemKind::Plugins, &self.plugins),
            (ItemKind::Subagents, &self.subagents),
        ] {
            for (name, f) in map {
                out.push((ItemRef::new(kind, name), Item::File(f.clone())));
            }
        }
        for (name, h) in &self.hooks {
            out.push((ItemRef::new(ItemKind::Hooks, name), Item::Hook(h.clone())));
        }
        out
    }

    /// Make every file item's path absolute: `~/` against `home`, anything
    /// relative against `base` (the directory of the file that defines it,
    /// itself absolute). `at` is this body's field path, for errors. Nothing
    /// is read, and links are not resolved.
    pub fn absolutize(&mut self, base: &Path, home: Option<&Path>, at: &str) -> Result<()> {
        if !base.is_absolute() {
            bail!(
                "{at}: file paths need an absolute base, not {}",
                base.display()
            );
        }
        for (kind, map) in [
            (ItemKind::Skills, &mut self.skills),
            (ItemKind::Plugins, &mut self.plugins),
            (ItemKind::Subagents, &mut self.subagents),
        ] {
            for (name, item) in map.iter_mut() {
                item.path = absolute(&item.path, base, home)
                    .map_err(|e| anyhow!("{at}.{kind}.{name}.path: {e}"))?;
            }
        }
        Ok(())
    }
}

fn absolute(path: &str, base: &Path, home: Option<&Path>) -> Result<String> {
    let joined = if path == "~" || path.starts_with("~/") {
        let home = home.filter(|home| home.is_absolute()).ok_or_else(|| {
            anyhow!("{path} starts with ~, but HOME is not set to an absolute path")
        })?;
        match path.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => home.to_path_buf(),
        }
    } else if path.starts_with('~') {
        bail!("{path}: only ~/ (your own home) is expanded; write the path in full");
    } else if Path::new(path).is_absolute() {
        return Ok(path.to_string());
    } else {
        base.join(path)
    };
    Ok(joined.to_string_lossy().into_owned())
}

/// A pattern of `exclude`: one item, or every item of a kind.
enum Pattern {
    One(ItemRef),
    Kind(ItemKind),
}

impl Pattern {
    fn parse(text: &str) -> Result<Self> {
        let (kind, name) = text
            .split_once('.')
            .ok_or_else(|| anyhow!("'{text}' is not <kind>.<name> or <kind>.*"))?;
        let kind = ItemKind::parse(kind).ok_or_else(|| {
            anyhow!(
                "'{kind}' is not a kind (expected {})",
                ItemKind::ALL
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        if name == "*" {
            Ok(Pattern::Kind(kind))
        } else if is_valid_id(name) {
            Ok(Pattern::One(ItemRef::new(kind, name)))
        } else {
            bail!("'{name}' is not an item name (expected [a-z][a-z0-9_-]{{0,31}})")
        }
    }

    fn matches(&self, item: &ItemRef) -> bool {
        match self {
            Pattern::One(one) => one == item,
            Pattern::Kind(kind) => item.kind == *kind,
        }
    }
}

/// The team's declarations a member's toolbox resolves against.
#[derive(Clone, Copy, Debug)]
pub struct Catalog<'a> {
    pub bundles: &'a BTreeMap<String, Toolbox>,
    pub defaults: &'a Toolbox,
    /// The legacy `defaults.inherit_mcp`: `inherit.operator: [mcp]` at the
    /// defaults level.
    pub inherit_mcp: bool,
}

impl Catalog<'_> {
    fn defaults_inherit(&self) -> Option<Inherit> {
        self.defaults.inherit.clone().or_else(|| {
            self.inherit_mcp.then(|| Inherit {
                operator: Selection::Kinds([SourceKind::Mcp].into_iter().collect()),
                project: Selection::All,
            })
        })
    }
}

/// Resolve one member's toolbox (`member`, found at `path` in the team file)
/// against the team's declarations, level by level: `defaults.toolbox`
/// (unless the member resets), then the member's own level. Each level is
/// finished before the next: its includes, depth-first in the order listed,
/// then its own items, a later definition replacing an earlier one whole,
/// then its exclusions, applied to the result so far.
pub fn resolve(catalog: Catalog<'_>, member: &Toolbox, path: &str) -> Result<Resolved> {
    let mut items = BTreeMap::new();
    let mut cache = HashMap::new();
    let mut levels: Vec<(&Toolbox, Origin, &str)> = Vec::new();
    if !member.reset {
        levels.push((catalog.defaults, Origin::Default, "defaults.toolbox"));
    }
    levels.push((member, Origin::Member, path));
    for (level, origin, at) in &levels {
        for (i, name) in level.include.iter().enumerate() {
            include(
                catalog.bundles,
                name,
                &format!("{at}.include[{i}]"),
                &mut cache,
                &mut items,
            )?;
        }
        for (item, def) in level.items() {
            items.insert(
                item,
                ResolvedItem {
                    item: def,
                    origin: origin.clone(),
                },
            );
        }
        for (i, text) in level.exclude.iter().enumerate() {
            let at = format!("{at}.exclude[{i}]");
            let pattern = Pattern::parse(text).map_err(|e| anyhow!("{at}: {e}"))?;
            let own: Vec<ItemRef> = level
                .items()
                .into_iter()
                .map(|(item, _)| item)
                .filter(|item| pattern.matches(item))
                .collect();
            if let Some(item) = own.first() {
                bail!("{at}: {text} excludes {item}, which this level defines itself");
            }
            let before = items.len();
            items.retain(|item, _| !pattern.matches(item));
            if items.len() == before {
                let names = items.keys().map(ItemRef::to_string);
                bail!("{at}: {text} matches no item{}", suggestion(text, names));
            }
        }
    }
    let inherit = member.inherit.clone().or_else(|| {
        if member.reset {
            None
        } else {
            catalog.defaults_inherit()
        }
    });
    Ok(Resolved {
        items,
        sources: inherit.as_ref().map(Sources::of).unwrap_or_default(),
        strict: levels.iter().any(|(level, _, _)| level.strict),
    })
}

/// A bundle's items once its includes are expanded: what including it adds,
/// each item with the origin of its last definition.
type Flat = BTreeMap<ItemRef, ResolvedItem>;

/// Add bundle `name`, fully expanded, to `items`. Expanding a bundle gives
/// the same map wherever it is included, so each bundle is expanded once per
/// resolution (`cache`) and a shared include graph costs no more than its
/// size. Inserting the expanded map over `items` keeps last-wins: an item the
/// bundle defines anywhere in its expansion replaces the earlier one, and
/// every other item stays.
fn include(
    bundles: &BTreeMap<String, Toolbox>,
    name: &str,
    at: &str,
    cache: &mut HashMap<String, Flat>,
    items: &mut BTreeMap<ItemRef, ResolvedItem>,
) -> Result<()> {
    let flat = flatten(bundles, name, at, &mut Vec::new(), cache)?;
    items.extend(flat.iter().map(|(k, v)| (k.clone(), v.clone())));
    Ok(())
}

fn flatten<'c>(
    bundles: &BTreeMap<String, Toolbox>,
    name: &str,
    at: &str,
    stack: &mut Vec<String>,
    cache: &'c mut HashMap<String, Flat>,
) -> Result<&'c Flat> {
    if !cache.contains_key(name) {
        if stack.iter().any(|n| n == name) {
            stack.push(name.to_string());
            bail!("{at}: bundles include each other: {}", stack.join(" -> "));
        }
        let Some(bundle) = bundles.get(name) else {
            bail!(
                "{at}: unknown toolbox '{name}'{}",
                suggestion(name, bundles.keys().cloned())
            );
        };
        stack.push(name.to_string());
        let mut flat = Flat::new();
        for (i, inner) in bundle.include.iter().enumerate() {
            let at = format!("toolboxes.{name}.include[{i}]");
            let inner = flatten(bundles, inner, &at, stack, cache)?;
            flat.extend(inner.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        for (item, def) in bundle.items() {
            flat.insert(
                item,
                ResolvedItem {
                    item: def,
                    origin: Origin::Bundle(name.to_string()),
                },
            );
        }
        stack.pop();
        cache.insert(name.to_string(), flat);
    }
    Ok(&cache[name])
}

/// " (did you mean X?)" for the closest candidate within two edits.
fn suggestion(target: &str, candidates: impl Iterator<Item = String>) -> String {
    candidates
        .map(|c| (edit_distance(target, &c), c))
        .filter(|(d, _)| *d <= 2)
        .min()
        .map(|(_, c)| format!(" (did you mean {c}?)"))
        .unwrap_or_default()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (prev + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// Where a toolbox body sits, which decides the fields it may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Bundle,
    Defaults,
    Member,
}

/// Check one toolbox body found at `path`: its own fields only. Includes are
/// checked against the bundles by [`validate_catalog`] and
/// [`check_includes`], exclusions by [`resolve`].
pub fn validate_body(body: &Toolbox, level: Level, path: &str) -> Result<()> {
    if level == Level::Bundle {
        for (set, field) in [
            (!body.exclude.is_empty(), "exclude"),
            (body.inherit.is_some(), "inherit"),
            (body.strict, "strict"),
        ] {
            if set {
                bail!(
                    "{path}.{field}: a bundle cannot set {field}; set it in defaults.toolbox \
or the member's toolbox"
                );
            }
        }
    }
    if body.reset && level != Level::Member {
        bail!("{path}.reset: only a member's toolbox can reset");
    }
    for (i, name) in body.include.iter().enumerate() {
        if !is_valid_id(name) {
            bail!("{path}.include[{i}]: '{name}' is not a toolbox name (expected [a-z][a-z0-9_-]{{0,31}})");
        }
    }
    for (i, text) in body.exclude.iter().enumerate() {
        Pattern::parse(text).map_err(|e| anyhow!("{path}.exclude[{i}]: {e}"))?;
    }
    if let Some(inherit) = &body.inherit {
        if inherit.project.kinds().contains(&SourceKind::Mcp) && inherit.project != Selection::All {
            bail!(
                "{path}.inherit.project: project MCP servers cannot be inherited; declare them \
under mcp"
            );
        }
    }
    for kind in ItemKind::ALL {
        let count = body.items().iter().filter(|(i, _)| i.kind == *kind).count();
        if count > MAX_ITEMS {
            bail!("{path}.{kind}: at most {MAX_ITEMS} items");
        }
    }
    for (item, def) in body.items() {
        let at = format!("{path}.{item}");
        if !is_valid_id(&item.name) {
            bail!(
                "{path}.{}: '{}' is not an item name (expected [a-z][a-z0-9_-]{{0,31}})",
                item.kind,
                item.name
            );
        }
        if RESERVED_NAMES.contains(&item.name.as_str())
            || (item.kind == ItemKind::Mcp && item.name == RESERVED_MCP)
        {
            bail!("{at}: the name '{}' is reserved for crew", item.name);
        }
        validate_runtimes(def.runtimes(), &at)?;
        match &def {
            Item::Mcp(server) => validate_mcp(server, &at)?,
            Item::File(file) => validate_text(&file.path, &format!("{at}.path"), MAX_TEXT_BYTES)?,
            Item::Hook(hook) => validate_hook(hook, &at)?,
        }
    }
    Ok(())
}

fn validate_runtimes(runtimes: &[RuntimeKind], at: &str) -> Result<()> {
    for (i, runtime) in runtimes.iter().enumerate() {
        if *runtime == RuntimeKind::Shell {
            bail!("{at}.runtimes[{i}]: shell members load no toolbox items");
        }
        if runtimes[..i].contains(runtime) {
            bail!("{at}.runtimes[{i}]: {runtime} is listed twice");
        }
    }
    Ok(())
}

fn validate_mcp(server: &McpServer, at: &str) -> Result<()> {
    match (&server.url, server.command.is_empty()) {
        (Some(_), false) => bail!("{at}: give either command or url, not both"),
        (None, true) => bail!("{at}: give command (a local server) or url (a remote one)"),
        (None, false) => {
            validate_command(&server.command, &format!("{at}.command"))?;
            if server.transport.is_some() || !server.headers.is_empty() {
                bail!("{at}: transport and headers are for remote servers (url)");
            }
            if let Some(cwd) = &server.cwd {
                let path = Path::new(cwd);
                if cwd.is_empty()
                    || path.is_absolute()
                    || path.components().any(|c| matches!(c, Component::ParentDir))
                {
                    bail!("{at}.cwd: a relative path inside the member's worktree");
                }
                validate_text(cwd, &format!("{at}.cwd"), MAX_TEXT_BYTES)?;
            }
            for (name, reference) in &server.env {
                if !is_env_name(name) {
                    bail!("{at}.env: '{name}' is not a variable name");
                }
                validate_reference(reference, &format!("{at}.env.{name}"))?;
            }
        }
        (Some(url), true) => {
            if !(url.starts_with("https://") || url.starts_with("http://"))
                || url.len() > MAX_URL_BYTES
                || url.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                bail!("{at}.url: an http:// or https:// URL without spaces");
            }
            if !server.env.is_empty() || server.cwd.is_some() {
                bail!("{at}: env and cwd are for local servers (command)");
            }
            let mut seen = BTreeSet::new();
            for (name, reference) in &server.headers {
                if !is_header_name(name) {
                    bail!("{at}.headers: '{name}' is not a header name");
                }
                if !seen.insert(name.to_ascii_lowercase()) {
                    bail!("{at}.headers: '{name}' is given twice");
                }
                validate_reference(reference, &format!("{at}.headers.{name}"))?;
            }
        }
    }
    Ok(())
}

fn validate_hook(hook: &Hook, at: &str) -> Result<()> {
    if hook.event.is_empty()
        || hook.event.len() > 64
        || !hook.event.starts_with(|c: char| c.is_ascii_alphabetic())
        || !hook.event.chars().all(|c| c.is_ascii_alphanumeric())
    {
        bail!("{at}.event: an event name such as SessionStart");
    }
    if let Some(matcher) = &hook.matcher {
        validate_text(matcher, &format!("{at}.matcher"), 256)?;
    }
    validate_command(&hook.command, &format!("{at}.command"))
}

fn validate_command(command: &[String], at: &str) -> Result<()> {
    if command.is_empty() || command[0].is_empty() {
        bail!("{at}: a program and its arguments");
    }
    if command.len() > MAX_ARGS {
        bail!("{at}: at most {MAX_ARGS} arguments");
    }
    for (i, arg) in command.iter().enumerate() {
        if arg.len() > MAX_TEXT_BYTES || arg.contains('\0') {
            bail!("{at}[{i}]: at most {MAX_TEXT_BYTES} bytes and no NUL");
        }
    }
    Ok(())
}

fn validate_text(text: &str, at: &str, max: usize) -> Result<()> {
    if text.is_empty() || text.len() > max || text.chars().any(char::is_control) {
        bail!("{at}: 1 to {max} bytes of printable text");
    }
    Ok(())
}

fn validate_reference(reference: &Reference, at: &str) -> Result<()> {
    match reference {
        Reference::Env { name, format } => {
            if !is_env_name(name) {
                bail!("{at}.env: '{name}' is not a variable name");
            }
            if PROTECTED_ENV.contains(&name.as_str()) {
                bail!(
                    "{at}.env: {name} carries a member's model credentials or crew's own \
settings and cannot be passed to a server"
                );
            }
            if let Some(format) = format {
                if format.matches("{}").count() != 1 {
                    bail!("{at}.format: exactly one {{}}, where the value goes");
                }
                validate_text(format, &format!("{at}.format"), 256)?;
            }
        }
        Reference::Value(value) => {
            if value.len() > MAX_TEXT_BYTES || value.contains('\0') {
                bail!("{at}.value: at most {MAX_TEXT_BYTES} bytes and no NUL");
            }
        }
    }
    Ok(())
}

/// `[A-Za-z_][A-Za-z0-9_]*`, at most 128 bytes.
pub fn is_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
}

/// Every include in `body` (at `path`) must name a bundle.
pub fn check_includes(
    body: &Toolbox,
    bundles: &BTreeMap<String, Toolbox>,
    path: &str,
) -> Result<()> {
    for (i, name) in body.include.iter().enumerate() {
        if !bundles.contains_key(name) {
            bail!(
                "{path}.include[{i}]: unknown toolbox '{name}'{}",
                suggestion(name, bundles.keys().cloned())
            );
        }
    }
    Ok(())
}

/// Check the team-wide declarations: bundle names and bodies, includes,
/// include cycles, `defaults.toolbox`, and `inherit_mcp` beside `inherit`.
pub fn validate_catalog(catalog: Catalog<'_>) -> Result<()> {
    if catalog.bundles.len() > MAX_BUNDLES {
        bail!("toolboxes: at most {MAX_BUNDLES} bundles");
    }
    for (name, bundle) in catalog.bundles {
        if !is_valid_id(name) {
            bail!("toolboxes: '{name}' is not a toolbox name (expected [a-z][a-z0-9_-]{{0,31}})");
        }
        let path = format!("toolboxes.{name}");
        validate_body(bundle, Level::Bundle, &path)?;
        check_includes(bundle, catalog.bundles, &path)?;
    }
    let graph: HashMap<&str, Vec<&str>> = catalog
        .bundles
        .iter()
        .map(|(name, b)| {
            (
                name.as_str(),
                b.include.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    if let Some(cycle) = find_cycle(&graph) {
        bail!(
            "toolboxes: bundles include each other: {}",
            cycle.join(" -> ")
        );
    }
    validate_body(catalog.defaults, Level::Defaults, "defaults.toolbox")?;
    check_includes(catalog.defaults, catalog.bundles, "defaults.toolbox")?;
    if catalog.inherit_mcp && catalog.defaults.inherit.is_some() {
        bail!(
            "defaults.inherit_mcp and defaults.toolbox.inherit are both set; replace \
inherit_mcp: true with defaults.toolbox.inherit.operator: [mcp]"
        );
    }
    // Defaults' exclusions act on defaults' own bundles whoever the member is.
    resolve(catalog, &Toolbox::default(), "defaults.toolbox")?;
    Ok(())
}

impl Resolved {
    /// The canonical form: kinds in canonical order, items by name, keys
    /// sorted, file items by their [`Content`] (every file item needs an
    /// entry) instead of their path, no origin and no status, nothing that
    /// varies between launches. Byte-identical for the same meaning.
    pub fn canonical(&self, content: &BTreeMap<ItemRef, Content>) -> Result<Vec<u8>> {
        let mut items = Vec::new();
        for (item, resolved) in &self.items {
            let def = match &resolved.item {
                Item::Mcp(server) => mcp_value(server),
                Item::File(file) => {
                    let entry = content
                        .get(item)
                        .ok_or_else(|| anyhow!("no content entry for {item}"))?
                        .canonical()
                        .map_err(|e| anyhow!("{item}: {e}"))?;
                    object(vec![
                        ("content", Value::from(entry)),
                        ("required", Value::from(file.required)),
                        ("runtimes", runtimes_value(&file.runtimes)),
                    ])
                }
                Item::Hook(hook) => {
                    let mut fields = vec![
                        ("command", Value::from(hook.command.clone())),
                        ("event", Value::from(hook.event.as_str())),
                        ("required", Value::from(hook.required)),
                        ("runtimes", runtimes_value(&hook.runtimes)),
                    ];
                    if let Some(matcher) = &hook.matcher {
                        fields.push(("matcher", Value::from(matcher.as_str())));
                    }
                    object(fields)
                }
            };
            items.push(object(vec![
                ("def", def),
                ("kind", Value::from(item.kind.as_str())),
                ("name", Value::from(item.name.as_str())),
            ]));
        }
        let kinds = |set: &BTreeSet<SourceKind>| {
            Value::from(set.iter().map(|k| k.as_str()).collect::<Vec<_>>())
        };
        let form = object(vec![
            (
                "inherit",
                object(vec![
                    ("operator", kinds(&self.sources.operator)),
                    ("project", kinds(&self.sources.project)),
                ]),
            ),
            ("items", Value::Array(items)),
            ("strict", Value::from(self.strict)),
            ("version", Value::from(CANONICAL_VERSION)),
        ]);
        Ok(serde_json::to_vec(&form)?)
    }
}

fn mcp_value(server: &McpServer) -> Value {
    let references = |map: &BTreeMap<String, Reference>| {
        object(
            map.iter()
                .map(|(name, r)| {
                    let value = match r {
                        Reference::Env { name, format } => {
                            let mut fields = vec![("env", Value::from(name.as_str()))];
                            if let Some(format) = format {
                                fields.push(("format", Value::from(format.as_str())));
                            }
                            object(fields)
                        }
                        Reference::Value(value) => {
                            object(vec![("value", Value::from(value.as_str()))])
                        }
                    };
                    (name.as_str(), value)
                })
                .collect(),
        )
    };
    let mut fields = vec![
        ("required", Value::from(server.required)),
        ("runtimes", runtimes_value(&server.runtimes)),
    ];
    match &server.url {
        Some(url) => {
            fields.push(("url", Value::from(url.as_str())));
            fields.push((
                "transport",
                Value::from(server.transport.unwrap_or_default().as_str()),
            ));
            fields.push(("headers", references(&server.headers)));
        }
        None => {
            fields.push(("command", Value::from(server.command.clone())));
            fields.push(("env", references(&server.env)));
            if let Some(cwd) = &server.cwd {
                fields.push(("cwd", Value::from(cwd.as_str())));
            }
        }
    }
    object(fields)
}

fn runtimes_value(runtimes: &[RuntimeKind]) -> Value {
    let mut names: Vec<&str> = runtimes.iter().map(|r| r.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    Value::from(names)
}

/// A JSON object with its keys inserted in sorted order, so the bytes are
/// the same whether or not `serde_json` preserves insertion order.
fn object(mut fields: Vec<(&str, Value)>) -> Value {
    fields.sort_by(|a, b| a.0.cmp(b.0));
    let mut map = Map::new();
    for (key, value) in fields {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// SHA-256 of a member's canonical toolbox, its runtime and the runtime
/// mapping's version, as 64 lowercase hex characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fingerprint(String);

impl Fingerprint {
    pub fn of(canonical: &[u8], runtime: RuntimeKind, mapping_version: u32) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"crew-toolbox\0");
        hasher.update(canonical);
        hasher.update(b"\0");
        hasher.update(runtime.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(mapping_version.to_string().as_bytes());
        let digest = hasher.finalize();
        Fingerprint(digest.iter().map(|b| format!("{b:02x}")).collect())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first 12 hex characters, as dashboards show it.
    pub fn short(&self) -> &str {
        self.0.get(..12).unwrap_or(&self.0)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Style, TeamConfig};

    const EXAMPLE: &str = r#"
project: shop
toolboxes:
  base:
    mcp:
      context7:
        command: [npx, -y, "@upstash/context7-mcp"]
  web:
    include: [base]
    mcp:
      playwright:
        command: [npx, "@playwright/mcp", --headless]
        required: true
    skills:
      frontend-qa:
        path: toolbox/skills/frontend-qa
  github:
    mcp:
      github:
        command: [github-mcp-server, stdio]
        env:
          GITHUB_TOKEN: { env: SHOP_GITHUB_TOKEN }
          GITHUB_TOOLSETS: { value: "repos,issues,pull_requests" }
      docs:
        url: https://docs.example.com/mcp
        headers:
          Authorization: { env: DOCS_TOKEN, format: "Bearer {}" }
defaults:
  runtime: claude
  toolbox:
    include: [base]
    inherit:
      operator: none
orchestrator:
  toolbox:
    reset: true
agents:
  - id: qa
    role: frontend QA
    toolbox:
      include: [web]
  - id: release
    role: release notes
    runtime: codex
    toolbox:
      include: [github]
      exclude: [mcp.context7]
      skills:
        changelog: { path: toolbox/skills/changelog }
      plugins:
        lint-kit: { path: ~/plugins/lint-kit }
"#;

    fn member(team: &TeamConfig, id: &str) -> Resolved {
        let (i, agent) = team
            .agents
            .iter()
            .enumerate()
            .find(|(_, a)| a.id == id)
            .unwrap();
        team.resolve_toolbox(&agent.toolbox, &format!("agents[{i}].toolbox"))
            .unwrap()
    }

    fn listing(resolved: &Resolved) -> Vec<String> {
        resolved
            .items
            .iter()
            .map(|(item, r)| format!("{item} {}", r.origin))
            .collect()
    }

    /// A stand-in snapshot hash per file item, derived from its reference.
    fn hash_of(item: &ItemRef) -> String {
        let digest = Sha256::digest(item.to_string().as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn hashes(resolved: &Resolved) -> BTreeMap<ItemRef, Content> {
        resolved
            .items
            .keys()
            .filter(|item| item.kind.is_file())
            .map(|item| (item.clone(), Content::Sha256(hash_of(item))))
            .collect()
    }

    fn fingerprint(resolved: &Resolved) -> Fingerprint {
        let canonical = resolved.canonical(&hashes(resolved)).unwrap();
        Fingerprint::of(&canonical, RuntimeKind::Claude, 1)
    }

    fn error(yaml: &str) -> String {
        format!("{:#}", TeamConfig::from_yaml(yaml).unwrap_err())
    }

    #[test]
    fn the_example_team_resolves_level_by_level() {
        let team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        assert_eq!(
            listing(&member(&team, "qa")),
            [
                "mcp.context7 bundle:base",
                "mcp.playwright bundle:web",
                "skills.frontend-qa bundle:web",
            ]
        );
        let release = member(&team, "release");
        assert_eq!(
            listing(&release),
            [
                "mcp.docs bundle:github",
                "mcp.github bundle:github",
                "skills.changelog member",
                "plugins.lint-kit member",
            ]
        );
        assert!(release.sources.operator.is_empty());
        assert!(!release.sources.project.contains(&SourceKind::Mcp));
        assert!(release.sources.project.contains(&SourceKind::Skills));
        let orchestrator = team
            .resolve_toolbox(&team.orchestrator.toolbox, "orchestrator.toolbox")
            .unwrap();
        assert!(orchestrator.items.is_empty());
        assert_eq!(orchestrator.sources, Sources::default());
    }

    #[test]
    fn a_team_without_toolboxes_keeps_its_shape_and_resolves_empty() {
        for input in [
            "project: legacy\n",
            "project: legacy\nagents:\n  - id: a\n    role: worker\n",
        ] {
            let team = TeamConfig::from_yaml(input).unwrap();
            let yaml = team.to_yaml().unwrap();
            assert!(!yaml.contains("toolbox"), "{yaml}");
            let json = serde_json::to_string(&team).unwrap();
            assert!(!json.contains("toolbox"), "{json}");
            for agent in &team.agents {
                let resolved = team.resolve_toolbox(&agent.toolbox, "agent").unwrap();
                assert!(resolved.items.is_empty());
                assert_eq!(resolved.sources, Sources::default());
                assert!(!resolved.strict);
            }
        }
        let team = TeamConfig::from_yaml(
            "project: legacy\ndefaults:\n  inherit_mcp: true\nagents:\n  - id: a\n",
        )
        .unwrap();
        let resolved = team
            .resolve_toolbox(&team.agents[0].toolbox, "agents[0].toolbox")
            .unwrap();
        assert_eq!(
            resolved.sources.operator,
            [SourceKind::Mcp].into_iter().collect()
        );
        assert_eq!(resolved.sources.project, Sources::default().project);
    }

    #[test]
    fn reordering_without_changing_meaning_keeps_the_canonical_bytes() {
        let reordered = r#"
project: shop
agents:
  - id: release
    toolbox:
      plugins:
        lint-kit: { path: ~/plugins/lint-kit }
      skills:
        changelog: { path: toolbox/skills/changelog }
      exclude: [mcp.context7]
      include: [github]
    runtime: codex
    role: release notes
  - id: qa
    toolbox:
      include: [web]
    role: frontend QA
orchestrator:
  toolbox:
    reset: true
defaults:
  toolbox:
    inherit:
      project: all
    include: [base]
  runtime: claude
toolboxes:
  github:
    mcp:
      docs:
        transport: http
        headers:
          Authorization: { format: "Bearer {}", env: DOCS_TOKEN }
        url: https://docs.example.com/mcp
      github:
        env:
          GITHUB_TOOLSETS: { value: "repos,issues,pull_requests" }
          GITHUB_TOKEN: { env: SHOP_GITHUB_TOKEN }
        command: [github-mcp-server, stdio]
  web:
    skills:
      frontend-qa:
        path: toolbox/skills/frontend-qa
    mcp:
      playwright:
        required: true
        command: [npx, "@playwright/mcp", --headless]
    include: [base]
  base:
    mcp:
      context7:
        command: [npx, -y, "@upstash/context7-mcp"]
"#;
        let a = TeamConfig::from_yaml(EXAMPLE).unwrap();
        let b = TeamConfig::from_yaml(reordered).unwrap();
        for id in ["qa", "release"] {
            let (ra, rb) = (member(&a, id), member(&b, id));
            assert_eq!(
                ra.canonical(&hashes(&ra)).unwrap(),
                rb.canonical(&hashes(&rb)).unwrap()
            );
            assert_eq!(fingerprint(&ra), fingerprint(&rb));
        }
        // A change of meaning changes the fingerprint.
        let changed = EXAMPLE.replace("--headless", "--headed");
        let c = TeamConfig::from_yaml(&changed).unwrap();
        assert_ne!(
            fingerprint(&member(&a, "qa")),
            fingerprint(&member(&c, "qa"))
        );
    }

    #[test]
    fn the_canonical_form_is_exact_and_carries_no_paths_or_origins() {
        let team = TeamConfig::from_yaml(
            r#"
project: canon
agents:
  - id: a
    toolbox:
      strict: true
      inherit: { operator: [mcp] }
      hooks:
        audit: { event: PreToolUse, matcher: Bash, command: [audit, --json] }
      skills:
        notes: { path: /srv/skills/notes, required: true, runtimes: [pi, claude] }
      mcp:
        docs:
          url: https://docs.example.com/mcp
          headers:
            Authorization: { env: DOCS_TOKEN, format: "Bearer {}" }
        local:
          command: [srv, --stdio]
          cwd: tools
          env:
            MODE: { value: ro }
"#,
        )
        .unwrap();
        let resolved = member(&team, "a");
        let canonical = String::from_utf8(resolved.canonical(&hashes(&resolved)).unwrap()).unwrap();
        assert_eq!(
            canonical,
            [
                r#"{"inherit":{"operator":["mcp"],"project":["skills","plugins","subagents","hooks","settings","memory"]},"#,
                r#""items":["#,
                r#"{"def":{"headers":{"Authorization":{"env":"DOCS_TOKEN","format":"Bearer {}"}},"required":false,"runtimes":[],"transport":"http","url":"https://docs.example.com/mcp"},"kind":"mcp","name":"docs"},"#,
                r#"{"def":{"command":["srv","--stdio"],"cwd":"tools","env":{"MODE":{"value":"ro"}},"required":false,"runtimes":[]},"kind":"mcp","name":"local"},"#,
                &format!(r#"{{"def":{{"content":"sha256:{}","required":true,"runtimes":["claude","pi"]}},"kind":"skills","name":"notes"}},"#, hash_of(&ItemRef::new(ItemKind::Skills, "notes"))),
                r#"{"def":{"command":["audit","--json"],"event":"PreToolUse","matcher":"Bash","required":false,"runtimes":[]},"kind":"hooks","name":"audit"}"#,
                r#"],"strict":true,"version":1}"#,
            ]
            .concat()
        );
        assert!(!canonical.contains("/srv/skills"));
        let err = resolved.canonical(&BTreeMap::new()).unwrap_err();
        assert_eq!(err.to_string(), "no content entry for skills.notes");
    }

    #[test]
    fn fingerprints_depend_on_runtime_and_mapping_version() {
        let team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        let qa = member(&team, "qa");
        let canonical = qa.canonical(&hashes(&qa)).unwrap();
        let claude = Fingerprint::of(&canonical, RuntimeKind::Claude, 1);
        assert_eq!(claude, Fingerprint::of(&canonical, RuntimeKind::Claude, 1));
        assert_eq!(claude.as_str().len(), 64);
        assert!(claude
            .as_str()
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(claude.short(), &claude.as_str()[..12]);
        assert_ne!(claude, Fingerprint::of(&canonical, RuntimeKind::Codex, 1));
        assert_ne!(claude, Fingerprint::of(&canonical, RuntimeKind::Claude, 2));
    }

    #[test]
    fn exclusions_and_resets_apply_per_level() {
        let team = TeamConfig::from_yaml(
            r#"
project: levels
toolboxes:
  base:
    mcp:
      context7: { command: [ctx] }
      docs: { url: "https://a.example/mcp" }
  extra:
    skills:
      notes: { path: /s/notes }
defaults:
  toolbox:
    include: [base, extra]
    exclude: [mcp.context7]
    strict: true
    inherit: { operator: all }
agents:
  - id: again
    toolbox:
      include: [base]
  - id: redefine
    toolbox:
      mcp:
        docs: { url: "https://b.example/mcp" }
  - id: fresh
    toolbox:
      reset: true
      include: [base]
  - id: wildcard
    toolbox:
      exclude: [skills.*]
"#,
        )
        .unwrap();
        // Defaults' exclusion acts on defaults' bundles; the member brings it back.
        let again = member(&team, "again");
        assert_eq!(
            listing(&again),
            [
                "mcp.context7 bundle:base",
                "mcp.docs bundle:base",
                "skills.notes bundle:extra"
            ]
        );
        assert!(again.strict);
        assert_eq!(again.sources.operator.len(), SourceKind::ALL.len());
        // A member's definition replaces the earlier one whole.
        let redefine = member(&team, "redefine");
        let docs = &redefine.items[&ItemRef::new(ItemKind::Mcp, "docs")];
        assert_eq!(docs.origin, Origin::Member);
        assert_eq!(
            docs.item,
            Item::Mcp(McpServer {
                url: Some("https://b.example/mcp".into()),
                ..McpServer::default()
            })
        );
        // Reset drops defaults' items, exclusions, inherit and strict.
        let fresh = member(&team, "fresh");
        assert_eq!(
            listing(&fresh),
            ["mcp.context7 bundle:base", "mcp.docs bundle:base"]
        );
        assert!(!fresh.strict);
        assert_eq!(fresh.sources, Sources::default());
        let wildcard = member(&team, "wildcard");
        assert_eq!(listing(&wildcard), ["mcp.docs bundle:base"]);
    }

    #[test]
    fn validation_names_the_field() {
        let cases: &[(&str, &str)] = &[
            (
                "toolboxes:\n  a: { include: [b] }\n  b: { include: [a] }\n",
                "toolboxes: bundles include each other: a -> b -> a",
            ),
            (
                "toolboxes:\n  web: {}\ndefaults:\n  toolbox: { include: [webb] }\n",
                "defaults.toolbox.include[0]: unknown toolbox 'webb' (did you mean web?)",
            ),
            (
                "toolboxes:\n  base: { mcp: { context7: { command: [c] } } }\n\
agents:\n  - id: a\n  - id: b\n  - id: c\n    toolbox: { include: [base], exclude: [mcp.contex7] }\n",
                "agents[2].toolbox.exclude[0]: mcp.contex7 matches no item (did you mean mcp.context7?)",
            ),
            (
                "toolboxes:\n  base: { mcp: { c: { command: [c] } } }\n\
agents:\n  - id: a\n    toolbox: { include: [base], exclude: [mcp.c, mcp.c] }\n",
                "agents[0].toolbox.exclude[1]: mcp.c matches no item",
            ),
            (
                "agents:\n  - id: a\n    toolbox:\n      mcp: { c: { command: [c] } }\n      exclude: [mcp.c]\n",
                "agents[0].toolbox.exclude[0]: mcp.c excludes mcp.c, which this level defines itself",
            ),
            (
                "defaults:\n  toolbox: { mcp: { crew: { command: [c] } } }\n",
                "defaults.toolbox.mcp.crew: the name 'crew' is reserved for crew",
            ),
            (
                "defaults:\n  toolbox: { skills: { crew-toolbox: { path: /x } } }\n",
                "defaults.toolbox.skills.crew-toolbox: the name 'crew-toolbox' is reserved for crew",
            ),
            (
                "defaults:\n  toolbox: { mcp: { g: { command: [g], env: { TOKEN: abc } } } }\n",
                "a plain string is not allowed here",
            ),
            (
                "defaults:\n  inherit_mcp: true\n  toolbox: { inherit: { operator: none } }\n",
                "defaults.inherit_mcp and defaults.toolbox.inherit are both set",
            ),
            (
                "defaults:\n  toolbox: { mcp: { g: { command: [g], env: { KEY: { env: OPENAI_API_KEY } } } } }\n",
                "defaults.toolbox.mcp.g.env.KEY.env: OPENAI_API_KEY carries a member's model credentials",
            ),
            (
                "toolboxes:\n  web: { exclude: [mcp.x] }\n",
                "toolboxes.web.exclude: a bundle cannot set exclude",
            ),
            (
                "defaults:\n  toolbox: { reset: true }\n",
                "defaults.toolbox.reset: only a member's toolbox can reset",
            ),
            (
                "defaults:\n  toolbox: { inherit: { project: [mcp] } }\n",
                "defaults.toolbox.inherit.project: project MCP servers cannot be inherited",
            ),
            (
                "defaults:\n  toolbox: { mcp: { g: { command: [g], url: \"https://x\" } } }\n",
                "defaults.toolbox.mcp.g: give either command or url, not both",
            ),
            (
                "defaults:\n  toolbox: { mcp: { g: { url: \"https://x\", headers: { A: { env: T, format: Bearer } } } } }\n",
                "defaults.toolbox.mcp.g.headers.A.format: exactly one {}",
            ),
            (
                "defaults:\n  toolbox: { skills: { s: { path: /s, runtimes: [shell] } } }\n",
                "defaults.toolbox.skills.s.runtimes[0]: shell members load no toolbox items",
            ),
            (
                "defaults:\n  toolbox: { mcp: { g: { command: [g], cwd: ../up } } }\n",
                "defaults.toolbox.mcp.g.cwd: a relative path inside the member's worktree",
            ),
            (
                "defaults:\n  toolbox: { exclude: [tools.x] }\n",
                "defaults.toolbox.exclude[0]: 'tools' is not a kind",
            ),
            (
                "orchestrator:\n  agent: false\n  toolbox: { reset: true }\nagents:\n  - id: a\n",
                "orchestrator.toolbox is set, but orchestrator.agent is false",
            ),
        ];
        for (body, expected) in cases {
            let yaml = format!("project: checks\n{body}");
            let message = error(&yaml);
            assert!(message.contains(expected), "{yaml}\n{message}");
        }
    }

    #[test]
    fn references_and_selections_round_trip_through_yaml_json_and_toml() {
        let team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        let yaml = team.to_yaml().unwrap();
        assert_eq!(TeamConfig::from_yaml(&yaml).unwrap(), team);
        let json = serde_json::to_value(&team).unwrap();
        assert_eq!(
            json["toolboxes"]["github"]["mcp"]["docs"]["headers"]["Authorization"],
            serde_json::json!({ "env": "DOCS_TOKEN", "format": "Bearer {}" })
        );
        assert_eq!(serde_json::from_value::<TeamConfig>(json).unwrap(), team);
        let toml = toml::to_string(&team).unwrap();
        assert_eq!(TeamConfig::from_toml(&toml).unwrap(), team);
        let inherit: Inherit =
            serde_yaml::from_str("operator: [memory, mcp]\nproject: none\n").unwrap();
        assert_eq!(
            serde_yaml::to_string(&inherit).unwrap(),
            "operator:\n- mcp\n- memory\nproject: none\n"
        );
        for (input, expected) in [
            (
                "{ env: A, value: b }",
                "give either `env` or `value`, not both",
            ),
            ("{ format: \"{}\" }", "`format` needs `env`"),
            ("{ name: A }", "unknown field `name`"),
            ("{}", "expected `env` or `value`"),
        ] {
            let err = serde_yaml::from_str::<Reference>(input).unwrap_err();
            assert!(err.to_string().contains(expected), "{input}: {err}");
        }
    }

    #[test]
    fn file_paths_become_absolute_against_their_file() {
        let mut team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        team.absolutize_toolbox_paths(Path::new("/work/shop"), Some(Path::new("/home/op")))
            .unwrap();
        assert_eq!(
            team.toolboxes["web"].skills["frontend-qa"].path,
            "/work/shop/toolbox/skills/frontend-qa"
        );
        let release = &team.agents[1].toolbox;
        assert_eq!(
            release.skills["changelog"].path,
            "/work/shop/toolbox/skills/changelog"
        );
        assert_eq!(
            release.plugins["lint-kit"].path,
            "/home/op/plugins/lint-kit"
        );
        let mut absolute = Toolbox::default();
        absolute.skills.insert(
            "s".into(),
            FileItem {
                path: "/already/there".into(),
                ..FileItem::default()
            },
        );
        absolute.absolutize(Path::new("/base"), None, "t").unwrap();
        assert_eq!(absolute.skills["s"].path, "/already/there");
    }

    #[test]
    fn a_home_path_without_a_home_is_refused_with_its_field() {
        let mut team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        let err = team
            .absolutize_toolbox_paths(Path::new("/work/shop"), None)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "agents[1].toolbox.plugins.lint-kit.path: ~/plugins/lint-kit starts with ~, \
but HOME is not set to an absolute path"
        );
        let mut team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        let err = team
            .absolutize_toolbox_paths(Path::new("/work/shop"), Some(Path::new("relative")))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("HOME is not set to an absolute path"),
            "{err}"
        );
        let mut other = Toolbox::default();
        other.skills.insert(
            "s".into(),
            FileItem {
                path: "~bob/skills/s".into(),
                ..FileItem::default()
            },
        );
        let err = other
            .absolutize(
                Path::new("/base"),
                Some(Path::new("/home/op")),
                "defaults.toolbox",
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "defaults.toolbox.skills.s.path: ~bob/skills/s: only ~/ (your own home) is \
expanded; write the path in full"
        );
        let err = other
            .absolutize(Path::new("base"), None, "defaults.toolbox")
            .unwrap_err();
        assert!(err.to_string().contains("need an absolute base"), "{err}");
    }

    #[test]
    fn a_team_file_on_disk_gets_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("crew.yaml");
        std::fs::write(&file, EXAMPLE.replace("~/plugins", "plugins")).unwrap();
        let team = TeamConfig::load(&file).unwrap();
        assert_eq!(
            Path::new(&team.toolboxes["web"].skills["frontend-qa"].path),
            dir.path().join("toolbox/skills/frontend-qa")
        );
        // A relative base, as a relative team file path or style folder
        // gives, is taken from the current directory.
        let cwd = std::env::current_dir().unwrap();
        let mut team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        team.absolutize_toolbox_paths(Path::new("teams/shop"), Some(Path::new("/home/op")))
            .unwrap();
        assert_eq!(
            Path::new(&team.toolboxes["web"].skills["frontend-qa"].path),
            cwd.join("teams/shop/toolbox/skills/frontend-qa")
        );
        let mut style = Style::parse(
            "qa",
            "toolboxes:\n  web: { skills: { s: { path: skills/s } } }\n",
            false,
        )
        .unwrap();
        style
            .absolutize_toolbox_paths(Path::new("styles"), None)
            .unwrap();
        assert_eq!(
            Path::new(&style.toolboxes["web"].skills["s"].path),
            cwd.join("styles/skills/s")
        );
    }

    #[test]
    fn a_shared_include_graph_is_expanded_once_per_bundle() {
        // b00 defines mcp.x; every later bundle includes its predecessor twice
        // and redefines mcp.x at every eighth step. Expanding each path anew
        // would take about 2^64 steps; each bundle is expanded once instead,
        // and last-wins still decides mcp.x and its origin.
        let mut yaml =
            String::from("project: deep\ntoolboxes:\n  b00:\n    mcp: { x: { command: [b00] } }\n");
        for n in 1..MAX_BUNDLES {
            yaml.push_str(&format!(
                "  b{n:02}:\n    include: [b{p:02}, b{p:02}]\n",
                p = n - 1
            ));
            if n % 8 == 0 {
                yaml.push_str(&format!(
                    "    mcp: {{ x: {{ command: [b{n:02}] }}, y{n:02}: {{ command: [y] }} }}\n"
                ));
            }
        }
        yaml.push_str("defaults:\n  toolbox: { include: [b63] }\nagents:\n  - id: a\n");
        let started = std::time::Instant::now();
        let team = TeamConfig::from_yaml(&yaml).unwrap();
        let resolved = member(&team, "a");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let x = &resolved.items[&ItemRef::new(ItemKind::Mcp, "x")];
        assert_eq!(x.origin, Origin::Bundle("b56".into()));
        assert_eq!(
            x.item,
            Item::Mcp(McpServer {
                command: vec!["b56".into()],
                ..McpServer::default()
            })
        );
        assert_eq!(resolved.items.len(), 1 + 7);
    }

    #[test]
    fn a_member_added_later_is_checked_like_a_team_file_member() {
        use crate::config::{validate_agent_spec, AgentSpec};
        let spec = |toolbox: &str| AgentSpec {
            id: "b".into(),
            toolbox: serde_yaml::from_str(toolbox).unwrap(),
            ..AgentSpec::default()
        };
        // Without the team: the body's own fields.
        for (toolbox, expected) in [
            (
                "mcp: { crew: { command: [x] } }",
                "agent 'b'.toolbox.mcp.crew: the name 'crew' is reserved for crew",
            ),
            (
                "mcp: { s: { command: [x], url: \"https://x\" } }",
                "agent 'b'.toolbox.mcp.s: give either command or url, not both",
            ),
            (
                "mcp: { s: { command: [x], env: { K: { env: ANTHROPIC_API_KEY } } } }",
                "agent 'b'.toolbox.mcp.s.env.K.env: ANTHROPIC_API_KEY carries",
            ),
        ] {
            let err = validate_agent_spec(&spec(toolbox)).unwrap_err();
            assert!(err.to_string().contains(expected), "{toolbox}: {err}");
        }
        validate_agent_spec(&spec("include: [anything]")).unwrap();
        // With the team: includes and exclusions as well.
        let team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        for (toolbox, expected) in [
            (
                "include: [webb]",
                "agent 'b'.toolbox.include[0]: unknown toolbox 'webb' (did you mean web?)",
            ),
            (
                "include: [web]\nexclude: [mcp.contex7]",
                "agent 'b'.toolbox.exclude[0]: mcp.contex7 matches no item (did you mean mcp.context7?)",
            ),
            (
                "mcp: { crew: { command: [x] } }",
                "agent 'b'.toolbox.mcp.crew: the name 'crew' is reserved for crew",
            ),
        ] {
            let err = team.validate_member_toolbox(&spec(toolbox)).unwrap_err();
            assert!(err.to_string().contains(expected), "{toolbox}: {err}");
        }
        team.validate_member_toolbox(&spec("include: [web]\nexclude: [mcp.context7]"))
            .unwrap();
        // What was accepted is accepted again when the roster is loaded.
        let mut roster = team.clone();
        roster.agents.push(spec("include: [web]"));
        TeamConfig::from_yaml(&roster.to_yaml().unwrap()).unwrap();
    }

    #[test]
    fn content_markers_keep_the_fingerprint_until_the_file_changes() {
        let team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        let release = member(&team, "release");
        let changelog = ItemRef::new(ItemKind::Skills, "changelog");
        let lint = ItemRef::new(ItemKind::Plugins, "lint-kit");
        let with = |changelog_content: Content| {
            let content: BTreeMap<ItemRef, Content> = [
                (changelog.clone(), changelog_content),
                (lint.clone(), Content::NotLoaded),
            ]
            .into_iter()
            .collect();
            Fingerprint::of(&release.canonical(&content).unwrap(), RuntimeKind::Codex, 1)
        };
        // A missing file fingerprints the same until it appears, and its
        // content then moves the fingerprint.
        assert_eq!(with(Content::Missing), with(Content::Missing));
        for other in [
            Content::Invalid,
            Content::TooLarge,
            Content::NotLoaded,
            Content::Sha256(hash_of(&changelog)),
        ] {
            assert_ne!(with(Content::Missing), with(other));
        }
        assert_ne!(
            with(Content::Sha256(hash_of(&changelog))),
            with(Content::Sha256(hash_of(&lint)))
        );
        let content: BTreeMap<ItemRef, Content> = [
            (changelog.clone(), Content::Sha256("ABC".into())),
            (lint.clone(), Content::NotLoaded),
        ]
        .into_iter()
        .collect();
        let err = release.canonical(&content).unwrap_err();
        assert_eq!(
            err.to_string(),
            "skills.changelog: 'ABC' is not 64 lowercase hex characters"
        );
    }

    #[test]
    fn a_resolved_toolbox_round_trips_with_string_keys_and_origins() {
        let team = TeamConfig::from_yaml(EXAMPLE).unwrap();
        let release = member(&team, "release");
        let json = serde_json::to_value(&release).unwrap();
        assert_eq!(json["items"]["mcp.github"]["origin"], "bundle:github");
        assert_eq!(json["items"]["skills.changelog"]["origin"], "member");
        assert_eq!(
            json["items"]["skills.changelog"]["item"]["file"]["path"],
            "toolbox/skills/changelog"
        );
        assert_eq!(json["sources"]["operator"], serde_json::json!([]));
        assert_eq!(serde_json::from_value::<Resolved>(json).unwrap(), release);
        let qa = member(&team, "qa");
        let yaml = serde_yaml::to_string(&qa).unwrap();
        assert_eq!(serde_yaml::from_str::<Resolved>(&yaml).unwrap(), qa);
        let content: BTreeMap<ItemRef, Content> =
            [(ItemRef::new(ItemKind::Skills, "s"), Content::NotLoaded)]
                .into_iter()
                .collect();
        assert_eq!(
            serde_json::to_string(&content).unwrap(),
            r#"{"skills.s":"not_loaded"}"#
        );
        for (bad, expected) in [
            (r#""mcp""#, "is not <kind>.<name>"),
            (r#""mcp.*""#, "names a kind, not one item"),
            (r#""tools.x""#, "is not a kind"),
        ] {
            let err = serde_json::from_str::<ItemRef>(bad).unwrap_err();
            assert!(err.to_string().contains(expected), "{bad}: {err}");
        }
        for (bad, expected) in [
            (r#""bundle:""#, "is not default, member or bundle:<name>"),
            (r#""team""#, "is not default, member or bundle:<name>"),
        ] {
            let err = serde_json::from_str::<Origin>(bad).unwrap_err();
            assert!(err.to_string().contains(expected), "{bad}: {err}");
        }
    }
    #[test]
    fn styles_carry_and_check_their_toolboxes() {
        let style = Style::parse(
            "qa",
            "toolboxes:\n  web: { skills: { s: { path: skills/s } } }\n\
defaults:\n  toolbox: { include: [web] }\n",
            false,
        )
        .unwrap();
        let mut style = style;
        style
            .absolutize_toolbox_paths(Path::new("/etc/crew/styles"), None)
            .unwrap();
        let team = style.team("p", ".", "");
        assert_eq!(
            team.toolboxes["web"].skills["s"].path,
            "/etc/crew/styles/skills/s"
        );
        let resolved = team.resolve_toolbox(&Toolbox::default(), "member").unwrap();
        assert_eq!(listing(&resolved), ["skills.s bundle:web"]);
        let err =
            Style::parse("bad", "defaults:\n  toolbox: { include: [nope] }\n", false).unwrap_err();
        assert!(err.to_string().contains("unknown toolbox 'nope'"), "{err}");
    }
}
