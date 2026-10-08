//! Server-side metric computation over a set of observations.
//!
//! Each metric uses only the observations that report what it needs and
//! states its own coverage: a missing counter is never added as zero, a ratio
//! sums its operands over one eligible set (never a mean of percentages), and
//! an unbounded part of a cost leaves the estimate's upper bound unknown.

use std::collections::BTreeSet;

use crew_core::rpc::features::{Fraction, Id, Label, List, Text, Timestamp, Wide};
use crew_core::rpc::usage_metrics::{
    CostBasis, CostEstimate, CounterMetric, CoverageExclusion, CoverageReason, EstimateState,
    MetricCoverage, MetricState, ObservationUnit, RatioBound, RatioMetric, UsageTotals,
};

use super::counters::{Counters, Normalized};
use super::price::{price, Book, Bounds, Priced, RowCost};

/// One observation as the metrics see it.
#[derive(Clone, Debug)]
pub struct Fact {
    pub unit: ObservationUnit,
    pub normalized: Normalized,
    pub model: Option<String>,
    /// The agent incarnation it belongs to, for reporting-agent counts.
    pub agent: String,
    /// The logical native stream it was read from. Native turn ids are only
    /// unique within their stream.
    pub source: String,
    /// Its correlated native or daemon turn within `source`, when one is known.
    pub turn: Option<String>,
    /// Why an `Unusable` observation could not be used.
    pub unusable: Option<CoverageReason>,
}

/// Counts of seen and eligible observations for one metric, with the reasons
/// the others were left out.
#[derive(Default)]
struct Tally {
    seen: u64,
    eligible: u64,
    exclusions: Vec<(CoverageReason, u64)>,
    agents: BTreeSet<String>,
}

impl Tally {
    fn include(&mut self, fact: &Fact) {
        self.seen += 1;
        self.eligible += 1;
        self.agents.insert(fact.agent.clone());
    }

    fn exclude(&mut self, reason: CoverageReason) {
        self.seen += 1;
        match self.exclusions.iter_mut().find(|(r, _)| *r == reason) {
            Some((_, n)) => *n += 1,
            None => self.exclusions.push((reason, 1)),
        }
    }

    fn state(&self) -> MetricState {
        if self.eligible == 0 {
            MetricState::Unavailable
        } else if self.eligible == self.seen {
            MetricState::Complete
        } else {
            MetricState::Partial
        }
    }

    /// The most frequent reason an observation was left out.
    fn main_reason(&self) -> Option<CoverageReason> {
        self.exclusions
            .iter()
            .max_by_key(|(_, n)| *n)
            .map(|(r, _)| *r)
    }

    fn coverage(&self, common: &Common) -> MetricCoverage {
        let mut exclusions = self.exclusions.clone();
        exclusions.sort_by(|a, b| b.1.cmp(&a.1));
        exclusions.truncate(32);
        MetricCoverage {
            eligible_observations: Wide(self.eligible as u128),
            observations_seen: Wide(self.seen as u128),
            exclusions: List::new(
                exclusions
                    .into_iter()
                    .map(|(reason, n)| CoverageExclusion {
                        reason,
                        observations: Wide(n as u128),
                    })
                    .collect(),
            )
            .expect("exclusions truncated to their limit"),
            known_requests: Wide(common.requests as u128),
            request_cardinality_complete: common.cardinality_complete,
            reporting_agents: Wide(self.agents.len() as u128),
            expected_agents: common.expected_agents.map(|n| Wide(n as u128)),
            source_gaps: List::default(),
            source_gaps_total: None,
            source_backlog_files: None,
            retained_from: None,
        }
    }
}

/// What every metric's coverage repeats.
struct Common {
    requests: u64,
    cardinality_complete: bool,
    expected_agents: Option<u64>,
}

/// Adds `v` to a checked sum; an overflow leaves no sum.
fn accumulate(sum: &mut Option<u128>, v: u128) {
    *sum = sum.and_then(|s| s.checked_add(v));
}

/// A metric whose sum overflowed: unavailable, never a wrapped or clamped total.
fn overflowed(tally: &Tally, common: &Common) -> CounterMetric {
    CounterMetric {
        known_sum: None,
        state: MetricState::Unavailable,
        reason: Some(CoverageReason::InvalidCounter),
        coverage: tally.coverage(common),
    }
}

fn unusable(fact: &Fact) -> Option<CoverageReason> {
    (fact.unit == ObservationUnit::Unusable)
        .then(|| fact.unusable.unwrap_or(CoverageReason::InvalidCounter))
}

