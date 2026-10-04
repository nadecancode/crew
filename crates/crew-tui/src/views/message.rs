//! One bus message in full. The bus log shows a line of each; a click on a
//! row opens the whole text here.

use crew_core::util::fmt_clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Widget};

use super::help::centered;
use super::question::wrap;
use crate::app::{App, MessageView};
use crate::theme::Theme;
use crate::widgets::canvas::put_str;
use crate::widgets::text::fit;

const KEYS: &str = "↑↓ or wheel scroll · esc or right-click close";

/// The lines of the message at `cols` columns (0 when it is gone).
pub fn line_count(app: &App, view: &MessageView, cols: usize) -> usize {
    app.messages
        .iter()
        .find(|m| m.id == view.id)
        .map_or(0, |m| wrap(&m.body, cols).len())
}

/// Columns of text the box has on a screen `width` wide.
pub fn text_cols(width: u16) -> usize {
    usize::from(width.saturating_sub(6).clamp(24, 110)).saturating_sub(4)
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, view: &MessageView, theme: &Theme) {
    let box_w = area.width.saturating_sub(6).clamp(24, 110).min(area.width);
    if box_w < 24 || area.height < 6 {
        return;
    }
    let cols = usize::from(box_w) - 4;
    let Some(message) = app.messages.iter().find(|m| m.id == view.id) else {
        // It scrolled out of the log the dashboard keeps.
        let rect = centered(area, box_w, 4);
        frame(buf, rect, "message", theme);
        put_str(
            buf,
            rect,
            i32::from(rect.x) + 2,
            i32::from(rect.y) + 1,
            &fit("This message is no longer in the log.", cols),
            theme.fg(theme.dim),
        );
        return;
    };
    let lines = wrap(&message.body, cols);
    // Borders, a blank row and the key hints frame the text.
    let visible = lines.len().clamp(1, usize::from(area.height).saturating_sub(6).max(1));
    let rect = centered(area, box_w, visible as u16 + 4);
    let title = format!(
        "{} → {} · {} · {}",
        message.from,
        message.to,
        message.kind,
        fmt_clock(message.timestamp)
    );
    frame(buf, rect, &fit(&title, cols), theme);

    let x = i32::from(rect.x) + 2;
    let mut y = i32::from(rect.y) + 1;
    let scroll = view.scroll.min(lines.len().saturating_sub(visible));
    for line in lines.iter().skip(scroll).take(visible) {
        put_str(buf, rect, x, y, line, theme.fg(theme.text));
        y += 1;
    }
    let below = lines.len().saturating_sub(scroll + visible);
    let hint = if below > 0 {
        format!("{KEYS} · {below} more line(s)")
    } else {
        KEYS.to_string()
    };
    put_str(
        buf,
        rect,
        x,
        i32::from(rect.bottom()) - 2,
        &fit(&hint, cols),
        theme.fg(theme.dim),
    );
}

fn frame(buf: &mut Buffer, rect: Rect, title: &str, theme: &Theme) {
    Clear.render(rect, buf);
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme.fg(theme.accent))
        .title(Span::styled(format!(" {title} "), theme.bold(theme.text)))
        .render(rect, buf);
}
