#![cfg(feature = "bindings")]

//! Gateway ports through the exported binding objects. A Rust fake stands in
//! for the platform signer and an isolated loopback TLS peer for the pinned
//! host. Neither is a Crew daemon, gateway or OS credential store; acceptance
//! against the integrated gateway belongs to the gateway validation card.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crew_client_ffi::gateway::{
    normalize_fingerprint, parse_gateway_profile, AuthTranscript, DeviceSigner, EnrollTranscript,
    EnrollmentOutcome, EnrollmentProgress, EnrollmentResult, EnrollmentUpdate, GatewayError,
    GatewayProfile, PublicKeyReply, PublicProfile, ReplyError, RetirementReply, SetupContext,
    SetupObserver, SignatureReply, SignerError,
};
use crew_client_ffi::records::{ClientError, ClientReason};
use crew_client_ffi::service::CrewClient;
use crew_client_transport::gateway as transport;
use crew_client_transport::TransportEnd;
use crew_protocol::native::{self, HexBytes};
use ed25519_dalek::Signer as _;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;

const CERT: &str = include_str!("../../crew-client-transport/tests/fixtures/gateway/cert.pem");
const KEY: &str = include_str!("../../crew-client-transport/tests/fixtures/gateway/key.pem");
const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(300);
const ENDPOINT: [u8; 16] = [1; 16];
const NONCE: [u8; 32] = [8; 32];
const PROFILE_ID: &str = "test-profile";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn leaf() -> HexBytes<32> {
    let certs = rustls_pemfile::certs(&mut CERT.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    HexBytes(Sha256::digest(certs[0].as_ref()).into())
}

fn public(port: u16) -> PublicProfile {
    PublicProfile {
        endpoint_id: HexBytes(ENDPOINT).to_hex(),
        name: "Test host".into(),
        url: format!("wss://127.0.0.1:{port}/native/ws"),
        tls_leaf_sha256: leaf().to_hex(),
        trust_pem: CERT.into(),
    }
}

fn trusted(port: u16) -> Arc<GatewayProfile> {
    GatewayProfile::trust(PROFILE_ID.into(), public(port), leaf().to_hex()).unwrap()
}

fn failure(error: transport::GatewayError) -> GatewayError {
    let ClientReason { code, message, details: _ } = error.reason();
    GatewayError::Failure { code, message }
}

fn code(error: &GatewayError) -> &str {
    let GatewayError::Failure { code, .. } = error;
    code
}

fn refused<T>(result: Result<T, GatewayError>) -> GatewayError {
    match result {
        Ok(_) => panic!("the gateway call must fail"),
        Err(error) => error,
    }
}

/// A listener that is never accepted. A dial would still queue a connection,
/// so an empty accept proves no TCP connection was attempted.
struct Unused(TcpListener);

impl Unused {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        Self(listener)
    }

    fn port(&self) -> u16 {
        self.0.local_addr().unwrap().port()
    }

    fn dialed(&self) -> bool {
        self.0.accept().is_ok()
    }
}

enum Unlock {
    Key(Vec<u8>),
    Fail(SignerError),
    Hold,
}

#[derive(Debug)]
enum Call {
    PublicKey,
    Auth {
        transcript: String,
        reply: String,
        bytes: Vec<u8>,
    },
    Enrollment {
        transcript: String,
        reply: String,
        bytes: Vec<u8>,
    },
    Retire,
}

/// The platform signer contract: answer through the reply object, never by
/// returning key material. `forged` signs with a different key.
struct FakeSigner {
    key: ed25519_dalek::SigningKey,
    forged: bool,
    unlocks: Mutex<VecDeque<Unlock>>,
    held: Sender<Arc<PublicKeyReply>>,
    calls: Sender<Call>,
}

struct Signer {
    held: Receiver<Arc<PublicKeyReply>>,
    calls: Receiver<Call>,
}

fn device_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[42; 32])
}

fn other_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[7; 32])
}

fn fake_signer(unlocks: Vec<Unlock>, forged: bool) -> (Box<FakeSigner>, Signer) {
    let (held, held_rx) = mpsc::channel();
    let (calls, calls_rx) = mpsc::channel();
    (
        Box::new(FakeSigner {
            key: device_key(),
            forged,
            unlocks: Mutex::new(unlocks.into()),
            held,
            calls,
        }),
        Signer {
            held: held_rx,
            calls: calls_rx,
        },
    )
}