fn counter(
    facts: &[Fact],
    common: &Common,
    valid: fn(&Normalized) -> bool,
    get: impl Fn(&Counters) -> Option<u64>,
    empty_reason: CoverageReason,
) -> CounterMetric {
    let mut tally = Tally::default();
    let mut sum = Some(0u128);
    for fact in facts {
        if let Some(reason) = unusable(fact) {
            tally.exclude(reason);
        } else if !valid(&fact.normalized) {
            tally.exclude(CoverageReason::InvalidCounter);
        } else if let Some(v) = get(&fact.normalized.counters) {
            accumulate(&mut sum, v as u128);
            tally.include(fact);
        } else {
            tally.exclude(CoverageReason::NotReported);
        }
    }
    let Some(sum) = sum else {
        return overflowed(&tally, common);
    };
    let state = tally.state();
    CounterMetric {
        known_sum: (tally.eligible > 0).then_some(Wide(sum)),
        state,
        reason: match state {
            MetricState::Complete => None,
            _ if tally.seen == 0 => Some(empty_reason),
            _ => tally.main_reason(),
        },
        coverage: tally.coverage(common),
    }
}

fn fraction(num: u128, den: u128) -> Option<Fraction> {
    if den == 0 {
        return None;
    }
    Fraction::new((num as f64 / den as f64).clamp(0.0, 1.0)).ok()
}

fn bound(num: u128, den: u128) -> Option<RatioBound> {
    Some(RatioBound {
        numerator: Wide(num),
        denominator: Wide(den),
        value: fraction(num, den)?,
    })
}

fn input_valid(n: &Normalized) -> bool {
    n.input_valid
}

fn output_valid(n: &Normalized) -> bool {
    n.output_valid
}

fn both_valid(n: &Normalized) -> bool {
    n.input_valid && n.output_valid
}

fn hit_rate(facts: &[Fact], common: &Common) -> RatioMetric {
    let mut tally = Tally::default();
    let (mut read, mut input) = (Some(0u128), Some(0u128));
    for fact in facts {
        let n = &fact.normalized;
        if let Some(reason) = unusable(fact) {
            tally.exclude(reason);
        } else if !n.input_valid {
            tally.exclude(CoverageReason::InvalidCounter);
        } else if let (Some(i), Some(r)) = (n.counters.input_total, n.counters.cache_read) {
            accumulate(&mut read, r as u128);
            accumulate(&mut input, i as u128);
            tally.include(fact);
        } else {
            tally.exclude(CoverageReason::NotReported);
        }
    }
    let (Some(read), Some(input)) = (read, input) else {
        return RatioMetric {
            numerator: None,
            denominator: None,
            value: None,
            lower: None,
            upper: None,
            state: MetricState::Unavailable,
            reason: Some(CoverageReason::InvalidCounter),
            coverage: tally.coverage(common),
        };
    };
    let (state, reason) = if tally.eligible == 0 {
        (
            MetricState::Unavailable,
            Some(CoverageReason::NoCoveredInput),
        )
    } else if input == 0 {
        (MetricState::Unavailable, Some(CoverageReason::ZeroInput))
    } else {
        let state = tally.state();
        (
            state,
            (state != MetricState::Complete)
                .then(|| tally.main_reason())
                .flatten(),
        )
    };
    RatioMetric {
        numerator: (tally.eligible > 0).then_some(Wide(read)),
        denominator: (tally.eligible > 0).then_some(Wide(input)),
        value: fraction(read, input),
        lower: None,
        upper: None,
        state,
        reason,
        coverage: tally.coverage(common),
    }
}

/// Prices every observation once; `None` for an unusable one.
fn costs(facts: &[Fact], book: &Book) -> Vec<Option<Priced>> {
    facts
        .iter()
        .map(|fact| {
            if unusable(fact).is_some() {
                return None;
            }
            let single = fact.unit == ObservationUnit::Request;
            Some(price(book, fact.model.as_deref(), &fact.normalized, single))
        })
        .collect()
}

