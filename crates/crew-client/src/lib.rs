//! Portable client state, actions and queries shared by crew front ends.
//!
//! Unlike `crew_core::client`, the blocking Unix socket client, this crate does
//! not connect to a daemon. Callers supply domain records and time; hosts own
//! transport, credentials, clocks and presentation.

pub mod actions;
pub mod api;
pub mod commands;
pub mod order;
pub mod policy;
pub mod topology;
pub mod usage;
pub mod work;

pub mod engine;
pub mod features;

mod history;
mod presentation;
mod questions;
mod state;

pub use api::*;
pub use engine::ClientCore;
pub use order::{is_orchestrator, natural_cmp, satellite_parent_id, sort_agents};
pub use work::{current_work, CurrentWork};
