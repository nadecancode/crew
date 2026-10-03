//! The launcher: pick a work style and state the objective, before a team
//! exists. `crew` shows it when it is about to start a new team.
//!
//! Like the dashboard it is pure presentation: the caller supplies the
//! choices (already resolved from the style folders) and gets back which one
//! was picked and the objective that was typed.

use anyhow::{Context, Result};
use crew_core::util::printable;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    self as term, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use ratatui::Frame;

use crate::slash::{self, slash_command};
use crate::theme::Theme;
use crate::views::compose::input_line;
use crate::widgets::canvas::put_str;
use crate::widgets::text::{fit, pad_right, printable_lines, width};

/// Longest objective the input accepts, in characters.
const MAX_OBJECTIVE_CHARS: usize = 4000;

/// One entry of the chooser.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyleChoice {
    pub name: String,
    /// Where it comes from: `built-in`, `user`, `project`, …
    pub origin: String,
    pub description: String,
    /// Label/value rows shown under the description (team, runtime, …).
    pub details: Vec<(String, String)>,
    /// The style's standing instructions, shown in full.
    pub instructions: String,
    /// The objective of the last session with this choice, which an empty
    /// objective continues.
    pub previous_objective: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Launcher {
    pub workspace: String,
    pub choices: Vec<StyleChoice>,
    pub selected: usize,
    pub objective: String,
    /// Cursor position as a character index into `objective`.
    pub cursor: usize,
    /// First visible line of the preview.
    pub scroll: u16,
    /// Why the last Enter did not launch.
    pub notice: Option<String>,
}

/// What the user decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// Index into the choices.
    pub choice: usize,
    pub objective: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Continue,
    Launch(Launch),
    Cancel,
}

impl Launcher {
    pub fn new(
        workspace: &str,
        choices: Vec<StyleChoice>,
        selected: usize,
        objective: &str,
    ) -> Self {
        let objective: String = sanitize(objective)
            .chars()
            .take(MAX_OBJECTIVE_CHARS)
            .collect();
        Self {
            workspace: workspace.to_string(),
            selected: selected.min(choices.len().saturating_sub(1)),
            choices,
            cursor: objective.chars().count(),
            objective,
            scroll: 0,
            notice: None,
        }
    }

    fn current(&self) -> Option<&StyleChoice> {
        self.choices.get(self.selected)
    }

    fn select(&mut self, delta: i32) {
        let len = self.choices.len() as i32;
        if len > 0 {
            self.selected = (self.selected as i32 + delta).rem_euclid(len) as usize;
            self.scroll = 0;
        }
    }

    fn byte_at(&self, char_index: usize) -> usize {
        self.objective
            .char_indices()
            .nth(char_index)
            .map_or(self.objective.len(), |(i, _)| i)
    }

    fn insert(&mut self, ch: char) {
        if self.objective.chars().count() >= MAX_OBJECTIVE_CHARS {
            return;
        }
        let at = self.byte_at(self.cursor);
        self.objective.insert(at, ch);
        self.cursor += 1;
    }

    fn delete_range(&mut self, from: usize, to: usize) {
        let (a, b) = (self.byte_at(from), self.byte_at(to));
        self.objective.replace_range(a..b, "");
        self.cursor = from;
    }

    /// Text pasted into the terminal goes into the objective as one line.
    pub fn paste(&mut self, text: &str) {
        for ch in sanitize(text).chars() {
            self.insert(ch);
        }
        self.notice = None;
    }

    fn clear(&mut self) {
        self.objective.clear();
        self.cursor = 0;
    }

