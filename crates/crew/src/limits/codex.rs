//! Codex: its rollout files (quota, context window, turn state) and the limit
//! messages it prints.

use chrono::{Local, TimeZone};
use serde_json::Value;

use super::{
    finite_percent, last_limit_on_screen, window_name, LimitInfo, LimitKind, QuotaReading,
};

/// The newest rate-limit reading in the text of a Codex session file. Codex
/// records one with every token count (`payload.rate_limits`, with a
/// `primary` and a `secondary` window).
pub fn codex_quota(rollout: &str) -> Vec<QuotaReading> {
    let Some(limits) = rollout
        .lines()
        .rev()
        .filter(|line| line.contains("\"rate_limits\""))
        .find_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            let limits = value.pointer("/payload/rate_limits")?.clone();
            // A session on another endpoint records the field with no windows.
            (limits.get("primary").is_some_and(|w| !w.is_null())
                || limits.get("secondary").is_some_and(|w| !w.is_null()))
            .then_some(limits)
        })
    else {
        return Vec::new();
    };
    ["primary", "secondary"]
        .iter()
        .filter_map(|name| {
            let w = limits.get(name)?;
            Some(QuotaReading {
                window: window_name(w.get("window_minutes")?.as_i64()?),
                percent_used: finite_percent(w.get("used_percent")?)?,
                resets_at: w
                    .get("resets_at")
                    .and_then(Value::as_i64)
                    .map(|secs| secs * 1000),
            })
        })
        .collect()
}

/// How full the context window is, from the text of a Codex session file:
/// `(tokens, window)` of the newest token count. Codex records one after
/// every model response and after a compaction.
pub fn codex_context(rollout: &str) -> Option<(u64, u64)> {
    rollout
        .lines()
        .rev()
        .filter(|line| line.contains("\"model_context_window\""))
        .find_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            let info = value.pointer("/payload/info")?;
            let window = info.get("model_context_window")?.as_u64()?;
            let tokens = info.pointer("/last_token_usage/total_tokens")?.as_u64()?;
            (window > 0).then_some((tokens, window))
        })
}

/// What a Codex session runs on, from the text of its session file: the
/// `(model, effort)` of the newest turn. Codex records both with every turn.
pub fn codex_running(rollout: &str) -> Option<(Option<String>, Option<String>)> {
    rollout
        .lines()
        .rev()
        .filter(|line| line.contains("\"turn_context\""))
        .find_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            if value.get("type")?.as_str()? != "turn_context" {
                return None;
            }
            let field = |key: &str| {
                value
                    .pointer(&format!("/payload/{key}"))
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
            };
            let running = (field("model"), field("effort"));
            (running.0.is_some() || running.1.is_some()).then_some(running)
        })
}

/// `codex_running` over the whole session file: the turn that is running
/// may have begun further back than any tail worth reading.
pub fn codex_running_in(path: &std::path::Path) -> Option<(Option<String>, Option<String>)> {
    codex_running_of(std::fs::File::open(path).ok()?)
}

/// `codex_running_in` for a file already open.
pub fn codex_running_of(mut file: std::fs::File) -> Option<(Option<String>, Option<String>)> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut newest = None;
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // The entry's type is near its start; most entries are something else.
        let head = String::from_utf8_lossy(&line[..line.len().min(200)]);
        if head.contains("\"type\":\"turn_context\"") {
            if let Some(running) = codex_running(&String::from_utf8_lossy(&line)) {
                newest = Some(running);
            }
        }
    }
    newest
}

/// The latest turn of a Codex session, once it has ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexTurnEnd {
    pub turn_id: String,
    /// When it ended (ms; Codex records whole seconds).
    pub ended_at: Option<i64>,
    /// The error it ended in: Codex's kind for it (`codex_error_info`:
    /// `cyber_policy`, `server_overloaded`, `usage_limit_exceeded`, …) and
    /// what it said.
    pub error: Option<(String, String)>,
    /// What the turn was given: the text of each input.
    pub input: Vec<String>,
}

