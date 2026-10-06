//! Real isolated gateway/daemon composition on loopback. No active operator
//! service, key, enrollment window or registered team is used by these tests.
#![cfg(all(feature = "gateway-tests", target_os = "linux"))]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc as sync, Arc, Mutex};
use std::time::{Duration, Instant};

use crew_client::api::*;
use crew_client::policy::NATIVE_LIMITS;
use crew_client_transport::gateway::*;
use crew_client_transport::{ClientObserver, DriverConfig, NativeSession, Protocol6Codec};
use crew_protocol::native::{HexBytes, PublicKey, PublicProfile, Signature};
use ed25519_dalek::Signer as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const CERT: &[u8] = include_bytes!("fixtures/gateway/cert.pem");
const KEY: &[u8] = include_bytes!("fixtures/gateway/key.pem");
const WAIT: Duration = Duration::from_secs(8);

fn isolated(name: &str) -> bool {
    if std::env::var_os("CREW_GATEWAY_TEST_CHILD").is_some() {
        return false;
    }
    let root = tempfile::Builder::new()
        .prefix("crew-gwc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut process = std::process::Command::new(std::env::current_exe().unwrap());
    let validation_binary = std::env::var_os("CREW_NATIVE_VALIDATION_BIN");
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| {
            key.starts_with("GIT_") || key.starts_with("HERDR_") || key.starts_with("CREW_")
        }) {
            process.env_remove(key);
        }
    }
    if let Some(binary) = validation_binary {
        process.env("CREW_NATIVE_VALIDATION_BIN", binary);
    }
    let output = process
        .args(["--exact", name, "--nocapture"])
        .env("CREW_GATEWAY_TEST_CHILD", "1")
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

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

async fn admin(method: &'static str, params: Value) -> Value {
    tokio::task::spawn_blocking(move || crew::native::admin_call(method, params))
        .await
        .unwrap()
        .unwrap()
}

struct Host {
    _workspace: tempfile::TempDir,
    workspace: PathBuf,
    socket: PathBuf,
    profile: PublicProfile,
    config: crew::native::Config,
    gateway: Option<tokio::task::JoinHandle<()>>,
    daemon: Option<tokio::task::JoinHandle<()>>,
}

impl Host {
    async fn start() -> Self {
        Self::start_with_crew(std::env::current_exe().unwrap()).await
    }

