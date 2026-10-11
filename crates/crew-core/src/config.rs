//! Declarative team configuration (`team.yaml` / `team.toml`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::guardrails::{self, Guardrails, ToolClass, Workflow};
use crate::toolbox::{self, Toolbox};
use crate::types::{RuntimeKind, MAX_MESSAGE_BYTES, ORCHESTRATOR_ID};
use crate::util::is_valid_id;

/// Most worker agents a team may have. A team file may declare none at all,
/// in which case the orchestrator agent decides who is needed.
pub const MAX_AGENTS: usize = 10;
/// File in the state directory holding the team as it currently stands.
pub const ROSTER_FILE: &str = "roster.yaml";
/// Directory in the state directory with the last team of each work style.
pub const ROSTERS_DIR: &str = "rosters";
/// Directory (next to each `config.toml`) that holds work-style presets.
pub const STYLES_DIR: &str = "styles";
/// Longest `instructions` text a team or style may carry.
pub const MAX_INSTRUCTIONS_BYTES: usize = 16 * 1024;
/// Reserve signed millisecond deadline headroom for the base timestamp.
const MAX_TIMER_SECS: u64 = i64::MAX as u64 / 2000;

/// A number of seconds for a timer named `key`: at least 1, and small
/// enough to count in milliseconds. Team files and the daemon's own calls
/// (a task created at run time) are held to the same rule.
pub fn validate_timer(key: &str, seconds: u64) -> Result<()> {
    if seconds == 0 || seconds > MAX_TIMER_SECS {
        bail!("{key} must be between 1 and {MAX_TIMER_SECS}");
    }
    Ok(())
}

/// A work style: a preset for how a team is staffed and how it works
/// (research, experiment, …), without the workspace and the objective, which
/// are supplied when a team is launched from it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Style {
    /// Taken from the file name.
    #[serde(skip)]
    pub name: String,
    /// One line for the chooser.
    #[serde(default)]
    pub description: String,
    /// How the team works; part of every member's briefing.
    #[serde(default)]
    pub instructions: String,
    /// `worktree` to give each member its own branch, `shared` to let them
    /// work on the same files.
    #[serde(default)]
    pub isolation: Isolation,
    #[serde(default)]
    pub backend: BackendChoice,
    #[serde(default)]
    pub herdr: HerdrConfig,
    /// Toolbox bundles, by name (see [`crate::toolbox`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub toolboxes: BTreeMap<String, Toolbox>,
    /// What is enforced about how members act (see [`crate::guardrails`]).
    #[serde(default, skip_serializing_if = "Guardrails::is_default")]
    pub guardrails: Guardrails,
    /// The chain of cards one unit of work goes through (see
    /// [`crate::guardrails`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<Workflow>,
    #[serde(default)]
    pub defaults: AgentDefaults,
    #[serde(default)]
    pub orchestrator: OrchestratorConfig,
    /// A fixed roster. Left out, the orchestrator staffs the team itself,
    /// guided by `instructions`.
    #[serde(default)]
    pub agents: Vec<AgentSpec>,
    #[serde(default)]
    pub tasks: Vec<TaskSpec>,
    #[serde(default)]
    pub limits: Limits,
    /// What the workspace remembers across sessions (see `crew::memory`).
    #[serde(default, skip_serializing_if = "MemoryConfig::is_default")]
    pub memory: MemoryConfig,
}

impl Style {
    /// Parse a style file; `name` is its file stem.
    pub fn parse(name: &str, text: &str, toml: bool) -> Result<Self> {
        if !is_valid_id(name) {
            bail!("'{name}' is not a style name (expected [a-z][a-z0-9_-]{{0,31}})");
        }
        let mut style: Style = if toml {
            toml::from_str(text)?
        } else if text.trim().is_empty() {
            Style::default()
        } else {
            serde_yaml::from_str(text)?
        };
        style.name = name.to_string();
        // Everything a team file is checked for applies to a style too.
        style.team("style-check", ".", "").validate()?;
        Ok(style)
    }

    pub fn to_yaml(&self) -> Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }

    /// Make the style's toolbox file paths absolute against `base`, the
    /// directory of the style file (a relative one is taken from the current
    /// directory). A `~/` path needs `home`.
    pub fn absolutize_toolbox_paths(&mut self, base: &Path, home: Option<&Path>) -> Result<()> {
        absolutize_toolboxes(
            &mut self.toolboxes,
            &mut self.defaults,
            &mut self.orchestrator,
            &mut self.agents,
            base,
            home,
        )
    }

    /// The team this style launches on `workspace` for `objective`.
    pub fn team(&self, project: &str, workspace: &str, objective: &str) -> TeamConfig {
        TeamConfig {
            project: project.to_string(),
            objective: objective.to_string(),
            style: Some(self.name.clone()),
            instructions: self.instructions.clone(),
            workspace: WorkspaceConfig {
                path: workspace.to_string(),
                isolation: self.isolation,
                ..WorkspaceConfig::default()
            },
            backend: self.backend,
            herdr: self.herdr.clone(),
            toolboxes: self.toolboxes.clone(),
            guardrails: self.guardrails.clone(),
            workflow: self.workflow.clone(),
            defaults: self.defaults.clone(),
            orchestrator: self.orchestrator.clone(),
            agents: self.agents.clone(),
            tasks: self.tasks.clone(),
            limits: self.limits.clone(),
            memory: self.memory.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TeamConfig {
    pub project: String,
    #[serde(default)]
    pub objective: String,
    /// The work style this team was launched from (`crew --style`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// How this team works: standing instructions that become part of every
    /// member's briefing, the orchestrator's included.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    /// Process backend: `auto` (herdr inside a herdr pane, pty otherwise), `pty`, `herdr`.
    #[serde(default)]
    pub backend: BackendChoice,
    #[serde(default)]
    pub herdr: HerdrConfig,
    /// Toolbox bundles, by name (see [`crate::toolbox`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub toolboxes: BTreeMap<String, Toolbox>,
    /// What is enforced about how members act (see [`crate::guardrails`]).
    #[serde(default, skip_serializing_if = "Guardrails::is_default")]
    pub guardrails: Guardrails,
    /// The chain of cards one unit of work goes through (see
    /// [`crate::guardrails`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<Workflow>,
    #[serde(default)]
    pub defaults: AgentDefaults,
    #[serde(default)]
    pub orchestrator: OrchestratorConfig,
    #[serde(default)]
    pub agents: Vec<AgentSpec>,
    #[serde(default)]
    pub tasks: Vec<TaskSpec>,
    #[serde(default)]
    pub limits: Limits,
    /// What the workspace remembers across sessions (see `crew::memory`).
    #[serde(default, skip_serializing_if = "MemoryConfig::is_default")]
    pub memory: MemoryConfig,
}

/// The workspace's memory: short notes every later session's members read
/// before interpreting the objective or a card.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    /// Off: nothing is remembered or briefed, and the memory tools refuse.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Where the memories live, relative to the workspace (`.crew/memory`
    /// when unset: the workspace's own state, not committed). Point it at
    /// a committed folder for a memory that travels with the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// How many memories a briefing lists (the newest); the rest are a
    /// `memory_search` away.
    #[serde(default = "default_memory_brief")]
    pub brief: usize,
    /// The model that distils a finished card into memories (a Claude Code
    /// model name; `haiku` unless set), from the card and its member's
    /// transcript; `off` keeps only what members and the operator write.
    #[serde(default = "default_memory_extract")]
    pub extract: String,
}

fn default_memory_brief() -> usize {
    50
}

fn default_memory_extract() -> String {
    "haiku".to_string()
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: None,
            brief: default_memory_brief(),
            extract: default_memory_extract(),
        }
    }
}

