//! The exported gateway objects against a real Crew daemon and native gateway
//! on loopback, in a child process with private runtime, state and config
//! directories. Every client step uses the Swift-facing facade: profile import
//! and trust, factory, enrollment, connection, availability, planning and
//! retirement. Host-console approvals and grant changes use the gateway's own
//! private admin socket, as the operator's `crew native` commands do. No
//! operator service, key, enrollment window or registered team is touched.
#![cfg(all(feature = "gateway-tests", target_os = "linux"))]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crew_client::api::{
    ActionIntent, ActionKind, ActionPlan, ClientNotice, ClientScope, ConnectionMode,
    EndpointStatus, Entity, EntityKey, OperationOutcomeKind, ReadOptions, ResourceKey,
    ResourceKind, ResourceRequest, ResourceStatus, TerminalChunk, ViewQuery, WorkspaceStatus,
};
use crew_client_ffi::callbacks::ClientObserver;
use crew_client_ffi::gateway::{
    normalize_fingerprint, parse_gateway_profile, AuthTranscript, DeviceSigner, EnrollTranscript,
    EnrollmentHandle, EnrollmentOutcome, EnrollmentProgress, EnrollmentResult, EnrollmentUpdate,
    GatewayDeviceAccess, GatewayProfile, PublicKeyReply, RetirementReply, SetupObserver,
    SignatureReply,
};
use crew_client_ffi::records::{ActionAvailability, ClientError, ClientReason, ViewPayload};
use crew_client_ffi::service::CrewClient;
use crew_protocol::native::HexBytes;
use ed25519_dalek::Signer as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const CERT: &[u8] = include_bytes!("../../crew-client-transport/tests/fixtures/gateway/cert.pem");
const KEY: &[u8] = include_bytes!("../../crew-client-transport/tests/fixtures/gateway/key.pem");
const WAIT: Duration = Duration::from_secs(10);
const PROFILE_ID: &str = "isolated-host";
const CHILD: &str = "CREW_FFI_GATEWAY_TEST_CHILD";