    async fn start_with_crew(crew_bin: PathBuf) -> Self {
        let workspace = tempfile::Builder::new()
            .prefix("crew-gw-w")
            .tempdir_in("/tmp")
            .unwrap();
        let canonical = workspace.path().canonicalize().unwrap();
        let daemon = crew::daemon::start(crew::daemon::DaemonOptions {
            workspace: canonical.clone(),
            crew_bin,
            pin: None,
        })
        .await
        .unwrap();
        let socket = daemon.socket().to_path_buf();
        let daemon = tokio::spawn(async move {
            daemon.serve().await.unwrap();
        });
        let runtime = crew_core::paths::runtime_dir();
        let native = runtime.join("native");
        crew_core::paths::ensure_private_dir(&native).unwrap();
        crew_core::paths::write_private_file(&native.join("key.pem"), KEY).unwrap();
        std::fs::write(native.join("cert.pem"), CERT).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let config = crew::native::Config {
            listen: address,
            endpoint: format!("wss://127.0.0.1:{}/native/ws", address.port()),
            certificate: native.join("cert.pem"),
            private_key: native.join("key.pem"),
            name: "Isolated test host".into(),
            runtime,
        };
        let gateway = crew::native::Gateway::open(config.clone()).unwrap();
        let profile = gateway.profile();
        let task = tokio::spawn(async move {
            gateway.run().await.unwrap();
        });
        let deadline = Instant::now() + WAIT;
        loop {
            let ready = tokio::task::spawn_blocking(|| {
                crew::native::admin_call("native_status", json!({}))
            })
            .await
            .unwrap();
            if ready.is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "isolated gateway startup");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            _workspace: workspace,
            workspace: canonical,
            socket,
            profile,
            config,
            gateway: Some(task),
            daemon: Some(daemon),
        }
    }

    fn client(&self, signer: Arc<dyn DeviceSigner>) -> GatewayClient {
        GatewayClient::new(
            GatewayProfile::trust(
                "isolated".into(),
                self.profile.clone(),
                self.profile.tls_leaf_sha256,
            )
            .unwrap(),
            signer,
        )
    }

    async fn restart(&mut self) {
        admin("native_stop", json!({})).await;
        tokio::time::timeout(WAIT, self.gateway.take().unwrap())
            .await
            .unwrap()
            .unwrap();
        let gateway = crew::native::Gateway::open(self.config.clone()).unwrap();
        assert_eq!(gateway.profile(), self.profile);
        self.gateway = Some(tokio::spawn(async move {
            gateway.run().await.unwrap();
        }));
        let deadline = Instant::now() + WAIT;
        loop {
            let ready = tokio::task::spawn_blocking(|| {
                crew::native::admin_call("native_status", json!({}))
            })
            .await
            .unwrap();
            if ready.is_ok() {
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn close(mut self) {
        admin("native_stop", json!({})).await;
        tokio::time::timeout(WAIT, self.gateway.take().unwrap())
            .await
            .unwrap()
            .unwrap();
        let socket = self.socket.clone();
        tokio::task::spawn_blocking(move || {
            let (mut caller, _) =
                crew_core::client::Client::operator(&socket, "gateway-test").unwrap();
            let _: Value = caller
                .call(crew_core::rpc::Call::Shutdown { stop_agents: true })
                .unwrap();
        })
        .await
        .unwrap();
        tokio::time::timeout(WAIT, self.daemon.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // A failed rich journey still tears down only its synthetic PTY team.
        if self.daemon.is_some() {
            if let Ok((mut caller, _)) =
                crew_core::client::Client::operator(&self.socket, "native-test-cleanup")
            {
                let _ = caller.set_timeout(Some(Duration::from_secs(3)));
                let _ = caller.call_value(crew_core::rpc::Call::Shutdown { stop_agents: true });
            }
        }
        if let Some(task) = self.gateway.take() {
            task.abort();
        }
        if let Some(task) = self.daemon.take() {
            task.abort();
        }
    }
}

#[cfg(feature = "journey-tests")]
#[path = "journey/mod.rs"]
mod journey;

struct TestSigner {
    key: ed25519_dalek::SigningKey,
    auth_calls: AtomicUsize,
    enroll_calls: AtomicUsize,
    block_unlock: AtomicBool,
    unlock_dropped: Arc<AtomicBool>,
    replay_enrollment: AtomicBool,
    prior_proof: Mutex<Option<Signature>>,
}

impl TestSigner {
    fn new(seed: u8) -> Self {
        Self {
            key: ed25519_dalek::SigningKey::from_bytes(&[seed; 32]),
            auth_calls: AtomicUsize::new(0),
            enroll_calls: AtomicUsize::new(0),
            block_unlock: AtomicBool::new(false),
            unlock_dropped: Arc::new(AtomicBool::new(false)),
            replay_enrollment: AtomicBool::new(false),
            prior_proof: Mutex::new(None),
        }
    }
}

impl DeviceSigner for TestSigner {
    fn public_key(&self) -> SignerFuture<'_, PublicKey> {
        Box::pin(async move {
            if self.block_unlock.load(Ordering::Acquire) {
                struct Dropped(Arc<AtomicBool>);
                impl Drop for Dropped {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                let _dropped = Dropped(self.unlock_dropped.clone());
                std::future::pending::<()>().await;
            }
            Ok(HexBytes(self.key.verifying_key().to_bytes()))
        })
    }
    fn sign_auth(&self, transcript: AuthTranscript) -> SignerFuture<'_, Signature> {
        Box::pin(async move {
            self.auth_calls.fetch_add(1, Ordering::AcqRel);
            Ok(HexBytes(
                self.key.sign(transcript.signing_bytes()).to_bytes(),
            ))
        })
    }
    fn sign_enrollment(&self, transcript: EnrollTranscript) -> SignerFuture<'_, Signature> {
        Box::pin(async move {
            self.enroll_calls.fetch_add(1, Ordering::AcqRel);
            let mut prior = self.prior_proof.lock().unwrap();
            if self.replay_enrollment.load(Ordering::Acquire) {
                return Ok(prior.unwrap());
            }
            let proof = HexBytes(self.key.sign(transcript.signing_bytes()).to_bytes());
            *prior = Some(proof);
            Ok(proof)
        })
    }
    fn retire(&self) -> SignerFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

struct Setup(tokio::sync::mpsc::Sender<EnrollmentUpdate>);
impl SetupObserver for Setup {
    fn enrollment_changed(&self, update: EnrollmentUpdate) {
        self.0.try_send(update).unwrap();
    }
}

async fn accepted(
    receiver: &mut tokio::sync::mpsc::Receiver<EnrollmentUpdate>,
) -> EnrollmentUpdate {
    loop {
        let update = tokio::time::timeout(WAIT, receiver.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(update.progress, EnrollmentProgress::AwaitingApproval { .. }) {
            return update;
        }
    }
}

async fn enroll(
    host: &Host,
    client: &GatewayClient,
    signer: &TestSigner,
    control: bool,
    lifetime: u64,
) -> GatewayDeviceAccess {
    enroll_capabilities(host, client, signer, control, false, lifetime).await
}

async fn enroll_capabilities(
    host: &Host,
    client: &GatewayClient,
    signer: &TestSigner,
    control: bool,
    terminal: bool,
    lifetime: u64,
) -> GatewayDeviceAccess {
    admin("native_enroll", json!({})).await;
    let before = crew_core::util::now_ms();
    let (progress, mut updates) = tokio::sync::mpsc::channel(8);
    let enrollment = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .enroll("Isolated device".into(), Arc::new(Setup(progress)))
                .await
                .unwrap()
        }
    });
    let update = accepted(&mut updates).await;
    let EnrollmentProgress::AwaitingApproval {
        enrollment_id,
        comparison_code,
        expires_at_ms,
    } = &update.progress
    else {
        unreachable!()
    };
    // The real server's accepted window cannot silently exceed the client's
    // native-v1 120 s wait bound. No copied server constant is consulted.
    assert!(*expires_at_ms > before && *expires_at_ms <= before + 120_100);
    let key_hash =
        HexBytes::<32>(Sha256::digest(signer.key.verifying_key().as_bytes()).into()).to_hex();
    let pending = admin("native_pending", json!({})).await;
    assert_eq!(pending[0]["comparison_code"], *comparison_code);
    admin("native_approve", json!({"enrollment_id":enrollment_id,"comparison_code":comparison_code,"public_key_hash":key_hash,
        "grants":[{"workspace":host.workspace,"control":control,"terminal":terminal}],"lifetime_secs":lifetime})).await;
    let result = tokio::time::timeout(WAIT, enrollment)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.context, update.context);
    match result.outcome {
        EnrollmentOutcome::Approved { access } => access,
        other => panic!("approval: {other:?}"),
    }
}

