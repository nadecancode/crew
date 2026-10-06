//! Portable native gateway wire version 1. Daemon protocol 6 is unchanged.
//!
//! These records contain public identity and proof material, never a signing
//! key or platform credential handle. Crypto, IO and authority checks belong
//! to the connector and server. Proofs use fixed bytes rather than JSON order.

use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

pub const NATIVE_PROTOCOL: u32 = 1;
pub const DAEMON_PROTOCOL: u32 = 6;
pub const AUTH_TRANSCRIPT_MAX: usize = 150;
pub const ENROLL_TRANSCRIPT_BYTES: usize = 174;
pub const MAX_LABEL_BYTES: usize = 256;
pub const MAX_LABEL_CHARS: usize = 64;
pub const CLOSE_DEVICE_ACCESS: u16 = 4101;
pub const CLOSE_WORKSPACE_ACCESS: u16 = 4102;
pub const CLOSE_GATEWAY_STOP: u16 = 4103;
pub const CLOSE_INCOMPATIBLE: u16 = 4104;

/// A fixed-width value encoded as canonical lowercase hex on the wire.
/// Debug intentionally omits bytes: the same type also carries nonces/proofs.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HexBytes<const N: usize>(pub [u8; N]);

impl<const N: usize> HexBytes<N> {
    pub fn from_hex(text: &str) -> Result<Self, &'static str> {
        if text.len() != N * 2 {
            return Err("invalid fixed-width hex value");
        }
        let mut bytes = [0; N];
        for (out, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            let digit = |b| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                _ => None,
            };
            *out = (digit(pair[0]).ok_or("invalid fixed-width hex value")? << 4)
                | digit(pair[1]).ok_or("invalid fixed-width hex value")?;
        }
        Ok(Self(bytes))
    }

    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(N * 2);
        for byte in self.0 {
            text.push(HEX[(byte >> 4) as usize] as char);
            text.push(HEX[(byte & 15) as usize] as char);
        }
        text
    }
}

impl<const N: usize> fmt::Debug for HexBytes<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HexBytes<{N}>(<redacted>)")
    }
}

impl<const N: usize> Serialize for HexBytes<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de, const N: usize> Deserialize<'de> for HexBytes<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HexVisitor<const N: usize>;
        impl<const N: usize> de::Visitor<'_> for HexVisitor<N> {
            type Value = HexBytes<N>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{} lowercase hex characters", N * 2)
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                HexBytes::from_hex(value).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(HexVisitor::<N>)
    }
}

pub type EndpointId = HexBytes<16>;
pub type DeviceId = HexBytes<16>;
pub type EnrollmentId = HexBytes<16>;
pub type WorkspaceId = HexBytes<8>;
pub type PublicKey = HexBytes<32>;
pub type Nonce = HexBytes<32>;
pub type LeafHash = HexBytes<32>;
pub type Signature = HexBytes<64>;
pub type ComparisonCode = HexBytes<8>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelPurpose {
    Index,
    Workspace,
    Read,
    Action,
    Terminal,
}

impl ChannelPurpose {
    pub fn code(self) -> u8 {
        match self {
            Self::Index => 0,
            Self::Workspace => 1,
            Self::Read => 2,
            Self::Action => 3,
            Self::Terminal => 4,
        }
    }

