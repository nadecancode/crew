//! Synthetic pinned loopback peers exercise connector failures before Core.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crew_client::api::ClientScope;
use crew_protocol::native::*;
use crew_protocol::rpc::{DaemonRef, HelloInfo, Role};
use ed25519_dalek::Signer as _;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;

use super::authority::Fence;
use super::connector::GatewayConnector;
use super::*;
use crate::queue::EventQueue;
use crate::service::Connector;
use crate::{ChannelPurpose as DriverPurpose, HostEffect, HostEvent, TransportEnd};

const CERT: &[u8] = include_bytes!("../../tests/fixtures/gateway/cert.pem");
const KEY: &[u8] = include_bytes!("../../tests/fixtures/gateway/key.pem");
const EXPIRED: &[u8] = include_bytes!("../../tests/fixtures/gateway/expired.pem");
const WRONG_NAME: &[u8] = include_bytes!("../../tests/fixtures/gateway/wrong-name.pem");
const WAIT: Duration = Duration::from_secs(5);

struct Key {
    key: ed25519_dalek::SigningKey,
    proofs: AtomicUsize,
}
impl Key {
    fn new() -> Self {
        Self {
            key: ed25519_dalek::SigningKey::from_bytes(&[42; 32]),
            proofs: AtomicUsize::new(0),
        }
    }
}
impl DeviceSigner for Key {
    fn public_key(&self) -> SignerFuture<'_, PublicKey> {
        Box::pin(async move { Ok(HexBytes(self.key.verifying_key().to_bytes())) })
    }
    fn sign_auth(&self, bytes: AuthTranscript) -> SignerFuture<'_, Signature> {
        Box::pin(async move {
            self.proofs.fetch_add(1, Ordering::AcqRel);
            Ok(HexBytes(self.key.sign(bytes.signing_bytes()).to_bytes()))
        })
    }
    fn sign_enrollment(&self, bytes: EnrollTranscript) -> SignerFuture<'_, Signature> {
        Box::pin(async move {
            self.proofs.fetch_add(1, Ordering::AcqRel);
            Ok(HexBytes(self.key.sign(bytes.signing_bytes()).to_bytes()))
        })
    }
    fn retire(&self) -> SignerFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn response() -> GatewayOpened<HelloInfo> {
    GatewayOpened {
        native_protocol: 1,
        protocol: 6,
        authority: GatewayAuthority {
            endpoint_id: HexBytes([1; 16]),
            endpoint_epoch: HexBytes([2; 16]),
            device_id: HexBytes([3; 16]),
            device_generation: 1,
            credential_expires_at: 90_000,
            server_time: 10_000,
        },
        purpose: ChannelPurpose::Workspace,
        grant: Some(WorkspaceGrant {
            workspace_id: HexBytes([4; 8]),
            grant_generation: 1,
            capabilities: vec![Capability::Read],
            allowed_actions: vec!["snapshot".into(), "subscribe".into()],
        }),
        route: Some(GatewayRoute {
            workspace_id: HexBytes([4; 8]),
            workspace_generation: 2,
            daemon: DaemonRef {
                pid: 77,
                started_at: 12_000,
            },
        }),
        hello: Some(HelloInfo {
            role: Role::Operator,
            agent_id: None,
            project: None,
            version: "test".into(),
            protocol: 6,
            pid: 77,
            web_proof: None,
        }),
    }
}

async fn peer(
    cert: &[u8],
    version: &'static rustls::SupportedProtocolVersion,
    records: Vec<String>,
) -> (GatewayProfile, tokio::task::JoinHandle<bool>) {
    let certificates = rustls_pemfile::certs(&mut std::io::BufReader::new(cert))
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    let leaf = HexBytes(Sha256::digest(certificates[0].as_ref()).into());
    let private_key = rustls_pemfile::private_key(&mut std::io::BufReader::new(KEY))
        .unwrap()
        .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[version])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certificates, private_key)
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let profile = PublicProfile {
        endpoint_id: HexBytes([1; 16]),
        name: "Test".into(),
        url: format!("wss://127.0.0.1:{}/native/ws", address.port()),
        tls_leaf_sha256: leaf,
        trust_pem: std::str::from_utf8(cert).unwrap().into(),
    };
    let trusted = GatewayProfile::trust("test".into(), profile, leaf).unwrap();
    let task = tokio::spawn(async move {
        let accepted = tokio::time::timeout(WAIT, listener.accept()).await;
        let Ok(Ok((stream, _))) = accepted else {
            return false;
        };
        let tls = tokio_rustls::TlsAcceptor::from(Arc::new(config))
            .accept(stream)
            .await;
        let Ok(tls) = tls else {
            return false;
        };
        let Ok(mut socket) = tokio_tungstenite::accept_async(tls).await else {
            return false;
        };
        socket
            .send(Message::text(
                serde_json::to_string(&NoticeFrame {
                    native: Notice::Challenge {
                        nonce: HexBytes([8; 32]),
                    },
                })
                .unwrap(),
            ))
            .await
            .unwrap();
        let Some(Ok(Message::Text(text))) = socket.next().await else {
            return false;
        };
        let proof: HandshakeRequest = serde_json::from_str(&text).unwrap();
        let HandshakeCall::NativeAuth(params) = proof.call else {
            panic!("typed auth required");
        };
        let bytes = auth_transcript(HexBytes([1; 16]), leaf, HexBytes([8; 32]), &params).unwrap();
        super::wire::verify_signature(params.public_key, &bytes, params.signature).unwrap();
        for record in records {
            if socket.send(Message::text(record)).await.is_err() {
                break;
            }
        }
        let _ = tokio::time::timeout(WAIT, socket.next()).await;
        true
    });
    (trusted, task)
}