    /// Run a slash command typed where the objective goes. Whatever it is,
    /// it is not an objective: no team is started with it.
    fn run_command(&mut self, name: &str, rest: &str) -> Step {
        let Some(command) = slash::find(slash::LAUNCHER, name) else {
            self.notice = Some(slash::unknown(name));
            return Step::Continue;
        };
        if command.args.is_empty() && !rest.is_empty() {
            self.notice = Some(slash::no_argument(command));
            return Step::Continue;
        }
        match command.name {
            "exit" => return Step::Cancel,
            "style" => {
                let wanted = rest.to_lowercase();
                let named = |c: &StyleChoice| c.name.to_lowercase();
                let found = self
                    .choices
                    .iter()
                    .position(|c| named(c) == wanted)
                    .or_else(|| {
                        self.choices
                            .iter()
                            .position(|c| !wanted.is_empty() && named(c).starts_with(&wanted))
                    });
                match found {
                    Some(index) => {
                        self.selected = index;
                        self.scroll = 0;
                        self.clear();
                        self.notice = None;
                    }
                    None => {
                        let names: Vec<&str> =
                            self.choices.iter().map(|c| c.name.as_str()).collect();
                        self.notice = Some(format!(
                            "no style named '{rest}' (there are: {})",
                            names.join(", ")
                        ));
                    }
                }
            }
            _ => {
                let usages: Vec<String> = slash::LAUNCHER
                    .iter()
                    .map(|c| format!("{}  {}", c.usage(), c.about))
                    .collect();
                self.clear();
                self.notice = Some(usages.join(" · "));
            }
        }
        Step::Continue
    }

    fn launch(&mut self) -> Step {
        if let Some((name, rest)) = slash_command(&self.objective) {
            let (name, rest) = (name.to_string(), rest.to_string());
            return self.run_command(&name, &rest);
        }
        let Some(choice) = self.current() else {
            return Step::Cancel;
        };
        let typed = self.objective.trim();
        let objective = if typed.is_empty() {
            choice.previous_objective.clone().unwrap_or_default()
        } else {
            typed.to_string()
        };
        if objective.trim().is_empty() {
            self.notice = Some("type what the team should do, then press enter".to_string());
            return Step::Continue;
        }
        Step::Launch(Launch {
            choice: self.selected,
            objective,
        })
    }

    /// A click on a style picks it; the wheel moves through the styles, or
    /// through the selected one's description when it is over that.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) {
        let Some(list) = list_rect(area, self) else {
            return;
        };
        let rows = Rect::new(
            list.x + 1,
            list.y + 1,
            list.width - 2,
            list.height.saturating_sub(2),
        );
        let over_list = rows.contains(Position::new(mouse.column, mouse.row));
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if over_list => {
                // The list keeps the selection in view; rows count from there.
                let first = self
                    .selected
                    .saturating_sub(usize::from(rows.height).saturating_sub(1));
                let index = first + usize::from(mouse.row - rows.y);
                if index < self.choices.len() {
                    self.selected = index;
                    self.scroll = 0;
                    self.notice = None;
                }
            }
            MouseEventKind::ScrollUp if over_list => {
                if self.selected > 0 {
                    self.select(-1);
                }
            }
            MouseEventKind::ScrollDown if over_list => {
                if self.selected + 1 < self.choices.len() {
                    self.select(1);
                }
            }
            MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(3),
            _ => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.objective.chars().count();
        match key.code {
            KeyCode::Esc => return Step::Cancel,
            KeyCode::Char('c' | 'd') if ctrl => return Step::Cancel,
            KeyCode::Enter => return self.launch(),
            KeyCode::Tab | KeyCode::BackTab if self.objective.starts_with('/') => {
                if let Some(completed) = slash::complete(slash::LAUNCHER, &self.objective) {
                    self.cursor = completed.chars().count();
                    self.objective = completed;
                }
            }
            KeyCode::Up | KeyCode::BackTab => self.select(-1),
            KeyCode::Down | KeyCode::Tab => self.select(1),
            KeyCode::Char('p') if ctrl => self.select(-1),
            KeyCode::Char('n') if ctrl => self.select(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(5),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Backspace if self.cursor > 0 => {
                self.delete_range(self.cursor - 1, self.cursor)
            }
            KeyCode::Delete if self.cursor < len => self.delete_range(self.cursor, self.cursor + 1),
            KeyCode::Char('u') if ctrl => self.delete_range(0, self.cursor),
            KeyCode::Char('k') if ctrl => self.delete_range(self.cursor, len),
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.objective.chars().collect();
                let mut start = self.cursor;
                while start > 0 && chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                while start > 0 && !chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                self.delete_range(start, self.cursor);
            }
            KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(ch)
            }
            _ => return Step::Continue,
        }
        self.notice = None;
        Step::Continue
    }
}

/// One line of printable text: line breaks become spaces, control
/// characters are dropped.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !c.is_control())
        .collect()
}