impl FakeSigner {
    fn sign(&self, bytes: &[u8]) -> Vec<u8> {
        let key = if self.forged {
            other_key()
        } else {
            self.key.clone()
        };
        key.sign(bytes).to_bytes().to_vec()
    }
}

impl DeviceSigner for FakeSigner {
    fn public_key(&self, reply: Arc<PublicKeyReply>) {
        let _ = self.calls.send(Call::PublicKey);
        let next = self.unlocks.lock().unwrap().pop_front();
        match next.unwrap_or_else(|| Unlock::Key(self.key.verifying_key().to_bytes().to_vec())) {
            // A wrong width is reported to the caller and also ends the request.
            Unlock::Key(bytes) => {
                let _ = reply.complete(bytes);
            }
            Unlock::Fail(error) => reply.fail(error).unwrap(),
            Unlock::Hold => self.held.send(reply).unwrap(),
        }
    }

    fn sign_auth(&self, transcript: Arc<AuthTranscript>, reply: Arc<SignatureReply>) {
        let bytes = transcript.signing_bytes();
        let signature = self.sign(&bytes);
        let _ = self.calls.send(Call::Auth {
            transcript: format!("{transcript:?}"),
            reply: format!("{reply:?}"),
            bytes,
        });
        reply.complete(signature).unwrap();
    }

    fn sign_enrollment(&self, transcript: Arc<EnrollTranscript>, reply: Arc<SignatureReply>) {
        let bytes = transcript.signing_bytes();
        let signature = self.sign(&bytes);
        let _ = self.calls.send(Call::Enrollment {
            transcript: format!("{transcript:?}"),
            reply: format!("{reply:?}"),
            bytes,
        });
        reply.complete(signature).unwrap();
    }

    fn retire(&self, reply: Arc<RetirementReply>) {
        let _ = self.calls.send(Call::Retire);
        reply.complete().unwrap();
    }
}

#[derive(Debug)]
enum Setup {
    Changed(EnrollmentUpdate),
    Finished(EnrollmentResult),
    Failed(ClientReason),
}

struct Observer(Sender<Setup>);

impl SetupObserver for Observer {
    fn changed(&self, update: EnrollmentUpdate) {
        let _ = self.0.send(Setup::Changed(update));
    }

    fn finished(&self, result: EnrollmentResult) {
        let _ = self.0.send(Setup::Finished(result));
    }

    fn failed(&self, reason: ClientReason) {
        let _ = self.0.send(Setup::Failed(reason));
    }
}

fn setup_observer() -> (Box<Observer>, Receiver<Setup>) {
    let (sender, receiver) = mpsc::channel();
    (Box::new(Observer(sender)), receiver)
}

fn next(setups: &Receiver<Setup>) -> Setup {
    setups.recv_timeout(WAIT).expect("setup callback")
}

fn quiet(setups: &Receiver<Setup>) {
    if let Ok(extra) = setups.recv_timeout(QUIET) {
        panic!("no callback may follow the terminal one: {extra:?}");
    }
}

fn connecting(setups: &Receiver<Setup>) -> SetupContext {
    match next(setups) {
        Setup::Changed(EnrollmentUpdate {
            context,
            progress: EnrollmentProgress::Connecting,
        }) => context,
        other => panic!("Connecting must come first: {other:?}"),
    }
}

fn finished(setups: &Receiver<Setup>, context: &SetupContext) -> EnrollmentOutcome {
    match next(setups) {
        Setup::Finished(result) => {
            assert_eq!(&result.context, context, "one attempt identity");
            result.outcome
        }
        other => panic!("terminal result expected: {other:?}"),
    }
}

#[derive(Clone, Copy)]
enum Host {
    /// Read the proof, then hold the connection without replying.
    Silent,
    /// Accept the enrollment intent and optionally approve it.
    Accept { approve: bool },
}

/// One isolated pinned loopback peer. Each connection reports the proof it
/// received, or None when the client closed before sending one.
struct Peer {
    _runtime: tokio::runtime::Runtime,
    port: u16,
    proofs: Receiver<Option<native::HandshakeCall>>,
}

fn peer(host: Host) -> Peer {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let certificates = rustls_pemfile::certs(&mut CERT.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let private_key = rustls_pemfile::private_key(&mut KEY.as_bytes())
        .unwrap()
        .unwrap();
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .unwrap(),
    );
    let (sender, proofs) = mpsc::channel();
    runtime.spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, config.clone(), host, sender.clone()));
        }
    });
    Peer {
        _runtime: runtime,
        port,
        proofs,
    }
}

