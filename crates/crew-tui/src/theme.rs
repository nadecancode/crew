//! Colours and glyphs. One palette in truecolor, one approximated in the
//! 256-colour cube for terminals that do not advertise 24-bit colour.

use crew_core::types::{Agent, AgentStatus, EventLevel, RuntimeKind, TaskStatus, OPERATOR_ID, ORCHESTRATOR_ID};
use ratatui::style::{Color, Modifier, Style};

pub const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];
/// Milliseconds each spinner frame is shown.
pub const SPINNER_MS: i64 = 150;

pub fn spinner_index(now_ms: i64) -> usize {
    ((now_ms.max(0) / SPINNER_MS) % SPINNER.len() as i64) as usize
}

pub fn spinner(now_ms: i64) -> &'static str {
    SPINNER[spinner_index(now_ms)]
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub text: Color,
    pub muted: Color,
    /// Box borders and the ring.
    pub dim: Color,
    /// Spokes and other barely-there structure.
    pub faint: Color,
    /// Claude agents, peer-to-peer traffic and highlights.
    pub accent: Color,
    /// Codex agents.
    pub codex: Color,
    /// pi agents.
    pub pi: Color,
    /// Shell agents.
    pub shell: Color,
    /// The orchestrator and its traffic.
    pub orch: Color,
    pub ok: Color,
    pub err: Color,
    pub warn: Color,
    /// The "private channel" banner.
    pub p2p: Color,
    pub bar_bg: Color,
    /// Usage bars: the part that is used, and the track behind it.
    pub gauge: Color,
    pub gauge_track: Color,
}

impl Theme {
    pub fn truecolor() -> Self {
        Self {
            text: Color::Rgb(204, 208, 216),
            muted: Color::Rgb(124, 130, 142),
            dim: Color::Rgb(78, 82, 94),
            faint: Color::Rgb(52, 55, 64),
            accent: Color::Rgb(232, 140, 100),
            codex: Color::Rgb(86, 196, 178),
            pi: Color::Rgb(226, 190, 96),
            shell: Color::Rgb(190, 150, 230),
            orch: Color::Rgb(110, 190, 240),
            ok: Color::Rgb(126, 200, 126),
            err: Color::Rgb(240, 96, 96),
            warn: Color::Rgb(230, 190, 96),
            p2p: Color::Rgb(240, 104, 124),
            bar_bg: Color::Rgb(30, 32, 40),
            gauge: Color::Rgb(177, 185, 249),
            gauge_track: Color::Rgb(74, 78, 112),
        }
    }

    pub fn indexed() -> Self {
        Self {
            text: Color::Indexed(252),
            muted: Color::Indexed(245),
            dim: Color::Indexed(240),
            faint: Color::Indexed(237),
            accent: Color::Indexed(209),
            codex: Color::Indexed(79),
            pi: Color::Indexed(179),
            shell: Color::Indexed(140),
            orch: Color::Indexed(75),
            ok: Color::Indexed(114),
            err: Color::Indexed(203),
            warn: Color::Indexed(179),
            p2p: Color::Indexed(204),
            bar_bg: Color::Indexed(235),
            gauge: Color::Indexed(147),
            gauge_track: Color::Indexed(60),
        }
    }

    /// Truecolor when the terminal advertises it, the 256-colour palette otherwise.
    pub fn detect() -> Self {
        match std::env::var("COLORTERM") {
            Ok(v) if v.contains("truecolor") || v.contains("24bit") => Self::truecolor(),
            _ => Self::indexed(),
        }
    }

    pub fn fg(&self, color: Color) -> Style {
        Style::default().fg(color)
    }

    pub fn bold(&self, color: Color) -> Style {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    }

    /// Each runtime has a colour of its own, so a glance tells which CLI
    /// an agent runs on.
    pub fn runtime_color(&self, runtime: RuntimeKind) -> Color {
        match runtime {
            RuntimeKind::Claude => self.accent,
            RuntimeKind::Codex => self.codex,
            RuntimeKind::Pi => self.pi,
            RuntimeKind::Shell => self.shell,
        }
    }

