//! Terminal text with its colours: a line that carries SGR escape codes
//! (`ESC [ … m`) becomes styled spans, so the live view of an agent's
//! terminal looks like the terminal does.
//!
//! Every other escape sequence is dropped, except the cursor-forward one
//! (`ESC [ n C`), which backends use to skip blank cells and so stands for
//! that many spaces.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// `text` as one styled line. `base` is the style of text that has none of
/// its own; a reset (`ESC [ 0 m`) goes back to it.
pub fn line(text: &str, base: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut style = base;
    let flush = |run: &mut String, spans: &mut Vec<Span<'static>>, style: Style| {
        if !run.is_empty() {
            spans.push(Span::styled(std::mem::take(run), style));
        }
    };
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut end = None;
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            end = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    match end {
                        Some('m') => {
                            flush(&mut run, &mut spans, style);
                            style = sgr(&params, style, base);
                        }
                        Some('C') => {
                            let n = params.parse::<usize>().unwrap_or(1).clamp(1, 512);
                            run.extend(std::iter::repeat_n(' ', n));
                        }
                        _ => {}
                    }
                }
                Some(']') => {
                    // An operating-system command runs to BEL or ESC \.
                    chars.next();
                    let mut last = '\0';
                    for c in chars.by_ref() {
                        if c == '\x07' || (last == '\x1b' && c == '\\') {
                            break;
                        }
                        last = c;
                    }
                }
                // A charset designation carries one more byte.
                Some('(' | ')' | '*' | '+' | '#' | '%') => {
                    chars.next();
                    chars.next();
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\t' => run.push(' '),
            c if c.is_control() => {}
            c => run.push(c),
        }
    }
    flush(&mut run, &mut spans, style);
    Line::from(spans)
}

/// The 16 ANSI colours, by their SGR code.
fn ansi_color(code: u16) -> Option<Color> {
    Some(match code {
        30 | 40 => Color::Black,
        31 | 41 => Color::Red,
        32 | 42 => Color::Green,
        33 | 43 => Color::Yellow,
        34 | 44 => Color::Blue,
        35 | 45 => Color::Magenta,
        36 | 46 => Color::Cyan,
        37 | 47 => Color::Gray,
        90 | 100 => Color::DarkGray,
        91 | 101 => Color::LightRed,
        92 | 102 => Color::LightGreen,
        93 | 103 => Color::LightYellow,
        94 | 104 => Color::LightBlue,
        95 | 105 => Color::LightMagenta,
        96 | 106 => Color::LightCyan,
        97 | 107 => Color::White,
        _ => return None,
    })
}

/// Apply the parameters of one SGR sequence to `style`.
fn sgr(params: &str, mut style: Style, base: Style) -> Style {
    let codes: Vec<u16> = params
        .split([';', ':'])
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let mut i = 0;
    while i < codes.len() {
        let code = codes[i];
        match code {
            0 => style = base,
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            7 => style = style.add_modifier(Modifier::REVERSED),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style.add_modifier.remove(Modifier::BOLD | Modifier::DIM),
            23 => style.add_modifier.remove(Modifier::ITALIC),
            24 => style.add_modifier.remove(Modifier::UNDERLINED),
            27 => style.add_modifier.remove(Modifier::REVERSED),
            29 => style.add_modifier.remove(Modifier::CROSSED_OUT),
            38 | 48 => {
                // 38;5;n and 38;2;r;g;b.
                let color = match codes.get(i + 1) {
                    Some(5) => {
                        i += 2;
                        codes.get(i).map(|n| Color::Indexed((*n).min(255) as u8))
                    }
                    Some(2) => {
                        i += 4;
                        match (codes.get(i - 2), codes.get(i - 1), codes.get(i)) {
                            (Some(r), Some(g), Some(b)) => {
                                Some(Color::Rgb(*r as u8, *g as u8, *b as u8))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(color) = color {
                    style = if code == 38 {
                        style.fg(color)
                    } else {
                        style.bg(color)
                    };
                }
            }
            39 => style.fg = base.fg,
            49 => style.bg = base.bg,
            30..=37 | 90..=97 => style.fg = ansi_color(code),
            40..=47 | 100..=107 => style.bg = ansi_color(code),
            _ => {}
        }
        i += 1;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(text: &str) -> Vec<(String, Style)> {
        line(text, Style::default())
            .spans
            .into_iter()
            .map(|s| (s.content.into_owned(), s.style))
            .collect()
    }

    #[test]
    fn colours_and_attributes_become_spans() {
        let text = "plain \x1b[1;31mbold red\x1b[0m \x1b[38;5;208mindexed\x1b[39m \x1b[38;2;10;20;30mrgb\x1b[m end";
        let got = parts(text);
        assert_eq!(got[0], ("plain ".into(), Style::default()));
        assert_eq!(
            got[1],
            (
                "bold red".into(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            )
        );
        assert_eq!(got[2], (" ".into(), Style::default()));
        assert_eq!(
            got[3],
            ("indexed".into(), Style::default().fg(Color::Indexed(208)))
        );
        assert_eq!(
            got[5],
            ("rgb".into(), Style::default().fg(Color::Rgb(10, 20, 30)))
        );
        assert_eq!(got[6], (" end".into(), Style::default()));
        // Dim, background and bright colours; 22 lifts bold and dim.
        let got = parts("\x1b[2;44mdim on blue\x1b[22;49;97m white");
        assert_eq!(
            got[0],
            (
                "dim on blue".into(),
                Style::default().bg(Color::Blue).add_modifier(Modifier::DIM)
            )
        );
        assert_eq!(got[1], (" white".into(), Style::default().fg(Color::White)));
    }

    #[test]
    fn other_sequences_and_control_characters_are_dropped() {
        let got = parts("a\x1b[2Kb\x1b]0;title\x07c\x1b]8;;http://x\x1b\\d\x1b(Be\x07f\tg");
        assert_eq!(got, [("abcdef g".to_string(), Style::default())]);
        // Cursor-forward stands for blank cells; a reset returns to the base.
        let base = Style::default().fg(Color::Gray);
        let line = line("x\x1b[3Cy\x1b[31mz\x1b[0mw", base);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "x   yzw");
        assert_eq!(line.spans[0].style, base);
        assert_eq!(line.spans[2].style, base);
        // Nothing styled: one span, or none for an empty line.
        assert_eq!(parts("hello").len(), 1);
        assert!(parts("").is_empty());
        assert!(parts("\x1b[").is_empty());
    }
}
