//! pi: the candidate mapping, gated.
//!
//! pi's `usage.input/cacheRead/cacheWrite/output` map to `N/R/W/O`, with `I`
//! derived when all three input parts are known. No measured run of a
//! supported pi exists, so the mapping is not advertised: [`SUPPORTED`] stays
//! false and [`super::source::Adapter::new`] refuses pi with
//! `unsupported_runtime` until a native offline capture establishes
//! streaming identity, provider normalization, request count and
//! compaction/resume. A fixture parser is not a compatibility result.

use serde_json::Value;

use super::counters::{self, field, Normalized};

/// Whether pi's native usage is collected.
pub const SUPPORTED: bool = false;

/// The candidate normalization of one pi `usage` object.
pub fn normalize(usage: &Value) -> Normalized {
    counters::pi(
        field(usage.get("input")),
        field(usage.get("cacheRead")),
        field(usage.get("cacheWrite")),
        field(usage.get("output")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_metrics::source::{Adapter, Start};
    use crew_core::rpc::usage_metrics::CoverageReason;
    use crew_core::RuntimeKind;
    use serde_json::json;

    #[test]
    fn the_candidate_mapping_stays_unsupported() {
        let n = normalize(&json!({"input": 10, "cacheRead": 80, "cacheWrite": 10, "output": 5}));
        assert_eq!(n.counters.input_total, Some(100));
        assert_eq!(n.counters.cache_write_5m, None, "pi reports no TTL buckets");
        let unknown = normalize(&json!({"input": 10, "output": 5}));
        assert_eq!(unknown.counters.cache_read, None, "never an invented zero");
        const { assert!(!SUPPORTED) };
        assert_eq!(
            Adapter::new(RuntimeKind::Pi, "s", Start::Fresh),
            Err(CoverageReason::UnsupportedRuntime)
        );
    }
}
