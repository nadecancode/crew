//! Reading a registered stream into the ledger, one bounded batch at a time.
//!
//! The service decides when and how often a stream is read; this is the
//! step it runs on a blocking worker. Normalization happens here, outside any
//! database transaction, and each transaction carries at most
//! [`MAX_CHANGES`] observation changes with the cursor after the last line it
//! covers.

use std::fs::File;
use std::sync::Mutex;

use anyhow::{Context, Result};

use super::ledger::{Commit, Committed, Ledger, MAX_CHANGES};
use super::source::{self, Item};

/// Access to the ledger for one pump, taken only around each database step
/// so that reading and decoding never hold it.
pub trait Access {
    fn with<T>(&mut self, f: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T>;
}

impl Access for &mut Ledger {
    fn with<T>(&mut self, f: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T> {
        f(self)
    }
}

impl Access for &Mutex<Ledger> {
    fn with<T>(&mut self, f: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T> {
        let mut ledger = self.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut ledger)
    }
}

/// What one pump did.
#[derive(Debug, Default)]
pub struct Pumped {
    pub commits: Vec<Committed>,
    /// Everything present when the batch started is in the ledger.
    pub caught_up: bool,
}

fn changes(items: &[Item]) -> usize {
    items
        .iter()
        .filter(|i| matches!(i, Item::Observation(_)))
        .count()
}

/// Reads the next batch of `stream_id` from `file`, a fresh open of its
/// source, and commits it. `max_commits` stops early (for tests of
/// interrupted reading); the service passes `None`.
pub fn pump(
    mut ledger: impl Access,
    stream_id: &str,
    file: &File,
    now: i64,
    max_commits: Option<usize>,
) -> Result<Pumped> {
    let (mut from, mut adapter) = ledger
        .with(|l| l.cursor(stream_id))?
        .with_context(|| format!("unknown usage stream {stream_id}"))?;
    let batch = source::read(file, &from)?;
    let mut items = adapter.begin(&batch);
    let mut pumped = Pumped::default();
    for (index, line) in batch.lines.iter().enumerate() {
        adapter.line(line, &mut items);
        let last = index + 1 == batch.lines.len();
        if changes(&items) >= MAX_CHANGES && !last {
            let to = batch.checkpoint_after(file, index)?;
            pumped.commits.push(ledger.with(|l| {
                l.commit(Commit {
                    stream_id,
                    from: &from,
                    to: &to,
                    decoder: &adapter,
                    items: &items,
                    observed_at: now,
                })
            })?);
            from = to;
            items.clear();
            if max_commits.is_some_and(|m| pumped.commits.len() >= m) {
                return Ok(pumped);
            }
        }
    }
    pumped.commits.push(ledger.with(|l| {
        l.commit(Commit {
            stream_id,
            from: &from,
            to: &batch.checkpoint,
            decoder: &adapter,
            items: &items,
            observed_at: now,
        })
    })?);
    pumped.caught_up = batch.caught_up();
    Ok(pumped)
}

/// Pumps until everything present is in the ledger.
pub fn drain(
    mut ledger: impl Access,
    stream_id: &str,
    file: &File,
    now: i64,
) -> Result<Vec<Committed>> {
    let mut commits = Vec::new();
    loop {
        let pumped = pump(&mut ledger, stream_id, file, now, None)?;
        commits.extend(pumped.commits);
        if pumped.caught_up {
            return Ok(commits);
        }
    }
}

impl<A: Access> Access for &mut A {
    fn with<T>(&mut self, f: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T> {
        (**self).with(f)
    }
}
