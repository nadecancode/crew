//! Actual shared engine, native service and scripted agent processes on real
//! daemon-owned PTYs. Synthetic agents replace the provider, never ClientApi.
use super::*;
use crew_client_transport::local::{discover_profiles, LocalProfile};
use crew_client_transport::local::{LocalChannel, LocalTransport};
use crew_client_transport::queue::EventQueue;
use crew_client_transport::service::{ChannelHandle, Connector};
use crew_client_transport::{ChannelPurpose, HostEffect, HostEvent, SendDisposition, TransportEnd};
use crew_core::config::{
    AgentDefaults, AgentSpec, BackendChoice, Isolation, OrchestratorConfig, TaskSpec, TeamConfig,
    WorkspaceConfig,
};
use crew_protocol::RuntimeKind;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use tokio::sync::watch;

fn binary() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("CREW_NATIVE_VALIDATION_BIN")
            .expect("journey-tests require explicit feature-built CREW_NATIVE_VALIDATION_BIN"),
    );
    assert!(path.is_absolute() && path.is_file());
    path
}

async fn team() -> Host {
    let bin = binary();
    let host = Host::start_with_crew(bin.clone()).await;
    let agents = ["alpha", "beta"]
        .into_iter()
        .map(|id| AgentSpec {
            id: id.into(),
            role: "isolated native validation worker".into(),
            runtime: Some(RuntimeKind::Shell),
            command: vec![
                bin.to_string_lossy().into_owned(),
                "scripted-agent".into(),
                "--on-task=status working validating native service".into(),
                format!("--on-task=send operator question {id} native question"),
                "--on-task=wait-message operator".into(),
                "--on-task=sleep 60000".into(),
            ],
            ..Default::default()
        })
        .collect();
    let tasks = ["alpha", "beta"]
        .into_iter()
        .map(|id| TaskSpec {
            id: format!("{id}-work"),
            title: Some(format!("{id} real current work")),
            description: "Validate scoped native engine with real PTY".into(),
            agent: Some(id.into()),
            ..Default::default()
        })
        .collect();
    let config = TeamConfig {
        project: "native-journey".into(),
        objective: "real native transport acceptance".into(),
        style: None,
        instructions: String::new(),
        workspace: WorkspaceConfig {
            path: host.workspace.to_string_lossy().into_owned(),
            isolation: Isolation::Shared,
            ..Default::default()
        },
        backend: BackendChoice::Pty,
        herdr: Default::default(),
        defaults: AgentDefaults {
            runtime: RuntimeKind::Shell,
            ..Default::default()
        },
        orchestrator: OrchestratorConfig {
            agent: false,
            ..Default::default()
        },
        agents,
        tasks,
        limits: Default::default(),
    };
    let socket = host.socket.clone();
    tokio::task::spawn_blocking(move || {
        let (mut caller, _) =
            crew_core::client::Client::operator(&socket, "native-journey-seed").unwrap();
        caller.set_timeout(Some(Duration::from_secs(20))).unwrap();
        caller
            .call_value(crew_core::rpc::Call::LoadTeam {
                config: Box::new(config),
                herdr_socket: None,
            })
            .unwrap();
    })
    .await
    .unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let socket = host.socket.clone();
        let snapshot: crew_protocol::rpc::Snapshot = tokio::task::spawn_blocking(move || {
            let (mut caller, _) =
                crew_core::client::Client::operator(&socket, "native-journey-seed").unwrap();
            caller
                .call(crew_core::rpc::Call::Snapshot {
                    messages: Some(100),
                    events: Some(100),
                })
                .unwrap()
        })
        .await
        .unwrap();
        if snapshot.agents.len() == 2 && snapshot.questions.len() == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "scripted workers/questions before deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    host
}