impl MemoryConfig {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Refuse a directory that is not a plain relative path inside the
    /// workspace.
    pub fn validate(&self) -> Result<()> {
        if let Some(dir) = &self.dir {
            if dir.is_absolute()
                || dir
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                bail!(
                    "memory.dir must be a plain relative path inside the workspace, not {}",
                    dir.display()
                );
            }
        }
        if self.brief > 200 {
            bail!("memory.brief: at most 200 memories can be briefed");
        }
        if self.extract.trim().is_empty() {
            bail!("memory.extract: a model name, or `off`");
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendChoice {
    #[default]
    Auto,
    Pty,
    Herdr,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// Worktrees when the workspace is a git repository, shared otherwise.
    #[default]
    Auto,
    /// One git worktree and branch per agent.
    Worktree,
    /// All agents share the workspace directory (no isolation).
    Shared,
}

/// Which version control tool isolates the agents (`workspace.vcs`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VcsChoice {
    /// jj when it is on PATH (0.44 or newer), git otherwise; a workspace
    /// keeps the tool it was set up with.
    #[default]
    Auto,
    Jj,
    Git,
}

impl VcsChoice {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// OS sandbox policy. Off remains the default until the release gate passes.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    #[default]
    Off,
    Required,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    #[serde(default)]
    pub mode: SandboxMode,
    /// Explicit absolute mount sources; the sandbox builder checks real paths.
    #[serde(default)]
    pub read_only: Vec<PathBuf>,
    #[serde(default)]
    pub read_write: Vec<PathBuf>,
    /// Allow only the verified, untrusted-user Nix daemon endpoint.
    #[serde(default)]
    pub nix_daemon: bool,
}

impl SandboxConfig {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Validate syntax only. Canonical paths, protected mounts and host
    /// capabilities are checked by the sandbox builder before any launch.
    pub fn validate(&self) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let mut seen = HashMap::new();
        for (key, paths) in [
            ("read_only", &self.read_only),
            ("read_write", &self.read_write),
        ] {
            for path in paths {
                if path.as_os_str().as_bytes().contains(&0) {
                    bail!("workspace.sandbox.{key} contains a path with NUL");
                }
                if !path.is_absolute() {
                    bail!("workspace.sandbox.{key} requires nonempty absolute paths");
                }
                if let Some(previous) = seen.insert(path, key) {
                    if previous == key {
                        bail!(
                            "workspace.sandbox.{key} contains duplicate path {}",
                            path.display()
                        );
                    }
                    bail!(
                        "workspace.sandbox path {} appears in both read_only and read_write",
                        path.display()
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Directory the agents work on. Relative paths resolve against the
    /// directory `crew run` was invoked from.
    #[serde(default = "default_workspace_path")]
    pub path: String,
    #[serde(default)]
    pub isolation: Isolation,
    /// Create the directory and `git init` it (with an initial commit) when missing.
    #[serde(default)]
    pub init: bool,
    /// The version control tool: `auto` (jj when available), `jj` or `git`.
    #[serde(default, skip_serializing_if = "VcsChoice::is_default")]
    pub vcs: VcsChoice,
    /// Revision agent branches start from.
    #[serde(default = "default_base")]
    pub base: String,
    #[serde(default, skip_serializing_if = "SandboxConfig::is_default")]
    pub sandbox: SandboxConfig,
}

fn default_workspace_path() -> String {
    ".".to_string()
}

fn default_base() -> String {
    "HEAD".to_string()
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            path: default_workspace_path(),
            isolation: Isolation::default(),
            init: false,
            vcs: VcsChoice::default(),
            base: default_base(),
            sandbox: SandboxConfig::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HerdrLayout {
    /// One herdr tab per agent.
    #[default]
    Tabs,
    /// All agents tiled in a single tab.
    Tiled,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct HerdrConfig {
    #[serde(default)]
    pub layout: HerdrLayout,
    /// Label of the herdr workspace that hosts the team (default: `crew:<project>`).
    #[serde(default)]
    pub workspace_label: Option<String>,
}

/// Settings every agent inherits unless it overrides them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentDefaults {
    #[serde(default = "default_runtime")]
    pub runtime: RuntimeKind,
    #[serde(default)]
    pub model: Option<String>,
    /// How much the runtimes' own permission systems are in play.
    #[serde(default)]
    pub permissions: PermissionPolicy,
    /// `guarded` only — Claude Code permission mode (`acceptEdits`, `auto`,
    /// `plan`, ...).
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// Extra Claude Code tool permissions, e.g. `Bash(cargo test:*)`.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// `guarded` only — Codex sandbox mode.
    #[serde(default = "default_sandbox")]
    pub sandbox: String,
    /// Extra arguments appended to the runtime's command line.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Shell runtime: the command for agents that do not set their own
    /// (including members the orchestrator adds at runtime).
    #[serde(default)]
    pub command: Vec<String>,
    /// Let agents load the user's own MCP servers in addition to crew's.
    /// The same as `toolbox.inherit.operator: [mcp]`, which replaces it.
    #[serde(default)]
    pub inherit_mcp: bool,
    /// Answer the runtime's workspace-trust dialog automatically.
    #[serde(default)]
    pub auto_trust: bool,
    /// How many times a crashed agent is restarted (resuming its session).
    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,
    /// How bus messages reach a session: `auto` prefers the daemon socket
    /// (a Claude Code mod, Codex's app-server) and falls back to the terminal.
    #[serde(default)]
    pub delivery: DeliveryChoice,
    /// Ask for the 1M-token context window for the models that have one
    /// (Claude Code and Codex alike).
    #[serde(default = "default_true")]
    pub long_context: bool,
    /// Let Claude Code move a session to another model when its model's
    /// usage window is used up or the model is overloaded (the orchestrator
    /// is told). With `false` the turn fails instead and the agent is parked
    /// at the limit.
    #[serde(default = "default_true")]
    pub model_fallback: bool,
    /// What an agent goes on when its model cannot be used for a while —
    /// the model's own usage window, or its account's, used up: model →
    /// the models to use instead, in order (`fable: [opus]`). crew moves
    /// the agent itself and back again once the window resets. A model of
    /// another CLI starts it a fresh session there, handed the work.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_fallbacks: BTreeMap<String, Vec<String>>,
    /// What an agent goes on when its model refuses a turn's content
    /// (Codex's `cyber_policy`: flagged for possible cybersecurity risk), in
    /// order: a catalog model (`opus`), a pi model as pi names it
    /// (`zai/glm-5.3`), or a pi model's name under whichever provider pi
    /// has it (`glm-5.3`). crew moves the agent there, handed what was
    /// refused, and back once that work is done. Without one, the agent is
    /// set aside as blocked and the orchestrator told.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusal_fallbacks: Vec<String>,
    /// How members write to one another (and the orchestrator to them):
    /// `ultracave` (fragments), `megacave` (Classical Chinese), `caveman`
    /// or `plain`.
    #[serde(default, skip_serializing_if = "Voice::is_default")]
    pub voice: Voice,
    /// What every member's toolbox starts from (see [`crate::toolbox`]).
    #[serde(default, skip_serializing_if = "Toolbox::is_empty")]
    pub toolbox: Toolbox,
    /// What becomes of a member's conversation between cards: kept (and
    /// compacted when full), or cleared, each card starting in a fresh
    /// session.
    #[serde(default, skip_serializing_if = "ContextPolicy::is_default")]
    pub context: ContextPolicy,
}

/// What becomes of a member's conversation between cards.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextPolicy {
    /// The session carries on from card to card; a full window is
    /// compacted (`agent_compact`), or cleared when nothing is open.
    #[default]
    Keep,
    /// Each card starts in a fresh session: when a card is finished, the
    /// session is replaced by a new one with nothing of it kept. What the
    /// next card needs is on the board (results, commits), not in the
    /// conversation.
    Fresh,
}

impl ContextPolicy {
    pub fn is_default(&self) -> bool {
        *self == ContextPolicy::default()
    }
}

/// Whether agents run with their runtime's permission checks.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// No permission prompts and no sandbox: Claude Code runs with
    /// `--dangerously-skip-permissions`, Codex with
    /// `--dangerously-bypass-approvals-and-sandbox` ("yolo"). Agents never
    /// stall on a prompt, and nothing stops them from running any command.
    #[default]
    Bypass,
    /// Claude Code uses `permission_mode` (default `acceptEdits`) and Codex
    /// its `sandbox` (default `workspace-write`) with approvals off.
    Guarded,
}

/// How members write to one another, and the orchestrator to them: what
/// only models read. The voices follow caveman's
/// (https://github.com/JuliusBrussee/caveman). What lasts for people (code,
/// commits, docs, reports) and what the operator reads stay plain.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Voice {
    /// Classical Chinese (文言), technical terms verbatim.
    Megacave,
    /// Fragments: no articles, copulas or connectives. The fewest output
    /// tokens of the four.
    #[default]
    Ultracave,
    /// Short plain sentences, no filler.
    Caveman,
    /// Concise English.
    Plain,
}

impl Voice {
    pub fn is_default(&self) -> bool {
        *self == Voice::default()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryChoice {
    #[default]
    Auto,
    Socket,
    Terminal,
}

fn default_runtime() -> RuntimeKind {
    RuntimeKind::Claude
}

fn default_permission_mode() -> String {
    "acceptEdits".to_string()
}

fn default_sandbox() -> String {
    "workspace-write".to_string()
}

fn default_max_restarts() -> u32 {
    1
}

impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            runtime: default_runtime(),
            model: None,
            permissions: PermissionPolicy::default(),
            permission_mode: default_permission_mode(),
            allowed_tools: Vec::new(),
            sandbox: default_sandbox(),
            extra_args: Vec::new(),
            command: Vec::new(),
            inherit_mcp: false,
            auto_trust: false,
            max_restarts: default_max_restarts(),
            delivery: DeliveryChoice::default(),
            long_context: true,
            model_fallback: true,
            model_fallbacks: BTreeMap::new(),
            refusal_fallbacks: Vec::new(),
            voice: Voice::default(),
            toolbox: Toolbox::default(),
            context: ContextPolicy::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OrchestratorConfig {
    /// Run a real orchestrator agent. When false the daemon's scheduler alone
    /// plays the orchestrator (useful for scripted or shell-agent teams).
    #[serde(default = "default_true")]
    pub agent: bool,
    #[serde(default)]
    pub runtime: Option<RuntimeKind>,
    #[serde(default)]
    pub model: Option<String>,
    /// Shell runtime only: the command line to run.
    #[serde(default)]
    pub command: Vec<String>,
    /// Tell the orchestrator (metadata only, never the body) when workers talk directly.
    #[serde(default)]
    pub notify_p2p: bool,
    /// The orchestrator's own toolbox level (see [`crate::toolbox`]).
    #[serde(default, skip_serializing_if = "Toolbox::is_empty")]
    pub toolbox: Toolbox,
    /// The crew tools the orchestrator gets, by group or name (see
    /// [`crate::guardrails`]); left out, all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew_tools: Option<Vec<String>>,
}

fn default_true() -> bool {
    true
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            agent: true,
            runtime: None,
            model: None,
            command: Vec::new(),
            notify_p2p: false,
            toolbox: Toolbox::default(),
            crew_tools: None,
        }
    }
}

/// One agent in the team.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub runtime: Option<RuntimeKind>,
    #[serde(default)]
    pub model: Option<String>,
    /// Shorthand: creates a task with this description assigned to the agent.
    #[serde(default)]
    pub task: Option<String>,
    /// Shell runtime only: the command line to run, as an argument array.
    #[serde(default)]
    pub command: Vec<String>,
    /// Override the team's permission policy for this agent.
    #[serde(default)]
    pub permissions: Option<PermissionPolicy>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// This member's toolbox level (see [`crate::toolbox`]).
    #[serde(default, skip_serializing_if = "Toolbox::is_empty")]
    pub toolbox: Toolbox,
    /// The classes of its runtime's own tools this member gets (see
    /// [`crate::guardrails`]); left out, all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolClass>>,
    /// The crew tools this member gets, by group or name (see
    /// [`crate::guardrails`]); left out, all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew_tools: Option<Vec<String>>,
    /// Override the team's `defaults.context` for this member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextPolicy>,
    /// A pool: not a member itself but the template for up to this many,
    /// which crew makes as a workflow's slices can start (`<id>-1`,
    /// `<id>-2`, …, one per slice that has no member free) and retires
    /// when their slice is done. Named by a slice step's `agent` and by
    /// nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<u32>,
}

/// The number of `id` as a member of pool `pool` (`<pool>-<n>`, n ≥ 1).
pub fn pool_member_number(pool: &str, id: &str) -> Option<u32> {
    id.strip_prefix(pool)?
        .strip_prefix('-')?
        .parse()
        .ok()
        .filter(|n| *n >= 1)
}

impl AgentSpec {
    /// The spec a member of this pool is made from: this one, with the
    /// member's name and no pool of its own.
    pub fn pool_member(&self, n: u32) -> AgentSpec {
        AgentSpec {
            id: format!("{}-{n}", self.id),
            pool: None,
            task: None,
            ..self.clone()
        }
    }
}

impl TeamConfig {
    /// Whether `id` names a pool of this team.
    pub fn is_pool(&self, id: &str) -> bool {
        self.agents.iter().any(|a| a.id == id && a.pool.is_some())
    }

