//! Gateway records and opaque trusted values for the foreign interface.

use std::fmt;
use std::sync::Arc;

use crew_client_transport::gateway as transport;
use crew_protocol::native;

use crate::records::ClientReason;

/// Flatten the transport error hierarchy without inventing another error
/// policy. Codes and messages come from the reviewed transport's `reason`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Error)]
pub enum GatewayError {
    Failure { code: String, message: String },
}

impl From<transport::GatewayError> for GatewayError {
    fn from(error: transport::GatewayError) -> Self {
        let ClientReason {
            code,
            message,
            details: _,
        } = error.reason();
        Self::Failure { code, message }
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failure { code, message } => write!(f, "{code}: {message}"),
        }
    }
}

impl std::error::Error for GatewayError {}

pub(crate) fn invalid_profile() -> GatewayError {
    transport::GatewayError::InvalidProfile.into()
}

pub(crate) fn stopped() -> GatewayError {
    transport::GatewayError::Transport(crew_client_transport::TransportEnd::Stopped).into()
}

/// A Rust panic becomes the finite `stopped` failure; no panic payload or
/// partially built gateway object crosses the foreign interface.
pub(crate) fn gateway_boundary<T>(
    work: impl FnOnce() -> Result<T, GatewayError>,
) -> Result<T, GatewayError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_| Err(stopped()))
}

/// Exact public profile fields, with the native fixed-width hex values lowered
/// as strings. Conversion occurs inside the throwing trust method so malformed
/// import data gets a typed gateway error rather than a foreign lift failure.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct PublicProfile {
    pub endpoint_id: String,
    pub name: String,
    pub url: String,
    pub tls_leaf_sha256: String,
    pub trust_pem: String,
}

impl From<native::PublicProfile> for PublicProfile {
    fn from(value: native::PublicProfile) -> Self {
        let native::PublicProfile {
            endpoint_id,
            name,
            url,
            tls_leaf_sha256,
            trust_pem,
        } = value;
        Self {
            endpoint_id: endpoint_id.to_hex(),
            name,
            url,
            tls_leaf_sha256: tls_leaf_sha256.to_hex(),
            trust_pem,
        }
    }
}

impl TryFrom<PublicProfile> for native::PublicProfile {
    type Error = GatewayError;

    fn try_from(value: PublicProfile) -> Result<Self, Self::Error> {
        Ok(Self {
            endpoint_id: native::EndpointId::from_hex(&value.endpoint_id)
                .map_err(|_| invalid_profile())?,
            name: value.name,
            url: value.url,
            tls_leaf_sha256: native::LeafHash::from_hex(&value.tls_leaf_sha256)
                .map_err(|_| invalid_profile())?,
            trust_pem: value.trust_pem,
        })
    }
}

impl fmt::Debug for PublicProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublicProfile(<imported public metadata>)")
    }
}

/// The `crew native status` result wraps the profile with the endpoint epoch.
/// Only that exact shape is accepted; the epoch is not part of the profile.
fn native_status(text: &str) -> Option<native::PublicProfile> {
    let serde_json::Value::Object(mut status) = serde_json::from_str(text).ok()? else {
        return None;
    };
    let epoch = status.remove("endpoint_epoch")?;
    let profile = status.remove("profile")?;
    if !status.is_empty() {
        return None;
    }
    serde_json::from_value::<native::HexBytes<16>>(epoch).ok()?;
    serde_json::from_value(profile).ok()
}

/// Decode an imported host profile with the real native protocol schema:
/// either the bare public profile or the complete `crew native status`
/// output. This is bounded configuration import, not a JSON app-state or
/// frame interface, and it does not make the profile trusted.
#[uniffi::export]
pub fn parse_gateway_profile(text: String) -> Result<PublicProfile, GatewayError> {
    if text.len() > crew_protocol::types::MAX_FRAME_BYTES {
        return Err(invalid_profile());
    }
    serde_json::from_str::<native::PublicProfile>(&text)
        .ok()
        .or_else(|| native_status(&text))
        .map(Into::into)
        .ok_or_else(invalid_profile)
}

