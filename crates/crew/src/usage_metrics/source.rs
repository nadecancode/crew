//! Bounded, read-only tailing of one native session file, and what its
//! adapters produce.
//!
//! A stream is read in batches from its last committed complete-line
//! boundary up to the size the file had when the batch started; appends
//! beyond that belong to the next batch. An incomplete final line is never
//! consumed, so after a restart it is read again from the committed boundary.
//! The checkpoint holds offsets, file identity and a digest, never a line.
//!
//! A replaced, shrunk or rewritten file closes its generation. Everything
//! already in it when that is noticed is an ambiguous prefix: it seeds parser
//! state and is not charged again (`notes/usage-metrics.md`, loss of
//! continuity). The same seeding covers a session that existed before
//! tracking began. None of this claims tamper-proof source history.

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};

use crew_core::rpc::usage_metrics::{CoverageReason, ObservationUnit};
use crew_core::RuntimeKind;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::counters::Normalized;
use super::{claude, codex};

/// At most this much input per batch, unless one record alone is longer.
pub const BATCH_BYTES: u64 = 4 << 20;
/// At most this many lines per batch.
pub const BATCH_LINES: usize = 1_000;
/// The longest record decoded; a longer one is drained without being kept.
pub const MAX_RECORD: u64 = 8 << 20;
/// The deepest nesting of arrays and objects decoded. `serde_json` skips the
/// bodies a decoder ignores without its recursion limit, so the raw line is
/// checked against this before decoding.
pub const MAX_DEPTH: usize = 128;
/// The largest persisted parser state of one stream.
pub const MAX_STATE: usize = 64 << 10;

/// Bytes at the start of the file and before the checkpoint that the
/// continuity witness covers.
const HEAD: u64 = 256;
const TAIL: u64 = 64;
const DRAIN_CHUNK: u64 = 64 << 10;

/// Whether a stream is read from its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// A confirmed fresh native session: everything in it is charged.
    Fresh,
    /// A session that already existed when tracking began: its current
    /// content seeds state and only later appends are charged.
    Existing,
}

/// Where a stream's reading stands. Persisted atomically with what the batch
/// produced; holds no native text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Bumped each time continuity is lost.
    pub generation: u64,
    pub dev: u64,
    pub ino: u64,
    /// The byte after the last consumed complete line, or the drain position
    /// inside an oversized record.
    pub offset: u64,
    /// Lines consumed so far in this generation.
    pub line: u64,
    /// Lines starting before this offset only seed parser state.
    pub charge_from: u64,
    /// Inside an oversized record being drained.
    pub draining: bool,
    /// Digest of the file's first bytes and the bytes before `offset`.
    pub witness: Option<String>,
}

/// Why a file generation was closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Loss {
    /// Another file now has the name (device or inode changed).
    Replaced,
    /// The file is shorter than what was already read.
    Truncated,
    /// Bytes already read changed.
    Rewritten,
}

/// Whether a line is charged or only seeds parser state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Seed,
    Charge,
}

/// One complete line, without its newline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub generation: u64,
    pub offset: u64,
    /// One-based within the generation.
    pub number: u64,
    pub phase: Phase,
    pub text: Vec<u8>,
}

/// What one bounded read produced.
#[derive(Debug)]
pub struct Batch {
    pub lines: Vec<Line>,
    /// The checkpoint after these lines, to commit with what they produce.
    pub checkpoint: Checkpoint,
    /// Set when this batch started a new generation.
    pub loss: Option<Loss>,
    /// Records longer than [`MAX_RECORD`] that were skipped.
    pub oversized: u64,
    /// The file size captured when the batch started.
    pub size: u64,
    caught_up: bool,
}

impl Batch {
    /// The checkpoint after `self.lines[index]` and everything before it, to
    /// commit part of a batch.
    pub fn checkpoint_after(&self, file: &File, index: usize) -> io::Result<Checkpoint> {
        let Some(next) = self.lines.get(index + 1) else {
            return Ok(self.checkpoint.clone());
        };
        let mut cp = self.checkpoint.clone();
        cp.offset = next.offset;
        cp.line = next.number - 1;
        cp.draining = false;
        cp.witness = witness(file, cp.offset)?;
        Ok(cp)
    }