    /// The pool `id` is a member of (`<pool>-<n>`), if any.
    pub fn pool_of(&self, id: &str) -> Option<&AgentSpec> {
        self.agents
            .iter()
            .filter(|a| a.pool.is_some())
            .find(|a| pool_member_number(&a.id, id).is_some())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Agent the task is assigned to. Unassigned tasks go to the least busy worker.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_max_message_bytes")]
    pub max_message_bytes: usize,
    /// Remind an agent that went quiet while its task is still open.
    #[serde(default = "default_idle_nudge_secs")]
    pub idle_nudge_secs: u64,
    #[serde(default = "default_max_nudges")]
    pub max_nudges: u32,
    /// Flag an agent that shows no activity for this long while working.
    #[serde(default = "default_unresponsive_secs")]
    pub unresponsive_secs: u64,
    /// How many subagents team members may have running at once (0 disables).
    #[serde(default = "default_max_subagents")]
    pub max_subagents: usize,
    /// Resume an agent that hit a usage limit by itself: when the limit
    /// resets if the runtime said when, else every `limit_retry_secs`.
    #[serde(default = "default_true")]
    pub auto_resume: bool,
    /// How long to wait before retrying when the reset time is unknown.
    #[serde(default = "default_limit_retry_secs")]
    pub limit_retry_secs: u64,
    /// Tell the orchestrator when an agent's context window is this full,
    /// in percent, so it can queue a compaction at a good moment (0 never).
    #[serde(default = "default_context_warn_percent")]
    pub context_warn_percent: u8,
    /// Keep an idle Claude Code member's prompt cache warm: this many seconds
    /// after its last turn, crew asks it for a one-word reply, so the cached
    /// context is read (and its one-hour lifetime renewed) instead of being
    /// written again when work comes. 0 never (the default); below 3600 to
    /// matter, 3300 suggested. See notes/prompt-cache.md, D2 revision.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub keep_cache_warm_secs: u64,
    /// Keep-alives per rest, after which the cache is let go.
    #[serde(
        default = "default_keep_cache_warm_max",
        skip_serializing_if = "is_default_keep_cache_warm_max"
    )]
    pub keep_cache_warm_max: u32,
    /// The smallest context, in tokens, worth keeping warm.
    #[serde(
        default = "default_keep_cache_warm_min_tokens",
        skip_serializing_if = "is_default_keep_cache_warm_min_tokens"
    )]
    pub keep_cache_warm_min_tokens: u64,
}