/// How the latest turn in the text of a Codex session file ended (`None`
/// while one runs). Codex records `task_started` and `task_complete` (or
/// `turn_aborted`) around every turn, a turn that failed included; its
/// `notify` hook is only called for one that completed.
pub fn codex_last_turn(rollout: &str) -> Option<CodexTurnEnd> {
    let mut end: Option<CodexTurnEnd> = None;
    let mut input = Vec::new();
    for line in rollout.lines().rev() {
        let marked = ["\"task_complete\"", "\"turn_aborted\"", "\"task_started\""]
            .iter()
            .any(|mark| line.contains(mark));
        let given = end.is_some()
            && (line.contains("\"role\":\"user\"") || line.contains("\"user_message\""));
        if !marked && !given {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(payload) = value.get("payload") else {
            continue;
        };
        let kind = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match (value.get("type").and_then(Value::as_str), kind) {
            (Some("event_msg"), "task_complete" | "turn_aborted") => {
                if end.is_some() {
                    break;
                }
                let error = payload.get("error").filter(|e| !e.is_null()).map(|e| {
                    // A kind with details is an object keyed by the kind.
                    let kind = match e.get("codex_error_info") {
                        Some(Value::String(kind)) => kind.clone(),
                        Some(Value::Object(map)) => map.keys().next().cloned().unwrap_or_default(),
                        _ => String::new(),
                    };
                    let message = e.get("message").and_then(Value::as_str).unwrap_or_default();
                    (kind, message.to_string())
                });
                end = Some(CodexTurnEnd {
                    turn_id: payload
                        .get("turn_id")
                        .and_then(Value::as_str)
                        .or_else(|| value.get("timestamp").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string(),
                    ended_at: payload
                        .get("completed_at")
                        .and_then(Value::as_i64)
                        .map(|secs| secs * 1000),
                    error,
                    input: Vec::new(),
                });
            }
            (Some("event_msg"), "task_started") => {
                end.as_ref()?;
                break;
            }
            (Some("event_msg"), "user_message") => {
                if let Some(text) = payload.get("message").and_then(Value::as_str) {
                    input.push(text.to_string());
                }
            }
            (Some("response_item"), "message")
                if payload.get("role").and_then(Value::as_str) == Some("user") =>
            {
                let texts = payload
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten();
                input.extend(
                    texts
                        .filter_map(|c| c.get("text").and_then(Value::as_str))
                        .map(str::to_string),
                );
            }
            _ => {}
        }
    }
    let mut end = end?;
    input.reverse();
    end.input = input;
    Some(end)
}

/// Whether a Codex turn's error (`kind`, `message`: `CodexTurnEnd::error`)
/// is its login no longer working: its kind says so, or what it said is a
/// 401 or 403 (Codex 0.159 names "workspace routing discovery unauthorized
/// (401)" only `other`; it runs into it when the account was switched under
/// a running session). A content filter's refusal is never one.
pub fn codex_signed_out(kind: &str, message: &str) -> bool {
    !kind.ends_with("_policy")
        && (kind == "unauthorized" || super::classify_error(message) == Some(LimitKind::Auth))
}

/// The session file Codex keeps for `thread`, started around `started_ms`:
/// `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<thread>.jsonl`.
pub fn codex_rollout_path(thread: &str, started_ms: i64) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex")))?;
    codex_rollout_in(&home, thread, started_ms)
}

pub(super) fn codex_rollout_in(
    home: &std::path::Path,
    thread: &str,
    started_ms: i64,
) -> Option<std::path::PathBuf> {
    let suffix = format!("-{thread}.jsonl");
    // The folder is named after the local day the session started.
    [0i64, -1, 1].iter().find_map(|day| {
        let date = Local
            .timestamp_millis_opt(started_ms + day * 86_400_000)
            .single()?;
        let dir = home
            .join("sessions")
            .join(date.format("%Y/%m/%d").to_string());
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(&suffix))
            })
    })
}

/// Whether a Codex session file records a compaction after byte `offset`.
/// Codex writes a `compacted` entry (which carries the whole replacement
/// history, so it can be far longer than any tail worth reading).
pub fn codex_compacted_since(path: &std::path::Path, offset: u64) -> bool {
    std::fs::File::open(path).is_ok_and(|file| codex_compacted_since_in(file, offset))
}

/// `codex_compacted_since` for a file already open.
pub fn codex_compacted_since_in(mut file: std::fs::File, offset: u64) -> bool {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return false;
    }
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        // The entry's type comes right after its timestamp.
        let head = String::from_utf8_lossy(&line[..line.len().min(300)]);
        if head.starts_with('{') && head.contains("\"type\":\"compacted\"") {
            return true;
        }
    }
}

/// A usage limit shown in a Codex session.
pub fn codex_screen(screen: &str, now_ms: i64) -> Option<LimitInfo> {
    last_limit_on_screen(
        screen,
        |line| {
            // Codex prints its errors behind its error marker; replies, the echoed
            // prompt and wrapped text quoting the phrase do not start with it.
            let line = line.strip_prefix('■')?;
            if line.contains("out of credits") || line.contains("hit your spend cap") {
                Some(LimitKind::Credits)
            } else if line.contains("you've hit your usage limit")
                || line.contains("usage limit reached")
            {
                Some(LimitKind::Usage)
            } else {
                None
            }
        },
        |line| line.starts_with('•') || line.starts_with('■') || line.starts_with('└'),
        |line| line.starts_with('›'),
        now_ms,
    )
}
