//! Claude Code: its usage cache and `StopFailure` hook, its transcript, and the
//! limit messages it prints.

use crew_core::util::{one_line, truncate_chars};
use serde_json::Value;

use super::{
    finite_percent, last_limit_on_screen, parse_reset, LimitInfo, LimitKind, QuotaReading,
    TurnFailure,
};

/// The rate-limit windows Claude Code's engine reports to the crew mod
/// (`[{ kind, percentUsed, resetsAt }]`, as its status line has them).
pub fn claude_quota(payload: &Value) -> Vec<QuotaReading> {
    let Some(windows) = payload.get("rate_limits").and_then(Value::as_array) else {
        return Vec::new();
    };
    windows
        .iter()
        .filter_map(|w| {
            let window = match w.get("kind")?.as_str()? {
                "five_hour" => "5h".to_string(),
                "seven_day" => "week".to_string(),
                "spend_limit" => "spend".to_string(),
                other => other.replace('_', " "),
            };
            Some(QuotaReading {
                window,
                percent_used: finite_percent(w.get("percentUsed")?)?,
                resets_at: w
                    .get("resetsAt")
                    .and_then(Value::as_str)
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.timestamp_millis()),
            })
        })
        .collect()
}

/// Claude Code's own usage cache: `<config dir>/.claude.json`, where it keeps
/// what the account's usage page last said (`cachedUsageUtilization`).
pub fn claude_usage_cache_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME"))?;
    Some(std::path::PathBuf::from(dir).join(".claude.json"))
}

/// The account's windows from Claude Code's usage cache, and when Claude
/// Code fetched them. Besides the session and the all-models weekly window
/// (which sessions also report, live), this is the one place a model's own
/// weekly window is: `week/<model>`.
pub fn claude_cached_quota(text: &str) -> Option<(i64, Vec<QuotaReading>)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let cache = value.get("cachedUsageUtilization")?;
    let fetched_at = cache.get("fetchedAtMs")?.as_i64()?;
    let limits = cache
        .get("limits")
        .or_else(|| cache.pointer("/utilization/limits"))?
        .as_array()?;
    let readings = limits
        .iter()
        .filter_map(|limit| {
            let window = match limit.get("kind")?.as_str()? {
                "session" => "5h".to_string(),
                "weekly_all" => "week".to_string(),
                "weekly_scoped" => {
                    let model = limit.pointer("/scope/model/display_name")?.as_str()?.trim();
                    if model.is_empty() {
                        return None;
                    }
                    format!("week/{model}")
                }
                _ => return None,
            };
            Some(QuotaReading {
                window,
                percent_used: finite_percent(limit.get("percent")?)?,
                resets_at: limit
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.timestamp_millis()),
            })
        })
        .collect();
    Some((fetched_at, readings))
}

/// What a Claude Code session's transcript says about it, from its newest
/// reply on the main thread: the model, the reasoning effort, and how many
/// tokens that reply was answered over (the fill of the context window);
/// and the newest word on the session being moved to another model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionFacts {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub context_tokens: Option<u64>,
    pub switch: Option<ModelSwitch>,
}

/// Claude Code's note on moving the session to another model: when its
/// model's usage window is used up (`model_consent_fallback`), when it is
/// overloaded or unavailable (`model_fallback`), or when it refused and
/// another model took the session over (`model_refusal_fallback`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelSwitch {
    pub from: String,
    pub to: String,
    /// The note as Claude Code shows it.
    pub note: String,
}

/// The newest system line of the transcript about a model switch that
/// changed the session's model.
pub fn claude_model_switch(transcript: &str) -> Option<ModelSwitch> {
    transcript
        .lines()
        .rev()
        .filter(|line| line.contains("_fallback\""))
        .find_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            if value.get("type")?.as_str()? != "system" {
                return None;
            }
            match value.get("subtype")?.as_str()? {
                "model_fallback" | "model_consent_fallback" => {}
                // A refusal answered by another model only for that reply
                // leaves the session's model as it was.
                "model_refusal_fallback"
                    if value.get("scope").and_then(Value::as_str) != Some("local") => {}
                _ => return None,
            }
            let text = |key: &str| {
                value
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let (from, to) = (text("originalModel"), text("fallbackModel"));
            if from.is_empty() || to.is_empty() {
                return None;
            }
            Some(ModelSwitch {
                from,
                to,
                note: text("content"),
            })
        })
}

