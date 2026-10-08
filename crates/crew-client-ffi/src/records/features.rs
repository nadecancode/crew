//! Typed feature mirrors. Remote derives enforce field/variant parity at compile time.
//! Bounded primitives keep validation on every native-to-core conversion.

use super::shared::{ClientReason, RuntimeKind, Task, TaskCard};

pub use crew_client::features::DocLease;
pub use crew_client::features::FeatureAction;
pub use crew_client::features::FeatureAvailability;
pub use crew_client::features::FeatureChange;
pub use crew_client::features::FeatureRead;
pub use crew_client::features::FeatureReadRequest;
pub use crew_client::features::FeatureRejection;
pub use crew_client::features::FeatureResourceKind;
pub use crew_client::features::FeatureResourceValue;
pub use crew_client::features::FeatureResponse;
pub use crew_client::features::FeatureWatchState;
pub use crew_protocol::asides::AsideAdmissionFailure;
pub use crew_protocol::asides::AsideAnswerMeta;
pub use crew_protocol::asides::AsideAnswerReason;
pub use crew_protocol::asides::AsideAsk;
pub use crew_protocol::asides::AsideAskResult;
pub use crew_protocol::asides::AsideAvailability;
pub use crew_protocol::asides::AsideChanged;
pub use crew_protocol::asides::AsideCounts;
pub use crew_protocol::asides::AsideDetail;
pub use crew_protocol::asides::AsideGet;
pub use crew_protocol::asides::AsideGetResult;
pub use crew_protocol::asides::AsideGone;
pub use crew_protocol::asides::AsideGoneCause;
pub use crew_protocol::asides::AsideList;
pub use crew_protocol::asides::AsideListResult;
pub use crew_protocol::asides::AsideMember;
pub use crew_protocol::asides::AsideMembers;
pub use crew_protocol::asides::AsideMutationResult;
pub use crew_protocol::asides::AsideRemove;
pub use crew_protocol::asides::AsideStatus;
pub use crew_protocol::asides::AsideSummary;
pub use crew_protocol::asides::AsideTarget;
pub use crew_protocol::asides::AsideUnavailableReason;
pub use crew_protocol::asides::AsideUsage;
pub use crew_protocol::asides::AsideWatchResult;
pub use crew_protocol::docs::Alignment;
pub use crew_protocol::docs::Block;
pub use crew_protocol::docs::BlockedImageKind;
pub use crew_protocol::docs::DocAssetChunkData;
pub use crew_protocol::docs::DocAssetResult;
pub use crew_protocol::docs::DocBlock;
pub use crew_protocol::docs::DocCapturedHead;
pub use crew_protocol::docs::DocChangeKind;
pub use crew_protocol::docs::DocComparison;
pub use crew_protocol::docs::DocDiff;
pub use crew_protocol::docs::DocDiffResult;
pub use crew_protocol::docs::DocEntry;
pub use crew_protocol::docs::DocFailure;
pub use crew_protocol::docs::DocFailureCode;
pub use crew_protocol::docs::DocHeading;
pub use crew_protocol::docs::DocHistoryPage;
pub use crew_protocol::docs::DocHistoryResult;
pub use crew_protocol::docs::DocHistoryRow;
pub use crew_protocol::docs::DocImage;
pub use crew_protocol::docs::DocImageReason;
pub use crew_protocol::docs::DocImageState;
pub use crew_protocol::docs::DocIndexPage;
pub use crew_protocol::docs::DocPhase;
pub use crew_protocol::docs::DocReadResult;
pub use crew_protocol::docs::DocRef;
pub use crew_protocol::docs::DocSelectedChange;
pub use crew_protocol::docs::DocSnapshotData;
pub use crew_protocol::docs::DocSource;
pub use crew_protocol::docs::DocSourceKind;
pub use crew_protocol::docs::DocSourceState;
pub use crew_protocol::docs::DocState;
pub use crew_protocol::docs::DocVariant;
pub use crew_protocol::docs::DocVersionData;
pub use crew_protocol::docs::DocVersionKind;
pub use crew_protocol::docs::DocVersionSelector;
pub use crew_protocol::docs::DocWatchMode;
pub use crew_protocol::docs::DocsAsset;
pub use crew_protocol::docs::DocsChanged;
pub use crew_protocol::docs::DocsDiff;
pub use crew_protocol::docs::DocsHistory;
pub use crew_protocol::docs::DocsRead;
pub use crew_protocol::docs::DocsWatch;
pub use crew_protocol::docs::DocsWatchResult;
pub use crew_protocol::docs::ImageLimitKind;
pub use crew_protocol::docs::Inline;
pub use crew_protocol::docs::LinkTarget;
pub use crew_protocol::docs::ListItem;
pub use crew_protocol::docs::RasterType;
pub use crew_protocol::docs::UnsupportedImageKind;
pub use crew_protocol::epics::Epic;
pub use crew_protocol::epics::EpicAgentCounts;
pub use crew_protocol::epics::EpicBoard;
pub use crew_protocol::epics::EpicCounts;
pub use crew_protocol::epics::EpicDetail;
pub use crew_protocol::epics::EpicFilter;
pub use crew_protocol::epics::EpicGet;
pub use crew_protocol::epics::EpicReport;
pub use crew_protocol::epics::EpicReportSummary;
pub use crew_protocol::epics::EpicStatus;
pub use crew_protocol::epics::EpicSummary;
pub use crew_protocol::epics::ScopedTaskCard;
pub use crew_protocol::epics::TaskGetScoped;
pub use crew_protocol::epics::TasksQuery;
pub use crew_protocol::epics::TasksQueryResult;
pub use crew_protocol::features::Feature;
pub use crew_protocol::features::FeatureSession;
pub use crew_protocol::features::FeatureUnavailable;
pub use crew_protocol::features::PageRequest;
pub use crew_protocol::features::RunScope;
pub use crew_protocol::features::Unwatch;
pub use crew_protocol::features::UnwatchResult;
pub use crew_protocol::usage_metrics::AccountAttribution;
pub use crew_protocol::usage_metrics::ActivityKind;
pub use crew_protocol::usage_metrics::CompletionCohort;
pub use crew_protocol::usage_metrics::CostBasis;
pub use crew_protocol::usage_metrics::CostEstimate;
pub use crew_protocol::usage_metrics::CounterMetric;
pub use crew_protocol::usage_metrics::CoverageExclusion;
pub use crew_protocol::usage_metrics::CoverageReason;
pub use crew_protocol::usage_metrics::EpicAttribution;
pub use crew_protocol::usage_metrics::EstimateState;
pub use crew_protocol::usage_metrics::ExecutionStatusCounts;
pub use crew_protocol::usage_metrics::MetricCoverage;
pub use crew_protocol::usage_metrics::MetricState;
pub use crew_protocol::usage_metrics::ObservationUnit;
pub use crew_protocol::usage_metrics::PriceBook;
pub use crew_protocol::usage_metrics::PriceRow;
pub use crew_protocol::usage_metrics::RatioBound;
pub use crew_protocol::usage_metrics::RatioMetric;
pub use crew_protocol::usage_metrics::SourceGap;
pub use crew_protocol::usage_metrics::TimeBasis;
pub use crew_protocol::usage_metrics::TrackingState;
pub use crew_protocol::usage_metrics::UsageBinding;
pub use crew_protocol::usage_metrics::UsageBreakdown;
pub use crew_protocol::usage_metrics::UsageBreakdownRow;
pub use crew_protocol::usage_metrics::UsageBucket;
pub use crew_protocol::usage_metrics::UsageChanged;
pub use crew_protocol::usage_metrics::UsageCounters;
pub use crew_protocol::usage_metrics::UsageDimension;
pub use crew_protocol::usage_metrics::UsageEpicFilter;
pub use crew_protocol::usage_metrics::UsageGroupKey;
pub use crew_protocol::usage_metrics::UsageKind;
pub use crew_protocol::usage_metrics::UsageObservation;
pub use crew_protocol::usage_metrics::UsageObservations;
pub use crew_protocol::usage_metrics::UsagePrices;
pub use crew_protocol::usage_metrics::UsageQuery;
pub use crew_protocol::usage_metrics::UsageRun;
pub use crew_protocol::usage_metrics::UsageRuns;
pub use crew_protocol::usage_metrics::UsageScope;
pub use crew_protocol::usage_metrics::UsageSeries;
pub use crew_protocol::usage_metrics::UsageSummary;
pub use crew_protocol::usage_metrics::UsageTaskFilter;
pub use crew_protocol::usage_metrics::UsageTotals;
pub use crew_protocol::usage_metrics::UsageWatchResult;
pub use crew_protocol::usage_metrics::WorkAttribution;