    /// Everything present when the batch started has been consumed, except an
    /// incomplete final line; `false` when a budget stopped the batch.
    pub fn caught_up(&self) -> bool {
        self.caught_up
    }
}

impl Checkpoint {
    /// The start of a stream on `file`.
    pub fn new(file: &File, start: Start) -> io::Result<Self> {
        let meta = regular(file)?;
        Ok(Self {
            generation: 0,
            dev: meta.dev(),
            ino: meta.ino(),
            offset: 0,
            line: 0,
            charge_from: match start {
                Start::Fresh => 0,
                Start::Existing => meta.len(),
            },
            draining: false,
            witness: None,
        })
    }

    fn after_loss(&self, meta: &std::fs::Metadata) -> Self {
        Self {
            generation: self.generation + 1,
            dev: meta.dev(),
            ino: meta.ino(),
            offset: 0,
            line: 0,
            charge_from: meta.len(),
            draining: false,
            witness: None,
        }
    }
}

fn regular(file: &File) -> io::Result<std::fs::Metadata> {
    let meta = file.metadata()?;
    if meta.is_file() {
        Ok(meta)
    } else {
        Err(io::Error::other("native source is not a regular file"))
    }
}

fn read_at(file: &File, offset: u64, len: u64) -> io::Result<Vec<u8>> {
    let mut buf = vec![0; len as usize];
    let mut done = 0;
    while done < buf.len() {
        match file.read_at(&mut buf[done..], offset + done as u64) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(done);
    Ok(buf)
}

fn witness(file: &File, offset: u64) -> io::Result<Option<String>> {
    if offset == 0 {
        return Ok(None);
    }
    let head = read_at(file, 0, offset.min(HEAD))?;
    let tail_len = offset.min(TAIL);
    let tail = read_at(file, offset - tail_len, tail_len)?;
    let mut digest = Sha256::new();
    digest.update(&head);
    digest.update(b"\0");
    digest.update(&tail);
    Ok(Some(
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    ))
}

fn continuity(file: &File, meta: &std::fs::Metadata, cp: &Checkpoint) -> io::Result<Option<Loss>> {
    if (meta.dev(), meta.ino()) != (cp.dev, cp.ino) {
        return Ok(Some(Loss::Replaced));
    }
    if meta.len() < cp.offset {
        return Ok(Some(Loss::Truncated));
    }
    if witness(file, cp.offset)? != cp.witness {
        return Ok(Some(Loss::Rewritten));
    }
    Ok(None)
}

/// Reads the next bounded batch of `file` after `from`. `file` must be a
/// fresh open of the stream's source, so that a replaced file is noticed.
/// Never writes to the file.
pub fn read(file: &File, from: &Checkpoint) -> io::Result<Batch> {
    let meta = regular(file)?;
    let size = meta.len();
    let loss = continuity(file, &meta, from)?;
    let mut cp = match loss {
        Some(_) => from.after_loss(&meta),
        None => from.clone(),
    };
    let mut lines = Vec::new();
    let mut oversized = 0;
    // Whether nothing complete is left: set where each path ends.
    let caught_up;
    if cp.draining {
        drain(file, &mut cp, size)?;
        caught_up = cp.offset == size;
    } else if cp.offset >= size {
        caught_up = true;
    } else {
        let pos = cp.offset;
        let window = read_at(file, pos, (size - pos).min(BATCH_BYTES))?;
        let mut start = 0usize;
        while lines.len() < BATCH_LINES {
            let Some(nl) = window[start..].iter().position(|&b| b == b'\n') else {
                break;
            };
            push(
                &mut lines,
                &mut cp,
                pos + start as u64,
                &window[start..start + nl],
            );
            start += nl + 1;
        }
        let window_full = window.len() as u64 == BATCH_BYTES;
        // The window reached the end and holds no further complete line.
        let mut done = pos + window.len() as u64 == size && !window[start..].contains(&b'\n');
        if lines.is_empty() && start == 0 && window_full {
            done = false;
            // The first record is longer than a batch.
            let long = read_at(file, pos, (size - pos).min(MAX_RECORD + 1))?;
            match long.iter().position(|&b| b == b'\n') {
                Some(nl) if nl as u64 <= MAX_RECORD => {
                    push(&mut lines, &mut cp, pos, &long[..nl]);
                    start = nl + 1;
                }
                Some(nl) => {
                    oversized += 1;
                    cp.line += 1;
                    start = nl + 1;
                }
                None if long.len() as u64 > MAX_RECORD => {
                    oversized += 1;
                    cp.draining = true;
                    start = long.len();
                }
                // Incomplete and not yet too long: wait for its end.
                None => done = true,
            }
            done = done || pos + start as u64 == size;
        }
        cp.offset = pos + start as u64;
        caught_up = done;
    }
    cp.witness = witness(file, cp.offset)?;
    Ok(Batch {
        lines,
        checkpoint: cp,
        loss,
        oversized,
        size,
        caught_up,
    })
}

fn push(lines: &mut Vec<Line>, cp: &mut Checkpoint, offset: u64, text: &[u8]) {
    cp.line += 1;
    if text.iter().all(u8::is_ascii_whitespace) {
        return;
    }
    lines.push(Line {
        generation: cp.generation,
        offset,
        number: cp.line,
        phase: if offset < cp.charge_from {
            Phase::Seed
        } else {
            Phase::Charge
        },
        text: text.to_vec(),
    });
}

/// Skips the rest of an oversized record, keeping nothing of it.
fn drain(file: &File, cp: &mut Checkpoint, size: u64) -> io::Result<()> {
    let end = size.min(cp.offset + BATCH_BYTES);
    while cp.offset < end {
        let chunk = read_at(file, cp.offset, (end - cp.offset).min(DRAIN_CHUNK))?;
        if chunk.is_empty() {
            break;
        }
        if let Some(nl) = chunk.iter().position(|&b| b == b'\n') {
            cp.offset += nl as u64 + 1;
            cp.line += 1;
            cp.draining = false;
            return Ok(());
        }
        cp.offset += chunk.len() as u64;
    }
    Ok(())
}

/// One sanitized usage observation. Its counters replace those of any earlier
/// observation with the same key in the same logical stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// Stable within the logical stream: `m:<message id>` for an identified
    /// message, `c:<from vector>><to vector>` for a Codex increment,
    /// `l:<generation>:<offset>` for a record without identity.
    pub key: String,
    pub unit: ObservationUnit,
    pub normalized: Normalized,
    pub model: Option<String>,
    /// Provider request id, for diagnostic joins only.
    pub request_id: Option<String>,
    /// Native turn, scoped to the stream.
    pub turn: Option<String>,
    pub native_ms: Option<i64>,
    /// Time since the previous usage record in file order; unknown when
    /// either time is unknown or time went backwards.
    pub gap_ms: Option<u64>,
    pub generation: u64,
    pub offset: u64,
    pub line: u64,
    pub phase: Phase,
    /// Why this is not a counted request or why counters are missing.
    pub reasons: Vec<CoverageReason>,
    /// A later record with the same key may revise it (a streamed message).
    /// A key that may not be revised (a cumulative increment) seen again
    /// with other values is a conflicting replay, never a correction.
    pub revisable: bool,
    /// For a native batch, the offset in the same generation of the record
    /// its increment starts from: usage it sums can lie anywhere between.
    pub span_start: Option<u64>,
}