#[derive(Clone, Debug)]
enum Published {
    Attach(ClientScope, u64),
    Bytes(ClientScope, u64),
}
struct JourneyObserver {
    notices: sync::Sender<ClientNotice>,
    terminal: sync::Sender<TerminalChunk>,
    order: Arc<Mutex<Vec<Published>>>,
}
impl ClientObserver for JourneyObserver {
    fn changed(&self, notice: ClientNotice) {
        for outcome in &notice.outcomes {
            if outcome.kind == OperationOutcomeKind::Success {
                if let Some(id) = outcome.attachment_id {
                    self.order
                        .lock()
                        .unwrap()
                        .push(Published::Attach(outcome.scope.clone(), id));
                }
            }
        }
        let _ = self.notices.send(notice);
    }
    fn terminal(&self, chunk: TerminalChunk) {
        if !chunk.data.is_empty() {
            self.order
                .lock()
                .unwrap()
                .push(Published::Bytes(chunk.scope.clone(), chunk.attachment_id));
        }
        let _ = self.terminal.send(chunk);
    }
}

type Publication = (
    sync::Receiver<ClientNotice>,
    sync::Receiver<TerminalChunk>,
    Arc<Mutex<Vec<Published>>>,
);
fn observe(service: &NativeSession) -> Publication {
    let (notices, rx) = sync::channel();
    let (terminal, tr) = sync::channel();
    let order = Arc::new(Mutex::new(Vec::new()));
    service
        .observe(Arc::new(JourneyObserver {
            notices,
            terminal,
            order: order.clone(),
        }))
        .unwrap();
    (rx, tr, order)
}