fn default_keep_cache_warm_max() -> u32 {
    6
}

fn default_keep_cache_warm_min_tokens() -> u64 {
    50_000
}

// Left out when unset, so a team file saved without them reads the same.
fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn is_default_keep_cache_warm_max(n: &u32) -> bool {
    *n == default_keep_cache_warm_max()
}

fn is_default_keep_cache_warm_min_tokens(n: &u64) -> bool {
    *n == default_keep_cache_warm_min_tokens()
}

fn default_context_warn_percent() -> u8 {
    75
}

fn default_limit_retry_secs() -> u64 {
    1800
}

fn default_max_message_bytes() -> usize {
    16 * 1024
}

fn default_idle_nudge_secs() -> u64 {
    90
}

fn default_max_nudges() -> u32 {
    3
}

fn default_unresponsive_secs() -> u64 {
    900
}

fn default_max_subagents() -> usize {
    4
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: default_max_message_bytes(),
            idle_nudge_secs: default_idle_nudge_secs(),
            max_nudges: default_max_nudges(),
            unresponsive_secs: default_unresponsive_secs(),
            max_subagents: default_max_subagents(),
            auto_resume: true,
            limit_retry_secs: default_limit_retry_secs(),
            context_warn_percent: default_context_warn_percent(),
            keep_cache_warm_secs: 0,
            keep_cache_warm_max: default_keep_cache_warm_max(),
            keep_cache_warm_min_tokens: default_keep_cache_warm_min_tokens(),
        }
    }
}

