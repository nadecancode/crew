//! The durable usage ledger: registered native streams and who owned them
//! when, their cursors, the sanitized observations they produced, exact
//! seen-id membership, coverage, and the aggregates queries read.
//!
//! Every batch of a stream commits in one short transaction that replaces
//! revisions, moves aggregate contributions, records coverage and advances
//! the cursor together, so a crash before or after it leaves the same state
//! as reading without one. A commit whose starting cursor is not the stored
//! one is refused, which keeps replay idempotent.
//!
//! Attribution is captured, never looked up later: a stream's owner (run,
//! agent incarnation, launch, live or aside) and its work binding (task,
//! epic, account) each start at a recorded position in the native file, and
//! an observation takes those in force where it was first read. A streamed
//! correction keeps the original's run, time and binding.
//!
//! The tables are additive (`usage_` prefix) in the daemon's database; no
//! prompt, reply, tool output, credential or rejected line is stored.

use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use crew_core::rpc::usage_metrics::{
    AccountAttribution, CoverageReason, ObservationUnit, UsageKind, WorkAttribution,
};
use crew_core::RuntimeKind;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::counters::{Counters, Derived, Normalized, WriteKind};
use super::metrics::Fact;
use super::price::Book;
use super::source::{self, Adapter, Checkpoint, Item, Observation, Phase, Signal, Start};

/// At most this many observation changes per transaction.
pub const MAX_CHANGES: usize = 128;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS usage_schema (
    version INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_state (
    key   TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_runs (
    run_id         TEXT PRIMARY KEY,
    started_at     INTEGER,
    tracking_since INTEGER NOT NULL,
    ended_at       INTEGER,
    retained_from  INTEGER,
    build          TEXT
);
CREATE TABLE IF NOT EXISTS usage_streams (
    stream_id         TEXT PRIMARY KEY,
    runtime           TEXT NOT NULL,
    native_session_id TEXT NOT NULL,
    locator           TEXT NOT NULL,
    started_ms        INTEGER NOT NULL,
    admission         TEXT,
    registered_at     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_owners (
    stream_id       TEXT NOT NULL,
    seq             INTEGER NOT NULL,
    generation      INTEGER NOT NULL,
    from_offset     INTEGER NOT NULL,
    run_id          TEXT NOT NULL,
    kind            TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    incarnation     TEXT NOT NULL,
    launch_id       TEXT NOT NULL,
    aside_execution TEXT,
    aside_id        TEXT,
    cli_version     TEXT,
    active          INTEGER NOT NULL,
    registered_at   INTEGER NOT NULL,
    PRIMARY KEY (stream_id, seq)
);
CREATE TABLE IF NOT EXISTS usage_bindings (
    stream_id      TEXT NOT NULL,
    seq            INTEGER NOT NULL,
    owner_seq      INTEGER NOT NULL,
    generation     INTEGER NOT NULL,
    from_offset    INTEGER NOT NULL,
    work           TEXT NOT NULL,
    account        TEXT NOT NULL,
    board_revision INTEGER,
    event_seq      INTEGER,
    captured_at    INTEGER NOT NULL,
    PRIMARY KEY (stream_id, seq)
);
CREATE TABLE IF NOT EXISTS usage_cursors (
    stream_id  TEXT PRIMARY KEY,
    checkpoint TEXT NOT NULL,
    decoder    BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_seen_ids (
    stream_id TEXT NOT NULL,
    key       TEXT NOT NULL,
    digest    INTEGER NOT NULL,
    detail    INTEGER NOT NULL,
    run_id    TEXT NOT NULL,
    hour      INTEGER NOT NULL,
    PRIMARY KEY (stream_id, key)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_observations (
    stream_id       TEXT NOT NULL,
    key             TEXT NOT NULL,
    revision        INTEGER NOT NULL,
    run_id          TEXT NOT NULL,
    scope           TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    incarnation     TEXT NOT NULL,
    launch_id       TEXT NOT NULL,
    aside_execution TEXT NOT NULL,
    unit            TEXT NOT NULL,
    reason          TEXT NOT NULL,
    turn            TEXT,
    work            TEXT NOT NULL,
    account         TEXT NOT NULL,
    model           TEXT NOT NULL,
    request_id      TEXT,
    native_ms       INTEGER,
    hour            INTEGER NOT NULL,
    observed_at     INTEGER NOT NULL,
    i INTEGER, n INTEGER, r INTEGER, w INTEGER, w5 INTEGER, w1 INTEGER, o INTEGER,
    flags           INTEGER NOT NULL,
    band            INTEGER NOT NULL,
    reasons         TEXT NOT NULL,
    generation      INTEGER NOT NULL,
    offset          INTEGER NOT NULL,
    PRIMARY KEY (stream_id, key)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS usage_observations_age ON usage_observations (observed_at);
CREATE TABLE IF NOT EXISTS usage_rollups (
    run_id          TEXT NOT NULL,
    scope           TEXT NOT NULL,
    hour            INTEGER NOT NULL,
    stream_id       TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    incarnation     TEXT NOT NULL,
    aside_execution TEXT NOT NULL,
    work            TEXT NOT NULL,
    account         TEXT NOT NULL,
    model           TEXT NOT NULL,
    unit            TEXT NOT NULL,
    reason          TEXT NOT NULL,
    flags           INTEGER NOT NULL,
    band            INTEGER NOT NULL,
    count           INTEGER NOT NULL,
    i INTEGER, n INTEGER, r INTEGER, w INTEGER, w5 INTEGER, w1 INTEGER, o INTEGER,
    min_i           INTEGER,
    max_i           INTEGER,
    PRIMARY KEY (run_id, scope, hour, stream_id, agent_id, incarnation, aside_execution,
                 work, account, model, unit, reason, flags, band)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_turns (
    stream_id   TEXT NOT NULL,
    turn        TEXT NOT NULL,
    run_id      TEXT NOT NULL,
    scope       TEXT NOT NULL,
    hour        INTEGER NOT NULL,
    incarnation TEXT NOT NULL,
    work        TEXT NOT NULL,
    PRIMARY KEY (stream_id, turn)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_coverage (
    stream_id TEXT NOT NULL,
    reason    TEXT NOT NULL,
    count     INTEGER NOT NULL,
    first_at  INTEGER NOT NULL,
    last_at   INTEGER NOT NULL,
    PRIMARY KEY (stream_id, reason)
);
CREATE TABLE IF NOT EXISTS usage_uncertain (
    run_id    TEXT NOT NULL,
    hour      INTEGER NOT NULL,
    stream_id TEXT NOT NULL,
    PRIMARY KEY (run_id, hour, stream_id)
);
CREATE TABLE IF NOT EXISTS usage_diagnostics (
    stream_id  TEXT NOT NULL,
    kind       TEXT NOT NULL,
    request_id TEXT NOT NULL,
    offset     INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    native_ms  INTEGER,
    PRIMARY KEY (stream_id, kind, request_id, generation, offset)
);
"#;

/// Where a stream's native file is opened from. Persisted for recovery,
/// never exported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Locator {
    /// The member's private home, under its agent directory.
    Private { agent_id: String },
    /// A CLI home captured at launch (the operator's own or an account's).
    CliHome { path: PathBuf },
}

/// An aside execution's identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsideIds {
    pub execution_id: String,
    pub aside_id: String,
}

/// A native session to read, and who it belongs to from now on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub run_id: String,
    pub kind: UsageKind,
    pub runtime: RuntimeKind,
    pub native_session_id: String,
    pub agent_id: String,
    pub incarnation: String,
    pub launch_id: String,
    pub aside: Option<AsideIds>,
    pub locator: Locator,
    pub started_ms: i64,
    /// Whether the session is a confirmed fresh one, read from its start.
    pub start: Start,
    pub cli_version: Option<String>,
}

/// The work a member does from a turn boundary on, captured with the board
/// evidence that agreed under the model lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkBinding {
    pub work: WorkAttribution,
    pub account: AccountAttribution,
    pub board_revision: Option<u64>,
    pub event_seq: Option<u64>,
}

/// What one commit did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Committed {
    pub revision: u64,
    pub inserted: u64,
    pub revised: u64,
    pub unchanged: u64,
    pub seeded: u64,
    pub quarantined: u64,
    pub outside_horizon: u64,
    pub refused: u64,
}

impl Committed {
    /// Whether anything a query reads changed.
    pub fn visible(&self) -> bool {
        self.inserted
            + self.revised
            + self.seeded
            + self.quarantined
            + self.outside_horizon
            + self.refused
            > 0
    }
}

/// One batch, or part of one, to commit for a stream.
pub struct Commit<'a> {
    pub stream_id: &'a str,
    /// The cursor the batch was read from; the commit is refused unless it is
    /// the stored one.
    pub from: &'a Checkpoint,
    pub to: &'a Checkpoint,
    pub decoder: &'a Adapter,
    pub items: &'a [Item],
    pub observed_at: i64,
}

/// Retention limits (`notes/usage-metrics.md`, storage).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    pub detail_ms: i64,
    pub detail_rows: u64,
    pub run_ms: i64,
    pub runs: u64,
    pub batch: usize,
}

impl Default for Retention {
    fn default() -> Self {
        const DAY: i64 = 86_400_000;
        Self {
            detail_ms: 30 * DAY,
            detail_rows: 250_000,
            run_ms: 365 * DAY,
            runs: 2_000,
            batch: 1_000,
        }
    }
}

/// The durable ledger over its own connection to the daemon's database.
pub struct Ledger {
    conn: Connection,
    /// Context-tier boundaries of the default book, which partition retained
    /// aggregates so they still price exactly.
    thresholds: Vec<u64>,
}

/// An observation's stored identity and attribution.
struct Owner {
    seq: i64,
    run_id: String,
    kind: UsageKind,
    agent_id: String,
    incarnation: String,
    launch_id: String,
    aside_execution: Option<String>,
    active: bool,
}

/// The part of an observation an aggregate is keyed and summed by.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    run_id: String,
    scope: String,
    hour: i64,
    stream_id: String,
    agent_id: String,
    incarnation: String,
    aside_execution: String,
    work: String,
    account: String,
    model: String,
    unit: String,
    reason: String,
    flags: i64,
    band: i64,
    counters: [Option<i64>; 7],
}