fn core() -> crew_client::ClientCore {
    crew_client::ClientCore::new(ClientConfig {
        limits: NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap()
}

fn dispatch(
    service: &NativeSession,
    notices: &sync::Receiver<ClientNotice>,
    intent: ActionIntent,
    kind: OperationOutcomeKind,
) -> OperationNotice {
    let ActionPlan::Dispatch { operation_id } = service.plan(intent).unwrap() else {
        panic!("dispatch required");
    };
    let final_notice = notice(notices, |next| {
        next.outcomes
            .iter()
            .any(|outcome| outcome.operation_id == operation_id)
    });
    let outcome = final_notice
        .outcomes
        .into_iter()
        .find(|outcome| outcome.operation_id == operation_id)
        .unwrap();
    assert_eq!(outcome.kind, kind, "real native result: {outcome:?}");
    outcome
}

fn question(service: &NativeSession, from: &str) -> crew_protocol::Message {
    let ViewPayload::Questions(items) = service.query(ViewQuery::Questions).unwrap().payload else {
        panic!("questions");
    };
    items
        .into_iter()
        .find(|message| message.from == from)
        .unwrap()
}

fn projections(service: &NativeSession, scope: &ClientScope) {
    let view = service
        .query(ViewQuery::AgentDetail { id: "alpha".into() })
        .unwrap();
    assert_eq!(view.scope.as_ref(), Some(scope));
    assert_eq!(view.freshness, Freshness::Fresh);
    let ViewPayload::AgentDetail(agent) = view.payload else {
        panic!("agent detail");
    };
    assert_eq!(
        agent.agent.unwrap().current_work.unwrap().task_id,
        "alpha-work"
    );
    let ViewPayload::TaskDetail(task) = service
        .query(ViewQuery::TaskDetail {
            id: "alpha-work".into(),
        })
        .unwrap()
        .payload
    else {
        panic!("task detail");
    };
    assert_eq!(task.task.unwrap().task.id, "alpha-work");
}

fn terminal_echo(
    service: &NativeSession,
    notices: &sync::Receiver<ClientNotice>,
    terminal: &sync::Receiver<TerminalChunk>,
    scope: &ClientScope,
    agent: &str,
    marker: &str,
) -> u64 {
    let attached = dispatch(
        service,
        notices,
        ActionIntent::Attach {
            scope: scope.clone(),
            agent: agent.into(),
            rows: 24,
            cols: 80,
        },
        OperationOutcomeKind::Success,
    );
    let id = attached.attachment_id.unwrap();
    assert_eq!(id, attached.operation_id);
    let resized = dispatch(
        service,
        notices,
        ActionIntent::AttachResize {
            scope: scope.clone(),
            attachment_id: id,
            rows: 30,
            cols: 100,
        },
        OperationOutcomeKind::SentUnacknowledged,
    );
    assert_eq!(resized.attachment_id, Some(id));
    let written = dispatch(
        service,
        notices,
        ActionIntent::AttachInput {
            scope: scope.clone(),
            attachment_id: id,
            data: format!("{marker}\n").into_bytes(),
        },
        OperationOutcomeKind::SentUnacknowledged,
    );
    assert_eq!(written.attachment_id, Some(id));
    let deadline = Instant::now() + WAIT;
    let mut data = Vec::new();
    while !String::from_utf8_lossy(&data).contains(marker) {
        let chunk = terminal
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        assert_eq!(chunk.scope, *scope);
        if chunk.attachment_id == id {
            assert!(chunk.ended.is_none());
            data.extend(chunk.data);
        }
    }
    id
}

fn ordering(order: &Arc<Mutex<Vec<Published>>>) {
    let mut known = std::collections::HashSet::new();
    for event in order.lock().unwrap().iter() {
        match event {
            Published::Attach(scope, id) => {
                known.insert((scope.clone(), *id));
            }
            Published::Bytes(scope, id) => assert!(
                known.contains(&(scope.clone(), *id)),
                "identified success precedes real PTY bytes"
            ),
        }
    }
}

#[test]
fn real_local_native_engine_tasks_questions_confirmed_controls_and_two_terminal_scopes() {
    let name = "journey::real_local_native_engine_tasks_questions_confirmed_controls_and_two_terminal_scopes";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let host = team().await;
        let profiles: Vec<LocalProfile> = discover_profiles()
            .unwrap()
            .into_iter()
            .map(|workspace| workspace.profile)
            .collect();
        assert_eq!(profiles.len(), 1);
        let profile = profiles[0].clone();
        let service =
            NativeSession::local(core(), Protocol6Codec, profiles, DriverConfig::default())
                .unwrap();
        let (notices, terminal, order) = observe(&service);
        service
            .connect(profile.id, profile.endpoint_id, Some(profile.workspace_key))
            .unwrap();
        let scope = notice(&notices, |notice| notice.workspace == WorkspaceStatus::Live)
            .scope
            .unwrap();
        projections(&service, &scope);
        let alpha = question(&service, "alpha");
        dispatch(
            &service,
            &notices,
            ActionIntent::Answer {
                scope: scope.clone(),
                question_id: alpha.id.clone(),
                body: "native local answer".into(),
            },
            OperationOutcomeKind::Success,
        );
        assert!(matches!(
            service
                .plan(ActionIntent::Answer {
                    scope: scope.clone(),
                    question_id: alpha.id,
                    body: "stale answer".into()
                })
                .unwrap(),
            ActionPlan::Unavailable { .. }
        ));
        dispatch(
            &service,
            &notices,
            send_intent(&scope, "local-live-message"),
            OperationOutcomeKind::Success,
        );
        let first = terminal_echo(
            &service,
            &notices,
            &terminal,
            &scope,
            "alpha",
            "native-local-alpha-once",
        );
        let second = terminal_echo(
            &service,
            &notices,
            &terminal,
            &scope,
            "beta",
            "native-local-beta-once",
        );
        assert_ne!(first, second);
        ordering(&order);
        dispatch(
            &service,
            &notices,
            ActionIntent::Detach {
                scope: scope.clone(),
                attachment_id: first,
            },
            OperationOutcomeKind::ClosedLocally,
        );
        assert!(matches!(
            service
                .plan(ActionIntent::AttachInput {
                    scope: scope.clone(),
                    attachment_id: first,
                    data: b"closed bytes".to_vec()
                })
                .unwrap(),
            ActionPlan::Unavailable { .. }
        ));
        let ActionPlan::Confirm {
            confirmation_id,
            kind: ConfirmationKind::KillAgent,
            ..
        } = service
            .plan(ActionIntent::Agent {
                scope: scope.clone(),
                id: "beta".into(),
                action: AgentAction::Kill,
            })
            .unwrap()
        else {
            panic!("kill confirmation required");
        };
        service.confirm(confirmation_id).unwrap();
        notice(&notices, |_| matches!(service.query(ViewQuery::AgentDetail { id: "beta".into() }).unwrap().payload,
            ViewPayload::AgentDetail(data) if data.agent.as_ref().is_some_and(|agent| crew_client::policy::status_policy(agent.agent.status).terminal())));
        let deadline = Instant::now() + WAIT;
        loop {
            let chunk = terminal
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("killed agent terminal end {error:?}; detail {:?}", service.query(ViewQuery::AgentDetail { id: "beta".into() }).unwrap()));
            if chunk.attachment_id == second && chunk.ended.is_some() {
                break;
            }
        }
        assert!(matches!(
            service
                .plan(ActionIntent::AttachInput {
                    scope: scope.clone(),
                    attachment_id: second,
                    data: b"ended bytes".to_vec()
                })
                .unwrap(),
            ActionPlan::Unavailable { .. }
        ));
        service.reconnect().unwrap();
        let newer = notice(&notices, |notice| {
            notice.workspace == WorkspaceStatus::Live
                && notice
                    .scope
                    .as_ref()
                    .is_some_and(|new| new.attempt > scope.attempt)
        })
        .scope
        .unwrap();
        assert_eq!(newer.daemon_pid, scope.daemon_pid);
        assert_eq!(newer.daemon_started_at_ms, scope.daemon_started_at_ms);
        assert!(matches!(
            service
                .plan(send_intent(&scope, "stale-local-message"))
                .unwrap(),
            ActionPlan::Unavailable { .. }
        ));
        service.close().unwrap();
        host.close().await;
    });
}