/// Show the launcher until the user launches or cancels (`None`).
pub fn run_launcher(mut launcher: Launcher) -> Result<Option<Launch>> {
    let signals = crew_core::signals::TerminationSignals::install()
        .context("watching termination signals")?;
    let mut terminal = crate::enter_terminal().context("preparing the terminal")?;
    let theme = Theme::detect();
    let mut outcome = || -> Result<Option<Launch>> {
        loop {
            if signals.is_pending() {
                return Ok(None);
            }
            terminal.draw(|frame| render(frame, &launcher, &theme))?;
            if !term::poll(std::time::Duration::from_millis(100))? {
                continue;
            }
            match term::read()? {
                TermEvent::Key(key) if key.kind != KeyEventKind::Release => {
                    match launcher.handle_key(key) {
                        Step::Continue => {}
                        Step::Launch(launch) => return Ok(Some(launch)),
                        Step::Cancel => return Ok(None),
                    }
                }
                TermEvent::Paste(text) => launcher.paste(&text),
                TermEvent::Mouse(mouse) => {
                    let size = terminal.size().unwrap_or_default();
                    launcher.handle_mouse(mouse, Rect::new(0, 0, size.width, size.height));
                }
                _ => {}
            }
        }
    };
    let result = outcome();
    crate::leave_terminal();
    result
}

pub fn render(frame: &mut Frame, launcher: &Launcher, theme: &Theme) {
    let area = frame.area();
    render_into(frame.buffer_mut(), area, launcher, theme);
}

fn panel(buf: &mut Buffer, rect: Rect, title: &str, focused: bool, theme: &Theme) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.fg(if focused { theme.accent } else { theme.dim }))
        .title(Span::styled(
            format!(" {} ", printable(title)),
            theme.bold(theme.text),
        ));
    let inner = block.inner(rect);
    block.render(rect, buf);
    inner
}

/// Where the list of styles is drawn; `None` on a screen too small for the
/// launcher.
fn list_rect(area: Rect, launcher: &Launcher) -> Option<Rect> {
    if area.width < 44 || area.height < 12 {
        return None;
    }
    let longest = launcher
        .choices
        .iter()
        .map(|c| width(&c.name) + width(&c.origin))
        .max()
        .unwrap_or(0);
    let list_width = ((longest + 8) as u16).clamp(24, 38).min(area.width / 2);
    Some(Rect::new(area.x, area.y + 1, list_width, area.height - 5))
}

fn render_into(buf: &mut Buffer, area: Rect, launcher: &Launcher, theme: &Theme) {
    let Some(list) = list_rect(area, launcher) else {
        put_str(
            buf,
            area,
            i32::from(area.x),
            i32::from(area.y),
            &fit(
                "terminal too small for the crew launcher",
                usize::from(area.width),
            ),
            theme.fg(theme.warn),
        );
        return;
    };
    let x = i32::from(area.x);
    let full = usize::from(area.width);

    // Header.
    let title = "crew · new team";
    put_str(
        buf,
        area,
        x + 1,
        i32::from(area.y),
        title,
        theme.bold(theme.orch),
    );
    let room = full.saturating_sub(width(title) + 4);
    let workspace = fit(&launcher.workspace, room);
    put_str(
        buf,
        area,
        x + full as i32 - 1 - width(&workspace) as i32,
        i32::from(area.y),
        &workspace,
        theme.fg(theme.muted),
    );

    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 5);
    let objective = Rect::new(area.x, body.bottom(), area.width, 3);
    let footer_y = i32::from(objective.bottom());

    // Styles on the left, the selected one in full on the right.
    let preview = Rect::new(list.right(), body.y, body.width - list.width, body.height);
    render_list(buf, list, launcher, theme);
    render_preview(buf, preview, launcher, theme);
    render_objective(buf, objective, launcher, theme);

    let (text, style) = match (
        &launcher.notice,
        slash::hint(slash::LAUNCHER, &launcher.objective),
    ) {
        (Some(notice), _) => (notice.clone(), theme.fg(theme.warn)),
        // A command is being typed: it will not be taken for the objective.
        (None, Some(hint)) => (hint, theme.fg(theme.muted)),
        (None, None) => (
            "↑↓ style · type the objective · enter launch · pgup/pgdn scroll · esc cancel"
                .to_string(),
            theme.fg(theme.dim),
        ),
    };
    put_str(
        buf,
        area,
        x + 1,
        footer_y,
        &fit(&text, full.saturating_sub(2)),
        style,
    );
}

