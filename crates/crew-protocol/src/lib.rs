//! Platform-independent domain types shared by crew clients and the daemon.
//!
//! This crate contains the domain records and version control event payloads.
//! RPC requests and transport remain in `crew-core` during the first extraction.

pub mod types;
pub mod vcs;

pub use types::*;
