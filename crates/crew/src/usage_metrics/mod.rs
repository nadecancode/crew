//! Native usage metrics: the daemon reads usage the runtimes already write,
//! keeps sanitized observations with their coverage, and serves metrics that
//! never turn an unknown into a zero (`notes/usage-metrics.md`).
//!
//! - `counters`: validation and per-runtime normalization of native counters
//! - `price`: immutable price books and exact cost intervals
//! - `metrics`: metric computation with per-metric coverage
//! - `cohort`: cost per completed task over first completions
//! - `source`: bounded, read-only tailing of a native file, with continuity
//! - `claude`, `codex`, `pi`: the runtime adapters (pi is gated)
//! - `ledger`: durable streams, ownership, observations, coverage and aggregates
//! - `collect`: one bounded read of a stream into the ledger
//! - `query`: canonical queries and their answers from the ledger
//! - `service`: collection scheduling, watches and the daemon's handlers

pub mod claude;
pub mod codex;
pub mod cohort;
pub mod collect;
pub mod counters;
pub mod ledger;
pub mod metrics;
#[cfg(test)]
mod parity;
pub mod pi;
pub mod price;
pub mod query;
pub mod service;
pub mod source;
