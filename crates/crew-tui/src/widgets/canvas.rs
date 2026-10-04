//! Clipped drawing primitives on a ratatui buffer. Coordinates are signed so
//! geometry code can compute freely; anything outside the clip is dropped.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use unicode_width::UnicodeWidthChar;

/// Write `s` starting at (`x`, `y`), dropping whatever falls outside `clip`.
pub fn put_str(buf: &mut Buffer, clip: Rect, x: i32, y: i32, s: &str, style: Style) {
    let clip = clip.intersection(buf.area);
    if clip.is_empty() || y < i32::from(clip.y) || y >= i32::from(clip.bottom()) {
        return;
    }
    let mut cx = x;
    for ch in s.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0) as i32;
        if w == 0 {
            continue;
        }
        if cx >= i32::from(clip.x) && cx + w <= i32::from(clip.right()) {
            let cell = &mut buf[(cx as u16, y as u16)];
            cell.set_char(ch);
            cell.set_style(style);
            for extra in 1..w {
                buf[((cx + extra) as u16, y as u16)].reset();
            }
        }
        cx += w;
        if cx >= i32::from(clip.right()) {
            break;
        }
    }
}

pub fn put_cell(buf: &mut Buffer, clip: Rect, x: i32, y: i32, symbol: &str, style: Style) {
    put_str(buf, clip, x, y, symbol, style);
}

/// Fill a whole row of `area` with a style (used for bars).
pub fn fill_row(buf: &mut Buffer, area: Rect, y: u16, style: Style) {
    let area = area.intersection(buf.area);
    if y < area.y || y >= area.bottom() {
        return;
    }
    for x in area.x..area.right() {
        let cell = &mut buf[(x, y)];
        cell.set_char(' ');
        cell.set_style(style);
    }
}

/// Render a buffer as plain text, one line per row (tests and the preview).
pub fn buffer_text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.y..area.bottom() {
        let mut line = String::new();
        for x in area.x..area.right() {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