async fn serve(
    stream: tokio::net::TcpStream,
    config: Arc<rustls::ServerConfig>,
    host: Host,
    proofs: Sender<Option<native::HandshakeCall>>,
) {
    let Ok(tls) = tokio_rustls::TlsAcceptor::from(config).accept(stream).await else {
        let _ = proofs.send(None);
        return;
    };
    let Ok(mut socket) = tokio_tungstenite::accept_async(tls).await else {
        let _ = proofs.send(None);
        return;
    };
    let challenge = native::NoticeFrame {
        native: native::Notice::Challenge {
            nonce: HexBytes(NONCE),
        },
    };
    let sent = socket
        .send(Message::text(serde_json::to_string(&challenge).unwrap()))
        .await;
    let proof = match (sent, socket.next().await) {
        (Ok(()), Some(Ok(Message::Text(text)))) => {
            serde_json::from_str::<native::HandshakeRequest>(&text).ok()
        }
        _ => None,
    };
    let Some(proof) = proof else {
        let _ = proofs.send(None);
        return;
    };
    let _ = proofs.send(Some(proof.call.clone()));
    if let (Host::Accept { approve }, native::HandshakeCall::NativeEnroll(params)) =
        (host, &proof.call)
    {
        let bytes = native::enroll_transcript(HexBytes(ENDPOINT), leaf(), HexBytes(NONCE), params);
        let intent = native::NoticeFrame {
            native: native::Notice::EnrollmentAccepted {
                intent: native::EnrollmentAccepted {
                    enrollment_id: HexBytes([5; 16]),
                    comparison_code: HexBytes(Sha256::digest(&bytes)[..8].try_into().unwrap()),
                    expires_at: now_ms() + 60_000,
                },
            },
        };
        let _ = socket
            .send(Message::text(serde_json::to_string(&intent).unwrap()))
            .await;
        if approve {
            let now = now_ms();
            let opened = native::GatewayOpened::<crew_protocol::rpc::HelloInfo> {
                native_protocol: native::NATIVE_PROTOCOL,
                protocol: native::DAEMON_PROTOCOL,
                authority: native::GatewayAuthority {
                    endpoint_id: HexBytes(ENDPOINT),
                    endpoint_epoch: HexBytes([2; 16]),
                    device_id: HexBytes([3; 16]),
                    device_generation: 1,
                    credential_expires_at: now + 3_600_000,
                    server_time: now,
                },
                purpose: native::ChannelPurpose::Index,
                grant: None,
                route: None,
                hello: None,
            };
            let reply = serde_json::json!({"id": 0, "result": opened});
            let _ = socket.send(Message::text(reply.to_string())).await;
        }
    }
    // Hold the channel until the client closes it or the test ends.
    let _ = tokio::time::timeout(WAIT, async {
        while let Some(Ok(_)) = socket.next().await {}
    })
    .await;
}

fn verify(public_key: native::PublicKey, bytes: &[u8], signature: native::Signature) -> bool {
    ed25519_dalek::VerifyingKey::from_bytes(&public_key.0)
        .and_then(|key| {
            key.verify_strict(bytes, &ed25519_dalek::Signature::from_bytes(&signature.0))
        })
        .is_ok()
}