    /// What stands in front of a team member's name: its runtime's glyph,
    /// or a subagent's tether, in the runtime's colour.
    pub fn member_mark(&self, runtime: RuntimeKind, subagent: bool) -> (&'static str, Color) {
        let glyph = if subagent { "↳" } else { runtime_glyph(runtime) };
        (glyph, self.runtime_color(runtime))
    }

    /// The colour a name is written in where agents are mentioned (the bus,
    /// tasks, logs): the orchestrator's, or the member's runtime's.
    pub fn name_color(&self, agents: &[Agent], id: &str) -> Color {
        if id == ORCHESTRATOR_ID || id == OPERATOR_ID {
            return self.orch;
        }
        match agents.iter().find(|a| a.id == id) {
            Some(agent) if agent.orchestrator => self.orch,
            Some(agent) => self.runtime_color(agent.runtime),
            // Not on the team (any more).
            None => self.text,
        }
    }

    /// The colour of an agent's state; at work it shows its runtime's.
    pub fn state_color(&self, agent: &Agent) -> Color {
        match agent.status {
            AgentStatus::Working => self.runtime_color(agent.runtime),
            status => self.agent_color(status),
        }
    }

    pub fn agent_color(&self, status: AgentStatus) -> Color {
        match status {
            AgentStatus::Working => self.accent,
            AgentStatus::Done => self.ok,
            AgentStatus::Failed => self.err,
            AgentStatus::Blocked => self.warn,
            AgentStatus::Spawning => self.orch,
            AgentStatus::Waiting | AgentStatus::Idle => self.muted,
            AgentStatus::Stopped => self.dim,
        }
    }

    pub fn task_color(&self, status: TaskStatus) -> Color {
        match status {
            TaskStatus::Working => self.accent,
            TaskStatus::Done => self.ok,
            TaskStatus::Failed => self.err,
            TaskStatus::Blocked => self.warn,
            TaskStatus::Review => self.orch,
            TaskStatus::Ready => self.text,
            TaskStatus::Waiting | TaskStatus::Queued => self.muted,
        }
    }

    pub fn level_color(&self, level: EventLevel) -> Color {
        match level {
            EventLevel::Debug => self.dim,
            EventLevel::Info => self.muted,
            EventLevel::Warn => self.warn,
            EventLevel::Error => self.err,
        }
    }
}

/// Glyph for the CLI an agent runs on.
/// The CLI's name, as people say it.
pub fn runtime_label(runtime: RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Claude => "Claude Code",
        RuntimeKind::Codex => "Codex",
        RuntimeKind::Pi => "pi",
        RuntimeKind::Shell => "shell",
    }
}

pub fn runtime_glyph(runtime: RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Claude => "✱",
        RuntimeKind::Codex => "❂",
        RuntimeKind::Pi => "π",
        RuntimeKind::Shell => "$",
    }
}

/// Glyph for an agent's state. Working agents get the animated spinner.
pub fn agent_icon(status: AgentStatus, paused: bool, now_ms: i64) -> &'static str {
    if paused {
        return "‖";
    }
    match status {
        AgentStatus::Working => spinner(now_ms),
        AgentStatus::Done => "✓",
        AgentStatus::Waiting => "○",
        AgentStatus::Blocked => "⊘",
        AgentStatus::Failed => "✗",
        AgentStatus::Spawning => "…",
        AgentStatus::Idle => "·",
        AgentStatus::Stopped => "■",
    }
}

pub fn task_icon(status: TaskStatus, now_ms: i64) -> &'static str {
    match status {
        TaskStatus::Queued => "·",
        TaskStatus::Ready => "▸",
        TaskStatus::Working => spinner(now_ms),
        TaskStatus::Waiting => "○",
        TaskStatus::Blocked => "⊘",
        TaskStatus::Review => "◈",
        TaskStatus::Done => "✓",
        TaskStatus::Failed => "✗",
    }
}
