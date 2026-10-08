//! Typed feature replies and channel evidence. No feature body crosses FFI as JSON.

use crew_protocol::features::{Feature, FeatureSession, FeatureVersions, Id, FEATURE_VERSION};
use serde::{Deserialize, Serialize};

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
