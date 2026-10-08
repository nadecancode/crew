//! Agent runtimes: how a particular coding-agent CLI is launched, how it
//! reports lifecycle signals, and how messages are phrased for it.
//!
//! `AgentRuntime` is the adapter seam. Claude Code and Codex are implemented;
//! `shell` runs an arbitrary command (used for scripted agents and tests).
//! Adding another CLI means adding one more implementation of this trait.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use crew_core::config::{AgentDefaults, AgentSpec};
use crew_core::{Agent, Message, RuntimeKind, Usage};

use crate::endpoints::Endpoint;
use crate::limits::{LimitInfo, QuotaReading};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod claude;
pub mod codex;
pub mod files;
pub mod home;
pub mod pi;
pub mod prompt;
pub mod shell;
pub mod toolbox;

/// Per-agent launch settings, resolved from the team defaults and the agent's
/// own overrides. Persisted so an agent can be restarted after a daemon restart.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct AgentSettings {
    /// Run without the runtime's permission checks.
    #[serde(default)]
    pub bypass_permissions: bool,
    pub permission_mode: String,
    pub allowed_tools: Vec<String>,
    pub sandbox: String,
    pub extra_args: Vec<String>,
    pub inherit_mcp: bool,
    pub auto_trust: bool,
    pub max_restarts: u32,
    /// Push messages over the daemon socket when the runtime supports it.
    #[serde(default = "default_true")]
    pub socket_delivery: bool,
    /// Shell runtime: the command line.
    pub command: Vec<String>,
    /// Ask for the 1M-token context window when the model has one.
    #[serde(default = "default_true")]
    pub long_context: bool,
    /// Let the runtime move the session to another model when its own is
    /// unavailable (its window used up, overloaded).
    #[serde(default = "default_true")]
    pub model_fallback: bool,
    /// How members write to one another.
    #[serde(default)]
    pub voice: crew_core::config::Voice,
    /// Runtime-neutral selection, resolved from the daemon's team catalog.
    #[serde(default)]
    pub toolbox: crew_core::toolbox::Resolved,
}

fn default_true() -> bool {
    true
}

impl AgentSettings {
    pub fn resolve(defaults: &AgentDefaults, spec: &AgentSpec) -> Self {
        let mut allowed_tools = defaults.allowed_tools.clone();
        allowed_tools.extend(spec.allowed_tools.iter().cloned());
        let mut extra_args = defaults.extra_args.clone();
        extra_args.extend(spec.extra_args.iter().cloned());
        Self {
            bypass_permissions: spec.permissions.unwrap_or(defaults.permissions)
                == crew_core::config::PermissionPolicy::Bypass,
            permission_mode: spec
                .permission_mode
                .clone()
                .unwrap_or_else(|| defaults.permission_mode.clone()),
            allowed_tools,
            sandbox: defaults.sandbox.clone(),
            extra_args,
            inherit_mcp: defaults.inherit_mcp,
            auto_trust: defaults.auto_trust,
            max_restarts: defaults.max_restarts,
            socket_delivery: defaults.delivery != crew_core::config::DeliveryChoice::Terminal,
            command: if spec.command.is_empty() {
                defaults.command.clone()
            } else {
                spec.command.clone()
            },
            long_context: defaults.long_context,
            model_fallback: defaults.model_fallback,
            voice: defaults.voice,
            toolbox: crew_core::toolbox::Resolved::default(),
        }
    }
}

/// Everything a runtime needs to build a command line.
pub struct LaunchContext<'a> {
    pub agent: &'a Agent,
    pub settings: &'a AgentSettings,
    /// Absolute path of the crew binary (for the MCP bridge and hooks).
    pub crew_bin: &'a Path,
    /// Private per-agent directory for generated config files.
    pub agent_dir: &'a Path,
    pub identity_path: &'a Path,
    /// Standing instructions: who the agent is and how the team works.
    pub briefing: &'a str,
    /// Resume the runtime's previous session instead of starting fresh.
    pub resume: bool,
    /// Endpoint and key from the crew config, replacing the user's own
    /// setup for this session.
    pub endpoint: &'a Endpoint,
    /// Start the model in its 1M-token context variant.
    pub long_context: bool,
    /// The agent's private home (`<A>/rw/home`) when it runs in a sandbox: the
    /// CLI keeps its configuration, credentials and sessions there instead of in
    /// the operator's.
    pub private_home: Option<&'a Path>,
}

/// A fully resolved process launch: a program and an argument array. Nothing
/// here is ever passed through a shell by crew.
#[derive(Clone, Debug, PartialEq)]
pub struct LaunchSpec {
    pub agent_id: String,
    pub kind: RuntimeKind,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// The first prompt, if there is one.
    pub prompt: Option<String>,
    /// The runtime accepts the first prompt as a trailing argument.
    pub prompt_as_arg: bool,
    /// A helper process the session depends on (Codex's app-server).
    pub sidecar: Option<Sidecar>,
    /// Run the program inside this sandbox (`crew sandbox-run` in front of it).
    /// Only the PTY backend honours it; the daemon refuses herdr for a
    /// sandboxed agent.
    pub sandbox: Option<crate::sandbox::spec::Spec>,
}

/// A background process that must be running before the agent starts. It has
/// no terminal; the daemon owns it for the lifetime of the agent.
#[derive(Clone, Debug, PartialEq)]
pub struct Sidecar {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Unix socket the sidecar listens on; it is ready once this exists.
    pub socket: PathBuf,
}