struct Observer(sync::Sender<ClientNotice>);
impl ClientObserver for Observer {
    fn changed(&self, notice: ClientNotice) {
        let _ = self.0.send(notice);
    }
    fn terminal(&self, _: TerminalChunk) {}
}

fn notice(
    receiver: &sync::Receiver<ClientNotice>,
    predicate: impl Fn(&ClientNotice) -> bool,
) -> ClientNotice {
    let deadline = Instant::now() + WAIT;
    let mut last = None;
    loop {
        let next = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| panic!("typed gateway notice; last {last:?}"));
        if predicate(&next) {
            return next;
        }
        last = Some(next);
    }
}

type Trace = Arc<Mutex<Vec<ClientInput>>>;
struct TracedCore {
    core: crew_client::ClientCore,
    trace: Trace,
}
impl ClientApi for TracedCore {
    fn apply(
        &mut self,
        input: ClientInput,
        clock: ClockSample,
    ) -> Result<ClientUpdate, ClientError> {
        self.trace.lock().unwrap().push(input.clone());
        self.core.apply(input, clock)
    }
    fn view(&self, query: ViewQuery, clock: ClockSample) -> Result<ViewData, ClientError> {
        self.core.view(query, clock)
    }
    fn plan(
        &mut self,
        intent: ActionIntent,
        clock: ClockSample,
    ) -> Result<PlanUpdate, ClientError> {
        self.core.plan(intent, clock)
    }
    fn confirm(&mut self, id: u64, clock: ClockSample) -> Result<ClientUpdate, ClientError> {
        self.core.confirm(id, clock)
    }
    fn cancel_confirmation(&mut self, id: u64) -> Result<ClientUpdate, ClientError> {
        self.core.cancel_confirmation(id)
    }
}