#[test]
fn every_source_failure_flattens_to_its_own_reason_code() {
    let mut sources = vec![
        transport::GatewayError::InvalidProfile,
        transport::GatewayError::Trust,
        transport::GatewayError::Busy,
    ];
    for error in [
        SignerError::Locked,
        SignerError::Unavailable,
        SignerError::Canceled,
        SignerError::Retired,
        SignerError::InvalidProof,
    ] {
        // Adding a signer variant stops this match compiling until listed.
        match error {
            SignerError::Locked
            | SignerError::Unavailable
            | SignerError::Canceled
            | SignerError::Retired
            | SignerError::InvalidProof => sources.push(transport::GatewayError::Signer(error)),
        }
    }
    for end in [
        TransportEnd::Closed,
        TransportEnd::Authentication,
        TransportEnd::AgentContext,
        TransportEnd::MissingCredential,
        TransportEnd::CredentialLocked,
        TransportEnd::CredentialUnavailable,
        TransportEnd::Permission,
        TransportEnd::ProtocolMismatch,
        TransportEnd::TlsTrust,
        TransportEnd::Revoked,
        TransportEnd::GrantChanged,
        TransportEnd::Malformed,
        TransportEnd::Oversized,
        TransportEnd::Overflow,
        TransportEnd::Timeout,
        TransportEnd::Unreachable,
        TransportEnd::Stopped,
    ] {
        match end {
            TransportEnd::Closed
            | TransportEnd::Authentication
            | TransportEnd::AgentContext
            | TransportEnd::MissingCredential
            | TransportEnd::CredentialLocked
            | TransportEnd::CredentialUnavailable
            | TransportEnd::Permission
            | TransportEnd::ProtocolMismatch
            | TransportEnd::TlsTrust
            | TransportEnd::Revoked
            | TransportEnd::GrantChanged
            | TransportEnd::Malformed
            | TransportEnd::Oversized
            | TransportEnd::Overflow
            | TransportEnd::Timeout
            | TransportEnd::Unreachable
            | TransportEnd::Stopped => sources.push(transport::GatewayError::Transport(end)),
        }
    }
    for source in &sources {
        match source {
            transport::GatewayError::InvalidProfile
            | transport::GatewayError::Trust
            | transport::GatewayError::Signer(_)
            | transport::GatewayError::Transport(_)
            | transport::GatewayError::Busy => {}
        }
    }
    assert_eq!(sources.len(), 3 + 5 + 17);

    let mut codes: BTreeMap<String, Vec<transport::GatewayError>> = BTreeMap::new();
    for source in sources {
        let flattened = GatewayError::from(source);
        let reason = source.reason();
        let GatewayError::Failure { code, message } = &flattened;
        assert!(!code.is_empty() && !message.is_empty(), "{source:?}");
        assert_eq!((code, message), (&reason.code, &reason.message));
        assert_eq!(flattened.to_string(), format!("{code}: {message}"));
        codes.entry(code.clone()).or_default().push(source);
    }
    let shared: BTreeMap<_, _> = codes.into_iter().filter(|(_, v)| v.len() > 1).collect();
    let expected = BTreeMap::from([
        (
            "key_locked".to_string(),
            vec![
                transport::GatewayError::Signer(SignerError::Locked),
                transport::GatewayError::Transport(TransportEnd::CredentialLocked),
            ],
        ),
        (
            "key_unavailable".to_string(),
            vec![
                transport::GatewayError::Signer(SignerError::Unavailable),
                transport::GatewayError::Transport(TransportEnd::CredentialUnavailable),
            ],
        ),
        (
            "tls_trust".to_string(),
            vec![
                transport::GatewayError::Trust,
                transport::GatewayError::Transport(TransportEnd::TlsTrust),
            ],
        ),
    ]);
    assert_eq!(
        shared, expected,
        "only the reviewed alias groups share a code"
    );
}

