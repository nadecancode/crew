//! Platform credential stores expose bounded proofs, never private key bytes.

use std::future::Future;
use std::pin::Pin;

use crew_protocol::native::{self, PublicKey, Signature};

pub type SignerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SignerError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignerError {
    Locked,
    Unavailable,
    Canceled,
    Retired,
    InvalidProof,
}

/// Constructed by the connector after TLS identity verification. Foreign
/// bindings may expose an opaque object and its bounded signing preimage;
/// there must be no foreign constructor accepting a view-supplied byte string.
#[derive(Clone)]
pub struct AuthTranscript(pub(super) Vec<u8>);

impl AuthTranscript {
    pub fn signing_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for AuthTranscript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthTranscript(<redacted>)")
    }
}

/// The enrollment domain has exactly 174 bytes. Private construction also
/// prevents the OS callback from becoming an arbitrary signing API.
#[derive(Clone)]
pub struct EnrollTranscript(pub(super) [u8; native::ENROLL_TRANSCRIPT_BYTES]);

impl EnrollTranscript {
    pub fn signing_bytes(&self) -> &[u8; native::ENROLL_TRANSCRIPT_BYTES] {
        &self.0
    }
}

impl std::fmt::Debug for EnrollTranscript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EnrollTranscript(<redacted>)")
    }
}

/// Unlock/public-key retrieval happens before the network authentication
/// deadline. Only public_key may unlock or request user presence. sign_auth
/// and sign_enrollment must not prompt: they return Locked/Unavailable/Canceled
/// promptly when signing cannot proceed within the connector's 5 s deadline.
/// Dropping a future cancels its work. Implementations must neither log the
/// transcript nor export a private key or platform store handle.
pub trait DeviceSigner: Send + Sync + 'static {
    fn public_key(&self) -> SignerFuture<'_, PublicKey>;
    fn sign_auth(&self, transcript: AuthTranscript) -> SignerFuture<'_, Signature>;
    fn sign_enrollment(&self, transcript: EnrollTranscript) -> SignerFuture<'_, Signature>;
    /// Local retirement alone does not revoke a host's device grant.
    fn retire(&self) -> SignerFuture<'_, ()>;
}
