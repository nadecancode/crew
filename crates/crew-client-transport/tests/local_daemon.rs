#![cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command as Process;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use common::ProbeCore;
use crew_client::api::*;
use crew_client_transport::local::{discover_profiles, LocalProfile, LocalTransport};
use crew_client_transport::queue::EventQueue;
use crew_client_transport::{ChannelPurpose, HostEffect, HostEvent, TransportEnd};
use crew_client_transport::{ClientObserver, DriverConfig, NativeSession, Protocol6Codec};
use crew_core::rpc::{Call, SendParams};

const WAIT: Duration = Duration::from_secs(5);

/// Re-exec every environment-sensitive test; never mutate libtest's shared
/// process environment and never read the operator's active registry/token.
fn isolated(name: &str) -> bool {
    if std::env::var_os("CREW_NATIVE_TRANSPORT_TEST_CHILD").is_some() {
        return false;
    }
    let root = tempfile::Builder::new()
        .prefix("crew-nt")
        .tempdir_in("/tmp")
        .unwrap();
    let output = Process::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("CREW_NATIVE_TRANSPORT_TEST_CHILD", "1")
        .env_remove("CREW_IDENTITY")
        .env_remove("CREW_SOCKET")
        .env_remove("CREW_WEB")
        .env_remove("CREW_LAUNCH")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_NAMESPACE")
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

async fn daemon(workspace: PathBuf) -> (PathBuf, tokio::task::JoinHandle<Result<(), String>>) {
    let running = crew::daemon::start(crew::daemon::DaemonOptions {
        workspace,
        crew_bin: std::env::current_exe().unwrap(),
        pin: None,
    })
    .await
    .unwrap();
    let socket = running.socket().to_path_buf();
    let server =
        tokio::spawn(async move { running.serve().await.map_err(|error| error.to_string()) });
    (socket, server)
}

fn profile(socket: PathBuf) -> LocalProfile {
    LocalProfile {
        id: "disposable".into(),
        endpoint_id: "disposable".into(),
        workspace_key: "socket-profile".into(),
        socket,
    }
}

struct Observer {
    notices: mpsc::Sender<ClientNotice>,
    session: Mutex<Weak<NativeSession>>,
    queries: Mutex<usize>,
}

impl ClientObserver for Observer {
    fn changed(&self, notice: ClientNotice) {
        let session = self.session.lock().unwrap().upgrade();
        if let Some(session) = session {
            if session.query(ViewQuery::Overview).is_ok() {
                *self.queries.lock().unwrap() += 1;
            }
        }
        let _ = self.notices.send(notice);
    }
    fn terminal(&self, _: TerminalChunk) {}
}

fn next(rx: &Receiver<ClientNotice>, predicate: impl Fn(&ClientNotice) -> bool) -> ClientNotice {
    let deadline = Instant::now() + WAIT;
    loop {
        let notice = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("native typed notice before deadline");
        if predicate(&notice) {
            return notice;
        }
    }
}

#[test]
fn real_daemon_native_service_authenticates_reads_pushes_and_allows_observer_query_reentry() {
    if isolated(
        "real_daemon_native_service_authenticates_reads_pushes_and_allows_observer_query_reentry",
    ) {
        return;
    }
    runtime().block_on(async {
        let workspace = tempfile::Builder::new().prefix("crew-w").tempdir_in("/tmp").unwrap();
        let canonical = workspace.path().canonicalize().unwrap();
        let (socket, server) = daemon(canonical.clone()).await;
        let discovered = discover_profiles().unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].profile.socket, socket);
        let (core, probe) = ProbeCore::new();
        let session = Arc::new(NativeSession::local(core, Protocol6Codec, vec![profile(socket.clone())], DriverConfig::default()).unwrap());
        let (tx, rx) = mpsc::channel();
        let observer = Arc::new(Observer { notices: tx, session: Mutex::new(Arc::downgrade(&session)), queries: Mutex::new(0) });
        session.observe(observer.clone()).unwrap();
        session.connect("disposable".into(), "disposable".into(), Some("socket-profile".into())).unwrap();
        let live = next(&rx, |notice| notice.workspace == WorkspaceStatus::Live);
        let scope = live.scope.unwrap();
        assert_eq!(scope.workspace_id, canonical.to_string_lossy());
        assert_eq!(scope.daemon_pid, Some(std::process::id()));
        assert!(scope.daemon_started_at_ms.is_some());
        let view = session.query(ViewQuery::Overview).unwrap();
        assert_eq!(view.scope, Some(scope.clone()));
        assert!(matches!(view.payload, ViewPayload::Overview(data) if data.session.as_ref().is_some_and(|session| session.workspace == canonical.to_string_lossy())));

        let (mut caller, _) = crew_core::client::Client::operator(&socket, "native-test").unwrap();
        let _: crew_core::rpc::SendResult = caller.call(Call::Send(SendParams { to: "*".into(), body: "isolated native message".into(), id: Some("native-real-message".into()), ..SendParams::default() })).unwrap();
        next(&rx, |_| probe.lock().unwrap().inputs.iter().any(|input| matches!(input, ClientInput::Push { push: crew_core::rpc::Push::Message(message), .. } if message.id == "native-real-message")));
        assert!(*observer.queries.lock().unwrap() > 0, "callback reentered query outside all locks");
        session.close().unwrap();
        // Disposing the native object closes only client sockets.
        let _: crew_core::rpc::Snapshot = caller.call(Call::Snapshot { messages: Some(0), events: Some(0) }).unwrap();
        let _: serde_json::Value = caller.call(Call::Shutdown { stop_agents: true }).unwrap();
        tokio::time::timeout(WAIT, server).await.unwrap().unwrap().unwrap();
    });
}

