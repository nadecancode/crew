//! Shared building blocks for crew: the wire protocol spoken over the daemon's
//! Unix socket, the domain types it carries, team configuration, and a small
//! blocking client used by the CLI, the MCP bridge, hooks and the dashboard.

pub mod client;
pub mod config;
pub mod paths;
pub mod rpc;
pub mod signals;
pub mod types;
pub mod util;

pub use types::*;

#[cfg(test)]
mod test_support;
