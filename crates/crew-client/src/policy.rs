//! Shared runtime policy. Exporters borrow these descriptors; they do not own
//! a second timeout, command or validation table.

use crew_protocol::{
    AgentStatus, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES, MAX_OPTION_CHARS, MAX_QUESTION_OPTIONS,
};

use crate::{ClientError, ClientLimits, CommandKind, ConfirmationKind, GrantTier, Tone};

pub const DEFAULT_TIMEOUT_MS: u64 = 3_000;
pub const MESSAGE_RETRY_WAIT_MS: u64 = 30_000;
pub const NATIVE_LIMITS: ClientLimits = ClientLimits {
    messages: 1_000,
    events: 5_000,
    context_points: 600,
    buffered_pushes: 256,
    pending_operations: 64,
    page_size: 200,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayPermission {
    Read,
    Control,
    Terminal,
    Denied,
}

impl GatewayPermission {
    pub fn tier(self) -> Option<GrantTier> {
        match self {
            Self::Read => Some(GrantTier::Read),
            Self::Control => Some(GrantTier::Control),
            Self::Terminal => Some(GrantTier::Terminal),
            Self::Denied => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmationRule {
    None,
    Always(ConfirmationKind),
    RestartFlag(ConfirmationKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayPolicy {
    Never,
    IdentifiedSendOnClose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodPolicy {
    /// None denotes the ordinary unknown-method timeout fallback.
    pub kind: Option<CommandKind>,
    pub timeout_ms: u64,
    pub gateway: GatewayPermission,
    pub mutating: bool,
    pub confirmation: ConfirmationRule,
    pub replay: ReplayPolicy,
}

macro_rules! methods {
    ($($kind:ident: ($timeout:expr, $permission:ident, $mutating:expr, $confirmation:expr, $replay:ident)),+ $(,)?) => {
        pub static METHODS: &[MethodPolicy] = &[
            $(MethodPolicy { kind: Some(CommandKind::$kind), timeout_ms: $timeout,
                gateway: GatewayPermission::$permission, mutating: $mutating,
                confirmation: $confirmation, replay: ReplayPolicy::$replay }),+,
            MethodPolicy { kind: None, timeout_ms: DEFAULT_TIMEOUT_MS, gateway: GatewayPermission::Denied,
                mutating: false, confirmation: ConfirmationRule::None, replay: ReplayPolicy::Never },
        ];
    };
}

use ConfirmationKind::{KillAgent, RestartFresh, RestartLimited, StopTeam};
use ConfirmationRule::{Always, None as NoConfirmation, RestartFlag};

// Permission rows follow approved notes/native-gateway.md's exhaustive method
// table. LocalOperator still has the complete operator application set.
methods! {
    Ping: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    Snapshot: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    Subscribe: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    Workspaces: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    AgentsList: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    TasksList: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    TaskGet: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    History: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    MessageGet: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    Events: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    Inbox: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    AgentOutput: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    AgentTranscript: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    GitDiff: (DEFAULT_TIMEOUT_MS, Read, false, NoConfirmation, Never),
    ModelsList: (20_000, Control, false, NoConfirmation, Never),
    Send: (DEFAULT_TIMEOUT_MS, Control, true, NoConfirmation, IdentifiedSendOnClose),
    Answer: (DEFAULT_TIMEOUT_MS, Control, true, NoConfirmation, Never),
    AgentPause: (DEFAULT_TIMEOUT_MS, Control, true, NoConfirmation, Never),
    AgentResume: (30_000, Control, true, NoConfirmation, Never),
    AgentCompact: (DEFAULT_TIMEOUT_MS, Control, true, NoConfirmation, Never),
    AgentRestart: (30_000, Control, true, NoConfirmation, Never),
    AgentRestartFresh: (30_000, Control, true, Always(RestartFresh), Never),
    AgentKill: (30_000, Control, true, Always(KillAgent), Never),
    AgentSetModel: (30_000, Control, true, NoConfirmation, Never),
    AgentFocus: (DEFAULT_TIMEOUT_MS, Denied, true, NoConfirmation, Never),
    AgentInput: (DEFAULT_TIMEOUT_MS, Terminal, true, NoConfirmation, Never),
    Resume: (90_000, Control, true, RestartFlag(RestartLimited), Never),
    Shutdown: (90_000, Control, true, Always(StopTeam), Never),
    Attach: (DEFAULT_TIMEOUT_MS, Terminal, true, NoConfirmation, Never),
    AttachInput: (DEFAULT_TIMEOUT_MS, Terminal, true, NoConfirmation, Never),
    AttachResize: (DEFAULT_TIMEOUT_MS, Terminal, true, NoConfirmation, Never),
}

pub fn method_policy(kind: CommandKind) -> &'static MethodPolicy {
    METHODS
        .iter()
        .find(|policy| policy.kind == Some(kind))
        .expect("closed command policy")
}

pub fn timeout_for(method: &str) -> u64 {
    METHODS
        .iter()
        .find(|policy| policy.kind.is_some_and(|kind| kind.method() == method))
        .map_or(DEFAULT_TIMEOUT_MS, |policy| policy.timeout_ms)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebCommandKind {
    Declined,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandPolicy {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static str,
    pub about: &'static str,
    pub web: Option<WebCommandKind>,
}

macro_rules! commands {
    ($($name:literal: ($about:literal, $args:literal, [$($alias:literal),*], $web:expr)),+ $(,)?) => {
        pub const COMMANDS: &[CommandPolicy] = &[
            $(CommandPolicy { name: $name, about: $about, args: $args,
                aliases: &[$($alias),*], web: $web }),+
        ];
    };
}

commands! {
    "exit": ("leave the dashboard — the team keeps running", "", ["quit", "q"], Some(WebCommandKind::Declined)),
    "stop": ("stop the team: every agent and the daemon (asks first)", "", ["terminate", "shutdown"], None),
    "kill": ("stop one agent's process (asks first)", "[agent]", [], None),
    "restart": ("restart an agent, resuming its session", "[agent]", [], None),
    "restart-fresh": ("restart without resuming the old conversation (asks first)", "[agent]", [], None),
    "pause": ("suspend an agent", "[agent]", [], None),
    "unpause": ("let a suspended agent continue", "[agent]", [], None),
    "compact": ("compact an agent's context when its turn ends", "[agent]", [], None),
    "model": ("choose the model and CLI an agent runs on", "[agent]", ["cli", "harness"], None),
    "resume": ("continue the agents paused at a usage limit", "", [], None),
    "answer": ("answer the questions the team has for you", "", ["questions"], None),
    "tasks": ("show the task graph", "", [], None),
    "logs": ("show the event log", "", [], None),
    "mouse": ("turn mouse clicks on or off (off: drag selects text)", "", [], Some(WebCommandKind::Declined)),
    "help": ("keys and commands", "", ["?"], None),
    "usage": ("show usage: context windows, tokens, quotas", "", [], Some(WebCommandKind::New)),
    "graph": ("show the agent graph; reset forgets dragged positions", "[reset]", [], Some(WebCommandKind::New)),
    "keys": ("turn single-key shortcuts on or off", "on|off", [], Some(WebCommandKind::New)),
}

pub fn command_policy(name: &str) -> Option<&'static CommandPolicy> {
    COMMANDS.iter().find(|policy| {
        policy.name.eq_ignore_ascii_case(name)
            || policy
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    })
}

/// Leading whitespace and paths remain ordinary text. Unknown command-shaped
/// names still return Some so they cannot become agent messages by accident.
pub fn slash_command(text: &str) -> Option<(&str, &str)> {
    let line = text.strip_prefix('/')?;
    let end = line.find(char::is_whitespace).unwrap_or(line.len());
    let (name, rest) = line.split_at(end);
    let mut chars = name.chars();
    let word = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    (word || name == "?").then(|| (name, rest.trim()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusPolicy {
    pub status: AgentStatus,
    pub tone: Tone,
}

impl StatusPolicy {
    pub fn terminal(self) -> bool {
        self.status.is_terminal()
    }
}

pub static STATUSES: &[StatusPolicy] = &[
    StatusPolicy {
        status: AgentStatus::Spawning,
        tone: Tone::Info,
    },
    StatusPolicy {
        status: AgentStatus::Idle,
        tone: Tone::Neutral,
    },
    StatusPolicy {
        status: AgentStatus::Working,
        tone: Tone::Info,
    },
    StatusPolicy {
        status: AgentStatus::Waiting,
        tone: Tone::Warning,
    },
    StatusPolicy {
        status: AgentStatus::Blocked,
        tone: Tone::Warning,
    },
    StatusPolicy {
        status: AgentStatus::Done,
        tone: Tone::Success,
    },
    StatusPolicy {
        status: AgentStatus::Failed,
        tone: Tone::Danger,
    },
    StatusPolicy {
        status: AgentStatus::Stopped,
        tone: Tone::Neutral,
    },
];

pub fn status_policy(status: AgentStatus) -> &'static StatusPolicy {
    STATUSES
        .iter()
        .find(|policy| policy.status == status)
        .expect("closed agent status policy")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidationLimits {
    pub message_bytes: usize,
    pub frame_bytes: usize,
    pub question_options: usize,
    pub option_chars: usize,
    pub id_bytes: usize,
    pub message_id_bytes: usize,
}

pub const VALIDATION_LIMITS: ValidationLimits = ValidationLimits {
    message_bytes: MAX_MESSAGE_BYTES,
    frame_bytes: MAX_FRAME_BYTES,
    question_options: MAX_QUESTION_OPTIONS,
    option_chars: MAX_OPTION_CHARS,
    id_bytes: 32,
    message_id_bytes: 64,
};

#[derive(Clone, Copy, Debug)]
pub struct WebPolicy {
    pub method_timeouts: &'static [MethodPolicy],
    pub commands: &'static [CommandPolicy],
    pub statuses: &'static [StatusPolicy],
    pub limits: ValidationLimits,
}

pub fn web_policy() -> WebPolicy {
    WebPolicy {
        method_timeouts: METHODS,
        commands: COMMANDS,
        statuses: STATUSES,
        limits: VALIDATION_LIMITS,
    }
}

pub fn is_valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= VALIDATION_LIMITS.id_bytes
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

pub fn is_valid_message_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= VALIDATION_LIMITS.message_id_bytes
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

pub fn validate_limits(limits: ClientLimits) -> Result<(), ClientError> {
    for (field, value, maximum) in [
        ("messages", limits.messages, NATIVE_LIMITS.messages),
        ("events", limits.events, NATIVE_LIMITS.events),
        (
            "context_points",
            limits.context_points,
            NATIVE_LIMITS.context_points,
        ),
        (
            "buffered_pushes",
            limits.buffered_pushes,
            NATIVE_LIMITS.buffered_pushes,
        ),
        (
            "pending_operations",
            limits.pending_operations,
            NATIVE_LIMITS.pending_operations,
        ),
        ("page_size", limits.page_size, NATIVE_LIMITS.page_size),
    ] {
        if value == 0 || value > maximum {
            return Err(ClientError::InvalidInput {
                field: field.into(),
                reason: format!("expected 1–{maximum}"),
            });
        }
    }
    Ok(())
}
