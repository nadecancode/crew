//! Usage limits: recognising that a runtime has stopped because the account
//! ran out of quota (or credits, or a working login), and working out when it
//! can go on.
//!
//! Claude Code reports this through its `StopFailure` hook, which names the
//! kind of error. Codex has no such hook, so its terminal is read for the
//! message it prints. The texts matched here are the ones the CLIs print
//! (Codex 0.159, Claude Code 2.1); anything else is simply not a limit.

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone};
use crew_core::util::{one_line, truncate_chars};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    /// A plan or rate limit: it lifts by itself, or with another account.
    Usage,
    /// Out of credits or over a spend cap: someone has to pay or raise it.
    Credits,
    /// The login no longer works: someone has to sign in.
    Auth,
}

impl LimitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitKind::Usage => "usage",
            LimitKind::Credits => "credits",
            LimitKind::Auth => "auth",
        }
    }

    /// A few words for the dashboard.
    pub fn label(kind: &str) -> &'static str {
        match kind {
            "credits" => "out of credits",
            "auth" => "sign-in needed",
            _ => "usage limit reached",
        }
    }
}

/// A limit as a runtime reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitInfo {
    pub kind: LimitKind,
    pub message: String,
    /// When the runtime says it resets (epoch milliseconds), if it said.
    pub resets_at: Option<i64>,
}

/// One usage window as a runtime reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct QuotaReading {
    /// `5h`, `week`, `spend`, or a window length such as `30d`.
    pub window: String,
    pub percent_used: f32,
    pub resets_at: Option<i64>,
}