pub fn claude_session_facts(transcript: &str) -> Option<SessionFacts> {
    let switch = claude_model_switch(transcript);
    transcript
        .lines()
        .rev()
        .filter(|line| line.contains("\"assistant\""))
        .find_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            let main_thread = value.get("isSidechain").and_then(Value::as_bool) != Some(true);
            if value.get("type")?.as_str()? != "assistant" || !main_thread {
                return None;
            }
            let message = value.get("message")?;
            let usage = message.get("usage")?;
            let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
            let tokens = count("input_tokens")
                + count("cache_read_input_tokens")
                + count("cache_creation_input_tokens");
            let text = |v: Option<&Value>| {
                v.and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            Some(SessionFacts {
                // A reply crew or the CLI made up carries no real model.
                model: text(message.get("model")).filter(|m| !m.starts_with('<')),
                effort: text(value.get("effort")),
                context_tokens: (tokens > 0).then_some(tokens),
                switch: switch.clone(),
            })
        })
}

/// The transcript Claude Code keeps for `session`:
/// `<config dir>/projects/<project>/<session>.jsonl`.
pub fn claude_transcript_path(session: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".claude"))
        })?;
    claude_transcript_in(&home, session)
}

pub(super) fn claude_transcript_in(
    home: &std::path::Path,
    session: &str,
) -> Option<std::path::PathBuf> {
    // A session id is a plain name, never a path.
    if session.is_empty()
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let file = format!("{session}.jsonl");
    std::fs::read_dir(home.join("projects"))
        .ok()?
        .flatten()
        .map(|project| project.path().join(&file))
        .find(|path| path.is_file())
}

/// Classify the payload of Claude Code's `StopFailure` hook.
pub fn claude_stop_failure(payload: &Value, now_ms: i64) -> TurnFailure {
    let field = |name: &str| {
        payload
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
    };
    let error = field("error");
    let said = [field("error_details"), field("last_assistant_message")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let message = if said.is_empty() {
        error.replace('_', " ")
    } else {
        said.clone()
    };
    let message = truncate_chars(&one_line(&message), 300);
    let kind = match error {
        "rate_limit" => LimitKind::Usage,
        "billing_error" => LimitKind::Credits,
        "authentication_failed"
        | "oauth_org_not_allowed"
        | "account_on_hold"
        | "verification_required"
        | "cloud_credential_error" => LimitKind::Auth,
        other => {
            let what = if other.is_empty() {
                "unknown error"
            } else {
                other
            };
            return TurnFailure::Other(format!("{}: {message}", what.replace('_', " ")));
        }
    };
    TurnFailure::Limited(LimitInfo {
        kind,
        resets_at: parse_reset(&said, now_ms),
        message,
    })
}

/// A usage limit shown in a Claude Code session (a fallback: the
/// `StopFailure` hook normally reports it first).
pub fn claude_screen(screen: &str, now_ms: i64) -> Option<LimitInfo> {
    last_limit_on_screen(
        screen,
        |line| {
            // The message is the start of the line, after Claude's own marker;
            // text quoting the phrase mid-sentence, or echoed after the prompt
            // marker, is not the error.
            let line = line.trim_start_matches(['●', '⏺', ' ']);
            if line.starts_with("credit balance is too low") {
                Some(LimitKind::Credits)
            } else if (line.starts_with("you've hit your") && line.contains("limit"))
                || line.starts_with("usage limit reached")
                || line.starts_with("claude usage limit reached")
                || (line.starts_with("api error")
                    && (line.contains("(429)") || line.contains("rate limit")))
            {
                Some(LimitKind::Usage)
            } else {
                None
            }
        },
        // An answer, or a prompt marker followed by text: a message that
        // was sent after the limit message.
        |line| line.starts_with('●') || line.starts_with('⏺') || claude_prompt(line) == Some(true),
        // The input box: a prompt marker with nothing after it.
        |line| claude_prompt(line) == Some(false),
        now_ms,
    )
}

/// For a line that starts with Claude Code's prompt marker: whether text
/// follows it.
fn claude_prompt(line: &str) -> Option<bool> {
    let rest = line.strip_prefix('❯')?;
    Some(!rest.trim_matches([' ', '\u{a0}']).is_empty())
}
