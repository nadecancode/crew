//! Key reference overlay.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::slash;
use crate::theme::Theme;

const KEYS: &[(&str, &str)] = &[
    ("tab / shift-tab", "cycle agents (arrows work too)"),
    ("enter", "inspect the selected agent"),
    ("o", "select the orchestrator"),
    ("m", "message the selected agent"),
    ("M", "broadcast to every agent"),
    ("t", "task graph"),
    ("l", "event log"),
    ("p", "pause / resume the selected agent"),
    ("r", "restart the selected agent (R in the inspector)"),
    ("k", "kill the selected agent (asks first)"),
    ("g", "jump to the agent's herdr pane"),
    ("O", "choose the orchestrator's model and CLI"),
    ("s", "choose the selected agent's model and CLI"),
    ("u", "continue the agents paused at a usage limit"),
    ("U", "restart them on a new login or key (asks first)"),
    ("a", "reopen a question you put off with esc"),
    ("S", "stop the team: agents and daemon (asks first)"),
    ("/", "type a command (listed below)"),
    ("click", "open an agent or a bus message; wheel scrolls"),
    (
        "click a model",
        "a box's bottom label: choose its model and CLI",
    ),
    (
        "click terminal",
        "inspector: type straight into it · ctrl-] stops",
    ),
    ("f", "inspector: the terminal pane fills the view (toggle)"),
    (
        "right-click",
        "back / close, like esc (shift-drag selects text)",
    ),
    ("esc", "back / close"),
    ("q", "quit the dashboard — agents keep running"),
];

/// What the symbols mean and the keys of the other views, below the commands.
const NOTES: &[(&str, &str)] = &[
    (
        "✱  ❂  π  $",
        "an agent on Claude Code · Codex · pi · a shell",
    ),
    (
        "↳ box",
        "a subagent, tethered to the agent that launched it",
    ),
    ("⑂N  +N", "in-session subagents · subagents without a box"),
    (
        "temp",
        "a member added mid-run: it leaves once its work is done",
    ),
    (
        "inspector",
        "i type into the terminal · r refresh · d diff · ↑ transcript",
    ),
    (
        "? on a box",
        "it waits for your answer; the box opens itself",
    ),
    ("event log", "f filter to selected agent · Home/End"),
    ("compose", "a line starting with / is a command, never sent"),
    (
        "shift+enter",
        "a new line in any box (alt+enter or ctrl+j if not)",
    ),
];

/// Columns of the overlay, and of its left (key) column.
const WIDTH: u16 = 80;
const KEY_WIDTH: usize = 16;

/// The slash commands, packed into as few rows as fit.
fn command_rows() -> Vec<String> {
    slash::usage_rows(slash::DASHBOARD, usize::from(WIDTH) - KEY_WIDTH - 6)
}

/// A rectangle of at most `w`×`h` centred in `area`.
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

pub fn render(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let commands = command_rows();
    let rows = KEYS.len() + commands.len() + 1 + NOTES.len();
    let rect = centered(area, WIDTH, rows as u16 + 2);
    if rect.width < 12 || rect.height < 3 {
        return;
    }
    Clear.render(rect, buf);
    let mut lines = Vec::new();
    let mut row = |key: &str, what: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("  {key:<KEY_WIDTH$}"), theme.bold(theme.accent)),
            Span::styled(what, theme.fg(theme.text)),
        ]));
    };
    for (key, what) in KEYS {
        row(key, (*what).to_string());
    }
    for (index, usages) in commands.into_iter().enumerate() {
        row(if index == 0 { "commands" } else { "" }, usages);
    }
    row("", String::new());
    for (key, what) in NOTES {
        row(key, (*what).to_string());
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.fg(theme.muted))
        .title(Span::styled(" crew · keys ", theme.bold(theme.text)));
    Paragraph::new(lines).block(block).render(rect, buf);
}