fn open(
    profile: GatewayProfile,
    signer: Arc<Key>,
    expected: Option<ClientScope>,
) -> (EventQueue, Box<dyn crate::service::ChannelHandle>) {
    let queue = EventQueue::new(2).unwrap();
    let connector = GatewayConnector {
        public_key: HexBytes(signer.key.verifying_key().to_bytes()),
        signer,
        profile: profile.clone(),
        fence: Fence::default(),
    };
    let channel = connector
        .open(
            &HostEffect::Open {
                channel_id: 1,
                profile_id: "test".into(),
                endpoint_id: profile.endpoint_id(),
                attempt: 8,
                purpose: DriverPurpose::Workspace,
                expected_scope: expected,
            },
            &queue,
        )
        .unwrap();
    (queue, channel)
}

fn expected() -> ClientScope {
    ClientScope {
        endpoint_id: HexBytes::<16>([1; 16]).to_hex(),
        workspace_id: HexBytes::<8>([4; 8]).to_hex(),
        generation: Some(2),
        daemon_pid: Some(77),
        daemon_started_at_ms: Some(12_000),
        attempt: 8,
    }
}

async fn end(queue: &EventQueue) -> TransportEnd {
    match tokio::time::timeout(WAIT, queue.recv())
        .await
        .unwrap()
        .unwrap()
    {
        HostEvent::Ended { reason, .. } => reason,
        event => panic!("unexpected event before refusal: {event:?}"),
    }
}

#[tokio::test]
async fn tls_wrong_pin_wrong_name_expiry_and_feature_unioned_tls12_refuse_before_signing() {
    for (cert, version, wrong_pin) in [
        (CERT, &rustls::version::TLS13, true),
        (WRONG_NAME, &rustls::version::TLS13, false),
        (EXPIRED, &rustls::version::TLS13, false),
        (CERT, &rustls::version::TLS12, false),
    ] {
        let (mut profile, peer) = peer(cert, version, vec![]).await;
        if wrong_pin {
            profile.public.tls_leaf_sha256 = HexBytes([9; 32]);
        }
        let key = Arc::new(Key::new());
        let (queue, mut channel) = open(profile, key.clone(), Some(expected()));
        assert_eq!(end(&queue).await, TransportEnd::TlsTrust);
        assert_eq!(key.proofs.load(Ordering::Acquire), 0);
        channel.wait_closed().await;
        assert!(!tokio::time::timeout(WAIT, peer).await.unwrap().unwrap());
    }
}

#[tokio::test]
async fn route_less_or_divergent_authenticated_open_never_becomes_ready() {
    for mode in 0..4 {
        let mut opened = response();
        let mut captured = expected();
        let reason = match mode {
            0 => {
                opened.route = None;
                TransportEnd::Malformed
            }
            1 => {
                captured.daemon_pid = Some(999);
                TransportEnd::GrantChanged
            }
            2 => {
                captured.generation = Some(99);
                TransportEnd::GrantChanged
            }
            _ => {
                opened.hello.as_mut().unwrap().pid = 999;
                TransportEnd::Malformed
            }
        };
        let records = vec![serde_json::json!({"id":0,"result":opened}).to_string()];
        let (profile, peer) = peer(CERT, &rustls::version::TLS13, records).await;
        let (queue, mut channel) = open(profile, Arc::new(Key::new()), Some(captured));
        assert_eq!(end(&queue).await, reason);
        channel.wait_closed().await;
        assert!(tokio::time::timeout(WAIT, peer).await.unwrap().unwrap());
    }
}

#[tokio::test]
async fn native_notices_map_to_scoped_ends_and_unknown_native_notices_fail_closed() {
    for (notice, reason) in [
        (
            serde_json::to_value(Notice::GrantChanged).unwrap(),
            TransportEnd::GrantChanged,
        ),
        (
            serde_json::to_value(Notice::DeviceRevoked).unwrap(),
            TransportEnd::Revoked,
        ),
        (
            serde_json::to_value(Notice::GatewayStopping).unwrap(),
            TransportEnd::Stopped,
        ),
        (
            serde_json::json!({"kind":"future_native_notice"}),
            TransportEnd::Malformed,
        ),
    ] {
        let records = vec![
            serde_json::json!({"id":0,"result":response()}).to_string(),
            serde_json::json!({"native":notice}).to_string(),
        ];
        let (profile, peer) = peer(CERT, &rustls::version::TLS13, records).await;
        let (queue, mut channel) = open(profile, Arc::new(Key::new()), Some(expected()));
        assert!(matches!(
            tokio::time::timeout(WAIT, queue.recv())
                .await
                .unwrap()
                .unwrap(),
            HostEvent::WorkspaceOpened { .. }
        ));
        assert_eq!(end(&queue).await, reason);
        channel.wait_closed().await;
        assert!(tokio::time::timeout(WAIT, peer).await.unwrap().unwrap());
    }
}

#[tokio::test]
async fn websocket_frame_bound_is_applied_before_json_decoding() {
    let records = vec![
        serde_json::json!({"id":0,"result":response()}).to_string(),
        "x".repeat(crate::framing::MAX_FRAME_BYTES + 1),
    ];
    let (profile, peer) = peer(CERT, &rustls::version::TLS13, records).await;
    let (queue, mut channel) = open(profile, Arc::new(Key::new()), Some(expected()));
    assert!(matches!(
        tokio::time::timeout(WAIT, queue.recv())
            .await
            .unwrap()
            .unwrap(),
        HostEvent::WorkspaceOpened { .. }
    ));
    assert_eq!(end(&queue).await, TransportEnd::Oversized);
    channel.wait_closed().await;
    assert!(tokio::time::timeout(WAIT, peer).await.unwrap().unwrap());
}
