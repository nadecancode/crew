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
        tiers.sort_by_key(|r| r.above_input_tokens.map_or(0, |t| t + 1));
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

    fn add(self, other: Bounds) -> Bounds {
        Bounds {
            lo: self.lo + other.lo,
            hi: self.hi.zip(other.hi).map(|(a, b)| a + b),
        }
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
    /// The context tier could not be decided (total input unknown).
    pub tier_unknown: bool,
}

/// How an observation prices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priced {
    /// No model, or a model this book does not list.
    NoPrice,
    Cost(RowCost),
}

fn mul(tokens: u64, rate: u64) -> u128 {
    tokens as u128 * rate as u128
}

/// Prices one normalized observation.
pub fn price(book: &Book, model: Option<&str>, n: &Normalized) -> Priced {
    let Some(model) = model else {
        return Priced::NoPrice;
    };
    let tiers = book.tiers(model);
    if tiers.is_empty() {
        return Priced::NoPrice;
    }
    let c = &n.counters;
    let candidates: Vec<&Rate> = match c.input_total.filter(|_| n.input_valid) {
        Some(input) => {
            let tier = tiers
                .iter()
                .rev()
                .find(|r| r.above_input_tokens.is_none_or(|t| input > t))
                .copied()
                .unwrap_or(tiers[0]);
            vec![tier]
        }
        None => tiers.clone(),
    };
    let tier_unknown = candidates.len() > 1;
    let mut combined: Option<RowCost> = None;
    for rate in candidates {
        let cost = price_tier(rate, n);
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
            },
        });
    }
    let mut cost = combined.expect("at least one tier");
    cost.tier_unknown = tier_unknown;
    Priced::Cost(cost)
}

fn price_tier(rate: &Rate, n: &Normalized) -> RowCost {
    let c = &n.counters;
    let output = match (n.output_valid, c.output) {
        (true, Some(o)) => Bounds::exact(mul(o, rate.output)),
        _ => Bounds::unbounded(),
    };
    if !n.input_valid {
        return RowCost {
            total: output.add(Bounds::unbounded()),
            share: None,
            ttl_unknown_tokens: 0,
            residual_tokens: 0,
            tier_unknown: false,
        };
    }
    let read = c
        .cache_read
        .map_or(Bounds::unbounded(), |r| Bounds::exact(mul(r, rate.read)));
    let mut ttl_unknown_tokens = 0;
    let mut residual_tokens = 0;
    // Writes, as an interval, when their total is known.
    let write: Option<(u128, u128)> = match rate.write {
        WriteRate::Flat(p) => c.cache_write.map(|w| (mul(w, p), mul(w, p))),
        WriteRate::Buckets { m5, h1 } => {
            let known =
                mul(c.cache_write_5m.unwrap_or(0), m5) + mul(c.cache_write_1h.unwrap_or(0), h1);
            let total = c
                .cache_write
                .or_else(|| c.cache_write_5m.zip(c.cache_write_1h).map(|(a, b)| a + b));
            total.map(|w| {
                let rest = if c.cache_write_5m.is_some() && c.cache_write_1h.is_some() {
                    0
                } else {
                    w - c.cache_write_5m.unwrap_or(0) - c.cache_write_1h.unwrap_or(0)
                };
                ttl_unknown_tokens = rest;
                (known + mul(rest, m5.min(h1)), known + mul(rest, m5.max(h1)))
            })
        }
    };
    let ordinary = c.input_uncached.map(|n| mul(n, rate.input));
    let mut input_side = read;
    match (ordinary, write) {
        (Some(o), Some((wl, wh))) => {
            input_side = input_side.add(Bounds::exact(o)).add(Bounds::range(wl, wh));
        }
        (Some(o), None) => input_side = input_side.add(Bounds::exact(o)).add(Bounds::unbounded()),
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
                    input_side = input_side.add(Bounds::range(
                        mul(x, rate.input.min(p)),
                        mul(x, rate.input.max(p)),
                    ));
                }
                None => {
                    input_side = input_side.add(Bounds::unbounded());
                    if let Some((wl, wh)) = w {
                        input_side = input_side.add(Bounds::range(wl, wh));
                    }
                }
            }
        }
    }
    let share = match (ordinary, c.cache_read, write) {
        (Some(o), Some(r), Some((wl, wh))) => {
            let other = o + mul(r, rate.read);
            Some(ShareParts {
                other_lo: other,
                other_hi: other,
                write_lo: wl,
                write_hi: wh,
            })
        }
        _ => None,
    };
    RowCost {
        total: input_side.add(output),
        share,
        ttl_unknown_tokens,
        residual_tokens,
        tier_unknown: false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::counters;
    use super::*;

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
        let Priced::Cost(cost) = price(&book, Some("claude-haiku-4-5"), &n) else {
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
        let Priced::Cost(cost) = price(&book, Some("claude-haiku-4-5-20251001"), &n) else {
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
        let Priced::Cost(cost) = price(&book, Some("gpt-6-luna"), &n) else {
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
            match price(&book, Some("gpt-6.1-sol"), &n) {
                Priced::Cost(c) => c.total.lo,
                Priced::NoPrice => panic!("priced"),
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
        let Priced::Cost(cost) = price(&book, Some("gpt-6-astra"), &batch) else {
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
            price(&book, Some("claude-haiku-4"), &n),
            Priced::NoPrice,
            "a family prefix is not a match"
        );
        assert_eq!(price(&book, None, &n), Priced::NoPrice);
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
        let Priced::Cost(cost) = price(&book, Some("claude-opus-5-5"), &n) else {
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
}
