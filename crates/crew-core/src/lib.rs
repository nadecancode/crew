//! Shared building blocks for crew: the wire protocol spoken over the daemon's
//! Unix socket, the domain types it carries, team configuration, and a small
//! blocking client used by the CLI, the MCP bridge, hooks and the dashboard.

pub mod client;
pub mod config;
pub mod guardrails;
pub mod output_view;
pub mod paths;
pub mod rpc;
pub mod signals;
pub mod toolbox;
pub mod util;

pub use crew_protocol::{types, vcs, workflow};

pub use types::*;

#[cfg(test)]
mod test_support;
