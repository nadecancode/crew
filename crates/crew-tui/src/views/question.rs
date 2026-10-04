//! Questions the team has for the operator: the band that says one is
//! waiting, and the box it is read and answered in.
//!
//! The box shows the whole question (wrapped, scrollable), who wants to
//! know, the answers the asker suggests, and a line for the operator's own
//! words.

use crew_core::types::Message;
use crew_core::util::{fmt_elapsed, one_line};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Widget};
use unicode_width::UnicodeWidthChar;

use super::compose::input_line;
use super::help::centered;
use crate::app::{App, QuestionBox};
use crate::theme::Theme;
use crate::widgets::canvas::{fill_row, put_str};
use crate::widgets::text::{fit, width};

/// Keys of the answer box, shown under it.
const KEYS: &str = "↑↓ choose · enter send · tab next question · pgup/pgdn scroll · esc later";

/// Rows the band takes at the top of the dashboard.
pub fn band_height(app: &App, area: Rect) -> u16 {
    u16::from(!app.open_questions().is_empty() && area.height >= 8 && area.width >= 30)
}

/// How long ago `question` was asked, on the daemon's clock.
fn age(app: &App, question: &Message, now: i64) -> String {
    fmt_elapsed((now + app.clock_offset - question.timestamp).max(0))
}

/// `orchestrator`, or `orchestrator (for claude-2)` for a question passed on.
fn asker(question: &Message) -> String {
    match question.asked_by() {
        Some(member) => format!("{} (for {member})", question.from),
        None => question.from.clone(),
    }
}

/// One row: who is asking what, and the key that opens the answer box.
pub fn render_band(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    let open = app.open_questions();
    let Some(first) = open.first() else {
        return;
    };
    let bar = Style::default().bg(theme.bar_bg);
    fill_row(buf, area, area.y, bar);
    let x = i32::from(area.x) + 1;
    let y = i32::from(area.y);
    let cols = usize::from(area.width).saturating_sub(2);

    let key = "a answer";
    let lead = match open.len() {
        1 => format!("? question from {} · {} ago: ", asker(first), age(app, first, now)),
        n => format!("? {n} questions for you · the first from {}: ", asker(first)),
    };
    let lead = fit(&lead, cols);
    let style = theme.bold(theme.warn).bg(theme.bar_bg);
    put_str(buf, area, x, y, &lead, style);
    let room = cols.saturating_sub(width(&lead) + width(key) + 2);
    put_str(
        buf,
        area,
        x + width(&lead) as i32,
        y,
        &fit(&one_line(&first.body), room),
        bar.fg(theme.text),
    );
    if cols > width(&lead) + width(key) + 2 {
        let key_x = i32::from(area.right()) - 1 - width(key) as i32;
        put_str(buf, area, key_x, y, key, style);
    }
}

