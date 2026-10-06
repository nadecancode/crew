//! Device proof verification and bounded, host-approved enrollment intents.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crew_protocol::native::{
    auth_transcript, enroll_transcript, AuthParams, ComparisonCode, EndpointId, EnrollParams,
    EnrollmentAccepted, EnrollmentId, HexBytes, LeafHash, Nonce, PublicKey,
};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

const WINDOW: Duration = Duration::from_secs(120);

pub(crate) fn verify_auth(
    endpoint: EndpointId,
    leaf: LeafHash,
    nonce: Nonce,
    params: &AuthParams,
) -> bool {
    let Ok(bytes) = auth_transcript(endpoint, leaf, nonce, params) else {
        return false;
    };
    verify(params.public_key, &params.signature.0, &bytes)
}

fn verify(key: PublicKey, signature: &[u8; 64], bytes: &[u8]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&key.0) else {
        return false;
    };
    !key.is_weak()
        && key
            .verify_strict(bytes, &Signature::from_bytes(signature))
            .is_ok()
}

pub(crate) fn key_hash(key: PublicKey) -> HexBytes<32> {
    HexBytes(Sha256::digest(key.0).into())
}

#[derive(Clone)]
pub(crate) struct Intent {
    pub id: EnrollmentId,
    pub public_key: PublicKey,
    pub label: String,
    pub comparison: ComparisonCode,
    pub expires_at: i64,
    pub deadline: Instant,
    pub connection: u64,
    failures: u8,
}

impl std::fmt::Debug for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Intent")
            .field("material", &"<redacted>")
            .finish()
    }
}

impl Intent {
    pub fn accepted(&self) -> EnrollmentAccepted {
        EnrollmentAccepted {
            enrollment_id: self.id,
            comparison_code: self.comparison,
            expires_at: self.expires_at,
        }
    }
}

#[derive(Default)]
pub(crate) struct Enrollment {
    window: Option<Instant>,
    pub intents: BTreeMap<EnrollmentId, Intent>,
}

impl Enrollment {
    pub fn open(&mut self) {
        self.intents.clear();
        self.window = Some(Instant::now() + WINDOW);
    }

    pub fn close(&mut self) {
        self.window = None;
        self.intents.clear();
    }

    pub fn purge(&mut self) {
        let now = Instant::now();
        if self.window.is_none_or(|deadline| now >= deadline) {
            self.close();
            return;
        }
        self.intents.retain(|_, intent| now < intent.deadline);
    }

    pub fn remove_connection(&mut self, connection: u64) {
        self.intents
            .retain(|_, intent| intent.connection != connection);
    }