fn write_share(facts: &[Fact], costs: &[Option<Priced>], common: &Common) -> RatioMetric {
    let mut tally = Tally::default();
    let (mut other_lo, mut other_hi, mut write_lo, mut write_hi) =
        (Some(0u128), Some(0u128), Some(0u128), Some(0u128));
    for (fact, cost) in facts.iter().zip(costs) {
        if let Some(reason) = unusable(fact) {
            tally.exclude(reason);
        } else if !fact.normalized.input_valid {
            tally.exclude(CoverageReason::InvalidCounter);
        } else {
            match cost {
                None | Some(Priced::NoPrice) => tally.exclude(CoverageReason::NoPrice),
                Some(Priced::Overflow) => tally.exclude(CoverageReason::InvalidCounter),
                Some(Priced::Cost(RowCost { share: None, .. })) => {
                    tally.exclude(CoverageReason::NotReported)
                }
                Some(Priced::Cost(RowCost { share: Some(s), .. })) => {
                    accumulate(&mut other_lo, s.other_lo);
                    accumulate(&mut other_hi, s.other_hi);
                    accumulate(&mut write_lo, s.write_lo);
                    accumulate(&mut write_hi, s.write_hi);
                    tally.include(fact);
                }
            }
        }
    }
    let coverage = tally.coverage(common);
    // x / (other + x) grows with x and falls with other, so the extremes pair
    // the cheapest writes with the dearest other input, and the reverse.
    let operands = (|| {
        let lo = (write_lo?, other_hi?.checked_add(write_lo?)?);
        let hi = (write_hi?, other_lo?.checked_add(write_hi?)?);
        Some((lo, hi))
    })();
    if tally.eligible == 0 || operands.is_none() {
        let reason = match operands {
            None => CoverageReason::InvalidCounter,
            Some(_) => tally.main_reason().unwrap_or(CoverageReason::NotReported),
        };
        return RatioMetric {
            numerator: None,
            denominator: None,
            value: None,
            lower: None,
            upper: None,
            state: MetricState::Unavailable,
            reason: Some(reason),
            coverage,
        };
    }
    let ((lo_num, lo_den), (hi_num, hi_den)) = operands.expect("checked above");
    if lo_den == 0 || hi_den == 0 {
        return RatioMetric {
            numerator: Some(Wide(0)),
            denominator: Some(Wide(0)),
            value: None,
            lower: None,
            upper: None,
            state: MetricState::Unavailable,
            reason: Some(CoverageReason::ZeroInput),
            coverage,
        };
    }
    let state = tally.state();
    let reason = (state != MetricState::Complete)
        .then(|| tally.main_reason())
        .flatten();
    if (lo_num, lo_den) == (hi_num, hi_den) {
        RatioMetric {
            numerator: Some(Wide(lo_num)),
            denominator: Some(Wide(lo_den)),
            value: fraction(lo_num, lo_den),
            lower: None,
            upper: None,
            state,
            reason,
            coverage,
        }
    } else {
        RatioMetric {
            numerator: None,
            denominator: None,
            value: None,
            lower: bound(lo_num, lo_den),
            upper: bound(hi_num, hi_den),
            state,
            reason,
            coverage,
        }
    }
}

fn label(text: String) -> Label {
    let mut text = text;
    while text.len() > 512 {
        text.pop();
    }
    Text::new(text).expect("label within its byte limit")
}

