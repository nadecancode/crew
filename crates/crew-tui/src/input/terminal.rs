use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Command};

/// A key typed into the agent's terminal.
pub(super) fn typing_key(app: &mut App, key: KeyEvent, now: i64) -> Vec<Command> {
    if key.code == KeyCode::Char(']') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.detail.typing = false;
        return Vec::new();
    }
    let Some(text) = key_bytes(key) else {
        return Vec::new();
    };
    // The screen is read again right away, to show the key land.
    app.detail.output_at = None;
    app.detail.key_at = now;
    vec![Command::Keys {
        id: app.detail.agent.clone(),
        text,
    }]
}

/// What a terminal sends for a key: the character, or the usual escape
/// sequence for a special key. `None` for keys a terminal does not send.
pub fn key_bytes(key: KeyEvent) -> Option<String> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let shift = m.contains(KeyModifiers::SHIFT);
    // `CSI 1 ; n X` carries the modifiers of a cursor key.
    let mods = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let cursor = |letter: char| {
        if mods == 1 {
            format!("\x1b[{letter}")
        } else {
            format!("\x1b[1;{mods}{letter}")
        }
    };
    let tilde = |code: u8| {
        if mods == 1 {
            format!("\x1b[{code}~")
        } else {
            format!("\x1b[{code};{mods}~")
        }
    };
    Some(match key.code {
        // Alt+Enter is what Claude Code takes as a line break.
        KeyCode::Enter if shift || alt => "\x1b\r".to_string(),
        KeyCode::Enter => "\r".to_string(),
        KeyCode::Tab => "\t".to_string(),
        KeyCode::BackTab => "\x1b[Z".to_string(),
        KeyCode::Backspace => "\x7f".to_string(),
        KeyCode::Esc => "\x1b".to_string(),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char),
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
        KeyCode::Char(c) if ctrl => {
            let byte = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                ' ' | '@' => 0,
                '[' => 0x1b,
                '\\' => 0x1c,
                ']' => 0x1d,
                '^' => 0x1e,
                '_' => 0x1f,
                _ => return None,
            };
            (byte as char).to_string()
        }
        KeyCode::Char(c) if alt => format!("\x1b{c}"),
        KeyCode::Char(c) => c.to_string(),
        _ => return None,
    })
}