const HOUR: i64 = 3_600_000;

fn hour_of(ms: Option<i64>) -> i64 {
    ms.map_or(-1, |t| t - t.rem_euclid(HOUR))
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("attribution serializes")
}

/// The plain text of a unit-like enum (`request`, `counter_delta`, ...).
fn tag<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .expect("a unit enum serializes as a string")
}

fn kind_of(text: &str) -> Result<UsageKind> {
    Ok(serde_json::from_value(serde_json::Value::String(
        text.into(),
    ))?)
}

fn runtime_tag(runtime: RuntimeKind) -> String {
    tag(&runtime)
}

/// The flags of a normalized observation: which counters are reported or
/// derived, its validity and its write pricing.
fn flags(n: &Normalized) -> i64 {
    let c = &n.counters;
    let present = [
        c.input_total,
        c.input_uncached,
        c.cache_read,
        c.cache_write,
        c.cache_write_5m,
        c.cache_write_1h,
        c.output,
    ];
    let mut f = 0i64;
    for (bit, v) in present.iter().enumerate() {
        if v.is_some() {
            f |= 1 << bit;
        }
    }
    f |= (n.derived.input_total as i64) << 7;
    f |= (n.derived.input_uncached as i64) << 8;
    f |= (n.input_valid as i64) << 9;
    f |= (n.output_valid as i64) << 10;
    f |= ((n.write_kind == WriteKind::Flat) as i64) << 11;
    f
}

fn counters_of(n: &Normalized) -> [Option<i64>; 7] {
    let c = &n.counters;
    [
        c.input_total,
        c.input_uncached,
        c.cache_read,
        c.cache_write,
        c.cache_write_5m,
        c.cache_write_1h,
        c.output,
    ]
    .map(|v| v.map(|v| v as i64))
}

/// A normalized observation from stored flags and (possibly summed) counters.
fn normalized(flags: i64, sums: [Option<i64>; 7]) -> Normalized {
    let get = |i: usize| sums[i].map(|v| v as u64);
    Normalized {
        counters: Counters {
            input_total: get(0),
            input_uncached: get(1),
            cache_read: get(2),
            cache_write: get(3),
            cache_write_5m: get(4),
            cache_write_1h: get(5),
            output: get(6),
        },
        derived: Derived {
            input_total: flags & (1 << 7) != 0,
            input_uncached: flags & (1 << 8) != 0,
        },
        write_kind: if flags & (1 << 11) != 0 {
            WriteKind::Flat
        } else {
            WriteKind::Buckets
        },
        input_valid: flags & (1 << 9) != 0,
        output_valid: flags & (1 << 10) != 0,
        invalid: Vec::new(),
    }
}

/// A stable digest of what an observation says, kept with its seen id after
/// its detail expires, so an identical replay is recognized.
fn digest(o: &Observation) -> i64 {
    let mut h = Sha256::new();
    h.update(tag(&o.unit));
    h.update(b"\0");
    h.update(o.model.as_deref().unwrap_or(""));
    h.update(b"\0");
    h.update(flags(&o.normalized).to_le_bytes());
    for v in counters_of(&o.normalized) {
        h.update(v.unwrap_or(-1).to_le_bytes());
    }
    let bytes = h.finalize();
    i64::from_le_bytes(bytes[..8].try_into().expect("eight bytes"))
}

/// Why an unusable observation is unusable, for coverage.
fn unusable_reason(o: &Observation) -> String {
    if o.unit == ObservationUnit::Unusable {
        o.reasons
            .first()
            .map(tag)
            .unwrap_or_else(|| tag(&CoverageReason::InvalidCounter))
    } else {
        String::new()
    }
}

fn unknown_work() -> WorkAttribution {
    WorkAttribution::Unknown {
        reason: CoverageReason::UnknownAttribution,
    }
}

fn unknown_account() -> AccountAttribution {
    AccountAttribution::Unknown {
        reason: CoverageReason::UnknownAccount,
    }
}

fn add(a: Option<i64>, b: Option<i64>, sign: i64) -> Result<Option<i64>> {
    match (a, b) {
        (Some(a), Some(b)) => Ok(Some(
            b.checked_mul(sign)
                .and_then(|b| a.checked_add(b))
                .context("usage aggregate overflow")?,
        )),
        (None, Some(b)) if sign > 0 => Ok(Some(b)),
        (a, None) => Ok(a),
        (None, Some(_)) => bail!("usage aggregate lost a contribution"),
    }
}