/// Re-run one test in a child with private XDG directories and no inherited
/// Git, Crew or Herdr context. Returns true in the parent after it passed.
fn isolated(name: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    let root = tempfile::Builder::new()
        .prefix("crew-ffi-gw")
        .tempdir_in("/tmp")
        .unwrap();
    let mut process = std::process::Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| {
            key.starts_with("GIT_") || key.starts_with("HERDR_") || key.starts_with("CREW_")
        }) {
            process.env_remove(key);
        }
    }
    let output = process
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, "1")
        .env("XDG_RUNTIME_DIR", root.path().join("run"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("CREW_GLOBAL_CONFIG", root.path().join("global.toml"))
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated {name}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
    true
}

fn admin(method: &str, params: Value) -> Value {
    crew::native::admin_call(method, params).unwrap()
}

/// The isolated daemon and gateway run on their own runtime; the facade
/// calls below block only the test thread, as a platform worker would.
struct Host {
    runtime: tokio::runtime::Runtime,
    _workspace: tempfile::TempDir,
    workspace: PathBuf,
    socket: PathBuf,
    gateway: Option<tokio::task::JoinHandle<()>>,
    daemon: Option<tokio::task::JoinHandle<()>>,
}

impl Host {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let workspace = tempfile::Builder::new()
            .prefix("crew-ffi-gw-w")
            .tempdir_in("/tmp")
            .unwrap();
        let canonical = workspace.path().canonicalize().unwrap();
        let daemon = runtime
            .block_on(crew::daemon::start(crew::daemon::DaemonOptions {
                workspace: canonical.clone(),
                crew_bin: std::env::current_exe().unwrap(),
                pin: None,
            }))
            .unwrap();
        let socket = daemon.socket().to_path_buf();
        let daemon = runtime.spawn(async move {
            daemon.serve().await.unwrap();
        });
        let state = crew_core::paths::runtime_dir();
        let native = state.join("native");
        crew_core::paths::ensure_private_dir(&native).unwrap();
        crew_core::paths::write_private_file(&native.join("key.pem"), KEY).unwrap();
        std::fs::write(native.join("cert.pem"), CERT).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let gateway = crew::native::Gateway::open(crew::native::Config {
            listen: address,
            endpoint: format!("wss://127.0.0.1:{}/native/ws", address.port()),
            certificate: native.join("cert.pem"),
            private_key: native.join("key.pem"),
            name: "Isolated test host".into(),
            runtime: state,
        })
        .unwrap();
        let gateway = runtime.spawn(async move {
            gateway.run().await.unwrap();
        });
        let deadline = Instant::now() + WAIT;
        while crew::native::admin_call("native_status", json!({})).is_err() {
            assert!(Instant::now() < deadline, "isolated gateway startup");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            runtime,
            _workspace: workspace,
            workspace: canonical,
            socket,
            gateway: Some(gateway),
            daemon: Some(daemon),
        }
    }

    /// What the person pasted (the complete `crew native status` output) and
    /// the fingerprint they read at the console, typed in groups of four.
    fn imported(&self) -> (crew_client_ffi::gateway::PublicProfile, String) {
        let status = admin("native_status", json!({}));
        let pasted = serde_json::to_string_pretty(&status).unwrap();
        let read = status["profile"]["tls_leaf_sha256"]
            .as_str()
            .unwrap()
            .to_ascii_uppercase()
            .as_bytes()
            .chunks(4)
            .map(|group| std::str::from_utf8(group).unwrap().to_owned())
            .collect::<Vec<_>>()
            .join(" ");
        (parse_gateway_profile(pasted).unwrap(), read)
    }

    fn profile(&self) -> Arc<GatewayProfile> {
        let (profile, read) = self.imported();
        assert_eq!(
            normalize_fingerprint(read.clone()).as_deref(),
            Some(profile.tls_leaf_sha256.as_str())
        );
        GatewayProfile::trust(PROFILE_ID.into(), profile, read).unwrap()
    }

    fn close(mut self) {
        admin("native_stop", json!({}));
        let gateway = self.gateway.take().unwrap();
        self.runtime
            .block_on(async { tokio::time::timeout(WAIT, gateway).await })
            .unwrap()
            .unwrap();
        let (mut caller, _) =
            crew_core::client::Client::operator(&self.socket, "ffi-gateway-test").unwrap();
        let _: Value = caller
            .call(crew_core::rpc::Call::Shutdown { stop_agents: true })
            .unwrap();
        let daemon = self.daemon.take().unwrap();
        self.runtime
            .block_on(async { tokio::time::timeout(WAIT, daemon).await })
            .unwrap()
            .unwrap();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if self.daemon.is_some() {
            if let Ok((mut caller, _)) =
                crew_core::client::Client::operator(&self.socket, "ffi-gateway-cleanup")
            {
                let _ = caller.set_timeout(Some(Duration::from_secs(3)));
                let _ = caller.call_value(crew_core::rpc::Call::Shutdown { stop_agents: true });
            }
        }
        for task in [self.gateway.take(), self.daemon.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
    }
}

/// An enrollment signing request the stand-in keeps unanswered.
type HeldEnrollment = (Arc<EnrollTranscript>, Arc<SignatureReply>);

/// A platform signer stand-in. It answers through the reply objects exactly
/// as an OS credential adapter must, and holds enrollment signing when asked.
struct SignerState {
    key: ed25519_dalek::SigningKey,
    auth: AtomicUsize,
    enroll: AtomicUsize,
    retired: AtomicBool,
    hold_enrollment: Mutex<Option<Sender<HeldEnrollment>>>,
}

impl SignerState {
    fn new(seed: u8) -> Arc<Self> {
        Arc::new(Self {
            key: ed25519_dalek::SigningKey::from_bytes(&[seed; 32]),
            auth: AtomicUsize::new(0),
            enroll: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
            hold_enrollment: Mutex::new(None),
        })
    }

    fn key_hash(&self) -> String {
        HexBytes::<32>(Sha256::digest(self.key.verifying_key().as_bytes()).into()).to_hex()
    }

    fn signer(self: &Arc<Self>) -> Box<dyn DeviceSigner> {
        Box::new(Signer(self.clone()))
    }
}

struct Signer(Arc<SignerState>);

impl DeviceSigner for Signer {
    fn public_key(&self, reply: Arc<PublicKeyReply>) {
        reply
            .complete(self.0.key.verifying_key().to_bytes().to_vec())
            .unwrap();
    }

    fn sign_auth(&self, transcript: Arc<AuthTranscript>, reply: Arc<SignatureReply>) {
        self.0.auth.fetch_add(1, Ordering::AcqRel);
        let signature = self.0.key.sign(&transcript.signing_bytes());
        reply.complete(signature.to_bytes().to_vec()).unwrap();
    }

    fn sign_enrollment(&self, transcript: Arc<EnrollTranscript>, reply: Arc<SignatureReply>) {
        self.0.enroll.fetch_add(1, Ordering::AcqRel);
        if let Some(hold) = self.0.hold_enrollment.lock().unwrap().as_ref() {
            hold.send((transcript, reply)).unwrap();
            return;
        }
        let signature = self.0.key.sign(&transcript.signing_bytes());
        reply.complete(signature.to_bytes().to_vec()).unwrap();
    }

    fn retire(&self, reply: Arc<RetirementReply>) {
        self.0.retired.store(true, Ordering::Release);
        reply.complete().unwrap();
    }
}

#[derive(Debug)]
enum Setup {
    Changed(EnrollmentUpdate),
    Finished(EnrollmentResult),
    Failed(ClientReason),
}

struct Setups(Sender<Setup>);

impl SetupObserver for Setups {
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

fn enroll(client: &CrewClient, label: &str) -> (Arc<EnrollmentHandle>, Receiver<Setup>) {
    let (sender, receiver) = mpsc::channel();
    let handle = client
        .enroll(label.into(), Box::new(Setups(sender)))
        .unwrap();
    (handle, receiver)
}

fn next_setup(setups: &Receiver<Setup>) -> Setup {
    setups.recv_timeout(WAIT).expect("setup callback")
}

/// Wait for the pinned host's accepted intent; returns its id and code.
fn awaiting(setups: &Receiver<Setup>) -> (EnrollmentUpdate, String, String) {
    let Setup::Changed(connecting) = next_setup(setups) else {
        panic!("Connecting first")
    };
    assert_eq!(connecting.progress, EnrollmentProgress::Connecting);
    let Setup::Changed(update) = next_setup(setups) else {
        panic!("AwaitingApproval second")
    };
    assert_eq!(update.context, connecting.context);
    let EnrollmentProgress::AwaitingApproval {
        enrollment_id,
        comparison_code,
        ..
    } = update.progress.clone()
    else {
        panic!("AwaitingApproval expected: {update:?}")
    };
    (update, enrollment_id, comparison_code)
}

fn finished(setups: &Receiver<Setup>, update: &EnrollmentUpdate) -> EnrollmentOutcome {
    match next_setup(setups) {
        Setup::Finished(result) => {
            assert_eq!(result.context, update.context);
            assert!(setups.recv_timeout(Duration::from_millis(300)).is_err());
            result.outcome
        }
        Setup::Failed(reason) => panic!("a started attempt never fails to start: {}", reason.code),
        other => panic!("one terminal result expected: {other:?}"),
    }
}

/// The operator's console approval of exactly the code this device shows.
fn approve(host: &Host, signer: &SignerState, enrollment_id: &str, code: &str, control: bool) {
    let pending = admin("native_pending", json!({}));
    let shown: Vec<_> = pending
        .as_array()
        .unwrap()
        .iter()
        .filter(|intent| intent["enrollment_id"] == enrollment_id)
        .collect();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0]["comparison_code"], code);
    assert_eq!(shown[0]["public_key_hash"], signer.key_hash().as_str());
    admin(
        "native_approve",
        json!({"enrollment_id": enrollment_id, "comparison_code": code,
            "public_key_hash": signer.key_hash(),
            "grants": [{"workspace": host.workspace, "control": control, "terminal": false}],
            "lifetime_secs": 3600}),
    );
}

