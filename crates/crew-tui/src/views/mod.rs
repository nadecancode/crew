//! Rendering. Each view draws straight into the frame buffer; ratatui diffs
//! the result against the previous frame, so only changed cells reach the
//! terminal.

pub mod compose;
pub mod dashboard;
pub mod detail;
pub mod help;
pub mod logs;
pub mod message;
pub mod models;
pub mod question;
pub mod tasks;
pub mod usage;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;

use crate::app::{App, Modal, View};
use crate::theme::Theme;
use crate::widgets::canvas::{fill_row, put_str};
use crate::widgets::text::fit;

pub fn render(frame: &mut Frame, app: &App, theme: &Theme, now: i64) {
    let area = frame.area();
    render_buffer(frame.buffer_mut(), area, app, theme, now);
}

pub fn render_buffer(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    if area.is_empty() {
        return;
    }
    // A question waiting for the operator is announced above whatever view
    // is open, so it is not missed from the inspector or the logs.
    let band_h = question::band_height(app, area);
    if band_h > 0 {
        let band = Rect {
            height: band_h,
            ..area
        };
        question::render_band(buf, band, app, theme, now);
    }
    let view = view_area(app, area);
    match app.view {
        View::Dashboard => dashboard::render(buf, view, app, theme, now),
        View::Detail => detail::render(buf, view, app, theme, now),
        View::Tasks => tasks::render(buf, view, app, theme, now),
        View::Logs => logs::render(buf, view, app, theme),
    }
    if app.help {
        help::render(buf, area, theme);
    }
    match &app.modal {
        Some(Modal::Question(answer)) => question::render(buf, area, app, answer, theme, now),
        Some(Modal::Message(view)) => message::render(buf, area, app, view, theme),
        Some(Modal::Model(picker)) => models::render(buf, area, app, picker, theme),
        Some(modal) => compose::render(buf, area, modal, theme),
        None => {}
    }
    if !app.connected {
        banner(buf, area, app, theme);
    }
}

/// The area a view gets inside the screen: everything below the band that
/// announces questions for the operator.
fn view_area(app: &App, area: Rect) -> Rect {
    let band_h = question::band_height(app, area);
    Rect {
        y: area.y + band_h,
        height: area.height - band_h,
        ..area
    }
}

/// What a click at column `x`, row `y` of the screen lands on.
pub fn hit(app: &App, area: Rect, x: u16, y: u16) -> Option<dashboard::Hit> {
    if area.is_empty() {
        return None;
    }
    let view = view_area(app, area);
    if y < view.y {
        return Some(dashboard::Hit::Questions);
    }
    match app.view {
        View::Dashboard => dashboard::hit(app, view, x, y),
        View::Detail => detail::hit(app, x, y),
        _ => None,
    }
}

/// Top-row banner shown while the daemon is unreachable.
fn banner(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    let text = if app.loaded {
        "daemon disconnected — reconnecting… (showing last known state)"
    } else {
        "connecting to the crew daemon…"
    };
    let style = Style::default()
        .fg(theme.text)
        .bg(theme.err)
        .add_modifier(Modifier::BOLD);
    fill_row(buf, area, area.y, style);
    put_str(
        buf,
        area,
        i32::from(area.x) + 1,
        i32::from(area.y),
        &fit(text, usize::from(area.width).saturating_sub(2)),
        style,
    );
}
