//! Native usage counters: validation and per-runtime normalization.
//!
//! Every counter is optional. A runtime that does not report a field leaves
//! it unknown, and unknown is never zero; an explicitly reported zero is
//! evidence. Only nonnegative integers up to 2^53 - 1 are accepted, the range
//! the baseline collector (`scripts/cache/baseline.mjs`) trusts.

use crew_core::rpc::usage_metrics::CoverageReason;
use serde_json::Value;

/// The largest counter accepted from a native record.
pub const MAX_COUNTER: u64 = (1 << 53) - 1;

/// One native field, validated: `Ok(None)` when absent or null.
pub type Field = Result<Option<u64>, CoverageReason>;

/// Reads one native counter. Absent and null are unknown. Negative,
/// fractional, non-numeric and out-of-range values are invalid, never
/// clamped.
pub fn field(value: Option<&Value>) -> Field {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(v) if v <= MAX_COUNTER => Ok(Some(v)),
            _ => Err(CoverageReason::InvalidCounter),
        },
        Some(_) => Err(CoverageReason::InvalidCounter),
    }
}

/// Normalized counters, in the canonical names of `notes/usage-metrics.md`:
/// `I` input_total, `N` input_uncached, `R` cache_read, `W` cache_write,
/// `W5`/`W1` the native write buckets, `O` output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub input_total: Option<u64>,
    pub input_uncached: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub cache_write_5m: Option<u64>,
    pub cache_write_1h: Option<u64>,
    pub output: Option<u64>,
}

/// Which counters were derived rather than reported by the runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Derived {
    pub input_total: bool,
    pub input_uncached: bool,
}

/// How the runtime reports cache writes, which decides how they are priced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteKind {
    /// Anthropic: 5-minute and 1-hour buckets with their own prices.
    Buckets,
    /// OpenAI: one write price, whatever the TTL.
    Flat,
}

/// One observation's counters after validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Normalized {
    pub counters: Counters,
    pub derived: Derived,
    pub write_kind: WriteKind,
    /// False when an input-side counter or relation is invalid: every input
    /// metric then excludes this observation.
    pub input_valid: bool,
    /// Output stands on its own: an invalid input relation does not exclude it.
    pub output_valid: bool,
    /// Why counters were rejected, for coverage; empty when all were valid.
    pub invalid: Vec<CoverageReason>,
}

fn take(field: Field, invalid: &mut Vec<CoverageReason>, valid: &mut bool) -> Option<u64> {
    match field {
        Ok(v) => v,
        Err(reason) => {
            *valid = false;
            if !invalid.contains(&reason) {
                invalid.push(reason);
            }
            None
        }
    }
}

fn sum(parts: &[Option<u64>]) -> Option<Option<u64>> {
    let mut total: u64 = 0;
    for part in parts {
        let Some(v) = part else { return Some(None) };
        total = total.checked_add(*v)?;
    }
    (total <= MAX_COUNTER).then_some(Some(total))
}

/// Claude Code: `input_tokens` is ordinary input `N`, not `I`. `I` is
/// derived only when `N`, `R` and `W` are all known. The native 5m/1h split
/// is kept beside the aggregate write.
pub fn claude(
    uncached: Field,
    read: Field,
    write: Field,
    w5: Field,
    w1: Field,
    output: Field,
) -> Normalized {
    let mut invalid = Vec::new();
    let mut input_valid = true;
    let mut output_valid = true;
    let n = take(uncached, &mut invalid, &mut input_valid);
    let r = take(read, &mut invalid, &mut input_valid);
    let w = take(write, &mut invalid, &mut input_valid);
    let w5 = take(w5, &mut invalid, &mut input_valid);
    let w1 = take(w1, &mut invalid, &mut input_valid);
    let o = take(output, &mut invalid, &mut output_valid);
    if !buckets_fit(w, w5, w1) {
        input_valid = false;
        invalid.push(CoverageReason::InvalidCounter);
    }
    let mut derived = Derived::default();
    let i = match sum(&[n, r, w]) {
        Some(Some(total)) => {
            derived.input_total = true;
            Some(total)
        }
        Some(None) => None,
        None => {
            input_valid = false;
            invalid.push(CoverageReason::InvalidCounter);
            None
        }
    };
    Normalized {
        counters: Counters {
            input_total: i,
            input_uncached: n,
            cache_read: r,
            cache_write: w,
            cache_write_5m: w5,
            cache_write_1h: w1,
            output: o,
        },
        derived,
        write_kind: WriteKind::Buckets,
        input_valid,
        output_valid,
        invalid,
    }
}

/// Codex: `input_tokens` already includes cached input, so it is `I`.
/// `cache_write_input_tokens` is kept as reported, an explicit zero included;
/// absence stays unknown. `N = I - R - W` only when all three are known and
/// consistent. `I - R` alone is input not read from cache, not ordinary input.
pub fn codex(input: Field, read: Field, write: Field, output: Field) -> Normalized {
    let mut invalid = Vec::new();
    let mut input_valid = true;
    let mut output_valid = true;
    let i = take(input, &mut invalid, &mut input_valid);
    let r = take(read, &mut invalid, &mut input_valid);
    let w = take(write, &mut invalid, &mut input_valid);
    let o = take(output, &mut invalid, &mut output_valid);
    let mut derived = Derived::default();
    let mut n = None;
    if let Some(i) = i {
        let used = r.unwrap_or(0).checked_add(w.unwrap_or(0));
        match used {
            Some(used) if used <= i => {
                if let (Some(r), Some(w)) = (r, w) {
                    n = Some(i - r - w);
                    derived.input_uncached = true;
                }
            }
            _ => {
                input_valid = false;
                invalid.push(CoverageReason::InvalidCounter);
            }
        }
    }
    Normalized {
        counters: Counters {
            input_total: i,
            input_uncached: n,
            cache_read: r,
            cache_write: w,
            cache_write_5m: None,
            cache_write_1h: None,
            output: o,
        },
        derived,
        write_kind: WriteKind::Flat,
        input_valid,
        output_valid,
        invalid,
    }
}