pub type FeatureVersions = crew_protocol::features::FeatureVersions;
uniffi::custom_type!(FeatureVersions, std::collections::HashMap<String, u32>, { remote, lower: |value| value.iter().map(|(k,v)|(k.clone(),*v)).collect(), try_lift: |value| FeatureVersions::try_from(value.into_iter().collect::<std::collections::BTreeMap<_,_>>()).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type Id = crew_protocol::features::Id;
uniffi::custom_type!(Id, String, { remote, lower: |value| value.as_str().to_owned(), try_lift: |value| Id::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type UnavailableFeatures = crew_protocol::features::UnavailableFeatures;
uniffi::custom_type!(UnavailableFeatures, std::collections::HashMap<String, FeatureUnavailable>, { remote, lower: |value| value.iter().map(|(k,v)|(k.clone(),*v)).collect(), try_lift: |value| UnavailableFeatures::try_from(value.into_iter().collect::<std::collections::BTreeMap<_,_>>()).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type Wide = crew_protocol::features::Wide;
uniffi::custom_type!(Wide, String, { remote, lower: |value| value.0.to_string(), try_lift: |value| { if value.is_empty() || (value.len()>1 && value.starts_with('0')) || !value.bytes().all(|b| b.is_ascii_digit()) { return Err(uniffi::deps::anyhow::anyhow!("noncanonical counter")); } value.parse::<u128>().map(crew_protocol::features::Wide).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")) }, });

pub type Text8192 = crew_protocol::features::Text<8192>;
uniffi::custom_type!(Text8192, String, { remote, lower: |value| value.into_string(), try_lift: |value| Text8192::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedAsideTarget64 = crew_protocol::features::List<AsideTarget, 64>;
uniffi::custom_type!(BoundedAsideTarget64, Vec<AsideTarget>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedAsideTarget64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocRef2 = crew_protocol::features::List<DocRef, 2>;
uniffi::custom_type!(BoundedDocRef2, Vec<DocRef>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocRef2::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type Text512 = crew_protocol::features::Text<512>;
uniffi::custom_type!(Text512, String, { remote, lower: |value| value.into_string(), try_lift: |value| Text512::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type Timestamp = crew_protocol::features::Timestamp;
uniffi::custom_type!(Timestamp, i64, { remote, lower: |value| value.get(), try_lift: |value| Timestamp::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedFeatureSession = Box<FeatureSession>;
uniffi::custom_type!(BoxedFeatureSession, FeatureSession, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedEpicBoard = Box<EpicBoard>;
uniffi::custom_type!(BoxedEpicBoard, EpicBoard, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type Text65536 = crew_protocol::features::Text<65536>;
uniffi::custom_type!(Text65536, String, { remote, lower: |value| value.into_string(), try_lift: |value| Text65536::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedId64 = crew_protocol::features::List<Id, 64>;
uniffi::custom_type!(BoundedId64, Vec<Id>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedId64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedEpicDetail = Box<EpicDetail>;
uniffi::custom_type!(BoxedEpicDetail, EpicDetail, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedTasksQueryResult = Box<TasksQueryResult>;
uniffi::custom_type!(BoxedTasksQueryResult, TasksQueryResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedScopedTaskCard = Box<ScopedTaskCard>;
uniffi::custom_type!(BoxedScopedTaskCard, ScopedTaskCard, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedAsideMember200 = crew_protocol::features::List<AsideMember, 200>;
uniffi::custom_type!(BoundedAsideMember200, Vec<AsideMember>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedAsideMember200::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedAsideMembers = Box<AsideMembers>;
uniffi::custom_type!(BoxedAsideMembers, AsideMembers, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedText51232 = crew_protocol::features::List<Text512, 32>;
uniffi::custom_type!(BoundedText51232, Vec<Text512>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedText51232::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedText51264 = crew_protocol::features::List<Text512, 64>;
uniffi::custom_type!(BoundedText51264, Vec<Text512>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedText51264::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedCoverageExclusion32 = crew_protocol::features::List<CoverageExclusion, 32>;
uniffi::custom_type!(BoundedCoverageExclusion32, Vec<CoverageExclusion>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedCoverageExclusion32::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedSourceGap64 = crew_protocol::features::List<SourceGap, 64>;
uniffi::custom_type!(BoundedSourceGap64, Vec<SourceGap>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedSourceGap64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedAsideAnswerMeta64 = crew_protocol::features::List<AsideAnswerMeta, 64>;
uniffi::custom_type!(BoundedAsideAnswerMeta64, Vec<AsideAnswerMeta>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedAsideAnswerMeta64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedAsideSummary50 = crew_protocol::features::List<AsideSummary, 50>;
uniffi::custom_type!(BoundedAsideSummary50, Vec<AsideSummary>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedAsideSummary50::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedAsideListResult = Box<AsideListResult>;
uniffi::custom_type!(BoxedAsideListResult, AsideListResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedAsideDetail = Box<AsideDetail>;
uniffi::custom_type!(BoxedAsideDetail, AsideDetail, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedAsideGetResult = Box<AsideGetResult>;
uniffi::custom_type!(BoxedAsideGetResult, AsideGetResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedAsideSummary = Box<AsideSummary>;
uniffi::custom_type!(BoxedAsideSummary, AsideSummary, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedAsideAskResult = Box<AsideAskResult>;
uniffi::custom_type!(BoxedAsideAskResult, AsideAskResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedAsideMutationResult = Box<AsideMutationResult>;
uniffi::custom_type!(BoxedAsideMutationResult, AsideMutationResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedAsideWatchResult = Box<AsideWatchResult>;
uniffi::custom_type!(BoxedAsideWatchResult, AsideWatchResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedUnwatchResult = Box<UnwatchResult>;
uniffi::custom_type!(BoxedUnwatchResult, UnwatchResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedString64 = crew_protocol::features::List<String, 64>;
uniffi::custom_type!(BoundedString64, Vec<String>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedString64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type Text1024 = crew_protocol::features::Text<1024>;
uniffi::custom_type!(Text1024, String, { remote, lower: |value| value.into_string(), try_lift: |value| Text1024::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocSource64 = crew_protocol::features::List<DocSource, 64>;
uniffi::custom_type!(BoundedDocSource64, Vec<DocSource>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocSource64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocHeading128 = crew_protocol::features::List<DocHeading, 128>;
uniffi::custom_type!(BoundedDocHeading128, Vec<DocHeading>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocHeading128::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocVariant64 = crew_protocol::features::List<DocVariant, 64>;
uniffi::custom_type!(BoundedDocVariant64, Vec<DocVariant>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocVariant64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocEntry200 = crew_protocol::features::List<DocEntry, 200>;
uniffi::custom_type!(BoundedDocEntry200, Vec<DocEntry>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocEntry200::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedDocIndexPage = Box<DocIndexPage>;
uniffi::custom_type!(BoxedDocIndexPage, DocIndexPage, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type DocVersion = crew_protocol::docs::DocVersion;
uniffi::custom_type!(DocVersion, DocVersionData, { remote, lower: |value| (*value).clone(), try_lift: |value| DocVersion::try_from(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type Text262144 = crew_protocol::features::Text<262144>;
uniffi::custom_type!(Text262144, String, { remote, lower: |value| value.into_string(), try_lift: |value| Text262144::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedInline4096 = crew_protocol::features::List<Inline, 4096>;
uniffi::custom_type!(BoundedInline4096, Vec<Inline>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedInline4096::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type ExternalHref = crew_protocol::docs::ExternalHref;
uniffi::custom_type!(ExternalHref, String, { remote, lower: |value| value.as_str().to_owned(), try_lift: |value| crew_protocol::features::Text::<2048>::new(value).and_then(ExternalHref::try_from).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedBlock4096 = crew_protocol::features::List<Block, 4096>;
uniffi::custom_type!(BoundedBlock4096, Vec<Block>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedBlock4096::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedListItem4096 = crew_protocol::features::List<ListItem, 4096>;
uniffi::custom_type!(BoundedListItem4096, Vec<ListItem>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedListItem4096::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedAlignment4096 = crew_protocol::features::List<Alignment, 4096>;
uniffi::custom_type!(BoundedAlignment4096, Vec<Alignment>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedAlignment4096::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedBoundedInline40964096 = crew_protocol::features::List<BoundedInline4096, 4096>;
uniffi::custom_type!(BoundedBoundedInline40964096, Vec<BoundedInline4096>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedBoundedInline40964096::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedBoundedBoundedInline409640964096 =
    crew_protocol::features::List<BoundedBoundedInline40964096, 4096>;
uniffi::custom_type!(BoundedBoundedBoundedInline409640964096, Vec<BoundedBoundedInline40964096>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedBoundedBoundedInline409640964096::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocBlock4096 = crew_protocol::features::List<DocBlock, 4096>;
uniffi::custom_type!(BoundedDocBlock4096, Vec<DocBlock>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocBlock4096::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocImage64 = crew_protocol::features::List<DocImage, 64>;
uniffi::custom_type!(BoundedDocImage64, Vec<DocImage>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocImage64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type DocSnapshot = crew_protocol::docs::DocSnapshot;
uniffi::custom_type!(DocSnapshot, DocSnapshotData, { remote, lower: |value| (*value).clone(), try_lift: |value| DocSnapshot::try_from(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedDocSnapshot = Box<DocSnapshot>;
uniffi::custom_type!(BoxedDocSnapshot, DocSnapshot, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedDocReadResult = Box<DocReadResult>;
uniffi::custom_type!(BoxedDocReadResult, DocReadResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type DocAssetChunk = crew_protocol::docs::DocAssetChunk;
uniffi::custom_type!(DocAssetChunk, DocAssetChunkData, { remote, lower: |value| (*value).clone(), try_lift: |value| DocAssetChunk::try_from(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedDocAssetChunk = Box<DocAssetChunk>;
uniffi::custom_type!(BoxedDocAssetChunk, DocAssetChunk, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedDocAssetResult = Box<DocAssetResult>;
uniffi::custom_type!(BoxedDocAssetResult, DocAssetResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedDocCapturedHead64 = crew_protocol::features::List<DocCapturedHead, 64>;
uniffi::custom_type!(BoundedDocCapturedHead64, Vec<DocCapturedHead>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocCapturedHead64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocRef64 = crew_protocol::features::List<DocRef, 64>;
uniffi::custom_type!(BoundedDocRef64, Vec<DocRef>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocRef64::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocHistoryRow50 = crew_protocol::features::List<DocHistoryRow, 50>;
uniffi::custom_type!(BoundedDocHistoryRow50, Vec<DocHistoryRow>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocHistoryRow50::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedDocHistoryPage = Box<DocHistoryPage>;
uniffi::custom_type!(BoxedDocHistoryPage, DocHistoryPage, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedDocHistoryResult = Box<DocHistoryResult>;
uniffi::custom_type!(BoxedDocHistoryResult, DocHistoryResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedDocDiff = Box<DocDiff>;
uniffi::custom_type!(BoxedDocDiff, DocDiff, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedDocDiffResult = Box<DocDiffResult>;
uniffi::custom_type!(BoxedDocDiffResult, DocDiffResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedDocsWatchResult = Box<DocsWatchResult>;
uniffi::custom_type!(BoxedDocsWatchResult, DocsWatchResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type Fraction = crew_protocol::features::Fraction;
uniffi::custom_type!(Fraction, f64, { remote, lower: |value| value.get(), try_lift: |value| Fraction::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedCoverageReason32 = crew_protocol::features::List<CoverageReason, 32>;
uniffi::custom_type!(BoundedCoverageReason32, Vec<CoverageReason>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedCoverageReason32::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedUsageSummary = Box<UsageSummary>;
uniffi::custom_type!(BoxedUsageSummary, UsageSummary, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedUsageBucket720 = crew_protocol::features::List<UsageBucket, 720>;
uniffi::custom_type!(BoundedUsageBucket720, Vec<UsageBucket>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedUsageBucket720::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedUsageSeries = Box<UsageSeries>;
uniffi::custom_type!(BoxedUsageSeries, UsageSeries, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedActivityKind8 = crew_protocol::features::List<ActivityKind, 8>;
uniffi::custom_type!(BoundedActivityKind8, Vec<ActivityKind>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedActivityKind8::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedUsageBreakdownRow200 = crew_protocol::features::List<UsageBreakdownRow, 200>;
uniffi::custom_type!(BoundedUsageBreakdownRow200, Vec<UsageBreakdownRow>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedUsageBreakdownRow200::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedUsageBreakdown = Box<UsageBreakdown>;
uniffi::custom_type!(BoxedUsageBreakdown, UsageBreakdown, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedWide32 = crew_protocol::features::List<Wide, 32>;
uniffi::custom_type!(BoundedWide32, Vec<Wide>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedWide32::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedUsageObservation200 = crew_protocol::features::List<UsageObservation, 200>;
uniffi::custom_type!(BoundedUsageObservation200, Vec<UsageObservation>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedUsageObservation200::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedUsageObservations = Box<UsageObservations>;
uniffi::custom_type!(BoxedUsageObservations, UsageObservations, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedUsageRun200 = crew_protocol::features::List<UsageRun, 200>;
uniffi::custom_type!(BoundedUsageRun200, Vec<UsageRun>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedUsageRun200::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedUsageRuns = Box<UsageRuns>;
uniffi::custom_type!(BoxedUsageRuns, UsageRuns, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedPriceRow200 = crew_protocol::features::List<PriceRow, 200>;
uniffi::custom_type!(BoundedPriceRow200, Vec<PriceRow>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedPriceRow200::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type Text2048 = crew_protocol::features::Text<2048>;
uniffi::custom_type!(Text2048, String, { remote, lower: |value| value.into_string(), try_lift: |value| Text2048::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedText204816 = crew_protocol::features::List<Text2048, 16>;
uniffi::custom_type!(BoundedText204816, Vec<Text2048>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedText204816::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoxedPriceBook = Box<PriceBook>;
uniffi::custom_type!(BoxedPriceBook, PriceBook, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoxedUsageWatchResult = Box<UsageWatchResult>;
uniffi::custom_type!(BoxedUsageWatchResult, UsageWatchResult, { remote, lower: |value| *value, try_lift: |value| Ok(Box::new(value)), });

pub type BoundedId200 = crew_protocol::features::List<Id, 200>;
uniffi::custom_type!(BoundedId200, Vec<Id>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedId200::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedAsideGone200 = crew_protocol::features::List<AsideGone, 200>;
uniffi::custom_type!(BoundedAsideGone200, Vec<AsideGone>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedAsideGone200::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

pub type BoundedDocSelectedChange2 = crew_protocol::features::List<DocSelectedChange, 2>;
uniffi::custom_type!(BoundedDocSelectedChange2, Vec<DocSelectedChange>, { remote, lower: |value| value.into_vec(), try_lift: |value| BoundedDocSelectedChange2::new(value).map_err(|e| uniffi::deps::anyhow::anyhow!("{e}")), });

#[uniffi::remote(Record)]
pub struct FeatureAvailability {
    pub session: Option<FeatureSession>,
    pub reason: Option<ClientReason>,
}

#[uniffi::remote(Record)]
pub struct FeatureSession {
    pub accepted: FeatureVersions,
    pub connection_epoch: Id,
    pub workspace_run_id: Option<Id>,
    pub unavailable: UnavailableFeatures,
}

#[uniffi::remote(Enum)]
pub enum FeatureUnavailable {
    Unsupported,
    UnsupportedVersion,
    NotAuthorized,
    NotReady,
}

#[uniffi::remote(Enum)]
pub enum FeatureRejection {
    AsideAdmission(AsideAdmissionFailure),
}

#[uniffi::remote(Enum)]
pub enum AsideAdmissionFailure {
    QuestionLimit { max_bytes: u32 },
    TargetLimit { max_targets: u16 },
    PendingLimit { max_pending: u16 },
    MemoryLimit { max_bytes: Wide },
    ReceiptLimit { max_receipts: Wide, max_bytes: Wide },
    EmptyQuestion,
    EmptyTargets,
    DuplicateTarget,
}

#[uniffi::remote(Enum)]
pub enum FeatureAction {
    AskAside(AsideAsk),
    CancelAside(AsideGet),
    RemoveAside(AsideRemove),
    WatchAsides(AsideList),
    WatchDocs(DocsWatch),
    WatchUsage(UsageQuery),
    UnwatchAsides(Unwatch),
    UnwatchDocs(Unwatch),
    UnwatchUsage(Unwatch),
}

#[uniffi::remote(Record)]
pub struct AsideAsk {
    pub workspace_run_id: Id,
    pub request_id: Id,
    pub question: Text8192,
    pub targets: Option<BoundedAsideTarget64>,
}

#[uniffi::remote(Record)]
pub struct AsideTarget {
    pub agent_id: String,
    pub incarnation: Id,
}

#[uniffi::remote(Record)]
pub struct AsideGet {
    pub workspace_run_id: Id,
    pub id: Id,
    pub agent: Option<AsideTarget>,
}

#[uniffi::remote(Record)]
pub struct AsideRemove {
    pub workspace_run_id: Id,
    pub id: Id,
}

#[uniffi::remote(Record)]
pub struct AsideList {
    pub workspace_run_id: Id,
    pub page: PageRequest,
}

#[uniffi::remote(Record)]
pub struct PageRequest {
    pub cursor: Option<Id>,
    pub limit: Option<u16>,
}

#[uniffi::remote(Record)]
pub struct DocsWatch {
    pub selected: BoundedDocRef2,
}

#[uniffi::remote(Record)]
pub struct DocRef {
    pub doc_id: Id,
    pub source_id: Id,
    pub source_generation: Wide,
}

#[uniffi::remote(Record)]
pub struct UsageQuery {
    pub scope: UsageScope,
    pub from_ms: Option<Timestamp>,
    pub to_ms: Option<Timestamp>,
    pub group_by: Option<UsageDimension>,
    pub bucket_seconds: Option<u32>,
    pub price_book_id: Option<Id>,
    pub page: PageRequest,
}

#[uniffi::remote(Record)]
pub struct UsageScope {
    pub workspace_run_id: Id,
    pub kind: UsageKind,
    pub agent_incarnation: Option<Id>,
    pub task: UsageTaskFilter,
    pub epic: UsageEpicFilter,
    pub model: Option<Text512>,
    pub account_id: Option<Id>,
}

#[uniffi::remote(Enum)]
pub enum UsageKind {
    Live,
    Aside,
}

#[uniffi::remote(Enum)]
pub enum UsageTaskFilter {
    All,
    Id(String),
    NoTask,
    NotAttributed,
}

#[uniffi::remote(Enum)]
pub enum UsageEpicFilter {
    All,
    Id(String),
    NoEpic,
    NoTask,
    NotAttributed,
}

#[uniffi::remote(Enum)]
pub enum UsageDimension {
    Agent,
    Task,
    Epic,
    Model,
    Account,
    Runtime,
}

#[uniffi::remote(Record)]
pub struct Unwatch {
    pub watch_id: Id,
}

#[uniffi::remote(Record)]
pub struct FeatureWatchState {
    pub feature: Feature,
    pub watch_id: Id,
    pub connection_epoch: Id,
    pub epoch: Id,
    pub revision: Wide,
    pub query: FeatureAction,
    pub initial: FeatureResponse,
    pub stale: bool,

    pub change: Option<FeatureChange>,
}

#[uniffi::remote(Enum)]
pub enum Feature {
    Epics,
    Asides,
    Docs,
    UsageMetrics,
}

#[uniffi::remote(Enum)]
pub enum FeatureResponse {
    FeatureSession(BoxedFeatureSession),
    EpicBoard(BoxedEpicBoard),
    EpicDetail(BoxedEpicDetail),
    Tasks(BoxedTasksQueryResult),
    TaskCard(BoxedScopedTaskCard),
    AsideMembers(BoxedAsideMembers),
    Asides(BoxedAsideListResult),
    AsideDetail(BoxedAsideGetResult),
    AsideAsked(BoxedAsideAskResult),
    AsideMutation(BoxedAsideMutationResult),
    AsideWatch(BoxedAsideWatchResult),
    Unwatched(BoxedUnwatchResult),
    DocsIndex(BoxedDocIndexPage),
    Document(BoxedDocReadResult),
    DocAsset(BoxedDocAssetResult),
    DocHistory(BoxedDocHistoryResult),
    DocDiff(BoxedDocDiffResult),
    DocsWatch(BoxedDocsWatchResult),
    UsageSummary(BoxedUsageSummary),
    UsageSeries(BoxedUsageSeries),
    UsageBreakdown(BoxedUsageBreakdown),
    UsageObservations(BoxedUsageObservations),
    UsageRuns(BoxedUsageRuns),
    UsagePrices(BoxedPriceBook),
    UsageWatch(BoxedUsageWatchResult),
}

#[uniffi::remote(Record)]
pub struct EpicBoard {
    pub generation: Id,
    pub revision: Wide,
    pub epics: Vec<EpicSummary>,
    pub no_epic: EpicCounts,
    pub focus_epic: Option<String>,
}

#[uniffi::remote(Record)]
pub struct EpicSummary {
    pub id: String,
    pub title: Text512,
    pub source_message_id: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub last_done_at: Option<Timestamp>,

    pub revision: Wide,
    pub status: EpicStatus,
    pub counts: EpicCounts,
    pub agents: Vec<EpicAgentCounts>,
    pub report: Option<EpicReportSummary>,
}

#[uniffi::remote(Enum)]
pub enum EpicStatus {
    Planned,
    Active,
    Blocked,
    Done,
}

#[uniffi::remote(Record)]
pub struct EpicCounts {
    pub total: Wide,
    pub queued: Wide,
    pub ready: Wide,
    pub working: Wide,
    pub waiting: Wide,
    pub blocked: Wide,
    pub review: Wide,
    pub done: Wide,
    pub failed: Wide,
}

#[uniffi::remote(Record)]
pub struct EpicAgentCounts {
    pub agent_id: String,
    pub assigned: Wide,
    pub unfinished_assigned: Wide,
    pub review: Wide,
}

#[uniffi::remote(Record)]
pub struct EpicReportSummary {
    pub reported_at: Timestamp,
    pub covered_revision: Wide,
}

#[uniffi::remote(Record)]
pub struct EpicDetail {
    pub scope: RunScope,
    pub board_revision: Wide,
    pub epic: Epic,
    pub agents: Vec<EpicAgentCounts>,
    pub task_ids: Vec<String>,
}

#[uniffi::remote(Record)]
pub struct RunScope {
    pub workspace_run_id: Id,
}

#[uniffi::remote(Record)]
pub struct Epic {
    pub id: String,
    pub title: Text512,
    pub objective: Text65536,
    pub source_message_id: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub revision: Wide,
    pub status: EpicStatus,
    pub counts: EpicCounts,
    pub report: Option<EpicReport>,
}

#[uniffi::remote(Record)]
pub struct EpicReport {
    pub summary: Text65536,
    pub reported_at: Timestamp,
    pub covered_revision: Wide,
    pub decision_message_ids: BoundedId64,
}

#[uniffi::remote(Record)]
pub struct TasksQueryResult {
    pub workspace_run_id: Id,
    pub board_revision: Wide,
    pub tasks: Vec<Task>,
}

#[uniffi::remote(Record)]
pub struct ScopedTaskCard {
    pub workspace_run_id: Id,
    pub board_revision: Wide,
    pub card: TaskCard,
    pub epic: Option<EpicSummary>,
}

#[uniffi::remote(Record)]
pub struct AsideMembers {
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub total: Wide,
    pub rows: BoundedAsideMember200,
    pub next_cursor: Option<Id>,
}

#[uniffi::remote(Record)]
pub struct AsideMember {
    pub target: AsideTarget,
    pub runtime: RuntimeKind,
    pub model: Option<Text512>,
    pub availability: AsideAvailability,
    pub resets_at: Option<Timestamp>,
    pub last_turn_at: Option<Timestamp>,
}

#[uniffi::remote(Enum)]
pub enum AsideAvailability {
    Available,
    Unavailable { reason: AsideUnavailableReason },
}

#[uniffi::remote(Enum)]
pub enum AsideUnavailableReason {
    ShellMember,
    UnsupportedRuntime,
    UnsupportedVersion,
    NoSession,
    UnreadableSession,
    UsageLimit,
    MemberChanged,
    MemberRemoved,
    IsolationUnavailable,
    PrefixUnavailable,
    CredentialsUnavailable,
}

#[uniffi::remote(Record)]
pub struct AsideListResult {
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub rows: BoundedAsideSummary50,
    pub next_cursor: Option<Id>,
    pub evicted: bool,
}

#[uniffi::remote(Record)]
pub struct AsideSummary {
    pub id: Id,
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub asked_at: Timestamp,
    pub question_preview: Text512,
    pub question_truncated: bool,
    pub answer_counts: AsideCounts,

    pub total_cost: Option<CostEstimate>,
    pub answers: BoundedAsideAnswerMeta64,
}

#[uniffi::remote(Record)]
pub struct AsideCounts {
    pub total: Wide,
    pub waiting: Wide,
    pub answering: Wide,
    pub answered: Wide,
    pub failed: Wide,
    pub cancelled: Wide,
    pub not_available: Wide,
}

#[uniffi::remote(Record)]
pub struct CostEstimate {
    pub price_book_id: Option<Id>,
    pub prices_checked_at: Option<Timestamp>,
    pub basis: CostBasis,
    pub lower_pico_usd: Option<Wide>,
    pub upper_pico_usd: Option<Wide>,
    pub state: EstimateState,
    pub reason: Option<CoverageReason>,
    pub assumptions: BoundedText51232,
    pub excluded_models: BoundedText51264,
    pub coverage: MetricCoverage,
}

#[uniffi::remote(Enum)]
pub enum CostBasis {
    StandardApiEquivalent,
    RuntimeReported,
}

#[uniffi::remote(Enum)]
pub enum EstimateState {
    Finite,
    Partial,
    Unavailable,
}

#[uniffi::remote(Enum)]
pub enum CoverageReason {
    ZeroInput,
    NoCoveredInput,
    NotReported,
    InvalidCounter,
    UnknownTime,
    UnknownModel,
    UnknownProvider,
    UnknownAccount,
    UnknownAttribution,
    MultipleTasks,
    AmbiguousBoundary,
    NativeBatch,
    MissingRequestIdentity,
    MissingTurnIdentity,
    SourceGap,
    SourceBacklog,
    UnsupportedRuntime,
    UnsupportedSidechain,
    ParserLimit,
    RetentionLimit,
    IdentityLost,
    CorrectionOutsideHorizon,
    CompletionHistoryIncomplete,
    NoCompletions,
    NoEligibleCost,
    NoPrice,
    NotTracked,
}

#[uniffi::remote(Record)]
pub struct MetricCoverage {
    pub eligible_observations: Wide,
    pub observations_seen: Wide,
    pub exclusions: BoundedCoverageExclusion32,
    pub known_requests: Wide,
    pub request_cardinality_complete: bool,
    pub reporting_agents: Wide,
    pub expected_agents: Option<Wide>,
    pub source_gaps: BoundedSourceGap64,
    pub source_gaps_total: Option<Wide>,
    pub source_backlog_files: Option<Wide>,
    pub retained_from: Option<Timestamp>,
}

#[uniffi::remote(Record)]
pub struct CoverageExclusion {
    pub reason: CoverageReason,
    pub observations: Wide,
}

#[uniffi::remote(Record)]
pub struct SourceGap {
    pub source_id: Id,
    pub reason: CoverageReason,
    pub from: Option<Timestamp>,
    pub to: Option<Timestamp>,
}

#[uniffi::remote(Record)]
pub struct AsideAnswerMeta {
    pub target: AsideTarget,
    pub runtime: RuntimeKind,
    pub model: Option<Text512>,
    pub status: AsideStatus,
    pub reason: Option<AsideAnswerReason>,
    pub resets_at: Option<Timestamp>,
    pub context_at: Option<Timestamp>,
    pub context_approximate: bool,
    pub last_turn_at: Option<Timestamp>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub prefix_reproduced: bool,
    pub truncated: bool,
    pub usage: Option<AsideUsage>,
}

#[uniffi::remote(Enum)]
pub enum AsideStatus {
    Waiting,
    Answering,
    Answered,
    Failed,
    Cancelled,
    NotAvailable,
}

#[uniffi::remote(Enum)]
pub enum AsideAnswerReason {
    Unavailable(AsideUnavailableReason),
    Timeout,
    RuntimeFailed,
    Cancelled,
    Removed,
    DaemonStopped,
    RunReset,
}

#[uniffi::remote(Record)]
pub struct AsideUsage {
    pub counters: UsageCounters,
    pub cost: Option<CostEstimate>,
}

#[uniffi::remote(Record)]
pub struct UsageCounters {
    pub input_total: Option<Wide>,
    pub input_uncached: Option<Wide>,
    pub cache_read: Option<Wide>,
    pub cache_write: Option<Wide>,
    pub cache_write_5m: Option<Wide>,
    pub cache_write_1h: Option<Wide>,
    pub output: Option<Wide>,
    pub total_tokens: Option<Wide>,
}

#[uniffi::remote(Enum)]
pub enum AsideGetResult {
    Available {
        detail: BoxedAsideDetail,
    },
    Gone {
        id: Id,
        workspace_run_id: Id,
        aside_epoch: Id,
        revision: Wide,
        cause: AsideGoneCause,
    },
}

#[uniffi::remote(Record)]
pub struct AsideDetail {
    pub summary_id: Id,
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub question: Text8192,
    pub answer_meta: Option<AsideAnswerMeta>,

    pub answer_text: Option<Text65536>,
}

#[uniffi::remote(Enum)]
pub enum AsideGoneCause {
    Removed,
    Evicted,
    DaemonRestart,
    RunReset,
    Unknown,
}

#[uniffi::remote(Enum)]
pub enum AsideAskResult {
    Accepted {
        summary: BoxedAsideSummary,
        duplicate: bool,
    },
    Gone {
        id: Id,
        workspace_run_id: Id,
        aside_epoch: Id,
        revision: Wide,
        cause: AsideGoneCause,
    },
}

#[uniffi::remote(Record)]
pub struct AsideMutationResult {
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub revision: Wide,
    pub id: Id,
    pub changed: bool,
    pub gone: Option<AsideGoneCause>,
}

#[uniffi::remote(Record)]
pub struct AsideWatchResult {
    pub watch_id: Id,
    pub initial: AsideListResult,
}

#[uniffi::remote(Record)]
pub struct UnwatchResult {
    pub watch_id: Id,
    pub removed: bool,
}

#[uniffi::remote(Record)]
pub struct DocIndexPage {
    pub epoch: Id,
    pub revision: Wide,
    pub sources: BoundedDocSource64,
    pub entries: BoundedDocEntry200,
    pub next_cursor: Option<Id>,
    pub phase: DocPhase,
    pub watch_mode: DocWatchMode,
    pub limited: bool,
    pub omitted_entries: Option<Wide>,
    pub reason: Option<DocFailureCode>,
}

#[uniffi::remote(Record)]
pub struct DocSource {
    pub id: Id,
    pub generation: Wide,
    pub kind: DocSourceKind,
    pub member_ids: BoundedString64,
    pub display_name: Text512,
    pub branch: Option<Text1024>,
    pub head_commit: Option<Id>,
    pub state: DocSourceState,
    pub reason: Option<DocFailureCode>,
}

#[uniffi::remote(Enum)]
pub enum DocSourceKind {
    Workspace,
    Member,
    Shared,
}

#[uniffi::remote(Enum)]
pub enum DocSourceState {
    Ready,
    Missing,
    Removed,
    Unavailable,
}

#[uniffi::remote(Enum)]
pub enum DocFailureCode {
    Disabled,
    ForbiddenPath,
    SourceGone,
    TooLarge,
    InvalidUtf8,
    UnsupportedImage,
    UnstableFile,
    HistoryUnavailable,
    ObjectMissing,
    SnapshotExpired,
    StaleCursor,
    Busy,
}

#[uniffi::remote(Record)]
pub struct DocEntry {
    pub id: Id,
    pub relative_path: Text1024,
    pub variants: BoundedDocVariant64,
}

#[uniffi::remote(Record)]
pub struct DocVariant {
    pub source_id: Id,
    pub source_generation: Wide,
    pub markdown_hash: Option<Id>,
    pub bytes: Option<u32>,
    pub observed_at_ms: Option<Timestamp>,
    pub title: Option<Text512>,
    pub headings: BoundedDocHeading128,
    pub headings_limited: bool,
    pub state: DocState,
    pub reason: Option<DocFailureCode>,
    pub comparison: DocComparison,
}

#[uniffi::remote(Record)]
pub struct DocHeading {
    pub anchor: Text512,
    pub title: Text512,
    pub level: u8,
    pub start_line: u32,
}

#[uniffi::remote(Enum)]
pub enum DocState {
    Present,
    Missing,
    Removed,
    Unavailable,
}

#[uniffi::remote(Enum)]
pub enum DocComparison {
    SameMarkdown,
    Different,
    OnlyHere,
    Unknown,
}

#[uniffi::remote(Enum)]
pub enum DocPhase {
    Building,
    Ready,
    Disabled,
    Unavailable,
}

#[uniffi::remote(Enum)]
pub enum DocWatchMode {
    Events,
    ReconcileOnly,
    None,
}

#[uniffi::remote(Enum)]
pub enum DocReadResult {
    Available(BoxedDocSnapshot),
    Unavailable(DocFailure),
}

#[uniffi::remote(Record)]
pub struct DocSnapshotData {
    pub epoch: Id,
    pub snapshot_id: Id,
    pub version: DocVersion,
    pub markdown: Text262144,
    pub blocks: BoundedDocBlock4096,
    pub outline: BoundedDocHeading128,
    pub images: BoundedDocImage64,
    pub render_hash: Id,
    pub notices: BoundedText51264,
}

#[uniffi::remote(Record)]
pub struct DocVersionData {
    pub id: Id,
    pub document: DocRef,
    pub kind: DocVersionKind,
    pub markdown_hash: Id,
    pub bytes: u32,
    pub observed_at_ms: Option<Timestamp>,
    pub recorded_at_ms: Option<Timestamp>,
    pub commit_id: Option<Id>,
    pub change_id: Option<Id>,
    pub author: Option<Text512>,
    pub subject: Option<Text512>,
}

#[uniffi::remote(Enum)]
pub enum DocVersionKind {
    Working,
    Recorded,
    Seen,
}

#[uniffi::remote(Record)]
pub struct DocBlock {
    pub block_key: Id,
    pub start_line: u32,
    pub end_line: u32,
    pub block: Block,
}

#[uniffi::remote(Enum)]
pub enum Block {
    Heading {
        level: u8,
        anchor: Text512,
        children: BoundedInline4096,
    },
    Paragraph {
        children: BoundedInline4096,
    },
    CodeBlock {
        language_hint: Option<Text512>,
        literal: Text262144,
    },
    Quote {
        blocks: BoundedBlock4096,
    },
    List {
        ordered: bool,
        start: Option<u32>,
        items: BoundedListItem4096,
    },
    Table {
        alignments: BoundedAlignment4096,
        header: BoundedBoundedInline40964096,
        rows: BoundedBoundedBoundedInline409640964096,
    },
    Rule,
}

#[uniffi::remote(Enum)]
pub enum Inline {
    Text {
        text: Text262144,
    },
    Emphasis {
        children: BoundedInline4096,
    },
    Strong {
        children: BoundedInline4096,
    },
    Code {
        text: Text262144,
    },
    SoftBreak,
    HardBreak,
    Link {
        label: BoundedInline4096,
        target: LinkTarget,
    },
    Image {
        alt: Text512,
        asset: u16,
    },
}

#[uniffi::remote(Enum)]
pub enum LinkTarget {
    Doc {
        doc_id: Id,
        preferred_source: Option<Id>,
        anchor: Option<Text512>,
    },
    Anchor {
        anchor: Text512,
    },
    External {
        href: ExternalHref,
    },
    Text {
        reason: Text512,
    },
}

#[uniffi::remote(Record)]
pub struct ListItem {
    pub checked: Option<bool>,
    pub blocks: BoundedBlock4096,
}

#[uniffi::remote(Enum)]
pub enum Alignment {
    None,
    Left,
    Center,
    Right,
}

#[uniffi::remote(Record)]
pub struct DocImage {
    pub asset_id: Option<Id>,
    pub relative_path: Option<Text1024>,
    pub hash: Option<Id>,
    pub media_type: Option<RasterType>,
    pub bytes: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: Text512,
    pub state: DocImageState,
    pub reason: Option<DocImageReason>,
    pub safe_external_href: Option<ExternalHref>,
}

#[uniffi::remote(Enum)]
pub enum RasterType {
    Png,
    Jpeg,
    Gif,
    Webp,
}

#[uniffi::remote(Enum)]
pub enum DocImageState {
    Ready,
    Remote,
    TooLarge,
    Blocked,
    Unsupported,
    Expired,
}

#[uniffi::remote(Enum)]
pub enum DocImageReason {
    Unsupported(UnsupportedImageKind),
    Blocked(BlockedImageKind),
    TooLarge(ImageLimitKind),
    Remote,
    Expired,
    Missing,
    Invalid,
    Unstable,
}

#[uniffi::remote(Enum)]
pub enum UnsupportedImageKind {
    Svg,
    Animated,
    Format,
}

#[uniffi::remote(Enum)]
pub enum BlockedImageKind {
    OutsideSource,
    NotAllowed,
}

#[uniffi::remote(Enum)]
pub enum ImageLimitKind {
    ImageBytes,
    DocumentBudget,
    AxisPixels,
    TotalPixels,
}

#[uniffi::remote(Record)]
pub struct DocFailure {
    pub document: Option<DocRef>,
    pub code: DocFailureCode,
    pub detail: Text512,
    pub bytes: Option<u32>,
}

#[uniffi::remote(Enum)]
pub enum DocAssetResult {
    Available(BoxedDocAssetChunk),
    Unavailable(DocFailure),
}

#[uniffi::remote(Record)]
pub struct DocAssetChunkData {
    pub snapshot_id: Id,
    pub asset_id: Id,
    pub hash: Id,
    pub media_type: RasterType,
    pub offset: u32,
    pub total_bytes: u32,
    pub data_base64: Text262144,
    pub next_offset: Option<u32>,
}

#[uniffi::remote(Enum)]
pub enum DocHistoryResult {
    Available(BoxedDocHistoryPage),
    Unavailable(DocFailure),
}

#[uniffi::remote(Record)]
pub struct DocHistoryPage {
    pub epoch: Id,
    pub doc_id: Id,
    pub captured_heads: BoundedDocCapturedHead64,
    pub recorded_versions: BoundedDocHistoryRow50,
    pub next_cursor: Option<Id>,
}

#[uniffi::remote(Record)]
pub struct DocCapturedHead {
    pub source_id: Id,
    pub source_generation: Wide,
    pub commit_id: Id,
}

#[uniffi::remote(Record)]
pub struct DocHistoryRow {
    pub version: DocVersion,
    pub sources: BoundedDocRef64,
}

#[uniffi::remote(Enum)]
pub enum DocDiffResult {
    Available(BoxedDocDiff),
    Unavailable(DocFailure),
}

#[uniffi::remote(Record)]
pub struct DocDiff {
    pub left: DocVersion,
    pub right: DocVersion,
    pub unified_diff: Text65536,
    pub added_lines: u32,
    pub removed_lines: u32,
    pub truncated: bool,
}

#[uniffi::remote(Record)]
pub struct DocsWatchResult {
    pub watch_id: Id,
    pub initial_index_page: DocIndexPage,
}

#[uniffi::remote(Record)]
pub struct UsageSummary {
    pub revision: Wide,

    pub query: UsageQuery,
    pub as_of: Timestamp,
    pub tracking_since: Option<Timestamp>,
    pub tracking_state: TrackingState,
    pub totals: UsageTotals,
    pub attribution_coverage: MetricCoverage,
    pub completion_cohort: Option<CompletionCohort>,

    pub executions_observed: Option<CounterMetric>,
    pub execution_statuses: Option<ExecutionStatusCounts>,
    pub limitations: BoundedCoverageReason32,
}

#[uniffi::remote(Enum)]
pub enum TrackingState {
    Tracking,
    CatchingUp,
    Partial,
    Paused,
    Closed,
    Unavailable,
}

#[uniffi::remote(Record)]
pub struct UsageTotals {
    pub input_total: CounterMetric,
    pub input_uncached: CounterMetric,
    pub cache_read: CounterMetric,
    pub cache_write: CounterMetric,
    pub cache_write_5m: CounterMetric,
    pub cache_write_1h: CounterMetric,
    pub output: CounterMetric,
    pub total_tokens: CounterMetric,
    pub hit_rate: RatioMetric,
    pub write_share: RatioMetric,
    pub estimate: CostEstimate,
    pub requests_observed: CounterMetric,
    pub turns_observed: CounterMetric,
}

#[uniffi::remote(Record)]
pub struct CounterMetric {
    pub known_sum: Option<Wide>,
    pub state: MetricState,
    pub reason: Option<CoverageReason>,
    pub coverage: MetricCoverage,
}

#[uniffi::remote(Enum)]
pub enum MetricState {
    Complete,
    Partial,

    Unavailable,
}

#[uniffi::remote(Record)]
pub struct RatioMetric {
    pub numerator: Option<Wide>,
    pub denominator: Option<Wide>,

    pub value: Option<Fraction>,

    pub lower: Option<RatioBound>,
    pub upper: Option<RatioBound>,
    pub state: MetricState,
    pub reason: Option<CoverageReason>,
    pub coverage: MetricCoverage,
}

#[uniffi::remote(Record)]
pub struct RatioBound {
    pub numerator: Wide,
    pub denominator: Wide,
    pub value: Fraction,
}

#[uniffi::remote(Record)]
pub struct CompletionCohort {
    pub first_completions: Wide,
    pub membership_complete: bool,
    pub tasks_with_observed_cost: Wide,
    pub membership_coverage: MetricCoverage,
    pub cost_per_completed_task: CostEstimate,
    pub includes_earlier_spend: bool,
}

#[uniffi::remote(Record)]
pub struct ExecutionStatusCounts {
    pub answered: Wide,
    pub failed: Wide,
    pub cancelled: Wide,
    pub still_answering: Wide,
    pub unknown: Wide,
    pub complete: bool,
}

#[uniffi::remote(Record)]
pub struct UsageSeries {
    pub revision: Wide,
    pub query: UsageQuery,
    pub rows: BoundedUsageBucket720,
    pub next_cursor: Option<Id>,
}

#[uniffi::remote(Record)]
pub struct UsageBucket {
    pub from_ms: Timestamp,
    pub to_ms: Timestamp,
    pub totals: UsageTotals,
    pub category_split_complete: bool,
}

#[uniffi::remote(Record)]
pub struct UsageBreakdown {
    pub revision: Wide,
    pub query: UsageQuery,
    pub rows: BoundedUsageBreakdownRow200,
    pub next_cursor: Option<Id>,
}

#[uniffi::remote(Record)]
pub struct UsageBreakdownRow {
    pub key: UsageGroupKey,
    pub totals: UsageTotals,
    pub activities: BoundedActivityKind8,
    pub reasons: BoundedCoverageExclusion32,
}

#[uniffi::remote(Enum)]
pub enum UsageGroupKey {
    Agent {
        id: String,
        incarnation: Id,
    },
    Task {
        id: String,
    },
    Epic {
        id: String,
    },
    Model {
        model: Text512,
        provider: Option<Text512>,
    },
    Account {
        id: Id,
    },
    Runtime {
        runtime: RuntimeKind,
    },
    NoEpic,
    NoTask,
    NotAttributed,
}

#[uniffi::remote(Enum)]
pub enum ActivityKind {
    Work,
    Orchestration,
    Maintenance,
    Keepalive,
    Aside,
}

#[uniffi::remote(Record)]
pub struct UsageObservations {
    pub revision: Wide,
    pub query: UsageQuery,
    pub rows: BoundedUsageObservation200,
    pub next_cursor: Option<Id>,
}

#[uniffi::remote(Record)]
pub struct UsageObservation {
    pub id: Id,
    pub revision: Wide,
    pub unit: ObservationUnit,
    pub binding: UsageBinding,
    pub time_basis: TimeBasis,
    pub native_time: Option<Timestamp>,
    pub observed_at: Timestamp,
    pub counters: UsageCounters,
    pub validity: BoundedCoverageReason32,
    pub diagnostic_codes: BoundedText51232,
    pub event_refs: BoundedWide32,
}

#[uniffi::remote(Enum)]
pub enum ObservationUnit {
    Request,
    CounterDelta,

    Unusable,
}

#[uniffi::remote(Record)]
pub struct UsageBinding {
    pub workspace_run_id: Id,
    pub source_id: Id,
    pub kind: UsageKind,
    pub agent_id: String,
    pub incarnation: Id,
    pub launch_id: Id,
    pub native_session_id: Option<Id>,
    pub native_turn_id: Option<Id>,
    pub aside_execution_id: Option<Id>,
    pub runtime: RuntimeKind,
    pub cli_version: Option<Text512>,
    pub model: Option<Text512>,
    pub provider: Option<Text512>,
    pub work: WorkAttribution,
    pub account: AccountAttribution,
    pub board_revision: Option<Wide>,
    pub event_seq: Option<Wide>,
}

#[uniffi::remote(Enum)]
pub enum WorkAttribution {
    Task { id: String, epic: EpicAttribution },
    NoTask { activity: ActivityKind },
    Unknown { reason: CoverageReason },
}

#[uniffi::remote(Enum)]
pub enum EpicAttribution {
    Epic { id: String },
    NoEpic,
    Unknown { reason: CoverageReason },
}

#[uniffi::remote(Enum)]
pub enum AccountAttribution {
    Known {
        id: Id,
        assignment_generation: Wide,
        interval_id: Id,
    },
    Unknown {
        reason: CoverageReason,
    },
}

#[uniffi::remote(Enum)]
pub enum TimeBasis {
    Native,
    CorrelatedTurn,
    Unknown,
}

#[uniffi::remote(Record)]
pub struct UsageRuns {
    pub rows: BoundedUsageRun200,
    pub next_cursor: Option<Id>,
}

#[uniffi::remote(Record)]
pub struct UsageRun {
    pub workspace_run_id: Id,
    pub started_at: Option<Timestamp>,
    pub tracking_since: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub state: TrackingState,
    pub coverage: MetricCoverage,
}

#[uniffi::remote(Record)]
pub struct PriceBook {
    pub id: Id,
    pub checked_at: Timestamp,
    pub basis: CostBasis,
    pub rows: BoundedPriceRow200,
    pub sources: BoundedText204816,
}

#[uniffi::remote(Record)]
pub struct PriceRow {
    pub provider: Text512,
    pub model: Text512,

    pub above_input_tokens: Option<Wide>,

    pub input_pico_usd_per_million: Wide,
    pub output_pico_usd_per_million: Wide,
    pub cache_read_pico_usd_per_million: Option<Wide>,
    pub cache_write_pico_usd_per_million: Option<Wide>,
    pub cache_write_5m_pico_usd_per_million: Option<Wide>,
    pub cache_write_1h_pico_usd_per_million: Option<Wide>,
}

#[uniffi::remote(Record)]
pub struct UsageWatchResult {
    pub epoch: Id,
    pub watch_id: Id,
    pub initial: UsageSummary,
}

#[uniffi::remote(Enum)]
pub enum FeatureChange {
    Asides(AsideChanged),
    Docs(DocsChanged),
    Usage(UsageChanged),
}

#[uniffi::remote(Record)]
pub struct AsideChanged {
    pub workspace_run_id: Id,
    pub aside_epoch: Id,
    pub watch_id: Id,
    pub from_revision: Wide,
    pub to_revision: Wide,
    pub changed_ids: BoundedId200,
    pub gone: BoundedAsideGone200,
    pub availability_changed: bool,
    pub resync_required: bool,
}

#[uniffi::remote(Record)]
pub struct AsideGone {
    pub id: Id,
    pub cause: AsideGoneCause,
}

#[uniffi::remote(Record)]
pub struct DocsChanged {
    pub epoch: Id,
    pub watch_id: Id,
    pub from_revision: Wide,
    pub to_revision: Wide,
    pub upserts: BoundedDocEntry200,
    pub removed_doc_ids: BoundedId200,
    pub changed_selected: BoundedDocSelectedChange2,
    pub history_sources_changed: BoundedId64,

    pub expired_seen_versions: BoundedId200,
    pub resync_required: bool,
}

#[uniffi::remote(Record)]
pub struct DocSelectedChange {
    pub document: DocRef,
    pub kind: DocChangeKind,
}

#[uniffi::remote(Enum)]
pub enum DocChangeKind {
    Content,
    Image,
    Removed,
    Policy,
    Availability,
}

#[uniffi::remote(Record)]
pub struct UsageChanged {
    pub workspace_run_id: Id,
    pub epoch: Id,
    pub watch_id: Id,
    pub from_revision: Wide,
    pub to_revision: Wide,
    pub resync_required: bool,
}

#[uniffi::remote(Enum)]
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

#[uniffi::remote(Enum)]
pub enum FeatureReadRequest {
    EpicBoard(RunScope),
    EpicDetail(EpicGet),
    Tasks(TasksQuery),
    TaskCard(TaskGetScoped),
    AsideMembers(AsideList),
    Asides(AsideList),
    AsideDetail(AsideGet),
    DocsIndex(PageRequest),
    Document(DocsRead),
    DocAsset(DocsAsset),
    DocHistory(DocsHistory),
    DocDiff(DocsDiff),
    UsageSummary(UsageQuery),
    UsageSeries(UsageQuery),
    UsageBreakdown(UsageQuery),
    UsageObservations(UsageQuery),
    UsageRuns(PageRequest),
    UsagePrices(UsagePrices),
}

#[uniffi::remote(Record)]
pub struct EpicGet {
    pub workspace_run_id: Id,
    pub id: String,
}

#[uniffi::remote(Record)]
pub struct TasksQuery {
    pub workspace_run_id: Id,
    pub epic: EpicFilter,
}

#[uniffi::remote(Enum)]
pub enum EpicFilter {
    All,
    Id(String),
    NoEpic,
}

#[uniffi::remote(Record)]
pub struct TaskGetScoped {
    pub workspace_run_id: Id,
    pub id: Option<String>,
}

#[uniffi::remote(Record)]
pub struct DocsRead {
    pub document: DocRef,
    pub version: DocVersionSelector,
}

#[uniffi::remote(Enum)]
pub enum DocVersionSelector {
    Current,
    Id(Id),
}

#[uniffi::remote(Record)]
pub struct DocsAsset {
    pub snapshot_id: Id,
    pub asset_id: Id,
    pub offset: u32,
    pub max_bytes: u32,
}

#[uniffi::remote(Record)]
pub struct DocsHistory {
    pub doc_id: Id,
    pub source_ids: Option<BoundedId64>,
    pub page: PageRequest,
}

#[uniffi::remote(Record)]
pub struct DocsDiff {
    pub left: Id,
    pub right: Id,
}

#[uniffi::remote(Record)]
pub struct UsagePrices {
    pub price_book_id: Option<Id>,
}

#[uniffi::remote(Record)]
pub struct FeatureRead {
    pub request: FeatureReadRequest,
    pub lease: Option<DocLease>,
}

#[uniffi::remote(Record)]
pub struct DocLease {
    pub connection_epoch: Id,
    pub docs_epoch: Id,
}

#[uniffi::remote(Record)]
pub struct FeatureResourceValue {
    pub connection_epoch: Id,
    pub lease: Option<DocLease>,
    pub response: FeatureResponse,
}
