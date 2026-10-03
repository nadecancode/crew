//! Key handling. Pure: mutates the app state and returns the daemon requests
//! the key press calls for.

use crew_core::types::BROADCAST;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::app::{
    App, Command, Compose, MessageView, Modal, ModelPicker, QuestionBox, View, HARNESSES,
};
use crate::slash::{self, slash_command, SlashCommand};
use crate::views::dashboard::Hit;
use crate::views::{self, logs, message};

/// Longest message the compose box accepts, in characters.
const MAX_COMPOSE_CHARS: usize = 8000;

/// Handle a key press. `page` is the number of rows a PgUp/PgDn should move.
pub fn handle_key(app: &mut App, key: KeyEvent, now: i64, page: usize) -> Vec<Command> {
    app.mark_dirty();
    // Typing into an agent's terminal: every key is the agent's, Ctrl+C
    // included; Ctrl+] is the one that comes back.
    if app.view == View::Detail && app.detail.typing && app.modal.is_none() && !app.help {
        return typing_key(app, key, now);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return vec![Command::Quit];
    }
    if app.modal.is_some() {
        return modal_key(app, key, now);
    }
    if app.help {
        app.help = false;
        return Vec::new();
    }
    match app.view {
        View::Dashboard => dashboard_key(app, key, now),
        View::Detail => detail_key(app, key, now, page),
        View::Tasks => tasks_key(app, key, now, page),
        View::Logs => logs_key(app, key, now, page),
    }
}

/// Rows the wheel moves per notch.
const WHEEL_ROWS: usize = 3;

/// Handle a mouse event on a screen of `area`: a click opens what it lands
/// on, the wheel scrolls, the right button goes back.
pub fn handle_mouse(
    app: &mut App,
    mouse: MouseEvent,
    area: Rect,
    now: i64,
    page: usize,
) -> Vec<Command> {
    if !app.mouse {
        // Switched off: the terminal should not be reporting, and if it
        // still does, nothing listens.
        return Vec::new();
    }
    app.screen.set(area);
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => click(app, area, mouse.column, mouse.row, now),
        // Back, wherever Esc goes back: out of a box, out of a view. While
        // typing into a terminal Esc is the agent's, so this stops the
        // typing instead.
        MouseEventKind::Down(MouseButton::Right)
            if app.view == View::Detail && app.detail.typing =>
        {
            app.mark_dirty();
            app.detail.typing = false;
            Vec::new()
        }
        MouseEventKind::Down(MouseButton::Right) => handle_key(app, key(KeyCode::Esc), now, page),
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let up = mouse.kind == MouseEventKind::ScrollUp;
            app.mark_dirty();
            let message_lines = match &app.modal {
                Some(Modal::Message(view)) => message::scroll_limit(app, view, area),
                _ => 0,
            };
            match &mut app.modal {
                Some(Modal::Message(view)) => {
                    scroll_message(view, up, WHEEL_ROWS, message_lines);
                    Vec::new()
                }
                Some(Modal::Question(_)) => handle_key(
                    app,
                    key(if up {
                        KeyCode::PageUp
                    } else {
                        KeyCode::PageDown
                    }),
                    now,
                    page,
                ),
                Some(_) => Vec::new(),
                None if app.help || app.view == View::Dashboard => Vec::new(),
                None => {
                    let code = if up { KeyCode::Up } else { KeyCode::Down };
                    let mut commands = Vec::new();
                    for _ in 0..WHEEL_ROWS {
                        commands.extend(handle_key(app, key(code), now, page));
                    }
                    commands
                }
            }
        }
        // Movement and releases mean nothing here, and cost no redraw.
        _ => Vec::new(),
    }
}

fn click(app: &mut App, area: Rect, x: u16, y: u16, now: i64) -> Vec<Command> {
    app.mark_dirty();
    if app.modal.is_some() {
        // A box is answered with the keyboard; a stray click changes nothing.
        return Vec::new();
    }
    if app.help {
        app.help = false;
        return Vec::new();
    }
    let hit = views::hit(app, area, x, y);
    // A click into the terminal pane types into it; one anywhere else
    // stops.
    if app.view == View::Detail {
        let into = matches!(hit, Some(Hit::Terminal));
        if into && !app.detail.typing {
            app.detail.typing = true;
            app.detail.scroll = 0;
            app.detail.output_at = None;
        } else if !into {
            app.detail.typing = false;
        }
    }
    match hit {
        Some(Hit::Terminal) => Vec::new(),
        Some(Hit::Agent(id)) => app.open_detail(&id, now),
        Some(Hit::Model(id)) => {
            app.selected = Some(id.clone());
            app.open_model_picker(&id, now)
        }
        Some(Hit::Message(id)) => {
            app.modal = Some(Modal::Message(MessageView { id, scroll: 0 }));
            Vec::new()
        }
        Some(Hit::Questions) => {
            open_questions(app, now);
            Vec::new()
        }
        None => Vec::new(),
    }
}

/// Bracketed paste: into a box, or straight into the terminal being
/// typed into.
pub fn handle_paste(app: &mut App, text: &str, now: i64) -> Vec<Command> {
    if app.view == View::Detail && app.detail.typing && app.modal.is_none() {
        // A pasted ESC could close the wrapper early and turn the tail into keys.
        let text = text.replace('\u{1b}', "");
        app.mark_dirty();
        app.detail.output_at = None;
        app.detail.key_at = now;
        return vec![Command::Keys {
            id: app.detail.agent.clone(),
            text: format!("\x1b[200~{text}\x1b[201~"),
        }];
    }
    // Pasted into the answer box, text is the operator's own answer.
    if let Some(Modal::Question(answer)) = &mut app.modal {
        answer.choice = usize::MAX;
    }
    let input = match &mut app.modal {
        Some(Modal::Compose(compose) | Modal::Type(compose)) => Some(compose),
        Some(Modal::Question(answer)) => Some(&mut answer.input),
        _ => None,
    };
    if let Some(compose) = input {
        for ch in text.replace("\r\n", "\n").chars() {
            let ch = if ch == '\r' { '\n' } else { ch };
            if ch == '\n' || !ch.is_control() {
                insert(compose, ch);
            }
        }
        compose.notice = None;
        app.mark_dirty();
    }
    Vec::new()
}

/// A key typed into the agent's terminal.
fn typing_key(app: &mut App, key: KeyEvent, now: i64) -> Vec<Command> {
    if key.code == KeyCode::Char(']') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.detail.typing = false;
        return Vec::new();
    }
    let Some(text) = key_bytes(key) else {
        return Vec::new();
    };
    // The screen is read again right away, to show the key land.
    app.detail.output_at = None;
    app.detail.key_at = now;
    vec![Command::Keys {
        id: app.detail.agent.clone(),
        text,
    }]
}

/// What a terminal sends for a key: the character, or the usual escape
/// sequence for a special key. `None` for keys a terminal does not send.
pub fn key_bytes(key: KeyEvent) -> Option<String> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let shift = m.contains(KeyModifiers::SHIFT);
    // `CSI 1 ; n X` carries the modifiers of a cursor key.
    let mods = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let cursor = |letter: char| {
        if mods == 1 {
            format!("\x1b[{letter}")
        } else {
            format!("\x1b[1;{mods}{letter}")
        }
    };
    let tilde = |code: u8| {
        if mods == 1 {
            format!("\x1b[{code}~")
        } else {
            format!("\x1b[{code};{mods}~")
        }
    };
    Some(match key.code {
        // Alt+Enter is what Claude Code takes as a line break.
        KeyCode::Enter if shift || alt => "\x1b\r".to_string(),
        KeyCode::Enter => "\r".to_string(),
        KeyCode::Tab => "\t".to_string(),
        KeyCode::BackTab => "\x1b[Z".to_string(),
        KeyCode::Backspace => "\x7f".to_string(),
        KeyCode::Esc => "\x1b".to_string(),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char),
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
        KeyCode::Char(c) if ctrl => {
            let byte = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                ' ' | '@' => 0,
                '[' => 0x1b,
                '\\' => 0x1c,
                ']' => 0x1d,
                '^' => 0x1e,
                '_' => 0x1f,
                _ => return None,
            };
            (byte as char).to_string()
        }
        KeyCode::Char(c) if alt => format!("\x1b{c}"),
        KeyCode::Char(c) => c.to_string(),
        _ => return None,
    })
}

// ---- actions shared by the dashboard and the inspector ---------------------

fn open_compose(app: &mut App, to: Option<String>) {
    let to = to
        .or_else(|| app.selected.clone())
        .unwrap_or_else(|| BROADCAST.to_string());
    app.modal = Some(Modal::Compose(Compose {
        to,
        ..Compose::default()
    }));
}

/// The message box as a command line: it opens with the slash typed.
fn open_command(app: &mut App) {
    open_compose(app, None);
    if let Some(Modal::Compose(compose)) = &mut app.modal {
        compose.text = "/".to_string();
        compose.cursor = 1;
    }
}

/// Open the answer box on the oldest question waiting for the operator.
fn open_questions(app: &mut App, now: i64) {
    match app.open_questions().first().map(|q| q.id.clone()) {
        Some(id) => app.modal = Some(Modal::Question(question_box(id))),
        None => app.set_status("no question is waiting for you", false, now),
    }
}

fn question_box(id: String) -> QuestionBox {
    QuestionBox {
        id,
        ..QuestionBox::default()
    }
}

/// Tell every agent paused at a usage limit to continue.
fn resume_paused(app: &mut App, now: i64) -> Vec<Command> {
    if app.agents.iter().any(|a| a.limit.is_some()) {
        vec![Command::ResumeLimited { restart: false }]
    } else {
        app.set_status("no agent is paused at a usage limit", true, now);
        Vec::new()
    }
}

fn toggle_pause(app: &mut App, now: i64) -> Vec<Command> {
    match app.selected_agent() {
        Some(agent) if agent.paused => vec![Command::Resume(agent.id.clone())],
        Some(agent) => vec![Command::Pause(agent.id.clone())],
        None => {
            app.set_status("no agent selected", true, now);
            Vec::new()
        }
    }
}

fn restart(app: &mut App, now: i64) -> Vec<Command> {
    match app.selected.clone() {
        Some(id) => vec![Command::Restart(id)],
        None => {
            app.set_status("no agent selected", true, now);
            Vec::new()
        }
    }
}

