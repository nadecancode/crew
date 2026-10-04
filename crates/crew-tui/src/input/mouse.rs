use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::app::{App, Command, MessageView, Modal, View};
use crate::views::dashboard::Hit;
use crate::views::{self, message};

use super::{handle_key, open_questions, scroll_message};

/// Rows the wheel moves per notch.
pub(super) const WHEEL_ROWS: usize = 3;

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
                Some(Modal::Message(view)) => {
                    message::scroll_limit(app, view, views::content_area(app, area))
                }
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
