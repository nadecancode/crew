//! Human documentation observer. Source registration and authentication belong
//! to the daemon; public RPC records are the frozen crew-protocol Docs types.
//! This service never sends agent messages or invokes snapshotting VCS commands.

pub mod config;
pub mod source;
