//! Answers to usage queries from the ledger.
//!
//! A query resolves to a canonical one first: its run, live or aside scope,
//! its exact half-open range, its bucket and price book, all stated in the
//! reply. Whole hours inside a range come from retained aggregates, so their
//! figures do not change when detail expires; partial hours at a range or
//! bucket edge come from retained detail, and a figure that needs expired
//! detail is unavailable rather than widened or truncated. Every metric
//! keeps its own coverage, and the source gaps, backlog and retention floor
//! of the run are repeated on each. Pages stop at their row limit and the
//! 512 KiB page budget; a cursor pins its query and the ledger revision.

use std::collections::{BTreeMap, BTreeSet};

use crew_core::rpc::features::{
    Id, List, PageRequest, Text, Timestamp, Wide, MAX_METADATA_ROWS, MAX_PAGE_BYTES,
};
use crew_core::rpc::usage_metrics::{
    AccountAttribution, ActivityKind, CompletionCohort, CounterMetric, CoverageExclusion,
    CoverageReason, EpicAttribution, MetricCoverage, MetricState, ObservationUnit, PriceBook,
    SourceGap, TimeBasis, TrackingState, UsageBinding, UsageBreakdown, UsageBreakdownRow,
    UsageBucket, UsageCounters, UsageDimension, UsageEpicFilter, UsageGroupKey, UsageKind,
    UsageObservation, UsageObservations, UsagePage, UsagePrices, UsageQuery, UsageRun, UsageRuns,
    UsageScope, UsageSeries, UsageSummary, UsageTaskFilter, UsageTotals, WorkAttribution,
};
use crew_core::rpc::RpcError;
use crew_core::RuntimeKind;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::counters::Normalized;
use super::ledger::{Aggregate, Gap, Ledger, ObservationRow, RunInfo, TurnRow};
use super::metrics::{category_split_complete, count, totals, Fact};
use super::price::Book;

const HOUR_MS: i64 = 3_600_000;
/// At most this many buckets per series page.
pub const MAX_BUCKETS: usize = 720;
/// At most this many source gaps listed on a metric's coverage.
const MAX_GAPS: usize = 64;

/// What the service knows that the ledger does not.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub now: i64,
    /// Model-backed member incarnations registered in the run, including
    /// unsupported or missing sources; `None` when the run's membership is
    /// not known (historical runs without evidence).
    pub expected_agents: Option<u64>,
    /// Source files with unread data, when the collector knows.
    pub backlog_files: Option<u64>,
}

/// The price books this build knows, newest first.
pub fn books() -> Vec<Book> {
    vec![Book::standard_2026_10_08()]
}

/// The book with `id`, or the newest.
pub fn book(id: Option<&Id>) -> Result<Book, RpcError> {
    let all = books();
    match id {
        None => Ok(all.into_iter().next().expect("one price book")),
        Some(id) => all
            .into_iter()
            .find(|b| b.id == id.as_str())
            .ok_or_else(|| RpcError::not_found(format!("no price book {}", id.as_str()))),
    }
}

fn invalid(message: &str) -> RpcError {
    RpcError::new(
        crew_core::rpc::features::INVALID_FEATURE_REQUEST,
        message.to_string(),
    )
}

/// The grid point at or below `ms`; saturating, so an unbounded end
/// (`i64::MIN`) stays unbounded.
fn floor(ms: i64, unit: i64) -> i64 {
    ms.saturating_sub(ms.rem_euclid(unit))
}

/// The grid point at or above `ms`; saturating at `i64::MAX`.
fn ceil(ms: i64, unit: i64) -> i64 {
    let f = floor(ms, unit);
    if f == ms {
        ms
    } else {
        f.saturating_add(unit)
    }
}

fn timestamp(ms: i64) -> Result<Timestamp, RpcError> {
    Timestamp::new(ms).map_err(|_| invalid("time out of range"))
}

/// The canonical form of `query`: run checked, price book and bucket
/// resolved, page limit defaulted; its range is kept exactly. `series`
/// resolves a bucket.
pub fn canonical(
    ledger: &Ledger,
    query: &UsageQuery,
    max_rows: u16,
    series: bool,
) -> Result<(UsageQuery, RunInfo, Book), RpcError> {
    query.validate(max_rows).map_err(invalid)?;
    let run = ledger
        .run(query.scope.workspace_run_id.as_str())
        .map_err(internal)?
        .ok_or_else(|| RpcError::not_found("no usage is tracked for that run"))?;
    let book = book(query.price_book_id.as_ref())?;
    // One owner for the shape: the protocol's rule, shared with clients that
    // check a reply against their request.
    let canonical = query
        .resolved_shape(
            Id::new(book.id.clone()).expect("price book id is valid"),
            max_rows,
            series,
        )
        .map_err(invalid)?;
    Ok((canonical, run, book))
}

fn internal(e: anyhow::Error) -> RpcError {
    RpcError::internal(format!("usage ledger: {e:#}"))
}

fn scope_name(kind: UsageKind) -> &'static str {
    match kind {
        UsageKind::Live => "live",
        UsageKind::Aside => "aside",
    }
}

fn work_of(a: &Aggregate) -> WorkAttribution {
    a.work().unwrap_or(WorkAttribution::Unknown {
        reason: CoverageReason::UnknownAttribution,
    })
}

fn keep_work(scope: &UsageScope, work: &WorkAttribution) -> bool {
    let task = match &scope.task {
        UsageTaskFilter::All => true,
        UsageTaskFilter::Id(t) => matches!(work, WorkAttribution::Task { id, .. } if id == t),
        UsageTaskFilter::NoTask => matches!(work, WorkAttribution::NoTask { .. }),
        UsageTaskFilter::NotAttributed => matches!(work, WorkAttribution::Unknown { .. }),
    };
    let epic = match &scope.epic {
        UsageEpicFilter::All => true,
        UsageEpicFilter::Id(e) => matches!(
            work,
            WorkAttribution::Task { epic: EpicAttribution::Epic { id }, .. } if id == e
        ),
        UsageEpicFilter::NoEpic => matches!(
            work,
            WorkAttribution::Task {
                epic: EpicAttribution::NoEpic,
                ..
            }
        ),
        UsageEpicFilter::NoTask => matches!(work, WorkAttribution::NoTask { .. }),
        UsageEpicFilter::NotAttributed => matches!(
            work,
            WorkAttribution::Unknown { .. }
                | WorkAttribution::Task {
                    epic: EpicAttribution::Unknown { .. },
                    ..
                }
        ),
    };
    task && epic
}

fn account_of(a: &Aggregate) -> Option<String> {
    match serde_json::from_str::<AccountAttribution>(&a.account).ok()? {
        AccountAttribution::Known { id, .. } => Some(id.as_str().to_string()),
        AccountAttribution::Unknown { .. } => None,
    }
}

/// Whether an aggregate is in the query's scope, apart from time.
fn keep(scope: &UsageScope, a: &Aggregate) -> bool {
    scope
        .agent_incarnation
        .as_ref()
        .is_none_or(|i| i.as_str() == a.incarnation)
        && scope.model.as_ref().is_none_or(|m| **m == a.model)
        && scope
            .account_id
            .as_ref()
            .is_none_or(|id| account_of(a).as_deref() == Some(id.as_str()))
        && keep_work(scope, &work_of(a))
}

fn range_of(q: &UsageQuery) -> Option<(i64, i64)> {
    match (q.from_ms, q.to_ms) {
        (None, None) => None,
        (from, to) => Some((
            from.map_or(i64::MIN, |t| t.get()),
            to.map_or(i64::MAX, |t| t.get()),
        )),
    }
}

/// Run-level evidence repeated on every metric's coverage.
struct Extras {
    gaps: Vec<SourceGap>,
    gaps_total: u64,
    backlog: Option<u64>,
    retained_from: Option<i64>,
    uncertain: BTreeSet<(i64, String)>,
    reasons: BTreeSet<CoverageReasonKey>,
}

/// `CoverageReason` ordered for deduplication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CoverageReasonKey(u8, CoverageReasonOrd);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CoverageReasonOrd(CoverageReason);

impl PartialOrd for CoverageReasonOrd {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CoverageReasonOrd {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        reason_text(self.0).cmp(&reason_text(other.0))
    }
}

