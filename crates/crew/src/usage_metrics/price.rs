//! Versioned list-price books and exact cost intervals.
//!
//! A price book is immutable and reviewed; collection never fetches prices.
//! Rates are kept in micro-USD per million tokens, so `tokens * rate` is an
//! exact cost in pico-USD. Unknown parts of a request widen the interval or
//! leave its upper bound unknown; they are never priced as zero.

use crew_core::rpc::features::{Id, List, Text, Timestamp, Wide};
use crew_core::rpc::usage_metrics::{CostBasis, PriceBook, PriceRow};

use super::counters::Normalized;

/// Write prices of one model: one flat price, or one per TTL bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteRate {
    Flat(u64),
    Buckets { m5: u64, h1: u64 },
}

/// One model's rates for one context tier, in micro-USD per million tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rate {
    pub provider: String,
    pub model: String,
    /// Exact native ids that are this model. A family prefix is never a match.
    pub aliases: Vec<String>,
    /// The tier applies to requests whose total input is above this many
    /// tokens; `None` is the ordinary tier.
    pub above_input_tokens: Option<u64>,
    pub input: u64,
    pub read: u64,
    pub write: WriteRate,
    pub output: u64,
}

impl Rate {
    fn matches(&self, model: &str) -> bool {
        self.model == model || self.aliases.iter().any(|a| a == model)
    }
}

/// An immutable price book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    pub id: String,
    pub checked_at_ms: i64,
    pub rates: Vec<Rate>,
    pub sources: Vec<String>,
}

/// The highest rate a reviewed book holds, in micro-USD per million tokens
/// (about 1.1 million USD per million tokens). Arithmetic is checked
/// regardless; the cap keeps an overflow out of reach for real counters.
pub const MAX_RATE: u64 = 1 << 40;

/// USD per million tokens, written as micro-USD per million.
const fn usd(micro: u64) -> u64 {
    micro
}

const ANTHROPIC: &str = "https://platform.claude.com/docs/en/about-claude/pricing";
const OPENAI: &str = "https://developers.openai.com/api/docs/pricing";
/// 2026-10-08T00:00:00Z.
const CHECKED_2026_10_08: i64 = 1_791_417_600_000;
/// OpenAI's long-context tier: input above this many tokens.
const OPENAI_LONG_CONTEXT: u64 = 272_000;

impl Book {
    /// The reference rows of `notes/usage-metrics.md`, checked 2026-10-08.
    pub fn standard_2026_10_08() -> Book {
        let claude = |model: &str, aliases: &[&str], input, read, m5, h1, output| Rate {
            provider: "anthropic".into(),
            model: model.into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            above_input_tokens: None,
            input,
            read,
            write: WriteRate::Buckets { m5, h1 },
            output,
        };
        let mut rates = vec![
            claude(
                "claude-fable-5-1",
                &[],
                usd(10_000_000),
                usd(250_000),
                usd(12_500_000),
                usd(20_000_000),
                usd(50_000_000),
            ),
            claude(
                "claude-opus-5-5",
                &[],
                usd(4_000_000),
                usd(200_000),
                usd(5_000_000),
                usd(8_000_000),
                usd(20_000_000),
            ),
            claude(
                "claude-sonnet-5-5",
                &[],
                usd(2_000_000),
                usd(100_000),
                usd(2_500_000),
                usd(4_000_000),
                usd(10_000_000),
            ),
            claude(
                "claude-haiku-4-5",
                &["claude-haiku-4-5-20251001"],
                usd(1_000_000),
                usd(100_000),
                usd(1_250_000),
                usd(2_000_000),
                usd(5_000_000),
            ),
        ];
        // OpenAI rows: above 272,000 input tokens the whole request costs
        // twice the input and cache rates and 1.5 times the output rate.
        for (model, input, read, write, output) in [
            (
                "gpt-6-astra",
                usd(10_000_000),
                usd(1_000_000),
                usd(12_500_000),
                usd(50_000_000),
            ),
            (
                "gpt-6.1-sol",
                usd(2_000_000),
                usd(100_000),
                usd(2_500_000),
                usd(10_000_000),
            ),
            (
                "gpt-6-luna",
                usd(100_000),
                usd(10_000),
                usd(125_000),
                usd(500_000),
            ),
        ] {
            for (above, k_in, k_out_num, k_out_den) in
                [(None, 1, 1, 1), (Some(OPENAI_LONG_CONTEXT), 2, 3, 2)]
            {
                rates.push(Rate {
                    provider: "openai".into(),
                    model: model.into(),
                    aliases: Vec::new(),
                    above_input_tokens: above,
                    input: input * k_in,
                    read: read * k_in,
                    write: WriteRate::Flat(write * k_in),
                    output: output * k_out_num / k_out_den,
                });
            }
        }
        Book {
            id: "standard-2026-10-08".into(),
            checked_at_ms: CHECKED_2026_10_08,
            rates,
            sources: vec![ANTHROPIC.into(), OPENAI.into()],
        }
    }

