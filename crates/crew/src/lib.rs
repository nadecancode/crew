//! crew: local-first orchestration for teams of coding agents.
//!
//! The crate is a library plus a thin binary so the integration tests can
//! drive the daemon in-process. The layers, bottom-up:
//!
//! - `persistence` — storage traits and their SQLite implementation
//! - `message_bus` — the agent-to-agent bus
//! - `tasks`, `scheduler` — the task graph and the pure planning function
//! - `git`, `vcs` — workspace isolation and commit exchange (jj or git)
//! - `agents` — runtime adapters (Claude Code, Codex, shell)
//! - `process` — where agent processes run (daemon-owned PTYs, herdr panes)
//! - `daemon` — the long-running process tying it all together
//! - `mcp` — the MCP bridge that gives agents their tools
//! - `sandbox` — bubblewrap sandboxes and the shim agents run behind
//! - `cli` — the command-line front end

pub mod agents;
pub mod cli;
pub mod daemon;
pub mod endpoints;
pub mod git;
pub mod limits;
pub mod mcp;
pub mod message_bus;
pub mod models;
#[cfg(feature = "native-gateway")]
pub mod native;
pub mod persistence;
pub mod process;
pub mod sandbox;
pub mod scheduler;
pub mod styles;
pub mod tasks;
pub mod transcript;
pub mod vcs;
pub mod web;

#[cfg(test)]
mod git_fixture_tests;
#[cfg(test)]
mod git_test_support;
