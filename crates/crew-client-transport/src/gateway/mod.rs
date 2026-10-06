//! Scoped device connections. TLS, proofs, enrollment and grants stay in Rust;
//! only typed setup progress and the shared NativeSession cross native bridges.

mod authority;
mod connector;
mod profile;
mod setup;
mod signer;
mod wire;

#[cfg(target_os = "linux")]
mod memory;
#[cfg(target_os = "linux")]
pub use memory::MemoryDeviceSigner;

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "gateway-tests"))]
mod tls_tests;

pub use profile::GatewayProfile;
pub use setup::{
    EnrollmentOutcome, EnrollmentProgress, EnrollmentResult, EnrollmentUpdate, GatewayClient,
    GatewayDeviceAccess, SetupContext, SetupObserver,
};
pub use signer::{AuthTranscript, DeviceSigner, EnrollTranscript, SignerError, SignerFuture};

use crate::TransportEnd;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayError {
    InvalidProfile,
    Trust,
    Signer(SignerError),
    Transport(TransportEnd),
    Busy,
}

impl GatewayError {
    pub(super) fn transport_end(self) -> TransportEnd {
        match self {
            Self::Transport(end) => end,
            Self::Trust => TransportEnd::TlsTrust,
            Self::Signer(SignerError::Locked) => TransportEnd::CredentialLocked,
            Self::Signer(SignerError::Unavailable) => TransportEnd::CredentialUnavailable,
            Self::Signer(SignerError::Canceled | SignerError::Retired) => TransportEnd::Stopped,
            Self::Signer(SignerError::InvalidProof) => TransportEnd::Authentication,
            Self::InvalidProfile => TransportEnd::Permission,
            Self::Busy => TransportEnd::Overflow,
        }
    }
    pub fn reason(self) -> crew_client::api::ClientReason {
        let (code, message) = match self {
            Self::Transport(reason) => return reason.reason(),
            Self::InvalidProfile => ("invalid_profile", "The public host profile is invalid."),
            Self::Trust => ("tls_trust", "The host identity could not be verified."),
            Self::Signer(SignerError::Locked) => {
                ("key_locked", "Unlock device access before connecting.")
            }
            Self::Signer(SignerError::Unavailable) => {
                ("key_unavailable", "Device secure storage is unavailable.")
            }
            Self::Signer(SignerError::Canceled) => ("key_canceled", "Device access was canceled."),
            Self::Signer(SignerError::Retired) => {
                ("key_retired", "This device key was retired locally.")
            }
            Self::Signer(SignerError::InvalidProof) => {
                ("invalid_proof", "Device access could not be verified.")
            }
            Self::Busy => ("setup_busy", "Device setup is already in progress."),
        };
        crew_client::api::ClientReason {
            code: code.into(),
            message: message.into(),
        }
    }
}