fn enrolled(host: &Host, client: &CrewClient, signer: &SignerState) -> GatewayDeviceAccess {
    admin("native_enroll", json!({}));
    let (_handle, setups) = enroll(client, "Isolated phone");
    let (update, enrollment_id, code) = awaiting(&setups);
    approve(host, signer, &enrollment_id, &code, false);
    match finished(&setups, &update) {
        EnrollmentOutcome::Approved { access } => access,
        other => panic!("approval expected: {other:?}"),
    }
}

struct Notices(Sender<ClientNotice>);

impl ClientObserver for Notices {
    fn changed(&self, notice: ClientNotice) {
        let _ = self.0.send(notice);
    }
    fn terminal(&self, _: TerminalChunk) {}
}

fn observe(client: &CrewClient) -> Receiver<ClientNotice> {
    let (sender, receiver) = mpsc::channel();
    client.observe(Box::new(Notices(sender))).unwrap();
    receiver
}

fn notice(
    notices: &Receiver<ClientNotice>,
    wanted: impl Fn(&ClientNotice) -> bool,
) -> ClientNotice {
    let deadline = Instant::now() + WAIT;
    let mut last = None;
    loop {
        let next = notices
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| panic!("typed gateway notice; last {last:?}"));
        if wanted(&next) {
            return next;
        }
        last = Some(next);
    }
}

