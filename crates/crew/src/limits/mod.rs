//! Usage limits: recognising that a runtime has stopped because the account
//! ran out of quota (or credits, or a working login), and working out when it
//! can go on.
//!
//! Claude Code reports this through its `StopFailure` hook, which names the
//! kind of error. Codex has no such hook, so its terminal is read for the
//! message it prints. The texts matched here are the ones the CLIs print
//! (Codex 0.159, Claude Code 2.1); anything else is simply not a limit.

#[cfg(test)]
use chrono::{Local, TimeZone};
use crew_core::util::{one_line, truncate_chars};
use serde_json::Value;

mod claude;
mod codex;
mod pi;
mod reset;
#[cfg(test)]
use claude::claude_transcript_in;
pub use claude::{
    claude_cached_quota, claude_model_switch, claude_quota, claude_screen, claude_session_facts,
    claude_stop_failure, claude_transcript_path, claude_usage_cache_path, ModelSwitch,
    SessionFacts,
};
#[cfg(test)]
use codex::codex_rollout_in;
pub use codex::{
    codex_compacted_since, codex_compacted_since_in, codex_context, codex_last_turn, codex_quota,
    codex_rollout_path, codex_running, codex_running_in, codex_running_of, codex_screen,
    CodexTurnEnd,
};
pub use pi::{pi_screen, pi_turn_error};
pub use reset::parse_reset;

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

/// A percentage a reading can carry: a number that is still finite as an f32
/// (`1e300` is not, and an infinite quota breaks the snapshots it travels in).
fn finite_percent(value: &Value) -> Option<f32> {
    Some(value.as_f64()? as f32).filter(|p| p.is_finite())
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

/// The last part of a file, from a line start, without reading all of it.
pub fn read_tail(path: &std::path::Path, max_bytes: u64) -> Option<String> {
    read_tail_of(&mut std::fs::File::open(path).ok()?, max_bytes)
}

/// `read_tail` of a file already open.
pub fn read_tail_of(file: &mut std::fs::File, max_bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
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
    fn a_quota_that_does_not_fit_an_f32_is_dropped() {
        // 1e300 becomes `inf` as an f32, which breaks every snapshot that carries it.
        let huge = claude_quota(&json!({ "rate_limits": [
            { "kind": "five_hour", "percentUsed": 1e300 },
            { "kind": "seven_day", "percentUsed": 21 },
        ]}));
        assert_eq!(huge.len(), 1);
        assert!(huge.iter().all(|r| r.percent_used.is_finite()));
        let cache = json!({ "cachedUsageUtilization": { "fetchedAtMs": 1, "limits": [
            { "kind": "session", "percent": 1e300 },
        ] } });
        assert!(claude_cached_quota(&cache.to_string())
            .unwrap()
            .1
            .is_empty());
        let rollout = r#"{"payload":{"rate_limits":{"primary":{"window_minutes":300,"used_percent":1e300}}}}"#;
        assert!(codex_quota(rollout).is_empty());
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
