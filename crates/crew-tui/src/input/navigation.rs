use crew_core::types::BROADCAST;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Command, Compose, Modal, QuestionBox, View};
use crate::views::logs;

// ---- actions shared by the dashboard and the inspector ---------------------

pub(super) fn open_compose(app: &mut App, to: Option<String>) {
    let to = to
        .or_else(|| app.selected.clone())
        .unwrap_or_else(|| BROADCAST.to_string());
    app.modal = Some(Modal::Compose(Compose {
        to,
        ..Compose::default()
    }));
}

/// The message box as a command line: it opens with the slash typed.
pub(super) fn open_command(app: &mut App) {
    open_compose(app, None);
    if let Some(Modal::Compose(compose)) = &mut app.modal {
        compose.text = "/".to_string();
        compose.cursor = 1;
    }
}

/// Open the answer box on the oldest question waiting for the operator.
pub(super) fn open_questions(app: &mut App, now: i64) {
    match app.open_questions().first().map(|q| q.id.clone()) {
        Some(id) => app.modal = Some(Modal::Question(question_box(id))),
        None => app.set_status("no question is waiting for you", false, now),
    }
}

pub(super) fn question_box(id: String) -> QuestionBox {
    QuestionBox {
        id,
        ..QuestionBox::default()
    }
}

/// Tell every agent paused at a usage limit to continue.
pub(super) fn resume_paused(app: &mut App, now: i64) -> Vec<Command> {
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

pub(super) fn dashboard_key(app: &mut App, key: KeyEvent, now: i64) -> Vec<Command> {
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

pub(super) fn detail_key(app: &mut App, key: KeyEvent, now: i64, page: usize) -> Vec<Command> {
    // The toolbox pane pages by its own height, so a page skips no row.
    let pane = usize::from(app.detail.terminal_rect.get().height.saturating_sub(2));
    let page = if app.detail.show_toolbox && pane > 0 {
        pane
    } else {
        page
    };
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
        KeyCode::Char('T') => {
            app.detail.show_toolbox = !app.detail.show_toolbox;
            app.detail.show_diff = false;
            // A document: it opens at its top (clamped below).
            app.detail.scroll = if app.detail.show_toolbox {
                usize::MAX
            } else {
                0
            };
        }
        KeyCode::Char('d') => {
            app.detail.show_diff = !app.detail.show_diff;
            app.detail.show_toolbox = false;
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
    if detail.show_toolbox {
        // Scrolled to the top is the last page's distance from the end, so
        // the first key down moves the text.
        let rows = usize::from(detail.terminal_rect.get().height.saturating_sub(2));
        return app
            .agent(&detail.agent)
            .and_then(|agent| agent.toolbox.as_ref())
            .map_or(0, |toolbox| {
                crate::views::detail::toolbox_rows(toolbox, detail.cols.get())
                    .len()
                    .saturating_sub(rows)
            });
    }
    if detail.show_diff {
        return detail.diff.as_deref().unwrap_or("").lines().count();
    }
    // Scrolled up, the pane shows the transcript when there is one.
    match detail.transcript_lines(detail.cols.get()) {
        Some(lines) => lines.len(),
        None => detail.output.lines().count(),
    }
}

pub(super) fn tasks_key(app: &mut App, key: KeyEvent, now: i64, page: usize) -> Vec<Command> {
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

pub(super) fn logs_key(app: &mut App, key: KeyEvent, now: i64, page: usize) -> Vec<Command> {
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
