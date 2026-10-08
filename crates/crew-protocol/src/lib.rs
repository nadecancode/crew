//! Platform-independent domain types shared by crew clients and the daemon.
//!
//! This crate contains domain records, configuration-free protocol records and
//! version control event payloads. Host RPC calls, configuration and transport
//! remain in `crew-core`.

pub mod asides;
pub mod docs;
pub mod epics;
pub mod features;
pub mod native;
pub mod rpc;
pub mod types;
pub mod usage_metrics;
pub mod vcs;

pub use types::*;
