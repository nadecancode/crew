//! Native UI callback contract. Delivery belongs to the shared Rust service.

use crate::records::{ClientNotice, TerminalChunk};

/// Implementations enqueue these owned values onto their platform executor.
/// They must not render from the native IO worker or retain credentials.
/// The service invokes callbacks outside its state and queue locks.
#[uniffi::export(callback_interface)]
pub trait ClientObserver: Send + Sync {
    fn changed(&self, notice: ClientNotice);
    fn terminal(&self, chunk: TerminalChunk);
}