async fn session(client: &GatewayClient) -> (NativeSession, sync::Receiver<ClientNotice>, Trace) {
    let core = crew_client::ClientCore::new(ClientConfig {
        limits: NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let session = client
        .session(
            TracedCore {
                core,
                trace: trace.clone(),
            },
            Protocol6Codec,
            DriverConfig {
                reconnect: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (sender, receiver) = sync::channel();
    session.observe(Arc::new(Observer(sender))).unwrap();
    session
        .connect(
            client.profile().profile_id().into(),
            client.profile().endpoint_id(),
            None,
        )
        .unwrap();
    (session, receiver, trace)
}

fn select(session: &NativeSession, receiver: &sync::Receiver<ClientNotice>) -> ClientScope {
    let deadline = Instant::now() + WAIT;
    let previous_attempt = session
        .query(ViewQuery::Overview)
        .unwrap()
        .scope
        .map_or(0, |scope| scope.attempt);
    let mut last = None;
    let listing = loop {
        if let Some(listing) = session.workspaces().unwrap() {
            if listing.joined.len() == 1
                && listing.joined[0].state == crew_protocol::WorkspaceState::Running
            {
                break listing;
            }
        }
        last = Some(
            receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "listing wait {error:?}; last {last:?}; listing {:?}",
                        session.workspaces().unwrap()
                    )
                }),
        );
    };
    assert!(listing.others.is_empty());
    assert_eq!(listing.joined.len(), 1);
    session
        .select_workspace(listing.joined[0].id.clone())
        .unwrap();
    notice(receiver, |next| {
        next.workspace == WorkspaceStatus::Live
            && next
                .scope
                .as_ref()
                .is_some_and(|scope| scope.attempt > previous_attempt)
    })
    .scope
    .unwrap()
}

fn send_intent(scope: &ClientScope, id: &str) -> ActionIntent {
    ActionIntent::SendMessage {
        scope: scope.clone(),
        to: "*".into(),
        body: "Isolated scoped submission".into(),
        message_id: id.into(),
    }
}

#[test]
fn real_gateway_enrolls_read_only_and_rebinds_control_after_grant_change_then_revokes_workspace() {
    let name = "real_gateway_enrolls_read_only_and_rebinds_control_after_grant_change_then_revokes_workspace";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let host = Host::start().await;
        let signer = Arc::new(TestSigner::new(31));
        let client = host.client(signer.clone());
        let access = enroll(&host, &client, &signer, false, 3600).await;
        let (session, notices, trace) = session(&client).await;
        let scope = select(&session, &notices);
        assert_eq!(scope.daemon_pid, Some(std::process::id()));
        assert!(scope.daemon_started_at_ms.is_some());
        assert_eq!(scope.workspace_id, crew_core::paths::session_id(&host.workspace.join(".crew")));
        assert!(matches!(session.plan(send_intent(&scope, "denied-read-only")).unwrap(), ActionPlan::Unavailable { .. }));
        let key = ResourceKey { entity: EntityKey { scope: scope.clone(), entity: Entity::Workspace }, kind: ResourceKind::History, options: ReadOptions::Bounded { limit: 10 } };
        let read = session.plan(ActionIntent::Read { scope: scope.clone(), resource: ResourceRequest { key: key.clone() } }).unwrap();
        let ActionPlan::Dispatch { operation_id } = read else { panic!("read dispatch"); };
        notice(&notices, |next| next.outcomes.iter().any(|outcome| outcome.operation_id == operation_id && outcome.kind == OperationOutcomeKind::Success));
        let value = session.query(ViewQuery::Evidence { key }).unwrap();
        assert!(matches!(value.payload, ViewPayload::Evidence(state) if matches!(state.status, ResourceStatus::Empty | ResourceStatus::Ready)));

        admin("native_grant", json!({"device_id":access.device_id,"selection":{"workspace":host.workspace,"control":true,"terminal":false}})).await;
        notice(&notices, |next| next.workspace == WorkspaceStatus::AccessRevoked);
        let newer = select(&session, &notices);
        assert!(newer.attempt > scope.attempt);
        let plan = session.plan(send_intent(&newer, "allowed-control")).unwrap();
        let ActionPlan::Dispatch { operation_id } = plan else { panic!("control dispatch"); };
        notice(&notices, |next| next.outcomes.iter().any(|outcome| outcome.operation_id == operation_id && outcome.kind == OperationOutcomeKind::Success));
        // Retiring a completed Action lane must leave state/index connected.
        assert_eq!(session.connection_context().unwrap().unwrap().scope, newer);
        {
            let recorded = trace.lock().unwrap();
            let unbind = recorded.iter().position(|input| matches!(input, ClientInput::Unbind { scope: old, .. } if old == &scope)).unwrap();
            let bind = recorded.iter().position(|input| matches!(input, ClientInput::Bind { scope: new, .. } if new == &newer)).unwrap();
            assert!(unbind < bind);
        }

        admin("native_revoke", json!({"device_id":access.device_id,"workspace":newer.workspace_id})).await;
        let revoked = notice(&notices, |next| next.workspace == WorkspaceStatus::AccessRevoked);
        assert_eq!(revoked.endpoint, EndpointStatus::Authenticated);
        assert!(session.connection_context().unwrap().is_none());
        assert!(matches!(session.plan(send_intent(&newer, "revoked-control")).unwrap(), ActionPlan::Unavailable { .. }));
        admin("native_grant", json!({"device_id":access.device_id,"selection":{"workspace":host.workspace,"control":true,"terminal":false}})).await;
        let restored = select(&session, &notices);
        assert!(restored.attempt > newer.attempt);
        assert_eq!(restored.generation, scope.generation, "membership identity stays independent of grant counter");
        let ActionPlan::Dispatch { operation_id } = session.plan(send_intent(&restored, "regranted-control")).unwrap() else { panic!("regrant dispatch"); };
        notice(&notices, |next| next.outcomes.iter().any(|outcome| outcome.operation_id == operation_id && outcome.kind == OperationOutcomeKind::Success));
        session.close().unwrap(); client.close(); host.close().await;
    });
}