/// The canonical form of a fingerprint a person typed or pasted: ASCII
/// whitespace and colons removed, letters lowercased. Returns None unless the
/// result is exactly 64 hex digits, so a form can flag the fingerprint field
/// itself before `GatewayProfile.trust` compares it.
#[uniffi::export]
pub fn normalize_fingerprint(text: String) -> Option<String> {
    if text.len() > 512 {
        return None;
    }
    let canonical: String = text
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ':')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    native::LeafHash::from_hex(&canonical)
        .ok()
        .map(|_| canonical)
}

/// A pinned host profile. It exists only after an explicit fingerprint
/// comparison; nothing else constructs one.
#[derive(uniffi::Object)]
pub struct GatewayProfile {
    pub(crate) inner: transport::GatewayProfile,
}

#[uniffi::export]
impl GatewayProfile {
    /// `verified_fingerprint` is the leaf SHA-256 that the person read from
    /// the trusted host's own `crew native status` output, in any form
    /// `normalize_fingerprint` accepts. Never pass the imported profile's
    /// `tls_leaf_sha256`: a mismatch is the `tls_trust` failure that protects
    /// against a substituted profile. A malformed fingerprint or profile is
    /// `invalid_profile`. `id` is the app's stable local name for this
    /// profile, later used by `connect(profileId:)`.
    #[uniffi::constructor]
    pub fn trust(
        id: String,
        profile: PublicProfile,
        verified_fingerprint: String,
    ) -> Result<Arc<Self>, GatewayError> {
        let fingerprint = normalize_fingerprint(verified_fingerprint)
            .and_then(|text| native::LeafHash::from_hex(&text).ok())
            .ok_or_else(invalid_profile)?;
        let profile = transport::GatewayProfile::trust(id, profile.try_into()?, fingerprint)?;
        Ok(Arc::new(Self { inner: profile }))
    }

    pub fn profile_id(&self) -> String {
        self.inner.profile_id().into()
    }

    pub fn public_profile(&self) -> PublicProfile {
        self.inner.public_profile().clone().into()
    }
}

impl fmt::Debug for GatewayProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GatewayProfile(<trusted public metadata>)")
    }
}

pub use transport::{
    EnrollmentOutcome, EnrollmentProgress, EnrollmentResult, EnrollmentUpdate, GatewayDeviceAccess,
    SetupContext, SignerError,
};

#[uniffi::remote(Enum)]
pub enum SignerError {
    Locked,
    Unavailable,
    Canceled,
    Retired,
    InvalidProof,
}

#[uniffi::remote(Record)]
pub struct SetupContext {
    pub profile_id: String,
    pub endpoint_id: String,
    pub attempt: u64,
}

#[uniffi::remote(Record)]
pub struct GatewayDeviceAccess {
    pub endpoint_id: String,
    pub device_id: String,
    pub credential_expires_at_ms: i64,
}

#[uniffi::remote(Enum)]
pub enum EnrollmentProgress {
    Connecting,
    AwaitingApproval {
        enrollment_id: String,
        comparison_code: String,
        expires_at_ms: i64,
    },
}

#[uniffi::remote(Record)]
pub struct EnrollmentUpdate {
    pub context: SetupContext,
    pub progress: EnrollmentProgress,
}

#[uniffi::remote(Enum)]
pub enum EnrollmentOutcome {
    Approved { access: GatewayDeviceAccess },
    Canceled,
    OutcomeUnknown,
    Failed { reason: ClientReason },
}

#[uniffi::remote(Record)]
pub struct EnrollmentResult {
    pub context: SetupContext,
    pub outcome: EnrollmentOutcome,
}

/// Rust constructs this object only from the connector's sealed transcript.
/// It has no foreign constructor or method accepting caller-chosen bytes.
#[derive(uniffi::Object)]
pub struct AuthTranscript {
    pub(crate) inner: transport::AuthTranscript,
}

#[uniffi::export]
impl AuthTranscript {
    pub fn signing_bytes(&self) -> Vec<u8> {
        self.inner.signing_bytes().to_vec()
    }
}

impl fmt::Debug for AuthTranscript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthTranscript(<redacted>)")
    }
}

#[derive(uniffi::Object)]
pub struct EnrollTranscript {
    pub(crate) inner: transport::EnrollTranscript,
}

#[uniffi::export]
impl EnrollTranscript {
    pub fn signing_bytes(&self) -> Vec<u8> {
        self.inner.signing_bytes().to_vec()
    }
}

impl fmt::Debug for EnrollTranscript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnrollTranscript(<redacted>)")
    }
}
