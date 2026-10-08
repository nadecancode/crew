//! Shared context for the negotiated feature services.
//!
//! These records describe routing and cancellation. They do not grant access:
//! the daemon checks authentication, feature readiness and scope before calling
//! a service. Defining this interface does not advertise any feature version.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crew_core::rpc::features::{FeatureUnavailable, Id};
use crew_core::rpc::RpcError;
use tokio::sync::Notify;

/// The physical authenticated connection's epoch, not its reusable principal.
pub type ConnId = Id;

/// Services use the existing finite RPC error envelope.
pub type FeatureError = RpcError;

/// Whether the complete service contract can be negotiated on a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    Unavailable(FeatureUnavailable),
}

/// A captured workspace run. Mutations still validate the run under the same
/// transaction or model lock as their write; this value is not a write fence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunCtx {
    pub workspace_run_id: Id,
}

/// Context shared with one connection's feature jobs and independent watches.
/// A cloned context retains its original epoch; it cannot be retagged onto a
/// replacement connection. Dropping a job's clone does not cancel its peers.
#[derive(Clone, Debug)]
pub struct ConnCtx {
    pub connection_epoch: ConnId,
    pub run: RunCtx,
    pub cancellation: Cancellation,
}

impl ConnCtx {
    pub fn new(connection_epoch: ConnId, run: RunCtx) -> Self {
        Self {
            connection_epoch,
            run,
            cancellation: Cancellation::default(),
        }
    }
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

/// Cancellation is a signal, not a release of a worker permit. A blocking job
/// keeps its permit and resources until it actually finishes.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<CancellationState>);

impl Cancellation {
    pub fn cancel(&self) {
        if !self.0.cancelled.swap(true, Ordering::AcqRel) {
            self.0.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        // Register before checking the sticky flag. notify_waiters does not
        // retain a permit for a future that has not registered yet.
        let notified = self.0.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.is_cancelled() {
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancellation_is_sticky_and_wakes_all_job_clones() {
        let cancellation = Cancellation::default();
        let mut jobs = Vec::new();
        for _ in 0..8 {
            let token = cancellation.clone();
            jobs.push(tokio::spawn(async move { token.cancelled().await }));
        }
        tokio::task::yield_now().await;
        cancellation.cancel();
        cancellation.cancel();
        for job in jobs {
            tokio::time::timeout(Duration::from_secs(1), job)
                .await
                .expect("a job missed cancellation")
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), cancellation.cancelled())
            .await
            .expect("a later job missed sticky cancellation");
    }

    #[tokio::test]
    async fn dropping_a_job_does_not_cancel_another_connection() {
        let run = RunCtx {
            workspace_run_id: Id::new("run-1").unwrap(),
        };
        let first = ConnCtx::new(Id::new("connection-1").unwrap(), run.clone());
        let second = ConnCtx::new(Id::new("connection-2").unwrap(), run);
        drop(first.clone());
        assert!(!first.cancellation.is_cancelled());
        first.cancellation.cancel();
        assert!(first.cancellation.is_cancelled());
        assert!(!second.cancellation.is_cancelled());
        assert_ne!(first.connection_epoch, second.connection_epoch);
    }
}