    /// The tiers of a model, ordinary tier first.
    pub fn tiers(&self, model: &str) -> Vec<&Rate> {
        let mut tiers: Vec<&Rate> = self.rates.iter().filter(|r| r.matches(model)).collect();
        tiers.sort_by_key(|r| r.above_input_tokens.map_or(0, |t| t.saturating_add(1)));
        tiers
    }

    /// The wire form: rates in pico-USD per million tokens.
    pub fn to_wire(&self) -> PriceBook {
        const PICO_PER_MICRO: u128 = 1_000_000;
        let wide = |micro: u64| Wide(micro as u128 * PICO_PER_MICRO);
        let label = |s: &str| Text::new(s.to_string()).expect("price labels are short");
        let rows = self
            .rates
            .iter()
            .flat_map(|r| {
                std::iter::once(r.model.as_str())
                    .chain(r.aliases.iter().map(String::as_str))
                    .map(move |model| (r, model))
            })
            .map(|(r, model)| {
                let (flat, m5, h1) = match r.write {
                    WriteRate::Flat(w) => (Some(wide(w)), None, None),
                    WriteRate::Buckets { m5, h1 } => (None, Some(wide(m5)), Some(wide(h1))),
                };
                PriceRow {
                    provider: label(&r.provider),
                    model: label(model),
                    above_input_tokens: r.above_input_tokens.map(|t| Wide(t as u128)),
                    input_pico_usd_per_million: wide(r.input),
                    output_pico_usd_per_million: wide(r.output),
                    cache_read_pico_usd_per_million: Some(wide(r.read)),
                    cache_write_pico_usd_per_million: flat,
                    cache_write_5m_pico_usd_per_million: m5,
                    cache_write_1h_pico_usd_per_million: h1,
                }
            })
            .collect();
        PriceBook {
            id: Id::new(self.id.clone()).expect("price book id is valid"),
            checked_at: Timestamp::new(self.checked_at_ms).expect("check date is in range"),
            basis: CostBasis::StandardApiEquivalent,
            rows: List::new(rows).expect("price book fits its row limit"),
            sources: List::new(
                self.sources
                    .iter()
                    .map(|s| Text::new(s.clone()).expect("source URLs are short"))
                    .collect(),
            )
            .expect("price book fits its source limit"),
        }
    }
}

/// A cost interval in pico-USD. `hi` is `None` when part of the request
/// cannot be bounded (an unknown counter).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub lo: u128,
    pub hi: Option<u128>,
}

impl Bounds {
    fn exact(v: u128) -> Bounds {
        Bounds { lo: v, hi: Some(v) }
    }

    fn range(lo: u128, hi: u128) -> Bounds {
        Bounds { lo, hi: Some(hi) }
    }

    fn unbounded() -> Bounds {
        Bounds { lo: 0, hi: None }
    }

    /// The sum of two intervals, or `None` when a finite part overflows.
    /// An unknown upper bound stays unknown; it never hides an overflow.
    pub fn checked_plus(self, other: Bounds) -> Option<Bounds> {
        let lo = self.lo.checked_add(other.lo)?;
        let hi = match (self.hi, other.hi) {
            (Some(a), Some(b)) => Some(a.checked_add(b)?),
            _ => None,
        };
        Some(Bounds { lo, hi })
    }

