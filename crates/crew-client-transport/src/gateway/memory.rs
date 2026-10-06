//! Explicit Linux memory-only choice. No automatic plaintext fallback exists.

use std::sync::Mutex;

use crew_protocol::native::{HexBytes, PublicKey, Signature};
use ed25519_dalek::Signer as _;
use zeroize::Zeroize;

use super::{AuthTranscript, DeviceSigner, EnrollTranscript, SignerError, SignerFuture};

/// A session key for users who explicitly choose memory-only device access.
/// Closing the process loses this key; host records must be revoked separately.
pub struct MemoryDeviceSigner(Mutex<Option<ed25519_dalek::SigningKey>>);

impl MemoryDeviceSigner {
    pub fn new() -> Result<Self, SignerError> {
        let mut seed = [0; 32];
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(&mut seed)
            .map_err(|_| SignerError::Unavailable)?;
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        seed.zeroize();
        Ok(Self(Mutex::new(Some(key))))
    }

    fn proof(&self, bytes: &[u8]) -> Result<Signature, SignerError> {
        let key = self.0.lock().map_err(|_| SignerError::Unavailable)?;
        Ok(HexBytes(
            key.as_ref()
                .ok_or(SignerError::Retired)?
                .sign(bytes)
                .to_bytes(),
        ))
    }
}

impl std::fmt::Debug for MemoryDeviceSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MemoryDeviceSigner(<redacted>)")
    }
}

impl DeviceSigner for MemoryDeviceSigner {
    fn public_key(&self) -> SignerFuture<'_, PublicKey> {
        Box::pin(async move {
            let key = self.0.lock().map_err(|_| SignerError::Unavailable)?;
            Ok(HexBytes(
                key.as_ref()
                    .ok_or(SignerError::Retired)?
                    .verifying_key()
                    .to_bytes(),
            ))
        })
    }
    fn sign_auth(&self, transcript: AuthTranscript) -> SignerFuture<'_, Signature> {
        Box::pin(async move { self.proof(transcript.signing_bytes()) })
    }
    fn sign_enrollment(&self, transcript: EnrollTranscript) -> SignerFuture<'_, Signature> {
        Box::pin(async move { self.proof(transcript.signing_bytes()) })
    }
    fn retire(&self) -> SignerFuture<'_, ()> {
        Box::pin(async move {
            // SigningKey's selected zeroize feature clears it on drop.
            self.0.lock().map_err(|_| SignerError::Unavailable)?.take();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_reveal_synthetic_seed_and_retirement_erases_access() {
        let signer = MemoryDeviceSigner(Mutex::new(Some(ed25519_dalek::SigningKey::from_bytes(
            &[b'm'; 32],
        ))));
        assert!(!format!("{signer:?}").contains(&"m".repeat(32)));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(signer.public_key().await.is_ok());
            signer.retire().await.unwrap();
            assert_eq!(signer.public_key().await, Err(SignerError::Retired));
        });
    }
}
