//! Independent observer forwarders. A domain registers before capturing its
//! initial result; this forwarder starts only once that result is queued.

use crew_core::rpc::features::{Feature, Id, MAX_PAGE_BYTES};
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
}

impl PendingWatch {
    pub fn new(
        feature: Feature,
        result: &Value,
        context: ConnCtx,
        service: &dyn FeatureService,
    ) -> Result<Self, RpcError> {
        let field = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::new("internal", "invalid feature watch result"))
        };
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
        let stream = service.watch_stream(context.clone(), id.clone())?;
        Ok(Self {
            feature,
            id,
            epoch,
            context,
            stream,
        })
    }

    pub async fn forward(mut self, tx: mpsc::Sender<ServerFrame>) {
        loop {
            let next = tokio::select! {
                _ = self.context.cancellation.cancelled() => break,
                next = self.stream.next() => next,
            };
            let Ok(Some(mut push)) = next else { break };
            let Some((feature, id, epoch)) = identity(&push) else {
                break;
            };
            if feature != self.feature || id != &self.id || epoch != &self.epoch {
                break;
            }
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
                        _ = self.context.cancellation.cancelled() => break,
                        result = tx.send(marker) => if result.is_err() { break; },
                    }
                }
            }
        }
    }
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