#[test]
fn real_scoped_native_engine_read_default_control_terminal_and_revocation_on_actual_ptys() {
    let name = "journey::real_scoped_native_engine_read_default_control_terminal_and_revocation_on_actual_ptys";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let host = team().await;
        let signer = Arc::new(TestSigner::new(49)); let client = host.client(signer.clone());
        let access = enroll_capabilities(&host, &client, &signer, false, false, 3600).await;
        let service = client.session(core(), Protocol6Codec, DriverConfig { reconnect: false, ..Default::default() }).await.unwrap();
        let (notices, terminal, order) = observe(&service);
        service.connect("isolated".into(), client.profile().endpoint_id(), None).unwrap();
        let read = select(&service, &notices); projections(&service, &read);
        let beta = question(&service, "beta");
        assert!(service.select_workspace("0202020202020202".into()).is_err());
        assert!(matches!(service.plan(ActionIntent::Answer { scope: read.clone(), question_id: beta.id.clone(), body: "read denied".into() }).unwrap(), ActionPlan::Unavailable { .. }));
        assert!(matches!(service.plan(ActionIntent::Attach { scope: read.clone(), agent: "alpha".into(), rows:24, cols:80 }).unwrap(), ActionPlan::Unavailable { .. }));
        assert!(matches!(service.plan(send_intent(&read, "read-denied")).unwrap(), ActionPlan::Unavailable { .. }));
        let key = ResourceKey { entity: EntityKey { scope: read.clone(), entity: Entity::Task("alpha-work".into()) }, kind: ResourceKind::TaskCard, options: ReadOptions::Default };
        dispatch(&service, &notices, ActionIntent::Read { scope: read.clone(), resource: ResourceRequest { key: key.clone() } }, OperationOutcomeKind::Success);
        assert!(matches!(service.query(ViewQuery::Evidence { key }).unwrap().payload, ViewPayload::Evidence(state) if state.status == ResourceStatus::Ready));
        admin("native_grant", json!({"device_id":access.device_id,"selection":{"workspace":host.workspace,"control":true,"terminal":false}})).await;
        notice(&notices, |notice| notice.workspace == WorkspaceStatus::AccessRevoked);
        let control = select(&service, &notices); assert!(control.attempt > read.attempt);
        assert!(matches!(service.plan(ActionIntent::Attach { scope: control.clone(), agent: "alpha".into(), rows:24, cols:80 }).unwrap(), ActionPlan::Unavailable { .. }), "Control alone never grants Terminal");
        dispatch(&service, &notices, ActionIntent::Answer { scope: control.clone(), question_id: beta.id.clone(), body: "native scoped answer".into() }, OperationOutcomeKind::Success);
        assert!(matches!(service.plan(ActionIntent::Answer { scope: control.clone(), question_id: beta.id, body: "stale scoped answer".into() }).unwrap(), ActionPlan::Unavailable { .. }));
        dispatch(&service, &notices, send_intent(&control, "scoped-live-message"), OperationOutcomeKind::Success);
        let ActionPlan::Confirm { confirmation_id, kind: ConfirmationKind::KillAgent, .. } = service.plan(ActionIntent::Agent { scope: control.clone(), id: "alpha".into(), action: AgentAction::Kill }).unwrap() else { panic!("scoped kill confirmation"); };
        service.cancel_confirmation(confirmation_id).unwrap();
        assert!(service.confirm(confirmation_id).is_err());
        assert!(matches!(service.query(ViewQuery::AgentDetail { id:"alpha".into() }).unwrap().payload, ViewPayload::AgentDetail(data) if data.agent.as_ref().is_some_and(|agent| agent.agent.pid.is_some())));
        admin("native_grant", json!({"device_id":access.device_id,"selection":{"workspace":host.workspace,"control":true,"terminal":true}})).await;
        notice(&notices, |notice| notice.workspace == WorkspaceStatus::AccessRevoked);
        let granted = select(&service, &notices);
        let alpha = terminal_echo(&service, &notices, &terminal, &granted, "alpha", "native-scoped-alpha-once");
        let beta = terminal_echo(&service, &notices, &terminal, &granted, "beta", "native-scoped-beta-once");
        assert_ne!(alpha, beta); ordering(&order);
        admin("native_revoke", json!({"device_id":access.device_id,"workspace":granted.workspace_id})).await;
        let deadline = Instant::now()+WAIT; let mut ended = std::collections::BTreeSet::new();
        while ended.len()!=2 {
            let chunk=terminal.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap();
            assert_eq!(chunk.scope, granted);
            if let Some(reason)=chunk.ended { assert_eq!(reason, TerminalEnd::Revocation); ended.insert(chunk.attachment_id); }
        }
        assert_eq!(ended,std::collections::BTreeSet::from([alpha,beta]));
        assert!(matches!(service.plan(ActionIntent::AttachInput { scope: granted.clone(), attachment_id: alpha, data:b"revoked bytes".to_vec() }).unwrap(), ActionPlan::Unavailable { .. }));
        assert!(matches!(service.plan(send_intent(&control,"old-control-scope")).unwrap(), ActionPlan::Unavailable { .. }));
        service.close().unwrap(); client.close(); host.close().await;
    });
}