impl Ledger {
    /// The ledger on its own connection to the database at `path`.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening usage ledger {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        Self::new(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::new(Connection::open_in_memory()?)
    }

    fn new(conn: Connection) -> Result<Self> {
        migrate(&conn)?;
        let mut thresholds: Vec<u64> = Book::standard_2026_10_08()
            .rates
            .iter()
            .filter_map(|r| r.above_input_tokens)
            .collect();
        thresholds.sort_unstable();
        thresholds.dedup();
        Ok(Self { conn, thresholds })
    }

    /// The metrics revision: bumped by every visible change.
    pub fn revision(&self) -> Result<u64> {
        revision(&self.conn)
    }

    /// Starts tracking a workspace run. Idempotent: a known run keeps its
    /// recorded start.
    pub fn begin_run(&mut self, run_id: &str, started_at: Option<i64>, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO usage_runs (run_id, started_at, tracking_since, build)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(run_id) DO NOTHING",
            params![run_id, started_at, now, env!("CARGO_PKG_VERSION")],
        )?;
        Ok(())
    }

    /// Ends a run: its streams stop, its history stays.
    pub fn end_run(&mut self, run_id: &str, now: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE usage_runs SET ended_at = ?2 WHERE run_id = ?1 AND ended_at IS NULL",
            params![run_id, now],
        )?;
        tx.execute(
            "UPDATE usage_owners SET active = 0 WHERE run_id = ?1",
            params![run_id],
        )?;
        bump(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Registers `reg`'s native session, now open as `file`, and returns its
    /// stream id. The same native session registered again continues its
    /// stream (cursor and seen ids), with a new owner from the file's current
    /// end; a session still owned by another active incarnation is refused.
    pub fn register(
        &mut self,
        reg: &Registration,
        file: &File,
        now: i64,
    ) -> Result<std::result::Result<String, CoverageReason>> {
        let Some(session) = source::ident(&reg.native_session_id) else {
            return Ok(Err(CoverageReason::IdentityLost));
        };
        let adapter = match Adapter::new(reg.runtime, &session, reg.start) {
            Ok(a) => a,
            Err(reason) => return Ok(Err(reason)),
        };
        let stream_id = format!("{}:{session}", runtime_tag(reg.runtime));
        let tx = self.conn.transaction()?;
        let cursor = load_cursor(&tx, &stream_id)?;
        let position = match &cursor {
            None => {
                let cp = Checkpoint::new(file, reg.start)?;
                tx.execute(
                    "INSERT INTO usage_streams
                     (stream_id, runtime, native_session_id, locator, started_ms, registered_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        stream_id,
                        runtime_tag(reg.runtime),
                        session,
                        json(&reg.locator),
                        reg.started_ms,
                        now
                    ],
                )?;
                save_cursor(&tx, &stream_id, &cp, &adapter)?;
                (0, 0)
            }
            Some((cp, _)) => {
                if let Some(owner) = current_owner(&tx, &stream_id)? {
                    if owner.active && owner.incarnation != reg.incarnation {
                        return Ok(Err(CoverageReason::AmbiguousBoundary));
                    }
                }
                tx.execute(
                    "UPDATE usage_owners SET active = 0 WHERE stream_id = ?1",
                    params![stream_id],
                )?;
                position(file, cp)?
            }
        };
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM usage_owners WHERE stream_id = ?1",
            params![stream_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO usage_owners
             (stream_id, seq, generation, from_offset, run_id, kind, agent_id, incarnation,
              launch_id, aside_execution, aside_id, cli_version, active, registered_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13)",
            params![
                stream_id,
                seq,
                position.0 as i64,
                position.1 as i64,
                reg.run_id,
                tag(&reg.kind),
                reg.agent_id,
                reg.incarnation,
                reg.launch_id,
                reg.aside.as_ref().map(|a| &a.execution_id),
                reg.aside.as_ref().map(|a| &a.aside_id),
                reg.cli_version,
                now
            ],
        )?;
        bump(&tx)?;
        tx.commit()?;
        Ok(Ok(stream_id))
    }

    /// The owner stopped: its stream drains but takes no new owner's work.
    pub fn release(&mut self, stream_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE usage_owners SET active = 0 WHERE stream_id = ?1",
            params![stream_id],
        )?;
        Ok(())
    }

    /// Captures `binding` at a turn boundary: usage first read at or after
    /// the file's current end belongs to it. `file` is the stream's source,
    /// open now.
    pub fn bind_work(
        &mut self,
        stream_id: &str,
        file: &File,
        binding: &WorkBinding,
        now: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let Some((cp, _)) = load_cursor(&tx, stream_id)? else {
            bail!("unknown usage stream {stream_id}");
        };
        let owner = current_owner(&tx, stream_id)?.context("usage stream has no owner")?;
        let (generation, offset) = position(file, &cp)?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM usage_bindings WHERE stream_id = ?1",
            params![stream_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO usage_bindings
             (stream_id, seq, owner_seq, generation, from_offset, work, account,
              board_revision, event_seq, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                stream_id,
                seq,
                owner.seq,
                generation as i64,
                offset as i64,
                json(&binding.work),
                json(&binding.account),
                binding.board_revision.map(|v| v as i64),
                binding.event_seq.map(|v| v as i64),
                now
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The stored cursor and decoder of a stream.
    pub fn cursor(&self, stream_id: &str) -> Result<Option<(Checkpoint, Adapter)>> {
        let row: Option<(String, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT checkpoint, decoder FROM usage_cursors WHERE stream_id = ?1",
                params![stream_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(cp, decoder)| {
            Ok((
                serde_json::from_str(&cp)?,
                serde_json::from_slice(&decoder)?,
            ))
        })
        .transpose()
    }

    /// Stops admitting usage from a stream, with the reason as a gap: when
    /// its exact seen-id membership cannot be kept.
    pub fn close_admission(
        &mut self,
        stream_id: &str,
        reason: CoverageReason,
        now: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE usage_streams SET admission = ?2 WHERE stream_id = ?1",
            params![stream_id, tag(&reason)],
        )?;
        cover(&tx, stream_id, &tag(&reason), 1, now)?;
        bump(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Commits one batch (or part of one) of a stream in one transaction.
    pub fn commit(&mut self, c: Commit<'_>) -> Result<Committed> {
        let changes = c
            .items
            .iter()
            .filter(|i| matches!(i, Item::Observation(_)))
            .count();
        if changes > MAX_CHANGES {
            bail!("{changes} usage changes exceed one transaction's {MAX_CHANGES}");
        }
        let thresholds = self.thresholds.clone();
        let tx = self.conn.transaction()?;
        let Some((stored, _)) = load_cursor(&tx, c.stream_id)? else {
            bail!("unknown usage stream {}", c.stream_id);
        };
        if &stored != c.from {
            bail!("stale usage cursor for {}", c.stream_id);
        }
        let admission: Option<String> = tx.query_row(
            "SELECT admission FROM usage_streams WHERE stream_id = ?1",
            params![c.stream_id],
            |r| r.get(0),
        )?;
        let mut done = Committed::default();
        for item in c.items {
            match item {
                Item::Observation(o) => {
                    if let Some(reason) = &admission {
                        cover(&tx, c.stream_id, reason, 1, c.observed_at)?;
                        done.refused += 1;
                        continue;
                    }
                    apply(&tx, c.stream_id, o, c.observed_at, &thresholds, &mut done)?;
                }
                Item::Skipped { skip, .. } => {
                    if let Some(reason) = skip.reason() {
                        cover(&tx, c.stream_id, &tag(&reason), 1, c.observed_at)?;
                        done.refused += 1;
                    }
                }
                Item::Gap { reason, .. } => {
                    cover(&tx, c.stream_id, &tag(reason), 1, c.observed_at)?;
                    done.refused += 1;
                }
                Item::Signal { signal, offset } => {
                    let (kind, request_id, native_ms) = match signal {
                        Signal::HistoryChanged { request_id } => {
                            ("history_changed", request_id.as_str(), None)
                        }
                        Signal::Compaction { native_ms } => ("compaction", "", *native_ms),
                        Signal::TurnEnd { .. } => continue,
                    };
                    tx.execute(
                        "INSERT OR IGNORE INTO usage_diagnostics
                         (stream_id, kind, request_id, offset, generation, native_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            c.stream_id,
                            kind,
                            request_id,
                            *offset as i64,
                            c.to.generation as i64,
                            native_ms
                        ],
                    )?;
                }
            }
        }
        save_cursor(&tx, c.stream_id, c.to, c.decoder)?;
        if done.visible() {
            bump(&tx)?;
        }
        done.revision = revision(&tx)?;
        tx.commit()?;
        Ok(done)
    }

    /// Facts for metrics from the retained aggregates of a run and scope
    /// (`live`, `aside` or `pre_tracking`), optionally within `[from, to)`
    /// hours and narrowed by `keep`. Aggregates outlive detail, so the same
    /// figures come back after detail expires.
    pub fn facts(
        &self,
        run_id: &str,
        scope: &str,
        range: Option<(i64, i64)>,
        keep: impl Fn(&Aggregate) -> bool,
    ) -> Result<Vec<Fact>> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, agent_id, incarnation, aside_execution, work, account, model,
                    unit, reason, flags, count, i, n, r, w, w5, w1, o, min_i, max_i, hour
             FROM usage_rollups WHERE run_id = ?1 AND scope = ?2",
        )?;
        let rows = stmt.query_map(params![run_id, scope], |r| {
            Ok((
                Aggregate {
                    stream_id: r.get(0)?,
                    agent_id: r.get(1)?,
                    incarnation: r.get(2)?,
                    aside_execution: r.get(3)?,
                    work: r.get(4)?,
                    account: r.get(5)?,
                    model: r.get(6)?,
                    unit: r.get(7)?,
                    reason: r.get(8)?,
                    hour: r.get(20)?,
                },
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
                [
                    r.get::<_, Option<i64>>(11)?,
                    r.get(12)?,
                    r.get(13)?,
                    r.get(14)?,
                    r.get(15)?,
                    r.get(16)?,
                    r.get(17)?,
                ],
                (r.get::<_, Option<i64>>(18)?, r.get::<_, Option<i64>>(19)?),
            ))
        })?;
        let mut facts = Vec::new();
        for row in rows {
            let (agg, flags, count, sums, (min_i, max_i)) = row?;
            if let Some((from, to)) = range {
                if agg.hour < from || agg.hour >= to {
                    continue;
                }
            }
            if !keep(&agg) {
                continue;
            }
            let unit: ObservationUnit =
                serde_json::from_value(serde_json::Value::String(agg.unit.clone()))?;
            let unusable = (!agg.reason.is_empty())
                .then(|| serde_json::from_value(serde_json::Value::String(agg.reason.clone())))
                .transpose()?;
            facts.push(Fact {
                unit,
                normalized: normalized(flags, sums),
                model: (!agg.model.is_empty()).then(|| agg.model.clone()),
                agent: agg.incarnation.clone(),
                source: agg.stream_id.clone(),
                turn: None,
                unusable,
                weight: count as u64,
                input_range: min_i.zip(max_i).map(|(a, b)| (a as u64, b as u64)),
            });
        }
        Ok(facts)
    }

    /// Distinct native turns of a run and scope, optionally within hours.
    pub fn turns(&self, run_id: &str, scope: &str, range: Option<(i64, i64)>) -> Result<u64> {
        let (from, to) = range.unwrap_or((i64::MIN, i64::MAX));
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM usage_turns
             WHERE run_id = ?1 AND scope = ?2 AND hour >= ?3 AND hour < ?4",
            params![run_id, scope, from, to],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Coverage a stream recorded: reason and count.
    pub fn coverage(&self, stream_id: &str) -> Result<BTreeMap<String, u64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT reason, count FROM usage_coverage WHERE stream_id = ?1")?;
        let rows = stmt.query_map(params![stream_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether some retained aggregate of a run can no longer be trusted
    /// because a correction arrived after its detail expired.
    pub fn uncertain(&self, run_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM usage_uncertain WHERE run_id = ?1)",
            params![run_id],
            |r| r.get(0),
        )?)
    }

    /// The retained detail rows of a stream, in file order.
    pub fn detail(&self, stream_id: &str) -> Result<Vec<Detail>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, revision, unit, run_id, work FROM usage_observations
             WHERE stream_id = ?1 ORDER BY generation, offset",
        )?;
        let rows = stmt.query_map(params![stream_id], |r| {
            Ok(Detail {
                key: r.get(0)?,
                revision: r.get::<_, i64>(1)? as u64,
                unit: r.get(2)?,
                run_id: r.get(3)?,
                work: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Prunes in bounded batches: detail first (keeping seen ids and the
    /// aggregates), then whole closed runs. Returns the rows removed.
    pub fn expire(&mut self, now: i64, policy: Retention) -> Result<u64> {
        let mut removed = 0u64;
        loop {
            let tx = self.conn.transaction()?;
            let total: i64 =
                tx.query_row("SELECT COUNT(*) FROM usage_observations", [], |r| r.get(0))?;
            let over = (total as u64).saturating_sub(policy.detail_rows);
            let cutoff = now - policy.detail_ms;
            let victims: Vec<(String, String, i64, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT stream_id, key, observed_at, run_id FROM usage_observations
                     ORDER BY observed_at LIMIT ?1",
                )?;
                let limit = policy.batch as i64;
                let rows = stmt.query_map(params![limit], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
                    .into_iter()
                    .enumerate()
                    .filter(|(i, v)| v.2 < cutoff || (*i as u64) < over)
                    .map(|(_, v)| v)
                    .collect()
            };
            if victims.is_empty() {
                break;
            }
            for (stream_id, key, observed_at, run_id) in &victims {
                tx.execute(
                    "DELETE FROM usage_observations WHERE stream_id = ?1 AND key = ?2",
                    params![stream_id, key],
                )?;
                tx.execute(
                    "UPDATE usage_seen_ids SET detail = 0 WHERE stream_id = ?1 AND key = ?2",
                    params![stream_id, key],
                )?;
                tx.execute(
                    "UPDATE usage_runs SET retained_from = MAX(COALESCE(retained_from, 0), ?2)
                     WHERE run_id = ?1",
                    params![run_id, observed_at + 1],
                )?;
            }
            removed += victims.len() as u64;
            bump(&tx)?;
            tx.commit()?;
        }
        // Whole runs that ended long ago, or beyond the run limit.
        let tx = self.conn.transaction()?;
        let old: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT run_id FROM usage_runs WHERE ended_at IS NOT NULL
                 ORDER BY ended_at DESC",
            )?;
            let ended: Vec<(String,)> = stmt
                .query_map([], |r| Ok((r.get(0)?,)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut ages = tx.prepare("SELECT ended_at FROM usage_runs WHERE run_id = ?1")?;
            let mut old = Vec::new();
            for (i, (run,)) in ended.into_iter().enumerate() {
                let ended_at: i64 = ages.query_row(params![run], |r| r.get(0))?;
                if ended_at < now - policy.run_ms || i as u64 >= policy.runs {
                    old.push(run);
                }
            }
            old
        };
        for run in old.iter().take(policy.batch) {
            tx.execute(
                "UPDATE usage_seen_ids SET detail = 0 WHERE run_id = ?1",
                params![run],
            )?;
            for table in [
                "usage_rollups",
                "usage_turns",
                "usage_uncertain",
                "usage_observations",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE run_id = ?1"),
                    params![run],
                )?;
            }
            // Seen ids stay while their stream still admits appends.
            tx.execute(
                "DELETE FROM usage_seen_ids WHERE run_id = ?1 AND stream_id IN
                 (SELECT stream_id FROM usage_streams WHERE admission IS NOT NULL)",
                params![run],
            )?;
            tx.execute("DELETE FROM usage_runs WHERE run_id = ?1", params![run])?;
            removed += 1;
        }
        if !old.is_empty() {
            bump(&tx)?;
        }
        tx.commit()?;
        Ok(removed)
    }
}

/// One retained detail row of a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub key: String,
    pub revision: u64,
    pub unit: String,
    pub run_id: String,
    /// The captured work binding, as JSON of `WorkAttribution`.
    pub work: String,
}

/// The key of a retained aggregate, for narrowing facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aggregate {
    pub stream_id: String,
    pub agent_id: String,
    pub incarnation: String,
    pub aside_execution: String,
    /// The captured work binding, as JSON of `WorkAttribution`.
    pub work: String,
    /// The captured account, as JSON of `AccountAttribution`.
    pub account: String,
    pub model: String,
    pub unit: String,
    pub reason: String,
    /// Hour start in milliseconds; -1 when the time is unknown.
    pub hour: i64,
}

impl Aggregate {
    pub fn work(&self) -> Option<WorkAttribution> {
        serde_json::from_str(&self.work).ok()
    }
}

/// Creates or upgrades the usage tables. Additive and repeatable.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)
        .context("creating usage tables")?;
    let version: Option<i64> = conn
        .query_row("SELECT version FROM usage_schema", [], |r| r.get(0))
        .optional()?;
    match version {
        None => {
            conn.execute(
                "INSERT INTO usage_schema (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )?;
        }
        Some(v) if v > SCHEMA_VERSION => {
            bail!("usage tables are version {v}, newer than this build's {SCHEMA_VERSION}")
        }
        Some(_) => {}
    }
    Ok(())
}

fn revision(conn: &Connection) -> Result<u64> {
    let v: Option<i64> = conn
        .query_row(
            "SELECT value FROM usage_state WHERE key = 'revision'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.unwrap_or(0) as u64)
}

fn bump(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "INSERT INTO usage_state (key, value) VALUES ('revision', 1)
         ON CONFLICT(key) DO UPDATE SET value = value + 1",
        [],
    )?;
    Ok(())
}

fn load_cursor(tx: &Connection, stream_id: &str) -> Result<Option<(Checkpoint, Vec<u8>)>> {
    let row: Option<(String, Vec<u8>)> = tx
        .query_row(
            "SELECT checkpoint, decoder FROM usage_cursors WHERE stream_id = ?1",
            params![stream_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(cp, d)| Ok((serde_json::from_str(&cp)?, d)))
        .transpose()
}

fn save_cursor(tx: &Connection, stream_id: &str, cp: &Checkpoint, decoder: &Adapter) -> Result<()> {
    let state = decoder
        .state()
        .map_err(|reason| anyhow::anyhow!("usage decoder state: {reason:?}"))?;
    tx.execute(
        "INSERT INTO usage_cursors (stream_id, checkpoint, decoder) VALUES (?1, ?2, ?3)
         ON CONFLICT(stream_id) DO UPDATE SET checkpoint = excluded.checkpoint,
                                              decoder = excluded.decoder",
        params![stream_id, serde_json::to_string(cp)?, state],
    )?;
    Ok(())
}

/// Where the file now ends, in cursor terms: its current length in the
/// cursor's generation, or the start of the next generation when another
/// file now has the name.
fn position(file: &File, cp: &Checkpoint) -> Result<(u64, u64)> {
    let meta = file.metadata()?;
    if (meta.dev(), meta.ino()) == (cp.dev, cp.ino) && meta.len() >= cp.offset {
        Ok((cp.generation, meta.len()))
    } else {
        Ok((cp.generation + 1, 0))
    }
}

fn owner_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Owner> {
    let kind: String = r.get(2)?;
    Ok(Owner {
        seq: r.get(0)?,
        run_id: r.get(1)?,
        kind: kind_of(&kind).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, e.into())
        })?,
        agent_id: r.get(3)?,
        incarnation: r.get(4)?,
        launch_id: r.get(5)?,
        aside_execution: r.get(6)?,
        active: r.get::<_, i64>(7)? != 0,
    })
}

const OWNER_COLUMNS: &str =
    "seq, run_id, kind, agent_id, incarnation, launch_id, aside_execution, active";

fn current_owner(tx: &Connection, stream_id: &str) -> Result<Option<Owner>> {
    Ok(tx
        .query_row(
            &format!(
                "SELECT {OWNER_COLUMNS} FROM usage_owners WHERE stream_id = ?1
             ORDER BY seq DESC LIMIT 1"
            ),
            params![stream_id],
            owner_from,
        )
        .optional()?)
}

/// The owner in force at a position: the latest registered at or before it.
fn owner_at(
    tx: &Connection,
    stream_id: &str,
    generation: u64,
    offset: u64,
) -> Result<Option<Owner>> {
    Ok(tx
        .query_row(
            &format!(
                "SELECT {OWNER_COLUMNS} FROM usage_owners WHERE stream_id = ?1
             AND (generation < ?2 OR (generation = ?2 AND from_offset <= ?3))
             ORDER BY generation DESC, from_offset DESC, seq DESC LIMIT 1"
            ),
            params![stream_id, generation as i64, offset as i64],
            owner_from,
        )
        .optional()?)
}

/// The work binding in force at a position, captured under that owner.
fn binding_at(
    tx: &Connection,
    stream_id: &str,
    owner_seq: i64,
    generation: u64,
    offset: u64,
) -> Result<Option<(String, String)>> {
    Ok(tx
        .query_row(
            "SELECT work, account FROM usage_bindings WHERE stream_id = ?1 AND owner_seq = ?2
             AND (generation < ?3 OR (generation = ?3 AND from_offset <= ?4))
             ORDER BY generation DESC, from_offset DESC, seq DESC LIMIT 1",
            params![stream_id, owner_seq, generation as i64, offset as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

fn cover(tx: &Connection, stream_id: &str, reason: &str, n: u64, now: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO usage_coverage (stream_id, reason, count, first_at, last_at)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(stream_id, reason) DO UPDATE SET count = count + excluded.count,
                                                      last_at = excluded.last_at",
        params![stream_id, reason, n as i64, now],
    )?;
    Ok(())
}

fn band(o: &Observation, thresholds: &[u64]) -> i64 {
    match (
        o.unit,
        o.normalized.input_valid,
        o.normalized.counters.input_total,
    ) {
        (ObservationUnit::Request, true, Some(i)) => {
            thresholds.iter().filter(|t| i > **t).count() as i64
        }
        _ => -1,
    }
}

/// Applies one observation: a new identity, a revision of a retained one, an
/// identical replay, a conflicting ambiguous replay (quarantined), or an
/// identity whose detail expired (outside the correction horizon).
fn apply(
    tx: &Transaction<'_>,
    stream_id: &str,
    o: &Observation,
    now: i64,
    thresholds: &[u64],
    done: &mut Committed,
) -> Result<()> {
    let seen: Option<(i64, bool, String, i64)> = tx
        .query_row(
            "SELECT digest, detail, run_id, hour FROM usage_seen_ids
             WHERE stream_id = ?1 AND key = ?2",
            params![stream_id, o.key],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let digest = digest(o);
    match seen {
        Some((old, _, _, _)) if old == digest => {
            done.unchanged += 1;
        }
        Some((_, _, _, _)) if o.phase == Phase::Seed => {
            // A replayed prefix whose values differ: not evidence enough.
            cover(
                tx,
                stream_id,
                &tag(&CoverageReason::AmbiguousBoundary),
                1,
                now,
            )?;
            done.quarantined += 1;
        }
        Some((_, false, run_id, hour)) => {
            // The original's detail expired: its retained contribution can no
            // longer be corrected, so the aggregates it is in become uncertain.
            cover(
                tx,
                stream_id,
                &tag(&CoverageReason::CorrectionOutsideHorizon),
                1,
                now,
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO usage_uncertain (run_id, hour, stream_id) VALUES (?1, ?2, ?3)",
                params![run_id, hour, stream_id],
            )?;
            done.outside_horizon += 1;
        }
        Some((_, true, _, _)) => {
            let old = stored_row(tx, stream_id, &o.key)?;
            let mut new = old.clone();
            new.counters = counters_of(&o.normalized);
            new.flags = flags(&o.normalized);
            new.band = band(o, thresholds);
            new.unit = tag(&o.unit);
            new.reason = unusable_reason(o);
            new.model = o.model.clone().unwrap_or_default();
            rollup(tx, &old, -1)?;
            rollup(tx, &new, 1)?;
            let c = &new.counters;
            tx.execute(
                "UPDATE usage_observations SET revision = revision + 1, unit = ?3, reason = ?4,
                 model = ?5, i = ?6, n = ?7, r = ?8, w = ?9, w5 = ?10, w1 = ?11, o = ?12,
                 flags = ?13, band = ?14, reasons = ?15
                 WHERE stream_id = ?1 AND key = ?2",
                params![
                    stream_id,
                    o.key,
                    new.unit,
                    new.reason,
                    new.model,
                    c[0],
                    c[1],
                    c[2],
                    c[3],
                    c[4],
                    c[5],
                    c[6],
                    new.flags,
                    new.band,
                    json(&o.reasons)
                ],
            )?;
            tx.execute(
                "UPDATE usage_seen_ids SET digest = ?3 WHERE stream_id = ?1 AND key = ?2",
                params![stream_id, o.key, digest],
            )?;
            done.revised += 1;
        }
        None => {
            let Some(owner) = owner_at(tx, stream_id, o.generation, o.offset)? else {
                bail!("usage stream {stream_id} has no owner");
            };
            let (work, account) = binding_at(tx, stream_id, owner.seq, o.generation, o.offset)?
                .unwrap_or_else(|| (json(&unknown_work()), json(&unknown_account())));
            let scope = match o.phase {
                Phase::Seed => "pre_tracking".to_string(),
                Phase::Charge => tag(&owner.kind),
            };
            let row = Row {
                run_id: owner.run_id.clone(),
                scope,
                hour: hour_of(o.native_ms),
                stream_id: stream_id.to_string(),
                agent_id: owner.agent_id.clone(),
                incarnation: owner.incarnation.clone(),
                aside_execution: owner.aside_execution.clone().unwrap_or_default(),
                work,
                account,
                model: o.model.clone().unwrap_or_default(),
                unit: tag(&o.unit),
                reason: unusable_reason(o),
                flags: flags(&o.normalized),
                band: band(o, thresholds),
                counters: counters_of(&o.normalized),
            };
            let c = &row.counters;
            tx.execute(
                "INSERT INTO usage_observations
                 (stream_id, key, revision, run_id, scope, agent_id, incarnation, launch_id,
                  aside_execution, unit, reason, turn, work, account, model, request_id,
                  native_ms, hour, observed_at, i, n, r, w, w5, w1, o, flags, band, reasons,
                  generation, offset)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28,
                         ?29, ?30)",
                params![
                    stream_id,
                    o.key,
                    row.run_id,
                    row.scope,
                    row.agent_id,
                    row.incarnation,
                    owner.launch_id,
                    row.aside_execution,
                    row.unit,
                    row.reason,
                    o.turn,
                    row.work,
                    row.account,
                    row.model,
                    o.request_id,
                    o.native_ms,
                    row.hour,
                    now,
                    c[0],
                    c[1],
                    c[2],
                    c[3],
                    c[4],
                    c[5],
                    c[6],
                    row.flags,
                    row.band,
                    json(&o.reasons),
                    o.generation as i64,
                    o.offset as i64
                ],
            )?;
            tx.execute(
                "INSERT INTO usage_seen_ids (stream_id, key, digest, detail, run_id, hour)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                params![stream_id, o.key, digest, row.run_id, row.hour],
            )?;
            if let Some(turn) = &o.turn {
                tx.execute(
                    "INSERT OR IGNORE INTO usage_turns
                     (stream_id, turn, run_id, scope, hour, incarnation, work)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        stream_id,
                        format!("{}:{turn}", o.generation),
                        row.run_id,
                        row.scope,
                        row.hour,
                        row.incarnation,
                        row.work
                    ],
                )?;
            }
            rollup(tx, &row, 1)?;
            if o.phase == Phase::Seed {
                done.seeded += 1;
            } else {
                done.inserted += 1;
            }
        }
    }
    Ok(())
}

fn stored_row(tx: &Connection, stream_id: &str, key: &str) -> Result<Row> {
    Ok(tx.query_row(
        "SELECT run_id, scope, hour, agent_id, incarnation, aside_execution, work, account,
                model, unit, reason, flags, band, i, n, r, w, w5, w1, o
         FROM usage_observations WHERE stream_id = ?1 AND key = ?2",
        params![stream_id, key],
        |r| {
            Ok(Row {
                run_id: r.get(0)?,
                scope: r.get(1)?,
                hour: r.get(2)?,
                stream_id: stream_id.to_string(),
                agent_id: r.get(3)?,
                incarnation: r.get(4)?,
                aside_execution: r.get(5)?,
                work: r.get(6)?,
                account: r.get(7)?,
                model: r.get(8)?,
                unit: r.get(9)?,
                reason: r.get(10)?,
                flags: r.get(11)?,
                band: r.get(12)?,
                counters: [
                    r.get(13)?,
                    r.get(14)?,
                    r.get(15)?,
                    r.get(16)?,
                    r.get(17)?,
                    r.get(18)?,
                    r.get(19)?,
                ],
            })
        },
    )?)
}

/// Adds (`sign` 1) or removes (-1) one observation's contribution to its
/// aggregate, with checked arithmetic.
fn rollup(tx: &Connection, row: &Row, sign: i64) -> Result<()> {
    const KEY: &str = "run_id = ?1 AND scope = ?2 AND hour = ?3 AND stream_id = ?4
        AND agent_id = ?5 AND incarnation = ?6 AND aside_execution = ?7 AND work = ?8
        AND account = ?9 AND model = ?10 AND unit = ?11 AND reason = ?12 AND flags = ?13
        AND band = ?14";
    let key = params![
        row.run_id,
        row.scope,
        row.hour,
        row.stream_id,
        row.agent_id,
        row.incarnation,
        row.aside_execution,
        row.work,
        row.account,
        row.model,
        row.unit,
        row.reason,
        row.flags,
        row.band
    ];
    type Stored = (i64, [Option<i64>; 7], Option<i64>, Option<i64>);
    let existing: Option<Stored> = tx
        .query_row(
            &format!(
                "SELECT count, i, n, r, w, w5, w1, o, min_i, max_i FROM usage_rollups WHERE {KEY}"
            ),
            key,
            |r| {
                Ok((
                    r.get(0)?,
                    [
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ],
                    r.get(8)?,
                    r.get(9)?,
                ))
            },
        )
        .optional()?;
    let (count, sums, min_i, max_i) = existing.unwrap_or((0, [None; 7], None, None));
    let count = count + sign;
    if count < 0 {
        bail!("usage aggregate count below zero");
    }
    if count == 0 {
        tx.execute(&format!("DELETE FROM usage_rollups WHERE {KEY}"), key)?;
        return Ok(());
    }
    let mut next = [None; 7];
    for (i, slot) in next.iter_mut().enumerate() {
        *slot = add(sums[i], row.counters[i], sign)?;
    }
    // The input range only widens: a removed request leaves it conservative.
    let request = row.unit == "request";
    let i = row.counters[0].filter(|_| request);
    let min_i = match (min_i, i) {
        (Some(a), Some(b)) if sign > 0 => Some(a.min(b)),
        (None, Some(b)) if sign > 0 => Some(b),
        (a, _) => a,
    };
    let max_i = match (max_i, i) {
        (Some(a), Some(b)) if sign > 0 => Some(a.max(b)),
        (None, Some(b)) if sign > 0 => Some(b),
        (a, _) => a,
    };
    tx.execute(
        "INSERT INTO usage_rollups
         (run_id, scope, hour, stream_id, agent_id, incarnation, aside_execution, work, account,
          model, unit, reason, flags, band, count, i, n, r, w, w5, w1, o, min_i, max_i)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21, ?22, ?23, ?24)
         ON CONFLICT DO UPDATE SET count = excluded.count, i = excluded.i, n = excluded.n,
           r = excluded.r, w = excluded.w, w5 = excluded.w5, w1 = excluded.w1, o = excluded.o,
           min_i = excluded.min_i, max_i = excluded.max_i",
        params![
            row.run_id,
            row.scope,
            row.hour,
            row.stream_id,
            row.agent_id,
            row.incarnation,
            row.aside_execution,
            row.work,
            row.account,
            row.model,
            row.unit,
            row.reason,
            row.flags,
            row.band,
            count,
            next[0],
            next[1],
            next[2],
            next[3],
            next[4],
            next[5],
            next[6],
            min_i,
            max_i
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_metrics::claude::tests::assistant;
    use crate::usage_metrics::collect::{drain, pump};
    use crate::usage_metrics::metrics::totals;
    use crew_core::rpc::features::Wide;
    use crew_core::rpc::usage_metrics::{EpicAttribution, UsageTotals};
    use serde_json::{json, Value};
    use std::io::Write;
    use std::path::Path;

    const NOW: i64 = 1_791_417_600_000;
    const DAY: i64 = 86_400_000;

    fn usage(n: u64, r: u64, w: u64, o: u64) -> Value {
        json!({"input_tokens": n, "cache_read_input_tokens": r, "cache_creation_input_tokens": w, "output_tokens": o})
    }

    fn msg(id: &str, o: u64) -> Value {
        assistant(id, NOW, usage(10, 80, 10, o))
    }

    fn text(values: &[Value]) -> String {
        values.iter().map(|v| format!("{v}\n")).collect()
    }

    fn append(path: &Path, values: &[Value]) {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(text(values).as_bytes())
            .unwrap();
    }

    /// Writes `values` as a new file under `path`'s name.
    fn replace(path: &Path, values: &[Value]) {
        let tmp = path.with_extension("new");
        std::fs::write(&tmp, text(values)).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    fn reg(run: &str, incarnation: &str, start: Start) -> Registration {
        Registration {
            run_id: run.into(),
            kind: UsageKind::Live,
            runtime: RuntimeKind::Claude,
            native_session_id: "s-1".into(),
            agent_id: "worker".into(),
            incarnation: incarnation.into(),
            launch_id: format!("launch-{incarnation}"),
            aside: None,
            locator: Locator::Private {
                agent_id: "worker".into(),
            },
            started_ms: NOW,
            start,
            cli_version: None,
        }
    }

    fn open(path: &Path) -> File {
        File::open(path).unwrap()
    }

    fn sum(ledger: &Ledger, run: &str, scope: &str) -> UsageTotals {
        let facts = ledger.facts(run, scope, None, |_| true).unwrap();
        totals(&facts, &Book::standard_2026_10_08(), None)
    }

    fn task(id: &str) -> WorkBinding {
        WorkBinding {
            work: WorkAttribution::Task {
                id: id.into(),
                epic: EpicAttribution::NoEpic,
            },
            account: unknown_account(),
            board_revision: Some(1),
            event_seq: Some(1),
        }
    }

    /// Every usage table, row by row, for comparing two ledgers.
    fn dump(ledger: &Ledger) -> Vec<String> {
        let mut out = Vec::new();
        for table in [
            "usage_runs",
            "usage_streams",
            "usage_owners",
            "usage_bindings",
            "usage_cursors",
            "usage_seen_ids",
            "usage_observations",
            "usage_rollups",
            "usage_turns",
            "usage_coverage",
            "usage_uncertain",
            "usage_diagnostics",
            "usage_state",
        ] {
            let mut stmt = ledger
                .conn
                .prepare(&format!("SELECT * FROM {table}"))
                .unwrap();
            let columns = stmt.column_count();
            let mut rows: Vec<String> = stmt
                .query_map([], |r| {
                    let mut row = vec![table.to_string()];
                    for i in 0..columns {
                        let v: rusqlite::types::Value = r.get(i)?;
                        row.push(format!("{v:?}"));
                    }
                    Ok(row.join("|"))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows.sort();
            out.extend(rows);
        }
        out
    }

    fn setup(values: &[Value]) -> (tempfile::TempDir, PathBuf, Ledger, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s-1.jsonl");
        std::fs::write(&path, text(values)).unwrap();
        let mut ledger = Ledger::in_memory().unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let stream = ledger
            .register(&reg("r1", "i1", Start::Fresh), &open(&path), NOW)
            .unwrap()
            .unwrap();
        (dir, path, ledger, stream)
    }

    #[test]
    fn streamed_copies_replace_their_aggregate_contribution_once() {
        let (_dir, path, mut ledger, stream) = setup(&[msg("a", 5)]);
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.inserted).sum::<u64>(), 1);
        append(&path, &[msg("a", 9)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let t = sum(&ledger, "r1", "live");
        assert_eq!(t.output.known_sum, Some(Wide(9)));
        assert_eq!(t.requests_observed.known_sum, Some(Wide(1)));
        assert_eq!(t.input_total.known_sum, Some(Wide(100)));

        // A later copy missing a field loses that coverage; a lower one is a revision.
        let mut missing = msg("a", 9);
        missing["message"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cache_read_input_tokens");
        append(&path, &[missing]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let t = sum(&ledger, "r1", "live");
        assert_eq!(t.input_total.known_sum, None, "no stale field is kept");
        append(&path, &[msg("a", 3)]);
        let before = ledger.revision().unwrap();
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.revised).sum::<u64>(), 1);
        assert!(ledger.revision().unwrap() > before);
        let t = sum(&ledger, "r1", "live");
        assert_eq!(t.output.known_sum, Some(Wide(3)));
        assert_eq!(t.input_total.known_sum, Some(Wide(100)));
        assert_eq!(t.requests_observed.known_sum, Some(Wide(1)));
        assert_eq!(
            ledger.detail(&stream).unwrap()[0].revision,
            4,
            "four revisions"
        );
    }

    #[test]
    fn interrupted_reading_ends_in_the_same_state_as_reading_through() {
        let values: Vec<Value> = (0..300).map(|i| msg(&format!("m{i}"), i)).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s-1.jsonl");
        std::fs::write(&path, text(&values)).unwrap();

        let through = dir.path().join("through.db");
        let mut ledger = Ledger::open(&through).unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let stream = ledger
            .register(&reg("r1", "i1", Start::Fresh), &open(&path), NOW)
            .unwrap()
            .unwrap();
        let commits = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert!(commits.len() >= 3, "300 changes need three transactions");
        let expected = dump(&ledger);

        let crashed = dir.path().join("crashed.db");
        {
            let mut ledger = Ledger::open(&crashed).unwrap();
            ledger.begin_run("r1", Some(NOW), NOW).unwrap();
            ledger
                .register(&reg("r1", "i1", Start::Fresh), &open(&path), NOW)
                .unwrap()
                .unwrap();
        }
        for _ in 0..20 {
            // A crash after each transaction: reopen and go on.
            let mut ledger = Ledger::open(&crashed).unwrap();
            let pumped = pump(&mut ledger, &stream, &open(&path), NOW, Some(1)).unwrap();
            // A crash before a transaction: work decoded and dropped.
            let (from, mut adapter) = ledger.cursor(&stream).unwrap().unwrap();
            let _ = adapter.decode(&source::read(&open(&path), &from).unwrap());
            drop(ledger);
            if pumped.caught_up {
                break;
            }
        }
        assert_eq!(dump(&Ledger::open(&crashed).unwrap()), expected);

        // A replayed commit from an old cursor is refused and changes nothing.
        let mut ledger = Ledger::open(&crashed).unwrap();
        let (_, adapter) = ledger.cursor(&stream).unwrap().unwrap();
        let start = Checkpoint::new(&open(&path), Start::Fresh).unwrap();
        let stale = ledger.commit(Commit {
            stream_id: &stream,
            from: &start,
            to: &start,
            decoder: &adapter,
            items: &[],
            observed_at: NOW,
        });
        assert!(stale.is_err());
        assert_eq!(dump(&ledger), expected);
    }

    #[test]
    fn replayed_prefixes_are_not_charged_again_and_conflicts_are_quarantined() {
        let (_dir, path, mut ledger, stream) = setup(&[msg("a", 5), msg("b", 6)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let before = sum(&ledger, "r1", "live");

        // The same history as a new file: identical ids are no-ops.
        replace(&path, &[msg("a", 5), msg("b", 6)]);
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.unchanged).sum::<u64>(), 2);
        assert_eq!(sum(&ledger, "r1", "live"), before);

        // Rewritten with a different value: quarantined, not applied.
        replace(&path, &[msg("a", 500), msg("b", 6)]);
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.quarantined).sum::<u64>(), 1);
        assert_eq!(sum(&ledger, "r1", "live"), before);
        let coverage = ledger.coverage(&stream).unwrap();
        assert_eq!(coverage.get("ambiguous_boundary"), Some(&1));
        assert_eq!(
            coverage.get("source_gap"),
            Some(&2),
            "two closed generations"
        );

        // New usage after the rewrite is charged.
        append(&path, &[msg("c", 7)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(
            sum(&ledger, "r1", "live").requests_observed.known_sum,
            Some(Wide(3))
        );
    }

    #[test]
    fn expired_detail_keeps_aggregates_and_old_ids_stay_outside_the_horizon() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("crew.db");
        let path = dir.path().join("s-1.jsonl");
        std::fs::write(&path, text(&[msg("a", 5), msg("b", 6)])).unwrap();
        let mut ledger = Ledger::open(&db).unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let stream = ledger
            .register(&reg("r1", "i1", Start::Fresh), &open(&path), NOW)
            .unwrap()
            .unwrap();
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let before = sum(&ledger, "r1", "live");
        let removed = ledger.expire(NOW + 31 * DAY, Retention::default()).unwrap();
        assert_eq!(removed, 2);
        assert!(ledger.detail(&stream).unwrap().is_empty());
        assert_eq!(
            sum(&ledger, "r1", "live"),
            before,
            "aggregates are sufficient"
        );

        // An identical old copy appended later is recognized; a changed one is
        // outside the horizon, never new spend.
        append(&path, &[msg("a", 5)]);
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.unchanged).sum::<u64>(), 1);
        assert!(!ledger.uncertain("r1").unwrap());
        append(&path, &[msg("a", 1)]);
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.outside_horizon).sum::<u64>(), 1);
        assert_eq!(sum(&ledger, "r1", "live"), before);
        assert!(
            ledger.uncertain("r1").unwrap(),
            "the stale sum is not a bound"
        );

        // The same after a restart and a rewrite.
        drop(ledger);
        let mut ledger = Ledger::open(&db).unwrap();
        replace(&path, &[msg("a", 2), msg("b", 6), msg("a", 3)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        append(&path, &[msg("b", 60)]);
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.outside_horizon).sum::<u64>(), 1);
        assert_eq!(sum(&ledger, "r1", "live"), before);

        // A stream that cannot keep its membership admits nothing more.
        ledger
            .close_admission(&stream, CoverageReason::RetentionLimit, NOW)
            .unwrap();
        append(&path, &[msg("z", 1)]);
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.refused).sum::<u64>(), 1);
        assert_eq!(sum(&ledger, "r1", "live"), before);
        assert_eq!(
            ledger.coverage(&stream).unwrap().get("retention_limit"),
            Some(&2)
        );
    }

    #[test]
    fn ownership_and_work_are_captured_where_usage_was_first_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s-1.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut ledger = Ledger::in_memory().unwrap();
        let mut missing = reg("r1", "i1", Start::Fresh);
        missing.native_session_id = "../s".into();
        assert_eq!(
            ledger.register(&missing, &open(&path), NOW).unwrap(),
            Err(CoverageReason::IdentityLost)
        );
        let mut pi = reg("r1", "i1", Start::Fresh);
        pi.runtime = RuntimeKind::Pi;
        assert_eq!(
            ledger.register(&pi, &open(&path), NOW).unwrap(),
            Err(CoverageReason::UnsupportedRuntime)
        );

        let stream = ledger
            .register(&reg("r1", "i1", Start::Fresh), &open(&path), NOW)
            .unwrap()
            .unwrap();
        append(&path, &[msg("before-binding", 1)]);
        ledger
            .bind_work(&stream, &open(&path), &task("t1"), NOW)
            .unwrap();
        append(&path, &[msg("a", 1)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        ledger
            .bind_work(&stream, &open(&path), &task("t2"), NOW)
            .unwrap();
        // A late copy of a keeps t1; b is t2's.
        append(&path, &[msg("b", 2), msg("a", 4)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();

        // Another incarnation cannot take the stream while it is owned.
        assert_eq!(
            ledger
                .register(&reg("r2", "i2", Start::Fresh), &open(&path), NOW)
                .unwrap(),
            Err(CoverageReason::AmbiguousBoundary)
        );
        ledger.release(&stream).unwrap();
        // Written before the new owner registered, read after: the old owner's.
        append(&path, &[msg("c", 1)]);
        ledger
            .register(&reg("r2", "i2", Start::Fresh), &open(&path), NOW)
            .unwrap()
            .unwrap();
        append(&path, &[msg("d", 1)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();

        let detail: BTreeMap<String, (String, Option<WorkAttribution>)> = ledger
            .detail(&stream)
            .unwrap()
            .into_iter()
            .map(|d| (d.key, (d.run_id, serde_json::from_str(&d.work).ok())))
            .collect();
        let work = |key: &str| match &detail[key].1 {
            Some(WorkAttribution::Task { id, .. }) => id.clone(),
            Some(WorkAttribution::Unknown { .. }) => "unknown".into(),
            other => format!("{other:?}"),
        };
        assert_eq!(work("m:before-binding"), "unknown");
        assert_eq!(work("m:a"), "t1");
        assert_eq!(work("m:b"), "t2");
        assert_eq!(work("m:c"), "t2");
        assert_eq!(detail["m:c"].0, "r1");
        assert_eq!(detail["m:d"].0, "r2");
        assert_eq!(work("m:d"), "unknown", "the new owner has no binding yet");
        let t1 = ledger
            .facts(
                "r1",
                "live",
                None,
                |a| matches!(a.work(), Some(WorkAttribution::Task { id, .. }) if id == "t1"),
            )
            .unwrap();
        let t1 = totals(&t1, &Book::standard_2026_10_08(), None);
        assert_eq!(
            t1.output.known_sum,
            Some(Wide(4)),
            "the late copy replaced a in t1"
        );
    }

    #[test]
    fn history_before_tracking_is_kept_apart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s-1.jsonl");
        std::fs::write(&path, text(&[msg("old", 7)])).unwrap();
        let mut ledger = Ledger::in_memory().unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let stream = ledger
            .register(&reg("r1", "i1", Start::Existing), &open(&path), NOW)
            .unwrap()
            .unwrap();
        append(&path, &[msg("new", 3), msg("old", 8)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let live = sum(&ledger, "r1", "live");
        assert_eq!(live.requests_observed.known_sum, Some(Wide(1)));
        assert_eq!(live.output.known_sum, Some(Wide(3)));
        let old = sum(&ledger, "r1", "pre_tracking");
        assert_eq!(
            old.output.known_sum,
            Some(Wide(8)),
            "its streamed correction keeps the old scope"
        );
    }

    #[test]
    fn nothing_but_sanitized_evidence_is_stored() {
        let mut secret = msg("a", 1);
        secret["message"]["content"] = json!([{"type": "text", "text": "PRIVATE REPLY"}]);
        secret["cwd"] = json!("/home/PRIVATE");
        let tool = json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "PRIVATE TOOL \"usage\""}]}});
        let (_dir, path, mut ledger, stream) = setup(&[secret, tool]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let all = dump(&ledger).join("\n");
        assert!(!all.contains("PRIVATE"), "{all}");
    }

    #[test]
    fn retained_aggregates_price_and_count_like_their_observations() {
        let book = Book::standard_2026_10_08();
        for (name, runtime, session) in [
            (
                "01-claude-worker",
                RuntimeKind::Claude,
                "6ba7c9c9-43e5-4ccb-ab55-8b6fa4c18128",
            ),
            (
                "01-codex-worker",
                RuntimeKind::Codex,
                "01a119f6-1996-7a21-91c6-45a47fae3617",
            ),
        ] {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/fixtures/usage/lifecycle/{name}.jsonl"));
            let mut ledger = Ledger::in_memory().unwrap();
            ledger.begin_run("r1", Some(NOW), NOW).unwrap();
            let mut r = reg("r1", "i1", Start::Fresh);
            r.runtime = runtime;
            r.native_session_id = session.into();
            let stream = ledger.register(&r, &open(&path), NOW).unwrap().unwrap();
            drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
            let mut aggregate = sum(&ledger, "r1", "live");

            // The same observations one by one.
            let file = open(&path);
            let mut adapter = Adapter::new(runtime, session, Start::Fresh).unwrap();
            let batch =
                source::read(&file, &Checkpoint::new(&file, Start::Fresh).unwrap()).unwrap();
            let mut latest = BTreeMap::new();
            for item in adapter.decode(&batch) {
                if let Item::Observation(o) = item {
                    latest.insert(o.key.clone(), *o);
                }
            }
            let facts: Vec<Fact> = latest
                .values()
                .map(|o| Fact {
                    unit: o.unit,
                    normalized: o.normalized.clone(),
                    model: o.model.clone(),
                    agent: "i1".into(),
                    source: stream.clone(),
                    turn: None,
                    unusable: None,
                    weight: 1,
                    input_range: None,
                })
                .collect();
            let mut each = totals(&facts, &book, None);
            assert!(aggregate.estimate.lower_pico_usd.is_some(), "{name}");
            // Coverage counts observations either way; only the turn ids differ.
            aggregate.turns_observed = each.turns_observed.clone();
            each.turns_observed = aggregate.turns_observed.clone();
            assert_eq!(aggregate, each, "{name}");
            assert!(ledger.turns("r1", "live", None).unwrap() > 0);
        }
    }
}