    pub fn begin(
        &mut self,
        endpoint: EndpointId,
        leaf: LeafHash,
        nonce: Nonce,
        params: &EnrollParams,
        connection: u64,
        now_ms: i64,
    ) -> Result<EnrollmentAccepted, &'static str> {
        self.purge();
        let Some(deadline) = self.window else {
            return Err("enrollment is unavailable");
        };
        if self.intents.len() >= 8
            || self
                .intents
                .values()
                .any(|i| i.public_key == params.public_key)
            || !crew_protocol::native::valid_label(&params.label)
        {
            return Err("enrollment is unavailable");
        }
        let bytes = enroll_transcript(endpoint, leaf, nonce, params);
        if !verify(params.public_key, &params.signature.0, &bytes) {
            return Err("enrollment is unavailable");
        }
        let hash = Sha256::digest(bytes);
        let comparison = HexBytes(hash[..8].try_into().expect("fixed digest width"));
        if self.intents.values().any(|i| i.comparison == comparison) {
            return Err("enrollment is unavailable");
        }
        let id = super::fresh().map_err(|_| "enrollment is unavailable")?;
        let remaining_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let intent = Intent {
            id,
            public_key: params.public_key,
            label: params.label.trim().to_owned(),
            comparison,
            expires_at: now_ms.saturating_add(remaining_ms),
            deadline,
            connection,
            failures: 0,
        };
        let accepted = intent.accepted();
        self.intents.insert(id, intent);
        Ok(accepted)
    }

    /// Called only on the local authenticated control socket. The network
    /// cannot approve or change an intent's selected grants.
    pub fn check(
        &mut self,
        id: EnrollmentId,
        comparison: ComparisonCode,
        expected_key_hash: HexBytes<32>,
    ) -> Result<Intent, &'static str> {
        self.purge();
        let Some(intent) = self.intents.get_mut(&id) else {
            return Err("enrollment intent is unavailable");
        };
        if !crew_core::util::ct_eq(&intent.comparison.to_hex(), &comparison.to_hex())
            || !crew_core::util::ct_eq(
                &key_hash(intent.public_key).to_hex(),
                &expected_key_hash.to_hex(),
            )
        {
            intent.failures += 1;
            if intent.failures >= 5 {
                self.intents.remove(&id);
            }
            return Err("enrollment comparison failed");
        }
        Ok(intent.clone())
    }

    pub fn consume(&mut self, id: EnrollmentId) -> Option<Intent> {
        let intent = self.intents.remove(&id)?;
        self.close();
        Some(intent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_protocol::native::{ChannelPurpose, DAEMON_PROTOCOL, NATIVE_PROTOCOL};
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn comparison_code_matches_shared_known_answer() {
        let (endpoint, leaf, nonce, params, expected) = crew_protocol::native::comparison_vector();
        let digest = Sha256::digest(enroll_transcript(endpoint, leaf, nonce, &params));
        assert_eq!(&digest[..8], &expected.0);
    }

    fn params(key: &SigningKey) -> AuthParams {
        AuthParams {
            native_protocol: NATIVE_PROTOCOL,
            protocol: DAEMON_PROTOCOL,
            public_key: HexBytes(key.verifying_key().to_bytes()),
            signature: HexBytes([0; 64]),
            purpose: ChannelPurpose::Workspace,
            workspace: Some(HexBytes([7; 8])),
        }
    }

    #[test]
    fn proof_cannot_move_to_a_different_connection_endpoint_or_scope() {
        let key = SigningKey::from_bytes(&[9; 32]);
        let (endpoint, leaf, nonce) = (HexBytes([1; 16]), HexBytes([2; 32]), HexBytes([3; 32]));
        let mut p = params(&key);
        p.signature = HexBytes(
            key.sign(&auth_transcript(endpoint, leaf, nonce, &p).unwrap())
                .to_bytes(),
        );
        assert!(verify_auth(endpoint, leaf, nonce, &p));
        assert!(!verify_auth(endpoint, leaf, HexBytes([4; 32]), &p));
        assert!(!verify_auth(HexBytes([4; 16]), leaf, nonce, &p));
        assert!(!verify_auth(endpoint, HexBytes([4; 32]), nonce, &p));
        p.purpose = ChannelPurpose::Terminal;
        assert!(!verify_auth(endpoint, leaf, nonce, &p));
    }

    #[test]
    fn enrollment_is_host_windowed_single_use_and_connection_owned() {
        let key = SigningKey::from_bytes(&[8; 32]);
        let (endpoint, leaf, nonce) = (HexBytes([1; 16]), HexBytes([2; 32]), HexBytes([3; 32]));
        let mut p = EnrollParams {
            native_protocol: 1,
            protocol: 6,
            public_key: HexBytes(key.verifying_key().to_bytes()),
            client_nonce: HexBytes([5; 32]),
            signature: HexBytes([0; 64]),
            label: "Phone".into(),
        };
        p.signature = HexBytes(
            key.sign(&enroll_transcript(endpoint, leaf, nonce, &p))
                .to_bytes(),
        );
        let mut book = Enrollment::default();
        assert!(book.begin(endpoint, leaf, nonce, &p, 1, 1).is_err());
        book.open();
        let accepted = book.begin(endpoint, leaf, nonce, &p, 1, 1).unwrap();
        assert!(book.begin(endpoint, leaf, nonce, &p, 2, 1).is_err());
        let intent = book
            .check(
                accepted.enrollment_id,
                accepted.comparison_code,
                key_hash(p.public_key),
            )
            .unwrap();
        assert_eq!(intent.connection, 1);
        assert!(book.consume(intent.id).is_some());
        assert!(book
            .check(intent.id, accepted.comparison_code, key_hash(p.public_key))
            .is_err());
        book.open();
        let accepted = book.begin(endpoint, leaf, nonce, &p, 2, 1).unwrap();
        book.remove_connection(2);
        assert!(book
            .check(
                accepted.enrollment_id,
                accepted.comparison_code,
                key_hash(p.public_key)
            )
            .is_err());
    }

    #[test]
    fn five_wrong_comparisons_remove_the_pending_intent() {
        let key = SigningKey::from_bytes(&[8; 32]);
        let (endpoint, leaf, nonce) = (HexBytes([1; 16]), HexBytes([2; 32]), HexBytes([3; 32]));
        let mut p = EnrollParams {
            native_protocol: 1,
            protocol: 6,
            public_key: HexBytes(key.verifying_key().to_bytes()),
            client_nonce: HexBytes([5; 32]),
            signature: HexBytes([0; 64]),
            label: "Phone".into(),
        };
        p.signature = HexBytes(
            key.sign(&enroll_transcript(endpoint, leaf, nonce, &p))
                .to_bytes(),
        );
        let mut book = Enrollment::default();
        book.open();
        let accepted = book.begin(endpoint, leaf, nonce, &p, 1, 1).unwrap();
        for _ in 0..5 {
            assert!(book
                .check(
                    accepted.enrollment_id,
                    HexBytes([0; 8]),
                    key_hash(p.public_key)
                )
                .is_err());
        }
        assert!(book.intents.is_empty());
    }
}