    fn widen(self, other: Bounds) -> Bounds {
        Bounds {
            lo: self.lo.min(other.lo),
            hi: self.hi.zip(other.hi).map(|(a, b)| a.max(b)),
        }
    }
}

/// The input-side parts write share needs, as intervals: the cost of input
/// other than writes, and the cost of writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShareParts {
    pub other_lo: u128,
    pub other_hi: u128,
    pub write_lo: u128,
    pub write_hi: u128,
}

/// The cost of one observation under one price book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowCost {
    pub total: Bounds,
    /// `Some` only when every input-side counter, writes included, is known.
    pub share: Option<ShareParts>,
    /// Cache-write tokens whose TTL bucket is not reported (priced as a range).
    pub ttl_unknown_tokens: u64,
    /// Codex input not read from cache, with writes unknown, priced between
    /// the ordinary-input and write rates.
    pub residual_tokens: u64,
    /// The context tier could not be decided: total input unknown, or a
    /// native batch whose requests could fall in either tier.
    pub tier_unknown: bool,
    /// At least one priced counter was reported. A row without one has no
    /// observed cost, only an unknown one.
    pub observed: bool,
}

/// How an observation prices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priced {
    /// No model, or a model this book does not list.
    NoPrice,
    Cost(RowCost),
    /// The cost does not fit the arithmetic: no bound is claimed.
    Overflow,
}

fn mul(tokens: u64, rate: u64) -> u128 {
    tokens as u128 * rate as u128
}

/// What decides the context tier of the counters being priced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tiering {
    /// Whole requests, each with a total input in `min..=max`: one request
    /// has `min == max`; a sum of requests prices exactly when they all
    /// share a tier.
    Requests { min: u64, max: u64 },
    /// A native batch, a partial increment or unknown input: any tier.
    Unknown,
}

/// Prices one normalized observation, or a sum of observations that share
/// their model, unit and reported fields. A context tier applies per
/// request, so only whole requests' total inputs decide it; a batch or
/// partial increment is priced across every tier, whatever its size.
pub fn price(book: &Book, model: Option<&str>, n: &Normalized, tiering: Tiering) -> Priced {
    let Some(model) = model else {
        return Priced::NoPrice;
    };
    let tiers = book.tiers(model);
    if tiers.is_empty() {
        return Priced::NoPrice;
    }
    let tier_of = |input: u64| {
        tiers
            .iter()
            .rposition(|r| r.above_input_tokens.is_none_or(|t| input > t))
            .unwrap_or(0)
    };
    let candidates: Vec<&Rate> = match tiering {
        Tiering::Requests { min, max } if n.input_valid && min <= max => {
            tiers[tier_of(min)..=tier_of(max)].to_vec()
        }
        _ => tiers.clone(),
    };
    let tier_unknown = candidates.len() > 1;
    let mut combined: Option<RowCost> = None;
    for rate in candidates {
        let Some(cost) = price_tier(rate, n) else {
            return Priced::Overflow;
        };
        combined = Some(match combined {
            None => cost,
            Some(prev) => RowCost {
                total: prev.total.widen(cost.total),
                share: prev.share.zip(cost.share).map(|(a, b)| ShareParts {
                    other_lo: a.other_lo.min(b.other_lo),
                    other_hi: a.other_hi.max(b.other_hi),
                    write_lo: a.write_lo.min(b.write_lo),
                    write_hi: a.write_hi.max(b.write_hi),
                }),
                ttl_unknown_tokens: prev.ttl_unknown_tokens.max(cost.ttl_unknown_tokens),
                residual_tokens: prev.residual_tokens.max(cost.residual_tokens),
                tier_unknown,
                observed: prev.observed || cost.observed,
            },
        });
    }
    let mut cost = combined.expect("at least one tier");
    cost.tier_unknown = tier_unknown;
    Priced::Cost(cost)
}