#[test]
fn real_gateway_enrollment_cancel_is_known_before_write_and_unknown_after_accepted_proof() {
    let name =
        "real_gateway_enrollment_cancel_is_known_before_write_and_unknown_after_accepted_proof";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let host = Host::start().await;
        let blocked = Arc::new(TestSigner::new(32));
        blocked.block_unlock.store(true, Ordering::Release);
        let client = host.client(blocked.clone());
        let (progress, mut updates) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .enroll("Canceled before unlock".into(), Arc::new(Setup(progress)))
                    .await
                    .unwrap()
            }
        });
        let connecting = tokio::time::timeout(WAIT, updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(client.cancel_enrollment(connecting.context.attempt));
        let first = task.await.unwrap();
        assert_eq!(first.outcome, EnrollmentOutcome::Canceled);
        assert!(blocked.unlock_dropped.load(Ordering::Acquire));
        assert_eq!(blocked.enroll_calls.load(Ordering::Acquire), 0);
        blocked.block_unlock.store(false, Ordering::Release);
        admin("native_enroll", json!({})).await;
        let other = host.client(blocked.clone());
        let (progress, mut updates) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn({
            let other = other.clone();
            async move {
                other
                    .enroll("Canceled pending proof".into(), Arc::new(Setup(progress)))
                    .await
                    .unwrap()
            }
        });
        let accepted = accepted(&mut updates).await;
        assert_ne!(accepted.context.attempt, connecting.context.attempt);
        assert!(!other.cancel_enrollment(connecting.context.attempt));
        assert!(other.cancel_enrollment(accepted.context.attempt));
        let result = task.await.unwrap();
        assert_eq!(result.outcome, EnrollmentOutcome::OutcomeUnknown);
        assert_eq!(blocked.enroll_calls.load(Ordering::Acquire), 1);
        assert!(admin("native_devices", json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty());
        client.close();
        other.close();
        host.close().await;
    });
}

#[test]
fn real_gateway_wrong_device_and_expired_device_cannot_reconnect_or_gain_workspace_access() {
    let name =
        "real_gateway_wrong_device_and_expired_device_cannot_reconnect_or_gain_workspace_access";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let host = Host::start().await;
        let unknown_signer = Arc::new(TestSigner::new(33));
        let unknown = host.client(unknown_signer.clone());
        let (unknown_session, notices, _) = session(&unknown).await;
        let failure = notice(&notices, |next| next.endpoint == EndpointStatus::SignedOut);
        assert_eq!(failure.reason.unwrap().code, "authentication");
        assert_eq!(unknown_signer.auth_calls.load(Ordering::Acquire), 1);
        assert!(unknown_session.workspaces().unwrap().is_none());
        unknown_session.close().unwrap();
        unknown.close();

        let signer = Arc::new(TestSigner::new(34));
        let client = host.client(signer.clone());
        let _access = enroll(&host, &client, &signer, false, 3).await;
        let (active, notices, _) = session(&client).await;
        let scope = select(&active, &notices);
        assert!(active.select_workspace("0101010101010101".into()).is_err());
        assert_eq!(active.connection_context().unwrap().unwrap().scope, scope);
        notice(&notices, |next| next.endpoint == EndpointStatus::SignedOut);
        active.reconnect().unwrap();
        let failure = notice(&notices, |next| {
            next.endpoint == EndpointStatus::SignedOut
                && next
                    .reason
                    .as_ref()
                    .is_some_and(|reason| reason.code == "authentication")
        });
        assert!(failure.scope.is_some());
        assert!(active.connection_context().unwrap().is_none());
        active.close().unwrap();
        client.close();
        host.close().await;
    });
}