fn estimate(
    facts: &[Fact],
    costs: &[Option<Priced>],
    book: &Book,
    common: &Common,
) -> CostEstimate {
    let mut tally = Tally::default();
    let mut total = Bounds { lo: 0, hi: Some(0) };
    let mut overflow = false;
    let mut excluded: BTreeSet<String> = BTreeSet::new();
    let (mut ttl_unknown, mut residual, mut tier_unknown) = (0u128, 0u128, 0u64);
    for (fact, cost) in facts.iter().zip(costs) {
        if let Some(reason) = unusable(fact) {
            tally.exclude(reason);
            total.hi = None;
            continue;
        }
        match cost {
            None | Some(Priced::NoPrice) => {
                tally.exclude(CoverageReason::NoPrice);
                excluded.insert(fact.model.clone().unwrap_or_else(|| "unknown model".into()));
                total.hi = None;
            }
            Some(Priced::Overflow) => {
                tally.exclude(CoverageReason::InvalidCounter);
                overflow = true;
            }
            // Nothing priced was reported: an unknown cost, not an observed zero.
            Some(Priced::Cost(cost)) if !cost.observed => {
                tally.exclude(CoverageReason::NotReported);
                total.hi = None;
            }
            Some(Priced::Cost(cost)) => {
                match total.checked_plus(cost.total) {
                    Some(sum) => total = sum,
                    None => overflow = true,
                }
                match (
                    ttl_unknown.checked_add(cost.ttl_unknown_tokens as u128),
                    residual.checked_add(cost.residual_tokens as u128),
                ) {
                    (Some(t), Some(r)) => (ttl_unknown, residual) = (t, r),
                    _ => overflow = true,
                }
                tier_unknown += cost.tier_unknown as u64;
                tally.include(fact);
            }
        }
    }
    let (lo, hi) = (total.lo, total.hi);
    let mut assumptions = Vec::new();
    if ttl_unknown > 0 {
        assumptions.push(label(format!(
            "{ttl_unknown} cache-write tokens with no TTL reported, priced between the 5-minute and 1-hour rates"
        )));
    }
    if residual > 0 {
        assumptions.push(label(format!(
            "{residual} input tokens not read from cache, with no write count, priced between the ordinary-input and write rates"
        )));
    }
    if tier_unknown > 0 {
        assumptions.push(label(format!(
            "{tier_unknown} observations with unknown total input or batched requests, priced across both context tiers"
        )));
    }
    // An overflowing sum claims no bound at all.
    let (state, reason) = if overflow {
        (
            EstimateState::Unavailable,
            Some(CoverageReason::InvalidCounter),
        )
    } else if tally.eligible == 0 {
        let reason = if tally.seen == 0 {
            CoverageReason::NoEligibleCost
        } else {
            tally
                .main_reason()
                .unwrap_or(CoverageReason::NoEligibleCost)
        };
        (EstimateState::Unavailable, Some(reason))
    } else if hi.is_none() {
        (
            EstimateState::Partial,
            tally.main_reason().or(Some(CoverageReason::NotReported)),
        )
    } else {
        (EstimateState::Finite, None)
    };
    let available = state != EstimateState::Unavailable;
    CostEstimate {
        price_book_id: Some(Id::new(book.id.clone()).expect("price book id is valid")),
        prices_checked_at: Timestamp::new(book.checked_at_ms).ok(),
        basis: CostBasis::StandardApiEquivalent,
        lower_pico_usd: available.then_some(Wide(lo)),
        upper_pico_usd: if available { hi.map(Wide) } else { None },
        state,
        reason,
        assumptions: List::new(assumptions).expect("at most three assumptions"),
        excluded_models: List::new(excluded.into_iter().take(64).map(label).collect())
            .expect("excluded models truncated to their limit"),
        coverage: tally.coverage(common),
    }
}

fn requests(facts: &[Fact], common: &Common) -> CounterMetric {
    let mut tally = Tally::default();
    for fact in facts {
        match fact.unit {
            ObservationUnit::Request => tally.include(fact),
            ObservationUnit::CounterDelta => tally.exclude(CoverageReason::NativeBatch),
            ObservationUnit::Unusable => {
                tally.exclude(fact.unusable.unwrap_or(CoverageReason::InvalidCounter))
            }
        }
    }
    let state = tally.state();
    CounterMetric {
        known_sum: (tally.eligible > 0).then_some(Wide(tally.eligible as u128)),
        state,
        reason: match state {
            MetricState::Complete => None,
            _ if tally.seen == 0 => Some(CoverageReason::NotReported),
            _ => tally.main_reason(),
        },
        coverage: tally.coverage(common),
    }
}

fn turns(facts: &[Fact], common: &Common) -> CounterMetric {
    let mut tally = Tally::default();
    let mut ids: BTreeSet<(&str, &str)> = BTreeSet::new();
    for fact in facts {
        match &fact.turn {
            Some(turn) => {
                ids.insert((&fact.source, turn));
                tally.include(fact);
            }
            None => tally.exclude(CoverageReason::MissingTurnIdentity),
        }
    }
    let state = tally.state();
    CounterMetric {
        known_sum: (tally.eligible > 0).then_some(Wide(ids.len() as u128)),
        state,
        reason: match state {
            MetricState::Complete => None,
            _ if tally.seen == 0 => Some(CoverageReason::NotReported),
            _ => tally.main_reason(),
        },
        coverage: tally.coverage(common),
    }
}

/// Whether every usable observation reports all four input/output categories
/// (ordinary input, cache read, cache write, output), so a stacked category
/// chart of them has no hidden part. An empty set has nothing hidden.
pub fn category_split_complete(facts: &[Fact]) -> bool {
    facts.iter().all(|f| {
        let n = &f.normalized;
        let c = &n.counters;
        f.unit != ObservationUnit::Unusable
            && both_valid(n)
            && c.input_uncached.is_some()
            && c.cache_read.is_some()
            && c.cache_write.is_some()
            && c.output.is_some()
    })
}

