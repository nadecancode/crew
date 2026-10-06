//! Platform-independent domain types shared by crew clients and the daemon.
//!
//! This crate contains domain records, configuration-free protocol records and
//! version control event payloads. Host RPC calls, configuration and transport
//! remain in `crew-core`.

pub mod native;
pub mod rpc;
pub mod types;
pub mod vcs;

pub use types::*;
