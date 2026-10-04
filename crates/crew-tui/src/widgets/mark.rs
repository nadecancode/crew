//! What a VCS action leaves on a box: a bright segment running along its
//! border, and a label on the bottom border (`anim::Mark`).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::canvas::put_str;
use super::text::{fit, width};
use crate::anim::{Mark, MarkKind, Tone};
use crate::theme::{spinner, Theme};

pub fn tone_color(tone: Tone, theme: &Theme) -> Color {
    match tone {
        Tone::Ok => theme.ok,
        Tone::Conflict => theme.err,
        Tone::Error => theme.warn,
    }
}

/// The label as drawn: a running mark leads with the spinner.
pub fn text(mark: &Mark, now: i64) -> String {
    match mark.kind {
        MarkKind::Running { .. } => format!("{} {}", spinner(now), mark.label),
        _ => mark.label.clone(),
    }
}

/// The style of the label (and, for a write, of the sweep).
pub fn style(mark: &Mark, theme: &Theme) -> Style {
    match mark.kind {
        MarkKind::Running { .. } => theme.fg(theme.accent),
        MarkKind::Done => theme.bold(tone_color(mark.tone, theme)),
        // A read is quiet; only a failed one takes a colour.
        MarkKind::Glance if mark.tone == Tone::Error => theme.fg(theme.warn),
        MarkKind::Glance => theme.fg(theme.muted),
    }
}

/// Cells of the border, clockwise from the top left corner.
fn perimeter(rect: Rect) -> Vec<(u16, u16)> {
    let (x0, y0) = (rect.x, rect.y);
    let (x1, y1) = (rect.right() - 1, rect.bottom() - 1);
    let mut cells = Vec::new();
    cells.extend((x0..=x1).map(|x| (x, y0)));
    cells.extend((y0 + 1..=y1).map(|y| (x1, y)));
    cells.extend((x0..x1).rev().map(|x| (x, y1)));
    cells.extend((y0 + 1..y1).rev().map(|y| (x0, y)));
    cells
}

/// Light up `cells` in `style`, keeping the glyph that is there.
fn light(buf: &mut Buffer, cells: impl Iterator<Item = (u16, u16)>, style: Style) {
    for (x, y) in cells {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_style(style);
        }
    }
}

/// Draw the sweep and the label of `mark` on the box `rect`.
pub fn draw_mark(buf: &mut Buffer, clip: Rect, rect: Rect, mark: &Mark, theme: &Theme, now: i64) {
    if rect.width < 8 || rect.height < 3 || !mark.is_live(now) {
        return;
    }
    let style = style(mark, theme);
    let inside = rect.intersection(clip).intersection(buf.area);
    // The label has the bottom border to itself: the model's name and the
    // progress bar give way for as long as it lives.
    let y = i32::from(rect.bottom()) - 1;
    let border = buf
        .cell((rect.x, rect.bottom() - 1))
        .map_or_else(Style::default, |c| c.style());
    put_str(
        buf,
        clip,
        i32::from(rect.x) + 1,
        y,
        &"─".repeat(usize::from(rect.width) - 2),
        border,
    );
    if let Some(t) = mark.sweep(now) {
        match mark.kind {
            MarkKind::Done => {
                let cells = perimeter(rect);
                let len = cells.len();
                let tail = (len / 6).max(4);
                let head = (t * len as f64) as usize;
                light(
                    buf,
                    (0..tail)
                        .map(|i| cells[(head + len - i) % len])
                        .filter(|(x, y)| inside.contains((*x, *y).into())),
                    style,
                );
            }
            _ => {
                let y = rect.bottom() - 1;
                let span = usize::from(rect.width) - 2;
                let head = (t * (span + 3) as f64) as usize;
                light(
                    buf,
                    (0..3)
                        .filter_map(|i| head.checked_sub(i))
                        .filter(|i| *i < span)
                        .map(|i| (rect.x + 1 + i as u16, y))
                        .filter(|(x, y)| inside.contains((*x, *y).into())),
                    style,
                );
            }
        }
    }
    // A space each side, the corners left alone.
    let room = usize::from(rect.width).saturating_sub(4);
    let label = fit(&text(mark, now), room);
    if label.is_empty() {
        return;
    }
    let x = i32::from(rect.x) + 1;
    put_str(buf, clip, x, y, " ", border);
    put_str(buf, clip, x + 1, y, &label, style);
    put_str(buf, clip, x + 1 + width(&label) as i32, y, " ", border);
}
