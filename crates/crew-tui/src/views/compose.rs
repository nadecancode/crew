//! Modal dialogs: composing a message (or a slash command) and confirmations.

use crew_core::types::BROADCAST;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Widget};
use unicode_width::UnicodeWidthChar;

use super::help::centered;
use crate::app::{Compose, Modal};
use crate::slash;
use crate::theme::Theme;
use crate::widgets::canvas::put_str;
use crate::widgets::text::{fit, width};

pub fn render(buf: &mut Buffer, area: Rect, modal: &Modal, theme: &Theme) {
    match modal {
        Modal::Compose(compose) => render_compose(buf, area, compose, theme),
        Modal::Type(compose) => render_type(buf, area, compose, theme),
        Modal::ConfirmKill(id) => render_confirm(
            buf,
            area,
            "kill agent",
            &format!("Stop {id}'s process? Its task stays open."),
            "y kill · any other key cancels",
            theme,
        ),
        Modal::ConfirmRestartPaused(count) => render_confirm(
            buf,
            area,
            "restart paused agents",
            &format!(
                "Restart {count} agent(s) paused at a usage limit? They start afresh on the \
login or key now in place and resume their conversations."
            ),
            "y restart · any other key cancels",
            theme,
        ),
        // Drawn by their own views, which need the app.
        Modal::Question(_) | Modal::Message(_) | Modal::Model(_) => {}
        Modal::ConfirmStop => render_confirm(
            buf,
            area,
            "stop the team",
            "Stop every agent and the crew daemon? Committed work stays on the agents' branches.",
            "y stop · any other key cancels",
            theme,
        ),
    }
}

fn frame(buf: &mut Buffer, rect: Rect, title: &str, theme: &Theme) {
    Clear.render(rect, buf);
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme.fg(theme.accent))
        .title(Span::styled(format!(" {title} "), theme.bold(theme.text)))
        .render(rect, buf);
}

fn render_compose(buf: &mut Buffer, area: Rect, compose: &Compose, theme: &Theme) {
    let rect = centered(area, area.width.saturating_sub(8).clamp(20, 90), 5);
    if rect.width < 12 || rect.height < 5 {
        return;
    }
    let recipient = if compose.to == BROADCAST {
        "everyone (broadcast)".to_string()
    } else {
        compose.to.clone()
    };
    // A line that opens with a slash goes to crew, not to the recipient.
    let command_hint = slash::hint(slash::DASHBOARD, &compose.text);
    let title = match command_hint {
        Some(_) => "command".to_string(),
        None => format!("message → {recipient}"),
    };
    frame(buf, rect, &title, theme);
    let x = i32::from(rect.x) + 2;
    let y = i32::from(rect.y) + 1;
    let inner = usize::from(rect.width) - 4;

    put_str(buf, rect, x, y, "› ", theme.bold(theme.accent));
    input_line(
        buf,
        rect,
        (x + 2, y),
        inner.saturating_sub(2),
        &compose.text,
        Some(compose.cursor),
        theme,
    );
    let (hint, style) = match (&compose.notice, command_hint) {
        (Some(notice), _) => (notice.clone(), theme.fg(theme.warn)),
        (None, Some(hint)) => (hint, theme.fg(theme.muted)),
        (None, None) => (
            "enter send · shift+enter new line · tab recipient · esc cancel · / commands"
                .to_string(),
            theme.fg(theme.dim),
        ),
    };
    put_str(buf, rect, x, y + 2, &fit(&hint, inner), style);
}

/// The line typed into an agent's terminal.
fn render_type(buf: &mut Buffer, area: Rect, compose: &Compose, theme: &Theme) {
    let rect = centered(area, area.width.saturating_sub(8).clamp(20, 90), 5);
    if rect.width < 12 || rect.height < 5 {
        return;
    }
    frame(
        buf,
        rect,
        &format!("type into {}'s terminal", compose.to),
        theme,
    );
    let x = i32::from(rect.x) + 2;
    let y = i32::from(rect.y) + 1;
    let inner = usize::from(rect.width) - 4;
    put_str(buf, rect, x, y, "❯ ", theme.bold(theme.accent));
    input_line(
        buf,
        rect,
        (x + 2, y),
        inner.saturating_sub(2),
        &compose.text,
        Some(compose.cursor),
        theme,
    );
    let hint = "enter sends as a prompt · shift+enter new line · enter alone presses ⏎ · esc back";
    put_str(buf, rect, x, y + 2, &fit(hint, inner), theme.fg(theme.dim));
}

/// Draw an editable line in the `cols` columns from `origin`, scrolled sideways so
/// the cursor (a character index; `None` hides it) stays in view. Line breaks
/// show as `⏎`.
pub(crate) fn input_line(
    buf: &mut Buffer,
    clip: Rect,
    origin: (i32, i32),
    cols: usize,
    text: &str,
    cursor: Option<usize>,
    theme: &Theme,
) {
    let (x, y) = origin;
    let chars: Vec<char> = text
        .chars()
        .map(|c| if c == '\n' { '⏎' } else { c })
        .collect();
    let at = cursor.unwrap_or(0).min(chars.len());
    let cell = |c: &char| UnicodeWidthChar::width(*c).unwrap_or(0);
    let room = cols.saturating_sub(1);
    let mut start = at;
    let mut used = 0;
    while start > 0 {
        let next = cell(&chars[start - 1]);
        if used + next > room {
            break;
        }
        used += next;
        start -= 1;
    }
    let end = x + cols as i32;
    let cursor_style = theme.fg(theme.text).add_modifier(Modifier::REVERSED);
    let mut cx = x;
    for (i, ch) in chars.iter().enumerate().skip(start) {
        let w = cell(ch) as i32;
        if cx + w > end {
            break;
        }
        let style = if cursor == Some(i) {
            cursor_style
        } else {
            theme.fg(theme.text)
        };
        put_str(buf, clip, cx, y, &ch.to_string(), style);
        cx += w;
    }
    if cursor.is_some_and(|c| c >= chars.len()) && cx < end {
        put_str(buf, clip, cx, y, " ", cursor_style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_compose_line_renders_within_a_frame_budget() {
        let text = format!("{}日本tail", "x".repeat(7990));
        let clip = Rect::new(0, 0, 120, 1);
        let mut buf = Buffer::empty(clip);
        let mut fastest = std::time::Duration::MAX;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            input_line(
                &mut buf,
                clip,
                (0, 0),
                80,
                &text,
                Some(text.chars().count()),
                &Theme::truecolor(),
            );
            fastest = fastest.min(start.elapsed());
        }
        assert!(buf
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>()
            .contains("tail"));
        eprintln!("8000-character input frame: {fastest:?}");
        assert!(
            fastest < std::time::Duration::from_millis(20),
            "{fastest:?}"
        );
    }
}

fn render_confirm(
    buf: &mut Buffer,
    area: Rect,
    title: &str,
    question: &str,
    keys: &str,
    theme: &Theme,
) {
    let rect = centered(area, (width(question) as u16 + 4).clamp(48, 110), 5);
    if rect.width < 12 || rect.height < 5 {
        return;
    }
    frame(buf, rect, title, theme);
    let x = i32::from(rect.x) + 2;
    let y = i32::from(rect.y) + 1;
    let inner = usize::from(rect.width) - 4;
    put_str(buf, rect, x, y, &fit(question, inner), theme.fg(theme.text));
    put_str(buf, rect, x, y + 2, &fit(keys, inner), theme.fg(theme.dim));
}