fn confirm_kill(app: &mut App, now: i64) {
    match app.selected.clone() {
        Some(id) => app.modal = Some(Modal::ConfirmKill(id)),
        None => app.set_status("no agent selected", true, now),
    }
}

fn focus_pane(app: &mut App, now: i64) -> Vec<Command> {
    let Some(id) = app.selected.clone() else {
        app.set_status("no agent selected", true, now);
        return Vec::new();
    };
    if app.session.backend != "herdr" {
        app.set_status(
            "g jumps to a herdr pane; this session uses the pty backend (try `crew agent attach`)",
            true,
            now,
        );
        return Vec::new();
    }
    vec![Command::Focus(id)]
}

/// Keys that mean the same thing on the dashboard and in the inspector.
fn common_key(app: &mut App, key: KeyEvent, now: i64) -> Option<Vec<Command>> {
    let none = Some(Vec::new());
    match key.code {
        KeyCode::Char('q') => Some(vec![Command::Quit]),
        KeyCode::Char('?') => {
            app.help = true;
            none
        }
        KeyCode::Char('m') => {
            open_compose(app, None);
            none
        }
        KeyCode::Char('M') => {
            open_compose(app, Some(BROADCAST.to_string()));
            none
        }
        KeyCode::Char('/') => {
            open_command(app);
            none
        }
        KeyCode::Char('S') => {
            app.modal = Some(Modal::ConfirmStop);
            none
        }
        KeyCode::Char('a') => {
            open_questions(app, now);
            none
        }
        KeyCode::Char('t') => {
            app.view = View::Tasks;
            none
        }
        KeyCode::Char('l') => {
            app.view = View::Logs;
            none
        }
        KeyCode::Char('p') => Some(toggle_pause(app, now)),
        KeyCode::Char('k') => {
            confirm_kill(app, now);
            none
        }
        KeyCode::Char('g') => Some(focus_pane(app, now)),
        // What the orchestrator runs on is the operator's to choose.
        KeyCode::Char('O') => Some(match app.orchestrator().map(|a| a.id.clone()) {
            Some(id) => app.open_model_picker(&id, now),
            None => {
                app.set_status("this team runs without an orchestrator agent", true, now);
                Vec::new()
            }
        }),
        KeyCode::Char('s') => {
            let id = if app.view == View::Detail {
                Some(app.detail.agent.clone())
            } else {
                app.selected
                    .clone()
                    .or_else(|| app.orchestrator().map(|a| a.id.clone()))
            };
            Some(
                id.map(|id| app.open_model_picker(&id, now))
                    .unwrap_or_default(),
            )
        }
        KeyCode::Char('u') => Some(resume_paused(app, now)),
        KeyCode::Char('U') => {
            match app.agents.iter().filter(|a| a.limit.is_some()).count() {
                0 => app.set_status("no agent is paused at a usage limit", true, now),
                paused => app.modal = Some(Modal::ConfirmRestartPaused(paused)),
            }
            none
        }
        _ => None,
    }
}

// ---- views ------------------------------------------------------------------

fn dashboard_key(app: &mut App, key: KeyEvent, now: i64) -> Vec<Command> {
    match key.code {
        KeyCode::Tab | KeyCode::Right | KeyCode::Down => app.select_step(1),
        KeyCode::BackTab | KeyCode::Left | KeyCode::Up => app.select_step(-1),
        KeyCode::Enter => {
            if let Some(id) = app.selected.clone() {
                return app.open_detail(&id, now);
            }
        }
        KeyCode::Char('o') => {
            if !app.select_orchestrator() {
                app.set_status("this team runs without an orchestrator agent", true, now);
            }
        }
        KeyCode::Char('r') => return restart(app, now),
        KeyCode::Esc => app.status = None,
        _ => return common_key(app, key, now).unwrap_or_default(),
    }
    Vec::new()
}

fn detail_key(app: &mut App, key: KeyEvent, now: i64, page: usize) -> Vec<Command> {
    match key.code {
        KeyCode::Esc => app.view = View::Dashboard,
        KeyCode::Tab | KeyCode::BackTab => {
            app.select_step(if key.code == KeyCode::Tab { 1 } else { -1 });
            if let Some(id) = app.selected.clone() {
                return app.open_detail(&id, now);
            }
        }
        KeyCode::Char('o') => {
            if app.select_orchestrator() {
                if let Some(id) = app.selected.clone() {
                    return app.open_detail(&id, now);
                }
            }
        }
        KeyCode::Char('r') => {
            let id = app.detail.agent.clone();
            app.detail.pending = true;
            app.detail.requested_at = now;
            // Scrolled up, the transcript is read again too.
            app.detail.transcript_at = None;
            app.detail.transcript_error = None;
            let mut cmds = vec![
                Command::FetchOutput { id: id.clone() },
                Command::FetchInbox { id: id.clone() },
            ];
            if app.detail.show_diff {
                cmds.push(Command::FetchDiff { id });
            }
            return cmds;
        }
        KeyCode::Char('R') => return restart(app, now),
        KeyCode::Char('f') => app.detail.full = !app.detail.full,
        KeyCode::Char('i') => {
            app.modal = Some(Modal::Type(Compose {
                to: app.detail.agent.clone(),
                ..Compose::default()
            }));
        }
        KeyCode::Char('d') => {
            app.detail.show_diff = !app.detail.show_diff;
            app.detail.scroll = 0;
            if app.detail.show_diff {
                app.detail.diff = None;
                return vec![Command::FetchDiff {
                    id: app.detail.agent.clone(),
                }];
            }
        }
        KeyCode::Up => app.detail.scroll = app.detail.scroll.saturating_add(1),
        KeyCode::Down => app.detail.scroll = app.detail.scroll.saturating_sub(1),
        KeyCode::PageUp => app.detail.scroll = app.detail.scroll.saturating_add(page.max(1)),
        KeyCode::PageDown => app.detail.scroll = app.detail.scroll.saturating_sub(page.max(1)),
        KeyCode::Home => app.detail.scroll = output_lines(app),
        KeyCode::End => app.detail.scroll = 0,
        _ => return common_key(app, key, now).unwrap_or_default(),
    }
    // Scrolling past the top is clamped when rendering; keep the stored value sane.
    app.detail.scroll = app.detail.scroll.min(output_lines(app));
    Vec::new()
}

fn output_lines(app: &App) -> usize {
    let detail = &app.detail;
    if detail.show_diff {
        return detail.diff.as_deref().unwrap_or("").lines().count();
    }
    // Scrolled up, the pane shows the transcript when there is one.
    match detail.transcript_lines(detail.cols.get()) {
        Some(lines) => lines.len(),
        None => detail.output.lines().count(),
    }
}

fn tasks_key(app: &mut App, key: KeyEvent, now: i64, page: usize) -> Vec<Command> {
    let max = app.tasks.len() * 2 + 16;
    match key.code {
        KeyCode::Char('q') => return vec![Command::Quit],
        KeyCode::Char('?') => app.help = true,
        KeyCode::Esc | KeyCode::Char('t') => app.view = View::Dashboard,
        KeyCode::Char('l') => app.view = View::Logs,
        KeyCode::Char('/') => open_command(app),
        KeyCode::Char('a') => open_questions(app, now),
        KeyCode::Up | KeyCode::Char('k') => app.tasks_scroll = app.tasks_scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => app.tasks_scroll = (app.tasks_scroll + 1).min(max),
        KeyCode::PageUp => app.tasks_scroll = app.tasks_scroll.saturating_sub(page.max(1)),
        KeyCode::PageDown => app.tasks_scroll = (app.tasks_scroll + page.max(1)).min(max),
        KeyCode::Home => app.tasks_scroll = 0,
        KeyCode::End => app.tasks_scroll = max,
        _ => {}
    }
    Vec::new()
}

fn logs_key(app: &mut App, key: KeyEvent, now: i64, page: usize) -> Vec<Command> {
    let page = page.max(1);
    let max = logs::visible_events(app).len().saturating_sub(page);
    let scroll = app.logs.scroll.min(max);
    match key.code {
        KeyCode::Char('q') => return vec![Command::Quit],
        KeyCode::Char('?') => app.help = true,
        KeyCode::Esc | KeyCode::Char('l') => app.view = View::Dashboard,
        KeyCode::Char('t') => app.view = View::Tasks,
        KeyCode::Char('/') => open_command(app),
        KeyCode::Char('a') => open_questions(app, now),
        KeyCode::Char('f') => {
            app.logs.filter_agent = match app.logs.filter_agent {
                Some(_) => None,
                None => app.selected.clone(),
            };
            app.logs.scroll = 0;
        }
        KeyCode::Up | KeyCode::Char('k') => app.logs.scroll = (scroll + 1).min(max),
        KeyCode::Down | KeyCode::Char('j') => app.logs.scroll = scroll.saturating_sub(1),
        KeyCode::PageUp => app.logs.scroll = (scroll + page).min(max),
        KeyCode::PageDown => app.logs.scroll = scroll.saturating_sub(page),
        KeyCode::Home => app.logs.scroll = max,
        KeyCode::End => app.logs.scroll = 0,
        _ => {}
    }
    Vec::new()
}

// ---- modals -----------------------------------------------------------------

