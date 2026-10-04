//! Reset times as the CLIs print them ("resets 3pm", "in 2h 5m", "Jan 2, 9am"),
//! read as an absolute time.

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone};

use super::normalise;

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