/// Select the one granted workspace from the authenticated listing and wait
/// for the gateway-verified route to go live with a fresh attempt.
fn select(client: &CrewClient, notices: &Receiver<ClientNotice>, after: u64) -> ClientScope {
    let deadline = Instant::now() + WAIT;
    let listing = loop {
        if let Some(listing) = client.workspaces().unwrap() {
            if listing.joined.len() == 1
                && listing.joined[0].state == crew_protocol::WorkspaceState::Running
            {
                break listing;
            }
        }
        notices
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("granted workspace listing");
    };
    assert!(listing.others.is_empty(), "ungranted sessions stay hidden");
    client
        .select_workspace(listing.joined[0].id.clone())
        .unwrap();
    notice(notices, |next| {
        next.workspace == WorkspaceStatus::Live
            && next
                .scope
                .as_ref()
                .is_some_and(|scope| scope.attempt > after)
    })
    .scope
    .unwrap()
}

fn send(scope: &ClientScope, id: &str) -> ActionIntent {
    ActionIntent::SendMessage {
        scope: scope.clone(),
        to: "*".into(),
        body: "Scoped submission from the Swift facade".into(),
        message_id: id.into(),
    }
}

fn availability(client: &CrewClient, intent: &ActionIntent) -> ActionAvailability {
    let view = client
        .query(ViewQuery::Availability {
            intent: Box::new(intent.clone()),
        })
        .unwrap();
    assert_eq!(
        view.scope.as_ref(),
        Some(crew_client::actions::action_scope(intent))
    );
    let ViewPayload::Availability(value) = view.payload else {
        panic!("typed availability expected")
    };
    assert_eq!(value.action, ActionKind::SendMessage);
    value
}

fn succeeded(notices: &Receiver<ClientNotice>, operation_id: u64) {
    notice(notices, |next| {
        next.outcomes.iter().any(|outcome| {
            outcome.operation_id == operation_id && outcome.kind == OperationOutcomeKind::Success
        })
    });
}