/// Native evidence that is not usage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// A native turn finished; usage after it belongs to the next turn.
    TurnEnd {
        turn: String,
        native_ms: Option<i64>,
    },
    Compaction {
        native_ms: Option<i64>,
    },
    /// Claude's own report that the request's history prefix changed: a
    /// `thinking_drop` with `messagesHistoryChanged` and `prefix_mismatch`,
    /// joined to usage by request id only.
    HistoryChanged {
        request_id: String,
    },
}

/// A record that was read but not counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Sidechain,
    Synthetic,
    Malformed,
    Oversized,
    /// An unchanged cumulative vector.
    Duplicate,
    MissingCumulative,
    /// A record of another native session, such as copied history.
    ForeignSession,
}

impl Skip {
    /// The coverage reason this skip leaves, if it loses usage.
    pub fn reason(self) -> Option<CoverageReason> {
        match self {
            Self::Sidechain => Some(CoverageReason::UnsupportedSidechain),
            Self::Malformed | Self::Oversized => Some(CoverageReason::ParserLimit),
            Self::MissingCumulative => Some(CoverageReason::NotReported),
            Self::ForeignSession => Some(CoverageReason::AmbiguousBoundary),
            Self::Synthetic | Self::Duplicate => None,
        }
    }
}

/// What a batch produced, in file order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Observation(Box<Observation>),
    Signal {
        signal: Signal,
        offset: u64,
    },
    Skipped {
        skip: Skip,
        offset: u64,
    },
    /// Usage between the previous item and this one is unknown.
    Gap {
        reason: CoverageReason,
        offset: u64,
    },
}

