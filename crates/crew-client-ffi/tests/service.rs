#![cfg(feature = "bindings")]

//! These tests exercise the actual Rust session and foreign facade lifetime.
//! The probe implements only ClientApi observation; it never opens a daemon or
//! claims that a native application journey passed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crew_client::api::*;
use crew_client_ffi::callbacks::ClientObserver;
use crew_client_ffi::service::{CrewClient, ProfileSelection};
use crew_client_transport::queue::EventQueue;
use crew_client_transport::service::{ChannelHandle, Connector};
use crew_client_transport::{DriverConfig, NativeSession, Protocol6Codec};
use crew_client_transport::{HostEffect, TransportEnd};

const WAIT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Calls {
    queries: Vec<ViewQuery>,
    plans: Vec<ActionIntent>,
    confirmations: Vec<u64>,
    cancellations: Vec<u64>,
}

struct ProbeCore {
    calls: Arc<Mutex<Calls>>,
    dropped: Arc<AtomicBool>,
}

fn update() -> ClientUpdate {
    ClientUpdate {
        revision: 0,
        changes: ChangeSet::default(),
        effects: Vec::new(),
        outcomes: Vec::new(),
    }
}

fn probe_error() -> ClientError {
    ClientError::InvalidInput {
        field: "probe".into(),
        reason: "probe has no state projection".into(),
    }
}

impl ClientApi for ProbeCore {
    fn apply(&mut self, _: ClientInput, _: ClockSample) -> Result<ClientUpdate, ClientError> {
        Ok(update())
    }

    fn view(&self, query: ViewQuery, _: ClockSample) -> Result<ViewData, ClientError> {
        self.calls.lock().unwrap().queries.push(query);
        Err(probe_error())
    }

    fn plan(&mut self, intent: ActionIntent, _: ClockSample) -> Result<PlanUpdate, ClientError> {
        self.calls.lock().unwrap().plans.push(intent);
        Err(probe_error())
    }

    fn confirm(&mut self, id: u64, _: ClockSample) -> Result<ClientUpdate, ClientError> {
        self.calls.lock().unwrap().confirmations.push(id);
        Err(ClientError::StaleConfirmation)
    }

    fn cancel_confirmation(&mut self, id: u64) -> Result<ClientUpdate, ClientError> {
        self.calls.lock().unwrap().cancellations.push(id);
        Ok(update())
    }
}

impl Drop for ProbeCore {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

struct NoConnections;

impl Connector for NoConnections {
    fn open(&self, _: &HostEffect, _: &EventQueue) -> Result<Box<dyn ChannelHandle>, TransportEnd> {
        Err(TransportEnd::Permission)
    }
}

fn client() -> (Arc<CrewClient>, Arc<Mutex<Calls>>, Arc<AtomicBool>) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let dropped = Arc::new(AtomicBool::new(false));
    let session = NativeSession::with_connector(
        ProbeCore {
            calls: calls.clone(),
            dropped: dropped.clone(),
        },
        Protocol6Codec,
        NoConnections,
        DriverConfig::default(),
    )
    .unwrap();
    (
        CrewClient::from_session(session, Vec::new()).unwrap(),
        calls,
        dropped,
    )
}

fn scope() -> ClientScope {
    ClientScope {
        endpoint_id: "test-endpoint".into(),
        workspace_id: "test-workspace".into(),
        generation: Some(u64::MAX),
        daemon_pid: Some(u32::MAX),
        daemon_started_at_ms: Some(i64::MIN),
        attempt: 91,
    }
}

fn query() -> ViewQuery {
    ViewQuery::Team
}

struct Observer {
    client: Weak<CrewClient>,
    notice: Sender<ClientNotice>,
    dropped: Sender<()>,
}

impl ClientObserver for Observer {
    fn changed(&self, notice: ClientNotice) {
        // Reentry into the actual service must complete while this callback is
        // running. A facade/service lock held across delivery would deadlock.
        let client = self.client.upgrade().unwrap();
        assert_eq!(client.query(query()).unwrap_err(), probe_error());
        let _ = self.notice.send(notice);
    }

    fn terminal(&self, _: TerminalChunk) {}
}