#[test]
fn swift_facade_enrolls_connects_and_follows_real_grant_changes_then_retires_locally() {
    let name = "swift_facade_enrolls_connects_and_follows_real_grant_changes_then_retires_locally";
    if isolated(name) {
        return;
    }
    let host = Host::start();
    let signer = SignerState::new(51);
    let client = CrewClient::gateway(host.profile(), signer.signer()).unwrap();
    assert_eq!(client.connection_context().unwrap(), None);
    assert_eq!(
        signer.auth.load(Ordering::Acquire),
        0,
        "factory never dials"
    );

    let access = enrolled(&host, &client, &signer);
    let devices = admin("native_devices", json!({}));
    assert_eq!(devices.as_array().unwrap().len(), 1);
    assert_eq!(devices[0]["id"], access.device_id.as_str());
    assert_eq!(signer.enroll.load(Ordering::Acquire), 1);

    let notices = observe(&client);
    client.connect(PROFILE_ID.into()).unwrap();
    let scope = select(&client, &notices, 0);
    assert_eq!(scope.endpoint_id, access.endpoint_id);
    assert_eq!(scope.daemon_pid, Some(std::process::id()));
    assert!(scope.daemon_started_at_ms.is_some());
    assert_eq!(
        scope.workspace_id,
        crew_core::paths::session_id(&host.workspace.join(".crew"))
    );
    let context = client.connection_context().unwrap().unwrap();
    assert_eq!(context.mode, ConnectionMode::ScopedGateway);
    assert_eq!(context.scope, scope);
    assert!(
        signer.auth.load(Ordering::Acquire) >= 2,
        "index and workspace proofs"
    );

    // Read-only grant: the shared availability answer and the planner agree.
    let denied = send(&scope, "read-only-denied");
    let answer = availability(&client, &denied);
    assert!(!answer.available);
    let ActionPlan::Unavailable { reason } = client.plan(denied).unwrap() else {
        panic!("read-only send must be unavailable")
    };
    assert_eq!(answer.reason, Some(reason));
    let key = ResourceKey {
        entity: EntityKey {
            scope: scope.clone(),
            entity: Entity::Workspace,
        },
        kind: ResourceKind::History,
        options: ReadOptions::Bounded { limit: 10 },
    };
    let ActionPlan::Dispatch { operation_id } = client
        .plan(ActionIntent::Read {
            scope: scope.clone(),
            resource: ResourceRequest { key: key.clone() },
        })
        .unwrap()
    else {
        panic!("read dispatch")
    };
    succeeded(&notices, operation_id);
    let ViewPayload::Evidence(evidence) =
        client.query(ViewQuery::Evidence { key }).unwrap().payload
    else {
        panic!("typed evidence expected")
    };
    assert!(matches!(
        evidence.status,
        ResourceStatus::Empty | ResourceStatus::Ready
    ));

    // A real grant change fences the old route; the new one carries Control.
    admin(
        "native_grant",
        json!({"device_id": access.device_id,
            "selection": {"workspace": host.workspace, "control": true, "terminal": false}}),
    );
    notice(&notices, |next| {
        next.workspace == WorkspaceStatus::AccessRevoked
    });
    let stale = availability(&client, &send(&scope, "old-scope"));
    assert!(!stale.available, "the old attempt's scope stays retired");
    let controlled = select(&client, &notices, scope.attempt);
    assert_eq!(controlled.generation, scope.generation);
    let allowed = send(&controlled, "control-allowed");
    assert!(availability(&client, &allowed).available);
    let ActionPlan::Dispatch { operation_id } = client.plan(allowed).unwrap() else {
        panic!("control dispatch")
    };
    succeeded(&notices, operation_id);

    // Workspace revocation at the console ends the route, not the device.
    admin(
        "native_revoke",
        json!({"device_id": access.device_id, "workspace": controlled.workspace_id}),
    );
    let revoked = notice(&notices, |next| {
        next.workspace == WorkspaceStatus::AccessRevoked
    });
    assert_eq!(revoked.endpoint, EndpointStatus::Authenticated);
    assert_eq!(client.connection_context().unwrap(), None);
    let after = send(&controlled, "revoked-control");
    assert!(!availability(&client, &after).available);
    assert!(matches!(
        client.plan(after).unwrap(),
        ActionPlan::Unavailable { .. }
    ));

    // Local retirement closes the client and the key, never the host record.
    client.retire_device_access().unwrap();
    assert!(signer.retired.load(Ordering::Acquire));
    assert_eq!(
        client.connect(PROFILE_ID.into()),
        Err(ClientError::ClosedClient)
    );
    assert_eq!(
        admin("native_devices", json!({})).as_array().unwrap().len(),
        1,
        "the host grant remains until revoked at the console"
    );
    host.close();
}