impl TeamConfig {
    /// Parse a team file; the format follows the extension (`.toml`, else YAML).
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading team config {}", path.display()))?;
        let is_toml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
        let mut cfg = if is_toml {
            Self::from_toml(&text)
        } else {
            Self::from_yaml(&text)
        }
        .with_context(|| format!("parsing team config {}", path.display()))?;
        // Toolbox paths are relative to the file that names them; from here
        // on (the daemon's copy, a roster) they are absolute.
        let dir = path.parent().unwrap_or_else(|| Path::new(""));
        cfg.absolutize_toolbox_paths(dir, home_dir().as_deref())
            .with_context(|| format!("in team config {}", path.display()))?;
        Ok(cfg)
    }

    /// Make the team's toolbox file paths absolute against `base`, the
    /// directory of the team file (a relative one is taken from the current
    /// directory). A `~/` path needs `home`.
    pub fn absolutize_toolbox_paths(&mut self, base: &Path, home: Option<&Path>) -> Result<()> {
        absolutize_toolboxes(
            &mut self.toolboxes,
            &mut self.defaults,
            &mut self.orchestrator,
            &mut self.agents,
            base,
            home,
        )
    }

    /// The declarations every member's toolbox resolves against.
    pub fn toolbox_catalog(&self) -> toolbox::Catalog<'_> {
        toolbox::Catalog {
            bundles: &self.toolboxes,
            defaults: &self.defaults.toolbox,
            inherit_mcp: self.defaults.inherit_mcp,
        }
    }

    /// Resolve a member's toolbox level (found at `path`) against this team.
    pub fn resolve_toolbox(&self, member: &Toolbox, path: &str) -> Result<toolbox::Resolved> {
        toolbox::resolve(self.toolbox_catalog(), member, path)
    }

    /// Check a member added after the team was loaded (`agent_add`, a
    /// subagent) the way a team file's members are checked: its toolbox
    /// body, its includes against this team's bundles, and its resolution.
    /// What this accepts, a saved roster accepts too.
    pub fn validate_member_toolbox(&self, agent: &AgentSpec) -> Result<()> {
        let path = format!("agent '{}'.toolbox", agent.id);
        toolbox::validate_body(&agent.toolbox, toolbox::Level::Member, &path)?;
        toolbox::check_includes(&agent.toolbox, &self.toolboxes, &path)?;
        self.resolve_toolbox(&agent.toolbox, &path)?;
        Ok(())
    }

    fn validate_toolboxes(&self) -> Result<()> {
        let catalog = self.toolbox_catalog();
        toolbox::validate_catalog(catalog)?;
        let mut members: Vec<(&Toolbox, String)> = Vec::new();
        if self.orchestrator.agent {
            members.push((
                &self.orchestrator.toolbox,
                "orchestrator.toolbox".to_string(),
            ));
        } else if !self.orchestrator.toolbox.is_empty() {
            bail!("orchestrator.toolbox is set, but orchestrator.agent is false");
        }
        for (i, agent) in self.agents.iter().enumerate() {
            members.push((&agent.toolbox, format!("agents[{i}].toolbox")));
        }
        for (body, path) in members {
            toolbox::validate_body(body, toolbox::Level::Member, &path)?;
            toolbox::check_includes(body, &self.toolboxes, &path)?;
            toolbox::resolve(catalog, body, &path)?;
        }
        Ok(())
    }

    pub fn from_yaml(text: &str) -> Result<Self> {
        let cfg: TeamConfig = serde_yaml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let cfg: TeamConfig = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_yaml(&self) -> Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }

    /// Resolve the workspace directory against the invocation directory.
    pub fn workspace_dir(&self, cwd: &Path) -> PathBuf {
        let p = Path::new(&self.workspace.path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        }
    }

    /// Tasks declared explicitly plus the per-agent `task:` shorthands.
    pub fn all_tasks(&self) -> Vec<TaskSpec> {
        let mut tasks = self.tasks.clone();
        for agent in &self.agents {
            if let Some(desc) = &agent.task {
                tasks.push(TaskSpec {
                    id: format!("{}-task", agent.id),
                    title: None,
                    description: desc.clone(),
                    agent: Some(agent.id.clone()),
                    ..TaskSpec::default()
                });
            }
        }
        tasks
    }

    pub fn validate(&self) -> Result<()> {
        if self.project.trim().is_empty() {
            bail!("`project` must not be empty");
        }
        if self.project.len() > 64
            || !self
                .project
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            bail!("`project` may only contain letters, digits, '-', '_' and '.' (max 64 chars)");
        }
        self.workspace.sandbox.validate()?;
        if let Some(style) = &self.style {
            if !is_valid_id(style) {
                bail!("`style` '{style}' is not a style name (expected [a-z][a-z0-9_-]{{0,31}})");
            }
        }
        if self.instructions.len() > MAX_INSTRUCTIONS_BYTES {
            bail!("`instructions` is longer than {MAX_INSTRUCTIONS_BYTES} bytes");
        }
        if self.agents.len() > MAX_AGENTS {
            bail!(
                "a team has at most {MAX_AGENTS} agents, found {}",
                self.agents.len()
            );
        }
        if self.agents.is_empty() && !self.orchestrator.agent {
            bail!(
                "the team file declares no agents; either list them under `agents:` or keep \
`orchestrator.agent: true` so the orchestrator can decide who is needed"
            );
        }
        let mut ids = HashSet::new();
        for (i, agent) in self.agents.iter().enumerate() {
            // The toolbox body first, so a file's errors name `agents[i]`.
            toolbox::validate_body(
                &agent.toolbox,
                toolbox::Level::Member,
                &format!("agents[{i}].toolbox"),
            )?;
            validate_agent_spec(agent)?;
            if !ids.insert(agent.id.as_str()) {
                bail!("duplicate agent id '{}'", agent.id);
            }
            let runtime = agent.runtime.unwrap_or(self.defaults.runtime);
            if runtime == RuntimeKind::Shell
                && agent.command.is_empty()
                && self.defaults.command.is_empty()
            {
                bail!(
                    "agent '{}' uses the shell runtime and needs `command`",
                    agent.id
                );
            }
        }
        // A pool's members are named <pool>-<n> by crew: nobody else is.
        for pool in self.agents.iter().filter(|a| a.pool.is_some()) {
            if let Some(member) = self
                .agents
                .iter()
                .find(|a| pool_member_number(&pool.id, &a.id).is_some())
            {
                bail!(
                    "agent '{}' is named like a member of pool '{}' (<id>-<n>), which crew makes \
itself",
                    member.id,
                    pool.id
                );
            }
        }
        if self.orchestrator.agent
            && self.orchestrator.runtime.unwrap_or(self.defaults.runtime) == RuntimeKind::Shell
            && self.orchestrator.command.is_empty()
            && self.defaults.command.is_empty()
        {
            bail!("the orchestrator uses the shell runtime and needs `command`");
        }
        if let Some(selection) = &self.orchestrator.crew_tools {
            guardrails::crew_tools_allowed(selection)
                .map_err(|e| anyhow!("orchestrator.crew_tools: {e}"))?;
        }
        if let Some(workflow) = &self.workflow {
            workflow.validate(&self.agents)?;
        }
        self.memory.validate()?;
        validate_model_fallbacks(&self.defaults.model_fallbacks)?;
        validate_refusal_fallbacks(&self.defaults.refusal_fallbacks)?;
        for (key, model) in [
            ("defaults.model", &self.defaults.model),
            ("orchestrator.model", &self.orchestrator.model),
        ] {
            if model.as_deref().is_some_and(|value| !model_name(value)) {
                bail!("{key} is not a model name");
            }
        }
        let tasks = self.all_tasks();
        let mut task_ids = HashSet::new();
        for task in &tasks {
            if let Some(timeout) = task.timeout_secs {
                validate_timer(&format!("task '{}'.timeout_secs", task.id), timeout)?;
            }
            if !is_valid_id(&task.id) {
                bail!(
                    "invalid task id '{}' (expected [a-z][a-z0-9_-]{{0,31}})",
                    task.id
                );
            }
            if !task_ids.insert(task.id.as_str()) {
                bail!("duplicate task id '{}'", task.id);
            }
            for who in [&task.agent, &task.reviewer].into_iter().flatten() {
                if !ids.contains(who.as_str()) && who != ORCHESTRATOR_ID {
                    bail!("task '{}' references unknown agent '{who}'", task.id);
                }
                if self.is_pool(who) {
                    bail!(
                        "task '{}' names pool '{who}', whose members take a workflow's slices \
and nothing else",
                        task.id
                    );
                }
            }
        }
        for task in &tasks {
            for dep in &task.depends_on {
                if !task_ids.contains(dep.as_str()) {
                    bail!("task '{}' depends on unknown task '{dep}'", task.id);
                }
            }
        }
        let graph: HashMap<&str, Vec<&str>> = tasks
            .iter()
            .map(|t| {
                (
                    t.id.as_str(),
                    t.depends_on.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        if let Some(cycle) = find_cycle(&graph) {
            bail!("task dependencies form a cycle: {}", cycle.join(" -> "));
        }
        if self.limits.max_message_bytes == 0 || self.limits.max_message_bytes > MAX_MESSAGE_BYTES {
            bail!("limits.max_message_bytes must be between 1 and {MAX_MESSAGE_BYTES}");
        }
        validate_timer("limits.idle_nudge_secs", self.limits.idle_nudge_secs)?;
        validate_timer("limits.unresponsive_secs", self.limits.unresponsive_secs)?;
        validate_timer("limits.limit_retry_secs", self.limits.limit_retry_secs)?;
        if self.limits.context_warn_percent > 100 {
            bail!("limits.context_warn_percent must be between 0 and 100 (0 disables warnings)");
        }
        self.validate_toolboxes()?;
        Ok(())
    }
}

/// The operator's home for `~/` in toolbox paths: `HOME`, when it is set to
/// an absolute path.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
}

fn absolutize_toolboxes(
    bundles: &mut BTreeMap<String, Toolbox>,
    defaults: &mut AgentDefaults,
    orchestrator: &mut OrchestratorConfig,
    agents: &mut [AgentSpec],
    base: &Path,
    home: Option<&Path>,
) -> Result<()> {
    // A relative base (a relative workspace or config path) is taken from
    // the current directory now, so no file path is left relative to a
    // member's working directory later.
    let base = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir()
            .context("finding the current directory")?
            .join(base)
    };
    for (name, bundle) in bundles.iter_mut() {
        bundle.absolutize(&base, home, &format!("toolboxes.{name}"))?;
    }
    defaults
        .toolbox
        .absolutize(&base, home, "defaults.toolbox")?;
    orchestrator
        .toolbox
        .absolutize(&base, home, "orchestrator.toolbox")?;
    for (i, agent) in agents.iter_mut().enumerate() {
        agent
            .toolbox
            .absolutize(&base, home, &format!("agents[{i}].toolbox"))?;
    }
    Ok(())
}

/// Most models `model_fallbacks` names a fallback for, and most fallbacks
/// of one model.
pub const MAX_FALLBACK_MODELS: usize = 32;
pub const MAX_FALLBACKS: usize = 8;