#[test]
fn imported_profile_needs_an_independent_fingerprint_and_debug_hides_material() {
    let native_profile = native::PublicProfile {
        endpoint_id: HexBytes(ENDPOINT),
        name: "Test host".into(),
        url: "wss://127.0.0.1:9443/native/ws".into(),
        tls_leaf_sha256: leaf(),
        trust_pem: CERT.into(),
    };
    let imported = parse_gateway_profile(serde_json::to_string(&native_profile).unwrap()).unwrap();
    assert_eq!(imported, public(9443));
    // The complete `crew native status` output is accepted as pasted.
    let status = serde_json::json!({
        "profile": native_profile,
        "endpoint_epoch": HexBytes::<16>([2; 16]),
    });
    assert_eq!(
        parse_gateway_profile(serde_json::to_string_pretty(&status).unwrap()).unwrap(),
        public(9443)
    );
    let mut extended = serde_json::to_value(&native_profile).unwrap();
    extended["device_token"] = "unexpected".into();
    let mut status_extra = status.clone();
    status_extra["device_token"] = "unexpected".into();
    let mut status_epoch = status.clone();
    status_epoch["endpoint_epoch"] = 7.into();
    for text in [
        "{}".to_string(),
        "not json".to_string(),
        extended.to_string(),
        status_extra.to_string(),
        status_epoch.to_string(),
        serde_json::json!({"profile": native_profile}).to_string(),
        "x".repeat(crew_protocol::types::MAX_FRAME_BYTES + 1),
    ] {
        assert_eq!(
            code(&parse_gateway_profile(text).unwrap_err()),
            "invalid_profile"
        );
    }

    let profile = trusted(9443);
    assert_eq!(profile.profile_id(), PROFILE_ID);
    assert_eq!(profile.public_profile(), public(9443));
    // The comparison the person made out of band, not the imported field.
    let other = HexBytes::<32>([9; 32]).to_hex();
    assert_eq!(
        refused(GatewayProfile::trust(
            PROFILE_ID.into(),
            public(9443),
            other
        )),
        failure(transport::GatewayError::Trust)
    );
    // Typed or pasted forms normalize identically on every platform.
    let canonical = leaf().to_hex();
    let grouped = canonical
        .to_ascii_uppercase()
        .as_bytes()
        .chunks(4)
        .map(|group| std::str::from_utf8(group).unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    let colons = canonical
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap())
        .collect::<Vec<_>>()
        .join(":");
    for typed in [
        canonical.clone(),
        grouped.clone(),
        colons,
        format!("\n {grouped}\t"),
    ] {
        assert_eq!(
            normalize_fingerprint(typed.clone()),
            Some(canonical.clone())
        );
        GatewayProfile::trust(PROFILE_ID.into(), public(9443), typed).unwrap();
    }
    for malformed in [
        String::new(),
        canonical[..62].to_string(),
        format!("{canonical}00"),
        "zz".repeat(32),
        format!("{}-", &canonical[..63]),
        " ".repeat(1024) + &canonical,
    ] {
        assert_eq!(normalize_fingerprint(malformed), None);
    }
    for fingerprint in [
        "".to_string(),
        "zz".repeat(32),
        leaf().to_hex()[..62].to_string(),
    ] {
        assert_eq!(
            code(&refused(GatewayProfile::trust(
                PROFILE_ID.into(),
                public(9443),
                fingerprint
            ))),
            "invalid_profile"
        );
    }
    let mut plaintext = public(9443);
    plaintext.url = "ws://127.0.0.1:9443/native/ws".into();
    let mut short_endpoint = public(9443);
    short_endpoint.endpoint_id.pop();
    for invalid in [plaintext, short_endpoint] {
        assert_eq!(
            code(&refused(GatewayProfile::trust(
                PROFILE_ID.into(),
                invalid,
                leaf().to_hex()
            ))),
            "invalid_profile"
        );
    }
    for debug in [format!("{imported:?}"), format!("{profile:?}")] {
        assert!(!debug.contains("CERTIFICATE") && !debug.contains(&leaf().to_hex()));
    }
}

#[test]
fn gateway_factory_unlocks_once_and_returns_a_disconnected_client() {
    let host = Unused::new();
    let (signer, fake) = fake_signer(Vec::new(), false);
    let client = CrewClient::gateway(trusted(host.port()), signer).unwrap();
    assert!(matches!(
        fake.calls.recv_timeout(WAIT).unwrap(),
        Call::PublicKey
    ));
    assert!(
        fake.calls.recv_timeout(QUIET).is_err(),
        "no proof before IO"
    );
    assert_eq!(client.connection_context().unwrap(), None);
    assert_eq!(client.workspaces().unwrap(), None);
    assert!(matches!(
        client.connect("unknown-profile".into()),
        Err(ClientError::InvalidInput { field, .. }) if field == "profile_id"
    ));
    assert!(!host.dialed(), "construction never connects");

    client.close().unwrap();
    let (observer, setups) = setup_observer();
    assert_eq!(
        refused(client.enroll("Test phone".into(), observer)),
        failure(transport::GatewayError::Transport(TransportEnd::Stopped))
    );
    quiet(&setups);
    assert_eq!(
        client.connect(PROFILE_ID.into()),
        Err(ClientError::ClosedClient)
    );
    assert!(!host.dialed());
}

