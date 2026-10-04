use crew_core::types::BROADCAST;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Command, Compose, MessageView, Modal, ModelPicker, QuestionBox, HARNESSES};
use crate::slash::{self, slash_command};
use crate::views::{self, message};

use super::{question_box, run_slash};

/// Longest message the compose box accepts, in characters.
const MAX_COMPOSE_CHARS: usize = 8000;

// ---- modals -----------------------------------------------------------------

pub(super) fn modal_key(app: &mut App, key: KeyEvent, now: i64) -> Vec<Command> {
    let Some(modal) = app.modal.take() else {
        return Vec::new();
    };
    match modal {
        Modal::ConfirmKill(id) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => vec![Command::Kill(id)],
            _ => Vec::new(),
        },
        Modal::ConfirmRestartFresh(id) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => vec![Command::RestartFresh(id)],
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
            let lines =
                message::scroll_limit(app, &view, views::content_area(app, app.screen.get()));
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
pub(super) fn scroll_message(view: &mut MessageView, up: bool, rows: usize, lines: usize) {
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
            let limit = views::question::scroll_limit(
                &q.body,
                options.len(),
                views::content_area(app, app.screen.get()),
            );
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

pub(super) fn insert(compose: &mut Compose, ch: char) {
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