fn modal_key(app: &mut App, key: KeyEvent, now: i64) -> Vec<Command> {
    let Some(modal) = app.modal.take() else {
        return Vec::new();
    };
    match modal {
        Modal::ConfirmKill(id) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => vec![Command::Kill(id)],
            _ => Vec::new(),
        },
        Modal::ConfirmRestartPaused(_) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                vec![Command::ResumeLimited { restart: true }]
            }
            _ => Vec::new(),
        },
        Modal::Question(answer) => question_key(app, answer, key, now),
        Modal::Model(picker) => picker_key(app, picker, key, now),
        Modal::Message(mut view) => {
            let lines = message::scroll_limit(app, &view, app.screen.get());
            view.scroll = view.scroll.min(lines);
            match key.code {
                KeyCode::Up => scroll_message(&mut view, true, 1, lines),
                KeyCode::Down => scroll_message(&mut view, false, 1, lines),
                KeyCode::PageUp => scroll_message(&mut view, true, 10, lines),
                KeyCode::PageDown => scroll_message(&mut view, false, 10, lines),
                KeyCode::Home => view.scroll = 0,
                KeyCode::End => view.scroll = lines,
                _ => return Vec::new(),
            }
            app.modal = Some(Modal::Message(view));
            Vec::new()
        }
        Modal::ConfirmStop => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.stopping = true;
                vec![Command::StopTeam]
            }
            _ => Vec::new(),
        },
        Modal::Type(mut compose) => {
            match key.code {
                KeyCode::Esc => return Vec::new(),
                KeyCode::Enter if !newline_key(key) => {
                    // The screen is read again right after.
                    app.detail.output_at = None;
                    return vec![Command::Type {
                        id: compose.to,
                        text: compose.text,
                    }];
                }
                _ => edit(&mut compose, key),
            }
            app.modal = Some(Modal::Type(compose));
            Vec::new()
        }
        Modal::Compose(mut compose) => {
            match key.code {
                KeyCode::Esc => return Vec::new(),
                KeyCode::Enter if !newline_key(key) => {
                    // A line that opens with a slash is a command for crew and
                    // is never sent to an agent, whether or not it exists.
                    if let Some((name, rest)) = slash_command(&compose.text) {
                        match run_slash(app, name, rest, &compose.to, now) {
                            Ok(commands) => return commands,
                            Err(notice) => compose.notice = Some(notice),
                        }
                    } else {
                        let body = compose.text.trim().to_string();
                        if body.is_empty() {
                            return Vec::new();
                        }
                        return vec![Command::Send {
                            to: compose.to,
                            body,
                        }];
                    }
                }
                KeyCode::Tab | KeyCode::BackTab if compose.text.starts_with('/') => {
                    if let Some(completed) = slash::complete(slash::DASHBOARD, &compose.text) {
                        compose.cursor = completed.chars().count();
                        compose.text = completed;
                    }
                    compose.notice = None;
                }
                KeyCode::Tab => compose.to = next_recipient(app, &compose.to, 1),
                KeyCode::BackTab => compose.to = next_recipient(app, &compose.to, -1),
                _ => {
                    edit(&mut compose, key);
                    compose.notice = None;
                }
            }
            app.modal = Some(Modal::Compose(compose));
            Vec::new()
        }
    }
}

/// Move a message view by `rows`, within its last visible-page offset.
fn scroll_message(view: &mut MessageView, up: bool, rows: usize, lines: usize) {
    view.scroll = if up {
        view.scroll.min(lines).saturating_sub(rows)
    } else {
        view.scroll.min(lines).saturating_add(rows).min(lines)
    };
}

/// A key pressed in the model picker: ←/→ pick the CLI, ↑/↓ the model,
/// Enter puts the agent on it.
fn picker_key(app: &mut App, mut picker: ModelPicker, key: KeyEvent, now: i64) -> Vec<Command> {
    let rows = app.model_rows(picker.runtime).len();
    let last = rows.saturating_sub(1);
    let step_cli = |picker: &mut ModelPicker, app: &App, delta: i32| {
        let at = HARNESSES
            .iter()
            .position(|r| *r == picker.runtime)
            .unwrap_or(0) as i32;
        picker.runtime = HARNESSES[(at + delta).rem_euclid(HARNESSES.len() as i32) as usize];
        picker.row = app.start_row(&picker.agent, picker.runtime);
        picker.moved = true;
    };
    picker.notice = None;
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
        KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => step_cli(&mut picker, app, -1),
        KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => step_cli(&mut picker, app, 1),
        KeyCode::Char(c @ '1'..='3') => {
            picker.runtime = HARNESSES[usize::from(c as u8 - b'1')];
            picker.row = app.start_row(&picker.agent, picker.runtime);
            picker.moved = true;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            picker.row = picker.row.saturating_sub(1);
            picker.moved = true;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            picker.row = (picker.row + 1).min(last);
            picker.moved = true;
        }
        KeyCode::PageUp => picker.row = picker.row.saturating_sub(8),
        KeyCode::PageDown => picker.row = (picker.row + 8).min(last),
        KeyCode::Home => picker.row = 0,
        KeyCode::End => picker.row = last,
        KeyCode::Enter => return choose_model(app, picker, now),
        _ => {}
    }
    app.modal = Some(Modal::Model(picker));
    Vec::new()
}

/// Enter in the model picker: put the agent on the highlighted model, or
/// say why not and leave the picker open.
fn choose_model(app: &mut App, mut picker: ModelPicker, now: i64) -> Vec<Command> {
    let Some(agent) = app.agent(&picker.agent).cloned() else {
        app.set_status(
            format!("{} is no longer on the team", picker.agent),
            true,
            now,
        );
        return Vec::new();
    };
    let Some(choice) = app
        .model_rows(picker.runtime)
        .get(picker.row)
        .map(|c| (*c).clone())
    else {
        picker.notice = Some(if app.models_pending {
            "the models are still being listed".to_string()
        } else {
            "no model to choose here".to_string()
        });
        app.modal = Some(Modal::Model(picker));
        return Vec::new();
    };
    if let Some(why) = &choice.unavailable {
        picker.notice = Some(format!("{} cannot be started here: {why}", choice.name));
        app.modal = Some(Modal::Model(picker));
        return Vec::new();
    }
    if app.current_row(&agent.id, picker.runtime) == Some(picker.row) {
        app.set_status(
            format!("{} already runs on {}", agent.id, choice.name),
            false,
            now,
        );
        return Vec::new();
    }
    let cli = crate::theme::runtime_label(choice.runtime);
    let label = if choice.runtime == agent.runtime {
        format!(
            "restarting {} on {}, resuming its session",
            agent.id, choice.name
        )
    } else {
        format!(
            "moving {} to {cli} on {}: a fresh session there, handed the work",
            agent.id, choice.name
        )
    };
    app.set_status(format!("{label}…"), false, now);
    vec![Command::SetModel {
        id: agent.id,
        model: choice.model,
        runtime: choice.runtime,
        label,
    }]
}

/// A key pressed in the answer box of a question.
fn question_key(app: &mut App, mut answer: QuestionBox, key: KeyEvent, now: i64) -> Vec<Command> {
    let open: Vec<(String, Vec<String>, usize)> = app
        .open_questions()
        .iter()
        .map(|q| {
            let options = q.options();
            let limit = views::question::scroll_limit(&q.body, options.len(), app.screen.get());
            (q.id.clone(), options, limit)
        })
        .collect();
    let Some(index) = open.iter().position(|(id, _, _)| *id == answer.id) else {
        // Answered from somewhere else meanwhile: on to the next, if any.
        if let Some((id, _, _)) = open.first() {
            app.modal = Some(Modal::Question(question_box(id.clone())));
        }
        return Vec::new();
    };
    let (_, options, length) = &open[index];
    answer.scroll = answer.scroll.min(*length);
    // The row after the suggested answers takes the operator's own words.
    let own = options.len();
    answer.choice = answer.choice.min(own);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => return Vec::new(),
        KeyCode::Tab | KeyCode::BackTab => {
            let step = if key.code == KeyCode::Tab { 1 } else { -1 };
            let next = (index as i32 + step).rem_euclid(open.len() as i32) as usize;
            if next != index {
                answer = question_box(open[next].0.clone());
            }
        }
        KeyCode::Up => answer.choice = answer.choice.saturating_sub(1),
        KeyCode::Down => answer.choice = (answer.choice + 1).min(own),
        KeyCode::PageUp => answer.scroll = answer.scroll.saturating_sub(5),
        KeyCode::PageDown => answer.scroll = (answer.scroll + 5).min(*length),
        // The box opened by itself a moment ago: this Enter was meant for
        // what was on screen before, not as an answer.
        KeyCode::Enter
            if answer.popped_at > 0 && now - answer.popped_at < crate::app::POPUP_GRACE_MS => {}
        KeyCode::Enter if !newline_key(key) => {
            let typed = answer.choice == own;
            let body = if typed {
                answer.input.text.trim().to_string()
            } else {
                options[answer.choice].clone()
            };
            if body.is_empty() {
                answer.input.notice = Some("pick an answer with ↑↓, or type your own".to_string());
            } else if typed && slash_command(&answer.input.text).is_some() {
                answer.input.notice = Some(
                    "that is a command, not an answer — esc closes this box first".to_string(),
                );
            } else {
                app.answering.insert(answer.id.clone());
                // Straight on to the next question, when there is one.
                if let Some((id, _, _)) = open.iter().find(|(id, _, _)| *id != answer.id) {
                    app.modal = Some(Modal::Question(question_box(id.clone())));
                }
                return vec![Command::Answer {
                    id: answer.id,
                    body,
                }];
            }
        }
        // On a suggested answer a digit picks a row; anything else typed
        // starts the operator's own answer.
        KeyCode::Char(ch) if answer.choice < own && !ctrl => {
            match ch.to_digit(10).map(|d| d as usize) {
                Some(row) if (1..=own + 1).contains(&row) => answer.choice = row - 1,
                _ => {
                    answer.choice = own;
                    edit(&mut answer.input, key);
                }
            }
            answer.input.notice = None;
        }
        _ if answer.choice == own => {
            edit(&mut answer.input, key);
            answer.input.notice = None;
        }
        _ => {}
    }
    app.modal = Some(Modal::Question(answer));
    Vec::new()
}