/// Lifecycle signals a runtime reports through its hooks.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    /// The session is up and accepts input.
    Ready,
    TurnStarted,
    TurnEnded,
    /// The runtime is showing a prompt only a human can answer.
    Blocked(String),
    Unblocked,
    /// What the agent is doing right now (e.g. the tool it is running).
    Activity(String),
    Session {
        id: Option<String>,
        transcript: Option<String>,
    },
    /// An in-session subagent started (+1) or stopped (-1).
    Subagents(i32),
    /// The runtime stopped because the account hit a usage limit (or ran
    /// out of credits, or lost its login).
    Limited(LimitInfo),
    /// The turn ended in some other API error.
    TurnFailed(String),
    /// The account's usage windows, as the runtime last saw them.
    Quota(Vec<QuotaReading>),
    /// What the session runs on, as far as this report says: the model's
    /// id, the reasoning effort.
    Model {
        model: Option<String>,
        effort: Option<String>,
    },
    /// How full the session's context window is: tokens used, window size.
    Context {
        tokens: u64,
        window: u64,
    },
    /// The session's context was compacted, by crew's request or by the
    /// runtime itself.
    Compacted,
    /// A compaction crew asked for was not carried out, and why.
    CompactSkipped(String),
    Exiting,
}

/// An interactive dialog found on the agent's screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    /// `trust`, `permission`, ...; `notice` for a menu that needs no
    /// answer (Codex's "Giving this request a little extra thought"),
    /// which crew dismisses whenever it sees one.
    pub kind: &'static str,
    pub description: String,
    /// Logical key presses (`enter`, `down`, `1`, …) that accept the
    /// dialog, in order; used only when the team opted in. Empty when crew
    /// will not answer this dialog.
    pub accept_keys: Vec<&'static str>,
}

pub trait AgentRuntime: Send + Sync {
    fn kind(&self) -> RuntimeKind;

    /// Write any per-agent config files and build the launch command.
    fn launch(&self, ctx: &LaunchContext<'_>) -> Result<LaunchSpec>;

    /// Translate a hook event (`crew hook <event>`) into lifecycle signals.
    fn on_hook(&self, event: &str, payload: &Value) -> Vec<Signal>;

    /// Recognise a blocking dialog in rendered terminal output.
    fn detect_dialog(&self, _screen: &str) -> Option<Dialog> {
        None
    }

    /// What to submit at the session's prompt to make it compact its
    /// context, keeping what `keep` names; `None` when the runtime has no
    /// such command.
    fn compact_command(&self, _keep: Option<&str>) -> Option<String> {
        None
    }

    /// Recognise a usage-limit message that is the last thing on the screen.
    fn detect_limit(&self, _screen: &str, _now_ms: i64) -> Option<LimitInfo> {
        None
    }

    /// Phrase incoming bus messages as input for this runtime's session.
    fn nudge(&self, messages: &[Message]) -> String {
        prompt::format_nudge(messages)
    }

    /// Token accounting from the runtime's transcript, when available.
    fn usage(&self, _transcript: &Path) -> Option<Usage> {
        None
    }

    /// A command (argument array) that injects `text` into the session over
    /// the runtime's own socket, for runtimes that have one.
    fn queue_command(&self, _socket: &Path, _session: &str, _text: &str) -> Option<Vec<String>> {
        None
    }
}

pub fn runtime_for(kind: RuntimeKind) -> Arc<dyn AgentRuntime> {
    match kind {
        RuntimeKind::Claude => Arc::new(claude::ClaudeRuntime),
        RuntimeKind::Codex => Arc::new(codex::CodexRuntime),
        RuntimeKind::Pi => Arc::new(pi::PiRuntime),
        RuntimeKind::Shell => Arc::new(shell::ShellRuntime),
    }
}

/// Quote a string for a POSIX shell. Crew itself never runs a shell; this is
/// only for the hook command strings that Claude Code's settings format
/// requires, and for typing a command into a herdr pane's shell.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'=' | b':' | b',')
        })
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Write an executable file for a test. A child process writes it: had the
/// (threaded) test process held the write descriptor, a fork in another test
/// thread could inherit it and make executing the file fail with ETXTBSY.
#[cfg(test)]
pub(crate) fn write_executable(path: &Path, text: &str) {
    let status = std::process::Command::new("sh")
        .args(["-c", r#"printf '%s' "$2" > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .arg(text)
        .status()
        .unwrap();
    assert!(status.success(), "writing {}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_quote("/usr/bin/crew"), "/usr/bin/crew");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
    }

    #[test]
    fn settings_merge_defaults_and_overrides() {
        let defaults = AgentDefaults {
            allowed_tools: vec!["Bash(cargo test:*)".into()],
            ..AgentDefaults::default()
        };
        let spec = AgentSpec {
            id: "a".into(),
            permission_mode: Some("plan".into()),
            allowed_tools: vec!["WebFetch".into()],
            ..AgentSpec::default()
        };
        let s = AgentSettings::resolve(&defaults, &spec);
        assert!(s.bypass_permissions, "bypass is the team default");
        let guarded = AgentSpec {
            permissions: Some(crew_core::config::PermissionPolicy::Guarded),
            ..AgentSpec::default()
        };
        assert!(!AgentSettings::resolve(&defaults, &guarded).bypass_permissions);
        assert_eq!(s.permission_mode, "plan");
        assert_eq!(s.allowed_tools, ["Bash(cargo test:*)", "WebFetch"]);
        assert_eq!(s.sandbox, "workspace-write");
        let s = AgentSettings::resolve(&defaults, &AgentSpec::default());
        assert_eq!(s.permission_mode, "acceptEdits");
    }
}