#[test]
fn real_gateway_existing_key_recovers_without_reenrollment_and_device_revoke_is_individual() {
    let name =
        "real_gateway_existing_key_recovers_without_reenrollment_and_device_revoke_is_individual";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let host = Host::start().await;
        let first_signer = Arc::new(TestSigner::new(35)); let first = host.client(first_signer.clone());
        let first_access = enroll(&host, &first, &first_signer, false, 3600).await;
        // A new client with the same OS key authenticates its committed record.
        let recovery = host.client(first_signer.clone());
        let (one, one_notices, _) = session(&recovery).await; select(&one, &one_notices);
        admin("native_enroll", json!({})).await;
        let (progress, mut updates) = tokio::sync::mpsc::channel(8);
        let repeated = recovery.enroll("Existing key cannot enroll again".into(), Arc::new(Setup(progress))).await.unwrap();
        assert!(matches!(repeated.outcome, EnrollmentOutcome::Failed { reason } if reason.code == "authentication"));
        assert!(updates.try_recv().is_ok()); assert!(updates.try_recv().is_err());
        assert_eq!(admin("native_devices", json!({})).await.as_array().unwrap().len(), 1);

        let second_signer = Arc::new(TestSigner::new(36)); let second = host.client(second_signer.clone());
        let _second_access = enroll(&host, &second, &second_signer, false, 3600).await;
        let (two, two_notices, _) = session(&second).await; let second_scope = select(&two, &two_notices);
        admin("native_revoke", json!({"device_id":first_access.device_id})).await;
        notice(&one_notices, |next| next.endpoint == EndpointStatus::SignedOut);
        assert_eq!(two.connection_context().unwrap().unwrap().scope, second_scope);
        assert_eq!(two.query(ViewQuery::Overview).unwrap().freshness, Freshness::Fresh);
        assert!(matches!(one.plan(send_intent(&second_scope, "wrong-device-scope")).unwrap(), ActionPlan::Unavailable { .. }));
        one.close().unwrap(); two.close().unwrap(); first.close(); recovery.close(); second.close(); host.close().await;
    });
}

#[test]
fn real_gateway_rejects_cached_enrollment_proof_for_a_fresh_nonce_without_replaying_or_creating_device(
) {
    let name = "real_gateway_rejects_cached_enrollment_proof_for_a_fresh_nonce_without_replaying_or_creating_device";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let host = Host::start().await;
        admin("native_enroll", json!({})).await;
        let signer = Arc::new(TestSigner::new(37)); let client = host.client(signer.clone());
        let (progress, mut updates) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn({ let client = client.clone(); async move { client.enroll("First nonce".into(), Arc::new(Setup(progress))).await.unwrap() } });
        let first = accepted(&mut updates).await;
        assert!(client.cancel_enrollment(first.context.attempt));
        assert_eq!(task.await.unwrap().outcome, EnrollmentOutcome::OutcomeUnknown);
        signer.replay_enrollment.store(true, Ordering::Release);
        let (progress, mut updates) = tokio::sync::mpsc::channel(8);
        let result = client.enroll("Fresh nonce, old signature".into(), Arc::new(Setup(progress))).await.unwrap();
        assert!(matches!(result.outcome, EnrollmentOutcome::Failed { reason } if reason.code == "invalid_proof"));
        assert_ne!(result.context.attempt, first.context.attempt);
        assert_eq!(signer.enroll_calls.load(Ordering::Acquire), 2);
        assert!(matches!(updates.try_recv().unwrap().progress, EnrollmentProgress::Connecting));
        assert!(updates.try_recv().is_err());
        assert!(admin("native_devices", json!({})).await.as_array().unwrap().is_empty());
        client.close(); host.close().await;
    });
}