/// A runtime's decoder. Its state is persisted with the checkpoint.
pub trait Decoder {
    fn decode(&mut self, line: &Line, out: &mut Vec<Item>);
    /// A new file generation starts: cumulative state reseeds.
    fn reset(&mut self);
}

/// The decoder for a runtime with a supported native adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Adapter {
    Claude(claude::Claude),
    Codex(codex::Codex),
}

impl Adapter {
    /// The adapter for `runtime`'s native session `session`. pi stays
    /// unsupported until a native capture from a supported pi exists.
    pub fn new(runtime: RuntimeKind, session: &str, start: Start) -> Result<Self, CoverageReason> {
        match runtime {
            RuntimeKind::Claude => Ok(Self::Claude(claude::Claude::new(session))),
            RuntimeKind::Codex => Ok(Self::Codex(codex::Codex::new(session, start))),
            RuntimeKind::Pi | RuntimeKind::Shell => Err(CoverageReason::UnsupportedRuntime),
        }
    }

    fn decoder(&mut self) -> &mut dyn Decoder {
        match self {
            Self::Claude(d) => d,
            Self::Codex(d) => d,
        }
    }

    /// Decodes a batch, starting a new generation first when it lost continuity.
    pub fn decode(&mut self, batch: &Batch) -> Vec<Item> {
        let mut out = self.begin(batch);
        for line in &batch.lines {
            self.line(line, &mut out);
        }
        out
    }

    /// What a batch reports before its lines: a new generation (the decoder
    /// reseeds) and records drained for their size.
    pub fn begin(&mut self, batch: &Batch) -> Vec<Item> {
        let mut out = Vec::new();
        if batch.loss.is_some() {
            self.decoder().reset();
            out.push(Item::Gap {
                reason: CoverageReason::SourceGap,
                offset: 0,
            });
        }
        for _ in 0..batch.oversized {
            out.push(Item::Skipped {
                skip: Skip::Oversized,
                offset: batch.checkpoint.offset,
            });
        }
        out
    }

    /// Decodes one line of a batch, after [`Adapter::begin`].
    pub fn line(&mut self, line: &Line, out: &mut Vec<Item>) {
        self.decoder().decode(line, out);
    }

    /// The persisted state, refused when it outgrows [`MAX_STATE`].
    pub fn state(&self) -> Result<Vec<u8>, CoverageReason> {
        let bytes = serde_json::to_vec(self).map_err(|_| CoverageReason::ParserLimit)?;
        if bytes.len() > MAX_STATE {
            return Err(CoverageReason::ParserLimit);
        }
        Ok(bytes)
    }
}