/// Run a slash command typed into the message box, which was addressed to
/// `to`. `Err` is why nothing was run; the box then stays open to say so.
fn run_slash(
    app: &mut App,
    name: &str,
    rest: &str,
    to: &str,
    now: i64,
) -> Result<Vec<Command>, String> {
    let command = slash::find(slash::DASHBOARD, name).ok_or_else(|| slash::unknown(name))?;
    if command.args.is_empty() && !rest.is_empty() {
        return Err(slash::no_argument(command));
    }
    match command.name {
        "exit" => return Ok(vec![Command::Quit]),
        "stop" => app.modal = Some(Modal::ConfirmStop),
        "kill" => app.modal = Some(Modal::ConfirmKill(slash_agent(app, command, rest, to)?)),
        "restart" => return Ok(vec![Command::Restart(slash_agent(app, command, rest, to)?)]),
        "pause" => return Ok(vec![Command::Pause(slash_agent(app, command, rest, to)?)]),
        "unpause" => return Ok(vec![Command::Resume(slash_agent(app, command, rest, to)?)]),
        "compact" => return Ok(vec![Command::Compact(slash_agent(app, command, rest, to)?)]),
        "model" => {
            // Nobody named and nobody selected: the orchestrator.
            let id = match slash_agent(app, command, rest, to) {
                Ok(id) => id,
                Err(e) if rest.is_empty() => app.orchestrator().map(|a| a.id.clone()).ok_or(e)?,
                Err(e) => return Err(e),
            };
            return Ok(app.open_model_picker(&id, now));
        }
        "resume" => return Ok(resume_paused(app, now)),
        "answer" => open_questions(app, now),
        "tasks" => app.view = View::Tasks,
        "logs" => app.view = View::Logs,
        "mouse" => {
            let on = !app.mouse;
            let note = if on {
                "mouse on: click an agent or a message, wheel scrolls (shift-drag selects text)"
            } else {
                "mouse off: drag selects text as usual; /mouse turns it back on"
            };
            app.set_status(note, false, now);
            return Ok(vec![Command::SetMouse(on)]);
        }
        "help" => app.help = true,
        other => return Err(slash::unknown(other)),
    }
    Ok(Vec::new())
}

/// The agent a command is about: the one named, else the one the message
/// box was addressed to, else the selected one.
fn slash_agent(app: &App, command: &SlashCommand, rest: &str, to: &str) -> Result<String, String> {
    if rest.is_empty() {
        return Some(to)
            .filter(|to| *to != BROADCAST)
            .map(str::to_string)
            .or_else(|| app.selected.clone())
            .ok_or_else(|| format!("name the agent: {}", command.usage()));
    }
    if app.agents.iter().any(|a| a.id == rest) {
        return Ok(rest.to_string());
    }
    let ids: Vec<&str> = app.agents.iter().map(|a| a.id.as_str()).collect();
    Err(format!(
        "no agent named {rest} (there are: {})",
        ids.join(", ")
    ))
}

/// Cycle through every agent, then the broadcast address.
fn next_recipient(app: &App, current: &str, delta: i32) -> String {
    let mut all: Vec<&str> = app.agents.iter().map(|a| a.id.as_str()).collect();
    all.push(BROADCAST);
    let len = all.len() as i32;
    let index = all
        .iter()
        .position(|id| *id == current)
        .map_or(0, |i| i as i32);
    all[(index + delta).rem_euclid(len) as usize].to_string()
}

/// Shift+Enter starts a new line in a box. A terminal without the kitty
/// keyboard protocol cannot tell it from Enter; there Alt+Enter and Ctrl+J
/// do the same.
fn newline_key(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT),
        KeyCode::Char('j') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

fn byte_at(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(i, _)| i)
}

fn insert(compose: &mut Compose, ch: char) {
    if compose.text.chars().count() >= MAX_COMPOSE_CHARS {
        return;
    }
    let at = byte_at(&compose.text, compose.cursor);
    compose.text.insert(at, ch);
    compose.cursor += 1;
}