#[test]
fn real_gateway_restart_fences_old_epoch_then_reconnects_with_fresh_scope_and_same_device_key() {
    let name = "real_gateway_restart_fences_old_epoch_then_reconnects_with_fresh_scope_and_same_device_key";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let mut host = Host::start().await;
        let signer = Arc::new(TestSigner::new(38));
        let client = host.client(signer.clone());
        let access = enroll(&host, &client, &signer, false, 3600).await;
        let core = crew_client::ClientCore::new(ClientConfig {
            limits: NATIVE_LIMITS,
            snapshot_mode: SnapshotMode::OrderedConnection,
        })
        .unwrap();
        let active = client
            .session(core, Protocol6Codec, DriverConfig::default())
            .await
            .unwrap();
        let (sender, notices) = sync::channel();
        active.observe(Arc::new(Observer(sender))).unwrap();
        active
            .connect("isolated".into(), client.profile().endpoint_id(), None)
            .unwrap();
        let old = select(&active, &notices);
        let old_grant = active.connection_context().unwrap().unwrap().grant;
        host.restart().await;
        notice(&notices, |notice| {
            notice.endpoint == EndpointStatus::Disconnected
        });
        active.reconnect().unwrap();
        let newer = notice(&notices, |notice| {
            notice.workspace == WorkspaceStatus::Live
                && notice
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.attempt > old.attempt)
        })
        .scope
        .unwrap();
        assert!(newer.attempt > old.attempt);
        assert_eq!(newer.daemon_pid, old.daemon_pid);
        assert_eq!(newer.daemon_started_at_ms, old.daemon_started_at_ms);
        assert_ne!(
            active.connection_context().unwrap().unwrap().grant.grant_id,
            old_grant.grant_id
        );
        let devices = admin("native_devices", json!({})).await;
        assert_eq!(devices.as_array().unwrap().len(), 1);
        assert_eq!(devices[0]["id"], access.device_id);
        assert_eq!(signer.enroll_calls.load(Ordering::Acquire), 1);
        active.close().unwrap();
        client.close();
        host.close().await;
    });
}