fn render_list(buf: &mut Buffer, rect: Rect, launcher: &Launcher, theme: &Theme) {
    let inner = panel(buf, rect, "work style", false, theme);
    let rows = usize::from(inner.height);
    let cols = usize::from(inner.width);
    if rows == 0 || cols < 6 {
        return;
    }
    // Keep the selection in view.
    let first = launcher.selected.saturating_sub(rows.saturating_sub(1));
    for (row, (index, choice)) in launcher
        .choices
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .enumerate()
    {
        let y = i32::from(inner.y) + row as i32;
        let selected = index == launcher.selected;
        let origin = fit(&choice.origin, cols.saturating_sub(6) / 2);
        let name_room = cols.saturating_sub(6 + width(&origin));
        let line = format!(
            " {} {} {}",
            if selected { "▸" } else { " " },
            pad_right(&choice.name, name_room),
            origin
        );
        let style = if selected {
            theme.bold(theme.accent).add_modifier(Modifier::REVERSED)
        } else {
            theme.fg(theme.text)
        };
        put_str(
            buf,
            inner,
            i32::from(inner.x),
            y,
            &pad_right(&line, cols),
            style,
        );
    }
}

fn render_preview(buf: &mut Buffer, rect: Rect, launcher: &Launcher, theme: &Theme) {
    let Some(choice) = launcher.current() else {
        panel(buf, rect, "no styles", false, theme);
        return;
    };
    let inner = panel(buf, rect, &choice.name, false, theme);
    if inner.width < 8 || inner.height == 0 {
        return;
    }
    let mut lines: Vec<Line> = Vec::new();
    if !choice.description.is_empty() {
        lines.push(Line::styled(
            choice.description.clone(),
            theme.fg(theme.text),
        ));
        lines.push(Line::default());
    }
    let label_width = choice
        .details
        .iter()
        .map(|(l, _)| width(l))
        .max()
        .unwrap_or(0);
    for (label, value) in &choice.details {
        lines.push(Line::from(vec![
            Span::styled(pad_right(label, label_width + 2), theme.fg(theme.muted)),
            Span::styled(value.clone(), theme.fg(theme.text)),
        ]));
    }
    if !choice.instructions.trim().is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled("How the team works", theme.bold(theme.orch)));
        for line in choice.instructions.trim_end().lines() {
            lines.push(Line::styled(line.to_string(), theme.fg(theme.muted)));
        }
    }
    printable_lines(&mut lines);
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((launcher.scroll, 0))
        .render(
            Rect::new(inner.x + 1, inner.y, inner.width - 1, inner.height),
            buf,
        );
}

fn render_objective(buf: &mut Buffer, rect: Rect, launcher: &Launcher, theme: &Theme) {
    let inner = panel(buf, rect, "objective", true, theme);
    if inner.width < 8 || inner.height == 0 {
        return;
    }
    let x = i32::from(inner.x) + 1;
    let y = i32::from(inner.y);
    let cols = usize::from(inner.width).saturating_sub(2);
    put_str(buf, inner, x, y, "› ", theme.bold(theme.accent));
    let text_x = x + 2;
    let room = cols.saturating_sub(3);
    let cursor_style = theme.fg(theme.text).add_modifier(Modifier::REVERSED);

    if launcher.objective.is_empty() {
        put_str(buf, inner, text_x, y, " ", cursor_style);
        let hint = match launcher
            .current()
            .and_then(|c| c.previous_objective.as_deref())
        {
            Some(previous) => format!("enter continues: {previous}"),
            None => "what should the team do?".to_string(),
        };
        put_str(
            buf,
            inner,
            text_x + 2,
            y,
            &fit(&hint, room.saturating_sub(2)),
            theme.fg(theme.dim),
        );
        return;
    }
    input_line(
        buf,
        inner,
        (text_x, y),
        room + 1,
        &launcher.objective,
        Some(launcher.cursor),
        theme,
    );
}

