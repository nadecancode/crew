//! Independent observer forwarders. A domain registers before capturing its
//! initial result; this forwarder starts only once that result is queued.

use crew_core::rpc::features::{Feature, Id, Wide, MAX_PAGE_BYTES};
use crew_core::rpc::{Push, RpcError, ServerFrame};
use serde_json::Value;
use tokio::sync::mpsc;

use super::{ConnCtx, FeatureService};

/// Domain-owned bounded/coalesced metadata stream. This port never carries a
/// document, aside answer, native record or arbitrary legacy push.
#[async_trait::async_trait]
pub trait FeatureWatch: Send {
    async fn next(&mut self) -> Result<Option<Push>, RpcError>;
}

pub(crate) struct PendingWatch {
    pub feature: Feature,
    pub id: Id,
    epoch: Id,
    context: ConnCtx,
    stream: Box<dyn FeatureWatch>,
    marker: Push,
}

impl PendingWatch {
    pub fn new(
        feature: Feature,
        result: &Value,
        context: ConnCtx,
        service: &dyn FeatureService,
    ) -> Result<Self, RpcError> {
        let id = Id::new(field(result.get("watch_id"))?).map_err(RpcError::invalid)?;
        let epoch = match feature {
            Feature::Asides => field(result.pointer("/initial/aside_epoch"))?,
            Feature::Docs => field(result.pointer("/initial_index_page/epoch"))?,
            Feature::UsageMetrics => field(result.get("epoch"))?,
            Feature::Epics => {
                return Err(RpcError::new(
                    "internal",
                    "epics uses its grouped task stream",
                ))
            }
        };
        let epoch = Id::new(epoch).map_err(RpcError::invalid)?;
        let revision = match feature {
            Feature::Asides | Feature::UsageMetrics => result.pointer("/initial/revision"),
            Feature::Docs => result.pointer("/initial_index_page/revision"),
            Feature::Epics => None,
        }
        .cloned()
        .map(serde_json::from_value::<Wide>)
        .transpose()
        .map_err(|_| RpcError::internal("invalid watch revision"))?
        .unwrap_or_default();
        let marker = terminal(
            feature,
            &id,
            &epoch,
            &context.run.workspace_run_id,
            revision,
        );
        let stream = service.watch_stream(context.clone(), id.clone())?;
        Ok(Self {
            feature,
            id,
            epoch,
            context,
            stream,
            marker,
        })
    }

    pub async fn forward(mut self, tx: mpsc::Sender<ServerFrame>) {
        loop {
            let next = tokio::select! {
                biased;
                _ = self.context.connection_closed.cancelled() => break,
                _ = self.context.cancellation.cancelled() => {
                    self.finish(&tx, self.marker.clone()).await;
                    break;
                },
                next = self.stream.next() => next,
            };
            let Ok(Some(mut push)) = next else {
                self.finish(&tx, self.marker.clone()).await;
                break;
            };
            let Some((feature, id, epoch)) = identity(&push) else {
                self.finish(&tx, self.marker.clone()).await;
                break;
            };
            if feature != self.feature || id != &self.id {
                self.finish(&tx, self.marker.clone()).await;
                break;
            }
            if epoch != &self.epoch {
                self.finish(&tx, resync(push)).await;
                break;
            }
            self.marker = resync(push.clone());
            let frame = ServerFrame::push(push.clone());
            if crew_core::rpc::features::encode_frame(&frame)
                .map_or(true, |bytes| bytes.len() > MAX_PAGE_BYTES)
            {
                push = resync(push);
            }
            let frame = ServerFrame::push(push);
            match tx.try_send(frame) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => break,
                Err(mpsc::error::TrySendError::Full(frame)) => {
                    // Backpressure loses an intermediate update. Send only a
                    // tiny resync marker, while the domain coalesces its queue.
                    let marker = ServerFrame::push(resync(frame.push.expect("push frame")));
                    tokio::select! {
                        biased;
                        _ = self.context.connection_closed.cancelled() => break,
                        _ = self.context.cancellation.cancelled() => {
                            self.finish(&tx, self.marker.clone()).await;
                            break;
                        },
                        result = tx.send(marker) => if result.is_err() { break; },
                    }
                }
            }
        }
    }

    fn finish(
        &self,
        tx: &mpsc::Sender<ServerFrame>,
        marker: Push,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let closed = self.context.connection_closed.clone();
        let tx = tx.clone();
        async move {
            // Scope expiry must retire the client's watch. Physical disconnect
            // carries no final frame, and also releases a backpressured send.
            tokio::select! {
                biased;
                _ = closed.cancelled() => {},
                _ = tx.send(ServerFrame::push(marker)) => {},
            }
        }
    }
}

