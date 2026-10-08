//! Observer bookkeeping stays in the shared core, separate from task/agent activity.

use super::*;
use crate::features::{
    usage_query_matches, FeatureAction, FeatureChange, FeatureReadRequest, FeatureResponse,
    FeatureWatchState,
};
use crew_protocol::asides::{AsideAskResult, AsideChanged, AsideGetResult, AsideGoneCause};
use crew_protocol::docs::DocsChanged;
use crew_protocol::features::{Feature, FeatureSession, Id, List};
use crew_protocol::usage_metrics::UsageChanged;

impl ClientState {
    pub(super) fn filter_removed_asides(&self, value: &mut ResourceValue) -> bool {
        let ResourceValue::Feature(value) = value else {
            return true;
        };
        match &mut value.response {
            FeatureResponse::Asides(list) => {
                let rows = std::mem::take(&mut list.rows)
                    .into_vec()
                    .into_iter()
                    .filter(|r| {
                        !self
                            .removed_asides
                            .contains(&(r.workspace_run_id.clone(), r.id.clone()))
                    })
                    .collect();
                list.rows = List::new(rows).expect("filtered bounded list");
            }
            FeatureResponse::AsideDetail(result) => {
                if let AsideGetResult::Available { detail } = result.as_ref() {
                    return !self
                        .removed_asides
                        .contains(&(detail.workspace_run_id.clone(), detail.summary_id.clone()));
                }
            }
            _ => {}
        }
        true
    }
    pub(crate) fn feature_completed(
        &mut self,
        command: &Command,
        context: &FeatureSession,
        response: &FeatureResponse,
        update: &mut StateUpdate,
    ) -> Result<(), ClientError> {
        let root = self.features.session.as_ref().map(|s| &s.connection_epoch);
        let watch = match (command, response) {
            (Command::AsideWatch(q), FeatureResponse::AsideWatch(r))
                if root == Some(&context.connection_epoch)
                    && q.workspace_run_id == r.initial.workspace_run_id
                    && context.workspace_run_id.as_ref() == Some(&q.workspace_run_id) =>
            {
                Some((
                    Feature::Asides,
                    r.watch_id.clone(),
                    r.initial.aside_epoch.clone(),
                    r.initial.revision,
                    FeatureAction::WatchAsides(q.clone()),
                ))
            }
            (Command::DocsWatch(q), FeatureResponse::DocsWatch(r))
                if root == Some(&context.connection_epoch) =>
            {
                Some((
                    Feature::Docs,
                    r.watch_id.clone(),
                    r.initial_index_page.epoch.clone(),
                    r.initial_index_page.revision,
                    FeatureAction::WatchDocs(q.clone()),
                ))
            }
            (Command::UsageWatch(q), FeatureResponse::UsageWatch(r))
                if root == Some(&context.connection_epoch)
                    && usage_query_matches(q, &r.initial.query) =>
            {
                Some((
                    Feature::UsageMetrics,
                    r.watch_id.clone(),
                    r.epoch.clone(),
                    r.initial.revision,
                    FeatureAction::WatchUsage(q.clone()),
                ))
            }
            _ => None,
        };
        if watch.is_none()
            && matches!(
                command,
                Command::AsideWatch(_) | Command::DocsWatch(_) | Command::UsageWatch(_)
            )
        {
            return Err(ClientError::DecodeFailure {
                message: "watch reply scope differs".into(),
            });
        }
        if let Some((feature, watch_id, epoch, revision, query)) = watch {
            if feature != Feature::UsageMetrics {
                if self
                    .feature_watches
                    .iter()
                    .any(|w| w.feature == feature && w.watch_id != watch_id)
                {
                    return Err(ClientError::DecodeFailure {
                        message: "observer watch bound exceeded".into(),
                    });
                }
            } else if !self.feature_watches.iter().any(|w| w.watch_id == watch_id)
                && self
                    .feature_watches
                    .iter()
                    .filter(|w| w.feature == feature)
                    .count()
                    >= 4
            {
                return Err(ClientError::DecodeFailure {
                    message: "metric watch bound exceeded".into(),
                });
            }
            self.feature_watches.retain(|w| w.watch_id != watch_id);
            self.feature_watches.push(FeatureWatchState {
                feature,
                watch_id,
                connection_epoch: context.connection_epoch.clone(),
                epoch,
                revision,
                query,
                initial: response.clone(),
                stale: false,
                change: None,
            });
            update.changes.availability = true;
        }
        match (command, response) {
            (
                Command::AsideUnwatch(q) | Command::DocsUnwatch(q) | Command::UsageUnwatch(q),
                FeatureResponse::Unwatched(r),
            ) if q.watch_id == r.watch_id => {
                self.feature_watches.retain(|w| {
                    w.watch_id != r.watch_id || w.connection_epoch != context.connection_epoch
                });
                update.changes.availability = true;
            }
            (Command::AsideRemove(q), FeatureResponse::AsideMutation(r))
                if q.workspace_run_id == r.workspace_run_id && q.id == r.id =>
            {
                self.purge_aside(&r.workspace_run_id, &r.id, AsideGoneCause::Removed);
                update.changes.availability = true;
            }
            (Command::AsideAsk(q), FeatureResponse::AsideAsked(r)) => {
                let matches = match r.as_ref() {
                    AsideAskResult::Accepted { summary, .. } => {
                        summary.id == q.request_id && summary.workspace_run_id == q.workspace_run_id
                    }
                    AsideAskResult::Gone {
                        id,
                        workspace_run_id,
                        ..
                    } => id == &q.request_id && workspace_run_id == &q.workspace_run_id,
                };
                if !matches {
                    return Err(ClientError::DecodeFailure {
                        message: "aside reply identity differs".into(),
                    });
                }
                self.invalidate_feature(Feature::Asides);
                update.changes.availability = true;
            }
            (Command::AsideCancel(q), FeatureResponse::AsideMutation(r))
                if q.workspace_run_id == r.workspace_run_id && q.id == r.id =>
            {
                self.invalidate_feature(Feature::Asides);
                update.changes.availability = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn invalidate_feature(&mut self, feature: Feature) {
        for resource in self.resources.values_mut() {
            if matches!(&resource.key.options,ReadOptions::Feature(read) if read.command().required_feature()==Some(feature))
            {
                resource.status = ResourceStatus::Stale;
            }
        }
    }

    pub(crate) fn clear_aside_values(&mut self) {
        for resource in self.resources.values_mut() {
            if matches!(&resource.key.options,ReadOptions::Feature(q) if q.command().required_feature()==Some(Feature::Asides))
            {
                resource.value = None;
                resource.status = ResourceStatus::Stale;
            }
        }
        self.pending_reads.retain(|_,p|!matches!(&p.key.options,ReadOptions::Feature(q) if q.command().required_feature()==Some(Feature::Asides)));
        for watch in &mut self.feature_watches {
            if let FeatureResponse::AsideWatch(reply) = &mut watch.initial {
                reply.initial.rows = List::default();
                watch.stale = true;
            }
        }
    }

    pub(crate) fn purge_aside(&mut self, run: &Id, id: &Id, cause: AsideGoneCause) {
        if cause == AsideGoneCause::Removed {
            // Bound tombstones without allowing an older outstanding read to revive
            // a forgotten id. Reset cached asides/pending reads before rotating.
            if self.removed_asides.len() >= 1024 {
                self.resources.retain(|_,r|!matches!(&r.key.options,ReadOptions::Feature(q) if q.command().required_feature()==Some(Feature::Asides)));
                self.pending_reads.retain(|_,p|!matches!(&p.key.options,ReadOptions::Feature(q) if q.command().required_feature()==Some(Feature::Asides)));
                self.removed_asides.clear();
            }
            self.removed_asides.insert((run.clone(), id.clone()));
        }
        for resource in self.resources.values_mut() {
            let Some(ResourceValue::Feature(value)) = &mut resource.value else {
                continue;
            };
            match &mut value.response {
                FeatureResponse::Asides(list) if &list.workspace_run_id == run => {
                    let rows = std::mem::take(&mut list.rows)
                        .into_vec()
                        .into_iter()
                        .filter(|r| &r.id != id)
                        .collect();
                    list.rows = List::new(rows).expect("filtered bounded list");
                    resource.status = ResourceStatus::Stale;
                }
                FeatureResponse::AsideDetail(detail) => {
                    let same = match detail.as_ref() {
                        AsideGetResult::Available { detail } => {
                            &detail.workspace_run_id == run && &detail.summary_id == id
                        }
                        AsideGetResult::Gone {
                            workspace_run_id,
                            id: old,
                            ..
                        } => workspace_run_id == run && old == id,
                    };
                    if same {
                        resource.status = ResourceStatus::Stale;
                        resource.error = Some(ClientReason {
                            details: None,
                            code: match cause {
                                AsideGoneCause::Removed => "removed",
                                AsideGoneCause::Evicted => "evicted",
                                AsideGoneCause::DaemonRestart => "daemon_restart",
                                AsideGoneCause::RunReset => "run_reset",
                                AsideGoneCause::Unknown => "gone",
                            }
                            .into(),
                            message: "Aside is no longer retained.".into(),
                        });
                        if !matches!(
                            cause,
                            AsideGoneCause::Evicted | AsideGoneCause::DaemonRestart
                        ) {
                            resource.value = None;
                        }
                    }
                }
                _ => {}
            }
        }
        for watch in &mut self.feature_watches {
            if let FeatureResponse::AsideWatch(r) = &mut watch.initial {
                if &r.initial.workspace_run_id == run {
                    let rows = std::mem::take(&mut r.initial.rows)
                        .into_vec()
                        .into_iter()
                        .filter(|r| &r.id != id)
                        .collect();
                    r.initial.rows = List::new(rows).expect("filtered bounded list");
                }
            }
        }
        if cause == AsideGoneCause::Removed {
            self.pending_reads.retain(|_,pending|!matches!(&pending.key.options,ReadOptions::Feature(read) if matches!(&read.request,FeatureReadRequest::AsideDetail(q) if &q.id==id&&&q.workspace_run_id==run)));
        }
    }

    pub(super) fn aside_changed(
        &mut self,
        change: AsideChanged,
        update: &mut StateUpdate,
    ) -> Result<(), ClientError> {
        let Some(index) = self
            .feature_watches
            .iter()
            .position(|w| w.feature == Feature::Asides && w.watch_id == change.watch_id)
        else {
            return Ok(());
        };
        let watch = &mut self.feature_watches[index];
        if !matches!(&watch.query,FeatureAction::WatchAsides(q) if q.workspace_run_id==change.workspace_run_id)
        {
            return Ok(());
        }
        if watch.epoch == change.aside_epoch && change.to_revision <= watch.revision {
            return Ok(());
        }
        let gap = watch.epoch != change.aside_epoch
            || change.from_revision != watch.revision
            || change.resync_required;
        watch.stale = true;
        watch.change = Some(FeatureChange::Asides(change.clone()));
        watch.revision = change.to_revision;
        if gap {
            self.clear_aside_values();
        }
        for gone in change.gone.iter() {
            self.purge_aside(&change.workspace_run_id, &gone.id, gone.cause);
        }
        self.invalidate_feature(Feature::Asides);
        update.changes.availability = true;
        Ok(())
    }

    pub(super) fn docs_changed(
        &mut self,
        change: DocsChanged,
        update: &mut StateUpdate,
    ) -> Result<(), ClientError> {
        let Some(watch) = self
            .feature_watches
            .iter_mut()
            .find(|w| w.feature == Feature::Docs && w.watch_id == change.watch_id)
        else {
            return Ok(());
        };
        if watch.epoch == change.epoch && change.to_revision <= watch.revision {
            return Ok(());
        }
        if watch.epoch != change.epoch
            || change.from_revision != watch.revision
            || change.resync_required
        {
            watch.stale = true;
        }
        watch.revision = change.to_revision;
        watch.stale = true;
        watch.change = Some(FeatureChange::Docs(change.clone()));
        let selected = match &watch.query {
            FeatureAction::WatchDocs(q) => q.selected.clone(),
            _ => unreachable!(),
        };
        // Expiry includes unselected retained views. Never silently select a
        // newer version. A selected expired snapshot can remain visibly stale.
        for resource in self.resources.values_mut() {
            if let Some(ResourceValue::Feature(v)) = &resource.value {
                if let FeatureResponse::Document(r) = &v.response {
                    if let crew_protocol::docs::DocReadResult::Available(doc) = r.as_ref() {
                        let expired = change.expired_seen_versions.contains(&doc.version.id);
                        let denied = change
                            .removed_doc_ids
                            .contains(&doc.version.document.doc_id)
                            || change.changed_selected.iter().any(|s| {
                                s.document == doc.version.document
                                    && matches!(
                                        s.kind,
                                        crew_protocol::docs::DocChangeKind::Removed
                                            | crew_protocol::docs::DocChangeKind::Policy
                                    )
                            });
                        if expired || denied {
                            let retained = !denied && selected.contains(&doc.version.document);
                            resource.error = Some(ClientReason {
                                details: None,
                                code: if denied {
                                    "unavailable"
                                } else {
                                    "version_expired"
                                }
                                .into(),
                                message: "The captured document version is no longer available."
                                    .into(),
                            });
                            if !retained {
                                resource.value = None;
                            }
                        }
                    }
                }
            }
        }
        // Outstanding old reads cannot publish a revoked/expired version.
        self.pending_reads.retain(|_, p| !matches!(&p.key.options,ReadOptions::Feature(q) if q.command().required_feature()==Some(Feature::Docs)));
        self.invalidate_feature(Feature::Docs);
        update.changes.availability = true;
        Ok(())
    }

    pub(super) fn usage_changed(
        &mut self,
        change: UsageChanged,
        update: &mut StateUpdate,
    ) -> Result<(), ClientError> {
        let Some(watch) = self
            .feature_watches
            .iter_mut()
            .find(|w| w.feature == Feature::UsageMetrics && w.watch_id == change.watch_id)
        else {
            return Ok(());
        };
        if !matches!(&watch.query,FeatureAction::WatchUsage(q) if q.scope.workspace_run_id==change.workspace_run_id)
        {
            return Ok(());
        }
        if watch.epoch == change.epoch && change.to_revision <= watch.revision {
            return Ok(());
        }
        if watch.epoch != change.epoch
            || change.from_revision != watch.revision
            || change.resync_required
        {
            watch.stale = true;
        }
        watch.revision = change.to_revision;
        watch.stale = true;
        watch.change = Some(FeatureChange::Usage(change));
        self.invalidate_feature(Feature::UsageMetrics);
        update.changes.availability = true;
        Ok(())
    }
}