/// Render one frame as plain text (tests, documentation).
#[doc(hidden)]
pub fn render_to_string(launcher: &Launcher, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render_into(&mut buf, area, launcher, &Theme::truecolor());
    crate::widgets::canvas::buffer_text(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_objective_renders_within_a_frame_budget() {
        let mut launcher = launcher();
        launcher.objective = format!("{}tail", "x".repeat(3996));
        launcher.cursor = 4000;
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        let mut fastest = std::time::Duration::MAX;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            render_into(&mut buf, area, &launcher, &Theme::truecolor());
            fastest = fastest.min(start.elapsed());
        }
        assert!(buf
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>()
            .contains("tail"));
        eprintln!("4000-character launcher frame: {fastest:?}");
        assert!(
            fastest < std::time::Duration::from_millis(20),
            "{fastest:?}"
        );
    }

    #[test]
    fn style_preview_never_renders_terminal_controls() {
        let poison = "\u{1b}]52;c;Y29tbWFuZA==\u{7}\u{9b}2J\tvisible";
        let launcher = Launcher::new(
            "/work",
            vec![StyleChoice {
                name: poison.into(),
                description: poison.into(),
                details: vec![(poison.into(), poison.into())],
                instructions: poison.into(),
                ..StyleChoice::default()
            }],
            0,
            "",
        );
        let area = Rect::new(0, 0, 180, 40);
        let mut buf = Buffer::empty(area);
        render_into(&mut buf, area, &launcher, &Theme::truecolor());
        for cell in &buf.content {
            assert!(
                !cell.symbol().chars().any(char::is_control),
                "{:?}",
                cell.symbol()
            );
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    fn type_text(launcher: &mut Launcher, text: &str) {
        for ch in text.chars() {
            assert_eq!(launcher.handle_key(key(KeyCode::Char(ch))), Step::Continue);
        }
    }

    fn launcher() -> Launcher {
        let choice = |name: &str, origin: &str, description: &str| StyleChoice {
            name: name.into(),
            origin: origin.into(),
            description: description.into(),
            details: vec![("team".into(), "staffed by the orchestrator".into())],
            ..StyleChoice::default()
        };
        let mut research = choice("research", "built-in", "Investigate a question.");
        research.instructions = "Cite a source for every claim.\nLook for evidence against.".into();
        research.details.push((
            "last time".into(),
            "3 members: scout, skeptic, writer".into(),
        ));
        research.previous_objective = Some("How does the scheduler pick tasks?".into());
        Launcher::new(
            "/work/payments",
            vec![
                choice("default", "built-in", "General software work."),
                research,
                choice("field-notes", "project", "Our own style."),
            ],
            0,
            "",
        )
    }

    #[test]
    fn the_chooser_shows_the_styles_and_the_selected_one_in_full() {
        let mut l = launcher();
        let frame = render_to_string(&l, 100, 20);
        assert!(frame.contains("crew · new team"), "{frame}");
        assert!(frame.contains("/work/payments"), "{frame}");
        assert!(frame.contains("▸ default"), "{frame}");
        assert!(
            frame.contains("research") && frame.contains("field-notes"),
            "{frame}"
        );
        assert!(frame.contains("project"), "{frame}");
        assert!(frame.contains("General software work."), "{frame}");
        assert!(frame.contains("what should the team do?"), "{frame}");
        assert!(frame.contains("enter launch"), "{frame}");

        assert_eq!(l.handle_key(key(KeyCode::Down)), Step::Continue);
        let frame = render_to_string(&l, 100, 20);
        assert!(frame.contains("▸ research"), "{frame}");
        assert!(frame.contains("Investigate a question."), "{frame}");
        assert!(
            frame.contains("last time  3 members: scout, skeptic, writer"),
            "{frame}"
        );
        assert!(frame.contains("How the team works"), "{frame}");
        assert!(frame.contains("Cite a source for every claim."), "{frame}");
        assert!(
            frame.contains("enter continues: How does the scheduler pick tasks?"),
            "{frame}"
        );

        // Selection wraps around in both directions.
        l.handle_key(key(KeyCode::Down));
        l.handle_key(key(KeyCode::Tab));
        assert_eq!(l.selected, 0);
        l.handle_key(key(KeyCode::Up));
        assert_eq!(l.selected, 2);
        l.handle_key(ctrl('p'));
        assert_eq!(l.selected, 1);
    }

    #[test]
    fn enter_launches_the_selected_style_with_the_typed_objective() {
        let mut l = launcher();
        // Nothing typed and nothing to continue: stay, and say why.
        assert_eq!(l.handle_key(key(KeyCode::Enter)), Step::Continue);
        assert!(render_to_string(&l, 100, 20).contains("type what the team should do"));

        type_text(&mut l, "  compare the two queue designs ");
        assert!(l.notice.is_none());
        assert!(render_to_string(&l, 100, 20).contains("compare the two queue designs"));
        l.handle_key(key(KeyCode::Down));
        assert_eq!(
            l.handle_key(key(KeyCode::Enter)),
            Step::Launch(Launch {
                choice: 1,
                objective: "compare the two queue designs".into(),
            })
        );

        // An empty objective continues the style's last session.
        let mut l = launcher();
        l.handle_key(key(KeyCode::Down));
        assert_eq!(
            l.handle_key(key(KeyCode::Enter)),
            Step::Launch(Launch {
                choice: 1,
                objective: "How does the scheduler pick tasks?".into(),
            })
        );
        assert_eq!(launcher().handle_key(key(KeyCode::Esc)), Step::Cancel);
        assert_eq!(launcher().handle_key(ctrl('c')), Step::Cancel);
    }

    #[test]
    fn a_slash_line_is_a_command_and_never_the_objective() {
        // /exit typed where the objective goes leaves; no team is started.
        for line in ["/exit", "/quit", "/q", "/cancel", "/stop", "/Exit"] {
            let mut l = launcher();
            type_text(&mut l, line);
            assert_eq!(l.handle_key(key(KeyCode::Enter)), Step::Cancel, "{line}");
        }

        // Neither is a command that does not exist: it stays in the box.
        let mut l = launcher();
        type_text(&mut l, "/compact");
        assert!(render_to_string(&l, 100, 20).contains("no command /compact"));
        assert_eq!(l.handle_key(key(KeyCode::Enter)), Step::Continue);
        assert_eq!(l.objective, "/compact");
        assert!(l.notice.as_deref().unwrap().contains("no command /compact"));
        l.handle_key(ctrl('u'));
        type_text(&mut l, "/exit now");
        assert_eq!(l.handle_key(key(KeyCode::Enter)), Step::Continue);
        assert_eq!(l.notice.as_deref(), Some("/exit takes no argument"));

        // While a command is typed the footer describes it, and tab completes.
        let mut l = launcher();
        type_text(&mut l, "/e");
        let frame = render_to_string(&l, 100, 20);
        assert!(
            frame.contains("/exit  leave without starting a team"),
            "{frame}"
        );
        l.handle_key(key(KeyCode::Tab));
        assert_eq!(
            (l.objective.as_str(), l.cursor, l.selected),
            ("/exit", 5, 0)
        );

        // /style picks a style by name or the start of one.
        let mut l = launcher();
        type_text(&mut l, "/style field");
        assert_eq!(l.handle_key(key(KeyCode::Enter)), Step::Continue);
        assert_eq!((l.selected, l.objective.as_str(), l.cursor), (2, "", 0));
        type_text(&mut l, "/style nope");
        assert_eq!(l.handle_key(key(KeyCode::Enter)), Step::Continue);
        assert!(l
            .notice
            .as_deref()
            .unwrap()
            .contains("no style named 'nope' (there are: default, research, field-notes)"));
        l.handle_key(ctrl('u'));
        type_text(&mut l, "/help");
        assert_eq!(l.handle_key(key(KeyCode::Enter)), Step::Continue);
        assert!(l.objective.is_empty());
        assert!(render_to_string(&l, 140, 20).contains("/style <name>  pick a work style by name"));

        // A path, or a line given after a space, is an objective.
        for text in [
            "/src/queue.rs drops messages; find out why",
            " /exit handling needs a test",
        ] {
            let mut l = launcher();
            type_text(&mut l, text);
            assert_eq!(
                l.handle_key(key(KeyCode::Enter)),
                Step::Launch(Launch {
                    choice: 0,
                    objective: text.trim().into(),
                }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_click_picks_a_style_and_the_wheel_scrolls() {
        let area = Rect::new(0, 0, 100, 20);
        let mouse = |l: &mut Launcher, kind, column, row| {
            l.handle_mouse(
                MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
                area,
            )
        };
        let click = MouseEventKind::Down(MouseButton::Left);
        let mut l = launcher();
        // The styles are listed from the third row of the screen down.
        let frame = render_to_string(&l, 100, 20);
        let row_of =
            |name: &str| frame.lines().position(|line| line.contains(name)).unwrap() as u16;
        mouse(&mut l, click, 5, row_of("field-notes"));
        assert_eq!(l.selected, 2);
        mouse(&mut l, click, 5, row_of("▸ default"));
        assert_eq!(l.selected, 0);
        // Under the last style, on the border, or in the preview: nothing.
        for (x, y) in [(5, 10), (0, 3), (60, 3)] {
            mouse(&mut l, click, x, y);
            assert_eq!(l.selected, 0, "({x}, {y})");
        }
        // The wheel walks the list without wrapping, and scrolls the preview.
        mouse(&mut l, MouseEventKind::ScrollUp, 5, 3);
        assert_eq!(l.selected, 0);
        mouse(&mut l, MouseEventKind::ScrollDown, 5, 3);
        mouse(&mut l, MouseEventKind::ScrollDown, 5, 3);
        mouse(&mut l, MouseEventKind::ScrollDown, 5, 3);
        assert_eq!(l.selected, 2);
        mouse(&mut l, MouseEventKind::ScrollDown, 60, 5);
        assert_eq!(l.scroll, 3);
        mouse(&mut l, MouseEventKind::ScrollUp, 60, 5);
        assert_eq!(l.scroll, 0);
        // Too small a screen has no list to click.
        l.handle_mouse(
            MouseEvent {
                kind: click,
                column: 1,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 30, 8),
        );
        assert_eq!(l.selected, 2);
    }

    #[test]
    fn the_objective_is_an_editable_line() {
        let mut l = launcher();
        type_text(&mut l, "build a queue");
        l.handle_key(key(KeyCode::Home));
        type_text(&mut l, "re");
        assert_eq!(l.objective, "rebuild a queue");
        l.handle_key(key(KeyCode::End));
        l.handle_key(key(KeyCode::Backspace));
        l.handle_key(ctrl('w'));
        assert_eq!(l.objective, "rebuild a ");
        l.handle_key(key(KeyCode::Left));
        l.handle_key(key(KeyCode::Left));
        l.handle_key(key(KeyCode::Delete));
        assert_eq!(l.objective, "rebuild  ");
        l.handle_key(ctrl('u'));
        assert_eq!(l.objective, " ");
        l.handle_key(ctrl('k'));
        assert_eq!((l.objective.as_str(), l.cursor), ("", 0));

        // Pasted text becomes one line; wide characters and long text are fine.
        l.paste("survey 日本語 sources\r\nand compare\tthem\u{7}");
        assert_eq!(l.objective, "survey 日本語 sources  and compare them");
        l.paste(&"x".repeat(MAX_OBJECTIVE_CHARS * 2));
        assert_eq!(l.objective.chars().count(), MAX_OBJECTIVE_CHARS);
        let frame = render_to_string(&l, 60, 14);
        assert!(frame.lines().all(|line| width(line) <= 60), "{frame}");

        // A prefilled objective (from the command line) is editable too.
        let l = Launcher::new("/w", launcher().choices, 9, "from the\ncommand line");
        assert_eq!(l.objective, "from the command line");
        assert_eq!(l.cursor, 21);
        assert_eq!(l.selected, 2, "an out-of-range selection is clamped");
    }

    #[test]
    fn small_and_empty_screens_do_not_panic() {
        let l = launcher();
        for (w, h) in [(0, 0), (1, 1), (20, 5), (43, 30), (44, 12), (200, 60)] {
            let frame = render_to_string(&l, w, h);
            assert!(
                frame.lines().all(|line| width(line) <= usize::from(w)),
                "{w}x{h}"
            );
        }
        assert!(render_to_string(&l, 30, 8).contains("too small"));
        let empty = Launcher::new("/w", Vec::new(), 0, "");
        assert!(render_to_string(&empty, 80, 20).contains("no styles"));
        assert_eq!(
            Launcher::new("/w", Vec::new(), 0, "x").handle_key(key(KeyCode::Enter)),
            Step::Cancel
        );
        // A long list keeps the selection on screen.
        let mut many = launcher();
        many.choices = (0..40)
            .map(|i| StyleChoice {
                name: format!("style-{i}"),
                ..StyleChoice::default()
            })
            .collect();
        many.selected = 39;
        assert!(render_to_string(&many, 80, 16).contains("▸ style-39"));
    }
}