fn model_name(m: &str) -> bool {
    !m.trim().is_empty()
        && m.len() <= 120
        && !m.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn validate_refusal_fallbacks(fallbacks: &[String]) -> Result<()> {
    if fallbacks.len() > MAX_FALLBACKS {
        bail!("`defaults.refusal_fallbacks` lists more than {MAX_FALLBACKS} models");
    }
    if let Some(bad) = fallbacks.iter().find(|m| !model_name(m)) {
        bail!("`defaults.refusal_fallbacks`: '{bad}' is not a model name");
    }
    Ok(())
}

fn validate_model_fallbacks(fallbacks: &BTreeMap<String, Vec<String>>) -> Result<()> {
    if fallbacks.len() > MAX_FALLBACK_MODELS {
        bail!("`defaults.model_fallbacks` names more than {MAX_FALLBACK_MODELS} models");
    }
    for (model, instead) in fallbacks {
        if !model_name(model) {
            bail!("`defaults.model_fallbacks`: '{model}' is not a model name");
        }
        if instead.is_empty() || instead.len() > MAX_FALLBACKS {
            bail!("`defaults.model_fallbacks.{model}` lists 1 to {MAX_FALLBACKS} models to use instead");
        }
        for other in instead {
            if !model_name(other) {
                bail!("`defaults.model_fallbacks.{model}`: '{other}' is not a model name");
            }
            if other.eq_ignore_ascii_case(model) {
                bail!("`defaults.model_fallbacks.{model}` names {model} itself");
            }
        }
    }
    Ok(())
}

/// Validate a single agent declaration (also used by `crew agent add`).
pub fn validate_agent_spec(agent: &AgentSpec) -> Result<()> {
    if !is_valid_id(&agent.id) {
        bail!(
            "invalid agent id '{}' (expected [a-z][a-z0-9_-]{{0,31}})",
            agent.id
        );
    }
    if agent.id == ORCHESTRATOR_ID
        || agent.id == crate::types::OPERATOR_ID
        || agent.id == "integration"
    {
        bail!("agent id '{}' is reserved", agent.id);
    }
    if agent.role.len() > 200 {
        bail!("agent '{}': role is too long", agent.id);
    }
    if let Some(n) = agent.pool {
        if n == 0 || n as usize > MAX_AGENTS {
            bail!("agent '{}': pool is 1 to {MAX_AGENTS}", agent.id);
        }
        if agent.id.len() > 29 {
            bail!(
                "agent '{}': a pool's id has at most 29 characters, since its members are named \
<id>-<n>",
                agent.id
            );
        }
        if agent.task.is_some() {
            bail!(
                "agent '{}' is a pool: it takes no task of its own; its members take a workflow's \
slices",
                agent.id
            );
        }
    }
    if agent.name.as_deref().is_some_and(|name| {
        name.trim().is_empty() || name.len() > 200 || name.chars().any(char::is_control)
    }) {
        bail!(
            "agent '{}'.name must be nonempty printable text (max 200 bytes)",
            agent.id
        );
    }
    if agent
        .model
        .as_deref()
        .is_some_and(|model| !model_name(model))
    {
        bail!("agent '{}'.model is not a model name", agent.id);
    }
    if let Some(selection) = &agent.crew_tools {
        guardrails::crew_tools_allowed(selection)
            .map_err(|e| anyhow!("agent '{}'.crew_tools: {e}", agent.id))?;
    }
    if let Some(tools) = &agent.tools {
        let mut seen = HashSet::new();
        for class in tools {
            if !seen.insert(class) {
                bail!("agent '{}'.tools names {} twice", agent.id, class.as_str());
            }
        }
    }
    // What can be checked without the team: the body's own fields. Includes
    // and exclusions need the team's bundles (see
    // [`TeamConfig::validate_member_toolbox`]).
    toolbox::validate_body(
        &agent.toolbox,
        toolbox::Level::Member,
        &format!("agent '{}'.toolbox", agent.id),
    )?;
    Ok(())
}

/// Find a dependency cycle, returned as the path that closes it.
pub fn find_cycle<'a>(graph: &HashMap<&'a str, Vec<&'a str>>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }

    fn visit<'a>(
        node: &'a str,
        graph: &HashMap<&'a str, Vec<&'a str>>,
        marks: &mut HashMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match marks.get(node) {
            Some(Mark::Done) => return None,
            Some(Mark::Visiting) => {
                let start = stack.iter().position(|n| *n == node).unwrap_or(0);
                let mut cycle: Vec<String> = stack[start..].iter().map(|s| s.to_string()).collect();
                cycle.push(node.to_string());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(node, Mark::Visiting);
        stack.push(node);
        if let Some(deps) = graph.get(node) {
            for dep in deps {
                if let Some(cycle) = visit(dep, graph, marks, stack) {
                    return Some(cycle);
                }
            }
        }
        stack.pop();
        marks.insert(node, Mark::Done);
        None
    }

    let mut marks = HashMap::new();
    let mut nodes: Vec<&str> = graph.keys().copied().collect();
    nodes.sort_unstable();
    for node in nodes {
        let mut stack = Vec::new();
        if let Some(cycle) = visit(node, graph, &mut marks, &mut stack) {
            return Some(cycle);
        }
    }
    None
}

/// Parse `KEY=VALUE`.
pub fn parse_key_value(s: &str) -> Result<(String, String)> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("expected KEY=VALUE, got '{s}'"))?;
    Ok((k.to_string(), v.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sandbox_keeps_legacy_yaml_and_json_workspace_shapes() {
        let mut configured = TeamConfig::from_yaml("project: test\n").unwrap();
        configured
            .workspace
            .sandbox
            .read_only
            .push("/opt/tool".into());
        let yaml = configured.to_yaml().unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert!(value["workspace"].get("sandbox").is_some());
        assert_eq!(TeamConfig::from_yaml(&yaml).unwrap(), configured);
        let json = serde_json::to_value(&configured).unwrap();
        assert!(json["workspace"].get("sandbox").is_some());
        assert_eq!(
            serde_json::from_value::<TeamConfig>(json).unwrap(),
            configured
        );

        for input in [
            "project: test\n",
            "project: test\nworkspace:\n  sandbox:\n    mode: off\n",
        ] {
            let legacy = TeamConfig::from_yaml(input).unwrap();
            let yaml = legacy.to_yaml().unwrap();
            let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
            let expected: serde_yaml::Value =
                serde_yaml::from_str("path: .\nisolation: auto\ninit: false\nbase: HEAD\n")
                    .unwrap();
            assert_eq!(value["workspace"], expected);
            let json = serde_json::to_value(&legacy).unwrap();
            assert_eq!(
                json["workspace"],
                serde_json::json!({
                    "path": ".", "isolation": "auto", "init": false, "base": "HEAD"
                })
            );
            assert_eq!(TeamConfig::from_yaml(&yaml).unwrap(), legacy);
            assert_eq!(serde_json::from_value::<TeamConfig>(json).unwrap(), legacy);
        }
    }

    #[test]
    fn sandbox_config_defaults_off_and_preserves_explicit_paths() {
        let legacy_yaml = TeamConfig::from_yaml("project: test\n").unwrap();
        let legacy_toml = TeamConfig::from_toml("project = \"test\"\n").unwrap();
        for legacy in [legacy_yaml, legacy_toml] {
            assert_eq!(legacy.workspace.sandbox, SandboxConfig::default());
            assert_eq!(legacy.workspace.sandbox.mode, SandboxMode::Off);
        }
        let yaml = "project: test\nworkspace:\n  sandbox:\n    mode: off\n    read_only: ['/opt/tool kit', '/srv/../data']\n    read_write: ['/mnt/cache']\n    nix_daemon: true\n";
        let team = TeamConfig::from_yaml(yaml).unwrap();
        assert_eq!(
            team.workspace.sandbox.read_only,
            vec![
                PathBuf::from("/opt/tool kit"),
                PathBuf::from("/srv/../data")
            ]
        );
        assert_eq!(
            team.workspace.sandbox.read_write,
            vec![PathBuf::from("/mnt/cache")]
        );
        assert!(team.workspace.sandbox.nix_daemon);
        assert_eq!(
            TeamConfig::from_yaml(&team.to_yaml().unwrap()).unwrap(),
            team
        );
        let sandbox: SandboxConfig = serde_yaml::from_str("mode: required\n").unwrap();
        assert_eq!(sandbox.mode, SandboxMode::Required);
        assert!(sandbox.read_only.is_empty() && sandbox.read_write.is_empty());
        assert!(!sandbox.nix_daemon);
        for text in [
            "mode: optional",
            "nix_deamon: true",
            "read_only: ['/opt']\nunknown: true",
        ] {
            assert!(
                serde_yaml::from_str::<SandboxConfig>(text).is_err(),
                "{text}"
            );
        }
    }

    #[test]
    fn sandbox_config_rejects_invalid_duplicate_and_conflicting_paths() {
        let mut team = TeamConfig::from_yaml("project: test\n").unwrap();
        // Explicit mount roots are syntactic here; canonicalization and the
        // protected-set checks belong to the sandbox builder at start time.
        team.workspace.sandbox.read_only = vec!["/opt/tool".into()];
        team.workspace.sandbox.read_write = vec!["/srv/cache".into()];
        team.validate().unwrap();
        for (paths, expected) in [
            (vec![PathBuf::new()], "absolute"),
            (vec![PathBuf::from("relative")], "absolute"),
            (vec![PathBuf::from("~/tools")], "absolute"),
            (vec![PathBuf::from("/opt/\0bad")], "NUL"),
            (
                vec![PathBuf::from("/opt"), PathBuf::from("/opt")],
                "duplicate",
            ),
        ] {
            for read_only in [true, false] {
                let mut invalid = team.clone();
                if read_only {
                    invalid.workspace.sandbox.read_only = paths.clone();
                } else {
                    invalid.workspace.sandbox.read_write = paths.clone();
                }
                let error = invalid.validate().unwrap_err().to_string();
                assert!(error.contains("workspace.sandbox"), "{error}");
                assert!(error.contains(expected), "{error}");
            }
        }
        let mut conflict = team;
        conflict.workspace.sandbox.read_write = conflict.workspace.sandbox.read_only.clone();
        let error = conflict.validate().unwrap_err().to_string();
        assert!(
            error.contains("read_only") && error.contains("read_write"),
            "{error}"
        );
    }

    #[test]
    fn sandbox_required_is_accepted_and_its_enforcement_is_the_daemons() {
        // Supersedes sandbox_required_is_unavailable_until_daemon_enforcement_is_wired:
        // the daemon now runs every launch of a team that requires sandboxes in
        // one, and refuses such a team where it cannot (crew/tests/sandbox.rs,
        // required_starts_on_a_capable_host_and_is_refused_with_the_probes_reason_otherwise).
        let yaml = "project: test\nworkspace:\n  sandbox:\n    mode: required\n";
        let team = TeamConfig::from_yaml(yaml).unwrap();
        assert_eq!(team.workspace.sandbox.mode, SandboxMode::Required);
        let toml = "project = \"test\"\n[workspace.sandbox]\nmode = \"required\"\n";
        let team = TeamConfig::from_toml(toml).unwrap();
        assert_eq!(team.workspace.sandbox.mode, SandboxMode::Required);
        TeamConfig::from_yaml(&yaml.replace("required", "off")).unwrap();
    }

    #[test]
    fn integration_is_reserved_for_the_integration_branch() {
        let agent = AgentSpec {
            id: "integration".into(),
            ..AgentSpec::default()
        };
        assert!(validate_agent_spec(&agent).is_err());
        assert!(TeamConfig::from_yaml("project: test\nagents:\n  - id: integration\n").is_err());
        validate_agent_spec(&AgentSpec {
            id: "integration-worker".into(),
            ..AgentSpec::default()
        })
        .unwrap();
    }

    #[test]
    fn config_rejects_zero_and_overflowing_timers_and_bad_percentages() {
        for (key, value) in [
            ("idle_nudge_secs", 0u64),
            ("unresponsive_secs", 0),
            ("limit_retry_secs", 0),
            ("idle_nudge_secs", u64::MAX),
            ("unresponsive_secs", u64::MAX),
            ("limit_retry_secs", u64::MAX),
            ("context_warn_percent", 101),
        ] {
            let text = format!("project: test\nlimits:\n  {key}: {value}\n");
            let error =
                TeamConfig::from_yaml(&text).expect_err("invalid timer/percentage must fail");
            assert!(error.to_string().contains(key), "{error}");
        }
        for value in [0, u64::MAX] {
            let text = format!("project: test\ntasks:\n  - id: task\n    description: work\n    timeout_secs: {value}\n");
            let error = TeamConfig::from_yaml(&text).expect_err("invalid task timeout must fail");
            assert!(error.to_string().contains("timeout_secs"), "{error}");
        }
        TeamConfig::from_yaml("project: test\nlimits:\n  idle_nudge_secs: 1\n  unresponsive_secs: 1\n  limit_retry_secs: 1\n  context_warn_percent: 0\n  max_subagents: 0\n  max_nudges: 0\n").unwrap();
    }

    #[test]
    fn config_rejects_invalid_display_names_and_model_selectors() {
        for key in ["name", "model"] {
            for value in ["", "bad\u{1b}name", &"x".repeat(201)] {
                let mut agent = AgentSpec {
                    id: "worker".into(),
                    ..AgentSpec::default()
                };
                if key == "name" {
                    agent.name = Some(value.into());
                } else {
                    agent.model = Some(value.into());
                }
                let error = validate_agent_spec(&agent).expect_err("invalid name/model must fail");
                assert!(error.to_string().contains(key), "{error}");
            }
        }
        for model in ["sonnet[1m]", "provider/model", "gpt-6-astra"] {
            validate_agent_spec(&AgentSpec {
                id: "worker".into(),
                name: Some("Database expert 日本語".into()),
                model: Some(model.into()),
                ..AgentSpec::default()
            })
            .unwrap();
        }
        for section in ["defaults", "orchestrator"] {
            let error = TeamConfig::from_yaml(&format!("project: test\n{section}:\n  model: ''\n"))
                .unwrap_err();
            assert!(error.to_string().contains(&format!("{section}.model")));
        }
    }

    #[test]
    fn a_style_is_a_team_without_its_workspace_and_objective() {
        let style = Style::parse(
            "research",
            "description: Investigate a question\n\
             instructions: |\n  Cite a source for every claim.\n\
             isolation: shared\n\
             defaults:\n  model: sonnet\n\
             agents:\n  - id: scout\n    role: finds sources\n",
            false,
        )
        .unwrap();
        assert_eq!(style.name, "research");
        assert_eq!(style.description, "Investigate a question");
        let team = style.team("notes", "/work/notes", "How does X work?");
        assert_eq!(team.style.as_deref(), Some("research"));
        assert_eq!(team.instructions.trim(), "Cite a source for every claim.");
        assert_eq!(team.objective, "How does X work?");
        assert_eq!(team.workspace.path, "/work/notes");
        assert_eq!(team.workspace.isolation, Isolation::Shared);
        assert_eq!(team.defaults.model.as_deref(), Some("sonnet"));
        assert_eq!(team.agents[0].id, "scout");
        team.validate().unwrap();
        // The style and its instructions survive the roster round trip.
        let again = TeamConfig::from_yaml(&team.to_yaml().unwrap()).unwrap();
        assert_eq!(again, team);

        // An empty file is the plainest style; TOML works too.
        assert!(Style::parse("plain", "", false).unwrap().agents.is_empty());
        let toml = Style::parse("exp", "description = \"Run experiments\"\n", true).unwrap();
        assert_eq!(toml.description, "Run experiments");

        // Checked like a team file: bad names, unknown keys, broken rosters.
        assert!(Style::parse("Not A Name", "", false).is_err());
        assert!(Style::parse("x", "objective: no", false).is_err());
        assert!(Style::parse("x", "agents:\n  - id: a\n  - id: a\n", false).is_err());
        assert!(Style::parse("x", "orchestrator:\n  agent: false\n", false).is_err());

        // A team file without the new fields still loads, and does not grow them.
        let plain = TeamConfig::from_yaml("project: p\n").unwrap();
        assert!(plain.style.is_none() && plain.instructions.is_empty());
        assert!(!plain.to_yaml().unwrap().contains("style"));
        let mut long = plain.clone();
        long.instructions = "x".repeat(MAX_INSTRUCTIONS_BYTES + 1);
        assert!(long.validate().is_err());
    }

    const DEMO: &str = r#"
project: checkout-rewrite
objective: >
  Rewrite the checkout service while preserving API compatibility.
agents:
  - id: architect
    role: architecture
  - id: backend
    role: backend implementation
    runtime: codex
tasks:
  - id: design
    agent: architect
    description: Write the design
  - id: build
    agent: backend
    depends_on: [design]
    reviewer: architect
"#;

    #[test]
    fn parses_yaml_with_defaults() {
        let cfg = TeamConfig::from_yaml(DEMO).unwrap();
        assert_eq!(cfg.project, "checkout-rewrite");
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[1].runtime, Some(RuntimeKind::Codex));
        assert_eq!(cfg.defaults.runtime, RuntimeKind::Claude);
        assert_eq!(cfg.defaults.permissions, PermissionPolicy::Bypass);
        assert_eq!(cfg.backend, BackendChoice::Auto);
        assert_eq!(cfg.workspace.isolation, Isolation::Auto);
        assert!(cfg.orchestrator.agent);
        assert_eq!(cfg.tasks[1].depends_on, vec!["design"]);
    }

    #[test]
    fn parses_toml() {
        let cfg = TeamConfig::from_toml(
            r#"
project = "t"
[[agents]]
id = "a"
role = "x"
task = "do the thing"
"#,
        )
        .unwrap();
        let tasks = cfg.all_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "a-task");
        assert_eq!(tasks[0].agent.as_deref(), Some("a"));
    }

    #[test]
    fn rejects_bad_configs() {
        let bad_id = DEMO.replace("id: architect", "id: Architect");
        assert!(TeamConfig::from_yaml(&bad_id).is_err());

        let dup = DEMO.replace("id: backend", "id: architect");
        assert!(TeamConfig::from_yaml(&dup).is_err());

        let unknown_dep = DEMO.replace("depends_on: [design]", "depends_on: [nope]");
        assert!(TeamConfig::from_yaml(&unknown_dep).is_err());

        let reserved = DEMO.replace("id: backend", "id: orchestrator");
        assert!(TeamConfig::from_yaml(&reserved).is_err());

        let unknown_field = format!("{DEMO}\nbogus: 1\n");
        assert!(TeamConfig::from_yaml(&unknown_field).is_err());
    }

    #[test]
    fn rejects_dependency_cycles() {
        let cyclic = DEMO.replace(
            "  - id: design\n    agent: architect",
            "  - id: design\n    depends_on: [build]\n    agent: architect",
        );
        let err = TeamConfig::from_yaml(&cyclic).unwrap_err().to_string();
        assert!(err.contains("cycle"), "{err}");
    }

    #[test]
    fn an_objective_only_team_leaves_the_roster_to_the_orchestrator() {
        let cfg = TeamConfig::from_yaml("project: t\nobjective: build a thing\n").unwrap();
        assert!(cfg.agents.is_empty());
        assert!(cfg.orchestrator.agent);
        // Without an orchestrator agent somebody has to list the team.
        assert!(TeamConfig::from_yaml(
            "project: t\nobjective: build a thing\norchestrator:\n  agent: false\n"
        )
        .is_err());
        // A roster round-trips through YAML, so it can be saved and re-run.
        let demo = TeamConfig::from_yaml(DEMO).unwrap();
        assert_eq!(
            TeamConfig::from_yaml(&demo.to_yaml().unwrap()).unwrap(),
            demo
        );
    }

    #[test]
    fn shell_agents_need_a_command() {
        let cfg = r#"
project: t
agents:
  - id: a
    runtime: shell
"#;
        assert!(TeamConfig::from_yaml(cfg).is_err());
    }

    #[test]
    fn a_style_names_what_its_members_fall_back_to() {
        let style = Style::parse(
            "s",
            "defaults:\n  model_fallbacks:\n    fable: [opus, gpt-6-astra]\n",
            false,
        )
        .unwrap();
        assert_eq!(
            style.defaults.model_fallbacks["fable"],
            ["opus", "gpt-6-astra"]
        );
        // It travels with the team file, and is left out when empty.
        let team = style.team("p", ".", "o");
        assert!(team.to_yaml().unwrap().contains("model_fallbacks"));
        assert!(!Style::default()
            .team("p", ".", "o")
            .to_yaml()
            .unwrap()
            .contains("model_fallbacks"));
        for bad in [
            "defaults:\n  model_fallbacks:\n    fable: []\n",
            "defaults:\n  model_fallbacks:\n    fable: [fable]\n",
            "defaults:\n  model_fallbacks:\n    fable: [\"two words\"]\n",
            "defaults:\n  model_fallbacks:\n    fable: opus\n",
        ] {
            assert!(Style::parse("s", bad, false).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_team_names_the_voice_its_members_write_in() {
        assert_eq!(AgentDefaults::default().voice, Voice::Ultracave);
        let style = Style::parse("s", "defaults:\n  voice: megacave\n", false).unwrap();
        assert_eq!(style.defaults.voice, Voice::Megacave);
        assert!(style
            .team("p", ".", "o")
            .to_yaml()
            .unwrap()
            .contains("voice: megacave"));
        assert!(!Style::default()
            .team("p", ".", "o")
            .to_yaml()
            .unwrap()
            .contains("voice:"));
        assert!(Style::parse("s", "defaults:\n  voice: grunt\n", false).is_err());
    }

    #[test]
    fn a_style_names_what_takes_over_what_a_model_refuses() {
        let style = Style::parse(
            "s",
            "defaults:\n  refusal_fallbacks: [glm-5.3, deepseek/deepseek-flash, opus]\n",
            false,
        )
        .unwrap();
        assert_eq!(
            style.defaults.refusal_fallbacks,
            ["glm-5.3", "deepseek/deepseek-flash", "opus"]
        );
        assert!(style
            .team("p", ".", "o")
            .to_yaml()
            .unwrap()
            .contains("refusal_fallbacks"));
        assert!(!Style::default()
            .team("p", ".", "o")
            .to_yaml()
            .unwrap()
            .contains("refusal_fallbacks"));
        for bad in [
            "defaults:\n  refusal_fallbacks: [\"two words\"]\n",
            "defaults:\n  refusal_fallbacks: glm-5.3\n",
        ] {
            assert!(Style::parse("s", bad, false).is_err(), "{bad}");
        }
    }
}
