//! Small helpers shared across crates: time, identifiers, validation.

use std::borrow::Cow;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `HH:MM:SS.mmm` in local time, the format used by the message bus log.
pub fn fmt_clock(ts_ms: i64) -> String {
    match Local.timestamp_millis_opt(ts_ms).single() {
        Some(t) => t.format("%H:%M:%S%.3f").to_string(),
        None => "--:--:--.---".to_string(),
    }
}

/// A time for people: `3:19 PM`, with the date when it is not today.
pub fn fmt_when(ts_ms: i64, now_ms: i64) -> String {
    let (Some(at), Some(now)) = (
        Local.timestamp_millis_opt(ts_ms).single(),
        Local.timestamp_millis_opt(now_ms).single(),
    ) else {
        return "-".to_string();
    };
    if at.date_naive() == now.date_naive() {
        at.format("%-I:%M %p").to_string()
    } else {
        at.format("%b %-d, %-I:%M %p").to_string()
    }
}

/// `YYYY-MM-DD HH:MM:SS` in local time.
pub fn fmt_datetime(ts_ms: i64) -> String {
    match Local.timestamp_millis_opt(ts_ms).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => "-".to_string(),
    }
}

/// Compact elapsed time such as `42s`, `3m07s`, `2h14m`.
pub fn fmt_elapsed(ms: i64) -> String {
    let s = (ms.max(0)) / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Agent and task identifiers: `[a-z][a-z0-9_-]{0,31}`.
///
/// The pattern matches herdr's agent-name rule so an agent id can be used
/// verbatim as a herdr agent name, a git branch component and a directory name.
pub fn is_valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// A random, unguessable token (244 bits) used to authenticate agents.
pub fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// A short unique id with a prefix, e.g. `m-3f9a1c2e4b5d`.
pub fn new_id(prefix: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &u[..12])
}

/// Constant-time string comparison for tokens.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// FNV-1a, used to derive stable short names (socket files) from paths.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Truncate to at most `max` characters, appending `…` when cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Text safe to write to a terminal cell: drop controls and expand tabs.
pub fn printable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .filter_map(|ch| {
                if ch == '\t' {
                    Some(' ')
                } else if ch.is_control() {
                    None
                } else {
                    Some(ch)
                }
            })
            .collect(),
    )
}

/// Collapse a body to a single display line.
pub fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() || ch.is_control() {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out.trim_end().to_string()
}

/// The slash command a line of operator input opens with, as `(name, rest)`:
/// `/kill claude-2` is `("kill", "claude-2")`.
///
/// Only a line whose very first character is the slash counts, and only when
/// a plain word follows it, so a path (`/src/main.rs is slow`) is ordinary
/// text, and so is anything typed after a leading space. Whether the name is
/// a command that exists is the caller's business: text that looks like a
/// command must never reach an agent as an objective or a message.
pub fn slash_command(text: &str) -> Option<(&str, &str)> {
    let line = text.strip_prefix('/')?;
    let end = line.find(char::is_whitespace).unwrap_or(line.len());
    let (name, rest) = line.split_at(end);
    let mut chars = name.chars();
    let word = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    (word || name == "?").then(|| (name, rest.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands_are_told_from_text() {
        assert_eq!(slash_command("/exit"), Some(("exit", "")));
        assert_eq!(
            slash_command("/kill  claude-2 \n"),
            Some(("kill", "claude-2"))
        );
        assert_eq!(
            slash_command("/Stop now please"),
            Some(("Stop", "now please"))
        );
        assert_eq!(slash_command("/?"), Some(("?", "")));
        assert_eq!(slash_command("/exit\nand more"), Some(("exit", "and more")));
        // Paths, text after a space, and everything else are plain text.
        for text in [
            "",
            "/",
            "/ exit",
            " /exit",
            "//exit",
            "/src/main.rs is slow",
            "/etc/hosts",
            "/2fast",
            "/exit.",
            "exit",
            "fix /exit handling",
        ] {
            assert_eq!(slash_command(text), None, "{text:?}");
        }
    }

    #[test]
    fn ids() {
        assert!(is_valid_id("claude-1"));
        assert!(is_valid_id("orchestrator"));
        assert!(is_valid_id("a"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("1abc"));
        assert!(!is_valid_id("Claude"));
        assert!(!is_valid_id("a b"));
        assert!(!is_valid_id("../etc"));
        assert!(!is_valid_id(&"a".repeat(33)));
    }

    #[test]
    fn tokens_are_unique_and_compared_exactly() {
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert!(ct_eq(&a, &a.clone()));
        assert!(!ct_eq(&a, &b));
        assert!(!ct_eq(&a, &a[..10]));
    }

    #[test]
    fn display_helpers() {
        assert_eq!(fmt_elapsed(42_000), "42s");
        assert_eq!(fmt_elapsed(187_000), "3m07s");
        assert_eq!(fmt_elapsed(8_040_000), "2h14m");
        assert_eq!(one_line("a\n  b\tc\n"), "a b c");
        assert_eq!(truncate_chars("hello world", 6), "hello…");
        assert_eq!(truncate_chars("hi", 6), "hi");
    }
}