#[test]
fn real_daemon_one_subscribed_socket_snapshot_barrier_covers_interleaved_messages() {
    if isolated("real_daemon_one_subscribed_socket_snapshot_barrier_covers_interleaved_messages") {
        return;
    }
    runtime().block_on(async {
        let workspace = tempfile::Builder::new()
            .prefix("crew-w")
            .tempdir_in("/tmp")
            .unwrap();
        let (socket, server) = daemon(workspace.path().to_path_buf()).await;
        let queue = EventQueue::new(1).unwrap();
        let open = HostEffect::Open {
            channel_id: 1,
            profile_id: "disposable".into(),
            endpoint_id: "disposable".into(),
            attempt: 1,
            purpose: ChannelPurpose::Workspace,
            expected_scope: None,
        };
        let mut channel = LocalTransport::open(profile(socket.clone()), &open, &queue).unwrap();
        assert!(matches!(
            tokio::time::timeout(WAIT, queue.recv()).await.unwrap(),
            Some(HostEvent::WorkspaceOpened { .. })
        ));
        channel
            .send(1, Command::Subscribe.encode(1).unwrap())
            .unwrap();
        loop {
            if let Some(HostEvent::Frame { text, .. }) =
                tokio::time::timeout(WAIT, queue.recv()).await.unwrap()
            {
                let frame: crew_core::rpc::ServerFrame = serde_json::from_str(&text).unwrap();
                if frame.id == Some(1) {
                    assert!(frame.error.is_none());
                    break;
                }
            }
        }
        channel
            .send(
                2,
                Command::Snapshot {
                    messages: Some(300),
                    events: Some(0),
                }
                .encode(2)
                .unwrap(),
            )
            .unwrap();
        let writer_socket = socket.clone();
        let writes = std::thread::spawn(move || {
            let (mut caller, _) =
                crew_core::client::Client::operator(&writer_socket, "native-test-writer").unwrap();
            for ordinal in 0..20 {
                let _: crew_core::rpc::SendResult = caller
                    .call(Call::Send(SendParams {
                        to: "*".into(),
                        body: format!("message {ordinal}"),
                        id: Some(format!("native-barrier-{ordinal}")),
                        ..SendParams::default()
                    }))
                    .unwrap();
            }
        });
        let mut snapshot_received = false;
        let mut seen = BTreeSet::new();
        let deadline = tokio::time::Instant::now() + WAIT;
        while !snapshot_received || seen.len() != 20 {
            let event = tokio::time::timeout_at(deadline, queue.recv())
                .await
                .unwrap()
                .unwrap();
            let HostEvent::Frame { text, .. } = event else {
                continue;
            };
            let frame: crew_core::rpc::ServerFrame = serde_json::from_str(&text).unwrap();
            if frame.id == Some(2) {
                let snapshot: crew_core::rpc::Snapshot =
                    serde_json::from_value(frame.result.unwrap()).unwrap();
                seen = snapshot
                    .messages
                    .into_iter()
                    .filter_map(|message| {
                        message
                            .id
                            .starts_with("native-barrier-")
                            .then_some(message.id)
                    })
                    .collect();
                snapshot_received = true;
            } else if snapshot_received {
                if let Some(crew_core::rpc::Push::Message(message)) = frame.push {
                    if message.id.starts_with("native-barrier-") {
                        assert!(
                            seen.insert(message.id),
                            "same-connection barrier duplicated a message after its snapshot"
                        );
                    }
                }
            }
        }
        writes.join().unwrap();
        channel.close();
        channel.wait_closed().await;
        let (mut caller, _) =
            crew_core::client::Client::operator(&socket, "native-test-stop").unwrap();
        let _: serde_json::Value = caller.call(Call::Shutdown { stop_agents: true }).unwrap();
        tokio::time::timeout(WAIT, server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    });
}

#[test]
fn real_daemon_missing_bad_and_unsafe_token_are_refused_with_safe_diagnostics() {
    if isolated("real_daemon_missing_bad_and_unsafe_token_are_refused_with_safe_diagnostics") {
        return;
    }
    runtime().block_on(async {
        let workspace = tempfile::Builder::new()
            .prefix("crew-w")
            .tempdir_in("/tmp")
            .unwrap();
        let (socket, server) = daemon(workspace.path().to_path_buf()).await;
        let token_path = crew_core::paths::operator_token_path(&socket);
        let valid = std::fs::read(&token_path).unwrap();
        let (mut keeper, _) =
            crew_core::client::Client::operator(&socket, "native-test-stop").unwrap();
        for expected in [
            TransportEnd::MissingCredential,
            TransportEnd::Authentication,
            TransportEnd::Permission,
        ] {
            match expected {
                TransportEnd::MissingCredential => std::fs::remove_file(&token_path).unwrap(),
                TransportEnd::Authentication => crew_core::paths::write_private_file(
                    &token_path,
                    b"synthetic-invalid-token-must-not-leak",
                )
                .unwrap(),
                TransportEnd::Permission => {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let queue = EventQueue::new(1).unwrap();
            let open = HostEffect::Open {
                channel_id: 1,
                profile_id: "disposable".into(),
                endpoint_id: "disposable".into(),
                attempt: 1,
                purpose: ChannelPurpose::Workspace,
                expected_scope: None,
            };
            let mut channel = LocalTransport::open(profile(socket.clone()), &open, &queue).unwrap();
            let end = tokio::time::timeout(WAIT, queue.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(end, HostEvent::Ended { reason, .. } if reason == expected),
                "{end:?}"
            );
            assert!(!format!("{end:?}").contains("synthetic-invalid-token"));
            channel.close();
            channel.wait_closed().await;
        }
        crew_core::paths::write_private_file(&token_path, &valid).unwrap();
        let _: serde_json::Value = keeper.call(Call::Shutdown { stop_agents: true }).unwrap();
        tokio::time::timeout(WAIT, server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    });
}

#[test]
fn agent_context_refuses_before_any_native_local_socket_dial() {
    if std::env::var_os("CREW_NATIVE_AGENT_REFUSAL_CHILD").is_none() {
        let output = Process::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent_context_refuses_before_any_native_local_socket_dial",
                "--nocapture",
            ])
            .env("CREW_NATIVE_AGENT_REFUSAL_CHILD", "1")
            .env("CREW_IDENTITY", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    runtime().block_on(async {
        let queue = EventQueue::new(1).unwrap();
        let open = HostEffect::Open {
            channel_id: 1,
            profile_id: "disposable".into(),
            endpoint_id: "disposable".into(),
            attempt: 1,
            purpose: ChannelPurpose::Workspace,
            expected_scope: None,
        };
        let mut channel = LocalTransport::open(
            profile(PathBuf::from("/no-native-socket-may-be-opened")),
            &open,
            &queue,
        )
        .unwrap();
        assert!(matches!(
            queue.recv().await,
            Some(HostEvent::Ended {
                reason: TransportEnd::AgentContext,
                ..
            })
        ));
        assert_eq!(discover_profiles().unwrap_err(), TransportEnd::AgentContext);
        channel.close();
        channel.wait_closed().await;
    });
}

struct FailingCore(ProbeCore);
impl ClientApi for FailingCore {
    fn apply(
        &mut self,
        input: ClientInput,
        clock: ClockSample,
    ) -> Result<ClientUpdate, ClientError> {
        if matches!(input, ClientInput::Push { .. }) {
            return Err(ClientError::InternalFailure {
                code: "synthetic-state-failure".into(),
            });
        }
        self.0.apply(input, clock)
    }
    fn view(&self, query: ViewQuery, clock: ClockSample) -> Result<ViewData, ClientError> {
        self.0.view(query, clock)
    }
    fn plan(
        &mut self,
        intent: ActionIntent,
        clock: ClockSample,
    ) -> Result<PlanUpdate, ClientError> {
        self.0.plan(intent, clock)
    }
    fn confirm(&mut self, id: u64, clock: ClockSample) -> Result<ClientUpdate, ClientError> {
        self.0.confirm(id, clock)
    }
    fn cancel_confirmation(&mut self, id: u64) -> Result<ClientUpdate, ClientError> {
        self.0.cancel_confirmation(id)
    }
}

#[test]
fn core_input_failure_fences_native_io_and_publishes_safe_client_termination() {
    if isolated("core_input_failure_fences_native_io_and_publishes_safe_client_termination") {
        return;
    }
    runtime().block_on(async {
        let workspace = tempfile::Builder::new()
            .prefix("crew-w")
            .tempdir_in("/tmp")
            .unwrap();
        let (socket, server) = daemon(workspace.path().to_path_buf()).await;
        let (core, _) = ProbeCore::new();
        let session = Arc::new(
            NativeSession::local(
                FailingCore(core),
                Protocol6Codec,
                vec![profile(socket.clone())],
                DriverConfig::default(),
            )
            .unwrap(),
        );
        let (tx, rx) = mpsc::channel();
        let observer = Arc::new(Observer {
            notices: tx,
            session: Mutex::new(Arc::downgrade(&session)),
            queries: Mutex::new(0),
        });
        session.observe(observer).unwrap();
        session
            .connect(
                "disposable".into(),
                "disposable".into(),
                Some("socket-profile".into()),
            )
            .unwrap();
        next(&rx, |notice| notice.workspace == WorkspaceStatus::Live);
        let (mut caller, _) =
            crew_core::client::Client::operator(&socket, "native-test-failure").unwrap();
        let _: crew_core::rpc::SendResult = caller
            .call(Call::Send(SendParams {
                to: "*".into(),
                body: "isolated failure signal".into(),
                id: Some("native-core-failure".into()),
                ..SendParams::default()
            }))
            .unwrap();
        let failed = next(&rx, |notice| notice.endpoint == EndpointStatus::Stopped);
        assert_eq!(failed.reason.as_ref().unwrap().code, "client_state");
        assert!(failed.changes.reset && failed.outcomes.is_empty());
        assert!(!format!("{failed:?}").contains("synthetic-state-failure"));
        assert!(matches!(
            session.query(ViewQuery::Overview),
            Err(ClientError::ClosedClient)
        ));
        let _ = session.close();
        // A failing client still cannot stop the disposable team implicitly.
        let _: crew_core::rpc::Snapshot = caller
            .call(Call::Snapshot {
                messages: Some(0),
                events: Some(0),
            })
            .unwrap();
        let _: serde_json::Value = caller.call(Call::Shutdown { stop_agents: true }).unwrap();
        tokio::time::timeout(WAIT, server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    });
}

#[test]
fn real_daemon_reconnect_uses_new_pid_token_and_attempt_on_same_socket_profile() {
    const NAME: &str =
        "real_daemon_reconnect_uses_new_pid_token_and_attempt_on_same_socket_profile";
    if let Some(workspace) = std::env::var_os("CREW_NATIVE_DAEMON_WORKER") {
        runtime().block_on(async {
            let running = crew::daemon::start(crew::daemon::DaemonOptions {
                workspace: workspace.into(),
                crew_bin: std::env::current_exe().unwrap(),
                pin: None,
            })
            .await
            .unwrap();
            let published = serde_json::json!({"socket":running.socket(),"pid":std::process::id()});
            std::fs::write(
                std::env::var_os("CREW_NATIVE_DAEMON_READY").unwrap(),
                published.to_string(),
            )
            .unwrap();
            running.serve().await.unwrap();
        });
        return;
    }
    if isolated(NAME) {
        return;
    }
    struct Worker(std::process::Child);
    impl Drop for Worker {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn worker(
        name: &str,
        workspace: &std::path::Path,
        ready: &std::path::Path,
    ) -> (Worker, PathBuf, u32) {
        let _ = std::fs::remove_file(ready);
        let child = Worker(
            Process::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env("CREW_NATIVE_DAEMON_WORKER", workspace)
                .env("CREW_NATIVE_DAEMON_READY", ready)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(text) = std::fs::read_to_string(ready) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    return (
                        child,
                        value["socket"].as_str().unwrap().into(),
                        value["pid"].as_u64().unwrap() as u32,
                    );
                }
            }
            assert!(Instant::now() < deadline, "isolated worker did not publish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let workspace = tempfile::Builder::new()
        .prefix("crew-restart")
        .tempdir_in("/tmp")
        .unwrap();
    let ready = workspace.path().join("ready.json");
    let (mut first, socket, first_pid) = worker(NAME, workspace.path(), &ready);
    let (core, _) = ProbeCore::new();
    let session = Arc::new(
        NativeSession::local(
            core,
            Protocol6Codec,
            vec![profile(socket.clone())],
            DriverConfig::default(),
        )
        .unwrap(),
    );
    let (tx, rx) = mpsc::channel();
    session
        .observe(Arc::new(Observer {
            notices: tx,
            session: Mutex::new(Arc::downgrade(&session)),
            queries: Mutex::new(0),
        }))
        .unwrap();
    session
        .connect(
            "disposable".into(),
            "disposable".into(),
            Some("socket-profile".into()),
        )
        .unwrap();
    let before = next(&rx, |notice| notice.workspace == WorkspaceStatus::Live)
        .scope
        .unwrap();
    assert_eq!(before.daemon_pid, Some(first_pid));
    {
        let (mut caller, _) =
            crew_core::client::Client::operator(&socket, "native-restart-stop").unwrap();
        let _: serde_json::Value = caller.call(Call::Shutdown { stop_agents: true }).unwrap();
    }
    assert!(first.0.wait().unwrap().success());
    let (mut second, again, second_pid) = worker(NAME, workspace.path(), &ready);
    assert_eq!(again, socket);
    assert_ne!(first_pid, second_pid);
    session.reconnect().unwrap();
    let after = next(&rx, |notice| {
        notice.workspace == WorkspaceStatus::Live
            && notice
                .scope
                .as_ref()
                .is_some_and(|scope| scope.daemon_pid == Some(second_pid))
    })
    .scope
    .unwrap();
    assert!(after.attempt > before.attempt);
    assert_eq!(after.workspace_id, before.workspace_id);
    assert_eq!(after.daemon_pid, Some(second_pid));
    session.close().unwrap();
    {
        let (mut caller, _) =
            crew_core::client::Client::operator(&socket, "native-restart-stop").unwrap();
        let _: serde_json::Value = caller.call(Call::Shutdown { stop_agents: true }).unwrap();
    }
    assert!(second.0.wait().unwrap().success());
}