/// One tier's cost of one row, with every sum checked: `None` on overflow.
fn price_tier(rate: &Rate, n: &Normalized) -> Option<RowCost> {
    let add = |a: u128, b: u128| a.checked_add(b);
    let c = &n.counters;
    let output_known = n.output_valid && c.output.is_some();
    let output = match (n.output_valid, c.output) {
        (true, Some(o)) => Bounds::exact(mul(o, rate.output)),
        _ => Bounds::unbounded(),
    };
    if !n.input_valid {
        return Some(RowCost {
            total: output.checked_plus(Bounds::unbounded())?,
            share: None,
            ttl_unknown_tokens: 0,
            residual_tokens: 0,
            tier_unknown: false,
            observed: output_known,
        });
    }
    let read = c
        .cache_read
        .map_or(Bounds::unbounded(), |r| Bounds::exact(mul(r, rate.read)));
    let mut ttl_unknown_tokens = 0;
    let mut residual_tokens = 0;
    // The charge of the reported TTL buckets, whatever else is unknown.
    let known_buckets = match rate.write {
        WriteRate::Buckets { m5, h1 } => add(
            mul(c.cache_write_5m.unwrap_or(0), m5),
            mul(c.cache_write_1h.unwrap_or(0), h1),
        )?,
        WriteRate::Flat(_) => 0,
    };
    // Writes, as an interval, when their total is known.
    let write: Option<(u128, u128)> = match rate.write {
        WriteRate::Flat(p) => c.cache_write.map(|w| (mul(w, p), mul(w, p))),
        WriteRate::Buckets { m5, h1 } => {
            let known = known_buckets;
            let total = c.cache_write.or_else(|| {
                c.cache_write_5m
                    .zip(c.cache_write_1h)
                    .and_then(|(a, b)| a.checked_add(b))
            });
            match total {
                Some(w) => {
                    let rest = if c.cache_write_5m.is_some() && c.cache_write_1h.is_some() {
                        0
                    } else {
                        w - c.cache_write_5m.unwrap_or(0) - c.cache_write_1h.unwrap_or(0)
                    };
                    ttl_unknown_tokens = rest;
                    Some((
                        add(known, mul(rest, m5.min(h1)))?,
                        add(known, mul(rest, m5.max(h1)))?,
                    ))
                }
                None => None,
            }
        }
    };
    // Without a total, known buckets still bound the writes from below.
    let written = match write {
        Some((wl, wh)) => Bounds::range(wl, wh),
        None => Bounds {
            lo: known_buckets,
            hi: None,
        },
    };
    let ordinary = c.input_uncached.map(|n| mul(n, rate.input));
    let mut input_side = read;
    match (ordinary, write) {
        (Some(o), _) => {
            input_side = input_side
                .checked_plus(Bounds::exact(o))?
                .checked_plus(written)?
        }
        (None, w) => {
            // Codex without write counters: input not read from cache is
            // ordinary input or writes, priced between the two rates.
            let residual = match (rate.write, c.input_total, c.cache_read, w) {
                (WriteRate::Flat(p), Some(i), Some(r), None) => Some((i - r, p)),
                _ => None,
            };
            match residual {
                Some((x, p)) => {
                    residual_tokens = x;
                    input_side = input_side.checked_plus(Bounds::range(
                        mul(x, rate.input.min(p)),
                        mul(x, rate.input.max(p)),
                    ))?;
                }
                None => {
                    input_side = input_side
                        .checked_plus(Bounds::unbounded())?
                        .checked_plus(written)?
                }
            }
        }
    }
    let share = match (ordinary, c.cache_read, write) {
        (Some(o), Some(r), Some((wl, wh))) => {
            let other = add(o, mul(r, rate.read))?;
            Some(ShareParts {
                other_lo: other,
                other_hi: other,
                write_lo: wl,
                write_hi: wh,
            })
        }
        _ => None,
    };
    let input_known = c.cache_read.is_some()
        || ordinary.is_some()
        || write.is_some()
        || c.cache_write_5m.is_some()
        || c.cache_write_1h.is_some();
    Some(RowCost {
        total: input_side.checked_plus(output)?,
        share,
        ttl_unknown_tokens,
        residual_tokens,
        tier_unknown: false,
        observed: output_known || input_known,
    })
}

