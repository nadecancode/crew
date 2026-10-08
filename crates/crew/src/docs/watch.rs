//! Native notifications are bounded hints. Their paths and bodies are discarded;
//! only the descriptor-based source scanner can turn a hint into content.
use crate::daemon::features::{ConnCtx, FeatureWatch};
use crew_core::rpc::docs::*;
use crew_core::rpc::{Push, RpcError};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub struct Native {
    watcher: RecommendedWatcher,
    paths: BTreeSet<PathBuf>,
}
impl Native {
    pub fn new(dirty: Arc<AtomicBool>) -> Result<Self, notify::Error> {
        let watcher =
            notify::recommended_watcher(move |_: Result<notify::Event, notify::Error>| {
                dirty.store(true, Ordering::Release);
            })?;
        Ok(Self {
            watcher,
            paths: BTreeSet::new(),
        })
    }
    pub fn sync(&mut self, paths: BTreeSet<PathBuf>) -> Result<(), notify::Error> {
        if paths.len() > 4096 {
            return Err(notify::Error::generic("Docs watcher directory limit"));
        }
        for path in self.paths.difference(&paths) {
            let _ = self.watcher.unwatch(path);
        }
        for path in paths.difference(&self.paths) {
            self.watcher.watch(path, RecursiveMode::NonRecursive)?;
        }
        self.paths = paths;
        Ok(())
    }
}

pub struct Queue {
    inner: Mutex<Pending>,
    notify: Notify,
}
struct Pending {
    value: Option<DocsChanged>,
    closed: bool,
}
impl Default for Queue {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Pending {
                value: None,
                closed: false,
            }),
            notify: Notify::new(),
        }
    }
}
impl Queue {
    pub fn push(&self, mut change: DocsChanged) {
        let mut pending = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if pending.closed {
            return;
        }
        if let Some(old) = pending.value.take() {
            let mut sources = old.history_sources_changed.to_vec();
            sources.extend(change.history_sources_changed.iter().cloned());
            sources.sort();
            sources.dedup();
            sources.truncate(64);
            change.history_sources_changed =
                crew_core::rpc::features::List::new(sources).expect("bounded source change set");
            change.from_revision = old.from_revision;
            // A slow consumer gets one small resync, not a second queued delta.
            change.resync_required = true;
            change.upserts = Default::default();
            change.removed_doc_ids = Default::default();
            let mut expired = old.expired_seen_versions.to_vec();
            expired.extend(change.expired_seen_versions.iter().cloned());
            expired.sort();
            expired.dedup();
            expired.truncate(200);
            change.expired_seen_versions =
                crew_core::rpc::features::List::new(expired).expect("bounded expiries");
            for item in old.changed_selected.iter() {
                if !change
                    .changed_selected
                    .iter()
                    .any(|x| x.document == item.document)
                    && change.changed_selected.len() < 2
                {
                    let mut all = change.changed_selected.to_vec();
                    all.push(item.clone());
                    change.changed_selected =
                        crew_core::rpc::features::List::new(all).expect("two selections");
                }
            }
        }
        pending.value = Some(change);
        drop(pending);
        self.notify.notify_one();
    }
    pub fn close(&self) {
        let mut pending = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        pending.closed = true;
        pending.value = None;
        drop(pending);
        self.notify.notify_waiters();
    }
    /// Deliver one final metadata invalidation, then EOF. Physical disconnect
    /// uses close() instead, which discards queued data immediately.
    pub fn finish(&self, change: DocsChanged) {
        self.push(change);
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.notify.notify_waiters();
    }
}

pub struct Stream {
    pub queue: Arc<Queue>,
    pub context: ConnCtx,
}
#[async_trait::async_trait]
impl FeatureWatch for Stream {
    async fn next(&mut self) -> Result<Option<Push>, RpcError> {
        loop {
            let notified = self.queue.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.context.cancellation.is_cancelled() {
                return Ok(None);
            }
            {
                let mut pending = self.queue.inner.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(change) = pending.value.take() {
                    return Ok(Some(Push::DocsChanged(Box::new(change))));
                }
                if pending.closed {
                    return Ok(None);
                }
            }
            tokio::select! {_ = self.context.cancellation.cancelled()=>return Ok(None),_ = notified=>{}}
        }
    }
}