#[test]
fn signer_unlock_failures_and_invalid_keys_are_typed_before_any_dial() {
    let mut weak = [0; 32];
    weak[0] = 1; // The identity point: a small-order Ed25519 key.
    for (unlock, expected) in [
        (Unlock::Key(weak.to_vec()), "invalid_proof"),
        (Unlock::Key(vec![9; 31]), "invalid_proof"),
        (Unlock::Key(vec![9; 33]), "invalid_proof"),
        (Unlock::Fail(SignerError::Locked), "key_locked"),
        (Unlock::Fail(SignerError::Unavailable), "key_unavailable"),
        (Unlock::Fail(SignerError::Canceled), "key_canceled"),
        (Unlock::Fail(SignerError::Retired), "key_retired"),
    ] {
        let host = Unused::new();
        let (signer, _fake) = fake_signer(vec![unlock], false);
        let error = refused(CrewClient::gateway(trusted(host.port()), signer));
        assert_eq!(code(&error), expected);
        assert!(!host.dialed());
    }
}

#[test]
fn enrollment_progress_and_approval_share_one_attempt_in_fifo_order() {
    let host = peer(Host::Accept { approve: true });
    let (signer, fake) = fake_signer(Vec::new(), false);
    let client = CrewClient::gateway(trusted(host.port), signer).unwrap();
    let (observer, setups) = setup_observer();
    let handle = client.enroll("Test phone".into(), observer).unwrap();
    let context = connecting(&setups);
    assert_eq!(context.profile_id, PROFILE_ID);
    assert_eq!(context.endpoint_id, HexBytes(ENDPOINT).to_hex());
    assert!(context.attempt > 0);

    let bytes = loop {
        match fake.calls.recv_timeout(WAIT).unwrap() {
            Call::PublicKey => {}
            Call::Enrollment {
                transcript,
                reply,
                bytes,
            } => {
                assert_eq!(transcript, "EnrollTranscript(<redacted>)");
                assert_eq!(reply, "SignatureReply(<redacted>)");
                break bytes;
            }
            other => panic!("unexpected signer call {other:?}"),
        }
    };
    assert_eq!(bytes.len(), native::ENROLL_TRANSCRIPT_BYTES);
    let Some(native::HandshakeCall::NativeEnroll(params)) = host.proofs.recv_timeout(WAIT).unwrap()
    else {
        panic!("enrollment proof expected");
    };
    assert_eq!(params.label, "Test phone");
    assert_eq!(params.public_key.0, device_key().verifying_key().to_bytes());
    assert!(verify(params.public_key, &bytes, params.signature));
    assert_eq!(
        bytes,
        native::enroll_transcript(HexBytes(ENDPOINT), leaf(), HexBytes(NONCE), &params)
    );

    match next(&setups) {
        Setup::Changed(EnrollmentUpdate {
            context: awaiting,
            progress:
                EnrollmentProgress::AwaitingApproval {
                    enrollment_id,
                    comparison_code,
                    expires_at_ms,
                },
        }) => {
            assert_eq!(awaiting, context);
            assert_eq!(enrollment_id, HexBytes([5; 16]).to_hex());
            let expected = HexBytes::<8>(Sha256::digest(&bytes)[..8].try_into().unwrap());
            assert_eq!(comparison_code, expected.to_hex());
            assert!(expires_at_ms > now_ms());
        }
        other => panic!("AwaitingApproval expected: {other:?}"),
    }
    let EnrollmentOutcome::Approved { access } = finished(&setups, &context) else {
        panic!("approval expected");
    };
    assert_eq!(access.endpoint_id, HexBytes(ENDPOINT).to_hex());
    assert_eq!(access.device_id, HexBytes([3; 16]).to_hex());
    assert!(access.credential_expires_at_ms > now_ms());
    quiet(&setups);
    assert!(handle.is_finished());
    assert!(!handle.cancel(), "cancel after completion changes nothing");

    // The next attempt may start as soon as the terminal callback arrived.
    let (observer, setups) = setup_observer();
    let retry = client.enroll("Test phone".into(), observer).unwrap();
    assert!(connecting(&setups).attempt > context.attempt);
    drop(retry);
    assert!(matches!(next(&setups), Setup::Finished(_)));
    client.close().unwrap();
}