/// A native identifier kept as evidence: a short plain token, never text.
pub fn ident(s: &str) -> Option<String> {
    (!s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')))
    .then(|| s.to_string())
}

/// A native model name kept as public metadata.
pub fn model(s: &str) -> Option<String> {
    (!s.is_empty()
        && s.len() <= 128
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/' | b'@' | b'+')
        }))
    .then(|| s.to_string())
}

/// Time since the previous usage record, updating it.
pub(super) fn gap(last: &mut Option<i64>, now: Option<i64>) -> Option<u64> {
    let gap = match (*last, now) {
        (Some(a), Some(b)) if b >= a => b.checked_sub(a).map(|d| d as u64),
        _ => None,
    };
    *last = now;
    gap
}

/// A string field, or nothing when it holds anything else. Reading it never
/// fails and keeps nothing of a non-string value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Str(pub Option<String>);

impl Str {
    pub fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

/// A boolean field, or nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Flag(pub Option<bool>);

/// A native timestamp in milliseconds: RFC 3339 text or an integer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Stamp(pub Option<i64>);

macro_rules! lenient {
    ($ty:ident, $expect:literal, { $($body:tt)* }) => {
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V;
                impl<'de> Visitor<'de> for V {
                    type Value = $ty;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str($expect)
                    }
                    $($body)*
                    fn visit_unit<E: de::Error>(self) -> Result<$ty, E> {
                        Ok($ty::default())
                    }
                    fn visit_none<E: de::Error>(self) -> Result<$ty, E> {
                        Ok($ty::default())
                    }
                    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<$ty, D::Error> {
                        d.deserialize_any(V)
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<$ty, A::Error> {
                        while seq.next_element::<IgnoredAny>()?.is_some() {}
                        Ok($ty::default())
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<$ty, A::Error> {
                        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                        Ok($ty::default())
                    }
                }
                d.deserialize_any(V)
            }
        }
    };
}