/// pi (candidate mapping, gated until a supported native capture exists):
/// `input`, `cacheRead`, `cacheWrite` are `N`, `R`, `W`; `I` is derived when
/// all three are known. pi reports no write TTL buckets.
pub fn pi(uncached: Field, read: Field, write: Field, output: Field) -> Normalized {
    let mut normalized = claude(uncached, read, write, Ok(None), Ok(None), output);
    normalized.write_kind = WriteKind::Flat;
    normalized
}

/// Known buckets cannot exceed a known aggregate write, and two known
/// buckets must add up to it.
fn buckets_fit(w: Option<u64>, w5: Option<u64>, w1: Option<u64>) -> bool {
    let known = w5.unwrap_or(0).checked_add(w1.unwrap_or(0));
    match (w, known) {
        (_, None) => false,
        (Some(w), Some(k)) if w5.is_some() && w1.is_some() => k == w,
        (Some(w), Some(k)) => k <= w,
        (None, Some(_)) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fields_keep_unknown_apart_from_zero_and_reject_bad_values() {
        let v = json!({"zero": 0, "n": 12, "neg": -1, "frac": 1.5, "big": 9_007_199_254_740_992u64, "text": "3", "null": null});
        assert_eq!(field(v.get("zero")), Ok(Some(0)));
        assert_eq!(field(v.get("n")), Ok(Some(12)));
        assert_eq!(field(v.get("absent")), Ok(None));
        assert_eq!(field(v.get("null")), Ok(None));
        for bad in ["neg", "frac", "big", "text"] {
            assert_eq!(
                field(v.get(bad)),
                Err(CoverageReason::InvalidCounter),
                "{bad}"
            );
        }
        assert_eq!(field(Some(&json!(MAX_COUNTER))), Ok(Some(MAX_COUNTER)));
    }

    #[test]
    fn claude_derives_total_input_only_from_all_three_parts() {
        let all = claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(Some(10)),
            Ok(Some(6)),
            Ok(Some(4)),
            Ok(Some(5)),
        );
        assert_eq!(all.counters.input_total, Some(100));
        assert!(all.derived.input_total && all.input_valid && all.output_valid);
        let no_write = claude(
            Ok(Some(10)),
            Ok(Some(80)),
            Ok(None),
            Ok(None),
            Ok(None),
            Ok(Some(5)),
        );
        assert_eq!(no_write.counters.input_total, None);
        assert!(no_write.input_valid);
    }

    #[test]
    fn claude_bucket_relations_are_checked() {
        let over = claude(
            Ok(Some(1)),
            Ok(Some(1)),
            Ok(Some(5)),
            Ok(Some(4)),
            Ok(Some(4)),
            Ok(Some(1)),
        );
        assert!(!over.input_valid && over.output_valid);
        let one_bucket = claude(
            Ok(Some(1)),
            Ok(Some(1)),
            Ok(Some(5)),
            Ok(Some(5)),
            Ok(None),
            Ok(Some(1)),
        );
        assert!(one_bucket.input_valid);
        assert_eq!(
            one_bucket.counters.cache_write_1h, None,
            "an absent bucket stays unknown"
        );
    }

    #[test]
    fn an_invalid_input_field_leaves_output_eligible() {
        let n = claude(
            Err(CoverageReason::InvalidCounter),
            Ok(Some(1)),
            Ok(Some(1)),
            Ok(None),
            Ok(None),
            Ok(Some(9)),
        );
        assert!(!n.input_valid && n.output_valid);
        assert_eq!(n.counters.output, Some(9));
        assert_eq!(n.invalid, vec![CoverageReason::InvalidCounter]);
    }

    #[test]
    fn codex_keeps_reported_total_and_derives_ordinary_input_only_when_consistent() {
        let known = codex(Ok(Some(100)), Ok(Some(80)), Ok(Some(5)), Ok(Some(5)));
        assert_eq!(known.counters.input_uncached, Some(15));
        assert!(known.derived.input_uncached && !known.derived.input_total);
        let no_write = codex(Ok(Some(100)), Ok(Some(80)), Ok(None), Ok(Some(5)));
        assert_eq!(no_write.counters.input_uncached, None);
        assert_eq!(no_write.counters.cache_write, None);
        let zero_write = codex(Ok(Some(100)), Ok(Some(80)), Ok(Some(0)), Ok(Some(5)));
        assert_eq!(
            zero_write.counters.cache_write,
            Some(0),
            "an explicit zero is evidence"
        );
        let reads_exceed = codex(Ok(Some(10)), Ok(Some(80)), Ok(None), Ok(Some(5)));
        assert!(!reads_exceed.input_valid && reads_exceed.output_valid);
    }

    #[test]
    fn overflowing_derived_totals_are_invalid() {
        let n = claude(
            Ok(Some(MAX_COUNTER)),
            Ok(Some(1)),
            Ok(Some(0)),
            Ok(None),
            Ok(None),
            Ok(Some(0)),
        );
        assert!(!n.input_valid);
        assert_eq!(n.counters.input_total, None);
    }
}