#[test]
fn cancel_before_the_proof_write_is_canceled_and_late_replies_are_ignored() {
    let host = peer(Host::Silent);
    let mut unlocks = vec![Unlock::Key(
        device_key().verifying_key().to_bytes().to_vec(),
    )];
    unlocks.extend((0..11).map(|_| Unlock::Hold));
    let (signer, fake) = fake_signer(unlocks, false);
    let client = CrewClient::gateway(trusted(host.port), signer).unwrap();

    let (observer, setups) = setup_observer();
    let handle = client.enroll("Test phone".into(), observer).unwrap();
    let context = connecting(&setups);
    let reply = fake.held.recv_timeout(WAIT).unwrap();
    assert!(reply.is_pending());
    assert!(handle.cancel());
    assert_eq!(finished(&setups, &context), EnrollmentOutcome::Canceled);
    quiet(&setups);
    assert!(!reply.is_pending());
    assert_eq!(
        reply.complete(device_key().verifying_key().to_bytes().to_vec()),
        Err(ReplyError::AlreadyCompleted)
    );
    assert_eq!(
        reply.fail(SignerError::Locked),
        Err(ReplyError::AlreadyCompleted)
    );

    // Cancel at once: it may land before or after Connecting is published.
    // Either way the attempt ends Canceled with its own context, never failed.
    let mut attempts = BTreeSet::from([context.attempt]);
    for _ in 0..10 {
        let (observer, setups) = setup_observer();
        let handle = client.enroll("Test phone".into(), observer).unwrap();
        assert!(handle.cancel());
        let context = connecting(&setups);
        assert_eq!(finished(&setups, &context), EnrollmentOutcome::Canceled);
        quiet(&setups);
        assert!(attempts.insert(context.attempt), "attempts are unique");
    }
    while let Ok(reply) = fake.held.try_recv() {
        assert!(!reply.is_pending());
    }
    assert!(
        host.proofs.recv_timeout(QUIET).is_err(),
        "no attempt reached the host"
    );
    client.close().unwrap();
}

#[test]
fn cancel_or_handle_drop_after_the_proof_write_is_outcome_unknown() {
    let host = peer(Host::Silent);
    let (signer, _fake) = fake_signer(Vec::new(), false);
    let client = CrewClient::gateway(trusted(host.port), signer).unwrap();
    for drop_handle in [false, true] {
        let (observer, setups) = setup_observer();
        let handle = client.enroll("Test phone".into(), observer).unwrap();
        let context = connecting(&setups);
        assert!(matches!(
            host.proofs.recv_timeout(WAIT).unwrap(),
            Some(native::HandshakeCall::NativeEnroll(_))
        ));
        if drop_handle {
            drop(handle);
        } else {
            assert!(handle.cancel());
        }
        // The proof may have reached durable approval: never Canceled.
        assert_eq!(
            finished(&setups, &context),
            EnrollmentOutcome::OutcomeUnknown
        );
        quiet(&setups);
    }
    client.close().unwrap();
}

#[test]
fn startup_failure_reports_failed_once_and_a_concurrent_attempt_is_refused() {
    let host = Unused::new();
    let mut unlocks = vec![Unlock::Key(
        device_key().verifying_key().to_bytes().to_vec(),
    )];
    unlocks.push(Unlock::Hold);
    let (signer, fake) = fake_signer(unlocks, false);
    let client = CrewClient::gateway(trusted(host.port()), signer).unwrap();

    for label in ["", " ", "bad\nlabel", &"x".repeat(65)] {
        let (observer, setups) = setup_observer();
        let handle = client.enroll(label.into(), observer).unwrap();
        match next(&setups) {
            Setup::Failed(reason) => assert_eq!(reason.code, "invalid_profile"),
            other => panic!("startup failure must use failed: {other:?}"),
        }
        quiet(&setups);
        assert!(handle.is_finished());
        assert!(!handle.cancel());
    }

    let (observer, setups) = setup_observer();
    let first = client.enroll("Test phone".into(), observer).unwrap();
    let context = connecting(&setups);
    let reply = fake.held.recv_timeout(WAIT).unwrap();
    let (second, second_setups) = setup_observer();
    assert_eq!(
        refused(client.enroll("Test phone".into(), second)),
        failure(transport::GatewayError::Busy)
    );
    quiet(&second_setups);
    assert!(first.cancel());
    assert_eq!(finished(&setups, &context), EnrollmentOutcome::Canceled);
    assert!(!reply.is_pending());
    assert!(!host.dialed());
    client.close().unwrap();
}