impl Drop for Observer {
    fn drop(&mut self) {
        // Swift deinitializers may also call back into the wrapper. Disposal
        // must release both the wrapper and service callback mutexes first.
        if let Some(client) = self.client.upgrade() {
            let _ = client.connection_context();
        }
        let _ = self.dropped.send(());
    }
}

fn observe(client: &Arc<CrewClient>) -> (u64, Receiver<ClientNotice>, Receiver<()>) {
    let (notice, notices) = mpsc::channel();
    let (dropped, drops) = mpsc::channel();
    let id = client
        .observe(Box::new(Observer {
            client: Arc::downgrade(client),
            notice,
            dropped,
        }))
        .unwrap();
    notices.recv_timeout(WAIT).unwrap();
    (id, notices, drops)
}

#[test]
fn typed_calls_delegate_and_closed_objects_report_recoverable_errors() {
    let (client, calls, dropped) = client();
    assert_eq!(client.workspaces().unwrap(), None);
    assert_eq!(client.connection_context().unwrap(), None);
    assert_eq!(client.query(query()).unwrap_err(), probe_error());
    let intent = ActionIntent::Detach {
        scope: scope(),
        attachment_id: 101,
    };
    assert_eq!(client.plan(intent.clone()).unwrap_err(), probe_error());
    assert_eq!(client.confirm(203), Err(ClientError::StaleConfirmation));
    client.cancel_confirmation(204).unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.queries, vec![query()]);
    assert_eq!(calls.plans, vec![intent]);
    assert_eq!(calls.confirmations, vec![203]);
    assert_eq!(calls.cancellations, vec![204]);
    drop(calls);

    // Connect IDs resolve only through configured connector selections.
    assert_eq!(
        client.connect("invented-host".into()),
        Err(ClientError::InvalidInput {
            field: "profile_id".into(),
            reason: "unknown configured profile".into(),
        })
    );
    client.close().unwrap();
    assert!(dropped.load(Ordering::Acquire));
    assert_eq!(
        client.query(query()).unwrap_err(),
        ClientError::ClosedClient
    );
    assert_eq!(client.confirm(203), Err(ClientError::ClosedClient));
    client.close().unwrap();
    client.remove_observer(900).unwrap();
}

#[test]
fn observer_replacement_and_stale_removal_preserve_current_subscription() {
    let (client, _, dropped) = client();
    let (first, old_notices, first_drop) = observe(&client);
    let (second, notices, second_drop) = observe(&client);
    assert!(second > first);
    first_drop.recv_timeout(WAIT).unwrap();
    client.remove_observer(first).unwrap();
    client.set_active(false).unwrap();
    notices.recv_timeout(WAIT).unwrap();
    assert!(old_notices.try_recv().is_err());
    client.remove_observer(second).unwrap();
    second_drop.recv_timeout(WAIT).unwrap();
    client.set_active(true).unwrap();
    assert!(notices.recv_timeout(WAIT).is_err());
    client.close().unwrap();
    assert!(dropped.load(Ordering::Acquire));
}

struct ClosingObserver {
    client: Weak<CrewClient>,
    closed: Sender<()>,
}

impl ClientObserver for ClosingObserver {
    fn changed(&self, _: ClientNotice) {
        self.client.upgrade().unwrap().close().unwrap();
        let _ = self.closed.send(());
    }
    fn terminal(&self, _: TerminalChunk) {}
}

#[test]
fn callback_can_dispose_without_self_join_or_reference_cycle() {
    let (client, _, dropped) = client();
    let weak = Arc::downgrade(&client);
    let (closed, closed_rx) = mpsc::channel();
    client
        .observe(Box::new(ClosingObserver {
            client: weak.clone(),
            closed,
        }))
        .unwrap();
    closed_rx.recv_timeout(WAIT).unwrap();
    assert!(dropped.load(Ordering::Acquire));
    drop(client);
    assert!(weak.upgrade().is_none());
}

#[test]
fn dropping_last_facade_reference_releases_real_session() {
    let (client, _, dropped) = client();
    let (_, _, observer_dropped) = observe(&client);
    drop(client);
    observer_dropped.recv_timeout(WAIT).unwrap();
    assert!(dropped.load(Ordering::Acquire));
}