fn terminal(feature: Feature, id: &Id, epoch: &Id, run: &Id, revision: Wide) -> Push {
    match feature {
        Feature::Asides => Push::Aside(Box::new(crew_core::rpc::asides::AsideChanged {
            workspace_run_id: run.clone(),
            aside_epoch: epoch.clone(),
            watch_id: id.clone(),
            from_revision: revision,
            to_revision: revision,
            changed_ids: Default::default(),
            gone: Default::default(),
            availability_changed: true,
            resync_required: true,
        })),
        Feature::Docs => Push::DocsChanged(Box::new(crew_core::rpc::docs::DocsChanged {
            epoch: epoch.clone(),
            watch_id: id.clone(),
            from_revision: revision,
            to_revision: revision,
            upserts: Default::default(),
            removed_doc_ids: Default::default(),
            changed_selected: Default::default(),
            history_sources_changed: Default::default(),
            expired_seen_versions: Default::default(),
            resync_required: true,
        })),
        Feature::UsageMetrics => {
            Push::UsageChanged(Box::new(crew_core::rpc::usage_metrics::UsageChanged {
                workspace_run_id: run.clone(),
                epoch: epoch.clone(),
                watch_id: id.clone(),
                from_revision: revision,
                to_revision: revision,
                resync_required: true,
            }))
        }
        Feature::Epics => unreachable!("epics has no independent watch"),
    }
}

fn field(value: Option<&Value>) -> Result<&str, RpcError> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new("internal", "invalid feature watch result"))
}

fn identity(push: &Push) -> Option<(Feature, &Id, &Id)> {
    match push {
        Push::Aside(p) => Some((Feature::Asides, &p.watch_id, &p.aside_epoch)),
        Push::DocsChanged(p) => Some((Feature::Docs, &p.watch_id, &p.epoch)),
        Push::UsageChanged(p) => Some((Feature::UsageMetrics, &p.watch_id, &p.epoch)),
        _ => None,
    }
}