fn edit(compose: &mut Compose, key: KeyEvent) {
    let len = compose.text.chars().count();
    compose.cursor = compose.cursor.min(len);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('u') if ctrl => {
            let at = byte_at(&compose.text, compose.cursor);
            compose.text.drain(..at);
            compose.cursor = 0;
        }
        KeyCode::Char('w') if ctrl => {
            // Delete the word before the cursor.
            let chars: Vec<char> = compose.text.chars().collect();
            let mut start = compose.cursor;
            while start > 0 && chars[start - 1].is_whitespace() {
                start -= 1;
            }
            while start > 0 && !chars[start - 1].is_whitespace() {
                start -= 1;
            }
            let (from, to) = (
                byte_at(&compose.text, start),
                byte_at(&compose.text, compose.cursor),
            );
            compose.text.drain(from..to);
            compose.cursor = start;
        }
        KeyCode::Char('a') if ctrl => compose.cursor = 0,
        KeyCode::Char('e') if ctrl => compose.cursor = len,
        // Enter reaches here only as a line break (its other meaning is the
        // box's to give).
        KeyCode::Enter => insert(compose, '\n'),
        KeyCode::Char('j') if ctrl => insert(compose, '\n'),
        KeyCode::Char(ch) if !ctrl => insert(compose, ch),
        KeyCode::Backspace if compose.cursor > 0 => {
            let from = byte_at(&compose.text, compose.cursor - 1);
            let to = byte_at(&compose.text, compose.cursor);
            compose.text.drain(from..to);
            compose.cursor -= 1;
        }
        KeyCode::Delete if compose.cursor < len => {
            let from = byte_at(&compose.text, compose.cursor);
            let to = byte_at(&compose.text, compose.cursor + 1);
            compose.text.drain(from..to);
        }
        KeyCode::Left => compose.cursor = compose.cursor.saturating_sub(1),
        KeyCode::Right => compose.cursor = (compose.cursor + 1).min(len),
        KeyCode::Home => compose.cursor = 0,
        KeyCode::End => compose.cursor = len,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_paste_cannot_inject_bracketed_paste_markers() {
        let mut a = app();
        a.view = View::Detail;
        a.detail.agent = "claude-1".into();
        a.detail.typing = true;
        let commands = handle_paste(
            &mut a,
            "日本\ntext\u{1b}[201~\ncommand\u{1b}[200~",
            1_000_000,
        );
        let [Command::Keys { text, .. }] = commands.as_slice() else {
            panic!("expected terminal paste")
        };
        assert_eq!(
            text.matches('\u{1b}').count(),
            2,
            "only the outer markers may contain ESC"
        );
        assert_eq!(text.matches("\u{1b}[201~").count(), 1);
        assert!(text.starts_with("\u{1b}[200~日本\ntext"));
        assert!(text.ends_with("command[200~\u{1b}[201~"));
    }
    use crate::fixture;

    fn app() -> App {
        let mut app = App::new();
        app.apply_snapshot(fixture::snapshot(1_000_000), 1_000_000);
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Vec<Command> {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE), 1_000_000, 10)
    }

    fn ctrl(app: &mut App, ch: char) -> Vec<Command> {
        handle_key(
            app,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL),
            1_000_000,
            10,
        )
    }

    fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            press(app, KeyCode::Char(ch));
        }
    }

    #[test]
    fn quit_never_asks_the_daemon_for_anything_else() {
        let mut a = app();
        assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Command::Quit]);
        assert_eq!(ctrl(&mut a, 'c'), vec![Command::Quit]);
        for view in [View::Detail, View::Tasks, View::Logs] {
            a.view = view;
            assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Command::Quit]);
        }
    }

    #[test]
    fn tab_cycles_and_enter_inspects() {
        let mut a = app();
        press(&mut a, KeyCode::Tab);
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.selected.as_deref(), Some("claude-3"));
        press(&mut a, KeyCode::BackTab);
        assert_eq!(a.selected.as_deref(), Some("claude-2"));
        let cmds = press(&mut a, KeyCode::Enter);
        assert_eq!(a.view, View::Detail);
        assert_eq!(a.detail.agent, "claude-2");
        assert!(cmds.contains(&Command::FetchOutput {
            id: "claude-2".into()
        }));
        assert!(cmds.contains(&Command::FetchInbox {
            id: "claude-2".into()
        }));
        // Tab inside the inspector moves to the next agent and refetches.
        let cmds = press(&mut a, KeyCode::Tab);
        assert_eq!(a.detail.agent, "claude-3");
        assert!(cmds.contains(&Command::FetchOutput {
            id: "claude-3".into()
        }));
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.view, View::Dashboard);
        press(&mut a, KeyCode::Char('o'));
        assert_eq!(a.selected.as_deref(), Some("orchestrator"));
    }

    #[test]
    fn the_inspector_types_into_the_agents_terminal() {
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.view, View::Detail);
        press(&mut a, KeyCode::Char('i'));
        assert!(
            matches!(&a.modal, Some(Modal::Type(c)) if c.to == "claude-1" && c.text.is_empty())
        );
        // Shortcuts are text here; the line goes in as one prompt.
        type_text(&mut a, "run the tests, k?");
        a.detail.output_at = Some(5);
        let cmds = press(&mut a, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Command::Type {
                id: "claude-1".into(),
                text: "run the tests, k?".into()
            }]
        );
        assert!(a.modal.is_none());
        // The screen is read again at once.
        assert!(a.detail.output_at.is_none());

        // Nothing typed: Enter alone goes to the terminal.
        press(&mut a, KeyCode::Char('i'));
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Type {
                id: "claude-1".into(),
                text: String::new()
            }]
        );
        // Esc sends nothing; a paste is text.
        press(&mut a, KeyCode::Char('i'));
        type_text(&mut a, "abc");
        handle_paste(&mut a, "def", 1_000_000);
        assert!(matches!(&a.modal, Some(Modal::Type(c)) if c.text == "abcdef"));
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.modal.is_none());
        assert_eq!(a.view, View::Detail);

        // Shift+Enter is a new line, as are Alt+Enter and Ctrl+J where the
        // terminal cannot report the shift; plain Enter still sends.
        let with = |a: &mut App, code: KeyCode, modifiers: KeyModifiers| {
            handle_key(a, KeyEvent::new(code, modifiers), 1_000_000, 10)
        };
        press(&mut a, KeyCode::Char('i'));
        type_text(&mut a, "one");
        assert!(with(&mut a, KeyCode::Enter, KeyModifiers::SHIFT).is_empty());
        type_text(&mut a, "two");
        assert!(with(&mut a, KeyCode::Enter, KeyModifiers::ALT).is_empty());
        type_text(&mut a, "three");
        assert!(ctrl(&mut a, 'j').is_empty());
        assert!(matches!(&a.modal, Some(Modal::Type(c)) if c.text == "one\ntwo\nthree\n"));
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Type {
                id: "claude-1".into(),
                text: "one\ntwo\nthree\n".into()
            }]
        );
        // The same in a message, and in an answer of the operator's own.
        press(&mut a, KeyCode::Char('m'));
        type_text(&mut a, "a");
        with(&mut a, KeyCode::Enter, KeyModifiers::SHIFT);
        type_text(&mut a, "b");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Send {
                to: "claude-1".into(),
                body: "a\nb".into()
            }]
        );
        a.questions = vec![fixture::question(1_000_000)];
        a.view = View::Dashboard;
        press(&mut a, KeyCode::Char('a'));
        type_text(&mut a, "x");
        with(&mut a, KeyCode::Enter, KeyModifiers::SHIFT);
        type_text(&mut a, "y");
        assert!(matches!(&a.modal, Some(Modal::Question(q)) if q.input.text == "x\ny"));
        let sent = press(&mut a, KeyCode::Enter);
        assert!(
            matches!(&sent[..], [Command::Answer { body, .. }] if body == "x\ny"),
            "{sent:?}"
        );
    }

    #[test]
    fn a_click_into_the_terminal_pane_types_straight_into_it() {
        use ratatui::layout::Rect;

        let mut a = app();
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.view, View::Detail);
        // Where the pane was drawn is what a click is measured against.
        a.detail.terminal_rect.set(Rect::new(50, 2, 60, 20));
        assert!(left_click(&mut a, 20, 10).is_empty() && !a.detail.typing);
        a.detail.scroll = 4;
        assert!(left_click(&mut a, 60, 10).is_empty());
        assert!(a.detail.typing && a.detail.scroll == 0);

        // Every key is the agent's now, in the terminal's own encoding; the
        // screen is read again at once each time.
        let keys = |a: &mut App, code: KeyCode, modifiers: KeyModifiers| {
            a.detail.output_at = Some(1);
            let cmds = handle_key(a, KeyEvent::new(code, modifiers), 1_000_000, 10);
            assert!(a.detail.output_at.is_none(), "{cmds:?}");
            match cmds.as_slice() {
                [Command::Keys { id, text }] if id == "claude-1" => text.clone(),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(keys(&mut a, KeyCode::Char('q'), KeyModifiers::NONE), "q");
        assert_eq!(
            keys(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL),
            "\x03"
        );
        assert_eq!(keys(&mut a, KeyCode::Esc, KeyModifiers::NONE), "\x1b");
        assert_eq!(keys(&mut a, KeyCode::Enter, KeyModifiers::NONE), "\r");
        assert_eq!(keys(&mut a, KeyCode::Enter, KeyModifiers::SHIFT), "\x1b\r");
        assert_eq!(keys(&mut a, KeyCode::Up, KeyModifiers::NONE), "\x1b[A");
        assert_eq!(
            keys(&mut a, KeyCode::Left, KeyModifiers::SHIFT),
            "\x1b[1;2D"
        );
        assert_eq!(
            keys(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT),
            "\x1b[Z"
        );
        assert_eq!(keys(&mut a, KeyCode::Backspace, KeyModifiers::NONE), "\x7f");
        assert_eq!(keys(&mut a, KeyCode::Delete, KeyModifiers::NONE), "\x1b[3~");
        assert_eq!(
            keys(&mut a, KeyCode::PageDown, KeyModifiers::NONE),
            "\x1b[6~"
        );
        assert_eq!(keys(&mut a, KeyCode::F(1), KeyModifiers::NONE), "\x1bOP");
        assert_eq!(keys(&mut a, KeyCode::F(5), KeyModifiers::NONE), "\x1b[15~");
        assert_eq!(keys(&mut a, KeyCode::Char('x'), KeyModifiers::ALT), "\x1bx");
        assert_eq!(keys(&mut a, KeyCode::Char('É'), KeyModifiers::SHIFT), "É");
        assert!(handle_key(
            &mut a,
            KeyEvent::new(KeyCode::CapsLock, KeyModifiers::NONE),
            1_000_000,
            10
        )
        .is_empty());
        assert!(a.view == View::Detail && a.modal.is_none() && a.detail.typing);
        // A paste arrives as one, bracketed.
        assert_eq!(
            handle_paste(&mut a, "two\nlines", 1_000_000),
            vec![Command::Keys {
                id: "claude-1".into(),
                text: "\x1b[200~two\nlines\x1b[201~".into()
            }]
        );
        // Right after a key the terminal is read as often as it answers;
        // later on, often.
        let fetches = |a: &mut App, at: i64| {
            a.detail.pending = false;
            a.detail.output_at = Some(at - 1);
            a.detail.requested_at = at - 1;
            a.due_commands(at)
                .iter()
                .any(|c| matches!(c, Command::FetchOutput { .. }))
        };
        assert!(fetches(&mut a, 1_000_000 + 1));
        assert!(!fetches(&mut a, 1_000_000 + crate::app::KEY_BURST_MS + 10));
        a.detail.pending = false;
        a.detail.output_at = Some(1_000_000 + crate::app::KEY_BURST_MS);
        a.detail.requested_at = 1_000_000 + crate::app::KEY_BURST_MS;
        let later = 1_000_000 + crate::app::KEY_BURST_MS + crate::app::TYPING_REFRESH_MS;
        assert!(a
            .due_commands(later)
            .iter()
            .any(|c| matches!(c, Command::FetchOutput { .. })));

        // Ctrl+] is the way out; so is a right-click, or a click elsewhere.
        assert!(ctrl(&mut a, ']').is_empty());
        assert!(!a.detail.typing);
        assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Command::Quit]);
        left_click(&mut a, 60, 10);
        assert!(a.detail.typing);
        mouse(&mut a, MouseEventKind::Down(MouseButton::Right), 60, 10);
        assert!(!a.detail.typing && a.view == View::Detail);
        left_click(&mut a, 60, 10);
        assert!(left_click(&mut a, 5, 5).is_empty());
        assert!(!a.detail.typing);
        // Tab is the agent's too while typing; another agent's inspector
        // starts without it.
        left_click(&mut a, 60, 10);
        assert!(
            matches!(press(&mut a, KeyCode::Tab).as_slice(), [Command::Keys { text, .. }] if text == "\t")
        );
        assert_eq!(a.detail.agent, "claude-1");
        a.open_detail("claude-2", 1_000_000);
        assert!(!a.detail.typing);

        // f fills the inspector with the terminal, and again not.
        assert!(!a.detail.full);
        press(&mut a, KeyCode::Char('f'));
        assert!(a.detail.full);
        press(&mut a, KeyCode::Char('f'));
        assert!(!a.detail.full);
    }

    #[test]
    fn compose_sends_as_typed_and_esc_cancels() {
        let mut a = app();
        press(&mut a, KeyCode::Char('m'));
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.to == "claude-1"));
        // Keys that are shortcuts elsewhere are plain text here.
        type_text(&mut a, "quick mkt");
        press(&mut a, KeyCode::Backspace);
        press(&mut a, KeyCode::Left);
        press(&mut a, KeyCode::Left);
        type_text(&mut a, "!");
        let cmds = press(&mut a, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Command::Send {
                to: "claude-1".into(),
                body: "quick !mk".into()
            }]
        );
        assert!(a.modal.is_none());

        press(&mut a, KeyCode::Char('M'));
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.to == "*"));
        type_text(&mut a, "hello");
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.modal.is_none());

        // Empty messages are not sent.
        press(&mut a, KeyCode::Char('m'));
        type_text(&mut a, "   ");
        assert!(press(&mut a, KeyCode::Enter).is_empty());
    }

    fn compose(app: &App) -> &Compose {
        match &app.modal {
            Some(Modal::Compose(compose)) => compose,
            other => panic!("expected the message box, found {other:?}"),
        }
    }

    /// Type `line` into the message box of the selected agent and press Enter.
    fn submit(app: &mut App, line: &str) -> Vec<Command> {
        press(app, KeyCode::Char('m'));
        type_text(app, line);
        press(app, KeyCode::Enter)
    }

    #[test]
    fn a_slash_line_is_a_command_and_is_never_sent() {
        // /exit leaves the dashboard; no agent gets to read it.
        let mut a = app();
        assert_eq!(submit(&mut a, "/exit"), vec![Command::Quit]);
        for alias in ["/quit", "/q", "/EXIT"] {
            assert_eq!(submit(&mut app(), alias), vec![Command::Quit], "{alias}");
        }

        // A command that does not exist is not sent either: the box stays
        // open and says so.
        let mut a = app();
        assert!(submit(&mut a, "/clear").is_empty());
        assert_eq!(compose(&a).text, "/clear");
        assert!(compose(&a)
            .notice
            .as_deref()
            .unwrap()
            .contains("no command /clear"));
        // Editing clears the note; an argument where none belongs is refused.
        press(&mut a, KeyCode::Backspace);
        assert!(compose(&a).notice.is_none());
        ctrl(&mut a, 'u');
        type_text(&mut a, "/exit now");
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert_eq!(
            compose(&a).notice.as_deref(),
            Some("/exit takes no argument")
        );
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.modal.is_none());

        // Text that merely contains or resembles a command is a message.
        for text in [" /exit", "/src/main.rs is slow", "please /exit", "//exit"] {
            let mut a = app();
            assert_eq!(
                submit(&mut a, text),
                vec![Command::Send {
                    to: "claude-1".into(),
                    body: text.trim().into()
                }],
                "{text:?}"
            );
        }
    }

    #[test]
    fn the_team_is_stopped_from_the_dashboard_after_a_confirmation() {
        let mut a = app();
        assert!(submit(&mut a, "/stop").is_empty());
        assert_eq!(a.modal, Some(Modal::ConfirmStop));
        assert!(press(&mut a, KeyCode::Char('n')).is_empty());
        assert!(a.modal.is_none() && !a.stopping);

        for open in ["/terminate", "/shutdown"] {
            let mut a = app();
            submit(&mut a, open);
            assert_eq!(a.modal, Some(Modal::ConfirmStop), "{open}");
        }
        // The key does the same from the dashboard and the inspector.
        for view in [View::Dashboard, View::Detail] {
            let mut a = app();
            a.view = view;
            assert!(press(&mut a, KeyCode::Char('S')).is_empty());
            assert_eq!(a.modal, Some(Modal::ConfirmStop));
            assert_eq!(press(&mut a, KeyCode::Char('y')), vec![Command::StopTeam]);
            assert!(a.stopping && a.modal.is_none());
        }
    }

    #[test]
    fn slash_commands_act_on_agents_and_views() {
        // Without a name, the agent is the one the box was addressed to.
        let mut a = app();
        assert!(submit(&mut a, "/kill").is_empty());
        assert_eq!(a.modal, Some(Modal::ConfirmKill("claude-1".into())));
        assert_eq!(
            press(&mut a, KeyCode::Char('y')),
            vec![Command::Kill("claude-1".into())]
        );
        assert!(submit(&mut a, "/kill claude-3").is_empty());
        assert_eq!(a.modal, Some(Modal::ConfirmKill("claude-3".into())));
        press(&mut a, KeyCode::Esc);

        assert_eq!(
            submit(&mut a, "/restart claude-2"),
            vec![Command::Restart("claude-2".into())]
        );
        assert_eq!(
            submit(&mut a, "/pause"),
            vec![Command::Pause("claude-1".into())]
        );
        assert_eq!(
            submit(&mut a, "/compact"),
            vec![Command::Compact("claude-1".into())]
        );
        assert_eq!(
            submit(&mut a, "/compact claude-3"),
            vec![Command::Compact("claude-3".into())]
        );
        assert_eq!(
            submit(&mut a, "/unpause claude-2"),
            vec![Command::Resume("claude-2".into())]
        );

        // An agent that does not exist is named in the note, with the real ones.
        assert!(submit(&mut a, "/kill ghost").is_empty());
        let notice = compose(&a).notice.clone().unwrap();
        assert!(
            notice.contains("no agent named ghost") && notice.contains("claude-2"),
            "{notice}"
        );
        press(&mut a, KeyCode::Esc);

        // A broadcast box has no agent of its own: the selection stands in.
        press(&mut a, KeyCode::Char('M'));
        type_text(&mut a, "/restart");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Restart("claude-1".into())]
        );

        assert!(
            submit(&mut a, "/resume").is_empty(),
            "nobody is at a usage limit"
        );
        assert!(a.status.as_ref().unwrap().is_error);
        a.agents[0].limit = Some(crew_core::types::UsageLimit::default());
        assert_eq!(
            submit(&mut a, "/resume"),
            vec![Command::ResumeLimited { restart: false }]
        );

        assert!(submit(&mut a, "/tasks").is_empty());
        assert_eq!(a.view, View::Tasks);
        // The other views open the command line with the slash key too.
        assert!(press(&mut a, KeyCode::Char('/')).is_empty());
        assert_eq!((compose(&a).text.as_str(), compose(&a).cursor), ("/", 1));
        type_text(&mut a, "logs");
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.view, View::Logs);
        press(&mut a, KeyCode::Char('/'));
        type_text(&mut a, "?");
        press(&mut a, KeyCode::Enter);
        assert!(a.help && a.modal.is_none());
    }

    fn answer_box(app: &App) -> &QuestionBox {
        match &app.modal {
            Some(Modal::Question(answer)) => answer,
            other => panic!("expected the answer box, found {other:?}"),
        }
    }

    fn with_questions() -> (App, String, String) {
        let mut a = app();
        let first = fixture::question(1_000_000);
        let mut second = first.clone();
        second.id = "m-second".into();
        second.body = "Ship without the migration?".into();
        second.metadata = serde_json::Value::Null;
        a.questions = vec![first.clone(), second.clone()];
        (a, first.id, second.id)
    }

    #[test]
    fn a_question_is_answered_by_picking_an_option_or_in_the_operators_words() {
        // Nothing to answer: say so instead of opening an empty box.
        let mut a = app();
        assert!(press(&mut a, KeyCode::Char('a')).is_empty());
        assert!(a.modal.is_none());
        assert!(a
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("no question is waiting"));

        // The oldest question opens with its first suggested answer selected.
        let (mut a, first, second) = with_questions();
        press(&mut a, KeyCode::Char('a'));
        assert_eq!(
            (answer_box(&a).id.as_str(), answer_box(&a).choice),
            (first.as_str(), 0)
        );
        press(&mut a, KeyCode::Down);
        assert_eq!(answer_box(&a).choice, 1);
        press(&mut a, KeyCode::Char('1'));
        assert_eq!(answer_box(&a).choice, 0, "a digit picks a row");
        press(&mut a, KeyCode::Char('2'));
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Answer {
                id: first.clone(),
                body: "Change the export and update the dashboards".into()
            }]
        );
        // The answered one is out of the way, and the next is up.
        assert!(a.answering.contains(&first));
        assert_eq!(a.open_questions().len(), 1);
        assert_eq!(answer_box(&a).id, second);

        // No suggestions: the box takes the operator's own words.
        assert!(
            press(&mut a, KeyCode::Enter).is_empty(),
            "an empty answer is not sent"
        );
        assert!(answer_box(&a)
            .input
            .notice
            .as_deref()
            .unwrap()
            .contains("type your own"));
        type_text(&mut a, "/exit");
        assert!(
            press(&mut a, KeyCode::Enter).is_empty(),
            "a command is not an answer"
        );
        assert!(answer_box(&a)
            .input
            .notice
            .as_deref()
            .unwrap()
            .contains("a command, not an answer"));
        ctrl(&mut a, 'u');
        assert!(answer_box(&a).input.notice.is_none());
        type_text(&mut a, "yes, ship it q");
        press(&mut a, KeyCode::Backspace);
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Answer {
                id: second,
                body: "yes, ship it".into()
            }]
        );
        assert!(a.modal.is_none(), "nothing is left to answer");
    }

    #[test]
    fn the_answer_box_moves_between_questions_and_keeps_typing_apart_from_keys() {
        let (mut a, first, second) = with_questions();
        assert!(submit(&mut a, "/answer").is_empty());
        assert_eq!(answer_box(&a).id, first);
        // Typing on a suggested answer starts the operator's own, also for
        // letters that are dashboard keys elsewhere.
        type_text(&mut a, "q");
        assert_eq!(
            (answer_box(&a).choice, answer_box(&a).input.text.as_str()),
            (2, "q")
        );
        type_text(&mut a, "2");
        assert_eq!(
            answer_box(&a).input.text,
            "q2",
            "on the typing row a digit is text"
        );
        press(&mut a, KeyCode::Up);
        assert_eq!(answer_box(&a).choice, 1);
        press(&mut a, KeyCode::Up);
        press(&mut a, KeyCode::Up);
        assert_eq!(answer_box(&a).choice, 0);
        // Enough wrapped rows to exercise scrolling rather than an invisible offset.
        a.questions
            .iter_mut()
            .find(|q| q.id == first)
            .unwrap()
            .body
            .push_str(&"\nQuestion line".repeat(60));
        press(&mut a, KeyCode::PageDown);
        assert_eq!(answer_box(&a).scroll, 5);
        press(&mut a, KeyCode::PageUp);
        assert_eq!(answer_box(&a).scroll, 0);

        // Tab walks the questions; each starts afresh.
        press(&mut a, KeyCode::Tab);
        assert_eq!(
            (
                answer_box(&a).id.as_str(),
                answer_box(&a).input.text.as_str()
            ),
            (second.as_str(), "")
        );
        press(&mut a, KeyCode::Tab);
        assert_eq!(answer_box(&a).id, first);
        press(&mut a, KeyCode::BackTab);
        assert_eq!(answer_box(&a).id, second);
        handle_paste(&mut a, "pasted\nanswer", 1_000_000);
        assert_eq!(answer_box(&a).input.text, "pasted\nanswer");

        // Answered from another terminal meanwhile: the next key moves on.
        a.questions.retain(|q| q.id != second);
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert_eq!(answer_box(&a).id, first);
        a.questions.clear();
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert!(a.modal.is_none());

        // Esc leaves the question open for later; the key works in every view.
        let (mut a, first, _) = with_questions();
        for view in [View::Detail, View::Tasks, View::Logs] {
            a.view = view;
            press(&mut a, KeyCode::Char('a'));
            assert_eq!(answer_box(&a).id, first, "{view:?}");
            assert!(press(&mut a, KeyCode::Esc).is_empty());
            assert!(a.modal.is_none());
        }
        assert_eq!(a.open_questions().len(), 2);
    }

    const SCREEN: Rect = Rect::new(0, 0, 120, 40);

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) -> Vec<Command> {
        let event = MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(app, event, SCREEN, 1_000_000, 10)
    }

    fn left_click(app: &mut App, x: u16, y: u16) -> Vec<Command> {
        mouse(app, MouseEventKind::Down(MouseButton::Left), x, y)
    }

    #[test]
    fn a_click_on_an_agent_opens_its_inspector() {
        let boxes = views::dashboard::agent_rects(&app(), SCREEN);
        // Six workers and the orchestrator, each with a box of its own.
        assert_eq!(boxes.len(), 7, "{boxes:?}");
        for (id, rect) in &boxes {
            let mut a = app();
            let commands = left_click(&mut a, rect.x + rect.width / 2, rect.y + rect.height / 2);
            assert_eq!(
                (a.view, a.detail.agent.as_str()),
                (View::Detail, id.as_str())
            );
            assert_eq!(a.selected.as_deref(), Some(id.as_str()));
            assert!(
                commands.contains(&Command::FetchOutput { id: id.clone() }),
                "{id}"
            );
            // The corners of the box count; the cell outside it does not.
            let mut a = app();
            left_click(&mut a, rect.x, rect.y);
            assert_eq!(a.detail.agent, *id);
            let mut a = app();
            left_click(&mut a, rect.right() - 1, rect.y);
            assert_eq!(a.detail.agent, *id);
            // The bottom border, where the box says what it runs on, opens
            // the model picker (a shell runs no model: the inspector).
            let mut a = app();
            let commands = left_click(&mut a, rect.right() - 1, rect.bottom() - 1);
            if a.agent(id).unwrap().runtime == crew_core::RuntimeKind::Shell {
                assert_eq!(a.detail.agent, *id);
            } else {
                assert!(
                    matches!(&a.modal, Some(Modal::Model(p)) if p.agent == *id),
                    "{id}: {:?}",
                    a.modal
                );
                assert_eq!(commands, vec![Command::FetchModels]);
                assert_eq!(a.view, View::Dashboard);
            }
        }
        // The boxes are where the screen shows them.
        let screen = crate::render_to_string(&app(), SCREEN.width, SCREEN.height, 1_000_000);
        let rows: Vec<&str> = screen.lines().collect();
        for (id, rect) in &boxes {
            let top: String = rows[usize::from(rect.y)]
                .chars()
                .skip(usize::from(rect.x))
                .take(usize::from(rect.width))
                .collect();
            assert!(
                top.contains(id.as_str()),
                "{id} is not drawn at {rect:?}: {top:?}"
            );
        }

        // Empty space opens nothing, and neither does a click under a box.
        let mut a = app();
        assert!(left_click(&mut a, 0, 0).is_empty());
        assert_eq!(a.view, View::Dashboard);
        a.modal = Some(Modal::ConfirmStop);
        let (_, rect) = &boxes[0];
        assert!(left_click(&mut a, rect.x + 1, rect.y + 1).is_empty());
        assert_eq!(
            (a.view, a.modal.clone()),
            (View::Dashboard, Some(Modal::ConfirmStop))
        );

        // On a screen too small for boxes the agents are rows of a list.
        let small = Rect::new(0, 0, 50, 12);
        let rows = views::dashboard::agent_rects(&app(), small);
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().all(|(_, r)| r.height == 1));
        let (id, row) = &rows[3];
        let mut a = app();
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut a, event, small, 1_000_000, 10);
        assert_eq!(a.detail.agent, *id);
    }

    #[test]
    fn a_click_on_a_bus_row_opens_the_message_in_full() {
        let mut a = app();
        let screen = crate::render_to_string(&a, SCREEN.width, SCREEN.height, 1_000_000);
        // The messages are listed under the heading, the newest last.
        let heading = screen
            .lines()
            .position(|l| l.contains("message bus"))
            .unwrap() as u16;
        let newest = a.messages.back().unwrap().clone();
        let y = heading + a.messages.len() as u16;
        assert!(left_click(&mut a, 30, y).is_empty());
        assert_eq!(
            a.modal,
            Some(Modal::Message(MessageView {
                id: newest.id.clone(),
                scroll: 0
            }))
        );
        // The one above it is the message before.
        let mut b = app();
        left_click(&mut b, 30, y - 1);
        let before = &b.messages[b.messages.len() - 2];
        assert!(matches!(&b.modal, Some(Modal::Message(view)) if view.id == before.id));
        // The heading of the bus is not a message, nor is a row left empty.
        let mut c = app();
        left_click(&mut c, 5, heading);
        assert!(c.modal.is_none());
        left_click(&mut c, 5, y + 1);
        assert!(c.modal.is_none());

        // It scrolls with the keys and the wheel, and closes on esc, on any
        // other key and on a right click.
        a.messages
            .iter_mut()
            .find(|m| m.id == newest.id)
            .unwrap()
            .body
            .push_str(&"\nMessage line".repeat(60));
        press(&mut a, KeyCode::Down);
        assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == 1));
        mouse(&mut a, MouseEventKind::ScrollUp, 10, 10);
        assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == 0));
        press(&mut a, KeyCode::Esc);
        assert!(a.modal.is_none());
        left_click(&mut a, 30, y);
        mouse(&mut a, MouseEventKind::Down(MouseButton::Right), 0, 0);
        assert!(a.modal.is_none());
    }

    #[test]
    fn scrolling_short_bodies_keeps_zero_offset_and_long_bodies_work_before_draw() {
        let mut a = app();
        let id = a.messages.back().unwrap().id.clone();
        a.messages.back_mut().unwrap().body = "short".into();
        a.modal = Some(Modal::Message(MessageView {
            id: id.clone(),
            scroll: 0,
        }));
        for code in [KeyCode::Down, KeyCode::PageDown, KeyCode::End] {
            press(&mut a, code);
            assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == 0));
        }
        mouse(&mut a, MouseEventKind::ScrollDown, 10, 10);
        assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == 0));
        a.messages.back_mut().unwrap().body = "long line\n".repeat(60);
        // No draw has happened: the fallback screen size still allows motion.
        press(&mut a, KeyCode::End);
        let before = match &a.modal {
            Some(Modal::Message(view)) => view.scroll,
            _ => unreachable!(),
        };
        assert!(before > 0);
        press(&mut a, KeyCode::Up);
        assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == before - 1));

        let (mut a, first, _) = with_questions();
        a.questions.iter_mut().find(|q| q.id == first).unwrap().body = "short".into();
        a.modal = Some(Modal::Question(question_box(first.clone())));
        press(&mut a, KeyCode::PageDown);
        mouse(&mut a, MouseEventKind::ScrollDown, 10, 10);
        assert_eq!(answer_box(&a).scroll, 0);
        a.questions.iter_mut().find(|q| q.id == first).unwrap().body = "long line\n".repeat(60);
        press(&mut a, KeyCode::PageDown);
        assert_eq!(answer_box(&a).scroll, 5);
    }

    #[test]
    fn the_wheel_scrolls_and_the_right_button_goes_back() {
        // The band that announces a question opens the answer box.
        let (mut a, first, _) = with_questions();
        a.questions
            .iter_mut()
            .find(|q| q.id == first)
            .unwrap()
            .body
            .push_str(&"\nQuestion line".repeat(60));
        left_click(&mut a, 40, 0);
        assert_eq!(answer_box(&a).id, first);
        mouse(&mut a, MouseEventKind::ScrollDown, 40, 10);
        assert_eq!(answer_box(&a).scroll, 5);
        mouse(&mut a, MouseEventKind::Down(MouseButton::Right), 40, 10);
        assert!(a.modal.is_none());

        // In the inspector the wheel moves the terminal output.
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        a.detail.output = (0..100).map(|i| format!("line {i}\n")).collect();
        mouse(&mut a, MouseEventKind::ScrollUp, 60, 20);
        assert_eq!(a.detail.scroll, WHEEL_ROWS);
        mouse(&mut a, MouseEventKind::ScrollDown, 60, 20);
        assert_eq!(a.detail.scroll, 0);
        mouse(&mut a, MouseEventKind::Down(MouseButton::Right), 60, 20);
        assert_eq!(a.view, View::Dashboard);

        // The help overlay closes on a click; pointer movement is nothing,
        // not even a reason to redraw.
        press(&mut a, KeyCode::Char('?'));
        left_click(&mut a, 5, 5);
        assert!(!a.help);
        let _ = a.take_dirty();
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
        ] {
            assert!(mouse(&mut a, kind, 60, 20).is_empty());
        }
        assert!(!a.take_dirty());

        // /mouse hands the pointer back to the terminal, and takes it again.
        assert_eq!(submit(&mut a, "/mouse"), vec![Command::SetMouse(false)]);
        assert!(a.status.as_ref().unwrap().text.contains("mouse off"));
        a.mouse = false;
        let boxes = views::dashboard::agent_rects(&a, SCREEN);
        assert!(
            left_click(&mut a, boxes[0].1.x + 2, boxes[0].1.y + 1).is_empty(),
            "off means off"
        );
        assert_eq!(a.view, View::Dashboard);
        assert_eq!(submit(&mut a, "/mouse"), vec![Command::SetMouse(true)]);
    }

    #[test]
    fn tab_completes_a_command_instead_of_changing_the_recipient() {
        let mut a = app();
        press(&mut a, KeyCode::Char('/'));
        type_text(&mut a, "st");
        press(&mut a, KeyCode::Tab);
        assert_eq!(
            (compose(&a).text.as_str(), compose(&a).cursor),
            ("/stop", 5)
        );
        assert_eq!(compose(&a).to, "claude-1");
        ctrl(&mut a, 'u');
        type_text(&mut a, "/k");
        press(&mut a, KeyCode::Tab);
        assert_eq!(compose(&a).text, "/kill ");
        type_text(&mut a, "claude-2");
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.modal, Some(Modal::ConfirmKill("claude-2".into())));
    }

    #[test]
    fn compose_editing_and_recipient_cycling() {
        let mut a = app();
        press(&mut a, KeyCode::Char('m'));
        type_text(&mut a, "héllo wörld again");
        ctrl(&mut a, 'w');
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.text == "héllo wörld "));
        press(&mut a, KeyCode::Home);
        press(&mut a, KeyCode::Delete);
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.text == "éllo wörld "));
        press(&mut a, KeyCode::End);
        ctrl(&mut a, 'u');
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.text.is_empty() && c.cursor == 0));
        handle_paste(&mut a, "two\r\nlines\u{7}", 1_000_000);
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.text == "two\nlines"));

        press(&mut a, KeyCode::Tab);
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.to == "claude-2"));
        press(&mut a, KeyCode::BackTab);
        press(&mut a, KeyCode::BackTab);
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.to == "*"));
    }

    #[test]
    fn kill_requires_confirmation() {
        let mut a = app();
        assert!(press(&mut a, KeyCode::Char('k')).is_empty());
        assert_eq!(a.modal, Some(Modal::ConfirmKill("claude-1".into())));
        assert!(press(&mut a, KeyCode::Char('n')).is_empty());
        assert!(a.modal.is_none());
        press(&mut a, KeyCode::Char('k'));
        assert_eq!(
            press(&mut a, KeyCode::Char('y')),
            vec![Command::Kill("claude-1".into())]
        );
    }

    #[test]
    fn pause_toggles_and_restart_targets_the_selection() {
        let mut a = app();
        assert_eq!(
            press(&mut a, KeyCode::Char('p')),
            vec![Command::Pause("claude-1".into())]
        );
        a.agents[0].paused = true;
        assert_eq!(
            press(&mut a, KeyCode::Char('p')),
            vec![Command::Resume("claude-1".into())]
        );
        assert_eq!(
            press(&mut a, KeyCode::Char('r')),
            vec![Command::Restart("claude-1".into())]
        );
        // In the inspector `r` refreshes and `R` restarts.
        press(&mut a, KeyCode::Enter);
        let cmds = press(&mut a, KeyCode::Char('r'));
        assert!(cmds.contains(&Command::FetchOutput {
            id: "claude-1".into()
        }));
        assert_eq!(
            press(&mut a, KeyCode::Char('R')),
            vec![Command::Restart("claude-1".into())]
        );
        assert_eq!(
            press(&mut a, KeyCode::Char('d')),
            vec![Command::FetchDiff {
                id: "claude-1".into()
            }]
        );
        assert!(a.detail.show_diff);
    }

    #[test]
    fn herdr_focus_only_with_the_herdr_backend() {
        let mut a = app();
        assert_eq!(
            press(&mut a, KeyCode::Char('g')),
            vec![Command::Focus("claude-1".into())]
        );
        a.session.backend = "pty".into();
        assert!(press(&mut a, KeyCode::Char('g')).is_empty());
        assert!(a.status.as_ref().unwrap().is_error);
    }

    #[test]
    fn views_switch_and_logs_scroll_and_filter() {
        let mut a = app();
        press(&mut a, KeyCode::Char('t'));
        assert_eq!(a.view, View::Tasks);
        press(&mut a, KeyCode::Char('l'));
        assert_eq!(a.view, View::Logs);
        for i in 0..40 {
            let mut e = a.events.back().unwrap().clone();
            e.seq += 1;
            e.agent_id = Some(if i % 2 == 0 { "claude-1" } else { "claude-2" }.into());
            a.apply_push(crew_core::rpc::Push::Event(e), 0);
        }
        press(&mut a, KeyCode::PageUp);
        assert_eq!(a.logs.scroll, 10);
        press(&mut a, KeyCode::Home);
        assert_eq!(a.logs.scroll, a.events.len() - 10);
        press(&mut a, KeyCode::End);
        assert_eq!(a.logs.scroll, 0);
        press(&mut a, KeyCode::Char('f'));
        assert_eq!(a.logs.filter_agent.as_deref(), Some("claude-1"));
        assert!(logs::visible_events(&a)
            .iter()
            .all(|e| e.agent_id.as_deref() == Some("claude-1")));
        press(&mut a, KeyCode::Char('f'));
        assert_eq!(a.logs.filter_agent, None);
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.view, View::Dashboard);
    }

    #[test]
    fn help_opens_and_any_key_closes_it() {
        let mut a = app();
        press(&mut a, KeyCode::Char('?'));
        assert!(a.help);
        assert!(press(&mut a, KeyCode::Char('q')).is_empty());
        assert!(!a.help);
    }

    // ---- the model picker ---------------------------------------------------

    fn picker(a: &App) -> ModelPicker {
        match &a.modal {
            Some(Modal::Model(picker)) => picker.clone(),
            other => panic!("the model picker is not open: {other:?}"),
        }
    }

    fn listed(a: &mut App) {
        a.apply_reply(crate::app::Reply::Models(Ok(fixture::models())), 1_000_000);
    }

    #[test]
    fn the_operator_chooses_the_orchestrators_model_in_a_picker() {
        use crew_core::types::ORCHESTRATOR_ID;
        use crew_core::RuntimeKind;
        let mut a = app();
        a.selected = Some("claude-1".to_string());
        // O is the orchestrator's whatever is selected, and asks for the models.
        assert_eq!(
            press(&mut a, KeyCode::Char('O')),
            vec![Command::FetchModels]
        );
        assert_eq!(
            (picker(&a).agent.as_str(), picker(&a).runtime),
            (ORCHESTRATOR_ID, RuntimeKind::Claude)
        );
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert_eq!(
            picker(&a).notice.as_deref(),
            Some("the models are still being listed")
        );
        // The listing comes in: the highlight is on what it runs on.
        listed(&mut a);
        assert!(!a.models_pending);
        assert_eq!(picker(&a).row, 2);
        assert_eq!(a.current_row(ORCHESTRATOR_ID, RuntimeKind::Claude), Some(2));
        // Its own model again changes nothing.
        let mut same = a.clone();
        assert!(press(&mut same, KeyCode::Enter).is_empty());
        assert!(same.modal.is_none());
        assert!(same
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("orchestrator already runs on Sonnet 5.5"));
        // Another model of its CLI: the session is resumed on it.
        press(&mut a, KeyCode::Up);
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::SetModel {
                id: ORCHESTRATOR_ID.to_string(),
                model: "opus".to_string(),
                runtime: RuntimeKind::Claude,
                label: "restarting orchestrator on Opus 5.5, resuming its session".to_string(),
            }]
        );
        assert!(a.modal.is_none());
        assert!(a
            .status
            .as_ref()
            .unwrap()
            .text
            .starts_with("restarting orchestrator on Opus 5.5"));
        // A listing that comes in after the highlight was moved leaves it.
        press(&mut a, KeyCode::Char('O'));
        press(&mut a, KeyCode::Up);
        listed(&mut a);
        assert_eq!(picker(&a).row, 1);
        // Esc leaves it as it was.
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.modal.is_none());
    }

    #[test]
    fn another_cli_is_a_tab_away_and_a_model_that_cannot_start_is_not_chosen() {
        use crew_core::types::ORCHESTRATOR_ID;
        use crew_core::RuntimeKind;
        let mut a = app();
        press(&mut a, KeyCode::Char('O'));
        listed(&mut a);
        // Codex: nothing is current there; the first model that starts.
        press(&mut a, KeyCode::Right);
        assert_eq!(
            (picker(&a).runtime, picker(&a).row),
            (RuntimeKind::Codex, 0)
        );
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Down);
        assert_eq!(picker(&a).row, 1, "the last row is the last");
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        let notice = picker(&a).notice.unwrap();
        assert!(
            notice.contains("GPT 6 Luna cannot be started here: not in the model list"),
            "{notice}"
        );
        press(&mut a, KeyCode::Up);
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::SetModel {
                id: ORCHESTRATOR_ID.to_string(),
                model: "gpt-6-astra".to_string(),
                runtime: RuntimeKind::Codex,
                label: "moving orchestrator to Codex on GPT 6 Astra: a fresh session there, handed the work".to_string(),
            }]
        );
        // pi by its number, and its own default first.
        press(&mut a, KeyCode::Char('O'));
        press(&mut a, KeyCode::Char('3'));
        assert_eq!((picker(&a).runtime, picker(&a).row), (RuntimeKind::Pi, 0));
        let commands = press(&mut a, KeyCode::Enter);
        assert!(
            matches!(&commands[..], [Command::SetModel { model, runtime: RuntimeKind::Pi, .. }] if model.is_empty()),
            "{commands:?}"
        );
        // The tabs wrap around.
        press(&mut a, KeyCode::Char('O'));
        press(&mut a, KeyCode::Left);
        assert_eq!(picker(&a).runtime, RuntimeKind::Pi);
        press(&mut a, KeyCode::Tab);
        assert_eq!(
            (picker(&a).runtime, picker(&a).row),
            (RuntimeKind::Claude, 2)
        );
    }

    #[test]
    fn s_is_for_the_selected_or_inspected_agent_and_a_shell_has_no_model() {
        use crew_core::RuntimeKind;
        let mut a = app();
        a.selected = Some("claude-2".to_string());
        assert_eq!(
            press(&mut a, KeyCode::Char('s')),
            vec![Command::FetchModels]
        );
        assert_eq!(
            (picker(&a).agent.as_str(), picker(&a).runtime),
            ("claude-2", RuntimeKind::Codex)
        );
        press(&mut a, KeyCode::Esc);
        a.open_detail("claude-3", 1_000_000);
        press(&mut a, KeyCode::Char('s'));
        assert_eq!(picker(&a).agent, "claude-3");
        press(&mut a, KeyCode::Esc);
        // A command opens it too.
        a.view = View::Dashboard;
        open_command(&mut a);
        type_text(&mut a, "model claude-4");
        assert_eq!(press(&mut a, KeyCode::Enter), vec![Command::FetchModels]);
        assert_eq!(picker(&a).agent, "claude-4");
        press(&mut a, KeyCode::Esc);
        // A shell runs no model.
        a.agents
            .iter_mut()
            .find(|x| x.id == "claude-5")
            .unwrap()
            .runtime = RuntimeKind::Shell;
        a.selected = Some("claude-5".to_string());
        assert!(press(&mut a, KeyCode::Char('s')).is_empty());
        assert!(a.modal.is_none());
        assert!(a
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("claude-5 is a shell agent"));
    }

    #[test]
    fn a_question_from_the_team_opens_its_answer_box_by_itself_once() {
        use crew_core::rpc::Push;
        let t0 = 1_000_000;
        let mut a = app();
        let question = fixture::question(t0);
        a.apply_push(Push::Message(question.clone()), t0);
        // The band and the status line say so; the next tick opens the box,
        // and the bell is due once.
        assert!(a
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("orchestrator, for claude-2 has a question for you"));
        a.tick(t0);
        assert_eq!(answer_box(&a).id, question.id);
        assert_eq!(answer_box(&a).popped_at, t0);
        assert!(a.take_ring());
        assert!(!a.take_ring(), "one bell per question");

        // An Enter right after it opened was meant for something else.
        let enter = |a: &mut App, at: i64| {
            handle_key(a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), at, 10)
        };
        assert!(enter(&mut a, t0 + 300).is_empty());
        assert_eq!(answer_box(&a).id, question.id, "still open, nothing sent");
        // Put off with esc, it stays in the band and does not open again.
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            t0 + 400,
            10,
        );
        a.tick(t0 + 500);
        assert!(a.modal.is_none());
        assert_eq!(a.open_questions().len(), 1);

        // While the operator types a message, a new question waits; it opens
        // once the message box is closed.
        let mut second = question.clone();
        second.id = "m-later".into();
        open_compose(&mut a, None);
        a.apply_push(Push::Message(second.clone()), t0 + 600);
        a.tick(t0 + 600);
        assert!(matches!(a.modal, Some(Modal::Compose(_))));
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            t0 + 700,
            10,
        );
        a.tick(t0 + 800);
        assert_eq!(answer_box(&a).id, second.id);
        // Past the moment, Enter sends the selected answer.
        assert_eq!(
            enter(&mut a, t0 + 800 + crate::app::POPUP_GRACE_MS),
            vec![Command::Answer {
                id: second.id.clone(),
                body: "Convert in the adapter".into(),
            }]
        );
        // Opened with `a`, the box takes Enter at once.
        a.modal = None;
        press(&mut a, KeyCode::Char('a'));
        assert_eq!(answer_box(&a).popped_at, 0);

        // Typing into an agent's terminal holds a question back too.
        let mut b = app();
        b.open_detail("claude-1", t0);
        b.detail.typing = true;
        b.apply_push(Push::Message(question.clone()), t0);
        b.tick(t0);
        assert!(b.modal.is_none());
        b.detail.typing = false;
        b.tick(t0 + 100);
        assert_eq!(answer_box(&b).id, question.id);
    }
}