lenient!(Str, "a string", {
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Str, E> {
        Ok(Str(Some(v.to_string())))
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Str, E> {
        Ok(Str(None))
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Str, E> {
        Ok(Str(None))
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Str, E> {
        Ok(Str(None))
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Str, E> {
        Ok(Str(None))
    }
});

lenient!(Flag, "a boolean", {
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Flag, E> {
        Ok(Flag(Some(v)))
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Flag, E> {
        Ok(Flag(None))
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Flag, E> {
        Ok(Flag(None))
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Flag, E> {
        Ok(Flag(None))
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Flag, E> {
        Ok(Flag(None))
    }
});

/// A native time is valid from the epoch up to the largest exact JSON
/// integer; anything else is unknown.
fn valid_time(ms: i64) -> Option<i64> {
    (0..=crew_core::rpc::features::Timestamp::MAX)
        .contains(&ms)
        .then_some(ms)
}

lenient!(Stamp, "a timestamp", {
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Stamp, E> {
        Ok(Stamp(
            chrono::DateTime::parse_from_rfc3339(v)
                .ok()
                .and_then(|t| valid_time(t.timestamp_millis())),
        ))
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Stamp, E> {
        Ok(Stamp(None))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Stamp, E> {
        Ok(Stamp(valid_time(v)))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Stamp, E> {
        Ok(Stamp(i64::try_from(v).ok().and_then(valid_time)))
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Stamp, E> {
        Ok(Stamp(None))
    }
});

/// Whether a record nests at most [`MAX_DEPTH`] arrays and objects.
pub fn within_depth(text: &[u8]) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for &b in text {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return false;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

/// Whether `text` holds any of `needles`: a cheap filter before decoding,
/// so that tool output in multi-gigabyte transcripts is not parsed. The
/// decoder still recognizes only documented record shapes.
pub(super) fn mentions(text: &[u8], needles: &[&[u8]]) -> bool {
    needles
        .iter()
        .any(|n| text.windows(n.len()).any(|w| w == *n))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    pub fn file(dir: &tempfile::TempDir, name: &str, text: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn append(path: &std::path::Path, text: &[u8]) {
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(text)
            .unwrap();
    }

    fn texts(batch: &Batch) -> Vec<String> {
        batch
            .lines
            .iter()
            .map(|l| String::from_utf8(l.text.clone()).unwrap())
            .collect()
    }

    #[test]
    fn a_partial_final_line_waits_and_is_read_whole_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "s.jsonl", b"{\"a\":1}\n{\"b\":");
        let f = File::open(&path).unwrap();
        let cp = Checkpoint::new(&f, Start::Fresh).unwrap();
        let first = read(&f, &cp).unwrap();
        assert_eq!(texts(&first), ["{\"a\":1}"]);
        assert_eq!(
            first.checkpoint.offset, 8,
            "stops at the complete-line boundary"
        );
        assert!(first.caught_up());
        append(&path, b"2}\n");
        // A restart reopens and resumes from the committed checkpoint.
        let again = read(&File::open(&path).unwrap(), &first.checkpoint).unwrap();
        assert_eq!(again.loss, None);
        assert_eq!(texts(&again), ["{\"b\":2}"]);
        assert_eq!(again.lines[0].number, 2);
        assert_eq!(again.lines[0].offset, 8);
    }

    #[test]
    fn a_batch_is_bounded_by_lines_bytes_and_the_size_at_its_start() {
        let dir = tempfile::tempdir().unwrap();
        let line = b"{\"x\":0}\n";
        let path = file(&dir, "s.jsonl", &line.repeat(BATCH_LINES + 5));
        let f = File::open(&path).unwrap();
        let cp = Checkpoint::new(&f, Start::Fresh).unwrap();
        let one = read(&f, &cp).unwrap();
        assert_eq!(one.lines.len(), BATCH_LINES);
        assert!(!one.caught_up());
        let two = read(&f, &one.checkpoint).unwrap();
        assert_eq!(two.lines.len(), 5);
        assert!(two.caught_up());

        let big = format!("{{\"pad\":\"{}\"}}\n", "x".repeat(1 << 20));
        let path = file(&dir, "big.jsonl", big.repeat(6).as_bytes());
        let f = File::open(&path).unwrap();
        let cp = Checkpoint::new(&f, Start::Fresh).unwrap();
        let one = read(&f, &cp).unwrap();
        assert_eq!(one.lines.len(), 3, "4 MiB holds three complete 1 MiB lines");
        assert!(one.checkpoint.offset <= BATCH_BYTES);
    }

    #[test]
    fn a_budget_stopped_batch_is_not_caught_up() {
        let dir = tempfile::tempdir().unwrap();
        let line = |n: usize| format!("{{\"pad\":\"{}\"}}\n", "x".repeat(n));
        // About 3 MiB and 2 MiB: the first window holds only the first.
        let text = format!("{}{}", line(3 << 20), line(2 << 20));
        let path = file(&dir, "s.jsonl", text.as_bytes());
        let f = File::open(&path).unwrap();
        let one = read(&f, &Checkpoint::new(&f, Start::Fresh).unwrap()).unwrap();
        assert_eq!(one.lines.len(), 1);
        assert!(!one.caught_up(), "a complete record remains");
        let two = read(&f, &one.checkpoint).unwrap();
        assert_eq!(two.lines.len(), 1);
        assert!(two.caught_up());

        // An oversized record drained before trailing complete records.
        let text = format!("{}{}{}", line(9 << 20), line(10), "{\"part");
        let path = file(&dir, "t.jsonl", text.as_bytes());
        let f = File::open(&path).unwrap();
        let mut cp = Checkpoint::new(&f, Start::Fresh).unwrap();
        let mut states = Vec::new();
        loop {
            let b = read(&f, &cp).unwrap();
            states.push((b.lines.len(), b.oversized, b.caught_up()));
            let done = b.caught_up();
            cp = b.checkpoint;
            if done {
                break;
            }
            assert!(states.len() < 10);
        }
        assert_eq!(states.last(), Some(&(1, 0, true)), "{states:?}");
        assert!(
            states[..states.len() - 1].iter().all(|s| !s.2),
            "{states:?}"
        );
        assert_eq!(states.iter().map(|s| s.1).sum::<u64>(), 1);
    }

    #[test]
    fn records_over_the_limit_are_drained_and_long_ones_under_it_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("{{\"pad\":\"{}\"}}\n", "y".repeat(5 << 20));
        let huge = format!("{{\"pad\":\"{}\"}}\n", "z".repeat(9 << 20));
        let text = format!("{long}{huge}{{\"after\":1}}\n");
        let path = file(&dir, "s.jsonl", text.as_bytes());
        let f = File::open(&path).unwrap();
        let mut cp = Checkpoint::new(&f, Start::Fresh).unwrap();
        let mut lines = Vec::new();
        let mut oversized = 0;
        for _ in 0..10 {
            let b = read(&f, &cp).unwrap();
            oversized += b.oversized;
            lines.extend(b.lines.iter().map(|l| (l.number, l.text.len())));
            cp = b.checkpoint;
            if cp.offset == text.len() as u64 {
                break;
            }
        }
        assert_eq!(oversized, 1);
        assert_eq!(lines, [(1, long.len() - 1), (3, 11)]);
        assert!(!cp.draining);
    }

    #[test]
    fn replaced_truncated_and_rewritten_files_close_the_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "s.jsonl", b"{\"a\":1}\n{\"a\":2}\n");
        let f = File::open(&path).unwrap();
        let start = Checkpoint::new(&f, Start::Fresh).unwrap();
        let read_all = read(&f, &start).unwrap().checkpoint;

        // Same length, different bytes already read.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .write_all_at(b"{\"a\":9}", 0)
            .unwrap();
        let b = read(&File::open(&path).unwrap(), &read_all).unwrap();
        assert_eq!(b.loss, Some(Loss::Rewritten));
        assert_eq!(b.checkpoint.generation, 1);
        assert!(
            b.lines.iter().all(|l| l.phase == Phase::Seed),
            "the prefix only seeds"
        );

        // Shorter than what was read.
        let path = file(&dir, "t.jsonl", b"{\"a\":1}\n{\"a\":2}\n");
        let f = File::open(&path).unwrap();
        let cp = read(&f, &Checkpoint::new(&f, Start::Fresh).unwrap())
            .unwrap()
            .checkpoint;
        std::fs::write(&path, b"{\"a\":1}\n").unwrap();
        let b = read(&File::open(&path).unwrap(), &cp).unwrap();
        assert_eq!(b.loss, Some(Loss::Truncated));

        // Another file under the name.
        let path = file(&dir, "u.jsonl", b"{\"a\":1}\n");
        let f = File::open(&path).unwrap();
        let cp = read(&f, &Checkpoint::new(&f, Start::Fresh).unwrap())
            .unwrap()
            .checkpoint;
        let other = file(&dir, "u2.jsonl", b"{\"a\":1}\n{\"a\":3}\n");
        std::fs::rename(&other, &path).unwrap();
        let b = read(&File::open(&path).unwrap(), &cp).unwrap();
        assert_eq!(b.loss, Some(Loss::Replaced));
        assert_eq!(b.checkpoint.charge_from, 16);
        assert!(b.lines.iter().all(|l| l.phase == Phase::Seed));
        // Appends after the loss are charged.
        append(&path, b"{\"a\":4}\n");
        let next = read(&File::open(&path).unwrap(), &b.checkpoint).unwrap();
        assert_eq!(next.loss, None);
        assert_eq!(next.lines.len(), 1);
        assert_eq!(next.lines[0].phase, Phase::Charge);
    }

    #[test]
    fn an_existing_session_seeds_its_current_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "s.jsonl", b"{\"old\":1}\n");
        let f = File::open(&path).unwrap();
        let cp = Checkpoint::new(&f, Start::Existing).unwrap();
        append(&path, b"{\"new\":1}\n");
        let b = read(&File::open(&path).unwrap(), &cp).unwrap();
        let phases: Vec<_> = b.lines.iter().map(|l| l.phase).collect();
        assert_eq!(phases, [Phase::Seed, Phase::Charge]);
    }

    #[test]
    fn special_files_are_refused_and_sources_stay_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Checkpoint::new(&File::open(dir.path()).unwrap(), Start::Fresh).is_err());
        let text = b"{\"a\":1}\n{\"a\":2";
        let path = file(&dir, "s.jsonl", text);
        let before = std::fs::metadata(&path).unwrap();
        let f = File::open(&path).unwrap();
        let cp = Checkpoint::new(&f, Start::Fresh).unwrap();
        let b = read(&f, &cp).unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), text);
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        assert_eq!(before.len(), after.len());
        let saved = serde_json::to_string(&b.checkpoint).unwrap();
        assert!(
            !saved.contains("\"a\""),
            "a checkpoint holds no line: {saved}"
        );
    }

    #[test]
    fn lenient_fields_keep_nothing_of_unexpected_values() {
        #[derive(Deserialize)]
        struct R {
            #[serde(default)]
            s: Str,
            #[serde(default)]
            f: Flag,
            #[serde(default)]
            t: Stamp,
        }
        let r: R = serde_json::from_str(
            r#"{"s":{"body":"PRIVATE"},"f":"yes","t":"2026-10-08T05:22:05.365Z"}"#,
        )
        .unwrap();
        assert_eq!(r.s, Str(None));
        assert_eq!(r.f, Flag(None));
        assert_eq!(r.t, Stamp(Some(1_791_436_925_365)));
        let nest = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert!(within_depth(nest(MAX_DEPTH).as_bytes()));
        assert!(!within_depth(nest(MAX_DEPTH + 1).as_bytes()));
        let quoted = format!("{{\"s\":\"{}\\\"\"}}", "[".repeat(MAX_DEPTH + 1));
        assert!(
            within_depth(quoted.as_bytes()),
            "brackets inside strings do not nest"
        );
        assert_eq!(ident("msg_01AB-c.d:e"), Some("msg_01AB-c.d:e".into()));
        assert_eq!(ident("has space"), None);
        assert_eq!(ident(&"a".repeat(129)), None);
        assert_eq!(
            model("claude-haiku-4-5-20251001").as_deref(),
            Some("claude-haiku-4-5-20251001")
        );
        assert_eq!(model("<synthetic>"), None);
    }

    #[test]
    fn gaps_are_unknown_when_time_is_missing_or_goes_backwards() {
        let mut last = None;
        assert_eq!(gap(&mut last, Some(1_000)), None);
        assert_eq!(gap(&mut last, Some(4_000)), Some(3_000));
        assert_eq!(
            gap(&mut last, Some(2_000)),
            None,
            "a regression is not a gap"
        );
        assert_eq!(gap(&mut last, Some(2_500)), Some(500));
        assert_eq!(gap(&mut last, None), None);
        assert_eq!(gap(&mut last, Some(9_000)), None);
        // Extremes never overflow.
        let mut last = Some(i64::MIN);
        assert_eq!(gap(&mut last, Some(i64::MAX)), None);
    }

    #[test]
    fn times_outside_the_exact_range_are_unknown() {
        #[derive(Deserialize)]
        struct R {
            t: Stamp,
        }
        let t = |v: serde_json::Value| serde_json::from_value::<R>(json!({ "t": v })).unwrap().t;
        assert_eq!(t(json!("1800-01-01T00:00:00Z")), Stamp(None));
        assert_eq!(t(json!(i64::MAX)), Stamp(None));
        assert_eq!(t(json!(u64::MAX)), Stamp(None));
        assert_eq!(t(json!(-5)), Stamp(None));
        assert_eq!(
            t(json!(9_007_199_254_740_991u64)),
            Stamp(Some(9_007_199_254_740_991))
        );
        assert_eq!(
            t(json!("2026-10-08T00:00:00Z")),
            Stamp(Some(1_791_417_600_000))
        );
    }
}