/// Name a window by its length in minutes.
fn window_name(minutes: i64) -> String {
    match minutes {
        300 => "5h".to_string(),
        10_080 => "week".to_string(),
        m if m > 0 && m % 1440 == 0 => format!("{}d", m / 1440),
        m if m > 0 && m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

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
                percent_used: w.get("percentUsed")?.as_f64()? as f32,
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
                percent_used: limit.get("percent")?.as_f64()? as f32,
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

/// What a window is called where it is shown: `session`, `week`, and a
/// model's own weekly window as `<model> week`.
pub fn window_label(window: &str) -> String {
    match window {
        "5h" => "session".to_string(),
        other => match other.strip_prefix("week/") {
            Some(model) => format!("{model} week"),
            None => other.to_string(),
        },
    }
}

/// Session first, then the weekly windows, then the rest.
pub fn window_rank(window: &str) -> u8 {
    match window {
        "5h" => 0,
        "week" => 1,
        w if w.starts_with("week/") => 2,
        _ => 3,
    }
}

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
                percent_used: w.get("used_percent")?.as_f64()? as f32,
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
    use std::io::{BufRead, BufReader};
    let mut reader = BufReader::new(std::fs::File::open(path).ok()?);
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

fn claude_transcript_in(home: &std::path::Path, session: &str) -> Option<std::path::PathBuf> {
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

/// The session file Codex keeps for `thread`, started around `started_ms`:
/// `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<thread>.jsonl`.
pub fn codex_rollout_path(thread: &str, started_ms: i64) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex")))?;
    codex_rollout_in(&home, thread, started_ms)
}

fn codex_rollout_in(
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

/// The last part of a file, from a line start, without reading all of it.
pub fn read_tail(path: &std::path::Path, max_bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(if start > 0 {
        text.split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default()
    } else {
        text
    })
}

/// Whether a Codex session file records a compaction after byte `offset`.
/// Codex writes a `compacted` entry (which carries the whole replacement
/// history, so it can be far longer than any tail worth reading).
pub fn codex_compacted_since(path: &std::path::Path, offset: u64) -> bool {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
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

/// What a failed turn means for the team.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnFailure {
    Limited(LimitInfo),
    /// Some other API error; the turn is over and can simply be retried.
    Other(String),
}

fn normalise(text: &str) -> String {
    text.replace(['\u{2019}', '\u{2018}'], "'")
        .replace('\u{a0}', " ")
        .to_lowercase()
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

/// The last limit message on a terminal screen, if it is the last thing in
/// the conversation: once anything follows it (the session went on), it is
/// history. `is_reply` recognises a line that belongs to the conversation
/// after the message, `is_prompt` the input line (one is always there).
fn last_limit_on_screen(
    screen: &str,
    matches: impl Fn(&str) -> Option<LimitKind>,
    is_reply: impl Fn(&str) -> bool,
    is_prompt: impl Fn(&str) -> bool,
    now_ms: i64,
) -> Option<LimitInfo> {
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let (at, kind) = lines
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, line)| matches(&normalise(line)).map(|kind| (i, kind)))?;
    let after = &lines[at + 1..];
    if after.len() > 8
        || after.iter().any(|l| is_reply(l.trim_start()))
        || after.iter().filter(|l| is_prompt(l.trim_start())).count() > 1
    {
        return None;
    }
    // A long message wraps; take it up to the input line.
    let mut message = lines[at].trim().to_string();
    for line in after
        .iter()
        .take_while(|l| !is_prompt(l.trim_start()))
        .take(3)
    {
        if line.starts_with("  ") || line.starts_with('\t') {
            message.push(' ');
            message.push_str(line.trim());
        } else {
            break;
        }
    }
    let message = message.trim_start_matches(['■', '●', '⏺', ' ']).to_string();
    Some(LimitInfo {
        kind,
        resets_at: parse_reset(&message, now_ms),
        message: truncate_chars(&one_line(&message), 300),
    })
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

/// What an error message from a model provider says, if it is about the
/// account: a usage limit, no credit, or a login that does not work.
pub fn classify_error(text: &str) -> Option<LimitKind> {
    let lower = text.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    if has(&[
        "credit balance",
        "insufficient credit",
        "out of credits",
        "billing",
        "payment required",
        "402",
    ]) {
        Some(LimitKind::Credits)
    } else if has(&[
        "usage limit",
        "rate limit",
        "rate_limit",
        "429",
        "too many requests",
        "quota exceeded",
        "hit your",
    ]) {
        Some(LimitKind::Usage)
    } else if has(&[
        "401",
        "unauthorized",
        "authentication",
        "not logged in",
        "no api key",
        "invalid api key",
        "token expired",
        "permission_error",
        "403",
    ]) {
        Some(LimitKind::Auth)
    } else {
        None
    }
}

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

/// For a line that starts with Claude Code's prompt marker: whether text
/// follows it.
fn claude_prompt(line: &str) -> Option<bool> {
    let rest = line.strip_prefix('❯')?;
    Some(!rest.trim_matches([' ', '\u{a0}']).is_empty())
}

// ---- reset times ----------------------------------------------------------

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

struct Scanner<'a> {
    rest: &'a str,
}

impl<'a> Scanner<'a> {
    fn skip(&mut self, chars: &[char]) {
        self.rest = self
            .rest
            .trim_start_matches(|c: char| c.is_whitespace() || chars.contains(&c));
    }

    fn number(&mut self) -> Option<u32> {
        let digits = self.rest.len()
            - self
                .rest
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .len();
        if digits == 0 || digits > 10 {
            return None;
        }
        let n = self.rest[..digits].parse().ok()?;
        self.rest = &self.rest[digits..];
        Some(n)
    }

    fn word(&mut self) -> &'a str {
        let len = self.rest.len()
            - self
                .rest
                .trim_start_matches(|c: char| c.is_ascii_alphabetic())
                .len();
        let word = &self.rest[..len];
        self.rest = &self.rest[len..];
        word
    }

    fn eat(&mut self, prefix: &str) -> bool {
        match self.rest.strip_prefix(prefix) {
            Some(rest) => {
                self.rest = rest;
                true
            }
            None => false,
        }
    }
}

/// `3 hours 12 minutes`, `2h 5m`, `45 min`.
fn parse_duration(text: &str) -> Option<i64> {
    let mut scan = Scanner { rest: text };
    let mut total = 0i64;
    let mut parts = 0;
    loop {
        scan.skip(&[',']);
        if scan.eat("and ") {
            continue;
        }
        let Some(n) = scan.number() else { break };
        scan.skip(&[]);
        let unit = match scan.word() {
            "d" | "day" | "days" => 86_400,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            _ => return None,
        };
        total += i64::from(n) * unit;
        parts += 1;
    }
    (parts > 0 && total > 0).then_some(total * 1000)
}

/// `3:19 PM`, `3pm`, `15:19`, optionally after a date: `Oct 3rd, 2026 3:19 PM`,
/// `Oct 3, 9am`. Times are local; a time already past today means tomorrow.
fn parse_clock(text: &str, now_ms: i64) -> Option<i64> {
    let now = Local.timestamp_millis_opt(now_ms).single()?;
    let mut scan = Scanner { rest: text };
    scan.skip(&[]);
    let mut date: Option<NaiveDate> = None;
    let mut explicit_year = false;

    let before = scan.rest;
    let word = scan.word();
    if let Some(month) = MONTHS
        .iter()
        .position(|m| word.len() >= 3 && word.starts_with(m))
    {
        scan.skip(&[]);
        let day = scan.number()?;
        for suffix in ["st", "nd", "rd", "th"] {
            if scan.eat(suffix) {
                break;
            }
        }
        scan.skip(&[',']);
        let mut year = now.year();
        // A four-digit number here is a year, unless it is the time (`1530`
        // is never written, `2026` is).
        let checkpoint = scan.rest;
        match scan.number() {
            Some(y) if (2000..2100).contains(&y) && !scan.rest.starts_with(':') => {
                year = y as i32;
                explicit_year = true;
                scan.skip(&[',']);
            }
            _ => scan.rest = checkpoint,
        }
        scan.eat("at ");
        date = Some(NaiveDate::from_ymd_opt(year, month as u32 + 1, day)?);
    } else if word == "tomorrow" {
        date = Some(now.date_naive() + Duration::days(1));
        scan.skip(&[',']);
        scan.eat("at ");
    } else if word == "today" || word == "at" {
        scan.skip(&[',']);
    } else {
        scan.rest = before;
    }

    scan.skip(&[]);
    let mut hour = scan.number()?;
    let minute = if scan.eat(":") { scan.number()? } else { 0 };
    scan.skip(&[]);
    let meridiem = scan.rest.get(..2).unwrap_or("");
    match meridiem {
        "am" | "pm" if (1..=12).contains(&hour) => {
            hour = hour % 12 + if meridiem == "pm" { 12 } else { 0 };
        }
        // Without am/pm only an unambiguous 24-hour time counts.
        _ if text.contains(':') && hour < 24 => {}
        _ => return None,
    }
    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
    let local = |date: NaiveDate| Local.from_local_datetime(&date.and_time(time)).earliest();
    let mut at = local(date.unwrap_or_else(|| now.date_naive()))?;
    if at <= now {
        match date {
            // "3pm" said at 4pm means tomorrow.
            None => at = local(now.date_naive() + Duration::days(1))?,
            // "Jan 2" said in December means next year.
            Some(d) if !explicit_year && now - at > Duration::days(180) => {
                at = local(NaiveDate::from_ymd_opt(d.year() + 1, d.month(), d.day())?)?;
            }
            Some(_) => {}
        }
    }
    Some(at.timestamp_millis())
}

/// Find when a limit message says it resets.
pub fn parse_reset(text: &str, now_ms: i64) -> Option<i64> {
    let text = normalise(text);
    const ANCHORS: [&str; 9] = [
        "try again at ",
        "try again in ",
        "resets_at",
        "resets at ",
        "resets in ",
        "reset at ",
        "reset in ",
        "resets ",
        "continuing automatically ",
    ];
    for anchor in ANCHORS {
        let Some(at) = text.find(anchor) else {
            continue;
        };
        let tail: String = text[at + anchor.len()..].chars().take(64).collect();
        let tail = tail.trim_start_matches(['"', ':', '=', ' ']);
        let parsed = if anchor == "resets_at" {
            // A Unix timestamp, in seconds or milliseconds.
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            match digits.parse::<i64>() {
                Ok(n) if digits.len() >= 13 => Some(n),
                Ok(n) if digits.len() >= 10 => Some(n * 1000),
                _ => None,
            }
        } else if anchor.ends_with("in ") || tail.starts_with("in ") {
            parse_duration(tail.trim_start_matches("in ")).map(|ms| now_ms + ms)
        } else {
            parse_clock(tail, now_ms)
        };
        // A reset in the past, or absurdly far off, was misread.
        if let Some(t) = parsed.filter(|t| *t > now_ms - 60_000 && *t < now_ms + 400 * 86_400_000) {
            return Some(t);
        }
    }
    None
}

pub use crew_core::util::fmt_when;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Noon, local time, on a fixed day.
    fn noon() -> i64 {
        Local
            .with_ymd_and_hms(2026, 10, 2, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis()
    }

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn reset_times_are_read_from_what_the_clis_print() {
        let now = noon();
        let cases = [
            // Codex.
            ("or try again at 3:19 PM.", local(2026, 10, 2, 15, 19)),
            ("Try again at 9:05 AM.", local(2026, 10, 3, 9, 5)),
            (
                "or try again at Oct 5th, 2026 9:00 AM.",
                local(2026, 10, 5, 9, 0),
            ),
            (
                "try again in 3 hours 12 minutes",
                now + (3 * 3600 + 12 * 60) * 1000,
            ),
            // Claude Code.
            (
                "You've hit your session limit · resets 3pm (America/Chicago)",
                local(2026, 10, 2, 15, 0),
            ),
            (
                "You've hit your weekly limit · resets Oct 6, 9am",
                local(2026, 10, 6, 9, 0),
            ),
            (
                "Usage limit reached · continuing automatically at 4:30pm",
                local(2026, 10, 2, 16, 30),
            ),
            ("limit resets at 15:45", local(2026, 10, 2, 15, 45)),
            ("resets in 2h 5m", now + (2 * 3600 + 5 * 60) * 1000),
            ("resets tomorrow at 8am", local(2026, 10, 3, 8, 0)),
            // Machine-readable.
            (
                r#"{"type":"usage_limit_reached","resets_at":1791000000}"#,
                1_791_000_000_000,
            ),
            ("Jan 2, 9am", 0),
        ];
        for (text, expected) in cases {
            let got = parse_reset(text, now);
            if expected == 0 {
                assert_eq!(got, None, "{text}");
            } else {
                assert_eq!(got, Some(expected), "{text}");
            }
        }
        // A date without a year that already passed long ago means next year.
        let december = local(2026, 12, 20, 12, 0);
        assert_eq!(
            parse_reset("resets Jan 2, 9am", december),
            Some(local(2027, 1, 2, 9, 0))
        );
        // Nothing usable: no guess is made.
        for text in [
            "Please try again later.",
            "resets soon",
            "try again at some point",
            "resets 99",
        ] {
            assert_eq!(parse_reset(text, now), None, "{text}");
        }
        assert_eq!(fmt_when(local(2026, 10, 2, 15, 19), now), "3:19 PM");
        assert_eq!(fmt_when(local(2026, 10, 5, 9, 0), now), "Oct 5, 9:00 AM");
    }

    /// The Codex screen as captured from a real session that got a
    /// `usage_limit_reached` answer.
    const CODEX_LIMITED: &str = "\
  [crew] Message from orchestrator (type task, id m-41732e88f2a1):
  Task \"Reply with the single word hi\" (id codex-1-task) is yours now.
  When finished: git_commit your work, then task_complete with a short summary.

■ You’ve hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at 3:19 PM.

› Ask Codex to do anything

  gpt-6.1-sol max · /work/.crew/worktrees/codex-1
  ? for shortcuts                                             ⚠ 1 warning · f2 to view
";

    #[test]
    fn a_codex_limit_is_read_from_the_screen_only_while_it_is_the_last_thing_said() {
        let now = noon();
        let limit = codex_screen(CODEX_LIMITED, now).expect("a limit");
        assert_eq!(limit.kind, LimitKind::Usage);
        assert!(
            limit.message.starts_with("You’ve hit your usage limit."),
            "{}",
            limit.message
        );
        assert_eq!(limit.resets_at, Some(local(2026, 10, 2, 15, 19)));

        // A narrow terminal wraps the message; the time is still found.
        let wrapped = CODEX_LIMITED.replace(
            "purchase more credits or try again at 3:19 PM.",
            "purchase more\n  credits or try again at 3:19 PM.",
        );
        assert_eq!(
            codex_screen(&wrapped, now).unwrap().resets_at,
            Some(local(2026, 10, 2, 15, 19))
        );

        // Once the session goes on, the message is history: the resume
        // prompt was sent, or the model is answering again.
        let resumed = CODEX_LIMITED.replace(
            "› Ask Codex to do anything",
            "› [crew] The usage limit that stopped you has lifted.\n\n› Ask Codex to do anything",
        );
        assert_eq!(codex_screen(&resumed, now), None);
        let answering = CODEX_LIMITED.replace(
            "› Ask Codex to do anything",
            "• Working (3s • esc to interrupt)\n\n› Ask Codex to do anything",
        );
        assert_eq!(codex_screen(&answering, now), None);
        // …and if it runs into the limit again, the new message counts.
        let again = format!("{resumed}\n■ You've hit your usage limit. Try again at 4:00 PM.\n\n› Ask Codex to do anything\n");
        assert_eq!(
            codex_screen(&again, now).unwrap().resets_at,
            Some(local(2026, 10, 2, 16, 0))
        );

        let credits = "■ Your workspace is out of credits. Add credits to continue.\n› Ask Codex to do anything\n";
        assert_eq!(codex_screen(credits, now).unwrap().kind, LimitKind::Credits);
        // Talking about limits is not hitting one.
        assert_eq!(
            codex_screen("• The API has a usage limit of 100 requests.\n› \n", now),
            None
        );
        assert_eq!(codex_screen("", now), None);
    }

    #[test]
    fn a_pi_turn_that_failed_is_classified_by_what_the_provider_said() {
        let now = local(2026, 10, 2, 12, 0);
        let failed = |details: &str| {
            pi_turn_error(&json!({ "error": "error", "error_details": details }), now)
        };
        match failed("429 {\"type\":\"rate_limit_error\",\"message\":\"This request would exceed your usage limit · resets 3:19 PM\"}") {
            TurnFailure::Limited(info) => {
                assert_eq!(info.kind, LimitKind::Usage);
                assert_eq!(info.resets_at, Some(local(2026, 10, 2, 15, 19)));
                assert!(info.message.starts_with("429 "), "{}", info.message);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(failed("Your credit balance is too low to access the Anthropic API"), TurnFailure::Limited(i) if i.kind == LimitKind::Credits)
        );
        assert!(
            matches!(failed("401 authentication_error: invalid x-api-key"), TurnFailure::Limited(i) if i.kind == LimitKind::Auth)
        );
        assert!(
            matches!(failed("500 Internal server error"), TurnFailure::Other(what) if what == "500 Internal server error")
        );
        assert!(
            matches!(pi_turn_error(&json!({ "error": "error" }), now), TurnFailure::Other(what) if what == "error")
        );
        assert_eq!(classify_error("Overloaded"), None);

        // On the screen, the provider's error line, unless the session went
        // on after it.
        let screen = "● Reading the file\n\nError: 429 rate_limit_error: You've hit your usage limit · resets 3:19 PM\n\n> \n";
        let info = pi_screen(screen, now).unwrap();
        assert_eq!(
            (info.kind, info.resets_at),
            (LimitKind::Usage, Some(local(2026, 10, 2, 15, 19)))
        );
        let went_on = format!("{screen}● Trying again\n> \n");
        assert!(pi_screen(&went_on, now).is_none());
        assert!(pi_screen("● all good\n> \n", now).is_none());
    }

    #[test]
    fn a_claude_limit_is_read_from_the_screen_as_a_fallback() {
        let now = noon();
        let box_ = "────────────────────────── claude-1 ─\n❯\u{a0}\n────────────────────────────────────\n  Haiku 4.5 │ claude-1\n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n";
        // As captured from a real session that got HTTP 429.
        let rate = format!(
            "● API Error: Request rejected (429) · This request would exceed your account's rate limit. Please try again later.\n\n✻ Brewed for 1s · done 1:19 PM\n\n{box_}"
        );
        let limit = claude_screen(&rate, now).expect("a limit");
        assert_eq!((limit.kind, limit.resets_at), (LimitKind::Usage, None));
        assert!(
            limit
                .message
                .starts_with("API Error: Request rejected (429)"),
            "{}",
            limit.message
        );

        let plan =
            format!("  You've hit your session limit · resets 3pm (America/Chicago)\n\n{box_}");
        assert_eq!(
            claude_screen(&plan, now).unwrap().resets_at,
            Some(local(2026, 10, 2, 15, 0))
        );
        let credits = format!("● Credit balance is too low\n{box_}");
        assert_eq!(
            claude_screen(&credits, now).unwrap().kind,
            LimitKind::Credits
        );

        // Followed by a new message or a new answer, it is history.
        let went_on = format!(
            "{}❯ continue please\n\n● Sure.\n\n{box_}",
            rate.replace(box_, "")
        );
        assert_eq!(claude_screen(&went_on, now), None);
        assert_eq!(claude_screen(&format!("● All done.\n{box_}"), now), None);
    }

    #[test]
    fn quoted_limit_text_is_not_a_limit() {
        let now = noon();
        let box_ = "────────────────────────── claude-1 ─\n❯\u{a0}\n────────────────────────────────────\n";
        // The task text on screen, an echoed prompt and an assistant reply all
        // mention the phrase; none of them is the runtime's error.
        for quoted in [
            "  When it prints 'usage limit reached' the agent is parked.\n",
            "❯ why does 'usage limit reached' park the agent?\n",
            "● The check looks for \"usage limit reached\" in the screen text.\n",
        ] {
            assert_eq!(
                claude_screen(&format!("{quoted}\n{box_}"), now),
                None,
                "{quoted}"
            );
        }
        for quoted in [
            "• The check looks for \"usage limit reached\" in the screen text.\n› \n",
            "› explain the usage limit reached branch\n› \n",
            "  When it says usage limit reached, park the agent.\n› \n",
        ] {
            assert_eq!(codex_screen(quoted, now), None, "{quoted}");
        }
    }

    #[test]
    fn quota_readings_come_from_the_mod_and_from_codex_session_files() {
        let claude = claude_quota(&json!({ "rate_limits": [
            { "kind": "five_hour", "percentUsed": 62.5, "resetsAt": "2026-10-02T20:00:00Z" },
            { "kind": "seven_day", "percentUsed": 21 },
            { "kind": "spend_limit", "percentUsed": 104.2 },
            { "percentUsed": 1 },
        ]}));
        assert_eq!(claude.len(), 3);
        assert_eq!(
            (claude[0].window.as_str(), claude[0].percent_used),
            ("5h", 62.5)
        );
        assert_eq!(claude[0].resets_at, Some(1_790_971_200_000));
        assert_eq!(
            (claude[1].window.as_str(), claude[1].resets_at),
            ("week", None)
        );
        assert_eq!(claude[2].window, "spend");
        assert!(claude_quota(&json!({})).is_empty());

        // Claude Code's own usage cache has every window of the account.
        let cache = json!({ "cachedUsageUtilization": { "fetchedAtMs": 1_790_966_580_750i64, "utilization": {
            "five_hour": { "utilization": 33 },
            "limits": [
                { "kind": "session", "percent": 33, "resets_at": "2026-10-02T19:19:59.673623+00:00" },
                { "kind": "weekly_all", "percent": 65, "resets_at": "2026-10-04T04:59:59.673649+00:00" },
                { "kind": "weekly_scoped", "percent": 72, "resets_at": "2026-10-04T04:59:59.673840+00:00",
                  "scope": { "model": { "id": null, "display_name": "Fable" }, "surface": null }, "is_active": true },
                { "kind": "weekly_scoped", "percent": 10, "scope": { "model": null } },
                { "kind": "something_else", "percent": 1 },
            ],
        } } });
        let (fetched_at, readings) = claude_cached_quota(&cache.to_string()).unwrap();
        assert_eq!(fetched_at, 1_790_966_580_750);
        let seen: Vec<(&str, f32, Option<i64>)> = readings
            .iter()
            .map(|r| (r.window.as_str(), r.percent_used, r.resets_at))
            .collect();
        assert_eq!(
            seen,
            [
                ("5h", 33.0, Some(1_790_968_799_673i64)),
                ("week", 65.0, Some(1_791_089_999_673i64)),
                ("week/Fable", 72.0, Some(1_791_089_999_673i64)),
            ]
        );
        // The limits may sit at the top of the cache too; without them, or
        // without a cache, there is nothing.
        let flat = json!({ "cachedUsageUtilization": { "fetchedAtMs": 5, "limits": [{ "kind": "session", "percent": 1 }] } });
        assert_eq!(claude_cached_quota(&flat.to_string()).unwrap().1.len(), 1);
        assert!(claude_cached_quota(
            &json!({ "cachedUsageUtilization": { "fetchedAtMs": 5 } }).to_string()
        )
        .is_none());
        assert!(claude_cached_quota("{}").is_none());
        assert!(claude_cached_quota("not json").is_none());
        assert_eq!(window_label("week/Fable"), "Fable week");
        assert_eq!(window_label("5h"), "session");
        assert_eq!(window_label("week"), "week");
        assert!(
            window_rank("5h") < window_rank("week")
                && window_rank("week") < window_rank("week/Fable")
        );

        // As Codex writes it (captured from a real session), newest last.
        let rollout = concat!(
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","primary":{"used_percent":12.0,"window_minutes":300,"resets_at":1791000000},"secondary":null}}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":56.0,"window_minutes":10080,"resets_at":1791310669},"secondary":{"used_percent":3.5,"window_minutes":43200,"resets_at":1793000000},"credits":{"has_credits":false},"plan_type":"pro"}}}"#,
            "\n",
            "not json\n",
        );
        let codex = codex_quota(rollout);
        assert_eq!(codex.len(), 2);
        assert_eq!(
            (codex[0].window.as_str(), codex[0].percent_used),
            ("week", 56.0)
        );
        assert_eq!(codex[0].resets_at, Some(1_791_310_669_000));
        assert_eq!(codex[1].window, "30d");
        // A session on another endpoint records the field with nothing in it.
        let empty = r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","primary":null,"secondary":null}}}"#;
        assert!(codex_quota(empty).is_empty());

        // How full the context window is: the newest token count, which
        // after a compaction is the small one.
        let log = concat!(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":900000},"last_token_usage":{"total_tokens":208098},"model_context_window":258400}}}"#,
            "\n",
            r#"{"type":"compacted","payload":{"message":""}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":16973},"model_context_window":258400}}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null,"note":"model_context_window"}}"#,
            "\ncut off in the middle of a li",
        );
        assert_eq!(codex_context(log), Some((16973, 258400)));
        assert_eq!(codex_context(rollout), None);
        assert_eq!(codex_context(""), None);

        // What the session runs on is in the entry that opens each turn.
        let turns = concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"text":"about turn_context entries","model":"nope"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-6-astra","effort":"xhigh","summary":"none"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"text":"about turn_context entries","model":"nope"}}"#,
        );
        assert_eq!(
            codex_running(turns),
            Some((Some("gpt-6-astra".into()), Some("xhigh".into())))
        );
        assert_eq!(codex_running(log), None);

        // A turn that began long ago is found in the file itself.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rollout.jsonl");
        std::fs::write(&file, format!("{turns}\n{}\n", "x".repeat(400_000))).unwrap();
        assert_eq!(
            codex_running_in(&file),
            Some((Some("gpt-6-astra".into()), Some("xhigh".into())))
        );
        assert_eq!(codex_running_in(&dir.path().join("missing")), None);

        // A Claude Code transcript names the model, the effort and the fill
        // with every reply; a subagent's replies say nothing of the agent.
        let transcript = concat!(
            r#"{"type":"assistant","isSidechain":false,"effort":"high","message":{"model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_read_input_tokens":40000,"cache_creation_input_tokens":100,"output_tokens":50}}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"the word \"assistant\""}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":false,"effort":"xhigh","message":{"model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_read_input_tokens":45313,"cache_creation_input_tokens":196,"output_tokens":93}}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"effort":"low","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":9}}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":false,"message":{"model":"<synthetic>"}}"#,
        );
        assert_eq!(
            claude_session_facts(transcript),
            Some(SessionFacts {
                model: Some("claude-opus-5-5".into()),
                effort: Some("xhigh".into()),
                context_tokens: Some(45_511),
                switch: None,
            })
        );
        assert_eq!(claude_session_facts(""), None);

        // Claude Code's word on moving the session to another model: when
        // its model's weekly window is used up, when it is overloaded, and
        // when a refusal handed the session to another model. A refusal
        // answered by another model for one reply only is not a move, nor
        // is a line that merely mentions one.
        let moved = concat!(
            r#"{"type":"system","subtype":"model_consent_fallback","content":"Switched to Opus 5.5 for this session · Fable 5.1 requires usage credits · /model to change","level":"warning","choice":"switch_default","originalModel":"claude-fable-5-1[1m]","fallbackModel":"claude-opus-5-5[1m]","persistedAsDefault":false}"#,
            "\n",
            r#"{"type":"user","message":{"content":"a \"model_fallback\" line that is not one"}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":false,"effort":"xhigh","message":{"model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_read_input_tokens":500,"cache_creation_input_tokens":0,"output_tokens":9}}}"#,
        );
        let facts = claude_session_facts(moved).unwrap();
        assert_eq!(
            facts.switch,
            Some(ModelSwitch {
                from: "claude-fable-5-1[1m]".into(),
                to: "claude-opus-5-5[1m]".into(),
                note: "Switched to Opus 5.5 for this session · Fable 5.1 requires usage credits · /model to change".into(),
            })
        );
        assert_eq!(facts.model.as_deref(), Some("claude-opus-5-5"));
        let overloaded = r#"{"type":"system","subtype":"model_fallback","content":"Switched to Sonnet 5.5 due to high demand for Opus 5.5","trigger":"overloaded","originalModel":"claude-opus-5-5","fallbackModel":"claude-sonnet-5-5"}"#;
        assert_eq!(
            claude_model_switch(overloaded).map(|s| s.to),
            Some("claude-sonnet-5-5".into())
        );
        let refusal = r#"{"type":"system","subtype":"model_refusal_fallback","direction":"retry","scope":"session","content":"Opus 5.5 declined. Switched to Sonnet 5.5.","originalModel":"claude-opus-5-5","fallbackModel":"claude-sonnet-5-5"}"#;
        assert_eq!(
            claude_model_switch(refusal).map(|s| s.from),
            Some("claude-opus-5-5".into())
        );
        let one_reply = refusal.replace(r#""scope":"session""#, r#""scope":"local""#);
        assert_eq!(claude_model_switch(&one_reply), None);
        assert_eq!(claude_model_switch(transcript), None);
        // The newest word counts.
        assert_eq!(
            claude_model_switch(&format!("{refusal}\n{overloaded}\n")).map(|s| s.note),
            Some("Switched to Sonnet 5.5 due to high demand for Opus 5.5".into())
        );
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("projects/-work-demo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("abc-123.jsonl"), "").unwrap();
        assert_eq!(
            claude_transcript_in(home.path(), "abc-123"),
            Some(project.join("abc-123.jsonl"))
        );
        assert_eq!(claude_transcript_in(home.path(), "nope"), None);
        assert_eq!(claude_transcript_in(home.path(), "../abc-123"), None);

        // A compaction is an entry of its own, looked for from where the
        // file ended when the compaction was asked for.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let before = r#"{"timestamp":"t","type":"response_item","payload":{"text":"the \"type\":\"compacted\" entry"}}"#;
        std::fs::write(&path, format!("{before}\n")).unwrap();
        assert!(
            !codex_compacted_since(&path, 0),
            "only text that mentions one"
        );
        let offset = std::fs::metadata(&path).unwrap().len();
        let entry = format!(
            r#"{{"timestamp":"t","ordinal":350,"type":"compacted","payload":{{"message":"","replacement_history":"{}"}}}}"#,
            "x".repeat(600_000)
        );
        std::fs::write(&path, format!("{before}\n{entry}\n{before}\n")).unwrap();
        assert!(codex_compacted_since(&path, offset));
        assert!(codex_compacted_since(&path, 0));
        assert!(
            !codex_compacted_since(&path, offset + entry.len() as u64 + 1),
            "an older compaction"
        );
        assert!(!codex_compacted_since(&dir.path().join("missing"), 0));
        assert_eq!(window_name(300), "5h");
        assert_eq!(window_name(720), "12h");
        assert_eq!(window_name(45), "45m");

        // Found by thread id under the day the session started; only the
        // end of the file is read.
        let tmp = tempfile::tempdir().unwrap();
        let started = local(2026, 10, 2, 23, 50);
        let dir = tmp.path().join("sessions/2026/10/02");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rollout-2026-10-02T23-50-00-abc-123.jsonl");
        std::fs::write(&file, format!("{}\n{rollout}", "x".repeat(5000))).unwrap();
        let find = |thread: &str, at: i64| codex_rollout_in(tmp.path(), thread, at);
        assert_eq!(find("abc-123", started).as_deref(), Some(file.as_path()));
        // Looked up just after midnight, it is in yesterday's folder.
        assert_eq!(
            find("abc-123", started + 3_600_000).as_deref(),
            Some(file.as_path())
        );
        assert_eq!(find("other", started), None);
        let tail = read_tail(&file, 1500).unwrap();
        assert!(
            !tail.contains("xxxx") && tail.starts_with("{\"type\""),
            "{}",
            &tail[..40]
        );
        assert_eq!(codex_quota(&tail).len(), 2);
        assert_eq!(
            read_tail(&file, 1 << 20).unwrap().len(),
            5001 + rollout.len()
        );
    }

    #[test]
    fn stop_failures_are_sorted_into_limits_and_other_errors() {
        let now = noon();
        let limited = |payload: Value| match claude_stop_failure(&payload, now) {
            TurnFailure::Limited(info) => info,
            other => panic!("{other:?}"),
        };
        let info = limited(json!({
            "hook_event_name": "StopFailure",
            "error": "rate_limit",
            "error_details": "You've hit your session limit · resets 3pm (America/Chicago)",
        }));
        assert_eq!(
            (info.kind, info.resets_at),
            (LimitKind::Usage, Some(local(2026, 10, 2, 15, 0)))
        );
        assert!(info.message.contains("session limit"));
        // No details: the error name is the message, and no time is invented.
        let info = limited(json!({ "error": "rate_limit" }));
        assert_eq!(
            (info.message.as_str(), info.resets_at),
            ("rate limit", None)
        );
        assert_eq!(
            limited(json!({ "error": "billing_error" })).kind,
            LimitKind::Credits
        );
        for error in [
            "authentication_failed",
            "oauth_org_not_allowed",
            "account_on_hold",
            "cloud_credential_error",
        ] {
            assert_eq!(
                limited(json!({ "error": error })).kind,
                LimitKind::Auth,
                "{error}"
            );
        }
        for error in [
            "overloaded",
            "server_error",
            "max_output_tokens",
            "invalid_request",
            "unknown",
            "",
        ] {
            let failure =
                claude_stop_failure(&json!({ "error": error, "error_details": "boom" }), now);
            assert!(
                matches!(&failure, TurnFailure::Other(text) if text.ends_with(": boom")),
                "{error}: {failure:?}"
            );
        }
        assert_eq!(LimitKind::label("usage"), "usage limit reached");
        assert_eq!(LimitKind::label(LimitKind::Auth.as_str()), "sign-in needed");
    }

    #[test]
    fn a_codex_turn_that_ended_in_an_error_is_read_from_the_session_file_with_what_it_was_given() {
        let started = |turn: &str| {
            format!(
                r#"{{"type":"event_msg","payload":{{"type":"task_started","turn_id":"{turn}","started_at":1791007870}}}}"#
            )
        };
        let given = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"[crew] Message from lead (type review_request, id m-69):\n\nPlease review design-b3.md"}]}}"#;
        let tool = r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}"}}"#;
        let refused = r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-2","last_agent_message":null,"error":{"message":"This content was flagged for possible cybersecurity risk.","codex_error_info":"cyber_policy"},"completed_at":1791007884}}"#;
        let fine = r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1","last_agent_message":"done","completed_at":1791007000}}"#;
        let rollout = [
            started("t-1"),
            fine.to_string(),
            started("t-2"),
            given.to_string(),
            tool.to_string(),
            refused.to_string(),
        ]
        .join("\n");
        let end = codex_last_turn(&rollout).unwrap();
        assert_eq!(end.turn_id, "t-2");
        assert_eq!(end.ended_at, Some(1_791_007_884_000));
        assert_eq!(
            end.error,
            Some((
                "cyber_policy".into(),
                "This content was flagged for possible cybersecurity risk.".into()
            ))
        );
        assert_eq!(end.input, ["[crew] Message from lead (type review_request, id m-69):\n\nPlease review design-b3.md"]);
        // A turn that completed has no error; one that runs has not ended.
        let end = codex_last_turn(&[started("t-1"), fine.to_string()].join("\n")).unwrap();
        assert_eq!((end.turn_id.as_str(), end.error), ("t-1", None));
        assert_eq!(
            codex_last_turn(&format!("{rollout}\n{}\n{given}", started("t-3"))),
            None
        );
        // An interrupted turn has ended too; a kind with details is named by its key.
        let aborted = r#"{"timestamp":"2026-10-03T06:00:00.000Z","type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}"#;
        assert_eq!(
            codex_last_turn(&format!("{rollout}\n{aborted}"))
                .unwrap()
                .turn_id,
            "2026-10-03T06:00:00.000Z"
        );
        let failed = refused.replace(
            r#""cyber_policy""#,
            r#"{"response_stream_disconnected":{"http_status_code":502}}"#,
        );
        assert_eq!(
            codex_last_turn(&failed).unwrap().error.unwrap().0,
            "response_stream_disconnected"
        );
        assert_eq!(codex_last_turn(""), None);
    }
}