struct BlockingObserver {
    entered: Sender<()>,
    resume: Mutex<Receiver<()>>,
    delivered: Arc<std::sync::atomic::AtomicUsize>,
}

impl ClientObserver for BlockingObserver {
    fn changed(&self, _: ClientNotice) {
        self.delivered.fetch_add(1, Ordering::AcqRel);
        self.entered.send(()).unwrap();
        self.resume.lock().unwrap().recv_timeout(WAIT).unwrap();
    }
    fn terminal(&self, _: TerminalChunk) {}
}

#[test]
fn removing_blocked_observer_discards_queued_deliveries_without_waiting_for_callback() {
    let (client, _, _) = client();
    let (entered, entered_rx) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first = client
        .observe(Box::new(BlockingObserver {
            entered,
            resume: Mutex::new(resume_rx),
            delivered: delivered.clone(),
        }))
        .unwrap();
    entered_rx.recv_timeout(WAIT).unwrap();
    // These notices queue behind the in-flight callback. Removal must prevent
    // their delivery and must return before that callback can finish.
    client.set_active(false).unwrap();
    client.set_active(true).unwrap();
    client.remove_observer(first).unwrap();
    let (notice, notices) = mpsc::channel();
    let (dropped, _) = mpsc::channel();
    let second = client
        .observe(Box::new(Observer {
            client: Arc::downgrade(&client),
            notice,
            dropped,
        }))
        .unwrap();
    client.remove_observer(first).unwrap();
    resume.send(()).unwrap();
    notices.recv_timeout(WAIT).unwrap();
    assert_eq!(delivered.load(Ordering::Acquire), 1);
    client.remove_observer(second).unwrap();
    client.close().unwrap();
}

#[test]
#[cfg(not(all(feature = "local", any(target_os = "linux", target_os = "macos"))))]
fn unavailable_local_factory_has_the_same_typed_abi() {
    assert!(matches!(
        CrewClient::local(),
        Err(ClientError::InvalidInput { field, reason })
            if field == "local" && reason == "local_unavailable"
    ));
}