#[test]
fn mismatching_signatures_are_refused_before_any_proof_is_sent() {
    let host = peer(Host::Silent);
    let (signer, _fake) = fake_signer(Vec::new(), true);
    let client = CrewClient::gateway(trusted(host.port), signer).unwrap();
    let (observer, setups) = setup_observer();
    let _handle = client.enroll("Test phone".into(), observer).unwrap();
    let context = connecting(&setups);
    let EnrollmentOutcome::Failed { reason } = finished(&setups, &context) else {
        panic!("a forged enrollment proof must fail");
    };
    assert_eq!(reason.code, "invalid_proof");
    assert_eq!(host.proofs.recv_timeout(WAIT).unwrap(), None);

    // Device authentication applies the same check before its proof write.
    client.connect(PROFILE_ID.into()).unwrap();
    assert_eq!(host.proofs.recv_timeout(WAIT).unwrap(), None);
    client.close().unwrap();
}

#[test]
fn connect_signs_the_sealed_auth_transcript_through_the_foreign_signer() {
    let host = peer(Host::Silent);
    let (signer, fake) = fake_signer(Vec::new(), false);
    let client = CrewClient::gateway(trusted(host.port), signer).unwrap();
    client.connect(PROFILE_ID.into()).unwrap();
    let Some(native::HandshakeCall::NativeAuth(params)) = host.proofs.recv_timeout(WAIT).unwrap()
    else {
        panic!("device authentication proof expected");
    };
    assert_eq!(params.purpose, native::ChannelPurpose::Index);
    assert_eq!(params.workspace, None);
    let expected =
        native::auth_transcript(HexBytes(ENDPOINT), leaf(), HexBytes(NONCE), &params).unwrap();
    assert!(verify(params.public_key, &expected, params.signature));
    let bytes = loop {
        match fake.calls.recv_timeout(WAIT).unwrap() {
            Call::PublicKey => {}
            Call::Auth {
                transcript,
                reply,
                bytes,
            } => {
                assert_eq!(transcript, "AuthTranscript(<redacted>)");
                assert_eq!(reply, "SignatureReply(<redacted>)");
                break bytes;
            }
            other => panic!("unexpected signer call {other:?}"),
        }
    };
    assert_eq!(bytes, expected);
    assert!(bytes.len() <= native::AUTH_TRANSCRIPT_MAX);
    client.close().unwrap();
}

#[test]
fn a_changed_device_key_fails_setup_without_dialing() {
    let host = Unused::new();
    let (signer, _fake) = fake_signer(
        vec![
            Unlock::Key(device_key().verifying_key().to_bytes().to_vec()),
            Unlock::Key(other_key().verifying_key().to_bytes().to_vec()),
        ],
        false,
    );
    let client = CrewClient::gateway(trusted(host.port()), signer).unwrap();
    let (observer, setups) = setup_observer();
    let _handle = client.enroll("Test phone".into(), observer).unwrap();
    let context = connecting(&setups);
    let EnrollmentOutcome::Failed { reason } = finished(&setups, &context) else {
        panic!("a key change must fail");
    };
    assert_eq!(reason.code, "invalid_proof");
    assert!(!host.dialed());
    client.close().unwrap();
}

#[test]
fn close_ends_pending_setup_and_retirement_is_local_and_final() {
    let host = Unused::new();
    let mut unlocks = vec![Unlock::Key(
        device_key().verifying_key().to_bytes().to_vec(),
    )];
    unlocks.push(Unlock::Hold);
    let (signer, fake) = fake_signer(unlocks, false);
    let client = CrewClient::gateway(trusted(host.port()), signer).unwrap();
    let (observer, setups) = setup_observer();
    let _handle = client.enroll("Test phone".into(), observer).unwrap();
    let context = connecting(&setups);
    let reply = fake.held.recv_timeout(WAIT).unwrap();
    client.close().unwrap();
    assert_eq!(finished(&setups, &context), EnrollmentOutcome::Canceled);
    quiet(&setups);
    assert!(!reply.is_pending());

    let (signer, fake) = fake_signer(Vec::new(), false);
    let client = CrewClient::gateway(trusted(host.port()), signer).unwrap();
    client.retire_device_access().unwrap();
    let calls: Vec<_> = std::iter::from_fn(|| fake.calls.recv_timeout(QUIET).ok()).collect();
    assert!(matches!(calls.as_slice(), [Call::PublicKey, Call::Retire]));
    let (observer, setups) = setup_observer();
    assert_eq!(
        refused(client.enroll("Test phone".into(), observer)),
        failure(transport::GatewayError::Transport(TransportEnd::Stopped))
    );
    quiet(&setups);
    assert_eq!(
        client.connect(PROFILE_ID.into()),
        Err(ClientError::ClosedClient)
    );
    assert!(!host.dialed());
}
