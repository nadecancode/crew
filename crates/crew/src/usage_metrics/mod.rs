//! Native usage metrics: the daemon reads usage the runtimes already write,
//! keeps sanitized observations with their coverage, and serves metrics that
//! never turn an unknown into a zero (`notes/usage-metrics.md`).
//!
//! - `counters`: validation and per-runtime normalization of native counters
//! - `price`: immutable price books and exact cost intervals
//! - `metrics`: metric computation with per-metric coverage
//! - `cohort`: cost per completed task over first completions

pub mod cohort;
pub mod counters;
pub mod metrics;
pub mod price;