fn resync(mut push: Push) -> Push {
    match &mut push {
        Push::Aside(p) => {
            p.resync_required = true;
            p.changed_ids = Default::default();
            p.gone = Default::default();
        }
        Push::DocsChanged(p) => {
            p.resync_required = true;
            p.upserts = Default::default();
            p.removed_doc_ids = Default::default();
            p.changed_selected = Default::default();
            p.history_sources_changed = Default::default();
            p.expired_seen_versions = Default::default();
        }
        Push::UsageChanged(p) => p.resync_required = true,
        _ => {}
    }
    push
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::features::{Caller, RunCtx};
    use std::collections::VecDeque;
    use std::time::Duration;

    struct Script {
        replies: VecDeque<Result<Option<Push>, RpcError>>,
        pending: bool,
    }

    #[async_trait::async_trait]
    impl FeatureWatch for Script {
        async fn next(&mut self) -> Result<Option<Push>, RpcError> {
            if let Some(reply) = self.replies.pop_front() {
                return reply;
            }
            if self.pending {
                std::future::pending().await
            } else {
                Ok(None)
            }
        }
    }

    fn pending(
        feature: Feature,
        replies: Vec<Result<Option<Push>, RpcError>>,
        wait: bool,
    ) -> (PendingWatch, ConnCtx) {
        let id = Id::new("watch").unwrap();
        let epoch = Id::new("epoch").unwrap();
        let run = Id::new("run").unwrap();
        let context = ConnCtx::new(
            Id::new("connection").unwrap(),
            RunCtx {
                workspace_run_id: run.clone(),
            },
            Caller::Operator,
        );
        (
            PendingWatch {
                feature,
                id: id.clone(),
                epoch: epoch.clone(),
                context: context.clone(),
                stream: Box::new(Script {
                    replies: replies.into(),
                    pending: wait,
                }),
                marker: terminal(feature, &id, &epoch, &run, Wide(7)),
            },
            context,
        )
    }

    async fn one_final(watch: PendingWatch) -> Push {
        let (tx, mut rx) = mpsc::channel(2);
        tokio::spawn(watch.forward(tx));
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .is_none());
        frame.push.unwrap()
    }

    #[tokio::test]
    async fn stream_end_error_identity_and_run_expiry_retire_each_observer_with_resync() {
        for feature in [Feature::Asides, Feature::Docs, Feature::UsageMetrics] {
            for case in 0..4 {
                let replies = match case {
                    0 => vec![Ok(None)],
                    1 => vec![Err(RpcError::internal("fixture"))],
                    2 => vec![Ok(Some(Push::Resync))],
                    _ => vec![],
                };
                let (watch, context) = pending(feature, replies, true);
                let expected = watch.marker.clone();
                if case == 3 {
                    context.cancellation.cancel();
                }
                assert_eq!(one_final(watch).await, expected, "{feature:?} case{case}");
            }
        }
    }

    #[tokio::test]
    async fn a_new_epoch_is_forwarded_once_without_foreign_watch_metadata() {
        for feature in [Feature::Asides, Feature::Docs, Feature::UsageMetrics] {
            let changed = terminal(
                feature,
                &Id::new("watch").unwrap(),
                &Id::new("new-epoch").unwrap(),
                &Id::new("run").unwrap(),
                Wide(9),
            );
            let (watch, _) = pending(feature, vec![Ok(Some(changed.clone()))], false);
            assert_eq!(one_final(watch).await, changed);
            let foreign = terminal(
                feature,
                &Id::new("foreign").unwrap(),
                &Id::new("new-epoch").unwrap(),
                &Id::new("run").unwrap(),
                Wide(9),
            );
            let (watch, _) = pending(feature, vec![Ok(Some(foreign))], false);
            let expected = watch.marker.clone();
            assert_eq!(one_final(watch).await, expected);
        }
    }

    #[tokio::test]
    async fn physical_close_sends_nothing_and_releases_a_backpressured_terminal_send() {
        let (watch, context) = pending(Feature::Docs, vec![Ok(None)], false);
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(ServerFrame::ok(1, serde_json::json!({})))
            .await
            .unwrap();
        let task = tokio::spawn(watch.forward(tx));
        tokio::task::yield_now().await;
        context.connection_closed.cancel();
        context.cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rx.recv().await.unwrap().id, Some(1));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn slow_reader_receives_small_resync_and_keeps_bounded_forwarding() {
        let push = terminal(
            Feature::Docs,
            &Id::new("watch").unwrap(),
            &Id::new("epoch").unwrap(),
            &Id::new("run").unwrap(),
            Wide(9),
        );
        let (watch, context) = pending(Feature::Docs, vec![Ok(Some(push.clone()))], true);
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(ServerFrame::ok(1, serde_json::json!({})))
            .await
            .unwrap();
        let task = tokio::spawn(watch.forward(tx));
        assert_eq!(rx.recv().await.unwrap().id, Some(1));
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.push, Some(push));
        assert!(
            crew_core::rpc::features::encode_frame(&frame)
                .unwrap()
                .len()
                < 2048
        );
        context.connection_closed.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}