#[cfg(test)]
mod tests {
    use super::super::counters;
    use super::*;

    /// One whole request's tiering.
    fn one(n: &Normalized) -> Tiering {
        n.counters
            .input_total
            .map_or(Tiering::Unknown, |i| Tiering::Requests { min: i, max: i })
    }

    const PICO: u128 = 1_000_000_000_000;

    fn usd_pico(usd_millionths_x10: u128) -> u128 {
        // tenths of a micro-USD, in pico-USD
        usd_millionths_x10 * PICO / 10_000_000
    }

    #[test]
    fn haiku_example_prices_exactly() {
        let book = Book::standard_2026_10_08();
        let n = counters::claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(Some(10)),
            Ok(Some(6)),
            Ok(Some(4)),
            Ok(Some(5)),
        );
        let Priced::Cost(cost) = price(&book, Some("claude-haiku-4-5"), &n, one(&n)) else {
            panic!("priced")
        };
        // 0.0000585 USD total; 0.0000155 write; 0.0000335 input side.
        assert_eq!(cost.total, Bounds::exact(usd_pico(585)));
        let share = cost.share.unwrap();
        assert_eq!(
            (share.write_lo, share.write_hi),
            (usd_pico(155), usd_pico(155))
        );
        assert_eq!(share.other_lo + share.write_lo, usd_pico(335));
        assert_eq!(cost.ttl_unknown_tokens, 0);
    }

    #[test]
    fn haiku_without_ttl_split_is_a_range() {
        let book = Book::standard_2026_10_08();
        let n = counters::claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(Some(10)),
            Ok(None),
            Ok(None),
            Ok(Some(5)),
        );
        let Priced::Cost(cost) = price(&book, Some("claude-haiku-4-5-20251001"), &n, one(&n))
        else {
            panic!("priced")
        };
        assert_eq!(cost.total, Bounds::range(usd_pico(555), usd_pico(630)));
        assert_eq!(cost.ttl_unknown_tokens, 10);
        let share = cost.share.unwrap();
        assert_eq!(
            (share.write_lo, share.write_hi),
            (usd_pico(125), usd_pico(200))
        );
    }

    #[test]
    fn luna_without_write_counter_bounds_the_residual() {
        let book = Book::standard_2026_10_08();
        let n = counters::codex(Ok(Some(100)), Ok(Some(80)), Ok(None), Ok(Some(5)));
        let Priced::Cost(cost) = price(&book, Some("gpt-6-luna"), &n, one(&n)) else {
            panic!("priced")
        };
        assert_eq!(cost.total, Bounds::range(usd_pico(53), usd_pico(58)));
        assert_eq!(cost.residual_tokens, 20);
        assert!(cost.share.is_none(), "write share needs write counts");
    }

    #[test]
    fn the_long_context_tier_applies_above_272k_to_the_whole_request() {
        let book = Book::standard_2026_10_08();
        let at = |i: u64| {
            let n = counters::codex(Ok(Some(i)), Ok(Some(0)), Ok(Some(0)), Ok(Some(1_000_000)));
            match price(&book, Some("gpt-6.1-sol"), &n, one(&n)) {
                Priced::Cost(c) => c.total.lo,
                other => panic!("{other:?}"),
            }
        };
        // 272,000 input at 2 USD/M + 1M output at 10 USD/M.
        assert_eq!(
            at(272_000),
            272_000u128 * 2_000_000 + 1_000_000 * 10_000_000
        );
        // 272,001: input at 4 USD/M, output at 15 USD/M.
        assert_eq!(
            at(272_001),
            272_001u128 * 4_000_000 + 1_000_000 * 15_000_000
        );
    }

    #[test]
    fn unknown_tier_spans_both_and_unknown_models_have_no_price() {
        let book = Book::standard_2026_10_08();
        let batch = counters::codex(Ok(None), Ok(Some(10)), Ok(None), Ok(Some(10)));
        let Priced::Cost(cost) = price(&book, Some("gpt-6-astra"), &batch, one(&batch)) else {
            panic!("priced")
        };
        assert!(cost.tier_unknown);
        assert_eq!(
            cost.total.hi, None,
            "unknown ordinary input leaves the upper bound open"
        );
        let n = counters::claude(
            Ok(Some(1)),
            Ok(Some(1)),
            Ok(Some(1)),
            Ok(None),
            Ok(None),
            Ok(Some(1)),
        );
        assert_eq!(
            price(&book, Some("claude-haiku-4"), &n, one(&n)),
            Priced::NoPrice,
            "a family prefix is not a match"
        );
        assert_eq!(price(&book, None, &n, one(&n)), Priced::NoPrice);
    }

    #[test]
    fn missing_output_leaves_the_upper_bound_open() {
        let book = Book::standard_2026_10_08();
        let n = counters::claude(
            Ok(Some(1)),
            Ok(Some(1)),
            Ok(Some(0)),
            Ok(Some(0)),
            Ok(Some(0)),
            Ok(None),
        );
        let Priced::Cost(cost) = price(&book, Some("claude-opus-5-5"), &n, one(&n)) else {
            panic!("priced")
        };
        assert_eq!(cost.total.hi, None);
        assert!(cost.share.is_some(), "write share ignores output");
    }

    #[test]
    fn wire_rates_are_pico_per_million() {
        let wire = Book::standard_2026_10_08().to_wire();
        let haiku = wire
            .rows
            .iter()
            .find(|r| &*r.model == "claude-haiku-4-5-20251001")
            .unwrap();
        assert_eq!(haiku.input_pico_usd_per_million, Wide(1_000_000_000_000));
        assert_eq!(
            haiku.cache_write_1h_pico_usd_per_million,
            Some(Wide(2_000_000_000_000))
        );
        let long = wire
            .rows
            .iter()
            .find(|r| &*r.model == "gpt-6-luna" && r.above_input_tokens.is_some())
            .unwrap();
        assert_eq!(long.output_pico_usd_per_million, Wide(750_000_000_000));
    }

    #[test]
    fn a_native_batch_is_priced_across_the_tiers_its_requests_could_fall_in() {
        let book = Book::standard_2026_10_08();
        let n = |i: u64| counters::codex(Ok(Some(i)), Ok(Some(0)), Ok(Some(0)), Ok(Some(0)));
        let cost = |i: u64, single: bool| {
            let tiering = if single { one(&n(i)) } else { Tiering::Unknown };
            match price(&book, Some("gpt-6.1-sol"), &n(i), tiering) {
                Priced::Cost(c) => c,
                other => panic!("{other:?}"),
            }
        };
        // One 300,000-token request is long context: 1.20 USD.
        assert_eq!(
            cost(300_000, true).total,
            Bounds::exact(300_000 * 4_000_000)
        );
        // A batch of 300,000 could be two 150,000-token requests: 0.60 to 1.20 USD.
        let batch = cost(300_000, false);
        assert_eq!(
            batch.total,
            Bounds::range(300_000 * 2_000_000, 300_000 * 4_000_000)
        );
        assert!(batch.tier_unknown);
        // A small increment can be part of a long-context request.
        let small = cost(1_000, false);
        assert_eq!(
            small.total,
            Bounds::range(1_000 * 2_000_000, 1_000 * 4_000_000)
        );
        assert!(small.tier_unknown);
    }

    #[test]
    fn a_row_with_no_reported_counter_has_no_observed_cost() {
        let book = Book::standard_2026_10_08();
        let none = counters::claude(Ok(None), Ok(None), Ok(None), Ok(None), Ok(None), Ok(None));
        let Priced::Cost(cost) = price(&book, Some("claude-haiku-4-5"), &none, one(&none)) else {
            panic!("priced")
        };
        assert!(!cost.observed);
        assert_eq!(cost.total.hi, None);
        let output_only = counters::claude(
            Ok(None),
            Ok(None),
            Ok(None),
            Ok(None),
            Ok(None),
            Ok(Some(0)),
        );
        let Priced::Cost(cost) = price(
            &book,
            Some("claude-haiku-4-5"),
            &output_only,
            one(&output_only),
        ) else {
            panic!("priced")
        };
        assert!(cost.observed, "an explicit zero is observed");
    }

    #[test]
    fn rates_and_counters_at_their_limits_do_not_overflow() {
        let book = Book::standard_2026_10_08();
        for rate in &book.rates {
            assert!(rate.input.max(rate.read).max(rate.output) <= MAX_RATE);
            match rate.write {
                WriteRate::Flat(p) => assert!(p <= MAX_RATE),
                WriteRate::Buckets { m5, h1 } => assert!(m5.max(h1) <= MAX_RATE),
            }
        }
        let max = counters::MAX_COUNTER;
        let at_limit = |write| Book {
            id: "limit".into(),
            checked_at_ms: 0,
            rates: vec![Rate {
                provider: "test".into(),
                model: "m".into(),
                aliases: vec![],
                above_input_tokens: None,
                input: MAX_RATE,
                read: MAX_RATE,
                write,
                output: MAX_RATE,
            }],
            sources: vec![],
        };
        let flat = at_limit(WriteRate::Flat(MAX_RATE));
        let n = counters::codex(
            Ok(Some(max)),
            Ok(Some(max / 2)),
            Ok(Some(max / 2)),
            Ok(Some(max)),
        );
        assert!(
            matches!(price(&flat, Some("m"), &n, one(&n)), Priced::Cost(c) if c.total.hi.is_some())
        );
        let buckets = at_limit(WriteRate::Buckets {
            m5: MAX_RATE,
            h1: MAX_RATE,
        });
        let n = counters::claude(
            Ok(Some(max / 3)),
            Ok(Some(max / 3)),
            Ok(Some(max / 3)),
            Ok(None),
            Ok(Some(max / 3)),
            Ok(Some(max)),
        );
        assert!(
            matches!(price(&buckets, Some("m"), &n, Tiering::Unknown), Priced::Cost(c) if c.total.hi.is_some())
        );
        assert_eq!(
            Bounds::exact(u128::MAX).checked_plus(Bounds::exact(1)),
            None
        );
        let unknown_upper = Bounds {
            lo: u128::MAX,
            hi: None,
        };
        assert_eq!(
            unknown_upper.checked_plus(Bounds::exact(1)),
            None,
            "an unknown upper bound does not hide an overflow"
        );
    }

    #[test]
    fn a_known_ttl_bucket_keeps_its_charge_when_the_total_write_is_unknown() {
        let book = Book::standard_2026_10_08();
        let n = counters::claude(
            Ok(Some(0)),
            Ok(Some(0)),
            Ok(None),
            Ok(Some(100)),
            Ok(None),
            Ok(Some(0)),
        );
        let Priced::Cost(cost) = price(&book, Some("claude-haiku-4-5"), &n, one(&n)) else {
            panic!("priced")
        };
        // 100 five-minute write tokens at 1.25 USD/M.
        assert_eq!(cost.total.lo, 125_000_000);
        assert_eq!(cost.total.hi, None, "the rest of the writes is unknown");
        assert!(cost.observed);
    }

    #[test]
    fn a_sum_of_requests_prices_exactly_only_when_they_share_a_tier() {
        let book = Book::standard_2026_10_08();
        // Two requests of 150,000 and 200,000 input tokens, summed.
        let sum = counters::codex(Ok(Some(350_000)), Ok(Some(0)), Ok(Some(0)), Ok(Some(0)));
        let at = |min, max| match price(
            &book,
            Some("gpt-6.1-sol"),
            &sum,
            Tiering::Requests { min, max },
        ) {
            Priced::Cost(c) => c,
            other => panic!("{other:?}"),
        };
        let short = at(150_000, 200_000);
        assert_eq!(
            short.total,
            Bounds::exact(350_000 * 2_000_000),
            "not long context"
        );
        assert!(!short.tier_unknown);
        let long = at(300_000, 400_000);
        assert_eq!(long.total, Bounds::exact(350_000 * 4_000_000));
        let straddling = at(100_000, 300_000);
        assert!(straddling.tier_unknown);
        assert_eq!(
            straddling.total,
            Bounds::range(350_000 * 2_000_000, 350_000 * 4_000_000)
        );
    }
}