#[test]
#[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
fn real_local_factory_uses_protected_discovery_and_stays_disconnected() {
    const PROBE: &str = "CREW_FFI_FACTORY_PROBE";
    if let Ok(mode) = std::env::var(PROBE) {
        if mode == "agent" {
            assert!(matches!(
                CrewClient::local(),
                Err(ClientError::InvalidInput { field, reason })
                    if field == "local" && reason == "agent_context"
            ));
            return;
        }
        assert_eq!(mode, "operator");
        let client = CrewClient::local().unwrap();
        assert!(client.local_profiles().unwrap().is_empty());
        assert_eq!(client.workspaces().unwrap(), None);
        assert_eq!(client.connection_context().unwrap(), None);
        let view = client.query(ViewQuery::Team).unwrap();
        assert_eq!(view.scope, None);
        let crew_client_ffi::records::ViewPayload::Team(team) = view.payload else {
            panic!("real core must return typed team projection")
        };
        assert!(team.agents.is_empty());
        assert!(matches!(
            client.connect("unknown-profile".into()),
            Err(ClientError::InvalidInput { field, .. }) if field == "profile_id"
        ));
        client.close().unwrap();
        assert_eq!(
            client.connect("unknown-profile".into()),
            Err(ClientError::ClosedClient)
        );
        return;
    }

    // Run under a private empty registry. Never alter process-global env in a
    // parallel test or discover the operator's real workspaces.
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/native-validation");
    std::fs::create_dir_all(&root).unwrap();
    let scratch = tempfile::Builder::new()
        .prefix("ffi-local-factory-")
        .tempdir_in(root)
        .unwrap();
    for mode in ["operator", "agent"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        // The archive/fixture environment must never address another repo.
        for (name, _) in std::env::vars_os() {
            let name_text = name.to_string_lossy();
            if name_text.starts_with("GIT_")
                || name_text.starts_with("CREW_")
                || name_text.starts_with("HERDR_")
            {
                child.env_remove(name);
            }
        }
        child
            .args([
                "--exact",
                "real_local_factory_uses_protected_discovery_and_stays_disconnected",
                "--nocapture",
            ])
            .env(PROBE, mode)
            .env("XDG_STATE_HOME", scratch.path().join("state"));
        if mode == "agent" {
            child.env("CREW_IDENTITY", scratch.path().join("absent-identity"));
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "factory {mode}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn shared_core_fixture_projection_crosses_the_actual_session_facade() {
    let snapshot: crew_protocol::rpc::Snapshot =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/team10.json")).unwrap();
    let clock = ClockSample {
        wall_ms: snapshot.server_time,
        monotonic_ms: 0,
    };
    let captured = ClientScope {
        endpoint_id: "fixture-endpoint".into(),
        workspace_id: snapshot.session.workspace.clone(),
        generation: None,
        daemon_pid: Some(snapshot.session.daemon_pid),
        daemon_started_at_ms: Some(snapshot.session.daemon_started_at),
        attempt: 1,
    };
    let expected: std::collections::BTreeSet<_> = snapshot
        .agents
        .iter()
        .map(|agent| agent.id.clone())
        .collect();
    let mut core = crew_client::ClientCore::new(ClientConfig {
        limits: crew_client::policy::NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap();
    // Fixture data enters through the real reducer. This is not an
    // authenticated-channel test; N3's live composition gate supplies that.
    for input in [
        ClientInput::Bind {
            scope: captured.clone(),
            mode: ConnectionMode::LocalOperator,
            grant: Grant {
                grant_id: "fixture-local".into(),
                tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
                workspace_ids: vec![captured.workspace_id.clone()],
                expires_at_ms: None,
            },
            snapshot_mode: SnapshotMode::OrderedConnection,
        },
        ClientInput::SnapshotStarted {
            scope: captured.clone(),
            operation_id: 1 << 63,
        },
        ClientInput::Snapshot {
            scope: captured.clone(),
            operation_id: 1 << 63,
            snapshot: Box::new(snapshot),
        },
    ] {
        core.apply(input, clock).unwrap();
    }
    let session =
        NativeSession::with_connector(core, Protocol6Codec, NoConnections, DriverConfig::default())
            .unwrap();
    let client = CrewClient::from_session(session, Vec::new()).unwrap();
    let view = client.query(ViewQuery::Team).unwrap();
    assert_eq!(view.scope, Some(captured));
    let crew_client_ffi::records::ViewPayload::Team(team) = view.payload else {
        panic!("shared core must produce typed team data")
    };
    let actual: std::collections::BTreeSet<_> = team
        .agents
        .into_iter()
        .map(|agent| agent.agent.id)
        .collect();
    assert!(!actual.is_empty());
    assert_eq!(actual, expected);
    client.close().unwrap();
}

struct CountConnections(Arc<std::sync::atomic::AtomicUsize>);

impl Connector for CountConnections {
    fn open(&self, _: &HostEffect, _: &EventQueue) -> Result<Box<dyn ChannelHandle>, TransportEnd> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Err(TransportEnd::Permission)
    }
}

#[test]
fn unknown_and_retired_profile_ids_never_dial() {
    let dials = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let core = crew_client::ClientCore::new(ClientConfig {
        limits: crew_client::policy::NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap();
    let session = NativeSession::with_connector(
        core,
        Protocol6Codec,
        CountConnections(dials.clone()),
        DriverConfig::default(),
    )
    .unwrap();
    let client = CrewClient::from_session(
        session,
        vec![ProfileSelection {
            profile_id: "configured-profile".into(),
            endpoint_id: "configured-endpoint".into(),
            workspace_hint: None,
        }],
    )
    .unwrap();
    assert!(matches!(
        client.connect("unknown-profile".into()),
        Err(ClientError::InvalidInput { field, .. }) if field == "profile_id"
    ));
    client.close().unwrap();
    assert_eq!(
        client.connect("configured-profile".into()),
        Err(ClientError::ClosedClient)
    );
    assert_eq!(dials.load(Ordering::Acquire), 0);
}

fn availability(intent: ActionIntent) -> ViewQuery {
    ViewQuery::Availability {
        intent: Box::new(intent),
    }
}

fn gateway_core(captured: &ClientScope, clock: ClockSample) -> crew_client::ClientCore {
    let snapshot: crew_protocol::rpc::Snapshot =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/snapshot.json")).unwrap();
    let mut core = crew_client::ClientCore::new(ClientConfig {
        limits: crew_client::policy::NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap();
    for input in [
        ClientInput::Bind {
            scope: captured.clone(),
            mode: ConnectionMode::ScopedGateway,
            grant: Grant {
                grant_id: "device:grant".into(),
                tiers: vec![GrantTier::Read, GrantTier::Control],
                workspace_ids: vec![captured.workspace_id.clone()],
                expires_at_ms: None,
            },
            snapshot_mode: SnapshotMode::OrderedConnection,
        },
        ClientInput::SnapshotStarted {
            scope: captured.clone(),
            operation_id: 1 << 63,
        },
        ClientInput::Snapshot {
            scope: captured.clone(),
            operation_id: 1 << 63,
            snapshot: Box::new(snapshot),
        },
    ] {
        core.apply(input, clock).unwrap();
    }
    core
}

/// Availability travels the real facade, service worker and Core query. The
/// native value equals the Core's own answer for the same captured intent,
/// and repeated queries reserve no operation or confirmation identity.
#[test]
fn availability_crosses_the_actual_facade_with_core_reasons_and_no_planning() {
    use crew_client_ffi::records::{ViewData as NativeView, ViewPayload as NativePayload};
    let captured = ClientScope {
        endpoint_id: "gateway:test".into(),
        workspace_id: "/work/demo".into(),
        generation: Some(1),
        daemon_pid: Some(4000),
        daemon_started_at_ms: Some(1),
        attempt: 1,
    };
    let clock = ClockSample {
        wall_ms: 1_760_000_000_000,
        monotonic_ms: 0,
    };
    let mut twin = gateway_core(&captured, clock);
    let session = NativeSession::with_connector(
        gateway_core(&captured, clock),
        Protocol6Codec,
        NoConnections,
        DriverConfig::default(),
    )
    .unwrap();
    let client = CrewClient::from_session(session, Vec::new()).unwrap();
    let revision = client.query(ViewQuery::Team).unwrap().revision;

    let mut stale = captured.clone();
    stale.attempt = 0;
    let kill = ActionIntent::Agent {
        scope: captured.clone(),
        id: "claude-1".into(),
        action: AgentAction::Kill,
    };
    let mut codes = std::collections::BTreeSet::new();
    for intent in [
        ActionIntent::SendMessage {
            scope: captured.clone(),
            to: "claude-1".into(),
            body: "測定 🦀".into(),
            message_id: "m-stable".into(),
        },
        kill.clone(),
        ActionIntent::StopTeam {
            scope: stale.clone(),
        },
        ActionIntent::Answer {
            scope: captured.clone(),
            question_id: "missing".into(),
            body: "yes".into(),
        },
    ] {
        let expected: NativeView = twin
            .view(availability(intent.clone()), clock)
            .unwrap()
            .into();
        for _ in 0..3 {
            assert_eq!(
                client.query(availability(intent.clone())).unwrap(),
                expected
            );
        }
        assert_eq!(
            expected.scope.as_ref(),
            Some(crew_client::actions::action_scope(&intent))
        );
        let NativePayload::Availability(value) = expected.payload else {
            panic!("typed availability payload expected")
        };
        assert_eq!(value.available, value.reason.is_none());
        codes.insert(value.reason.map(|reason| reason.code));
    }
    assert!(codes.contains(&None));
    assert!(codes.contains(&Some("stale_scope".into())));
    assert!(
        codes.len() >= 3,
        "available and distinct Core reasons: {codes:?}"
    );

    // A malformed intent fails exactly as planning does; it is not a reason.
    let malformed = ActionIntent::SendMessage {
        scope: captured.clone(),
        to: "claude-1".into(),
        body: String::new(),
        message_id: "m-empty".into(),
    };
    assert_eq!(
        client.query(availability(malformed.clone())).unwrap_err(),
        twin.plan(malformed, clock).unwrap_err()
    );

    // Queries changed no revision and reserved no confirmation identity.
    assert_eq!(client.query(ViewQuery::Team).unwrap().revision, revision);
    let planned = client.plan(kill.clone()).unwrap();
    assert!(matches!(planned, ActionPlan::Confirm { .. }));
    assert_eq!(planned, twin.plan(kill, clock).unwrap().plan);
    client.close().unwrap();
}
