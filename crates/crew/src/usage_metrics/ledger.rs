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
use crew_core::rpc::asides::AsideUsage;
use crew_core::rpc::features::Wide;
use crew_core::rpc::usage_metrics::{
    AccountAttribution, CompletionCohort, CostEstimate, CounterMetric, CoverageReason,
    EpicAttribution, EstimateState, ExecutionStatusCounts, MetricCoverage, MetricState,
    ObservationUnit, UsageCounters, UsageKind, WorkAttribution,
};
use crew_core::RuntimeKind;
use rusqlite::{params, Connection, OptionalExtension, ToSql, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::cohort::{cohort_with, Member};
use super::counters::{Counters, Derived, Normalized, WriteKind};
use super::metrics::{totals, Fact};
use super::price::{Book, Bounds};
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
    task_id         TEXT,
    class           TEXT,
    binding_seq     INTEGER,
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
    has_turn        INTEGER NOT NULL,
    count           INTEGER NOT NULL,
    i INTEGER, n INTEGER, r INTEGER, w INTEGER, w5 INTEGER, w1 INTEGER, o INTEGER,
    min_i           INTEGER,
    max_i           INTEGER,
    PRIMARY KEY (run_id, scope, hour, stream_id, agent_id, incarnation, aside_execution,
                 work, account, model, unit, reason, flags, band, has_turn)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_turns (
    stream_id       TEXT NOT NULL,
    turn            TEXT NOT NULL,
    run_id          TEXT NOT NULL,
    scope           TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    incarnation     TEXT NOT NULL,
    aside_execution TEXT NOT NULL,
    work            TEXT NOT NULL,
    account         TEXT NOT NULL,
    model           TEXT NOT NULL,
    first_ms        INTEGER,
    PRIMARY KEY (stream_id, turn, run_id, scope, incarnation, aside_execution, work, account,
                 model)
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
CREATE TABLE IF NOT EXISTS usage_first_completions (
    run_id       TEXT NOT NULL,
    task_id      TEXT NOT NULL,
    completed_at INTEGER NOT NULL,
    event_seq    INTEGER,
    epic         TEXT NOT NULL,
    PRIMARY KEY (run_id, task_id)
);
CREATE TABLE IF NOT EXISTS usage_task_costs (
    run_id      TEXT NOT NULL,
    task_id     TEXT NOT NULL,
    class       TEXT NOT NULL,
    hour        INTEGER NOT NULL,
    stream_id   TEXT NOT NULL,
    incarnation TEXT NOT NULL,
    model       TEXT NOT NULL,
    unit        TEXT NOT NULL,
    reason      TEXT NOT NULL,
    flags       INTEGER NOT NULL,
    band        INTEGER NOT NULL,
    count       INTEGER NOT NULL,
    i INTEGER, n INTEGER, r INTEGER, w INTEGER, w5 INTEGER, w1 INTEGER, o INTEGER,
    min_i       INTEGER,
    max_i       INTEGER,
    PRIMARY KEY (run_id, task_id, class, hour, stream_id, incarnation, model, unit, reason,
                 flags, band)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_executions (
    execution_id TEXT PRIMARY KEY,
    aside_id     TEXT NOT NULL,
    run_id       TEXT NOT NULL,
    status       TEXT NOT NULL,
    started_at   INTEGER,
    finished_at  INTEGER
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
    /// Live task-bound usage: the task and where the usage stands against
    /// its first completion.
    task: Option<(String, String)>,
    /// The observation has a native turn (1) or not (0).
    has_turn: i64,
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

    /// Starts tracking a workspace run. `tracking_since` is when the
    /// ledger began recording the run's sources and first completions: a
    /// run created now passes its own start; a run found on upgrade passes
    /// the present. Idempotent: a known run keeps what it recorded.
    pub fn begin_run(
        &mut self,
        run_id: &str,
        started_at: Option<i64>,
        tracking_since: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO usage_runs (run_id, started_at, tracking_since, build)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(run_id) DO NOTHING",
            params![
                run_id,
                started_at,
                tracking_since,
                env!("CARGO_PKG_VERSION")
            ],
        )?;
        Ok(())
    }

    /// Records a task's first valid transition to Done in a run (after its
    /// review approval, when one is required), with its epic membership at
    /// that moment. A later completion of the same task changes nothing.
    /// Returns whether this was the first.
    pub fn first_completion(
        &mut self,
        run_id: &str,
        task_id: &str,
        completed_at: i64,
        event_seq: Option<u64>,
        epic: &EpicAttribution,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let first = tx.execute(
            "INSERT OR IGNORE INTO usage_first_completions
             (run_id, task_id, completed_at, event_seq, epic) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                run_id,
                task_id,
                completed_at,
                event_seq.map(|v| v as i64),
                json(epic)
            ],
        )? == 1;
        if first {
            bump(&tx)?;
        }
        tx.commit()?;
        Ok(first)
    }

    /// The tasks of a run first completed in `[from, to)` that `keep`
    /// accepts by task id and completion epic, and their cost per completed task: each one's
    /// task-bound live cost from the run's start through its first
    /// completion. Untimed usage recorded after a completion widens only the
    /// upper bound; unattributed usage or a lost source in the run leaves it
    /// open.
    pub fn cohort(
        &self,
        run_id: &str,
        range: Option<(i64, i64)>,
        keep: impl Fn(&str, &EpicAttribution) -> bool,
        book: &Book,
    ) -> Result<CompletionCohort> {
        let run: Option<(Option<i64>, i64)> = self
            .conn
            .query_row(
                "SELECT started_at, tracking_since FROM usage_runs WHERE run_id = ?1",
                params![run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let complete = run.is_some_and(|(started, since)| {
            range
                .map(|r| r.0)
                .filter(|from| *from != i64::MIN)
                .or(started)
                .is_some_and(|from| from >= since)
        });
        let (from, to) = range.unwrap_or((i64::MIN, i64::MAX));
        let mut stmt = self.conn.prepare(
            "SELECT task_id, epic FROM usage_first_completions
             WHERE run_id = ?1 AND completed_at >= ?2 AND completed_at < ?3 ORDER BY task_id",
        )?;
        let completions: Vec<(String, String)> = stmt
            .query_map(params![run_id, from, to], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut members = Vec::new();
        let mut uncertain = false;
        for (task, epic) in completions {
            let epic: EpicAttribution = serde_json::from_str(&epic)?;
            if !keep(&task, &epic) {
                continue;
            }
            let before = self.task_facts(run_id, &task, "before")?;
            let ambiguous = self.task_facts(run_id, &task, "ambiguous")?;
            let estimate = |facts: &[Fact]| totals(facts, book, None).estimate;
            let cost = if before.is_empty() && ambiguous.is_empty() {
                None
            } else {
                let b = estimate(&before);
                let lo = b.lower_pico_usd.map(|w| w.0);
                let hi = if ambiguous.is_empty() {
                    b.upper_pico_usd.map(|w| w.0)
                } else {
                    let a = estimate(&ambiguous);
                    b.upper_pico_usd
                        .zip(a.upper_pico_usd)
                        .and_then(|(x, y)| x.0.checked_add(y.0))
                };
                match (lo, before.is_empty()) {
                    (Some(lo), _) => Some(Bounds { lo, hi }),
                    // Only untimed spend that may predate the completion.
                    (None, true) => Some(Bounds { lo: 0, hi }),
                    (None, false) => None,
                }
            };
            let earlier_spend = range.is_some()
                && self.conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM usage_task_costs WHERE run_id = ?1
                     AND task_id = ?2 AND class = 'before' AND hour >= 0 AND hour < ?3)",
                    params![run_id, task, from.saturating_sub(from.rem_euclid(HOUR))],
                    |r| r.get(0),
                )?;
            uncertain |= self.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM usage_task_costs t JOIN usage_uncertain u
                 ON u.run_id = t.run_id AND u.hour = t.hour AND u.stream_id = t.stream_id
                 WHERE t.run_id = ?1 AND t.task_id = ?2)",
                params![run_id, task],
                |r| r.get::<_, bool>(0),
            )?;
            members.push(Member {
                task_id: task,
                cost,
                earlier_spend,
            });
        }
        let mut c = cohort_with(book, &members, complete, self.spend_gap(run_id)?);
        // A member's spend includes an aggregate a late correction made
        // uncertain: no bound of the average holds, the lower one included.
        if uncertain && c.cost_per_completed_task.state != EstimateState::Unavailable {
            unavailable(&mut c.cost_per_completed_task);
        }
        Ok(c)
    }

    /// Why some live spend of a run may be missing from task costs:
    /// unattributed usage, a lost or unreadable source, or a correction that
    /// arrived after its detail expired.
    fn spend_gap(&self, run_id: &str) -> Result<Option<CoverageReason>> {
        let unattributed: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM usage_rollups WHERE run_id = ?1 AND scope = 'live'
             AND work LIKE '{\"kind\":\"unknown\"%')",
            params![run_id],
            |r| r.get(0),
        )?;
        if unattributed {
            return Ok(Some(CoverageReason::UnknownAttribution));
        }
        let lost: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM usage_coverage c JOIN usage_owners o
             ON c.stream_id = o.stream_id WHERE o.run_id = ?1 AND o.kind = 'live'
             AND c.reason IN ('source_gap', 'parser_limit', 'not_reported',
                              'unsupported_sidechain', 'ambiguous_boundary', 'retention_limit',
                              'identity_lost', 'correction_outside_horizon'))",
            params![run_id],
            |r| r.get(0),
        )?;
        Ok(lost.then_some(CoverageReason::SourceGap))
    }

    fn task_facts(&self, run_id: &str, task_id: &str, class: &str) -> Result<Vec<Fact>> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, incarnation, model, unit, reason, flags, count,
                    i, n, r, w, w5, w1, o, min_i, max_i
             FROM usage_task_costs WHERE run_id = ?1 AND task_id = ?2 AND class = ?3",
        )?;
        let rows = stmt.query_map(params![run_id, task_id, class], |r| {
            Ok(Stored {
                stream_id: r.get(0)?,
                incarnation: r.get(1)?,
                model: r.get(2)?,
                unit: r.get(3)?,
                reason: r.get(4)?,
                flags: r.get(5)?,
                count: r.get(6)?,
                sums: [
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                ],
                input_range: r
                    .get::<_, Option<i64>>(14)?
                    .zip(r.get::<_, Option<i64>>(15)?),
            })
        })?;
        rows.map(|row| row?.fact()).collect()
    }

    /// An aside execution's state, for its status counts.
    pub fn execution_status(
        &mut self,
        run_id: &str,
        aside_id: &str,
        execution_id: &str,
        status: ExecutionStatus,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO usage_executions
             (execution_id, aside_id, run_id, status, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(execution_id) DO UPDATE SET status = excluded.status,
               started_at = COALESCE(excluded.started_at, started_at),
               finished_at = COALESCE(excluded.finished_at, finished_at)",
            params![
                execution_id,
                aside_id,
                run_id,
                tag(&status),
                started_at,
                finished_at
            ],
        )?;
        bump(&tx)?;
        tx.commit()?;
        Ok(())
    }

    fn sourced(&self, run_id: &str, execution_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM usage_owners WHERE run_id = ?1 AND aside_execution = ?2)",
            params![run_id, execution_id],
            |r| r.get(0),
        )?)
    }

    /// The new usage of one aside execution: counters that every one of its
    /// observations reported (`None` otherwise, never 0) and its cost.
    /// `None` when it has no registered source, as when its boundary
    /// between copied context and new usage is unknown.
    pub fn aside_usage(
        &self,
        run_id: &str,
        execution_id: &str,
        book: &Book,
    ) -> Result<Option<AsideUsage>> {
        if !self.sourced(run_id, execution_id)? {
            return Ok(None);
        }
        let uncertain_scopes = self.uncertain_scopes(run_id)?;
        let rows = self.rows(run_id, "aside", |a| a.aside_execution == execution_id)?;
        let uncertain = rows
            .iter()
            .any(|(a, _)| uncertain_scopes.contains(&(a.hour, a.stream_id.clone())));
        let facts: Vec<Fact> = rows.into_iter().map(|(_, f)| f).collect();
        let mut t = totals(&facts, book, None);
        if uncertain {
            // A late correction to expired detail: no figure stands.
            for m in [
                &mut t.input_total,
                &mut t.input_uncached,
                &mut t.cache_read,
                &mut t.cache_write,
                &mut t.cache_write_5m,
                &mut t.cache_write_1h,
                &mut t.output,
                &mut t.total_tokens,
            ] {
                m.state = MetricState::Unavailable;
            }
            unavailable(&mut t.estimate);
        }
        let whole = |m: &CounterMetric| {
            (m.state == MetricState::Complete)
                .then_some(m.known_sum)
                .flatten()
        };
        Ok(Some(AsideUsage {
            counters: UsageCounters {
                input_total: whole(&t.input_total),
                input_uncached: whole(&t.input_uncached),
                cache_read: whole(&t.cache_read),
                cache_write: whole(&t.cache_write),
                cache_write_5m: whole(&t.cache_write_5m),
                cache_write_1h: whole(&t.cache_write_1h),
                output: whole(&t.output),
                total_tokens: whole(&t.total_tokens),
            },
            cost: Some(t.estimate),
        }))
    }

    /// The cost of every execution of an aside, failed and cancelled spend
    /// included. An execution without a source leaves the upper bound open.
    pub fn aside_total_cost(
        &self,
        run_id: &str,
        aside_id: &str,
        book: &Book,
    ) -> Result<Option<CostEstimate>> {
        let mut stmt = self.conn.prepare(
            "SELECT execution_id FROM usage_executions WHERE run_id = ?1 AND aside_id = ?2
             UNION SELECT aside_execution FROM usage_owners
             WHERE run_id = ?1 AND aside_id = ?2 AND aside_execution IS NOT NULL",
        )?;
        let executions: Vec<String> = stmt
            .query_map(params![run_id, aside_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if executions.is_empty() {
            return Ok(None);
        }
        let mut sourced = std::collections::BTreeSet::new();
        for e in &executions {
            if self.sourced(run_id, e)? {
                sourced.insert(e.clone());
            }
        }
        let uncertain_scopes = self.uncertain_scopes(run_id)?;
        let rows = self.rows(run_id, "aside", |a| sourced.contains(&a.aside_execution))?;
        if rows
            .iter()
            .any(|(a, _)| uncertain_scopes.contains(&(a.hour, a.stream_id.clone())))
        {
            let mut estimate = totals(&[], book, None).estimate;
            unavailable(&mut estimate);
            return Ok(Some(estimate));
        }
        let facts: Vec<Fact> = rows.into_iter().map(|(_, f)| f).collect();
        let mut estimate = totals(&facts, book, None).estimate;
        if sourced.len() < executions.len() && estimate.state != EstimateState::Unavailable {
            estimate.upper_pico_usd = None;
            estimate.state = EstimateState::Partial;
            estimate.reason = Some(CoverageReason::NotTracked);
        }
        Ok(Some(estimate))
    }

    /// The executions in `observed` (those with newly observed usage in the
    /// query's cohort) and the status partition of that cohort. An execution
    /// of the run without a source has unknown usage, never zero: when the
    /// query narrows only by time, one started in `range` (any, unbounded)
    /// makes the count partial; when it narrows by member, model, account or
    /// work, whether such an execution belongs is unknown, so any makes it
    /// partial.
    pub fn executions(
        &self,
        run_id: &str,
        observed: &std::collections::BTreeSet<String>,
        range: Option<(i64, i64)>,
        narrowed: bool,
    ) -> Result<(CounterMetric, ExecutionStatusCounts)> {
        let mut status = self
            .conn
            .prepare("SELECT status FROM usage_executions WHERE execution_id = ?1")?;
        let mut observed_status = Vec::new();
        for id in observed {
            let s: Option<String> = status.query_row(params![id], |r| r.get(0)).optional()?;
            observed_status.push((id.clone(), s));
        }
        let observed = observed_status;
        let (from, to) = range.unwrap_or((i64::MIN, i64::MAX));
        let unsourced: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM usage_executions e WHERE e.run_id = ?1 AND NOT EXISTS
             (SELECT 1 FROM usage_owners o WHERE o.aside_execution = e.execution_id)
             AND (?2 = 1 OR ?3 = 1 OR (e.started_at IS NOT NULL
                  AND e.started_at >= ?4 AND e.started_at < ?5))",
            params![run_id, narrowed as i64, range.is_none() as i64, from, to],
            |r| r.get(0),
        )?;
        let mut counts = ExecutionStatusCounts {
            answered: Wide(0),
            failed: Wide(0),
            cancelled: Wide(0),
            still_answering: Wide(0),
            unknown: Wide(0),
            complete: false,
        };
        for (_, status) in &observed {
            let slot = match status.as_deref() {
                Some("answered") => &mut counts.answered,
                Some("failed") => &mut counts.failed,
                Some("cancelled") => &mut counts.cancelled,
                Some("still_answering") => &mut counts.still_answering,
                _ => &mut counts.unknown,
            };
            slot.0 += 1;
        }
        counts.complete = counts.unknown.0 == 0 && unsourced == 0;
        let n = observed.len() as u128;
        let metric = CounterMetric {
            known_sum: Some(Wide(n)),
            state: if unsourced == 0 {
                MetricState::Complete
            } else {
                MetricState::Partial
            },
            reason: (unsourced > 0).then_some(CoverageReason::NotTracked),
            coverage: MetricCoverage {
                eligible_observations: Wide(n),
                observations_seen: Wide(n + unsourced as u128),
                ..MetricCoverage::default()
            },
        };
        Ok((metric, counts))
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
                let owner = current_owner(&tx, &stream_id)?;
                if let Some(owner) = &owner {
                    if owner.active && owner.incarnation != reg.incarnation {
                        return Ok(Err(CoverageReason::AmbiguousBoundary));
                    }
                }
                // The home the session is opened from now, kept with the
                // stream; cursor and seen ids stay. A file in another place
                // is a new generation for the reader, reconciled by id.
                tx.execute(
                    "UPDATE usage_streams SET locator = ?2, started_ms = ?3 WHERE stream_id = ?1",
                    params![stream_id, json(&reg.locator), reg.started_ms],
                )?;
                // The same launch registered again (a daemon restart): its
                // owner and work binding continue.
                let same = owner.as_ref().is_some_and(|o| {
                    o.run_id == reg.run_id
                        && o.kind == reg.kind
                        && o.incarnation == reg.incarnation
                        && o.launch_id == reg.launch_id
                        && o.aside_execution.as_deref()
                            == reg.aside.as_ref().map(|a| a.execution_id.as_str())
                });
                if let (true, Some(owner)) = (same, owner) {
                    tx.execute(
                        "UPDATE usage_owners SET active = 1 WHERE stream_id = ?1 AND seq = ?2",
                        params![stream_id, owner.seq],
                    )?;
                    bump(&tx)?;
                    tx.commit()?;
                    return Ok(Ok(stream_id));
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

    /// Streams with an active owner, for the service to resume after a
    /// restart.
    pub fn active_streams(&self) -> Result<Vec<ActiveStream>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT s.stream_id, s.runtime, s.native_session_id, s.started_ms, s.locator
             FROM usage_streams s JOIN usage_owners o ON o.stream_id = s.stream_id
             WHERE o.active = 1 AND s.admission IS NULL ORDER BY s.stream_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, runtime, session, started, locator) = row?;
            out.push(ActiveStream {
                stream_id: id,
                runtime: serde_json::from_value(serde_json::Value::String(runtime))?,
                session,
                started_ms: started,
                locator: serde_json::from_str(&locator)?,
            });
        }
        Ok(out)
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
        Ok(self
            .rows(run_id, scope, |a| {
                range.is_none_or(|(from, to)| a.hour >= from && a.hour < to) && keep(a)
            })?
            .into_iter()
            .map(|(_, f)| f)
            .collect())
    }

    /// Retained aggregates of a run and scope that `keep` accepts, with
    /// their keys.
    pub fn rows(
        &self,
        run_id: &str,
        scope: &str,
        keep: impl Fn(&Aggregate) -> bool,
    ) -> Result<Vec<(Aggregate, Fact)>> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, agent_id, incarnation, aside_execution, work, account, model,
                    unit, reason, flags, count, i, n, r, w, w5, w1, o, min_i, max_i, hour,
                    has_turn
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
                    has_turn: r.get::<_, i64>(21)? != 0,
                },
                Stored {
                    stream_id: r.get(0)?,
                    incarnation: r.get(2)?,
                    model: r.get(6)?,
                    unit: r.get(7)?,
                    reason: r.get(8)?,
                    flags: r.get(9)?,
                    count: r.get(10)?,
                    sums: [
                        r.get(11)?,
                        r.get(12)?,
                        r.get(13)?,
                        r.get(14)?,
                        r.get(15)?,
                        r.get(16)?,
                        r.get(17)?,
                    ],
                    input_range: r
                        .get::<_, Option<i64>>(18)?
                        .zip(r.get::<_, Option<i64>>(19)?),
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (agg, stored) = row?;
            if keep(&agg) {
                out.push((agg, stored.fact()?));
            }
        }
        Ok(out)
    }

    /// Detail observations of a run and scope with a native time in
    /// `[from_ms, to_ms)`, one fact each, for buckets finer than an hour.
    /// Only retained detail can answer; see [`RunInfo::retained_from`].
    pub fn detail_rows(
        &self,
        run_id: &str,
        scope: &str,
        (from_ms, to_ms): (i64, i64),
        keep: impl Fn(&Aggregate) -> bool,
    ) -> Result<Vec<(Aggregate, i64, Fact)>> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, agent_id, incarnation, aside_execution, work, account, model,
                    unit, reason, flags, i, n, r, w, w5, w1, o, hour, turn, native_ms
             FROM usage_observations WHERE run_id = ?1 AND scope = ?2
             AND native_ms >= ?3 AND native_ms < ?4",
        )?;
        let rows = stmt.query_map(params![run_id, scope, from_ms, to_ms], |r| {
            let turn: Option<String> = r.get(18)?;
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
                    hour: r.get(17)?,
                    has_turn: turn.is_some(),
                },
                Stored {
                    stream_id: r.get(0)?,
                    incarnation: r.get(2)?,
                    model: r.get(6)?,
                    unit: r.get(7)?,
                    reason: r.get(8)?,
                    flags: r.get(9)?,
                    count: 1,
                    sums: [
                        r.get(10)?,
                        r.get(11)?,
                        r.get(12)?,
                        r.get(13)?,
                        r.get(14)?,
                        r.get(15)?,
                        r.get(16)?,
                    ],
                    input_range: None,
                },
                turn,
                r.get::<_, i64>(19)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (agg, stored, turn, native_ms) = row?;
            if keep(&agg) {
                let mut fact = stored.fact()?;
                fact.turn = turn;
                out.push((agg, native_ms, fact));
            }
        }
        Ok(out)
    }

    /// Native turns of a run and scope, one per attribution each has
    /// observations in, as `(stream, turn, attribution, first native time)`.
    /// The aggregate key of each carries the turn's attribution, with its
    /// hour from the first native time (-1 when unknown).
    pub fn turn_rows(&self, run_id: &str, scope: &str) -> Result<Vec<TurnRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, turn, agent_id, incarnation, aside_execution, work, account,
                    model, first_ms
             FROM usage_turns WHERE run_id = ?1 AND scope = ?2",
        )?;
        let rows = stmt.query_map(params![run_id, scope], |r| {
            let first_ms: Option<i64> = r.get(8)?;
            Ok(TurnRow {
                turn: r.get(1)?,
                first_ms,
                aggregate: Aggregate {
                    stream_id: r.get(0)?,
                    agent_id: r.get(2)?,
                    incarnation: r.get(3)?,
                    aside_execution: r.get(4)?,
                    work: r.get(5)?,
                    account: r.get(6)?,
                    model: r.get(7)?,
                    unit: String::new(),
                    reason: String::new(),
                    hour: hour_of(first_ms),
                    has_turn: true,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A run's tracking facts, if the ledger knows it.
    pub fn run(&self, run_id: &str) -> Result<Option<RunInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT run_id, started_at, tracking_since, ended_at, retained_from
                 FROM usage_runs WHERE run_id = ?1",
                params![run_id],
                run_info,
            )
            .optional()?)
    }

    /// Known runs, most recently tracked first, from `offset`.
    pub fn runs(&self, offset: u64, limit: u64) -> Result<Vec<RunInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, started_at, tracking_since, ended_at, retained_from FROM usage_runs
             ORDER BY tracking_since DESC, run_id LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], run_info)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Sources of a run that lost usage: stream, reason, count and when.
    pub fn gaps(&self, run_id: &str) -> Result<Vec<Gap>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT c.stream_id, c.reason, c.count, c.first_at, c.last_at
             FROM usage_coverage c JOIN usage_owners o ON c.stream_id = o.stream_id
             WHERE o.run_id = ?1 ORDER BY c.stream_id, c.reason",
        )?;
        let rows = stmt.query_map(params![run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (stream_id, reason, count, first_at, last_at) = row?;
            let reason: CoverageReason = serde_json::from_value(serde_json::Value::String(reason))?;
            out.push(Gap {
                stream_id,
                reason,
                count: count as u64,
                first_at,
                last_at,
            });
        }
        Ok(out)
    }

    /// Aggregates of a run that a late correction made uncertain, as
    /// `(hour, stream)`.
    pub fn uncertain_scopes(
        &self,
        run_id: &str,
    ) -> Result<std::collections::BTreeSet<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT hour, stream_id FROM usage_uncertain WHERE run_id = ?1")?;
        let rows = stmt.query_map(params![run_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether any source of a run is still owned (being collected).
    pub fn collecting(&self, run_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM usage_owners WHERE run_id = ?1 AND active = 1)",
            params![run_id],
            |r| r.get(0),
        )?)
    }

    /// Detail observations of a run and scope for listing, oldest native
    /// time first (untimed last), from `offset`.
    pub fn observation_rows(
        &self,
        run_id: &str,
        scope: &str,
        range: Option<(i64, i64)>,
        keep: impl Fn(&Aggregate) -> bool,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<ObservationRow>> {
        let (from, to) = range.unwrap_or((i64::MIN, i64::MAX));
        let mut stmt = self.conn.prepare(
            "SELECT o.stream_id, o.key, o.revision, o.agent_id, o.incarnation, o.launch_id,
                    o.aside_execution, o.unit, o.reason, o.turn, o.work, o.account, o.model,
                    o.native_ms, o.observed_at, o.flags, o.i, o.n, o.r, o.w, o.w5, o.w1, o.o,
                    o.reasons, o.hour, s.runtime, s.native_session_id, ow.cli_version,
                    b.board_revision, b.event_seq
             FROM usage_observations o
             JOIN usage_streams s ON s.stream_id = o.stream_id
             LEFT JOIN usage_owners ow ON ow.stream_id = o.stream_id
                  AND ow.incarnation = o.incarnation AND ow.launch_id = o.launch_id
                  AND ow.run_id = o.run_id
             LEFT JOIN usage_bindings b ON b.stream_id = o.stream_id AND b.seq = o.binding_seq
             WHERE o.run_id = ?1 AND o.scope = ?2
               AND (?3 = 1 OR (o.native_ms >= ?4 AND o.native_ms < ?5))
             GROUP BY o.stream_id, o.key
             ORDER BY o.native_ms IS NULL, o.native_ms, o.stream_id, o.key",
        )?;
        let unbounded = range.is_none() as i64;
        let rows = stmt.query_map(params![run_id, scope, unbounded, from, to], |r| {
            Ok(ObservationRow {
                aggregate: Aggregate {
                    stream_id: r.get(0)?,
                    agent_id: r.get(3)?,
                    incarnation: r.get(4)?,
                    aside_execution: r.get(6)?,
                    work: r.get(10)?,
                    account: r.get(11)?,
                    model: r.get(12)?,
                    unit: r.get(7)?,
                    reason: r.get(8)?,
                    hour: r.get(24)?,
                    has_turn: r.get::<_, Option<String>>(9)?.is_some(),
                },
                key: r.get(1)?,
                revision: r.get::<_, i64>(2)? as u64,
                launch_id: r.get(5)?,
                turn: r.get(9)?,
                native_ms: r.get(13)?,
                observed_at: r.get(14)?,
                normalized: normalized(
                    r.get(15)?,
                    [
                        r.get(16)?,
                        r.get(17)?,
                        r.get(18)?,
                        r.get(19)?,
                        r.get(20)?,
                        r.get(21)?,
                        r.get(22)?,
                    ],
                ),
                reasons: r.get(23)?,
                runtime: r.get(25)?,
                native_session_id: r.get(26)?,
                cli_version: r.get(27)?,
                board_revision: r.get::<_, Option<i64>>(28)?.map(|v| v as u64),
                event_seq: r.get::<_, Option<i64>>(29)?.map(|v| v as u64),
            })
        })?;
        let mut out = Vec::new();
        let mut skipped = 0;
        for row in rows {
            let row = row?;
            if !keep(&row.aggregate) {
                continue;
            }
            if skipped < offset {
                skipped += 1;
                continue;
            }
            out.push(row);
            if out.len() as u64 >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Distinct native turns of a run and scope.
    pub fn turns(&self, run_id: &str, scope: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM (SELECT DISTINCT stream_id, turn FROM usage_turns
             WHERE run_id = ?1 AND scope = ?2)",
            params![run_id, scope],
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
            // Each victim with the native time (or, untimed, the read time)
            // below which detail can now be missing.
            let victims: Vec<(String, String, i64, String, i64)> = {
                let mut stmt = tx.prepare(
                    "SELECT stream_id, key, observed_at, run_id, COALESCE(native_ms, observed_at)
                     FROM usage_observations ORDER BY observed_at LIMIT ?1",
                )?;
                let limit = policy.batch as i64;
                let rows = stmt.query_map(params![limit], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
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
            for (stream_id, key, _, run_id, time) in &victims {
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
                    params![run_id, time + 1],
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
                "usage_task_costs",
                "usage_first_completions",
                "usage_executions",
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

/// A stream with an active owner, as persisted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveStream {
    pub stream_id: String,
    pub runtime: RuntimeKind,
    pub session: String,
    pub started_ms: i64,
    pub locator: Locator,
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

/// An aggregate row as stored, before it becomes a fact.
struct Stored {
    stream_id: String,
    incarnation: String,
    model: String,
    unit: String,
    reason: String,
    flags: i64,
    count: i64,
    sums: [Option<i64>; 7],
    input_range: Option<(i64, i64)>,
}

impl Stored {
    fn fact(self) -> Result<Fact> {
        let unit: ObservationUnit = serde_json::from_value(serde_json::Value::String(self.unit))?;
        let unusable = (!self.reason.is_empty())
            .then(|| serde_json::from_value(serde_json::Value::String(self.reason)))
            .transpose()?;
        Ok(Fact {
            unit,
            normalized: normalized(self.flags, self.sums),
            model: (!self.model.is_empty()).then_some(self.model),
            agent: self.incarnation,
            source: self.stream_id,
            turn: None,
            unusable,
            weight: self.count as u64,
            input_range: self.input_range.map(|(a, b)| (a as u64, b as u64)),
        })
    }
}

/// How an aside execution ended, or that it has not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Answered,
    Failed,
    Cancelled,
    StillAnswering,
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
    /// Its observations have a native turn.
    pub has_turn: bool,
}

/// A native turn with one attribution it has observations in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRow {
    pub turn: String,
    pub first_ms: Option<i64>,
    pub aggregate: Aggregate,
}

/// A run as the ledger tracks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunInfo {
    pub run_id: String,
    pub started_at: Option<i64>,
    pub tracking_since: i64,
    pub ended_at: Option<i64>,
    /// Detail timed before this may have expired; aggregates remain.
    pub retained_from: Option<i64>,
}

fn run_info(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunInfo> {
    Ok(RunInfo {
        run_id: r.get(0)?,
        started_at: r.get(1)?,
        tracking_since: r.get(2)?,
        ended_at: r.get(3)?,
        retained_from: r.get(4)?,
    })
}

/// Usage a source of a run lost, by reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gap {
    pub stream_id: String,
    pub reason: CoverageReason,
    pub count: u64,
    pub first_at: i64,
    pub last_at: i64,
}

/// One retained observation, with what its public record needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationRow {
    pub aggregate: Aggregate,
    pub key: String,
    pub revision: u64,
    pub launch_id: String,
    pub turn: Option<String>,
    pub native_ms: Option<i64>,
    pub observed_at: i64,
    pub normalized: Normalized,
    /// JSON list of coverage reasons.
    pub reasons: String,
    pub runtime: String,
    pub native_session_id: String,
    pub cli_version: Option<String>,
    pub board_revision: Option<u64>,
    pub event_seq: Option<u64>,
}

impl Aggregate {
    pub fn work(&self) -> Option<WorkAttribution> {
        serde_json::from_str(&self.work).ok()
    }
}

/// An estimate over an aggregate a late correction made uncertain.
fn unavailable(e: &mut CostEstimate) {
    e.lower_pico_usd = None;
    e.upper_pico_usd = None;
    e.state = EstimateState::Unavailable;
    e.reason = Some(CoverageReason::CorrectionOutsideHorizon);
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
) -> Result<Option<(String, String, i64)>> {
    Ok(tx
        .query_row(
            "SELECT work, account, seq FROM usage_bindings WHERE stream_id = ?1
             AND owner_seq = ?2
             AND (generation < ?3 OR (generation = ?3 AND from_offset <= ?4))
             ORDER BY generation DESC, from_offset DESC, seq DESC LIMIT 1",
            params![stream_id, owner_seq, generation as i64, offset as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
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
        Some((_, _, _, _)) if !o.revisable => {
            // An increment is a fixed fact: the same key with other values
            // is a conflicting replay, not a correction.
            cover(
                tx,
                stream_id,
                &tag(&CoverageReason::AmbiguousBoundary),
                1,
                now,
            )?;
            done.quarantined += 1;
        }
        Some((_, true, _, _)) if o.phase == Phase::Seed => {
            // A replayed prefix whose values differ from retained detail:
            // not evidence enough to replace it.
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
            // longer be corrected, so the aggregates it is in become uncertain,
            // whether the changed value came as an append or in a rewritten
            // prefix.
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
            let (mut work, mut account, binding_seq) =
                match binding_at(tx, stream_id, owner.seq, o.generation, o.offset)? {
                    Some((work, account, seq)) => (work, account, Some(seq)),
                    None => (json(&unknown_work()), json(&unknown_account()), None),
                };
            // A native batch sums usage anywhere since its start record. When
            // a work or account binding changed in between, its tokens stay
            // as evidence but are not given to either side; when the owner
            // changed, not even to a run or member.
            let mut span_owner_changed = false;
            if let Some(start) = o.span_start {
                match owner_at(tx, stream_id, o.generation, start)? {
                    Some(first) if first.seq == owner.seq => {
                        let (w, a) = binding_at(tx, stream_id, owner.seq, o.generation, start)?
                            .map(|(w, a, _)| (w, a))
                            .unwrap_or_else(|| (json(&unknown_work()), json(&unknown_account())));
                        if w != work {
                            work = json(&WorkAttribution::Unknown {
                                reason: CoverageReason::MultipleTasks,
                            });
                        }
                        if a != account {
                            account = json(&unknown_account());
                        }
                    }
                    _ => span_owner_changed = true,
                }
            }
            let scope = match o.phase {
                Phase::Seed => "pre_tracking".to_string(),
                Phase::Charge => tag(&owner.kind),
            };
            let task = match serde_json::from_str::<WorkAttribution>(&work) {
                Ok(WorkAttribution::Task { id, .. }) if scope == "live" => {
                    let class = classify(tx, &owner.run_id, &id, o.native_ms)?;
                    Some((id, class))
                }
                _ => None,
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
                unit: if span_owner_changed {
                    tag(&ObservationUnit::Unusable)
                } else {
                    tag(&o.unit)
                },
                reason: if span_owner_changed {
                    tag(&CoverageReason::AmbiguousBoundary)
                } else {
                    unusable_reason(o)
                },
                flags: flags(&o.normalized),
                band: band(o, thresholds),
                counters: counters_of(&o.normalized),
                task,
                has_turn: o.turn.is_some() as i64,
            };
            let c = &row.counters;
            tx.execute(
                "INSERT INTO usage_observations
                 (stream_id, key, revision, run_id, scope, agent_id, incarnation, launch_id,
                  aside_execution, unit, reason, turn, work, account, model, request_id,
                  native_ms, hour, observed_at, i, n, r, w, w5, w1, o, flags, band, reasons,
                  generation, offset, task_id, class, binding_seq)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28,
                         ?29, ?30, ?31, ?32, ?33)",
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
                    o.offset as i64,
                    row.task.as_ref().map(|t| &t.0),
                    row.task.as_ref().map(|t| &t.1),
                    binding_seq
                ],
            )?;
            tx.execute(
                "INSERT INTO usage_seen_ids (stream_id, key, digest, detail, run_id, hour)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                params![stream_id, o.key, digest, row.run_id, row.hour],
            )?;
            if let Some(turn) = &o.turn {
                // One row per turn and attribution it has observations in,
                // with its first native time there.
                tx.execute(
                    "INSERT INTO usage_turns
                     (stream_id, turn, run_id, scope, agent_id, incarnation, aside_execution,
                      work, account, model, first_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT DO UPDATE SET first_ms = CASE
                         WHEN first_ms IS NULL THEN excluded.first_ms
                         WHEN excluded.first_ms IS NULL THEN first_ms
                         ELSE MIN(first_ms, excluded.first_ms) END",
                    params![
                        stream_id,
                        format!("{}:{turn}", o.generation),
                        row.run_id,
                        row.scope,
                        row.agent_id,
                        row.incarnation,
                        row.aside_execution,
                        row.work,
                        row.account,
                        row.model,
                        o.native_ms
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
                model, unit, reason, flags, band, i, n, r, w, w5, w1, o, task_id, class,
                turn IS NOT NULL
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
                task: r
                    .get::<_, Option<String>>(20)?
                    .zip(r.get::<_, Option<String>>(21)?),
                has_turn: r.get(22)?,
            })
        },
    )?)
}

/// Adds (`sign` 1) or removes (-1) one observation's contribution to the
/// aggregate row of `table` keyed by `key`, with checked arithmetic.
fn fold(
    tx: &Connection,
    table: &str,
    key: &[(&str, &dyn ToSql)],
    row: &Row,
    sign: i64,
) -> Result<()> {
    let filter = key
        .iter()
        .enumerate()
        .map(|(n, (column, _))| format!("{column} = ?{}", n + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let values: Vec<&dyn ToSql> = key.iter().map(|(_, v)| *v).collect();
    type Stored = (i64, [Option<i64>; 7], Option<i64>, Option<i64>);
    let existing: Option<Stored> = tx
        .query_row(
            &format!(
                "SELECT count, i, n, r, w, w5, w1, o, min_i, max_i FROM {table} WHERE {filter}"
            ),
            values.as_slice(),
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
        tx.execute(
            &format!("DELETE FROM {table} WHERE {filter}"),
            values.as_slice(),
        )?;
        return Ok(());
    }
    let mut next = [None; 7];
    for (n, slot) in next.iter_mut().enumerate() {
        *slot = add(sums[n], row.counters[n], sign)?;
    }
    // The input range only widens: a removed request leaves it conservative.
    let request = row.unit == "request";
    let input = row.counters[0].filter(|_| request);
    let widen = |bound: Option<i64>, pick: fn(i64, i64) -> i64| match (bound, input) {
        (Some(a), Some(b)) if sign > 0 => Some(pick(a, b)),
        (None, Some(b)) if sign > 0 => Some(b),
        (a, _) => a,
    };
    let (min_i, max_i) = (widen(min_i, i64::min), widen(max_i, i64::max));
    let mut columns: Vec<&str> = key.iter().map(|(c, _)| *c).collect();
    columns.extend([
        "count", "i", "n", "r", "w", "w5", "w1", "o", "min_i", "max_i",
    ]);
    let mut all = values;
    all.push(&count);
    for v in &next {
        all.push(v);
    }
    all.push(&min_i);
    all.push(&max_i);
    let marks = (1..=all.len())
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    tx.execute(
        &format!(
            "INSERT INTO {table} ({}) VALUES ({marks})
             ON CONFLICT DO UPDATE SET count = excluded.count, i = excluded.i, n = excluded.n,
               r = excluded.r, w = excluded.w, w5 = excluded.w5, w1 = excluded.w1,
               o = excluded.o, min_i = excluded.min_i, max_i = excluded.max_i",
            columns.join(", ")
        ),
        all.as_slice(),
    )?;
    Ok(())
}

/// Moves one observation's contribution in the run aggregates.
fn rollup(tx: &Connection, row: &Row, sign: i64) -> Result<()> {
    fold(
        tx,
        "usage_rollups",
        &[
            ("run_id", &row.run_id),
            ("scope", &row.scope),
            ("hour", &row.hour),
            ("stream_id", &row.stream_id),
            ("agent_id", &row.agent_id),
            ("incarnation", &row.incarnation),
            ("aside_execution", &row.aside_execution),
            ("work", &row.work),
            ("account", &row.account),
            ("model", &row.model),
            ("unit", &row.unit),
            ("reason", &row.reason),
            ("flags", &row.flags),
            ("band", &row.band),
            ("has_turn", &row.has_turn),
        ],
        row,
        sign,
    )?;
    if let Some((task, class)) = &row.task {
        fold(
            tx,
            "usage_task_costs",
            &[
                ("run_id", &row.run_id),
                ("task_id", task),
                ("class", class),
                ("hour", &row.hour),
                ("stream_id", &row.stream_id),
                ("incarnation", &row.incarnation),
                ("model", &row.model),
                ("unit", &row.unit),
                ("reason", &row.reason),
                ("flags", &row.flags),
                ("band", &row.band),
            ],
            row,
            sign,
        )?;
    }
    Ok(())
}

/// Where task-bound live usage stands against the task's first completion:
/// read before it was recorded, or timed at or before it, is `before`;
/// timed after it is `after`; untimed after it is `ambiguous`. Usage read
/// before a completion is recorded was written before it.
fn classify(
    tx: &Connection,
    run_id: &str,
    task_id: &str,
    native_ms: Option<i64>,
) -> Result<String> {
    let completed: Option<i64> = tx
        .query_row(
            "SELECT completed_at FROM usage_first_completions WHERE run_id = ?1 AND task_id = ?2",
            params![run_id, task_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match (completed, native_ms) {
        (None, _) => "before",
        (Some(t), Some(ms)) if ms <= t => "before",
        (Some(_), Some(_)) => "after",
        (Some(_), None) => "ambiguous",
    }
    .into())
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
            assert!(ledger.turns("r1", "live").unwrap() > 0);
        }
    }

    fn epic_task(id: &str, epic: &str) -> WorkBinding {
        WorkBinding {
            work: WorkAttribution::Task {
                id: id.into(),
                epic: EpicAttribution::Epic { id: epic.into() },
            },
            account: AccountAttribution::Known {
                id: crew_core::rpc::features::Id::new("acct-1".to_string()).unwrap(),
                assignment_generation: Wide(1),
                interval_id: crew_core::rpc::features::Id::new("iv-1".to_string()).unwrap(),
            },
            board_revision: Some(1),
            event_seq: Some(1),
        }
    }

    fn at(id: &str, ms: i64, o: u64) -> Value {
        assistant(id, ms, usage(10, 80, 10, o))
    }

    fn untimed(id: &str, o: u64) -> Value {
        let mut v = at(id, NOW, o);
        v.as_object_mut().unwrap().remove("timestamp");
        v
    }

    /// The exact cost of one Haiku request of 10/80/10 tokens and `o` output
    /// (no TTL split: a range), via the metrics of a single observation.
    fn one_cost(o: u64) -> (u128, u128) {
        let n = crate::usage_metrics::counters::claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(Some(10)),
            Ok(None),
            Ok(None),
            Ok(Some(o)),
        );
        let fact = Fact {
            unit: ObservationUnit::Request,
            normalized: n,
            model: Some("claude-haiku-4-5-20251001".into()),
            agent: "i1".into(),
            source: "s".into(),
            turn: None,
            unusable: None,
            weight: 1,
            input_range: None,
        };
        let e = totals(&[fact], &Book::standard_2026_10_08(), None).estimate;
        (e.lower_pico_usd.unwrap().0, e.upper_pico_usd.unwrap().0)
    }

    #[test]
    fn the_completed_task_cohort_uses_first_completions_and_spend_up_to_them() {
        let book = Book::standard_2026_10_08();
        let (_dir, path, mut ledger, stream) = setup(&[]);
        let t = NOW + 10 * HOUR;
        ledger
            .bind_work(&stream, &open(&path), &epic_task("t1", "e1"), NOW)
            .unwrap();
        append(&path, &[at("a", t - HOUR, 10)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        ledger
            .bind_work(&stream, &open(&path), &epic_task("t2", "e2"), NOW)
            .unwrap();
        append(&path, &[at("b", t - HOUR, 20)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();

        let e1 = EpicAttribution::Epic { id: "e1".into() };
        let e2 = EpicAttribution::Epic { id: "e2".into() };
        assert!(ledger
            .first_completion("r1", "t1", t, Some(5), &e1)
            .unwrap());
        assert!(
            !ledger
                .first_completion("r1", "t1", t + HOUR, Some(9), &e1)
                .unwrap(),
            "recompletion"
        );
        assert!(ledger
            .first_completion("r1", "t2", t, Some(6), &e2)
            .unwrap());
        // t1 gets more work after its completion: not part of its cost.
        ledger
            .bind_work(&stream, &open(&path), &epic_task("t1", "e1"), NOW)
            .unwrap();
        append(&path, &[at("c", t + HOUR, 1000)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();

        let range = Some((NOW, NOW + 20 * HOUR));
        let c = ledger.cohort("r1", range, |_, _| true, &book).unwrap();
        assert!(c.membership_complete);
        assert_eq!(c.first_completions, Wide(2));
        assert_eq!(c.tasks_with_observed_cost, Wide(2));
        let e = &c.cost_per_completed_task;
        let (a, b) = (one_cost(10), one_cost(20));
        assert_eq!(e.lower_pico_usd, Some(Wide((a.0 + b.0) / 2)));
        assert_eq!(e.upper_pico_usd, Some(Wide((a.1 + b.1).div_ceil(2))));
        assert!(!c.includes_earlier_spend);

        // The completion epic selects the cohort.
        let only = ledger.cohort("r1", range, |_, e| e == &e1, &book).unwrap();
        assert_eq!(only.first_completions, Wide(1));
        assert_eq!(only.cost_per_completed_task.lower_pico_usd, Some(Wide(a.0)));

        // A period starting after the spend includes the earlier spend.
        let late = ledger
            .cohort("r1", Some((t, t + HOUR)), |_, _| true, &book)
            .unwrap();
        assert!(late.includes_earlier_spend);

        // Untimed usage recorded after t1's completion may predate it.
        append(&path, &[untimed("d", 7)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let wider = ledger.cohort("r1", range, |_, e| e == &e1, &book).unwrap();
        let d = one_cost(7);
        assert_eq!(
            wider.cost_per_completed_task.lower_pico_usd,
            Some(Wide(a.0))
        );
        assert_eq!(
            wider.cost_per_completed_task.upper_pico_usd,
            Some(Wide(a.1 + d.1)),
            "ambiguous spend widens only the upper bound"
        );

        // A task completed with no observed cost: a whole-cohort lower bound.
        ledger.first_completion("r1", "t3", t, None, &e2).unwrap();
        let c = ledger.cohort("r1", range, |_, _| true, &book).unwrap();
        assert_eq!(c.first_completions, Wide(3));
        assert_eq!(c.tasks_with_observed_cost, Wide(2));
        assert_eq!(c.cost_per_completed_task.upper_pico_usd, None);
        assert_eq!(
            c.cost_per_completed_task.reason,
            Some(CoverageReason::NoEligibleCost)
        );

        // A period before tracking began: membership unknown, no figure.
        let early = ledger
            .cohort("r1", Some((NOW - DAY, NOW + 20 * HOUR)), |_, _| true, &book)
            .unwrap();
        assert!(!early.membership_complete);
        assert_eq!(early.cost_per_completed_task.lower_pico_usd, None);
        let empty = ledger
            .cohort("r1", Some((t + DAY, t + 2 * DAY)), |_, _| true, &book)
            .unwrap();
        assert_eq!(
            empty.cost_per_completed_task.reason,
            Some(CoverageReason::NoCompletions)
        );
    }

    #[test]
    fn unattributed_live_usage_leaves_cohort_cost_open() {
        let book = Book::standard_2026_10_08();
        let (_dir, path, mut ledger, stream) = setup(&[]);
        append(&path, &[at("unbound", NOW, 5)]);
        ledger
            .bind_work(&stream, &open(&path), &epic_task("t1", "e1"), NOW)
            .unwrap();
        append(&path, &[at("a", NOW, 10)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        ledger
            .first_completion("r1", "t1", NOW + HOUR, None, &EpicAttribution::NoEpic)
            .unwrap();
        let c = ledger.cohort("r1", None, |_, _| true, &book).unwrap();
        let e = &c.cost_per_completed_task;
        assert_eq!(e.reason, Some(CoverageReason::UnknownAttribution));
        assert_eq!(e.lower_pico_usd, Some(Wide(one_cost(10).0)));
        assert_eq!(e.upper_pico_usd, None);
        // Account captured with the binding narrows facts too.
        let facts = ledger
            .facts("r1", "live", None, |a| a.account.contains("acct-1"))
            .unwrap();
        assert_eq!(facts.iter().map(|f| f.weight).sum::<u64>(), 1);
    }

    #[test]
    fn aside_executions_charge_only_new_usage_in_their_own_scope() {
        let book = Book::standard_2026_10_08();
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::in_memory().unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        // The fork's file already holds the copied parent context.
        let path = dir.path().join("fork.jsonl");
        std::fs::write(&path, text(&[msg("parent-1", 100), msg("parent-2", 200)])).unwrap();
        let mut r = reg("r1", "i1", Start::Existing);
        r.kind = UsageKind::Aside;
        r.native_session_id = "fork-1".into();
        r.aside = Some(AsideIds {
            execution_id: "x1".into(),
            aside_id: "q1".into(),
        });
        let mut answer = msg("answer", 7);
        answer["sessionId"] = json!("fork-1");
        let mut copied = msg("parent-1", 100);
        copied["sessionId"] = json!("fork-1");
        std::fs::write(&path, text(&[copied.clone()])).unwrap();
        let stream = ledger.register(&r, &open(&path), NOW).unwrap().unwrap();
        append(&path, &[answer]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();

        let usage = ledger.aside_usage("r1", "x1", &book).unwrap().unwrap();
        assert_eq!(usage.counters.output, Some(Wide(7)), "only the new answer");
        assert_eq!(usage.counters.input_total, Some(Wide(100)));
        assert!(usage.cost.unwrap().lower_pico_usd.is_some());
        assert_eq!(
            sum(&ledger, "r1", "live").requests_observed.known_sum,
            None,
            "aside usage never enters live totals"
        );
        assert_eq!(
            sum(&ledger, "r1", "pre_tracking").output.known_sum,
            Some(Wide(100)),
            "copied context only seeds"
        );

        // A failed execution keeps its spend; one without a source is unknown.
        ledger
            .execution_status(
                "r1",
                "q1",
                "x1",
                ExecutionStatus::Failed,
                Some(NOW),
                Some(NOW),
            )
            .unwrap();
        ledger
            .execution_status(
                "r1",
                "q1",
                "x2",
                ExecutionStatus::Answered,
                Some(NOW),
                Some(NOW),
            )
            .unwrap();
        assert_eq!(ledger.aside_usage("r1", "x2", &book).unwrap(), None);
        let total = ledger.aside_total_cost("r1", "q1", &book).unwrap().unwrap();
        assert_eq!(total.state, EstimateState::Partial);
        assert_eq!(total.reason, Some(CoverageReason::NotTracked));
        assert_eq!(total.upper_pico_usd, None);
        assert!(total.lower_pico_usd.is_some());
        let ids = ledger
            .rows("r1", "aside", |_| true)
            .unwrap()
            .into_iter()
            .map(|(a, _)| a.aside_execution)
            .collect();
        let (observed, statuses) = ledger.executions("r1", &ids, None, false).unwrap();
        assert_eq!(observed.known_sum, Some(Wide(1)));
        assert_eq!(observed.state, MetricState::Partial);
        assert_eq!(statuses.failed, Wide(1));
        assert_eq!(statuses.answered, Wide(0), "x2 has no observed usage");
        assert!(!statuses.complete);
        assert_eq!(
            ledger.aside_total_cost("r1", "q-none", &book).unwrap(),
            None
        );
    }

    #[test]
    fn a_changed_expired_id_in_a_rewritten_prefix_is_outside_the_horizon() {
        let (_dir, path, mut ledger, stream) = setup(&[msg("a", 5), msg("b", 6)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let before = sum(&ledger, "r1", "live");
        ledger.expire(NOW + 31 * DAY, Retention::default()).unwrap();
        // No append in between: the rewrite alone carries the lower value.
        replace(&path, &[msg("a", 1), msg("b", 6)]);
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.outside_horizon).sum::<u64>(), 1);
        assert_eq!(done.iter().map(|c| c.quarantined).sum::<u64>(), 0);
        assert!(ledger.uncertain("r1").unwrap());
        assert_eq!(
            sum(&ledger, "r1", "live"),
            before,
            "aggregates untouched, marked uncertain"
        );
    }

    #[test]
    fn registering_again_keeps_membership_and_records_the_current_home() {
        let (dir, path, mut ledger, stream) = setup(&[msg("a", 5)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        // The same launch again: no new owner.
        assert_eq!(
            ledger
                .register(&reg("r1", "i1", Start::Fresh), &open(&path), NOW)
                .unwrap(),
            Ok(stream.clone())
        );
        let owners: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM usage_owners", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owners, 1);
        // Relocated: the same session now in another home.
        let moved = dir.path().join("elsewhere.jsonl");
        std::fs::copy(&path, &moved).unwrap();
        let mut relocated = reg("r1", "i1", Start::Fresh);
        relocated.launch_id = "launch-2".into();
        relocated.locator = Locator::CliHome {
            path: dir.path().join("home"),
        };
        relocated.started_ms = NOW + 5;
        ledger
            .register(&relocated, &open(&moved), NOW)
            .unwrap()
            .unwrap();
        let (locator, started): (String, i64) = ledger
            .conn
            .query_row(
                "SELECT locator, started_ms FROM usage_streams WHERE stream_id = ?1",
                params![stream],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Locator>(&locator).unwrap(),
            relocated.locator
        );
        assert_eq!(started, NOW + 5);
        // Seen ids survive: the copied history is not charged again, and
        // what is appended after the move is.
        let done = drain(&mut ledger, &stream, &open(&moved), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.unchanged).sum::<u64>(), 1);
        append(&moved, &[msg("b", 6)]);
        drain(&mut ledger, &stream, &open(&moved), NOW).unwrap();
        assert_eq!(
            sum(&ledger, "r1", "live").requests_observed.known_sum,
            Some(Wide(2))
        );
        let active = ledger.active_streams().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].locator, relocated.locator);
    }

    #[test]
    fn old_codex_token_counts_appended_again_never_charge_twice() {
        use crate::usage_metrics::source::Start;
        let tc = |ms: i64, total: (u64, u64, u64), last: (u64, u64, u64)| {
            let v = |(i, r, o): (u64, u64, u64)| json!({"input_tokens": i, "cached_input_tokens": r, "output_tokens": o});
            json!({"type": "event_msg",
                "timestamp": chrono::DateTime::from_timestamp_millis(ms).unwrap().to_rfc3339(),
                "payload": {"type": "token_count", "info": {"total_token_usage": v(total), "last_token_usage": v(last)}}})
        };
        let meta = json!({"type": "session_meta", "payload": {"id": "th-1"}});
        let ctx =
            json!({"type": "turn_context", "payload": {"model": "gpt-6-luna", "turn_id": "t1"}});
        let a = tc(NOW, (100, 80, 10), (100, 80, 10));
        let b = tc(NOW + 1, (150, 120, 15), (50, 40, 5));
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("crew.db");
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(
            &path,
            text(&[meta.clone(), ctx.clone(), a.clone(), b.clone()]),
        )
        .unwrap();
        let mut ledger = Ledger::open(&db).unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let mut r = reg("r1", "i1", Start::Fresh);
        r.runtime = RuntimeKind::Codex;
        r.native_session_id = "th-1".into();
        let stream = ledger.register(&r, &open(&path), NOW).unwrap().unwrap();
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let input = |l: &Ledger| sum(l, "r1", "live").input_total.known_sum;
        assert_eq!(input(&ledger), Some(Wide(150)));

        // The exact old records appended again.
        append(&path, &[a.clone(), b.clone()]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(input(&ledger), Some(Wide(150)));

        // After detail expiry, a restart and a rewrite holding the same history.
        ledger.expire(NOW + 31 * DAY, Retention::default()).unwrap();
        drop(ledger);
        let mut ledger = Ledger::open(&db).unwrap();
        append(&path, &[a.clone(), b.clone()]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(input(&ledger), Some(Wide(150)));
        replace(&path, &[meta, ctx, a.clone(), b.clone(), a, b]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(input(&ledger), Some(Wide(150)));
        assert!(
            !ledger.uncertain("r1").unwrap(),
            "identical replays are not corrections"
        );
    }

    #[test]
    fn a_replayed_codex_increment_with_other_metadata_is_quarantined() {
        use crate::usage_metrics::source::Start;
        let v = |i: u64, r: u64, o: u64| json!({"input_tokens": i, "cached_input_tokens": r, "output_tokens": o});
        let tc = |total: Value, last: Option<Value>| {
            let mut info = json!({"total_token_usage": total});
            if let Some(last) = last {
                info["last_token_usage"] = last;
            }
            json!({"type": "event_msg", "timestamp": "2026-10-08T00:00:00Z",
                "payload": {"type": "token_count", "info": info}})
        };
        let ctx = |model: &str| json!({"type": "turn_context", "payload": {"model": model, "turn_id": "t1"}});
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(
            &path,
            text(&[
                ctx("gpt-6-luna"),
                tc(v(100, 80, 10), Some(v(100, 80, 10))),
                tc(v(150, 120, 15), Some(v(50, 40, 5))),
            ]),
        )
        .unwrap();
        let mut ledger = Ledger::in_memory().unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let mut r = reg("r1", "i1", Start::Fresh);
        r.runtime = RuntimeKind::Codex;
        r.native_session_id = "th-1".into();
        let stream = ledger.register(&r, &open(&path), NOW).unwrap().unwrap();
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let before = dump(&ledger)
            .into_iter()
            .filter(|row| row.starts_with("usage_observations"))
            .collect::<Vec<_>>();
        // The 100..150 edge again under another model and without its
        // direct vector, after a rewrite that seeds the same history.
        replace(
            &path,
            &[
                ctx("gpt-6-luna"),
                tc(v(100, 80, 10), Some(v(100, 80, 10))),
                ctx("gpt-6.1-sol"),
                tc(v(150, 120, 15), None),
            ],
        );
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        append(
            &path,
            &[tc(v(100, 80, 10), None), tc(v(150, 120, 15), None)],
        );
        let done = drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(done.iter().map(|c| c.revised).sum::<u64>(), 0);
        let after = dump(&ledger)
            .into_iter()
            .filter(|row| row.starts_with("usage_observations"))
            .collect::<Vec<_>>();
        assert_eq!(
            after, before,
            "the original increment's model and unit stand"
        );
    }

    #[test]
    fn late_corrections_make_cohort_and_aside_readback_unavailable() {
        let book = Book::standard_2026_10_08();
        // Cohort: t1's spend corrected downward after its detail expired.
        let (_dir, path, mut ledger, stream) = setup(&[]);
        ledger
            .bind_work(&stream, &open(&path), &epic_task("t1", "e1"), NOW)
            .unwrap();
        append(&path, &[at("a", NOW, 10)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        ledger
            .first_completion("r1", "t1", NOW + HOUR, None, &EpicAttribution::NoEpic)
            .unwrap();
        let before = ledger.cohort("r1", None, |_, _| true, &book).unwrap();
        assert!(before.cost_per_completed_task.lower_pico_usd.is_some());
        ledger.expire(NOW + 31 * DAY, Retention::default()).unwrap();
        append(&path, &[at("a", NOW, 1)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let after = ledger.cohort("r1", None, |_, _| true, &book).unwrap();
        let e = &after.cost_per_completed_task;
        assert_eq!(e.state, EstimateState::Unavailable);
        assert_eq!(e.reason, Some(CoverageReason::CorrectionOutsideHorizon));
        assert_eq!((e.lower_pico_usd, e.upper_pico_usd), (None, None));

        // Aside: an execution's answer corrected after expiry.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fork.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut ledger = Ledger::in_memory().unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let mut r = reg("r1", "i1", Start::Fresh);
        r.kind = UsageKind::Aside;
        r.aside = Some(AsideIds {
            execution_id: "x1".into(),
            aside_id: "q1".into(),
        });
        let stream = ledger.register(&r, &open(&path), NOW).unwrap().unwrap();
        append(&path, &[msg("ans", 7)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert!(ledger
            .aside_usage("r1", "x1", &book)
            .unwrap()
            .unwrap()
            .counters
            .output
            .is_some());
        ledger.expire(NOW + 31 * DAY, Retention::default()).unwrap();
        append(&path, &[msg("ans", 2)]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let usage = ledger.aside_usage("r1", "x1", &book).unwrap().unwrap();
        assert_eq!(usage.counters.output, None);
        assert_eq!(usage.counters.input_total, None);
        let cost = usage.cost.unwrap();
        assert_eq!(cost.state, EstimateState::Unavailable);
        assert_eq!(cost.lower_pico_usd, None);
        let total = ledger.aside_total_cost("r1", "q1", &book).unwrap().unwrap();
        assert_eq!(total.state, EstimateState::Unavailable);
        assert_eq!(total.reason, Some(CoverageReason::CorrectionOutsideHorizon));
    }

    fn codex_tc(total: (u64, u64, u64), last: Option<(u64, u64, u64)>) -> Value {
        let v = |(i, r, o): (u64, u64, u64)| json!({"input_tokens": i, "cached_input_tokens": r, "output_tokens": o});
        let mut info = json!({"total_token_usage": v(total)});
        if let Some(last) = last {
            info["last_token_usage"] = v(last);
        }
        json!({"type": "event_msg", "timestamp": "2026-10-08T00:00:00Z",
            "payload": {"type": "token_count", "info": info}})
    }

    fn codex_setup(values: &[Value]) -> (tempfile::TempDir, PathBuf, Ledger, String) {
        use crate::usage_metrics::source::Start;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, text(values)).unwrap();
        let mut ledger = Ledger::open(&dir.path().join("crew.db")).unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let mut r = reg("r1", "i1", Start::Fresh);
        r.runtime = RuntimeKind::Codex;
        r.native_session_id = "th-1".into();
        let stream = ledger.register(&r, &open(&path), NOW).unwrap().unwrap();
        (dir, path, ledger, stream)
    }

    #[test]
    fn a_rewrite_holding_part_of_the_history_charges_nothing_already_charged() {
        let ctx =
            json!({"type": "turn_context", "payload": {"model": "gpt-6-luna", "turn_id": "t1"}});
        let a = codex_tc((100, 80, 10), Some((100, 80, 10)));
        let b = codex_tc((150, 120, 15), Some((50, 40, 5)));
        let c = codex_tc((200, 160, 20), Some((50, 40, 5)));
        let (dir, path, mut ledger, stream) = codex_setup(&[ctx.clone(), a.clone(), b, c.clone()]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let input = |l: &Ledger| sum(l, "r1", "live").input_total.known_sum;
        assert_eq!(input(&ledger), Some(Wide(200)));
        // Replaced by the old first record alone, then a restart, then the
        // old last record appended: no genuine request in between.
        replace(&path, &[ctx, a]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        drop(ledger);
        let mut ledger = Ledger::open(&dir.path().join("crew.db")).unwrap();
        append(&path, &[c]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(input(&ledger), Some(Wide(200)));
        // Usage beyond everything charged is new.
        append(&path, &[codex_tc((260, 200, 25), Some((60, 40, 5)))]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(input(&ledger), Some(Wide(260)));
    }

    #[test]
    fn a_batch_spanning_a_work_binding_change_belongs_to_neither_side() {
        let ctx =
            json!({"type": "turn_context", "payload": {"model": "gpt-6-luna", "turn_id": "t1"}});
        let (_dir, path, mut ledger, stream) =
            codex_setup(&[ctx, codex_tc((100, 80, 10), Some((100, 80, 10)))]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        ledger
            .bind_work(&stream, &open(&path), &task("ta"), NOW)
            .unwrap();
        // Task A's usage is never reported on its own.
        ledger
            .bind_work(&stream, &open(&path), &task("tb"), NOW)
            .unwrap();
        append(&path, &[codex_tc((300, 200, 30), Some((100, 60, 10)))]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        let by_task =
            |ledger: &Ledger, id: &str| {
                let facts = ledger
                .facts("r1", "live", None, |a| {
                    matches!(a.work(), Some(WorkAttribution::Task { id: t, .. }) if t == id)
                })
                .unwrap();
                totals(&facts, &Book::standard_2026_10_08(), None)
                    .input_total
                    .known_sum
            };
        assert_eq!(by_task(&ledger, "ta"), None);
        assert_eq!(
            by_task(&ledger, "tb"),
            None,
            "not all 200 given to the later binding"
        );
        let multiple = ledger
            .facts("r1", "live", None, |a| {
                matches!(
                    a.work(),
                    Some(WorkAttribution::Unknown {
                        reason: CoverageReason::MultipleTasks
                    })
                )
            })
            .unwrap();
        let t = totals(&multiple, &Book::standard_2026_10_08(), None);
        assert_eq!(
            t.input_total.known_sum,
            Some(Wide(200)),
            "tokens kept as evidence"
        );
        assert_eq!(
            sum(&ledger, "r1", "live").input_total.known_sum,
            Some(Wide(300))
        );
        // A single proven request after the boundary is the later binding's.
        append(&path, &[codex_tc((350, 240, 35), Some((50, 40, 5)))]);
        drain(&mut ledger, &stream, &open(&path), NOW).unwrap();
        assert_eq!(by_task(&ledger, "tb"), Some(Wide(50)));
    }
}