/// A second isolated daemon whose workspace can be granted to the same device.
struct ExtraDaemon {
    _workspace: tempfile::TempDir,
    workspace: PathBuf,
    socket: PathBuf,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ExtraDaemon {
    async fn start() -> Self {
        let workspace = tempfile::Builder::new()
            .prefix("crew-gw-b")
            .tempdir_in("/tmp")
            .unwrap();
        let canonical = workspace.path().canonicalize().unwrap();
        let daemon = crew::daemon::start(crew::daemon::DaemonOptions {
            workspace: canonical.clone(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let socket = daemon.socket().to_path_buf();
        let task = tokio::spawn(async move {
            daemon.serve().await.unwrap();
        });
        Self {
            _workspace: workspace,
            workspace: canonical,
            socket,
            task: Some(task),
        }
    }

    fn id(&self) -> String {
        crew_core::paths::session_id(&self.workspace.join(".crew"))
    }

    async fn close(mut self) {
        let socket = self.socket.clone();
        tokio::task::spawn_blocking(move || {
            let (mut caller, _) =
                crew_core::client::Client::operator(&socket, "gateway-test").unwrap();
            let _: Value = caller
                .call(crew_core::rpc::Call::Shutdown { stop_agents: true })
                .unwrap();
        })
        .await
        .unwrap();
        tokio::time::timeout(WAIT, self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}

impl Drop for ExtraDaemon {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Two granted workspaces on one device: the host's and a second daemon's.
async fn two_workspace_session(
    host: &Host,
    beta: &ExtraDaemon,
    seed: u8,
    hint: Option<String>,
) -> (
    GatewayClient,
    GatewayDeviceAccess,
    NativeSession,
    sync::Receiver<ClientNotice>,
) {
    let signer = Arc::new(TestSigner::new(seed));
    let client = host.client(signer.clone());
    let access = enroll(host, &client, &signer, false, 3600).await;
    admin(
        "native_grant",
        json!({"device_id":access.device_id,
        "selection":{"workspace":beta.workspace,"control":false,"terminal":false}}),
    )
    .await;
    let core = crew_client::ClientCore::new(ClientConfig {
        limits: NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap();
    let session = client
        .session(core, Protocol6Codec, DriverConfig::default())
        .await
        .unwrap();
    let (sender, notices) = sync::channel();
    session.observe(Arc::new(Observer(sender))).unwrap();
    session
        .connect("isolated".into(), client.profile().endpoint_id(), hint)
        .unwrap();
    (client, access, session, notices)
}

/// Wait until the authenticated listing is exactly these running workspaces.
fn listed(session: &NativeSession, receiver: &sync::Receiver<ClientNotice>, ids: &[String]) {
    let deadline = Instant::now() + WAIT;
    let mut last = None;
    loop {
        if let Some(listing) = session.workspaces().unwrap() {
            let running: Vec<_> = listing
                .joined
                .iter()
                .filter(|entry| entry.state == crew_protocol::WorkspaceState::Running)
                .map(|entry| entry.id.clone())
                .collect();
            if running.len() == listing.joined.len()
                && running.len() == ids.len()
                && ids.iter().all(|id| running.contains(id))
            {
                return;
            }
        }
        last = Some(
            receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| {
                    panic!(
                        "listing {ids:?}; have {:?}; last {last:?}",
                        session.workspaces().unwrap()
                    )
                }),
        );
    }
}

fn live_after(receiver: &sync::Receiver<ClientNotice>, attempt: u64) -> ClientScope {
    notice(receiver, |next| {
        next.workspace == WorkspaceStatus::Live
            && next
                .scope
                .as_ref()
                .is_some_and(|scope| scope.attempt > attempt)
    })
    .scope
    .unwrap()
}

#[test]
fn real_gateway_restart_then_workspace_revoke_keeps_the_device_and_its_other_workspace() {
    let name =
        "real_gateway_restart_then_workspace_revoke_keeps_the_device_and_its_other_workspace";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let mut host = Host::start().await;
        let beta = ExtraDaemon::start().await;
        let alpha_id = crew_core::paths::session_id(&host.workspace.join(".crew"));
        let (client, access, session, notices) =
            two_workspace_session(&host, &beta, 39, None).await;
        listed(&session, &notices, &[alpha_id.clone(), beta.id()]);
        session.select_workspace(alpha_id.clone()).unwrap();
        let alpha = live_after(&notices, 0);
        admin(
            "native_grant",
            json!({"device_id":access.device_id,
            "selection":{"workspace":host.workspace,"control":true,"terminal":true}}),
        )
        .await;
        notice(&notices, |next| {
            next.workspace == WorkspaceStatus::AccessRevoked
        });
        session.select_workspace(alpha_id.clone()).unwrap();
        let controlled = live_after(&notices, alpha.attempt);

        // A normal service restart, then the reconnect the app offers.
        host.restart().await;
        notice(&notices, |next| {
            next.endpoint == EndpointStatus::Disconnected
        });
        session.reconnect().unwrap();
        let restarted = live_after(&notices, controlled.attempt);
        assert_eq!(restarted.workspace_id, alpha_id);
        listed(&session, &notices, &[alpha_id.clone(), beta.id()]);

        // Revoking only alpha ends its route; the device stays signed in and
        // the restarted index lists the remaining workspace on its own.
        admin(
            "native_revoke",
            json!({"device_id":access.device_id,"workspace":alpha_id}),
        )
        .await;
        let ended = notice(&notices, |next| {
            next.workspace == WorkspaceStatus::AccessRevoked
        });
        assert_eq!(ended.endpoint, EndpointStatus::Authenticated);
        assert!(session.connection_context().unwrap().is_none());
        listed(&session, &notices, &[beta.id()]);

        // An explicit reconnect retries the pinned workspace. The gateway
        // refuses only that route; the device is not signed out.
        session.reconnect().unwrap();
        let refused = notice(&notices, |next| {
            next.workspace == WorkspaceStatus::AccessRevoked
                && next
                    .reason
                    .as_ref()
                    .is_some_and(|reason| reason.code == "authentication")
        });
        assert_ne!(refused.endpoint, EndpointStatus::SignedOut);
        notice(&notices, |next| {
            next.endpoint == EndpointStatus::Authenticated
        });
        listed(&session, &notices, &[beta.id()]);
        session.select_workspace(beta.id()).unwrap();
        let remaining = live_after(&notices, restarted.attempt);
        assert_eq!(remaining.workspace_id, beta.id());
        assert_eq!(
            session.connection_context().unwrap().unwrap().scope,
            remaining
        );
        session.close().unwrap();
        client.close();
        beta.close().await;
        host.close().await;
    });
}
