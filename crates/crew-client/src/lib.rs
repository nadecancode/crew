//! Deterministic client queries shared by crew front ends.
//!
//! Unlike `crew_core::client`, the blocking Unix socket client, this crate does
//! not connect to a daemon. Callers supply domain records and time; hosts own
//! transport, credentials, clocks and presentation.

pub mod api;
pub mod order;
pub mod work;

pub use api::*;
pub use order::{is_orchestrator, natural_cmp, satellite_parent_id, sort_agents};
pub use work::{current_work, CurrentWork};