#[test]
fn swift_facade_enrollment_cancel_unknown_devices_and_key_recovery_follow_the_host() {
    let name = "swift_facade_enrollment_cancel_unknown_devices_and_key_recovery_follow_the_host";
    if isolated(name) {
        return;
    }
    let host = Host::start();

    // An unenrolled key is refused by the real host and gains no listing.
    let stranger = SignerState::new(52);
    let unknown = CrewClient::gateway(host.profile(), stranger.signer()).unwrap();
    let notices = observe(&unknown);
    unknown.connect(PROFILE_ID.into()).unwrap();
    let refused = notice(&notices, |next| next.endpoint == EndpointStatus::SignedOut);
    assert_eq!(refused.reason.unwrap().code, "authentication");
    assert_eq!(unknown.workspaces().unwrap(), None);
    unknown.close().unwrap();

    // Cancel while the proof is still being signed: nothing reached the host.
    admin("native_enroll", json!({}));
    let signer = SignerState::new(53);
    let (held, holding) = mpsc::channel();
    *signer.hold_enrollment.lock().unwrap() = Some(held);
    let client = CrewClient::gateway(host.profile(), signer.signer()).unwrap();
    let (handle, setups) = enroll(&client, "Canceled before proof");
    let Setup::Changed(connecting) = next_setup(&setups) else {
        panic!("Connecting first")
    };
    let (_transcript, reply) = holding.recv_timeout(WAIT).unwrap();
    assert!(handle.cancel());
    assert_eq!(finished(&setups, &connecting), EnrollmentOutcome::Canceled);
    assert!(!reply.is_pending(), "the canceled request is closed");
    assert!(admin("native_pending", json!({}))
        .as_array()
        .unwrap()
        .is_empty());
    signer.hold_enrollment.lock().unwrap().take();

    // Cancel after the host accepted the proof: the outcome is unknown and
    // the app must not enroll again; the host created no device.
    admin("native_enroll", json!({}));
    let (handle, setups) = enroll(&client, "Canceled after proof");
    let (update, _, _) = awaiting(&setups);
    assert!(update.context.attempt > connecting.context.attempt);
    assert!(handle.cancel());
    assert_eq!(
        finished(&setups, &update),
        EnrollmentOutcome::OutcomeUnknown
    );
    assert!(admin("native_devices", json!({}))
        .as_array()
        .unwrap()
        .is_empty());

    // A completed enrollment, then a fresh client with the same key
    // recovers access by authenticating; enrolling it again is refused.
    let access = enrolled(&host, &client, &signer);
    client.close().unwrap();
    let recovered = CrewClient::gateway(host.profile(), signer.signer()).unwrap();
    let notices = observe(&recovered);
    recovered.connect(PROFILE_ID.into()).unwrap();
    let scope = select(&recovered, &notices, 0);
    assert_eq!(scope.endpoint_id, access.endpoint_id);
    admin("native_enroll", json!({}));
    let (_handle, setups) = enroll(&recovered, "Same key again");
    let Setup::Changed(again) = next_setup(&setups) else {
        panic!("Connecting first")
    };
    let EnrollmentOutcome::Failed { reason } = finished(&setups, &again) else {
        panic!("re-enrolling an approved key must fail")
    };
    assert_eq!(reason.code, "authentication");
    assert_eq!(
        admin("native_devices", json!({})).as_array().unwrap().len(),
        1
    );

    // Device revocation at the console signs this device out.
    admin("native_revoke", json!({"device_id": access.device_id}));
    let signed_out = notice(&notices, |next| next.endpoint == EndpointStatus::SignedOut);
    assert!(signed_out.reason.is_some());
    assert_eq!(recovered.connection_context().unwrap(), None);
    recovered.close().unwrap();
    host.close();
}