/// Break `text` into lines of at most `cols` columns, at spaces where it
/// can. Line breaks in the text are kept.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        let mut used = 0;
        for word in paragraph.split_whitespace() {
            let mut word = word;
            let space = usize::from(!line.is_empty());
            if used + space + width(word) > cols && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            } else if space == 1 {
                line.push(' ');
                used += 1;
            }
            // A word longer than a whole line is cut where the line ends.
            while width(word) > cols - used {
                let mut taken = 0;
                let mut cut = 0;
                for (at, ch) in word.char_indices() {
                    let w = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if taken + w > cols - used {
                        break;
                    }
                    taken += w;
                    cut = at + ch.len_utf8();
                }
                if cut == 0 {
                    // Not even one character fits what is left of the line.
                    if line.is_empty() {
                        cut = word.chars().next().map_or(word.len(), char::len_utf8);
                    } else {
                        lines.push(std::mem::take(&mut line));
                        used = 0;
                        continue;
                    }
                }
                line.push_str(&word[..cut]);
                lines.push(std::mem::take(&mut line));
                used = 0;
                word = &word[cut..];
            }
            line.push_str(word);
            used += width(word);
        }
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn frame(buf: &mut Buffer, rect: Rect, title: &str, theme: &Theme) {
    Clear.render(rect, buf);
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme.fg(theme.warn))
        .title(Span::styled(format!(" {title} "), theme.bold(theme.text)))
        .render(rect, buf);
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, answer: &QuestionBox, theme: &Theme, now: i64) {
    let open = app.open_questions();
    let outer = centered(area, area.width.saturating_sub(6).clamp(24, 104), area.height);
    if outer.width < 24 || area.height < 7 {
        return;
    }
    let cols = usize::from(outer.width) - 4;
    let Some(index) = open.iter().position(|q| q.id == answer.id) else {
        // Answered from somewhere else while the box was open.
        let rect = centered(area, outer.width, 5);
        frame(buf, rect, "question", theme);
        let x = i32::from(rect.x) + 2;
        let y = i32::from(rect.y) + 1;
        put_str(buf, rect, x, y, &fit("This question has been answered.", cols), theme.fg(theme.text));
        put_str(buf, rect, x, y + 2, &fit("any key closes", cols), theme.fg(theme.dim));
        return;
    };
    let question = open[index];
    let options = question.options();
    let own = options.len();
    let choice = answer.choice.min(own);

    // Everything but the question has a fixed height; the question gets
    // the rest and scrolls when it is longer.
    let lines = wrap(&question.body, cols);
    let fixed = 2 + 1 + (own + 1) + 1 + 1;
    let visible = lines
        .len()
        .min(usize::from(area.height).saturating_sub(fixed + 2).max(1));
    let rect = centered(area, outer.width, (visible + fixed) as u16);
    let counter = if open.len() > 1 {
        format!("question {} of {}", index + 1, open.len())
    } else {
        "question".to_string()
    };
    let title = format!("{counter} · {} · {} ago", question.asker(), age(app, question, now));
    frame(buf, rect, &fit(&title, cols), theme);

    let x = i32::from(rect.x) + 2;
    let mut y = i32::from(rect.y) + 1;
    let scroll = answer.scroll.min(lines.len() - visible.min(lines.len()));
    for (row, line) in lines.iter().skip(scroll).take(visible).enumerate() {
        // The last row says so when there is more below.
        let more = lines.len() - scroll - visible;
        if row + 1 == visible && more > 0 {
            let note = format!("↓ {} more line(s) · pgdn", more + 1);
            put_str(buf, rect, x, y, &fit(&note, cols), theme.fg(theme.dim));
        } else {
            put_str(buf, rect, x, y, line, theme.fg(theme.text));
        }
        y += 1;
    }
    y += 1;

    for (i, option) in options.iter().enumerate() {
        let selected = i == choice;
        let marker = if selected { "▸" } else { " " };
        let style = if selected { theme.bold(theme.accent) } else { theme.fg(theme.text) };
        put_str(buf, rect, x, y, &fit(&format!("{marker} {}. {option}", i + 1), cols), style);
        y += 1;
    }
    // The operator's own words.
    let selected = choice == own;
    let lead = format!("{} {}. › ", if selected { "▸" } else { " " }, own + 1);
    let style = if selected { theme.bold(theme.accent) } else { theme.fg(theme.muted) };
    put_str(buf, rect, x, y, &lead, style);
    let text_x = x + width(&lead) as i32;
    let room = cols.saturating_sub(width(&lead));
    if answer.input.text.is_empty() && !selected {
        put_str(buf, rect, text_x, y, &fit("type your own answer", room), theme.fg(theme.dim));
    } else {
        input_line(
            buf,
            rect,
            (text_x, y),
            room,
            &answer.input.text,
            selected.then_some(answer.input.cursor),
            theme,
        );
    }
    y += 2;

    let (hint, style) = match &answer.input.notice {
        Some(notice) => (notice.as_str(), theme.fg(theme.warn)),
        None => (KEYS, theme.fg(theme.dim)),
    };
    put_str(buf, rect, x, y, &fit(hint, cols), style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_wraps_at_spaces_and_keeps_line_breaks() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("one two three four", 100), ["one two three four"]);
        assert_eq!(wrap("first\n\nsecond line", 20), ["first", "", "second line"]);
        // Words longer than a line are cut; wide characters count double.
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab cdefghij", 4), ["ab", "cdef", "ghij"]);
        assert_eq!(wrap("日本語のテキスト", 6), ["日本語", "のテキ", "スト"]);
        assert_eq!(wrap("a 日本", 3), ["a", "日", "本"]);
        assert!(wrap("", 10).is_empty());
        assert!(wrap("  \n ", 10).is_empty());
        for cols in 1..30 {
            let lines = wrap("The legacy export writes amounts as integer cents; 日本語 too.", cols);
            assert!(lines.iter().all(|l| width(l) <= cols.max(2)), "{cols}: {lines:?}");
        }
    }
}