    pub fn accepts_workspace(self, workspace: Option<WorkspaceId>) -> bool {
        matches!((self, workspace), (Self::Index, None))
            || (self != Self::Index && workspace.is_some())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Read,
    Control,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthParams {
    pub native_protocol: u32,
    pub protocol: u32,
    pub public_key: PublicKey,
    pub signature: Signature,
    pub purpose: ChannelPurpose,
    #[serde(default)]
    pub workspace: Option<WorkspaceId>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollParams {
    pub native_protocol: u32,
    pub protocol: u32,
    pub public_key: PublicKey,
    pub client_nonce: Nonce,
    pub signature: Signature,
    pub label: String,
}

impl fmt::Debug for EnrollParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrollParams")
            .field("proof", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum HandshakeCall {
    NativeAuth(AuthParams),
    NativeEnroll(EnrollParams),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HandshakeRequest {
    pub id: u64,
    #[serde(flatten)]
    pub call: HandshakeCall,
}

impl<'de> Deserialize<'de> for HandshakeRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            id: u64,
            method: String,
            params: serde_json::Value,
        }
        let envelope = Envelope::deserialize(deserializer)?;
        let call = match envelope.method.as_str() {
            "native_auth" => HandshakeCall::NativeAuth(
                serde_json::from_value(envelope.params).map_err(de::Error::custom)?,
            ),
            "native_enroll" => HandshakeCall::NativeEnroll(
                serde_json::from_value(envelope.params).map_err(de::Error::custom)?,
            ),
            _ => return Err(de::Error::custom("unknown native handshake method")),
        };
        Ok(Self {
            id: envelope.id,
            call,
        })
    }
}

pub fn valid_label(label: &str) -> bool {
    !label.trim().is_empty()
        && label.len() <= MAX_LABEL_BYTES
        && label.chars().count() <= MAX_LABEL_CHARS
        && !label.chars().any(char::is_control)
}

/// Shared synthetic known-answer input. SHA-256 stays outside this crate;
/// both the server and connector must assert the expected comparison code.
pub fn comparison_vector() -> (EndpointId, LeafHash, Nonce, EnrollParams, ComparisonCode) {
    (
        HexBytes([1; 16]),
        HexBytes([2; 32]),
        HexBytes([3; 32]),
        EnrollParams {
            native_protocol: 1,
            protocol: 6,
            public_key: HexBytes([4; 32]),
            client_nonce: HexBytes([5; 32]),
            signature: HexBytes([0; 64]),
            label: "comparison-vector".to_owned(),
        },
        HexBytes([0xa4, 0xd3, 0xb0, 0x46, 0xe1, 0xee, 0x4c, 0xf0]),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayAuthority {
    pub endpoint_id: EndpointId,
    pub endpoint_epoch: HexBytes<16>,
    pub device_id: DeviceId,
    pub device_generation: u64,
    pub credential_expires_at: i64,
    pub server_time: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceGrant {
    pub workspace_id: WorkspaceId,
    pub grant_generation: u64,
    pub capabilities: Vec<Capability>,
    pub allowed_actions: Vec<String>,
}

/// A gateway-verified route on this channel, rather than an index hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayRoute {
    pub workspace_id: WorkspaceId,
    pub workspace_generation: u64,
    pub daemon: crate::rpc::DaemonRef,
}

/// H is the existing v6 HelloInfo; generic to avoid depending on host-bound RPC.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayOpened<H> {
    pub native_protocol: u32,
    pub protocol: u32,
    pub authority: GatewayAuthority,
    pub purpose: ChannelPurpose,
    pub grant: Option<WorkspaceGrant>,
    /// Absent only for Index. Workspace channels require verified route facts.
    pub route: Option<GatewayRoute>,
    pub hello: Option<H>,
}

/// L is the existing filtered v6 Workspaces listing, without another schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayListing<L> {
    pub authority: GatewayAuthority,
    pub grants: Vec<WorkspaceGrant>,
    pub listing: L,
}

/// Unsolicited index update. Grant metadata is outside the unchanged v6 push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListingNotice<L> {
    pub native_listing: GatewayListing<L>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentAccepted {
    pub enrollment_id: EnrollmentId,
    pub comparison_code: ComparisonCode,
    pub expires_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Notice {
    Challenge { nonce: Nonce },
    EnrollmentAccepted { intent: EnrollmentAccepted },
    GrantChanged,
    DeviceRevoked,
    GatewayStopping,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeFrame {
    pub native: Notice,
}

/// This is the only public profile payload. It has no device/operator token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicProfile {
    pub endpoint_id: EndpointId,
    pub name: String,
    pub url: String,
    pub tls_leaf_sha256: LeafHash,
    pub trust_pem: String,
}

pub fn auth_transcript(
    endpoint: EndpointId,
    leaf: LeafHash,
    nonce: Nonce,
    params: &AuthParams,
) -> Result<Vec<u8>, &'static str> {
    if !params.purpose.accepts_workspace(params.workspace) {
        return Err("invalid channel scope");
    }
    let mut bytes = Vec::with_capacity(AUTH_TRANSCRIPT_MAX);
    bytes.extend_from_slice(b"crew.native.auth.v1\0");
    bytes.extend_from_slice(&endpoint.0);
    bytes.extend_from_slice(&leaf.0);
    bytes.extend_from_slice(&nonce.0);
    bytes.extend_from_slice(&params.public_key.0);
    bytes.extend_from_slice(&params.native_protocol.to_be_bytes());
    bytes.extend_from_slice(&params.protocol.to_be_bytes());
    bytes.push(params.purpose.code());
    bytes.push(u8::from(params.workspace.is_some()));
    if let Some(workspace) = params.workspace {
        bytes.extend_from_slice(&workspace.0);
    }
    Ok(bytes)
}

pub fn enroll_transcript(
    endpoint: EndpointId,
    leaf: LeafHash,
    nonce: Nonce,
    params: &EnrollParams,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(ENROLL_TRANSCRIPT_BYTES);
    bytes.extend_from_slice(b"crew.native.enroll.v1\0");
    bytes.extend_from_slice(&endpoint.0);
    bytes.extend_from_slice(&leaf.0);
    bytes.extend_from_slice(&nonce.0);
    bytes.extend_from_slice(&params.public_key.0);
    bytes.extend_from_slice(&params.client_nonce.0);
    bytes.extend_from_slice(&params.native_protocol.to_be_bytes());
    bytes.extend_from_slice(&params.protocol.to_be_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_route_records_round_trip_and_reject_unknown_fields() {
        let route = GatewayRoute {
            workspace_id: HexBytes([6; 8]),
            workspace_generation: 1,
            daemon: crate::rpc::DaemonRef {
                pid: 42,
                started_at: 1_700_000_000_123,
            },
        };
        let encoded = serde_json::to_value(&route).unwrap();
        assert_eq!(encoded["daemon"]["started_at"], 1_700_000_000_123i64);
        assert_eq!(
            serde_json::from_value::<GatewayRoute>(encoded.clone()).unwrap(),
            route
        );
        let mut invalid = encoded;
        invalid["grant_generation"] = serde_json::json!(9);
        assert!(serde_json::from_value::<GatewayRoute>(invalid).is_err());
    }

    fn auth() -> AuthParams {
        AuthParams {
            native_protocol: 1,
            protocol: 6,
            public_key: HexBytes([4; 32]),
            signature: HexBytes([0; 64]),
            purpose: ChannelPurpose::Workspace,
            workspace: Some(HexBytes([5; 8])),
        }
    }

    #[test]
    fn proof_bytes_have_fixed_layout_and_scope_changes_are_distinct() {
        let params = auth();
        let bytes = auth_transcript(
            HexBytes([1; 16]),
            HexBytes([2; 32]),
            HexBytes([3; 32]),
            &params,
        )
        .unwrap();
        let expected = [
            b"crew.native.auth.v1\0".as_slice(),
            &[1; 16],
            &[2; 32],
            &[3; 32],
            &[4; 32],
            &[0, 0, 0, 1, 0, 0, 0, 6, 1, 1],
            &[5; 8],
        ]
        .concat();
        assert_eq!(bytes, expected);
        assert_eq!(bytes.len(), AUTH_TRANSCRIPT_MAX);
        let mut changed = params.clone();
        changed.purpose = ChannelPurpose::Terminal;
        assert_ne!(
            bytes,
            auth_transcript(
                HexBytes([1; 16]),
                HexBytes([2; 32]),
                HexBytes([3; 32]),
                &changed
            )
            .unwrap()
        );
        changed.purpose = ChannelPurpose::Index;
        assert!(auth_transcript(
            HexBytes([1; 16]),
            HexBytes([2; 32]),
            HexBytes([3; 32]),
            &changed
        )
        .is_err());
        changed.workspace = None;
        assert_eq!(
            auth_transcript(
                HexBytes([1; 16]),
                HexBytes([2; 32]),
                HexBytes([3; 32]),
                &changed
            )
            .unwrap()
            .len(),
            AUTH_TRANSCRIPT_MAX - 8
        );
    }

    #[test]
    fn enrollment_is_domain_separated_and_signature_and_label_are_not_identity_bytes() {
        let mut params = EnrollParams {
            native_protocol: 1,
            protocol: 6,
            public_key: HexBytes([4; 32]),
            client_nonce: HexBytes([5; 32]),
            signature: HexBytes([0; 64]),
            label: "Phone".into(),
        };
        let bytes = enroll_transcript(
            HexBytes([1; 16]),
            HexBytes([2; 32]),
            HexBytes([3; 32]),
            &params,
        );
        assert_eq!(bytes.len(), ENROLL_TRANSCRIPT_BYTES);
        assert!(bytes.starts_with(b"crew.native.enroll.v1\0"));
        assert_eq!(&bytes[bytes.len() - 8..], &[0, 0, 0, 1, 0, 0, 0, 6]);
        params.label = "different label".into();
        params.signature = HexBytes([9; 64]);
        assert_eq!(
            bytes,
            enroll_transcript(
                HexBytes([1; 16]),
                HexBytes([2; 32]),
                HexBytes([3; 32]),
                &params
            )
        );
        params.client_nonce = HexBytes([6; 32]);
        assert_ne!(
            bytes,
            enroll_transcript(
                HexBytes([1; 16]),
                HexBytes([2; 32]),
                HexBytes([3; 32]),
                &params
            )
        );
    }

    #[test]
    fn strict_wire_rejects_noncanonical_values_and_identity_overrides() {
        for bad in ["AA", "gg", "0", "0000"] {
            assert!(HexBytes::<1>::from_hex(bad).is_err());
        }
        let mut request = serde_json::to_value(auth()).unwrap();
        request["role"] = "operator".into();
        assert!(serde_json::from_value::<AuthParams>(request).is_err());
        let valid = HandshakeRequest {
            id: 1,
            call: HandshakeCall::NativeAuth(auth()),
        };
        let value = serde_json::to_value(valid).unwrap();
        assert!(serde_json::from_value::<HandshakeRequest>(value.clone()).is_ok());
        for (field, injected) in [
            ("role", serde_json::json!("operator")),
            ("token", serde_json::json!("x")),
            ("web", serde_json::json!(true)),
        ] {
            let mut probe = value.clone();
            probe[field] = injected;
            assert!(serde_json::from_value::<HandshakeRequest>(probe).is_err());
        }
        let challenge = NoticeFrame {
            native: Notice::Challenge {
                nonce: HexBytes([7; 32]),
            },
        };
        let encoded = serde_json::to_value(challenge).unwrap();
        assert_eq!(encoded["native"]["kind"], "challenge");
        assert_eq!(encoded["native"]["nonce"], "07".repeat(32));
        assert!(!format!("{:?}", auth()).contains(&"04".repeat(32)));
        let mut extra = encoded;
        extra["native"]["operator_token"] = "synthetic-secret".into();
        assert!(serde_json::from_value::<NoticeFrame>(extra).is_err());
    }
}
