//! pi: what the provider said when a turn failed, and the limit messages on its
//! screen.

use crew_core::util::{one_line, truncate_chars};
use serde_json::Value;

use super::{classify_error, last_limit_on_screen, parse_reset, LimitInfo, TurnFailure};

/// A pi turn that ended in an error, as the crew extension reports it:
/// `error` and the provider's message in `error_details`.
pub fn pi_turn_error(payload: &Value, now_ms: i64) -> TurnFailure {
    let field = |name: &str| {
        payload
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
    };
    let details = field("error_details");
    let message = if details.is_empty() {
        field("error")
    } else {
        details
    };
    let message = truncate_chars(&one_line(message), 300);
    match classify_error(&message) {
        Some(kind) => TurnFailure::Limited(LimitInfo {
            kind,
            resets_at: parse_reset(&message, now_ms),
            message,
        }),
        None => TurnFailure::Other(if message.is_empty() {
            "unknown error".to_string()
        } else {
            message
        }),
    }
}

/// A usage limit shown in a pi session (a fallback: the crew extension
/// normally reports a failed turn first). pi prints a provider's error as
/// it came, in a line of its own.
pub fn pi_screen(screen: &str, now_ms: i64) -> Option<LimitInfo> {
    last_limit_on_screen(
        screen,
        |line| {
            let looks_like_error =
                line.contains("error") || line.contains("limit") || line.contains("429");
            looks_like_error.then(|| classify_error(line)).flatten()
        },
        // pi's input box is a bordered line; anything after an error that
        // is not the box is the conversation going on.
        |line| line.starts_with('●') || line.starts_with('⏺'),
        |line| line.starts_with('>') || line.starts_with('│') || line.starts_with('─'),
        now_ms,
    )
}