fn reason_text(r: CoverageReason) -> String {
    serde_json::to_value(r)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Coverage reasons that mean a source lost usage.
fn loses_usage(r: CoverageReason) -> bool {
    matches!(
        r,
        CoverageReason::SourceGap
            | CoverageReason::ParserLimit
            | CoverageReason::NotReported
            | CoverageReason::UnsupportedSidechain
            | CoverageReason::AmbiguousBoundary
            | CoverageReason::RetentionLimit
            | CoverageReason::IdentityLost
            | CoverageReason::CorrectionOutsideHorizon
            | CoverageReason::SourceBacklog
            | CoverageReason::UnsupportedRuntime
            | CoverageReason::NotTracked
    )
}

fn extras(ledger: &Ledger, run: &RunInfo, ctx: &Context) -> Result<Extras, RpcError> {
    let gaps: Vec<Gap> = ledger
        .gaps(&run.run_id)
        .map_err(internal)?
        .into_iter()
        .filter(|g| loses_usage(g.reason))
        .collect();
    let reasons = gaps
        .iter()
        .map(|g| CoverageReasonKey(0, CoverageReasonOrd(g.reason)))
        .collect();
    let listed = gaps
        .iter()
        .filter_map(|g| {
            Some(SourceGap {
                source_id: Id::new(g.stream_id.clone()).ok()?,
                reason: g.reason,
                from: Timestamp::new(g.first_at).ok(),
                to: Timestamp::new(g.last_at).ok(),
            })
        })
        .take(MAX_GAPS)
        .collect();
    Ok(Extras {
        gaps_total: gaps.len() as u64,
        gaps: listed,
        backlog: ctx.backlog_files,
        retained_from: run.retained_from,
        uncertain: ledger.uncertain_scopes(&run.run_id).map_err(internal)?,
        reasons,
    })
}

fn decorate(c: &mut MetricCoverage, x: &Extras) {
    c.source_gaps = List::new(x.gaps.clone()).expect("gaps truncated to their limit");
    c.source_gaps_total = Some(Wide(x.gaps_total as u128));
    c.source_backlog_files = x.backlog.map(|n| Wide(n as u128));
    c.retained_from = x.retained_from.and_then(|t| Timestamp::new(t).ok());
}

/// Every metric of `t`, with `f` applied to its coverage.
fn each_coverage(t: &mut UsageTotals, mut f: impl FnMut(&mut MetricCoverage)) {
    for m in [
        &mut t.input_total,
        &mut t.input_uncached,
        &mut t.cache_read,
        &mut t.cache_write,
        &mut t.cache_write_5m,
        &mut t.cache_write_1h,
        &mut t.output,
        &mut t.total_tokens,
        &mut t.requests_observed,
        &mut t.turns_observed,
    ] {
        f(&mut m.coverage);
    }
    f(&mut t.hit_rate.coverage);
    f(&mut t.write_share.coverage);
    f(&mut t.estimate.coverage);
}

/// A retained aggregate made uncertain by a late correction: every figure
/// over it is unavailable, not a stale bound.
fn poison(t: &mut UsageTotals, reason: CoverageReason) {
    for m in [
        &mut t.input_total,
        &mut t.input_uncached,
        &mut t.cache_read,
        &mut t.cache_write,
        &mut t.cache_write_5m,
        &mut t.cache_write_1h,
        &mut t.output,
        &mut t.total_tokens,
        &mut t.requests_observed,
        &mut t.turns_observed,
    ] {
        m.known_sum = None;
        m.state = MetricState::Unavailable;
        m.reason = Some(reason);
    }
    for r in [&mut t.hit_rate, &mut t.write_share] {
        r.numerator = None;
        r.denominator = None;
        r.value = None;
        r.lower = None;
        r.upper = None;
        r.state = MetricState::Unavailable;
        r.reason = Some(reason);
    }
    let e = &mut t.estimate;
    e.lower_pico_usd = None;
    e.upper_pico_usd = None;
    e.state = crew_core::rpc::usage_metrics::EstimateState::Unavailable;
    e.reason = Some(reason);
}

/// Distinct native turns, with coverage over the observations that have one.
fn turn_metric(
    distinct: u64,
    rows: &[(Aggregate, Fact)],
    template: &MetricCoverage,
) -> CounterMetric {
    let seen = rows.iter().fold(0, |n, (_, f)| count(n, f.weight));
    let with = rows
        .iter()
        .filter(|(a, _)| a.has_turn)
        .fold(0, |n, (_, f)| count(n, f.weight));
    let missing = seen - with;
    let state = if with == 0 {
        MetricState::Unavailable
    } else if missing == 0 {
        MetricState::Complete
    } else {
        MetricState::Partial
    };
    CounterMetric {
        known_sum: (with > 0).then_some(Wide(distinct as u128)),
        state,
        reason: match state {
            MetricState::Complete => None,
            _ if seen == 0 => Some(CoverageReason::NotReported),
            _ => Some(CoverageReason::MissingTurnIdentity),
        },
        coverage: MetricCoverage {
            eligible_observations: Wide(with),
            observations_seen: Wide(seen),
            exclusions: List::new(if missing > 0 {
                vec![CoverageExclusion {
                    reason: CoverageReason::MissingTurnIdentity,
                    observations: Wide(missing),
                }]
            } else {
                Vec::new()
            })
            .expect("one exclusion"),
            ..template.clone()
        },
    }
}

/// Totals over aggregate rows, with turns, uncertainty and run evidence.
fn totals_of(
    rows: &[(Aggregate, Fact)],
    turns: u64,
    book: &Book,
    ctx: &Context,
    x: &Extras,
) -> UsageTotals {
    let facts: Vec<Fact> = rows.iter().map(|(_, f)| f.clone()).collect();
    let mut t = totals(&facts, book, ctx.expected_agents);
    t.turns_observed = turn_metric(turns, rows, &t.requests_observed.coverage);
    each_coverage(&mut t, |c| decorate(c, x));
    if rows
        .iter()
        .any(|(a, _)| x.uncertain.contains(&(a.hour, a.stream_id.clone())))
    {
        poison(&mut t, CoverageReason::CorrectionOutsideHorizon);
    }
    t
}

fn attribution_coverage(rows: &[(Aggregate, Fact)], x: &Extras) -> MetricCoverage {
    let mut seen = 0u128;
    let mut eligible = 0u128;
    let mut excluded: BTreeMap<String, (CoverageReason, u128)> = BTreeMap::new();
    for (a, f) in rows {
        seen = count(seen, f.weight);
        match work_of(a) {
            WorkAttribution::Unknown { reason } => {
                let n = &mut excluded.entry(reason_text(reason)).or_insert((reason, 0)).1;
                *n = count(*n, f.weight);
            }
            _ => eligible = count(eligible, f.weight),
        }
    }
    let mut c = MetricCoverage {
        eligible_observations: Wide(eligible),
        observations_seen: Wide(seen),
        exclusions: List::new(
            excluded
                .into_values()
                .take(32)
                .map(|(reason, n)| CoverageExclusion {
                    reason,
                    observations: Wide(n),
                })
                .collect(),
        )
        .expect("exclusions truncated to their limit"),
        ..MetricCoverage::default()
    };
    decorate(&mut c, x);
    c
}

/// The scope's usage over a range, fetched once and cut into exact
/// half-open intervals: whole hours from retained aggregates, partial hours
/// from retained detail.
struct Window {
    /// Aggregates in scope; untimed ones only when the query is unbounded.
    hours: Vec<(Aggregate, Fact)>,
    /// Retained detail in scope, for partial hours.
    detail: Vec<(Aggregate, i64, Fact)>,
    /// Native turns in scope, one per attribution.
    turns: Vec<TurnRow>,
    retained_from: Option<i64>,
}

fn whole_hour(h: i64, (a, b): (i64, i64)) -> bool {
    h >= 0 && h >= a && h.checked_add(HOUR_MS).is_some_and(|end| end <= b)
}

/// The parts of `[a, b)` that are not whole hours: at most one at each end.
/// An unbounded end (`i64::MIN` or `i64::MAX`) has none.
fn partial_parts((a, b): (i64, i64)) -> Vec<(i64, i64)> {
    let mut parts = Vec::new();
    if a >= b {
        return parts;
    }
    let left_end = if a == i64::MIN || a.rem_euclid(HOUR_MS) == 0 {
        a
    } else {
        ceil(a, HOUR_MS).min(b)
    };
    if left_end > a {
        parts.push((a, left_end));
    }
    if b != i64::MAX && b.rem_euclid(HOUR_MS) != 0 {
        let start = floor(b, HOUR_MS).max(left_end);
        if start < b {
            parts.push((start, b));
        }
    }
    parts
}

impl Window {
    /// Loads the scope's usage for `range` (`None`: unbounded). `fine`
    /// loads detail for the whole range, for buckets finer than its hours.
    fn load(
        ledger: &Ledger,
        q: &UsageQuery,
        range: Option<(i64, i64)>,
        fine: bool,
    ) -> Result<Self, RpcError> {
        let run = q.scope.workspace_run_id.as_str();
        let scope = scope_name(q.scope.kind);
        let hours = ledger
            .rows(run, scope, |a| {
                range.is_none_or(|(from, to)| {
                    a.hour >= 0 && a.hour >= floor(from, HOUR_MS) && a.hour < to
                }) && keep(&q.scope, a)
            })
            .map_err(internal)?;
        let mut detail = Vec::new();
        if let Some(range) = range {
            let spans = if fine {
                vec![range]
            } else {
                partial_parts(range)
            };
            for span in spans {
                detail.extend(
                    ledger
                        .detail_rows(run, scope, span, |a| keep(&q.scope, a))
                        .map_err(internal)?,
                );
            }
        }
        let turns = ledger
            .turn_rows(run, scope)
            .map_err(internal)?
            .into_iter()
            .filter(|t| keep(&q.scope, &t.aggregate))
            .collect();
        let retained_from = ledger
            .run(run)
            .map_err(internal)?
            .and_then(|r| r.retained_from);
        Ok(Self {
            hours,
            detail,
            turns,
            retained_from,
        })
    }

    /// The usage of `span` (`None`: unbounded), and whether a partial hour
    /// of it needs detail that has expired.
    fn cut(&self, span: Option<(i64, i64)>) -> (Vec<(Aggregate, Fact)>, bool) {
        let Some((a, b)) = span else {
            return (self.hours.clone(), false);
        };
        let mut rows: Vec<(Aggregate, Fact)> = self
            .hours
            .iter()
            .filter(|(agg, _)| whole_hour(agg.hour, (a, b)))
            .cloned()
            .collect();
        rows.extend(
            self.detail
                .iter()
                .filter(|(_, ms, _)| {
                    *ms >= a && *ms < b && !whole_hour(floor(*ms, HOUR_MS), (a, b))
                })
                .map(|(agg, _, f)| (agg.clone(), f.clone())),
        );
        let expired = self
            .retained_from
            .is_some_and(|r| partial_parts((a, b)).iter().any(|(start, _)| *start < r));
        (rows, expired)
    }

    /// Distinct native turns first observed in `span` (`None`: all) whose
    /// attribution `keep` accepts.
    fn turns(&self, span: Option<(i64, i64)>, keep: impl Fn(&Aggregate) -> bool) -> u64 {
        let ids: BTreeSet<(&str, &str)> = self
            .turns
            .iter()
            .filter(|t| {
                span.is_none_or(|(a, b)| t.first_ms.is_some_and(|ms| ms >= a && ms < b))
                    && keep(&t.aggregate)
            })
            .map(|t| (t.aggregate.stream_id.as_str(), t.turn.as_str()))
            .collect();
        ids.len() as u64
    }
}

/// Totals of one interval of a window, and whether part of it needed
/// detail that expired.
fn window_totals(
    w: &Window,
    span: Option<(i64, i64)>,
    keep: impl Fn(&Aggregate) -> bool,
    book: &Book,
    ctx: &Context,
    x: &Extras,
) -> (Vec<(Aggregate, Fact)>, UsageTotals, bool) {
    let (rows, expired) = w.cut(span);
    let rows: Vec<(Aggregate, Fact)> = rows.into_iter().filter(|(a, _)| keep(a)).collect();
    let mut t = totals_of(&rows, w.turns(span, keep), book, ctx, x);
    if expired {
        poison(&mut t, CoverageReason::RetentionLimit);
    }
    (rows, t, expired)
}

/// Whether a scope narrows usage by more than run, kind and time.
fn narrowed(scope: &UsageScope) -> bool {
    scope.agent_incarnation.is_some()
        || scope.model.is_some()
        || scope.account_id.is_some()
        || scope.task != UsageTaskFilter::All
        || scope.epic != UsageEpicFilter::All
}

fn tracking_state(ledger: &Ledger, run: &RunInfo, x: &Extras) -> Result<TrackingState, RpcError> {
    Ok(if run.ended_at.is_some() {
        TrackingState::Closed
    } else if !ledger.collecting(&run.run_id).map_err(internal)? {
        TrackingState::Paused
    } else if x.backlog.is_some_and(|n| n > 0) {
        TrackingState::CatchingUp
    } else if x.gaps_total > 0 {
        TrackingState::Partial
    } else {
        TrackingState::Tracking
    })
}

/// The cohort selector of a scope: tasks by completion epic, or none.
fn cohort(
    ledger: &Ledger,
    q: &UsageQuery,
    book: &Book,
) -> Result<Option<CompletionCohort>, RpcError> {
    // The completed-task cohort is defined over tasks, by task and
    // completion epic; a member, model or account selection has no cohort.
    if q.scope.kind != UsageKind::Live
        || q.scope.agent_incarnation.is_some()
        || q.scope.model.is_some()
        || q.scope.account_id.is_some()
    {
        return Ok(None);
    }
    let task = match &q.scope.task {
        UsageTaskFilter::All => None,
        UsageTaskFilter::Id(t) => Some(t.clone()),
        UsageTaskFilter::NoTask | UsageTaskFilter::NotAttributed => return Ok(None),
    };
    if matches!(q.scope.epic, UsageEpicFilter::NoTask) {
        return Ok(None);
    }
    let epic = q.scope.epic.clone();
    let c = ledger
        .cohort(
            q.scope.workspace_run_id.as_str(),
            range_of(q),
            |id, e| {
                task.as_ref().is_none_or(|t| t == id)
                    && match &epic {
                        UsageEpicFilter::All => true,
                        UsageEpicFilter::Id(id) => {
                            matches!(e, EpicAttribution::Epic { id: x } if x == id)
                        }
                        UsageEpicFilter::NoEpic => matches!(e, EpicAttribution::NoEpic),
                        UsageEpicFilter::NotAttributed => {
                            matches!(e, EpicAttribution::Unknown { .. })
                        }
                        UsageEpicFilter::NoTask => false,
                    }
            },
            book,
        )
        .map_err(internal)?;
    Ok(Some(c))
}

/// `usage_summary`: the totals of a scope and range.
pub fn summary(
    ledger: &Ledger,
    query: &UsageQuery,
    ctx: &Context,
) -> Result<UsageSummary, RpcError> {
    let (q, run, book) = canonical(ledger, query, MAX_METADATA_ROWS as u16, false)?;
    let x = extras(ledger, &run, ctx)?;
    let range = range_of(&q);
    let window = Window::load(ledger, &q, range, false)?;
    let (rows, t, expired) = window_totals(&window, range, |_| true, &book, ctx, &x);
    let mut limitations: BTreeSet<CoverageReasonKey> = x.reasons.clone();
    if !x.uncertain.is_empty() {
        limitations.insert(CoverageReasonKey(
            0,
            CoverageReasonOrd(CoverageReason::CorrectionOutsideHorizon),
        ));
    }
    let pre_tracking = ledger
        .rows(&run.run_id, "pre_tracking", |_| true)
        .map_err(internal)?;
    if !pre_tracking.is_empty() {
        limitations.insert(CoverageReasonKey(
            0,
            CoverageReasonOrd(CoverageReason::NotTracked),
        ));
    }
    let (executions_observed, execution_statuses) = if q.scope.kind == UsageKind::Aside {
        let observed = rows
            .iter()
            .filter(|(a, _)| !a.aside_execution.is_empty())
            .map(|(a, _)| a.aside_execution.clone())
            .collect();
        let (m, s) = ledger
            .executions(
                &run.run_id,
                &observed,
                range,
                narrowed(&q.scope),
                expired.then_some(CoverageReason::RetentionLimit),
            )
            .map_err(internal)?;
        (Some(m), Some(s))
    } else {
        (None, None)
    };
    Ok(UsageSummary {
        revision: Wide(ledger.revision().map_err(internal)? as u128),
        tracking_since: Timestamp::new(run.tracking_since).ok(),
        tracking_state: tracking_state(ledger, &run, &x)?,
        attribution_coverage: attribution_coverage(&rows, &x),
        completion_cohort: cohort(ledger, &q, &book)?,
        executions_observed,
        execution_statuses,
        limitations: List::new(limitations.into_iter().take(32).map(|k| k.1 .0).collect())
            .expect("limitations truncated to their limit"),
        as_of: timestamp(ctx.now)?,
        totals: t,
        query: q,
    })
}

/// A page cursor: the kind of page, the ledger revision, the offset and a
/// digest of the canonical query without its page.
fn cursor(kind: &str, revision: u64, offset: u64, q: &UsageQuery) -> Id {
    Id::new(format!("{kind}.{revision}.{offset}.{}", digest(q))).expect("cursors are short")
}

fn digest(q: &UsageQuery) -> String {
    let mut q = q.clone();
    q.page = PageRequest::default();
    let bytes = serde_json::to_vec(&q).expect("queries serialize");
    Sha256::digest(bytes)[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The offset a cursor resumes at, or `stale_cursor`.
fn resume(kind: &str, cursor: Option<&Id>, revision: u64, q: &UsageQuery) -> Result<u64, RpcError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let stale = || RpcError::new("stale_cursor", "the usage page cursor no longer applies");
    let parts: Vec<&str> = cursor.as_str().split('.').collect();
    match parts.as_slice() {
        [k, rev, offset, d] if *k == kind && *d == digest(q) => {
            if rev.parse::<u64>().ok() != Some(revision) {
                return Err(stale());
            }
            offset.parse().map_err(|_| stale())
        }
        _ => Err(stale()),
    }
}

/// A resumed offset that lies within `total` items, or `stale_cursor`.
fn within(offset: u64, total: u64) -> Result<u64, RpcError> {
    if offset <= total && offset <= i64::MAX as u64 {
        Ok(offset)
    } else {
        Err(RpcError::new(
            "stale_cursor",
            "the usage page cursor is out of range",
        ))
    }
}

/// Fills a page up to `max` rows and the page budget. A row that alone
/// exceeds the budget is `too_large`.
fn fill<T: Serialize>(
    rows: impl IntoIterator<Item = Result<T, RpcError>>,
    max: usize,
) -> Result<(Vec<T>, bool), RpcError> {
    // Room for the envelope, query and cursor around the rows.
    let budget = MAX_PAGE_BYTES - 16 * 1024;
    let mut out = Vec::new();
    let mut used = 0usize;
    for row in rows {
        let row = row?;
        if out.len() == max {
            return Ok((out, true));
        }
        let size = serde_json::to_vec(&row)
            .map_err(|e| RpcError::internal(e.to_string()))?
            .len()
            + 1;
        if size > budget {
            return Err(RpcError::too_large("one usage row exceeds the page budget"));
        }
        if used + size > budget {
            return Ok((out, true));
        }
        used += size;
        out.push(row);
    }
    Ok((out, false))
}

/// `usage_series`: totals per bucket over the range.
pub fn series(ledger: &Ledger, query: &UsageQuery, ctx: &Context) -> Result<UsageSeries, RpcError> {
    let (q, run, book) = canonical(ledger, query, MAX_BUCKETS as u16, true)?;
    let revision = ledger.revision().map_err(internal)?;
    let offset = resume("series", query.page.cursor.as_ref(), revision, &q)?;
    let x = extras(ledger, &run, ctx)?;
    let bucket = q.bucket_seconds.expect("series resolve a bucket") as i64 * 1_000;
    let start = q
        .from_ms
        .map(|t| t.get())
        .unwrap_or_else(|| floor(run.started_at.unwrap_or(run.tracking_since), HOUR_MS));
    let end = q
        .to_ms
        .map(|t| t.get())
        .unwrap_or_else(|| start + ceil(ctx.now - start, bucket));
    let count = ((end - start).max(0) + bucket - 1) / bucket;
    let offset = within(offset, count as u64)?;
    let limit = q.page.limit.unwrap_or(MAX_BUCKETS as u16) as u64;
    // Bounded: offset <= count, and every bucket edge lies in [start, end].
    let first = start + offset as i64 * bucket;
    let last = start
        .saturating_add(((offset + limit).min(count as u64) as i64).saturating_mul(bucket))
        .min(end);
    // Buckets finer than an hour, or not on the hour grid, need detail.
    let fine = bucket < HOUR_MS || start.rem_euclid(HOUR_MS) != 0;
    let window = Window::load(ledger, &q, Some((first, last)), fine)?;
    let buckets = (offset..(offset + limit).min(count as u64)).map(|i| {
        let from = start + i as i64 * bucket;
        let to = (from + bucket).min(end);
        let (rows, totals, _) = window_totals(&window, Some((from, to)), |_| true, &book, ctx, &x);
        let facts: Vec<Fact> = rows.iter().map(|(_, f)| f.clone()).collect();
        Ok(UsageBucket {
            from_ms: timestamp(from)?,
            to_ms: timestamp(to)?,
            category_split_complete: category_split_complete(&facts),
            totals,
        })
    });
    let (rows, more) = fill(buckets, limit as usize)?;
    let next = offset + rows.len() as u64;
    Ok(UsagePage {
        revision: Wide(revision as u128),
        next_cursor: (more || next < count as u64).then(|| cursor("series", revision, next, &q)),
        rows: List::new(rows).map_err(|_| RpcError::too_large("too many usage buckets"))?,
        query: q,
    })
}

fn group_key(dimension: UsageDimension, a: &Aggregate) -> UsageGroupKey {
    let label = |s: &str| Text::new(s.chars().take(512).collect::<String>()).expect("label");
    match dimension {
        UsageDimension::Agent => UsageGroupKey::Agent {
            id: a.agent_id.clone(),
            incarnation: Id::new(a.incarnation.clone()).expect("incarnation ids are short"),
        },
        UsageDimension::Task => match work_of(a) {
            WorkAttribution::Task { id, .. } => UsageGroupKey::Task { id },
            WorkAttribution::NoTask { .. } => UsageGroupKey::NoTask,
            WorkAttribution::Unknown { .. } => UsageGroupKey::NotAttributed,
        },
        UsageDimension::Epic => match work_of(a) {
            WorkAttribution::Task {
                epic: EpicAttribution::Epic { id },
                ..
            } => UsageGroupKey::Epic { id },
            WorkAttribution::Task {
                epic: EpicAttribution::NoEpic,
                ..
            } => UsageGroupKey::NoEpic,
            WorkAttribution::NoTask { .. } => UsageGroupKey::NoTask,
            _ => UsageGroupKey::NotAttributed,
        },
        UsageDimension::Model => {
            if a.model.is_empty() {
                UsageGroupKey::NotAttributed
            } else {
                UsageGroupKey::Model {
                    model: label(&a.model),
                    provider: None,
                }
            }
        }
        UsageDimension::Account => match account_of(a) {
            Some(id) => UsageGroupKey::Account {
                id: Id::new(id).expect("account ids are short"),
            },
            None => UsageGroupKey::NotAttributed,
        },
        UsageDimension::Runtime => {
            let runtime = a.stream_id.split(':').next().unwrap_or_default();
            match serde_json::from_value::<RuntimeKind>(serde_json::Value::String(runtime.into())) {
                Ok(runtime) => UsageGroupKey::Runtime { runtime },
                Err(_) => UsageGroupKey::NotAttributed,
            }
        }
    }
}

/// `usage_breakdown`: totals per group of one dimension, ordered by key.
pub fn breakdown(
    ledger: &Ledger,
    query: &UsageQuery,
    ctx: &Context,
) -> Result<UsageBreakdown, RpcError> {
    let (q, run, book) = canonical(ledger, query, MAX_METADATA_ROWS as u16, false)?;
    let dimension = q
        .group_by
        .ok_or_else(|| invalid("a breakdown needs group_by"))?;
    let revision = ledger.revision().map_err(internal)?;
    let offset = resume("breakdown", query.page.cursor.as_ref(), revision, &q)?;
    let x = extras(ledger, &run, ctx)?;
    let range = range_of(&q);
    let window = Window::load(ledger, &q, range, false)?;
    let (rows, expired) = window.cut(range);
    let mut groups: BTreeMap<String, (UsageGroupKey, Vec<(Aggregate, Fact)>)> = BTreeMap::new();
    for (a, f) in rows {
        let key = group_key(dimension, &a);
        let text = serde_json::to_string(&key).expect("group keys serialize");
        groups
            .entry(text)
            .or_insert_with(|| (key, Vec::new()))
            .1
            .push((a, f));
    }
    let total = groups.len() as u64;
    let offset = within(offset, total)?;
    let limit = q.page.limit.unwrap_or(MAX_METADATA_ROWS as u16) as usize;
    let page = groups
        .into_values()
        .skip(offset as usize)
        .map(|(key, rows)| {
            let turns = window.turns(range, |a| group_key(dimension, a) == key);
            let mut activities: BTreeSet<String> = BTreeSet::new();
            let mut reasons: BTreeMap<String, (CoverageReason, u128)> = BTreeMap::new();
            for (a, f) in &rows {
                match work_of(a) {
                    WorkAttribution::NoTask { activity } => {
                        activities.insert(serde_json::to_string(&activity).expect("activity"));
                    }
                    WorkAttribution::Unknown { reason } => {
                        let n = &mut reasons.entry(reason_text(reason)).or_insert((reason, 0)).1;
                        *n = count(*n, f.weight);
                    }
                    WorkAttribution::Task { .. } => {}
                }
            }
            Ok(UsageBreakdownRow {
                key,
                totals: {
                    let mut t = totals_of(&rows, turns, &book, ctx, &x);
                    if expired {
                        poison(&mut t, CoverageReason::RetentionLimit);
                    }
                    t
                },
                activities: List::new(
                    activities
                        .iter()
                        .filter_map(|a| serde_json::from_str::<ActivityKind>(a).ok())
                        .take(8)
                        .collect(),
                )
                .expect("activities fit"),
                reasons: List::new(
                    reasons
                        .into_values()
                        .take(32)
                        .map(|(reason, n)| CoverageExclusion {
                            reason,
                            observations: Wide(n),
                        })
                        .collect(),
                )
                .expect("reasons truncated to their limit"),
            })
        });
    let (rows, more) = fill(page, limit)?;
    let next = offset + rows.len() as u64;
    Ok(UsagePage {
        revision: Wide(revision as u128),
        next_cursor: (more || next < total).then(|| cursor("breakdown", revision, next, &q)),
        rows: List::new(rows).map_err(|_| RpcError::too_large("too many usage groups"))?,
        query: q,
    })
}

fn wide(v: Option<u64>) -> Option<Wide> {
    v.map(|v| Wide(v as u128))
}

fn counters(n: &Normalized) -> UsageCounters {
    let c = &n.counters;
    UsageCounters {
        input_total: wide(c.input_total.filter(|_| n.input_valid)),
        input_uncached: wide(c.input_uncached.filter(|_| n.input_valid)),
        cache_read: wide(c.cache_read.filter(|_| n.input_valid)),
        cache_write: wide(c.cache_write.filter(|_| n.input_valid)),
        cache_write_5m: wide(c.cache_write_5m.filter(|_| n.input_valid)),
        cache_write_1h: wide(c.cache_write_1h.filter(|_| n.input_valid)),
        output: wide(c.output.filter(|_| n.output_valid)),
        total_tokens: wide(
            c.input_total
                .zip(c.output)
                .filter(|_| n.input_valid && n.output_valid)
                .and_then(|(i, o)| i.checked_add(o)),
        ),
    }
}

fn observation(
    run_id: &str,
    kind: UsageKind,
    row: ObservationRow,
) -> Result<UsageObservation, RpcError> {
    let id = |s: String| Id::new(s).map_err(|_| RpcError::internal("usage id too long"));
    let a = &row.aggregate;
    let reasons: Vec<CoverageReason> = serde_json::from_str(&row.reasons).unwrap_or_default();
    let unit: ObservationUnit = serde_json::from_value(serde_json::Value::String(a.unit.clone()))
        .map_err(|e| RpcError::internal(e.to_string()))?;
    let account = serde_json::from_str(&a.account).unwrap_or(AccountAttribution::Unknown {
        reason: CoverageReason::UnknownAccount,
    });
    let runtime = serde_json::from_value(serde_json::Value::String(row.runtime.clone()))
        .map_err(|e| RpcError::internal(e.to_string()))?;
    let label = |s: &str| Text::<512>::new(s.chars().take(512).collect::<String>()).ok();
    Ok(UsageObservation {
        id: id(format!("{}/{}", a.stream_id, row.key))?,
        revision: Wide(row.revision as u128),
        unit,
        binding: UsageBinding {
            workspace_run_id: id(run_id.to_string())?,
            source_id: id(a.stream_id.clone())?,
            kind,
            agent_id: a.agent_id.clone(),
            incarnation: id(a.incarnation.clone())?,
            launch_id: id(row.launch_id.clone())?,
            native_session_id: Id::new(row.native_session_id.clone()).ok(),
            native_turn_id: row.turn.clone().and_then(|t| Id::new(t).ok()),
            aside_execution_id: (!a.aside_execution.is_empty())
                .then(|| Id::new(a.aside_execution.clone()).ok())
                .flatten(),
            runtime,
            cli_version: row.cli_version.as_deref().and_then(label),
            model: (!a.model.is_empty()).then(|| label(&a.model)).flatten(),
            provider: None,
            work: work_of(a),
            account,
            board_revision: wide(row.board_revision),
            event_seq: wide(row.event_seq),
        },
        time_basis: if row.native_ms.is_some() {
            TimeBasis::Native
        } else {
            TimeBasis::Unknown
        },
        native_time: row.native_ms.and_then(|t| Timestamp::new(t).ok()),
        observed_at: timestamp(row.observed_at)?,
        counters: counters(&row.normalized),
        validity: List::new(reasons.into_iter().take(32).collect()).expect("reasons fit"),
        diagnostic_codes: List::default(),
        event_refs: List::default(),
    })
}

/// `usage_observations`: retained detail rows of the scope and range.
pub fn observations(ledger: &Ledger, query: &UsageQuery) -> Result<UsageObservations, RpcError> {
    let (q, run, _) = canonical(ledger, query, MAX_METADATA_ROWS as u16, false)?;
    let revision = ledger.revision().map_err(internal)?;
    let offset = within(
        resume("observations", query.page.cursor.as_ref(), revision, &q)?,
        i64::MAX as u64,
    )?;
    let limit = q.page.limit.unwrap_or(MAX_METADATA_ROWS as u16) as u64;
    let rows = ledger
        .observation_rows(
            &run.run_id,
            scope_name(q.scope.kind),
            range_of(&q),
            |a| keep(&q.scope, a),
            offset,
            limit + 1,
        )
        .map_err(internal)?;
    let have_more = rows.len() as u64 > limit;
    let page = rows
        .into_iter()
        .take(limit as usize)
        .map(|row| observation(&run.run_id, q.scope.kind, row));
    let (rows, more) = fill(page, limit as usize)?;
    let next = offset + rows.len() as u64;
    Ok(UsagePage {
        revision: Wide(revision as u128),
        next_cursor: (more || have_more).then(|| cursor("observations", revision, next, &q)),
        rows: List::new(rows).map_err(|_| RpcError::too_large("too many observations"))?,
        query: q,
    })
}

/// `usage_runs`: the runs the ledger tracks, most recent first.
pub fn runs(ledger: &Ledger, page: &PageRequest, ctx: &Context) -> Result<UsageRuns, RpcError> {
    page.validate(MAX_METADATA_ROWS as u16).map_err(invalid)?;
    let limit = page.limit.unwrap_or(MAX_METADATA_ROWS as u16) as u64;
    let offset = match &page.cursor {
        None => 0,
        Some(c) => c
            .as_str()
            .strip_prefix("runs.")
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| {
                RpcError::new("stale_cursor", "the usage run cursor no longer applies")
            })?,
    };
    let offset = within(offset, i64::MAX as u64)?;
    let found = ledger.runs(offset, limit + 1).map_err(internal)?;
    let more = found.len() as u64 > limit;
    let mut rows = Vec::new();
    for run in found.into_iter().take(limit as usize) {
        let x = extras(ledger, &run, ctx)?;
        let mut coverage = MetricCoverage::default();
        decorate(&mut coverage, &x);
        rows.push(UsageRun {
            workspace_run_id: Id::new(run.run_id.clone()).expect("run ids are short"),
            started_at: run.started_at.and_then(|t| Timestamp::new(t).ok()),
            tracking_since: Timestamp::new(run.tracking_since).ok(),
            ended_at: run.ended_at.and_then(|t| Timestamp::new(t).ok()),
            state: tracking_state(ledger, &run, &x)?,
            coverage,
        });
    }
    let next = offset + rows.len() as u64;
    Ok(UsageRuns {
        rows: List::new(rows).expect("rows within the limit"),
        next_cursor: more.then(|| Id::new(format!("runs.{next}")).expect("short")),
    })
}

/// `usage_prices`: a price book in its wire form.
pub fn prices(req: &UsagePrices) -> Result<PriceBook, RpcError> {
    Ok(book(req.price_book_id.as_ref())?.to_wire())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_metrics::claude::tests::assistant;
    use crate::usage_metrics::collect::drain;
    use crate::usage_metrics::ledger::{Locator, Registration, Retention, WorkBinding};
    use crate::usage_metrics::source::Start;
    use crew_core::rpc::usage_metrics::{EstimateState, UsageEpicFilter, UsageTaskFilter};
    use serde_json::{json, Value};
    use std::fs::File;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    const NOW: i64 = 1_791_417_600_000;
    const DAY: i64 = 86_400_000;

    fn msg(id: &str, ms: i64, o: u64) -> Value {
        assistant(
            id,
            ms,
            json!({"input_tokens": 10, "cache_read_input_tokens": 80, "cache_creation_input_tokens": 10, "output_tokens": o}),
        )
    }

    fn append(path: &Path, values: &[Value]) {
        let text: String = values.iter().map(|v| format!("{v}\n")).collect();
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }

    fn bind(work: WorkAttribution) -> WorkBinding {
        WorkBinding {
            work,
            account: AccountAttribution::Unknown {
                reason: CoverageReason::UnknownAccount,
            },
            board_revision: None,
            event_seq: None,
        }
    }

    fn task(id: &str, epic: Option<&str>) -> WorkAttribution {
        WorkAttribution::Task {
            id: id.into(),
            epic: epic.map_or(EpicAttribution::NoEpic, |e| EpicAttribution::Epic {
                id: e.into(),
            }),
        }
    }

    /// A run with one Claude member: unattributed, t1 (epic e1), keepalive
    /// (no task) and t2 (no epic) usage over three hours.
    fn fixture() -> (tempfile::TempDir, PathBuf, Ledger, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s-1.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut ledger = Ledger::in_memory().unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let reg = Registration {
            run_id: "r1".into(),
            kind: UsageKind::Live,
            runtime: RuntimeKind::Claude,
            native_session_id: "s-1".into(),
            agent_id: "worker".into(),
            incarnation: "i1".into(),
            launch_id: "l1".into(),
            aside: None,
            locator: Locator::Private {
                agent_id: "worker".into(),
            },
            started_ms: NOW,
            start: Start::Fresh,
            cli_version: Some("2.1.291".into()),
        };
        let stream = ledger
            .register(&reg, &File::open(&path).unwrap(), NOW)
            .unwrap()
            .unwrap();
        let step = |ledger: &mut Ledger, work: Option<WorkAttribution>, values: &[Value]| {
            if let Some(work) = work {
                ledger
                    .bind_work(&stream, &File::open(&path).unwrap(), &bind(work), NOW)
                    .unwrap();
            }
            append(&path, values);
            drain(ledger, &stream, &File::open(&path).unwrap(), NOW).unwrap();
        };
        step(&mut ledger, None, &[msg("u", NOW + 1_000, 1)]);
        step(
            &mut ledger,
            Some(task("t1", Some("e1"))),
            &[msg("a", NOW + 2_000, 2), msg("b", NOW + 3_600_000 + 5, 3)],
        );
        step(
            &mut ledger,
            Some(WorkAttribution::NoTask {
                activity: ActivityKind::Keepalive,
            }),
            &[msg("k", NOW + 3_600_000 + 10, 4)],
        );
        step(
            &mut ledger,
            Some(task("t2", None)),
            &[msg("c", NOW + 2 * 3_600_000 + 7, 5)],
        );
        (dir, path, ledger, stream)
    }

    fn query(task: UsageTaskFilter, epic: UsageEpicFilter) -> UsageQuery {
        UsageQuery {
            scope: UsageScope {
                workspace_run_id: Id::new("r1").unwrap(),
                kind: UsageKind::Live,
                agent_incarnation: None,
                task,
                epic,
                model: None,
                account_id: None,
            },
            from_ms: None,
            to_ms: None,
            group_by: None,
            bucket_seconds: None,
            price_book_id: None,
            page: PageRequest::default(),
        }
    }

    fn all() -> UsageQuery {
        query(UsageTaskFilter::All, UsageEpicFilter::All)
    }

    fn ctx() -> Context {
        Context {
            now: NOW + 3 * 3_600_000,
            expected_agents: Some(1),
            backlog_files: Some(0),
        }
    }

    fn output(t: &UsageTotals) -> Option<u128> {
        t.output.known_sum.map(|w| w.0)
    }

    #[test]
    fn summaries_resolve_a_canonical_query_and_keep_attribution_apart() {
        let (_dir, _path, ledger, _) = fixture();
        let mut q = all();
        q.from_ms = Some(Timestamp::new(NOW + 5).unwrap());
        q.to_ms = Some(Timestamp::new(NOW + 3 * 3_600_000 - 5).unwrap());
        let s = summary(&ledger, &q, &ctx()).unwrap();
        assert_eq!(s.query.from_ms, q.from_ms, "the exact range");
        assert_eq!(s.query.to_ms, q.to_ms);
        assert_eq!(
            s.query.price_book_id.as_ref().map(|i| i.as_str()),
            Some("standard-2026-10-08")
        );
        assert_eq!(output(&s.totals), Some(15));
        assert_eq!(s.totals.requests_observed.known_sum, Some(Wide(5)));
        assert_eq!(
            s.totals.turns_observed.known_sum,
            Some(Wide(1)),
            "one native turn so far"
        );
        assert_eq!(s.tracking_state, TrackingState::Tracking);
        assert_eq!(s.attribution_coverage.eligible_observations, Wide(4));
        assert_eq!(s.attribution_coverage.observations_seen, Wide(5));
        assert!(s.completion_cohort.is_some());
        assert!(s.executions_observed.is_none());

        let by = |task, epic| output(&summary(&ledger, &query(task, epic), &ctx()).unwrap().totals);
        assert_eq!(
            by(UsageTaskFilter::Id("t1".into()), UsageEpicFilter::All),
            Some(5)
        );
        assert_eq!(by(UsageTaskFilter::NoTask, UsageEpicFilter::All), Some(4));
        assert_eq!(
            by(UsageTaskFilter::NotAttributed, UsageEpicFilter::All),
            Some(1)
        );
        assert_eq!(
            by(UsageTaskFilter::All, UsageEpicFilter::Id("e1".into())),
            Some(5)
        );
        assert_eq!(
            by(UsageTaskFilter::All, UsageEpicFilter::NoEpic),
            Some(5),
            "t2 has no epic"
        );
        assert_eq!(by(UsageTaskFilter::All, UsageEpicFilter::NoTask), Some(4));
        assert_eq!(
            by(UsageTaskFilter::All, UsageEpicFilter::NotAttributed),
            Some(1)
        );
        let none = summary(
            &ledger,
            &query(UsageTaskFilter::Id("zz".into()), UsageEpicFilter::All),
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            none.totals.output.known_sum, None,
            "no rows is unavailable, not zero"
        );
        assert_eq!(none.totals.estimate.state, EstimateState::Unavailable);

        // The wire form round-trips exactly.
        let wire = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<UsageSummary>(&wire).unwrap(), s);

        let mut unknown = all();
        unknown.scope.workspace_run_id = Id::new("r9").unwrap();
        assert_eq!(
            summary(&ledger, &unknown, &ctx()).unwrap_err().code,
            "not_found"
        );
        let mut bad = all();
        bad.price_book_id = Some(Id::new("old").unwrap());
        assert_eq!(
            summary(&ledger, &bad, &ctx()).unwrap_err().code,
            "not_found"
        );
    }

    #[test]
    fn series_buckets_add_up_and_fine_buckets_need_retained_detail() {
        let (_dir, _path, mut ledger, _) = fixture();
        let mut q = all();
        q.from_ms = Some(Timestamp::new(NOW).unwrap());
        q.to_ms = Some(Timestamp::new(NOW + 3 * 3_600_000).unwrap());
        let hourly = series(&ledger, &q, &ctx()).unwrap();
        assert_eq!(hourly.query.bucket_seconds, Some(3_600));
        let outputs: Vec<_> = hourly.rows.iter().map(|b| output(&b.totals)).collect();
        assert_eq!(outputs, [Some(3), Some(7), Some(5)]);
        assert!(hourly.next_cursor.is_none());

        q.bucket_seconds = Some(250);
        let fine = series(&ledger, &q, &ctx()).unwrap();
        assert_eq!(
            fine.query.bucket_seconds,
            Some(300),
            "a bucket nesting in hours"
        );
        assert_eq!(fine.rows.len(), 36);
        let fine_total: u128 = fine.rows.iter().filter_map(|b| output(&b.totals)).sum();
        assert_eq!(fine_total, 15);

        // Once detail expires, fine buckets say so; hourly ones still answer.
        ledger.expire(NOW + 40 * DAY, Retention::default()).unwrap();
        let fine = series(&ledger, &q, &ctx()).unwrap();
        let floor = NOW + 2 * 3_600_000 + 8;
        for b in fine.rows.iter() {
            let expired = b.totals.output.reason == Some(CoverageReason::RetentionLimit);
            assert_eq!(expired, b.from_ms.get() < floor, "{:?}", b.from_ms);
            assert_eq!(b.totals.output.known_sum, None);
        }
        q.bucket_seconds = None;
        let hourly = series(&ledger, &q, &ctx()).unwrap();
        let outputs: Vec<_> = hourly.rows.iter().map(|b| output(&b.totals)).collect();
        assert_eq!(outputs, [Some(3), Some(7), Some(5)]);
        assert!(hourly.rows[0]
            .totals
            .output
            .coverage
            .retained_from
            .is_some());
    }

    #[test]
    fn breakdowns_group_by_attribution_and_name_activities() {
        let (_dir, _path, ledger, _) = fixture();
        let mut q = all();
        q.group_by = Some(UsageDimension::Task);
        let b = breakdown(&ledger, &q, &ctx()).unwrap();
        let rows: BTreeMap<String, (Option<u128>, Vec<ActivityKind>)> = b
            .rows
            .iter()
            .map(|r| {
                (
                    serde_json::to_string(&r.key).unwrap(),
                    (output(&r.totals), r.activities.to_vec()),
                )
            })
            .collect();
        assert_eq!(rows[r#"{"kind":"task","id":"t1"}"#].0, Some(5));
        assert_eq!(
            rows[r#"{"kind":"no_task"}"#],
            (Some(4), vec![ActivityKind::Keepalive])
        );
        assert_eq!(rows[r#"{"kind":"not_attributed"}"#].0, Some(1));
        q.group_by = Some(UsageDimension::Epic);
        let b = breakdown(&ledger, &q, &ctx()).unwrap();
        let keys: Vec<String> = b
            .rows
            .iter()
            .map(|r| serde_json::to_string(&r.key).unwrap())
            .collect();
        assert!(keys.contains(&r#"{"kind":"no_epic"}"#.to_string()));
        assert!(keys.contains(&r#"{"kind":"epic","id":"e1"}"#.to_string()));
        q.group_by = Some(UsageDimension::Runtime);
        let b = breakdown(&ledger, &q, &ctx()).unwrap();
        assert_eq!(b.rows.len(), 1);
        assert_eq!(
            b.rows[0].key,
            UsageGroupKey::Runtime {
                runtime: RuntimeKind::Claude
            }
        );
        q.group_by = None;
        assert_eq!(
            breakdown(&ledger, &q, &ctx()).unwrap_err().code,
            crew_core::rpc::features::INVALID_FEATURE_REQUEST
        );
    }

    #[test]
    fn observation_pages_resume_and_cursors_go_stale() {
        let (_dir, path, mut ledger, stream) = fixture();
        let mut q = all();
        q.page.limit = Some(2);
        let first = observations(&ledger, &q).unwrap();
        assert_eq!(first.rows.len(), 2);
        let rec = &first.rows[0];
        assert_eq!(rec.binding.runtime, RuntimeKind::Claude);
        assert_eq!(rec.binding.cli_version.as_deref(), Some("2.1.291"));
        assert_eq!(rec.time_basis, TimeBasis::Native);
        assert_eq!(rec.counters.input_total, Some(Wide(100)));
        let mut next = q.clone();
        next.page.cursor = first.next_cursor.clone();
        let second = observations(&ledger, &next).unwrap();
        assert_eq!(second.rows.len(), 2);
        assert_eq!(
            second.query.page.cursor, first.next_cursor,
            "the request cursor is echoed"
        );
        assert_eq!(second.query.page.limit, Some(2));
        assert_eq!(first.query.page.cursor, None);
        assert_ne!(second.rows[0].id, first.rows[0].id);
        let mut other = next.clone();
        other.scope.task = UsageTaskFilter::NoTask;
        assert_eq!(
            observations(&ledger, &other).unwrap_err().code,
            "stale_cursor"
        );
        // New usage moves the revision: the old cursor is stale.
        append(&path, &[msg("z", NOW + 9, 1)]);
        drain(&mut ledger, &stream, &File::open(&path).unwrap(), NOW).unwrap();
        assert_eq!(
            observations(&ledger, &next).unwrap_err().code,
            "stale_cursor"
        );
    }

    #[test]
    fn a_late_correction_after_expiry_makes_affected_totals_unavailable() {
        let (_dir, path, mut ledger, stream) = fixture();
        ledger.expire(NOW + 40 * DAY, Retention::default()).unwrap();
        append(&path, &[msg("a", NOW + 2_000, 99)]);
        drain(&mut ledger, &stream, &File::open(&path).unwrap(), NOW).unwrap();
        let s = summary(&ledger, &all(), &ctx()).unwrap();
        assert_eq!(s.totals.output.state, MetricState::Unavailable);
        assert_eq!(
            s.totals.output.reason,
            Some(CoverageReason::CorrectionOutsideHorizon)
        );
        assert_eq!(s.totals.estimate.lower_pico_usd, None);
        assert!(s
            .limitations
            .contains(&CoverageReason::CorrectionOutsideHorizon));
        // Hours the correction does not touch still answer.
        let mut later = all();
        later.from_ms = Some(Timestamp::new(NOW + 3_600_000).unwrap());
        let s = summary(&ledger, &later, &ctx()).unwrap();
        assert_eq!(output(&s.totals), Some(12));
    }

    #[test]
    fn pages_stop_at_the_byte_budget_with_a_cursor() {
        let (_dir, _path, ledger, _) = fixture();
        let mut q = all();
        q.from_ms = Some(Timestamp::new(NOW).unwrap());
        q.to_ms = Some(Timestamp::new(NOW + 30 * DAY).unwrap());
        let page = series(&ledger, &q, &ctx()).unwrap();
        let size = serde_json::to_vec(&page).unwrap().len();
        assert!(size < MAX_PAGE_BYTES, "{size}");
        assert!(
            page.rows.len() < MAX_BUCKETS,
            "the budget, not the count, stopped it"
        );
        assert!(page.next_cursor.is_some());
        let mut next = q.clone();
        next.page.cursor = page.next_cursor.clone();
        let more = series(&ledger, &next, &ctx()).unwrap();
        assert_eq!(
            more.rows[0].from_ms.get(),
            NOW + page.rows.len() as i64 * 3_600_000
        );
    }

    #[test]
    fn runs_and_prices() {
        let (_dir, _path, ledger, _) = fixture();
        let r = runs(&ledger, &PageRequest::default(), &ctx()).unwrap();
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].state, TrackingState::Tracking);
        let book = prices(&UsagePrices {
            price_book_id: None,
        })
        .unwrap();
        assert_eq!(book.id.as_str(), "standard-2026-10-08");
        assert_eq!(
            prices(&UsagePrices {
                price_book_id: Some(Id::new("x").unwrap())
            })
            .unwrap_err()
            .code,
            "not_found"
        );
    }

    #[test]
    fn partial_hours_use_detail_and_unavailable_once_it_expires() {
        let (_dir, _path, mut ledger, _) = fixture();
        let at = |from: i64, to: i64| {
            let mut q = all();
            q.from_ms = Some(Timestamp::new(from).unwrap());
            q.to_ms = Some(Timestamp::new(to).unwrap());
            summary(&ledger, &q, &ctx()).unwrap()
        };
        // [NOW+1.5s, NOW+1h+7ms) holds a (2) and b (3), not u (1) or k (4).
        let s = at(NOW + 1_500, NOW + 3_600_000 + 7);
        assert_eq!(output(&s.totals), Some(5));
        assert_eq!(s.totals.requests_observed.known_sum, Some(Wide(2)));
        // Within one hour.
        assert_eq!(output(&at(NOW + 1_500, NOW + 2_500).totals), Some(2));
        // Whole hours alone need no detail.
        assert_eq!(output(&at(NOW, NOW + 2 * 3_600_000).totals), Some(10));
        assert_eq!(partial_parts((NOW, NOW + 2 * 3_600_000)), vec![]);
        assert_eq!(
            partial_parts((NOW + 5, NOW + 3 * 3_600_000 - 5)),
            vec![
                (NOW + 5, NOW + 3_600_000),
                (NOW + 2 * 3_600_000, NOW + 3 * 3_600_000 - 5)
            ]
        );
        assert_eq!(partial_parts((NOW + 5, NOW + 9)), vec![(NOW + 5, NOW + 9)]);
        assert_eq!(partial_parts((i64::MIN, i64::MAX)), vec![]);

        ledger.expire(NOW + 40 * DAY, Retention::default()).unwrap();
        let at = |from: i64, to: i64| {
            let mut q = all();
            q.from_ms = Some(Timestamp::new(from).unwrap());
            q.to_ms = Some(Timestamp::new(to).unwrap());
            summary(&ledger, &q, &ctx()).unwrap()
        };
        let s = at(NOW + 1_500, NOW + 3_600_000 + 7);
        assert_eq!(
            s.totals.output.known_sum, None,
            "never widened or truncated"
        );
        assert_eq!(s.totals.output.reason, Some(CoverageReason::RetentionLimit));
        assert_eq!(output(&at(NOW, NOW + 2 * 3_600_000).totals), Some(10));
    }

    #[test]
    fn every_metric_uses_the_requested_cohort() {
        let (_dir, path, mut ledger, stream) = fixture();
        // A second model in the same turn as c.
        let mut other = msg("d", NOW + 2 * 3_600_000 + 9, 6);
        other["message"]["model"] = json!("claude-opus-5-5");
        append(&path, &[other]);
        drain(&mut ledger, &stream, &File::open(&path).unwrap(), NOW).unwrap();
        let mut q = all();
        q.group_by = Some(UsageDimension::Model);
        let b = breakdown(&ledger, &q, &ctx()).unwrap();
        for row in b.rows.iter() {
            let turns = row.totals.turns_observed.known_sum;
            match &row.key {
                UsageGroupKey::Model { model, .. } if &**model == "claude-opus-5-5" => {
                    assert_eq!(turns, Some(Wide(1)), "only the turn it was in")
                }
                _ => assert_eq!(turns, Some(Wide(1))),
            }
        }
        // A model selection has no completed-task cohort.
        let mut opus = all();
        opus.scope.model = Some(Text::new("claude-opus-5-5".to_string()).unwrap());
        let s = summary(&ledger, &opus, &ctx()).unwrap();
        assert!(s.completion_cohort.is_none());
        assert_eq!(output(&s.totals), Some(6));
        assert_eq!(s.totals.turns_observed.known_sum, Some(Wide(1)));
    }

    #[test]
    fn series_buckets_resolve_by_the_shared_rule() {
        let (_dir, _path, ledger, _) = fixture();
        for (asked, resolved) in [
            (None, 3_600),
            (Some(1), 60),
            (Some(250), 300),
            (Some(1_800), 1_800),
            (Some(2_000), 3_600),
            (Some(3_601), 7_200),
            (Some(86_400), 86_400),
        ] {
            let mut q = all();
            q.from_ms = Some(Timestamp::new(NOW).unwrap());
            q.to_ms = Some(Timestamp::new(NOW + 3_600_000).unwrap());
            q.bucket_seconds = asked;
            let s = series(&ledger, &q, &ctx()).unwrap();
            assert_eq!(s.query.bucket_seconds, Some(resolved), "{asked:?}");
        }
        let mut q = all();
        q.bucket_seconds = Some(90_000);
        assert!(
            series(&ledger, &q, &ctx()).is_err(),
            "beyond a day is invalid"
        );
    }

    #[test]
    fn missing_bounds_stay_unbounded_and_bad_offsets_fail() {
        let (_dir, _path, ledger, _) = fixture();
        let mut to_only = all();
        to_only.to_ms = Some(Timestamp::new(NOW + 3_600_000 + 7).unwrap());
        let s = summary(&ledger, &to_only, &ctx()).unwrap();
        assert_eq!(output(&s.totals), Some(6), "u, a and b");
        let mut from_only = all();
        from_only.from_ms = Some(Timestamp::new(NOW + 3_600_000 + 7).unwrap());
        assert_eq!(
            output(&summary(&ledger, &from_only, &ctx()).unwrap().totals),
            Some(9)
        );
        let mut by = to_only.clone();
        by.group_by = Some(UsageDimension::Task);
        assert!(breakdown(&ledger, &by, &ctx()).is_ok());
        assert!(series(&ledger, &to_only, &ctx()).is_ok());
        assert!(observations(&ledger, &to_only).is_ok());
        assert_eq!(floor(i64::MIN, HOUR_MS), i64::MIN);
        assert_eq!(ceil(i64::MAX, HOUR_MS), i64::MAX);

        // A cursor with the right kind, revision and digest but an altered
        // offset fails instead of wrapping.
        let mut q = all();
        q.from_ms = Some(Timestamp::new(NOW).unwrap());
        q.to_ms = Some(Timestamp::new(NOW + 3 * 3_600_000).unwrap());
        let revision = ledger.revision().unwrap();
        for (kind, offset) in [("series", u64::MAX), ("series", 4), ("breakdown", 99)] {
            let mut page = q.clone();
            if kind == "breakdown" {
                page.group_by = Some(UsageDimension::Task);
            }
            let canonical = canonical(&ledger, &page, MAX_BUCKETS as u16, kind == "series")
                .unwrap()
                .0;
            page.page.cursor = Some(cursor(kind, revision, offset, &canonical));
            let err = match kind {
                "series" => series(&ledger, &page, &ctx()).unwrap_err(),
                _ => breakdown(&ledger, &page, &ctx()).unwrap_err(),
            };
            assert_eq!(err.code, "stale_cursor", "{kind} {offset}");
        }
        let mut page = q.clone();
        page.page.cursor =
            Some(Id::new(format!("observations.{revision}.{}.x", u64::MAX)).unwrap());
        assert_eq!(
            observations(&ledger, &page).unwrap_err().code,
            "stale_cursor"
        );
        let bad = PageRequest {
            cursor: Some(Id::new(format!("runs.{}", u64::MAX)).unwrap()),
            limit: None,
        };
        assert_eq!(
            runs(&ledger, &bad, &ctx()).unwrap_err().code,
            "stale_cursor"
        );
    }

    #[test]
    fn refused_and_drained_sources_are_gaps_of_the_run() {
        let (_dir, _path, mut ledger, stream) = fixture();
        ledger
            .record_source_gap("r1", "pi:s-9", CoverageReason::UnsupportedRuntime, NOW)
            .unwrap();
        ledger
            .record_source_gap("r1", "claude:s-8", CoverageReason::NotTracked, NOW)
            .unwrap();
        let registration = ledger.registration(&stream).unwrap().unwrap();
        assert!(ledger.retire(&stream, registration, Some(NOW)).unwrap());
        let s = summary(&ledger, &all(), &ctx()).unwrap();
        let reasons: BTreeSet<_> = s
            .totals
            .output
            .coverage
            .source_gaps
            .iter()
            .map(|g| (g.source_id.as_str().to_string(), reason_text(g.reason)))
            .collect();
        for (source, reason) in [
            ("pi:s-9", "unsupported_runtime"),
            ("claude:s-8", "not_tracked"),
            (stream.as_str(), "source_backlog"),
        ] {
            assert!(
                reasons.contains(&(source.to_string(), reason.to_string())),
                "{reasons:?}"
            );
        }
        assert_eq!(s.tracking_state, TrackingState::Paused);
        assert_eq!(s.totals.output.coverage.source_gaps_total, Some(Wide(3)));
    }

    #[test]
    fn aside_counts_are_unavailable_when_their_range_lost_detail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fork.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut ledger = Ledger::in_memory().unwrap();
        ledger.begin_run("r1", Some(NOW), NOW).unwrap();
        let reg = Registration {
            run_id: "r1".into(),
            kind: UsageKind::Aside,
            runtime: RuntimeKind::Claude,
            native_session_id: "s-1".into(),
            agent_id: "worker".into(),
            incarnation: "i1".into(),
            launch_id: "l1".into(),
            aside: Some(crate::usage_metrics::ledger::AsideIds {
                execution_id: "x1".into(),
                aside_id: "q1".into(),
            }),
            locator: Locator::Private {
                agent_id: "worker".into(),
            },
            started_ms: NOW,
            start: Start::Fresh,
            cli_version: None,
        };
        let stream = ledger
            .register(&reg, &File::open(&path).unwrap(), NOW)
            .unwrap()
            .unwrap();
        append(&path, &[msg("ans", NOW + 1_000, 7)]);
        drain(&mut ledger, &stream, &File::open(&path).unwrap(), NOW).unwrap();
        ledger
            .execution_status(
                "r1",
                "q1",
                "x1",
                crate::usage_metrics::ledger::ExecutionStatus::Answered,
                Some(NOW),
                Some(NOW + 2_000),
            )
            .unwrap();
        let partial_hour = || {
            let mut q = all();
            q.scope.kind = UsageKind::Aside;
            q.from_ms = Some(Timestamp::new(NOW + 500).unwrap());
            q.to_ms = Some(Timestamp::new(NOW + 1_500).unwrap());
            q
        };
        let before = summary(&ledger, &partial_hour(), &ctx()).unwrap();
        assert_eq!(
            before.executions_observed.as_ref().unwrap().known_sum,
            Some(Wide(1))
        );
        assert!(before.execution_statuses.as_ref().unwrap().complete);

        ledger.expire(NOW + 40 * DAY, Retention::default()).unwrap();
        let after = summary(&ledger, &partial_hour(), &ctx()).unwrap();
        let observed = after.executions_observed.unwrap();
        assert_eq!(observed.known_sum, None, "not a complete count of 0");
        assert_eq!(observed.state, MetricState::Unavailable);
        assert_eq!(observed.reason, Some(CoverageReason::RetentionLimit));
        assert!(!after.execution_statuses.unwrap().complete);
        // Whole hours need no detail and still count it.
        let mut whole = partial_hour();
        whole.from_ms = Some(Timestamp::new(NOW).unwrap());
        whole.to_ms = Some(Timestamp::new(NOW + 3_600_000).unwrap());
        let s = summary(&ledger, &whole, &ctx()).unwrap();
        assert_eq!(s.executions_observed.unwrap().known_sum, Some(Wide(1)));
    }
}
