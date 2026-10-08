//! Negotiated v6 feature records. Defining a record does not enable a service.
//!
//! Host IO, authorization, connection state and capability advertisement belong
//! to the daemon/client, not this portable crate.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const FEATURE_VERSION: u32 = 1;
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
pub const MAX_METADATA_ROWS: usize = 200;
pub const MAX_FEATURES: usize = 16;
/// Unlike old decoders' `invalid`, this means a recognized but malformed call.
pub const INVALID_FEATURE_REQUEST: &str = "invalid_feature_request";
pub const UNSUPPORTED_FEATURE: &str = "unsupported_feature";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Epics,
    Asides,
    Docs,
    UsageMetrics,
}

impl Feature {
    pub const ALL: [Self; 4] = [Self::Epics, Self::Asides, Self::Docs, Self::UsageMetrics];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Epics => "epics",
            Self::Asides => "asides",
            Self::Docs => "docs",
            Self::UsageMetrics => "usage_metrics",
        }
    }
}

/// Exact nonnegative integer. JSON numbers cannot preserve all counter bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Wide(pub u128);

/// Finite display fraction. Exact integer operands accompany it in metrics.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Fraction(f64);

impl Fraction {
    pub fn new(value: f64) -> Result<Self, &'static str> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err("fraction must be finite and between zero and one")
        }
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Fraction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(f64::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl Serialize for Wide {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Wide {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.is_empty()
            || text.len() > 39
            || !text.bytes().all(|b| b.is_ascii_digit())
            || (text.len() > 1 && text.starts_with('0'))
        {
            return Err(de::Error::custom(
                "expected canonical unsigned decimal string",
            ));
        }
        text.parse().map(Self).map_err(de::Error::custom)
    }
}

/// Milliseconds with an exact representation in browser/native JSON consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const MAX: i64 = 9_007_199_254_740_991;

    pub fn new(ms: i64) -> Result<Self, &'static str> {
        if (-Self::MAX..=Self::MAX).contains(&ms) {
            Ok(Self(ms))
        } else {
            Err("timestamp outside exact JSON integer range")
        }
    }

    pub const fn get(self) -> i64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// Byte-bounded UTF-8 content. Debug never emits document, question or answer text.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Text<const MAX: usize>(String);

impl<const MAX: usize> Text<MAX> {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.len() > MAX {
            return Err("text exceeds byte limit");
        }
        Ok(Self(value))
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl<const MAX: usize> Deref for Text<MAX> {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const MAX: usize> fmt::Debug for Text<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} UTF-8 bytes>", self.0.len())
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for Text<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// Opaque identifier, never path or bearer authority. Scope checks remain mandatory.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Id(String);

impl Id {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err("invalid opaque identifier");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// Bound collection growth during decoding, not after accepting an arbitrary array.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct List<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> Default for List<T, MAX> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T, const MAX: usize> List<T, MAX> {
    pub fn new(values: Vec<T>) -> Result<Self, &'static str> {
        if values.len() > MAX {
            return Err("collection exceeds entry limit");
        }
        Ok(Self(values))
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

impl<T, const MAX: usize> Deref for List<T, MAX> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for List<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Items<T, const MAX: usize>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for Items<T, MAX> {
            type Value = List<T, MAX>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "at most {MAX} entries")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX));
                while let Some(item) = seq.next_element()? {
                    if values.len() == MAX {
                        return Err(de::Error::custom("collection exceeds entry limit"));
                    }
                    values.push(item);
                }
                Ok(List(values))
            }
        }
        deserializer.deserialize_seq(Items::<T, MAX>(std::marker::PhantomData))
    }
}

pub type Label = Text<512>;
pub type Content = Text<65536>;

/// Unknown names are retained for negotiation, never silently enabled.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BTreeMap<String, u32>")]
pub struct FeatureVersions(BTreeMap<String, u32>);