/// Every metric of `UsageTotals`, over one set of observations.
pub fn totals(facts: &[Fact], book: &Book, expected_agents: Option<u64>) -> UsageTotals {
    let common = Common {
        requests: facts
            .iter()
            .filter(|f| f.unit == ObservationUnit::Request)
            .count() as u64,
        cardinality_complete: facts.iter().all(|f| f.unit == ObservationUnit::Request),
        expected_agents,
    };
    let costs = costs(facts, book);
    let none = CoverageReason::NotReported;
    let no_input = CoverageReason::NoCoveredInput;
    UsageTotals {
        input_total: counter(facts, &common, input_valid, |c| c.input_total, no_input),
        input_uncached: counter(facts, &common, input_valid, |c| c.input_uncached, no_input),
        cache_read: counter(facts, &common, input_valid, |c| c.cache_read, no_input),
        cache_write: counter(facts, &common, input_valid, |c| c.cache_write, no_input),
        cache_write_5m: counter(facts, &common, input_valid, |c| c.cache_write_5m, no_input),
        cache_write_1h: counter(facts, &common, input_valid, |c| c.cache_write_1h, no_input),
        output: counter(facts, &common, output_valid, |c| c.output, none),
        total_tokens: counter(
            facts,
            &common,
            both_valid,
            |c| {
                c.input_total
                    .zip(c.output)
                    .and_then(|(i, o)| i.checked_add(o))
            },
            none,
        ),
        hit_rate: hit_rate(facts, &common),
        write_share: write_share(facts, &costs, &common),
        estimate: estimate(facts, &costs, book, &common),
        requests_observed: requests(facts, &common),
        turns_observed: turns(facts, &common),
    }
}

#[cfg(test)]
mod tests {
    use super::super::counters;
    use super::*;

    fn fact(agent: &str, model: &str, n: Normalized) -> Fact {
        Fact {
            unit: ObservationUnit::Request,
            normalized: n,
            model: Some(model.into()),
            agent: agent.into(),
            source: format!("{agent}/stream"),
            turn: None,
            unusable: None,
        }
    }

    fn codex(i: Option<u64>, r: Option<u64>) -> Normalized {
        counters::codex(Ok(i), Ok(r), Ok(None), Ok(Some(1)))
    }

    #[test]
    fn hit_rate_sums_operands_on_rows_reporting_both() {
        let book = Book::standard_2026_10_08();
        let facts = [
            fact("a", "gpt-6-luna", codex(Some(100), Some(80))),
            fact("a", "gpt-6-luna", codex(Some(1000), Some(900))),
            fact("b", "gpt-6-luna", codex(Some(5000), None)),
        ];
        let t = totals(&facts, &book, Some(2));
        assert_eq!(t.hit_rate.numerator, Some(Wide(980)));
        assert_eq!(t.hit_rate.denominator, Some(Wide(1100)));
        assert!((t.hit_rate.value.unwrap().get() - 980.0 / 1100.0).abs() < 1e-12);
        assert_eq!(t.hit_rate.state, MetricState::Partial);
        assert_eq!(t.hit_rate.reason, Some(CoverageReason::NotReported));
        assert_eq!(t.hit_rate.coverage.eligible_observations, Wide(2));
        assert_eq!(t.hit_rate.coverage.observations_seen, Wide(3));
        assert_eq!(t.hit_rate.coverage.reporting_agents, Wide(1));
        assert_eq!(t.hit_rate.coverage.expected_agents, Some(Wide(2)));
        // Total input covers all three; cache read only two.
        assert_eq!(t.input_total.known_sum, Some(Wide(6100)));
        assert_eq!(t.input_total.state, MetricState::Complete);
        assert_eq!(t.cache_read.state, MetricState::Partial);
    }

