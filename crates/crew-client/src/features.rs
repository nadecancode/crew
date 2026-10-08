//! Typed feature replies and channel evidence. No feature body crosses FFI as JSON.

use crew_protocol::features::{Feature, FeatureSession, FeatureVersions, Id, FEATURE_VERSION};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureAvailability {
    pub session: Option<FeatureSession>,
    pub reason: Option<crate::ClientReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum FeatureRejection {
    AsideAdmission(crew_protocol::asides::AsideAdmissionFailure),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "params", rename_all = "snake_case")]
pub enum FeatureAction {
    AskAside(crew_protocol::asides::AsideAsk),
    CancelAside(crew_protocol::asides::AsideCancel),
    RemoveAside(crew_protocol::asides::AsideRemove),
    WatchAsides(crew_protocol::asides::AsideList),
    WatchDocs(crew_protocol::docs::DocsWatch),
    WatchUsage(crew_protocol::usage_metrics::UsageQuery),
    UnwatchAsides(crew_protocol::features::Unwatch),
    UnwatchDocs(crew_protocol::features::Unwatch),
    UnwatchUsage(crew_protocol::features::Unwatch),
}

impl FeatureAction {
    pub fn command(&self) -> crate::Command {
        match self {
            Self::AskAside(p) => crate::Command::AsideAsk(p.clone()),
            Self::CancelAside(p) => crate::Command::AsideCancel(p.clone()),
            Self::RemoveAside(p) => crate::Command::AsideRemove(p.clone()),
            Self::WatchAsides(p) => crate::Command::AsideWatch(p.clone()),
            Self::WatchDocs(p) => crate::Command::DocsWatch(p.clone()),
            Self::WatchUsage(p) => crate::Command::UsageWatch(Box::new(p.clone())),
            Self::UnwatchAsides(p) => crate::Command::AsideUnwatch(p.clone()),
            Self::UnwatchDocs(p) => crate::Command::DocsUnwatch(p.clone()),
            Self::UnwatchUsage(p) => crate::Command::UsageUnwatch(p.clone()),
        }
    }
    pub fn observer(&self) -> bool {
        !matches!(
            self,
            Self::AskAside(_) | Self::CancelAside(_) | Self::RemoveAside(_)
        )
    }
    pub fn current_run(&self) -> Option<&Id> {
        match self {
            Self::AskAside(p) => Some(&p.workspace_run_id),
            Self::CancelAside(p) => Some(&p.workspace_run_id),
            Self::RemoveAside(p) => Some(&p.workspace_run_id),
            Self::WatchAsides(p) => Some(&p.workspace_run_id),
            _ => None,
        }
    }
    pub fn validate(&self) -> Result<(), crate::ClientError> {
        let invalid = |message: &str| crate::ClientError::InvalidInput {
            field: "feature_action".into(),
            reason: message.into(),
        };
        match self {
            Self::AskAside(p) => p
                .validate()
                .map_err(|_| invalid("invalid aside question or targets")),
            Self::WatchAsides(p) => p.page.validate(50).map_err(invalid),
            Self::WatchUsage(p) => p.validate(200).map_err(invalid),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureWatchState {
    pub feature: Feature,
    pub watch_id: Id,
    pub connection_epoch: Id,
    pub epoch: Id,
    pub revision: crew_protocol::features::Wide,
    pub query: FeatureAction,
    pub initial: FeatureResponse,
    pub stale: bool,
    /// Latest bounded invalidation; resources are invalidated in the core even
    /// when a renderer has not read this notification yet.
    pub change: Option<FeatureChange>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum FeatureChange {
    Asides(crew_protocol::asides::AsideChanged),
    Docs(crew_protocol::docs::DocsChanged),
    Usage(crew_protocol::usage_metrics::UsageChanged),
}

/// Validate the v1 effective query without deriving or repricing its results.
pub fn usage_query_matches(
    requested: &crew_protocol::usage_metrics::UsageQuery,
    resolved: &crew_protocol::usage_metrics::UsageQuery,
    series: bool,
) -> bool {
    let Some(book) = &resolved.price_book_id else {
        return false;
    };
    requested
        .resolved_shape(book.clone(), if series { 720 } else { 200 }, series)
        .is_ok_and(|expected| &expected == resolved)
}

impl FeatureAvailability {
    pub fn supports(&self, feature: Feature) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.accepted.get(feature.name()) == Some(&FEATURE_VERSION))
    }
}

impl crate::Command {
    pub fn required_feature(&self) -> Option<Feature> {
        crew_protocol::features::feature_method(self.kind().method()).and_then(|m| m.feature)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum FeatureResponse {
    FeatureSession(Box<crew_protocol::features::FeatureSession>),
    EpicBoard(Box<crew_protocol::epics::EpicBoard>),
    EpicDetail(Box<crew_protocol::epics::EpicDetail>),
    Tasks(Box<crew_protocol::epics::TasksQueryResult>),
    TaskCard(Box<crew_protocol::epics::ScopedTaskCard>),
    AsideMembers(Box<crew_protocol::asides::AsideMembers>),
    Asides(Box<crew_protocol::asides::AsideListResult>),
    AsideDetail(Box<crew_protocol::asides::AsideGetResult>),
    AsideAsked(Box<crew_protocol::asides::AsideAskResult>),
    AsideMutation(Box<crew_protocol::asides::AsideMutationResult>),
    AsideWatch(Box<crew_protocol::asides::AsideWatchResult>),
    Unwatched(Box<crew_protocol::features::UnwatchResult>),
    DocsIndex(Box<crew_protocol::docs::DocIndexPage>),
    Document(Box<crew_protocol::docs::DocReadResult>),
    DocAsset(Box<crew_protocol::docs::DocAssetResult>),
    DocHistory(Box<crew_protocol::docs::DocHistoryResult>),
    DocDiff(Box<crew_protocol::docs::DocDiffResult>),
    DocsWatch(Box<crew_protocol::docs::DocsWatchResult>),
    UsageSummary(Box<crew_protocol::usage_metrics::UsageSummary>),
    UsageSeries(Box<crew_protocol::usage_metrics::UsageSeries>),
    UsageBreakdown(Box<crew_protocol::usage_metrics::UsageBreakdown>),
    UsageObservations(Box<crew_protocol::usage_metrics::UsageObservations>),
    UsageRuns(Box<crew_protocol::usage_metrics::UsageRuns>),
    UsagePrices(Box<crew_protocol::usage_metrics::PriceBook>),
    UsageWatch(Box<crew_protocol::usage_metrics::UsageWatchResult>),
}

/// Public opaque lease; the driver keeps the actual channel mapping private.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocLease {
    pub connection_epoch: Id,
    pub docs_epoch: Id,
}

/// Always negotiate a fixed set before ordinary traffic, on each eligible channel.
pub fn requested_versions() -> FeatureVersions {
    FeatureVersions::try_from(
        Feature::ALL
            .into_iter()
            .map(|f| (f.name().to_string(), FEATURE_VERSION))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .expect("fixed version map")
}

pub fn valid_negotiation(session: &FeatureSession) -> bool {
    let requested = requested_versions();
    session
        .accepted
        .iter()
        .all(|(name, version)| requested.get(name) == Some(version))
        && session
            .unavailable
            .keys()
            .all(|name| requested.contains_key(name) && !session.accepted.contains_key(name))
}

/// Closed read catalogue. Canonical keys include query and lease identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureResourceKind {
    EpicBoard,
    EpicDetail,
    Tasks,
    TaskCard,
    AsideMembers,
    Asides,
    AsideDetail,
    DocsIndex,
    Document,
    DocAsset,
    DocHistory,
    DocDiff,
    UsageSummary,
    UsageSeries,
    UsageBreakdown,
    UsageObservations,
    UsageRuns,
    UsagePrices,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "params", rename_all = "snake_case")]
pub enum FeatureReadRequest {
    EpicBoard(crew_protocol::features::RunScope),
    EpicDetail(crew_protocol::epics::EpicGet),
    Tasks(crew_protocol::epics::TasksQuery),
    TaskCard(crew_protocol::epics::TaskGetScoped),
    AsideMembers(crew_protocol::asides::AsideList),
    Asides(crew_protocol::asides::AsideList),
    AsideDetail(crew_protocol::asides::AsideGet),
    DocsIndex(crew_protocol::features::PageRequest),
    Document(crew_protocol::docs::DocsRead),
    DocAsset(crew_protocol::docs::DocsAsset),
    DocHistory(crew_protocol::docs::DocsHistory),
    DocDiff(crew_protocol::docs::DocsDiff),
    UsageSummary(crew_protocol::usage_metrics::UsageQuery),
    UsageSeries(crew_protocol::usage_metrics::UsageQuery),
    UsageBreakdown(crew_protocol::usage_metrics::UsageQuery),
    UsageObservations(crew_protocol::usage_metrics::UsageQuery),
    UsageRuns(crew_protocol::features::PageRequest),
    UsagePrices(crew_protocol::usage_metrics::UsagePrices),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureRead {
    pub request: FeatureReadRequest,
    pub lease: Option<DocLease>,
}

impl std::hash::Hash for FeatureRead {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // All inputs are deterministic, typed, integer/string records; no JSON map
        // ordering or float round-trip enters resource identity.
        std::hash::Hash::hash(
            &serde_json::to_vec(self).expect("typed feature read"),
            state,
        );
    }
}

impl FeatureRead {
    pub fn kind(&self) -> FeatureResourceKind {
        match &self.request {
            FeatureReadRequest::EpicBoard(_) => FeatureResourceKind::EpicBoard,
            FeatureReadRequest::EpicDetail(_) => FeatureResourceKind::EpicDetail,
            FeatureReadRequest::Tasks(_) => FeatureResourceKind::Tasks,
            FeatureReadRequest::TaskCard(_) => FeatureResourceKind::TaskCard,
            FeatureReadRequest::AsideMembers(_) => FeatureResourceKind::AsideMembers,
            FeatureReadRequest::Asides(_) => FeatureResourceKind::Asides,
            FeatureReadRequest::AsideDetail(_) => FeatureResourceKind::AsideDetail,
            FeatureReadRequest::DocsIndex(_) => FeatureResourceKind::DocsIndex,
            FeatureReadRequest::Document(_) => FeatureResourceKind::Document,
            FeatureReadRequest::DocAsset(_) => FeatureResourceKind::DocAsset,
            FeatureReadRequest::DocHistory(_) => FeatureResourceKind::DocHistory,
            FeatureReadRequest::DocDiff(_) => FeatureResourceKind::DocDiff,
            FeatureReadRequest::UsageSummary(_) => FeatureResourceKind::UsageSummary,
            FeatureReadRequest::UsageSeries(_) => FeatureResourceKind::UsageSeries,
            FeatureReadRequest::UsageBreakdown(_) => FeatureResourceKind::UsageBreakdown,
            FeatureReadRequest::UsageObservations(_) => FeatureResourceKind::UsageObservations,
            FeatureReadRequest::UsageRuns(_) => FeatureResourceKind::UsageRuns,
            FeatureReadRequest::UsagePrices(_) => FeatureResourceKind::UsagePrices,
        }
    }
    pub fn command(&self) -> crate::Command {
        match &self.request {
            FeatureReadRequest::EpicBoard(p) => crate::Command::EpicsList(p.clone()),
            FeatureReadRequest::EpicDetail(p) => crate::Command::EpicGet(p.clone()),
            FeatureReadRequest::Tasks(p) => crate::Command::TasksQuery(p.clone()),
            FeatureReadRequest::TaskCard(p) => crate::Command::TaskGetScoped(p.clone()),
            FeatureReadRequest::AsideMembers(p) => crate::Command::AsideMembers(p.clone()),
            FeatureReadRequest::Asides(p) => crate::Command::AsideList(p.clone()),
            FeatureReadRequest::AsideDetail(p) => crate::Command::AsideGet(p.clone()),
            FeatureReadRequest::DocsIndex(p) => crate::Command::DocsIndex(p.clone()),
            FeatureReadRequest::Document(p) => crate::Command::DocsRead(p.clone()),
            FeatureReadRequest::DocAsset(p) => crate::Command::DocsAsset(p.clone()),
            FeatureReadRequest::DocHistory(p) => crate::Command::DocsHistory(p.clone()),
            FeatureReadRequest::DocDiff(p) => crate::Command::DocsDiff(p.clone()),
            FeatureReadRequest::UsageSummary(p) => {
                crate::Command::UsageSummary(Box::new(p.clone()))
            }
            FeatureReadRequest::UsageSeries(p) => crate::Command::UsageSeries(Box::new(p.clone())),
            FeatureReadRequest::UsageBreakdown(p) => {
                crate::Command::UsageBreakdown(Box::new(p.clone()))
            }
            FeatureReadRequest::UsageObservations(p) => {
                crate::Command::UsageObservations(Box::new(p.clone()))
            }
            FeatureReadRequest::UsageRuns(p) => crate::Command::UsageRuns(p.clone()),
            FeatureReadRequest::UsagePrices(p) => crate::Command::UsagePrices(p.clone()),
        }
    }
    pub fn validate(&self) -> Result<(), crate::ClientError> {
        let invalid = |message: &str| crate::ClientError::InvalidInput {
            field: "feature_read".into(),
            reason: message.into(),
        };
        let docs = self.command().required_feature() == Some(Feature::Docs);
        if self.lease.is_some() && !docs {
            return Err(invalid("a document lease cannot scope another feature"));
        }
        if docs
            && self.lease.is_none()
            && !matches!(&self.request,FeatureReadRequest::DocsIndex(p) if p.cursor.is_none())
        {
            return Err(invalid("read a document index first and retain its lease"));
        }
        use FeatureReadRequest::*;
        let result = match &self.request {
            EpicBoard(_) => Ok(()),
            EpicDetail(p) => crew_protocol::epics::validate_id(&p.id),
            Tasks(p) => match &p.epic {
                crew_protocol::epics::EpicFilter::Id(id) => crew_protocol::epics::validate_id(id),
                _ => Ok(()),
            },
            TaskCard(p) => {
                p.id.as_ref()
                    .ok_or("name the task explicitly")
                    .and_then(|id| crew_protocol::epics::validate_id(id))
            }
            AsideMembers(p) => p.page.validate(200),
            Asides(p) => p.page.validate(50),
            AsideDetail(_) => Ok(()),
            DocsIndex(p) | UsageRuns(p) => p.validate(200),
            DocHistory(p) => p.page.validate(50),
            DocAsset(p) => p.validate(),
            UsageSummary(p) | UsageBreakdown(p) | UsageObservations(p) => p.validate(200),
            UsageSeries(p) => p.validate(720),
            Document(_) | DocDiff(_) | UsagePrices(_) => Ok(()),
        };
        result.map_err(invalid)
    }
    pub fn current_run(&self) -> Option<&Id> {
        match &self.request {
            FeatureReadRequest::EpicBoard(p) => Some(&p.workspace_run_id),
            FeatureReadRequest::EpicDetail(p) => Some(&p.workspace_run_id),
            FeatureReadRequest::Tasks(p) => Some(&p.workspace_run_id),
            FeatureReadRequest::TaskCard(p) => Some(&p.workspace_run_id),
            FeatureReadRequest::AsideMembers(p) | FeatureReadRequest::Asides(p) => {
                Some(&p.workspace_run_id)
            }
            FeatureReadRequest::AsideDetail(p) => Some(&p.workspace_run_id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureResourceValue {
    pub connection_epoch: Id,
    pub lease: Option<DocLease>,
    pub response: FeatureResponse,
}

impl FeatureRead {
    pub fn value(
        &self,
        context: &FeatureSession,
        response: &FeatureResponse,
    ) -> Option<FeatureResourceValue> {
        let feature = self.command().required_feature()?;
        if context.accepted.get(feature.name()) != Some(&FEATURE_VERSION)
            || response.resource_kind() != Some(self.kind())
            || self
                .lease
                .as_ref()
                .is_some_and(|l| l.connection_epoch != context.connection_epoch)
        {
            return None;
        }
        use FeatureReadRequest as Q;
        use FeatureResponse as R;
        let matches = match (&self.request, response) {
            (Q::EpicBoard(q), R::EpicBoard(r)) => q.workspace_run_id == r.generation,
            (Q::EpicDetail(q), R::EpicDetail(r)) => {
                q.workspace_run_id == r.scope.workspace_run_id && q.id == r.epic.id
            }
            (Q::Tasks(q), R::Tasks(r)) => q.workspace_run_id == r.workspace_run_id,
            (Q::TaskCard(q), R::TaskCard(r)) => {
                q.workspace_run_id == r.workspace_run_id && q.id.as_ref() == Some(&r.card.task.id)
            }
            (Q::AsideMembers(q), R::AsideMembers(r)) => q.workspace_run_id == r.workspace_run_id,
            (Q::Asides(q), R::Asides(r)) => q.workspace_run_id == r.workspace_run_id,
            (Q::AsideDetail(q), R::AsideDetail(r)) => match r.as_ref() {
                crew_protocol::asides::AsideGetResult::Available { detail } => {
                    q.workspace_run_id == detail.workspace_run_id
                        && q.id == detail.summary_id
                        && match (&q.agent, &detail.answer_meta) {
                            (Some(a), Some(b)) => a == &b.target,
                            (None, None) => detail.answer_text.is_none(),
                            _ => false,
                        }
                }
                crew_protocol::asides::AsideGetResult::Gone {
                    id,
                    workspace_run_id,
                    ..
                } => &q.id == id && &q.workspace_run_id == workspace_run_id,
            },
            (Q::Document(q), R::Document(r)) => match r.as_ref() {
                crew_protocol::docs::DocReadResult::Available(s) => {
                    q.document == s.version.document
                        && match &q.version {
                            crew_protocol::docs::DocVersionSelector::Current => true,
                            crew_protocol::docs::DocVersionSelector::Id(id) => id == &s.version.id,
                        }
                }
                crew_protocol::docs::DocReadResult::Unavailable(f) => {
                    f.document.as_ref().is_none_or(|d| d == &q.document)
                }
            },
            (Q::DocAsset(q), R::DocAsset(r)) => match r.as_ref() {
                crew_protocol::docs::DocAssetResult::Available(c) => {
                    q.snapshot_id == c.snapshot_id
                        && q.asset_id == c.asset_id
                        && q.offset == c.offset
                        && (c.data_base64.len() / 4 * 3
                            - c.data_base64
                                .as_bytes()
                                .iter()
                                .rev()
                                .take_while(|b| **b == b'=')
                                .count())
                            <= q.max_bytes as usize
                }
                crew_protocol::docs::DocAssetResult::Unavailable(_) => true,
            },
            (Q::DocHistory(q), R::DocHistory(r)) => match r.as_ref() {
                crew_protocol::docs::DocHistoryResult::Available(p) => {
                    q.doc_id == p.doc_id
                        && q.source_ids.as_ref().is_none_or(|ids| {
                            p.captured_heads.iter().all(|h| ids.contains(&h.source_id))
                        })
                }
                _ => true,
            },
            (Q::DocDiff(q), R::DocDiff(r)) => match r.as_ref() {
                crew_protocol::docs::DocDiffResult::Available(p) => {
                    q.left == p.left.id && q.right == p.right.id
                }
                _ => true,
            },
            (Q::UsageSummary(q), R::UsageSummary(r)) => usage_query_matches(q, &r.query, false),
            (Q::UsageSeries(q), R::UsageSeries(r)) => usage_query_matches(q, &r.query, true),
            (Q::UsageBreakdown(q), R::UsageBreakdown(r)) => usage_query_matches(q, &r.query, false),
            (Q::UsageObservations(q), R::UsageObservations(r)) => {
                usage_query_matches(q, &r.query, false)
            }
            (Q::UsagePrices(q), R::UsagePrices(r)) => {
                q.price_book_id.as_ref().is_none_or(|id| id == &r.id)
            }
            (Q::DocsIndex(_), R::DocsIndex(_)) | (Q::UsageRuns(_), R::UsageRuns(_)) => true,
            _ => false,
        };
        if !matches {
            return None;
        }
        let lease = if feature == Feature::Docs {
            let epoch = match response {
                R::DocsIndex(p) => Some(p.epoch.clone()),
                R::Document(r) => match r.as_ref() {
                    crew_protocol::docs::DocReadResult::Available(s) => Some(s.epoch.clone()),
                    _ => None,
                },
                R::DocHistory(r) => match r.as_ref() {
                    crew_protocol::docs::DocHistoryResult::Available(p) => Some(p.epoch.clone()),
                    _ => None,
                },
                _ => None,
            }
            .or_else(|| self.lease.as_ref().map(|l| l.docs_epoch.clone()))?;
            let recapture = matches!(&self.request,Q::DocsIndex(p) if p.cursor.is_none());
            if !recapture && self.lease.as_ref().is_some_and(|l| l.docs_epoch != epoch) {
                return None;
            }
            Some(DocLease {
                connection_epoch: context.connection_epoch.clone(),
                docs_epoch: epoch,
            })
        } else {
            None
        };
        Some(FeatureResourceValue {
            connection_epoch: context.connection_epoch.clone(),
            lease,
            response: response.clone(),
        })
    }
}

impl FeatureResponse {
    pub fn resource_kind(&self) -> Option<FeatureResourceKind> {
        match self {
            Self::EpicBoard(_) => Some(FeatureResourceKind::EpicBoard),
            Self::EpicDetail(_) => Some(FeatureResourceKind::EpicDetail),
            Self::Tasks(_) => Some(FeatureResourceKind::Tasks),
            Self::TaskCard(_) => Some(FeatureResourceKind::TaskCard),
            Self::AsideMembers(_) => Some(FeatureResourceKind::AsideMembers),
            Self::Asides(_) => Some(FeatureResourceKind::Asides),
            Self::AsideDetail(_) => Some(FeatureResourceKind::AsideDetail),
            Self::DocsIndex(_) => Some(FeatureResourceKind::DocsIndex),
            Self::Document(_) => Some(FeatureResourceKind::Document),
            Self::DocAsset(_) => Some(FeatureResourceKind::DocAsset),
            Self::DocHistory(_) => Some(FeatureResourceKind::DocHistory),
            Self::DocDiff(_) => Some(FeatureResourceKind::DocDiff),
            Self::UsageSummary(_) => Some(FeatureResourceKind::UsageSummary),
            Self::UsageSeries(_) => Some(FeatureResourceKind::UsageSeries),
            Self::UsageBreakdown(_) => Some(FeatureResourceKind::UsageBreakdown),
            Self::UsageObservations(_) => Some(FeatureResourceKind::UsageObservations),
            Self::UsageRuns(_) => Some(FeatureResourceKind::UsageRuns),
            Self::UsagePrices(_) => Some(FeatureResourceKind::UsagePrices),
            _ => None,
        }
    }
}