impl Deref for FeatureVersions {
    type Target = BTreeMap<String, u32>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<BTreeMap<String, u32>> for FeatureVersions {
    type Error = &'static str;
    fn try_from(value: BTreeMap<String, u32>) -> Result<Self, Self::Error> {
        if value.len() > MAX_FEATURES
            || value
                .keys()
                .any(|k| k.is_empty() || k.len() > 32 || k.chars().any(char::is_control))
        {
            return Err("invalid feature version map");
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeaturesNegotiate {
    pub versions: FeatureVersions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureUnavailable {
    Unsupported,
    UnsupportedVersion,
    NotAuthorized,
    NotReady,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BTreeMap<String, FeatureUnavailable>")]
pub struct UnavailableFeatures(BTreeMap<String, FeatureUnavailable>);

impl TryFrom<BTreeMap<String, FeatureUnavailable>> for UnavailableFeatures {
    type Error = &'static str;
    fn try_from(value: BTreeMap<String, FeatureUnavailable>) -> Result<Self, Self::Error> {
        if value.len() > MAX_FEATURES
            || value
                .keys()
                .any(|k| k.is_empty() || k.len() > 32 || k.chars().any(char::is_control))
        {
            return Err("invalid unavailable feature map");
        }
        Ok(Self(value))
    }
}

impl Deref for UnavailableFeatures {
    type Target = BTreeMap<String, FeatureUnavailable>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureSession {
    pub accepted: FeatureVersions,
    pub connection_epoch: Id,
    pub workspace_run_id: Option<Id>,
    pub unavailable: UnavailableFeatures,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScope {
    pub workspace_run_id: Id,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    pub cursor: Option<Id>,
    pub limit: Option<u16>,
}

impl PageRequest {
    pub fn validate(&self, max: u16) -> Result<(), &'static str> {
        if self.limit.is_some_and(|n| n == 0 || n > max) {
            Err("invalid page limit")
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unwatch {
    pub watch_id: Id,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnwatchResult {
    pub watch_id: Id,
    pub removed: bool,
}

/// Serialize the complete envelope, including escaping and its newline, bounded.
/// The caller supplies the actual frame, never only an inner result record.
pub fn encode_frame<T: Serialize>(frame: &T) -> Result<Vec<u8>, serde_json::Error> {
    struct Limited(Vec<u8>);
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) >= crate::MAX_FRAME_BYTES - 1 {
                return Err(std::io::Error::other(
                    "feature frame exceeds transport limit",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Limited(Vec::new());
    serde_json::to_writer(&mut out, frame)?;
    out.0.push(b'\n');
    Ok(out.0)
}

/// Routing inventory only; never a grant or an enabled-service list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureAccess {
    Discovery,
    CollaborativeRead,
    Administration,
    ObserverRead,
    ObserverAction,
    ObserverWatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureMethod {
    pub name: &'static str,
    pub feature: Option<Feature>,
    pub access: FeatureAccess,
}

pub const FEATURE_METHODS: &[FeatureMethod] = &[
    FeatureMethod {
        name: "features_negotiate",
        feature: None,
        access: FeatureAccess::Discovery,
    },
    FeatureMethod {
        name: "epics_list",
        feature: Some(Feature::Epics),
        access: FeatureAccess::CollaborativeRead,
    },
    FeatureMethod {
        name: "epic_get",
        feature: Some(Feature::Epics),
        access: FeatureAccess::CollaborativeRead,
    },
    FeatureMethod {
        name: "tasks_query",
        feature: Some(Feature::Epics),
        access: FeatureAccess::CollaborativeRead,
    },
    FeatureMethod {
        name: "task_get_scoped",
        feature: Some(Feature::Epics),
        access: FeatureAccess::CollaborativeRead,
    },
    FeatureMethod {
        name: "epic_create",
        feature: Some(Feature::Epics),
        access: FeatureAccess::Administration,
    },
    FeatureMethod {
        name: "epic_edit",
        feature: Some(Feature::Epics),
        access: FeatureAccess::Administration,
    },
    FeatureMethod {
        name: "task_create_scoped",
        feature: Some(Feature::Epics),
        access: FeatureAccess::Administration,
    },
    FeatureMethod {
        name: "task_revise",
        feature: Some(Feature::Epics),
        access: FeatureAccess::Administration,
    },
    FeatureMethod {
        name: "epic_complete",
        feature: Some(Feature::Epics),
        access: FeatureAccess::Administration,
    },
    FeatureMethod {
        name: "aside_members",
        feature: Some(Feature::Asides),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "aside_list",
        feature: Some(Feature::Asides),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "aside_get",
        feature: Some(Feature::Asides),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "aside_ask",
        feature: Some(Feature::Asides),
        access: FeatureAccess::ObserverAction,
    },
    FeatureMethod {
        name: "aside_cancel",
        feature: Some(Feature::Asides),
        access: FeatureAccess::ObserverAction,
    },
    FeatureMethod {
        name: "aside_remove",
        feature: Some(Feature::Asides),
        access: FeatureAccess::ObserverAction,
    },
    FeatureMethod {
        name: "aside_watch",
        feature: Some(Feature::Asides),
        access: FeatureAccess::ObserverWatch,
    },
    FeatureMethod {
        name: "aside_unwatch",
        feature: Some(Feature::Asides),
        access: FeatureAccess::ObserverWatch,
    },
    FeatureMethod {
        name: "docs_index",
        feature: Some(Feature::Docs),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "docs_read",
        feature: Some(Feature::Docs),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "docs_asset",
        feature: Some(Feature::Docs),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "docs_history",
        feature: Some(Feature::Docs),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "docs_diff",
        feature: Some(Feature::Docs),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "docs_watch",
        feature: Some(Feature::Docs),
        access: FeatureAccess::ObserverWatch,
    },
    FeatureMethod {
        name: "docs_unwatch",
        feature: Some(Feature::Docs),
        access: FeatureAccess::ObserverWatch,
    },
    FeatureMethod {
        name: "usage_summary",
        feature: Some(Feature::UsageMetrics),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "usage_series",
        feature: Some(Feature::UsageMetrics),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "usage_breakdown",
        feature: Some(Feature::UsageMetrics),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "usage_observations",
        feature: Some(Feature::UsageMetrics),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "usage_runs",
        feature: Some(Feature::UsageMetrics),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "usage_prices",
        feature: Some(Feature::UsageMetrics),
        access: FeatureAccess::ObserverRead,
    },
    FeatureMethod {
        name: "usage_watch",
        feature: Some(Feature::UsageMetrics),
        access: FeatureAccess::ObserverWatch,
    },
    FeatureMethod {
        name: "usage_unwatch",
        feature: Some(Feature::UsageMetrics),
        access: FeatureAccess::ObserverWatch,
    },
];

pub fn feature_method(name: &str) -> Option<&'static FeatureMethod> {
    FEATURE_METHODS.iter().find(|m| m.name == name)
}