    #[test]
    fn haiku_example_totals() {
        let book = Book::standard_2026_10_08();
        let n = counters::claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(Some(10)),
            Ok(Some(6)),
            Ok(Some(4)),
            Ok(Some(5)),
        );
        let t = totals(&[fact("a", "claude-haiku-4-5", n)], &book, None);
        assert_eq!(t.input_total.known_sum, Some(Wide(100)));
        assert_eq!(t.hit_rate.value.map(Fraction::get), Some(0.8));
        // write share 15.5 / 33.5, in micro-USD tenths: 155 / 335
        assert_eq!(t.write_share.numerator, Some(Wide(15_500_000)));
        assert_eq!(t.write_share.denominator, Some(Wide(33_500_000)));
        assert_eq!(t.estimate.state, EstimateState::Finite);
        assert_eq!(t.estimate.lower_pico_usd, Some(Wide(58_500_000)));
        assert_eq!(t.estimate.upper_pico_usd, Some(Wide(58_500_000)));
        assert_eq!(t.total_tokens.known_sum, Some(Wide(105)));
    }

    #[test]
    fn haiku_without_ttl_split_gives_ranges() {
        let book = Book::standard_2026_10_08();
        let n = counters::claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(Some(10)),
            Ok(None),
            Ok(None),
            Ok(Some(5)),
        );
        let t = totals(&[fact("a", "claude-haiku-4-5", n)], &book, None);
        assert_eq!(t.estimate.lower_pico_usd, Some(Wide(55_500_000)));
        assert_eq!(t.estimate.upper_pico_usd, Some(Wide(63_000_000)));
        assert_eq!(t.estimate.assumptions.len(), 1);
        let lower = t.write_share.lower.unwrap();
        let upper = t.write_share.upper.unwrap();
        assert_eq!(
            (lower.numerator, lower.denominator),
            (Wide(12_500_000), Wide(30_500_000))
        );
        assert_eq!(
            (upper.numerator, upper.denominator),
            (Wide(20_000_000), Wide(38_000_000))
        );
        assert_eq!(t.write_share.value, None);
        assert_eq!(t.cache_write_5m.state, MetricState::Unavailable);
        assert_eq!(
            t.cache_write_5m.known_sum, None,
            "unreported buckets are not zero"
        );
    }

    #[test]
    fn luna_without_writes_is_a_range_and_has_no_write_share() {
        let book = Book::standard_2026_10_08();
        let n = counters::codex(Ok(Some(100)), Ok(Some(80)), Ok(None), Ok(Some(5)));
        let t = totals(&[fact("a", "gpt-6-luna", n)], &book, None);
        assert_eq!(t.hit_rate.value.map(Fraction::get), Some(0.8));
        assert_eq!(t.estimate.lower_pico_usd, Some(Wide(5_300_000)));
        assert_eq!(t.estimate.upper_pico_usd, Some(Wide(5_800_000)));
        assert_eq!(t.input_uncached.state, MetricState::Unavailable);
        assert_eq!(t.write_share.state, MetricState::Unavailable);
        assert_eq!(t.write_share.reason, Some(CoverageReason::NotReported));
    }

    #[test]
    fn unpriced_models_make_the_estimate_partial_and_are_named() {
        let book = Book::standard_2026_10_08();
        let priced = counters::claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(Some(10)),
            Ok(Some(6)),
            Ok(Some(4)),
            Ok(Some(5)),
        );
        let facts = [
            fact("a", "claude-haiku-4-5", priced.clone()),
            fact("b", "mystery-1", priced),
        ];
        let t = totals(&facts, &book, None);
        assert_eq!(t.estimate.state, EstimateState::Partial);
        assert_eq!(t.estimate.lower_pico_usd, Some(Wide(58_500_000)));
        assert_eq!(t.estimate.upper_pico_usd, None);
        assert_eq!(t.estimate.excluded_models.len(), 1);
        assert_eq!(&*t.estimate.excluded_models[0], "mystery-1");
        let none = totals(&facts[1..], &book, None);
        assert_eq!(none.estimate.state, EstimateState::Unavailable);
        assert_eq!(none.estimate.reason, Some(CoverageReason::NoPrice));
        assert_eq!(none.estimate.lower_pico_usd, None);
    }

    #[test]
    fn zero_input_is_unavailable_with_its_own_reason() {
        let book = Book::standard_2026_10_08();
        let t = totals(
            &[fact("a", "gpt-6-luna", codex(Some(0), Some(0)))],
            &book,
            None,
        );
        assert_eq!(t.hit_rate.state, MetricState::Unavailable);
        assert_eq!(t.hit_rate.reason, Some(CoverageReason::ZeroInput));
        let empty = totals(&[], &book, None);
        assert_eq!(empty.hit_rate.reason, Some(CoverageReason::NoCoveredInput));
        assert_eq!(empty.output.known_sum, None);
        assert_eq!(empty.estimate.state, EstimateState::Unavailable);
    }

    #[test]
    fn batches_and_turns_keep_their_cardinality_honest() {
        let book = Book::standard_2026_10_08();
        let mut batch = fact("a", "gpt-6-luna", codex(Some(10), Some(5)));
        batch.unit = ObservationUnit::CounterDelta;
        let mut one = fact("a", "gpt-6-luna", codex(Some(10), Some(5)));
        one.turn = Some("t1".into());
        let mut two = one.clone();
        two.turn = Some("t1".into());
        let t = totals(&[one, two, batch], &book, None);
        assert_eq!(t.requests_observed.known_sum, Some(Wide(2)));
        assert_eq!(t.requests_observed.state, MetricState::Partial);
        assert_eq!(
            t.requests_observed.reason,
            Some(CoverageReason::NativeBatch)
        );
        assert!(!t.input_total.coverage.request_cardinality_complete);
        assert_eq!(t.turns_observed.known_sum, Some(Wide(1)));
        assert_eq!(
            t.turns_observed.reason,
            Some(CoverageReason::MissingTurnIdentity)
        );
        assert_eq!(
            t.input_total.known_sum,
            Some(Wide(30)),
            "batch tokens still count"
        );
    }

    #[test]
    fn a_partial_ttl_split_prices_only_the_unbucketed_rest_as_a_range() {
        let book = Book::standard_2026_10_08();
        // W = 10 with 6 known in the 5-minute bucket and the 1-hour bucket absent.
        let n = counters::claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(Some(10)),
            Ok(Some(6)),
            Ok(None),
            Ok(Some(5)),
        );
        let t = totals(&[fact("a", "claude-haiku-4-5", n)], &book, None);
        // 6 * 1.25 + 4 * [1.25, 2] micro-USD on writes: [12.5, 15.5].
        assert_eq!(t.estimate.lower_pico_usd, Some(Wide(55_500_000)));
        assert_eq!(t.estimate.upper_pico_usd, Some(Wide(58_500_000)));
        assert_eq!(t.cache_write_1h.known_sum, None);
        // A known total equal to its known bucket resolves the cost without
        // turning the absent bucket into a reported zero.
        let resolved = counters::claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(Some(6)),
            Ok(Some(6)),
            Ok(None),
            Ok(Some(5)),
        );
        let t = totals(&[fact("a", "claude-haiku-4-5", resolved)], &book, None);
        assert_eq!(t.estimate.lower_pico_usd, t.estimate.upper_pico_usd);
        assert_eq!(t.cache_write_1h.state, MetricState::Unavailable);
    }

    #[test]
    fn category_split_needs_all_four_categories_on_every_row() {
        let full = counters::claude(
            Ok(Some(1)),
            Ok(Some(1)),
            Ok(Some(0)),
            Ok(None),
            Ok(None),
            Ok(Some(1)),
        );
        let no_write = counters::codex(Ok(Some(10)), Ok(Some(5)), Ok(None), Ok(Some(1)));
        assert!(category_split_complete(&[fact("a", "m", full.clone())]));
        assert!(!category_split_complete(&[
            fact("a", "m", full),
            fact("a", "m", no_write)
        ]));
        assert!(category_split_complete(&[]));
    }

    #[test]
    fn invalid_input_keeps_output_and_excludes_input_metrics() {
        let book = Book::standard_2026_10_08();
        let bad = counters::codex(Ok(Some(10)), Ok(Some(80)), Ok(None), Ok(Some(7)));
        let t = totals(&[fact("a", "gpt-6-luna", bad)], &book, None);
        assert_eq!(t.output.known_sum, Some(Wide(7)));
        assert_eq!(t.input_total.state, MetricState::Unavailable);
        assert_eq!(t.input_total.reason, Some(CoverageReason::InvalidCounter));
        assert_eq!(t.hit_rate.state, MetricState::Unavailable);
        assert_eq!(
            t.estimate.state,
            EstimateState::Partial,
            "output is priced, input unknown"
        );
    }

    #[test]
    fn a_batch_delta_is_priced_across_tiers_whatever_its_size() {
        let book = Book::standard_2026_10_08();
        let n = counters::codex(Ok(Some(300_000)), Ok(Some(0)), Ok(Some(0)), Ok(Some(0)));
        let mut batch = fact("a", "gpt-6.1-sol", n);
        batch.unit = ObservationUnit::CounterDelta;
        let e = totals(&[batch], &book, None).estimate;
        // Two 150,000-token requests (0.60 USD) or one long-context request (1.20 USD).
        assert_eq!(e.lower_pico_usd, Some(Wide(600_000_000_000)));
        assert_eq!(e.upper_pico_usd, Some(Wide(1_200_000_000_000)));
        assert!(e
            .assumptions
            .iter()
            .any(|a| a.contains("both context tiers")));
    }

    #[test]
    fn a_priced_model_with_no_reported_counter_is_not_an_observed_zero() {
        let book = Book::standard_2026_10_08();
        let none = counters::claude(Ok(None), Ok(None), Ok(None), Ok(None), Ok(None), Ok(None));
        let e = totals(&[fact("a", "claude-haiku-4-5", none.clone())], &book, None).estimate;
        assert_eq!(e.state, EstimateState::Unavailable);
        assert_eq!(e.reason, Some(CoverageReason::NotReported));
        assert_eq!(e.lower_pico_usd, None);
        assert_eq!(e.coverage.eligible_observations, Wide(0));
        assert_eq!(e.coverage.observations_seen, Wide(1));
        // Beside an observed row it leaves the upper bound open.
        let seen = codex(Some(100), Some(80));
        let e = totals(
            &[
                fact("a", "claude-haiku-4-5", none),
                fact("a", "gpt-6-luna", seen),
            ],
            &book,
            None,
        )
        .estimate;
        assert_eq!(e.state, EstimateState::Partial);
        assert_eq!(e.coverage.eligible_observations, Wide(1));
        assert_eq!(e.upper_pico_usd, None);
    }

    #[test]
    fn overflowing_cost_sums_are_checked() {
        let book = Book::standard_2026_10_08();
        let n = codex(Some(1), Some(1));
        let facts = [
            fact("a", "gpt-6-luna", n.clone()),
            fact("a", "gpt-6-luna", n),
        ];
        let huge = RowCost {
            total: Bounds {
                lo: u128::MAX,
                hi: Some(u128::MAX),
            },
            share: Some(super::super::price::ShareParts {
                other_lo: u128::MAX,
                other_hi: u128::MAX,
                write_lo: 1,
                write_hi: 1,
            }),
            ttl_unknown_tokens: 0,
            residual_tokens: 0,
            tier_unknown: false,
            observed: true,
        };
        let common = Common {
            requests: 2,
            cardinality_complete: true,
            expected_agents: None,
        };
        let costs = [Some(Priced::Cost(huge)), Some(Priced::Cost(huge))];
        let e = estimate(&facts, &costs, &book, &common);
        assert_eq!(e.state, EstimateState::Unavailable);
        assert_eq!(e.reason, Some(CoverageReason::InvalidCounter));
        assert_eq!(e.lower_pico_usd, None, "no clamped bound");
        assert_eq!(e.upper_pico_usd, None);
        // An unknown upper bound does not hide an overflowing lower sum.
        let open = RowCost {
            total: Bounds {
                lo: u128::MAX,
                hi: None,
            },
            ..huge
        };
        let costs = [Some(Priced::Cost(open)), Some(Priced::Cost(huge))];
        let e = estimate(&facts, &costs, &book, &common);
        assert_eq!(e.state, EstimateState::Unavailable);
        assert_eq!(e.reason, Some(CoverageReason::InvalidCounter));
        let costs = [Some(Priced::Overflow), Some(Priced::Cost(huge))];
        assert_eq!(
            estimate(&facts, &costs, &book, &common).state,
            EstimateState::Unavailable
        );
        let costs = [Some(Priced::Cost(huge)), Some(Priced::Cost(huge))];
        let share = write_share(&facts, &costs, &common);
        assert_eq!(share.state, MetricState::Unavailable);
        assert_eq!(share.reason, Some(CoverageReason::InvalidCounter));
        let mut sum = Some(u128::MAX);
        accumulate(&mut sum, 1);
        assert_eq!(sum, None);
    }

    #[test]
    fn turn_ids_count_per_source_stream() {
        let book = Book::standard_2026_10_08();
        let mut a = fact("a", "gpt-6-luna", codex(Some(10), Some(5)));
        a.turn = Some("turn-1".into());
        let mut b = fact("b", "gpt-6-luna", codex(Some(10), Some(5)));
        b.turn = Some("turn-1".into());
        let mut a_again = a.clone();
        a_again.turn = Some("turn-1".into());
        let t = totals(&[a, b, a_again], &book, None);
        assert_eq!(t.turns_observed.known_sum, Some(Wide(2)));
    }

    #[test]
    fn only_unusable_rows_keep_their_own_reason() {
        let book = Book::standard_2026_10_08();
        let mut gap = fact("a", "gpt-6-luna", codex(None, None));
        gap.unit = ObservationUnit::Unusable;
        gap.unusable = Some(CoverageReason::SourceGap);
        let e = totals(&[gap], &book, None).estimate;
        assert_eq!(e.state, EstimateState::Unavailable);
        assert_eq!(e.reason, Some(CoverageReason::SourceGap));
    }
}
