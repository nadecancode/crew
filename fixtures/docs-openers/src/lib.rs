//! Compile the actual Docs safe-opener implementation and tests for another
//! Unix target without pulling the daemon's native C/PTY build dependencies in.
//! `cargo check --tests --target ...` checks types; it does not execute tests.
#[path = "../../../crates/crew/src/docs/config.rs"]
pub mod config;
#[path = "../../../crates/crew/src/os.rs"]
pub mod os;
#[path = "../../../crates/crew/src/docs/source.rs"]
pub mod source;
