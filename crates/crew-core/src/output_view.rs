//! Deterministic views of already captured tool output. This module does not
//! execute commands, read files, redact text, or replace native file reads.
//! Unknown output stays raw. A summary is evidence about these captured bytes,
//! not independent proof that the caller's command or reported status is true.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

/// The original command's outcome, supplied by the caller, never the helper's
/// exit status. A timeout can also have an exit code or terminating signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedStatus {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
}

impl CapturedStatus {
    pub fn success(self) -> bool {
        self.exit_code == Some(0) && self.signal.is_none() && !self.timed_out
    }

    pub fn valid(self) -> bool {
        !(self.exit_code.is_some() && self.signal.is_some())
            && self.exit_code.is_none_or(|n| (0..=255).contains(&n))
            && self.signal.is_none_or(|n| (1..=127).contains(&n))
            && (self.exit_code.is_some() || self.signal.is_some() || self.timed_out)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFormat {
    CargoTest,
    Raw,
    Machine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawReason {
    Requested,
    MachineFormat,
    OriginalStatus,
    InputTooLarge,
    NonTextOrControl,
    UnrecognizedOrIncomplete,
    NoTestsExecuted,
    RecoveryUnavailable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CargoSummary {
    pub suites: u64,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub measured: u64,
    pub filtered_out: u64,
}

impl CargoSummary {
    fn add(&mut self, other: Self) -> Option<()> {
        self.suites = self.suites.checked_add(other.suites)?;
        self.passed = self.passed.checked_add(other.passed)?;
        self.failed = self.failed.checked_add(other.failed)?;
        self.ignored = self.ignored.checked_add(other.ignored)?;
        self.measured = self.measured.checked_add(other.measured)?;
        self.filtered_out = self.filtered_out.checked_add(other.filtered_out)?;
        Some(())
    }
}

/// Exact half-open byte ranges. Lines, timestamps and paths are not invented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Content reference, not authority to retrieve another member's output.
    pub reference: String,
    pub stdout: ByteRange,
    pub stderr: ByteRange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum View {
    Summary {
        parser: String,
        counts: CargoSummary,
        omitted: String,
    },
    Raw {
        reason: RawReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputView {
    pub format_version: u32,
    pub original_status: CapturedStatus,
    pub source: Source,
    #[serde(flatten)]
    pub view: View,
}

/// Hash status and both streams with lengths and a versioned domain. Neither
/// capture time, private directory nor member identity changes the reference.
pub fn reference(stdout: &[u8], stderr: &[u8], status: CapturedStatus) -> String {
    let mut hash = Sha256::new();
    hash.update(b"crew-output-v1\0");
    for value in [status.exit_code, status.signal] {
        hash.update([u8::from(value.is_some())]);
        hash.update(value.unwrap_or_default().to_le_bytes());
    }
    hash.update([u8::from(status.timed_out)]);
    for stream in [stdout, stderr] {
        hash.update((stream.len() as u64).to_le_bytes());
        hash.update(stream);
    }
    format!("sha256:{:x}", hash.finalize())
}

pub fn view(
    stdout: &[u8],
    stderr: &[u8],
    status: CapturedStatus,
    format: InputFormat,
) -> OutputView {
    let parsed = summarize(stdout, stderr, status, format);
    OutputView {
        format_version: FORMAT_VERSION,
        original_status: status,
        source: Source {
            reference: reference(stdout, stderr, status),
            stdout: ByteRange {
                start: 0,
                end: stdout.len() as u64,
            },
            stderr: ByteRange {
                start: 0,
                end: stderr.len() as u64,
            },
        },
        view: match parsed {
            Ok(counts) => View::Summary {
                parser: "cargo_test_v1".into(),
                counts,
                omitted:
                    "test names, ignored reasons, build paths and timing; recover raw for details"
                        .into(),
            },
            Err(reason) => View::Raw { reason },
        },
    }
}

fn summarize(
    stdout: &[u8],
    stderr: &[u8],
    status: CapturedStatus,
    format: InputFormat,
) -> Result<CargoSummary, RawReason> {
    match format {
        InputFormat::Raw => return Err(RawReason::Requested),
        InputFormat::Machine => return Err(RawReason::MachineFormat),
        InputFormat::CargoTest => {}
    }
    if !status.valid() || !status.success() {
        return Err(RawReason::OriginalStatus);
    }
    if stdout.len().saturating_add(stderr.len()) > MAX_CAPTURE_BYTES {
        return Err(RawReason::InputTooLarge);
    }
    let mut total = CargoSummary::default();
    // Do not invent an interleaving or let a partial suite in one stream be
    // completed by a different stream. Normal cargo metadata may be on stderr.
    for bytes in [stdout, stderr] {
        let text = std::str::from_utf8(bytes).map_err(|_| RawReason::NonTextOrControl)?;
        if bytes
            .iter()
            .any(|b| b.is_ascii_control() && !matches!(b, b'\n' | b'\r'))
        {
            return Err(RawReason::NonTextOrControl);
        }
        let counts = parse_stream(text).ok_or(RawReason::UnrecognizedOrIncomplete)?;
        total
            .add(counts)
            .ok_or(RawReason::UnrecognizedOrIncomplete)?;
    }
    if total.passed == 0 {
        return Err(RawReason::NoTestsExecuted);
    }
    Ok(total)
}

struct Suite<'a> {
    count: u64,
    passed: u64,
    ignored: u64,
    names: HashSet<&'a str>,
}

fn parse_stream(text: &str) -> Option<CargoSummary> {
    // A cut-off last line is not complete captured libtest output.
    if !text.is_empty() && !text.ends_with('\n') {
        return None;
    }
    let mut total = CargoSummary::default();
    let mut suite: Option<Suite<'_>> = None;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        if line.contains('\r') || line.len() > 4096 {
            return None;
        }
        if let Some(running) = line.strip_prefix("running ") {
            if suite.is_some() {
                return None;
            }
            let (count, suffix) = running.split_once(' ')?;
            let count = number(count)?;
            if suffix != if count == 1 { "test" } else { "tests" } {
                return None;
            }
            suite = Some(Suite {
                count,
                passed: 0,
                ignored: 0,
                names: HashSet::new(),
            });
        } else if line.starts_with("test result:") {
            let active = suite.take()?;
            let result = result_line(line)?;
            if result.failed != 0
                || result.measured != 0
                || result.passed != active.passed
                || result.ignored != active.ignored
                || result.passed.checked_add(result.ignored)? != active.count
            {
                return None;
            }
            total.add(result)?;
        } else if let Some(test) = line.strip_prefix("test ") {
            let active = suite.as_mut()?;
            let (name, outcome) = test.rsplit_once(" ... ")?;
            if name.is_empty() || !active.names.insert(name) {
                return None;
            }
            match outcome {
                "ok" => active.passed = active.passed.checked_add(1)?,
                "ignored" => active.ignored = active.ignored.checked_add(1)?,
                _ if outcome.starts_with("ignored, ") && outcome.len() > 9 => {
                    active.ignored = active.ignored.checked_add(1)?;
                }
                _ => return None,
            }
            if active.passed.checked_add(active.ignored)? > active.count {
                return None;
            }
        } else if suite.is_some() || !cargo_metadata(line) {
            return None;
        }
    }
    suite.is_none().then_some(total)
}

fn number(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn seconds(s: &str) -> bool {
    let Some(value) = s.strip_suffix('s') else {
        return false;
    };
    let mut parts = value.split('.');
    number(parts.next().unwrap_or_default()).is_some()
        && parts.next().is_none_or(|p| number(p).is_some())
        && parts.next().is_none()
}

fn result_line(line: &str) -> Option<CargoSummary> {
    let fields: Vec<_> = line
        .strip_prefix("test result: ok. ")?
        .split("; ")
        .collect();
    let [passed, failed, ignored, measured, filtered, duration] = fields.as_slice() else {
        return None;
    };
    if !seconds(duration.strip_prefix("finished in ")?) {
        return None;
    }
    Some(CargoSummary {
        suites: 1,
        passed: number(passed.strip_suffix(" passed")?)?,
        failed: number(failed.strip_suffix(" failed")?)?,
        ignored: number(ignored.strip_suffix(" ignored")?)?,
        measured: number(measured.strip_suffix(" measured")?)?,
        filtered_out: number(filtered.strip_suffix(" filtered out")?)?,
    })
}

fn package(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn cargo_metadata(line: &str) -> bool {
    if let Some(rest) = line.strip_prefix("   Compiling ") {
        let Some((name, version_path)) = rest.split_once(" v") else {
            return false;
        };
        let (version, path) = version_path.split_once(' ').unwrap_or((version_path, ""));
        return package(name)
            && !version.is_empty()
            && version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
            && (path.is_empty() || (path.starts_with('(') && path.ends_with(')')));
    }
    if let Some(rest) = line.strip_prefix("    Finished `test` profile [") {
        let Some((profile, duration)) = rest.split_once("] target(s) in ") else {
            return false;
        };
        return !profile.is_empty()
            && profile
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'+' | b'-'))
            && seconds(duration);
    }
    if let Some(rest) = line.strip_prefix("     Running ") {
        let Some((target, executable)) = rest.split_once(" (") else {
            return false;
        };
        let target = target.strip_prefix("unittests ").unwrap_or(target);
        return !target.is_empty()
            && !target.contains(' ')
            && executable.len() > 1
            && executable.ends_with(')');
    }
    line.strip_prefix("   Doc-tests ").is_some_and(package)
}
