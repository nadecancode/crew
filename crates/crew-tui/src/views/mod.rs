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
    app.screen.set(area);
    if area.is_empty() {
        return;
    }
    if let Some(text) = sandbox_warning(app) {
        let style = theme.bold(theme.warn).bg(theme.bar_bg);
        fill_row(buf, area, area.y, style);
        let label = if area.width < 10 {
            "!".into()
        } else {
            fit(text, usize::from(area.width))
        };
        put_str(
            buf,
            area,
            i32::from(area.x),
            i32::from(area.y),
            &label,
            style,
        );
    }
    let content = content_area(app, area);
    // A question waiting for the operator is announced above whatever view
    // is open, so it is not missed from the inspector or the logs.
    let band_h = question::band_height(app, content);
    if band_h > 0 {
        let band = Rect {
            height: band_h,
            ..content
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
        help::render(buf, content, theme);
    }
    match &app.modal {
        Some(Modal::Question(answer)) => question::render(buf, content, app, answer, theme, now),
        Some(Modal::Message(view)) => message::render(buf, content, app, view, theme),
        Some(Modal::Model(picker)) => models::render(buf, content, app, picker, theme),
        Some(modal) => compose::render(buf, content, modal, theme),
        None => {}
    }
    if !app.connected {
        banner(buf, content, app, theme);
    }
}

/// The daemon supplies the mode; missing or unfamiliar state cannot confirm
/// confinement. Show fixed text, never the raw wire value.
fn sandbox_warning(app: &App) -> Option<&'static str> {
    match app.session.sandbox.as_str() {
        "required" => None,
        "off" => Some("NO SANDBOX · agents can access the host"),
        _ => Some("SANDBOX UNCONFIRMED · host access may be unrestricted"),
    }
}

/// Space below the persistent warning, shared by modal rendering and input
/// limits. app.screen retains the full terminal size, including before draw.
pub(crate) fn content_area(app: &App, area: Rect) -> Rect {
    let header = u16::from(area.height > 0 && sandbox_warning(app).is_some());
    Rect {
        y: area.y + header,
        height: area.height - header,
        ..area
    }
}

/// The area a view gets inside the screen: everything below the band that
/// announces questions for the operator.
pub(crate) fn view_area(app: &App, area: Rect) -> Rect {
    let area = content_area(app, area);
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
    if y < content_area(app, area).y {
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