/// A transport-loss injector around actual protected local IO. It forwards
/// verified events unchanged, except for one fully-written mutation's reply.
/// Core, daemon handlers, authentication and framing are never substituted.
struct CutReplyConnector {
    profile: LocalProfile,
    pauses: Arc<AtomicUsize>,
}

struct CutReplyChannel {
    local: Arc<Mutex<LocalChannel>>,
    cutoff: Arc<AtomicU64>,
    pauses: Arc<AtomicUsize>,
    finished: watch::Receiver<bool>,
}

impl ChannelHandle for CutReplyChannel {
    fn send(&self, request_id: u64, text: String) -> Result<(), TransportEnd> {
        let request: Value = serde_json::from_str(&text).map_err(|_| TransportEnd::Malformed)?;
        if request["method"] == "agent_pause" {
            self.cutoff.store(request_id, Ordering::Release);
            self.pauses.fetch_add(1, Ordering::AcqRel);
        }
        self.local
            .lock()
            .map_err(|_| TransportEnd::Stopped)?
            .send(request_id, text)
    }
    fn close(&self) {
        if let Ok(local) = self.local.lock() {
            local.close();
        }
    }
    fn fail(&self, reason: TransportEnd) {
        if let Ok(local) = self.local.lock() {
            local.fail(reason);
        }
    }
    fn wait_closed(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            while !*self.finished.borrow_and_update() {
                if self.finished.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}

impl Drop for CutReplyChannel {
    fn drop(&mut self) {
        self.close();
    }
}

impl Connector for CutReplyConnector {
    fn open(
        &self,
        effect: &HostEffect,
        events: &EventQueue,
    ) -> Result<Box<dyn ChannelHandle>, TransportEnd> {
        let HostEffect::Open {
            channel_id,
            purpose,
            ..
        } = effect
        else {
            return Err(TransportEnd::Permission);
        };
        let inner = EventQueue::new(1)?;
        let outer = events.register(*channel_id)?;
        let local = Arc::new(Mutex::new(LocalTransport::open(
            self.profile.clone(),
            effect,
            &inner,
        )?));
        let cutoff = Arc::new(AtomicU64::new(0));
        let (finished, completion) = watch::channel(false);
        let worker = local.clone();
        let target = cutoff.clone();
        let action = *purpose == ChannelPurpose::Action;
        tokio::spawn(async move {
            while let Some(event) = inner.recv().await {
                let request = target.load(Ordering::Acquire);
                if action && request != 0 {
                    if let HostEvent::Frame { text, .. } = &event {
                        let id = serde_json::from_str::<Value>(text)
                            .ok()
                            .and_then(|frame| frame["id"].as_u64());
                        if id == Some(request) {
                            continue;
                        }
                    }
                }
                let cut = action
                    && request != 0
                    && matches!(&event, HostEvent::SendProgress {
                    request_id, disposition: SendDisposition::FullyWritten, ..
                } if *request_id == request);
                let ended = matches!(event, HostEvent::Ended { .. });
                if outer.try_send(event).is_err() {
                    break;
                }
                if cut {
                    // The actual writer reported full acceptance before the
                    // real socket is closed; the daemon may already have run it.
                    if let Ok(local) = worker.lock() {
                        local.fail(TransportEnd::Closed);
                    }
                }
                if ended {
                    break;
                }
            }
            if let Ok(local) = worker.lock() {
                local.close();
            }
            finished.send_replace(true);
        });
        Ok(Box::new(CutReplyChannel {
            local,
            cutoff,
            pauses: self.pauses.clone(),
            finished: completion,
        }))
    }
}

#[test]
fn real_native_mutation_reply_loss_is_unknown_once_and_not_replayed_after_fresh_barrier() {
    let name = "journey::real_native_mutation_reply_loss_is_unknown_once_and_not_replayed_after_fresh_barrier";
    if isolated(name) {
        return;
    }
    runtime().block_on(async {
        let host = team().await;
        let profile = discover_profiles().unwrap().remove(0).profile;
        let pauses = Arc::new(AtomicUsize::new(0));
        let service = NativeSession::with_connector(core(), Protocol6Codec,
            CutReplyConnector { profile: profile.clone(), pauses: pauses.clone() },
            DriverConfig { reconnect: false, ..Default::default() }).unwrap();
        let (notices, _, _) = observe(&service);
        service.connect(profile.id, profile.endpoint_id, Some(profile.workspace_key)).unwrap();
        let scope = notice(&notices, |notice| notice.workspace == WorkspaceStatus::Live).scope.unwrap();
        let unknown = dispatch(&service, &notices, ActionIntent::Agent { scope: scope.clone(), id: "alpha".into(), action: AgentAction::Pause }, OperationOutcomeKind::Unknown);
        assert_eq!(unknown.scope, scope); assert_eq!(unknown.attachment_id, None);
        assert_eq!(pauses.load(Ordering::Acquire), 1);
        service.reconnect().unwrap();
        let newer = notice(&notices, |notice| notice.workspace == WorkspaceStatus::Live && notice.scope.as_ref().is_some_and(|current| current.attempt > scope.attempt)).scope.unwrap();
        assert_eq!(newer.daemon_pid, scope.daemon_pid); assert_eq!(newer.daemon_started_at_ms, scope.daemon_started_at_ms);
        let fresh = service.query(ViewQuery::AgentDetail { id: "alpha".into() }).unwrap();
        assert_eq!(fresh.scope.as_ref(), Some(&newer)); assert_eq!(fresh.freshness, Freshness::Fresh);
        assert!(matches!(fresh.payload,
            ViewPayload::AgentDetail(data) if data.agent.as_ref().is_some_and(|agent| agent.agent.paused)), "the real daemon accepted the mutation despite a lost reply");
        assert_eq!(pauses.load(Ordering::Acquire), 1, "unknown mutation is never replayed");
        while let Ok(notice) = notices.try_recv() {
            assert!(!notice.outcomes.iter().any(|outcome| outcome.operation_id == unknown.operation_id), "one final logical outcome");
        }
        assert!(matches!(service.plan(ActionIntent::Agent { scope, id: "alpha".into(), action: AgentAction::Pause }).unwrap(), ActionPlan::Unavailable { .. }));
        service.close().unwrap(); host.close().await;
    });
}
