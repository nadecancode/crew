//! Transport-only probe core. Real reducer/actions/projections are required by
//! native-transport-validation, a separate acceptance gate. This probe records
//! the exact inputs and emits explicit typed effects to exercise the driver.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crew_client::api::*;
use crew_client_transport::{
    Clock, DriverConfig, HostEffect, HostEvent, HostUpdate, Protocol6Codec, SessionDriver,
};
use crew_protocol::rpc::{SendParams, Snapshot};
use serde_json::{json, Value};

pub const PID: u32 = 4242;
pub const WORKSPACE: &str = "/fixture/workspace";
pub const STARTED: i64 = 1234;

#[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
pub fn isolated_local(name: &str) -> bool {
    if std::env::var_os("CREW_NATIVE_TRANSPORT_TEST_CHILD").is_some() {
        return false;
    }
    let root = tempfile::Builder::new()
        .prefix("crew-nt")
        .tempdir_in("/tmp")
        .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
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

#[derive(Default)]
pub struct Probe {
    pub inputs: Vec<ClientInput>,
    pub snapshot: Option<Snapshot>,
    pub scope: Option<ClientScope>,
}

pub struct ProbeCore {
    pub probe: Arc<Mutex<Probe>>,
    revision: u64,
    next_operation: u64,
    pub hold_next_outcome: bool,
    held: Vec<OperationNotice>,
}

impl ProbeCore {
    pub fn new() -> (Self, Arc<Mutex<Probe>>) {
        let probe = Arc::new(Mutex::new(Probe::default()));
        (
            Self {
                probe: probe.clone(),
                revision: 0,
                next_operation: 1,
                hold_next_outcome: false,
                held: Vec::new(),
            },
            probe,
        )
    }
    fn update(&mut self, effects: Vec<ClientEffect>) -> ClientUpdate {
        self.revision += 1;
        ClientUpdate {
            revision: self.revision,
            changes: ChangeSet {
                revision: self.revision,
                ..ChangeSet::default()
            },
            effects,
            outcomes: Vec::new(),
        }
    }
}

impl ClientApi for ProbeCore {
    fn apply(&mut self, input: ClientInput, _: ClockSample) -> Result<ClientUpdate, ClientError> {
        let mut outcomes = match &input {
            ClientInput::OperationFinished {
                scope,
                operation_id,
                outcome,
            } => vec![operation_notice(scope.clone(), *operation_id, outcome)],
            ClientInput::Wake { .. } => std::mem::take(&mut self.held),
            _ => Vec::new(),
        };
        let mut effects = Vec::new();
        if self.hold_next_outcome && !outcomes.is_empty() {
            self.held.append(&mut outcomes);
            self.hold_next_outcome = false;
            effects.push(ClientEffect::Schedule {
                timer_id: 7,
                deadline_ms: 30_000,
            });
        }
        {
            let mut probe = self.probe.lock().unwrap();
            match &input {
                ClientInput::Bind { scope, .. } => probe.scope = Some(scope.clone()),
                ClientInput::Snapshot { snapshot, .. } => {
                    probe.snapshot = Some((**snapshot).clone())
                }
                _ => {}
            }
            probe.inputs.push(input);
        }
        let mut update = self.update(effects);
        update.outcomes = outcomes;
        Ok(update)
    }

    fn view(&self, query: ViewQuery, _: ClockSample) -> Result<ViewData, ClientError> {
        if query != ViewQuery::Overview {
            return Err(ClientError::InvalidInput {
                field: "query".into(),
                reason: "probe supplies Overview only".into(),
            });
        }
        let probe = self.probe.lock().unwrap();
        let session = probe
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.session.clone());
        Ok(ViewData {
            scope: probe.scope.clone(),
            revision: self.revision,
            synchronized_at_ms: None,
            freshness: if session.is_some() {
                Freshness::Fresh
            } else {
                Freshness::Loading
            },
            payload: ViewPayload::Overview(Box::new(OverviewData {
                session,
                agent_count: 0,
                working_count: 0,
                task_count: 0,
                done_count: 0,
                question_count: 0,
                warnings: Vec::new(),
            })),
        })
    }

    fn plan(&mut self, intent: ActionIntent, _: ClockSample) -> Result<PlanUpdate, ClientError> {
        let operation_id = self.next_operation;
        self.next_operation += 1;
        let effect = match intent {
            ActionIntent::Agent {
                scope,
                id,
                action: AgentAction::Pause,
            } => ClientEffect::Request {
                scope,
                operation_id,
                lane: RequestLane::Action,
                command: Command::AgentPause { id },
            },
            ActionIntent::SendMessage {
                scope,
                to,
                body,
                message_id,
            } => ClientEffect::Request {
                scope,
                operation_id,
                lane: RequestLane::Action,
                command: Command::Send(SendParams {
                    to,
                    body,
                    id: Some(message_id),
                    ..SendParams::default()
                }),
            },
            ActionIntent::Read { scope, .. } => ClientEffect::Request {
                scope,
                operation_id,
                lane: RequestLane::Read,
                command: Command::AgentsList,
            },
            ActionIntent::Attach {
                scope,
                agent,
                rows,
                cols,
            } => ClientEffect::Request {
                scope,
                operation_id,
                lane: RequestLane::Terminal {
                    attachment_id: operation_id,
                },
                command: Command::Attach { agent, rows, cols },
            },
            ActionIntent::AttachInput {
                scope,
                attachment_id,
                data,
            } => {
                use base64::Engine as _;
                ClientEffect::Request {
                    scope,
                    operation_id,
                    lane: RequestLane::Terminal { attachment_id },
                    command: Command::AttachInput {
                        data: base64::engine::general_purpose::STANDARD.encode(data),
                    },
                }
            }
            ActionIntent::AttachResize {
                scope,
                attachment_id,
                rows,
                cols,
            } => ClientEffect::Request {
                scope,
                operation_id,
                lane: RequestLane::Terminal { attachment_id },
                command: Command::AttachResize { rows, cols },
            },
            ActionIntent::Detach {
                scope,
                attachment_id,
            } => ClientEffect::CloseAttachment {
                scope,
                operation_id,
                attachment_id,
            },
            _ => {
                return Err(ClientError::InvalidInput {
                    field: "intent".into(),
                    reason: "unsupported probe intent".into(),
                })
            }
        };
        Ok(PlanUpdate {
            plan: ActionPlan::Dispatch { operation_id },
            update: self.update(vec![effect]),
        })
    }

    fn confirm(&mut self, _: u64, _: ClockSample) -> Result<ClientUpdate, ClientError> {
        Err(ClientError::StaleConfirmation)
    }
    fn cancel_confirmation(&mut self, _: u64) -> Result<ClientUpdate, ClientError> {
        Ok(self.update(Vec::new()))
    }
}

fn operation_notice(
    scope: ClientScope,
    operation_id: u64,
    outcome: &OperationOutcome,
) -> OperationNotice {
    let (kind, reason, attachment_id) = match outcome {
        OperationOutcome::Succeeded { .. } => (OperationOutcomeKind::Success, None, None),
        OperationOutcome::Rejected {
            code,
            message,
            details,
        } => (
            OperationOutcomeKind::Rejected,
            Some(ClientReason {
                details: details.clone(),
                code: code.clone(),
                message: message.clone(),
            }),
            None,
        ),
        OperationOutcome::NotSent { reason } => {
            (OperationOutcomeKind::NotSent, Some(reason.clone()), None)
        }
        OperationOutcome::Canceled => (OperationOutcomeKind::Canceled, None, None),
        OperationOutcome::OutcomeUnknown { reason } => {
            (OperationOutcomeKind::Unknown, Some(reason.clone()), None)
        }
        OperationOutcome::ClosedLocally { attachment_id } => (
            OperationOutcomeKind::ClosedLocally,
            None,
            Some(*attachment_id),
        ),
        OperationOutcome::SentUnacknowledged { attachment_id } => (
            OperationOutcomeKind::SentUnacknowledged,
            None,
            Some(*attachment_id),
        ),
    };
    OperationNotice {
        scope,
        operation_id,
        kind,
        reason,
        attachment_id,
    }
}

#[derive(Default)]
pub struct TestClock(pub AtomicU64);
impl Clock for TestClock {
    fn sample(&self) -> ClockSample {
        let now = self.0.load(Ordering::SeqCst);
        ClockSample {
            wall_ms: STARTED + now as i64,
            monotonic_ms: now,
        }
    }
}

pub type Driver = SessionDriver<ProbeCore, Protocol6Codec>;

pub fn driver() -> (Driver, Arc<Mutex<Probe>>, Arc<TestClock>) {
    let (core, probe) = ProbeCore::new();
    let clock = Arc::new(TestClock::default());
    let driver = SessionDriver::with_clock(
        core,
        Protocol6Codec,
        DriverConfig {
            reconnect: false,
            ..DriverConfig::default()
        },
        clock.clone(),
    )
    .unwrap();
    (driver, probe, clock)
}

pub fn context(attempt: u64) -> ConnectionContext {
    ConnectionContext {
        scope: ClientScope {
            endpoint_id: "host".into(),
            workspace_id: "provisional".into(),
            generation: None,
            daemon_pid: Some(PID),
            daemon_started_at_ms: None,
            attempt,
        },
        mode: ConnectionMode::LocalOperator,
        snapshot_mode: SnapshotMode::OrderedConnection,
        grant: Grant {
            grant_id: "operator".into(),
            tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
            workspace_ids: vec!["provisional".into()],
            expires_at_ms: None,
        },
    }
}

pub fn snapshot() -> Snapshot {
    let mut snapshot = Snapshot::default();
    snapshot.session.workspace = WORKSPACE.into();
    snapshot.session.state_dir = format!("{WORKSPACE}/.crew");
    snapshot.session.daemon_pid = PID;
    snapshot.session.daemon_started_at = STARTED;
    snapshot.session.version = env!("CARGO_PKG_VERSION").into();
    snapshot.server_time = STARTED;
    snapshot
}

pub fn opened(driver: &mut Driver, open: &HostEffect) -> HostUpdate {
    let HostEffect::Open {
        channel_id,
        attempt,
        expected_scope,
        ..
    } = open
    else {
        panic!("open effect");
    };
    let mut context = context(*attempt);
    if let Some(scope) = expected_scope {
        context.scope = scope.clone();
        context.scope.daemon_started_at_ms = None;
        context.grant.workspace_ids = vec![scope.workspace_id.clone()];
    }
    driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: *channel_id,
            context,
            peer: PeerInfo {
                version: env!("CARGO_PKG_VERSION").into(),
                protocol: 6,
                pid: PID,
                project: None,
            },
        })
        .unwrap()
}

pub fn send(update: &HostUpdate, method: &str) -> (u64, u64, Value) {
    update
        .effects
        .iter()
        .find_map(|effect| match effect {
            HostEffect::Send {
                channel_id,
                request_id,
                text,
            } => {
                let value: Value = serde_json::from_str(text).unwrap();
                (value["method"] == method).then_some((*channel_id, *request_id, value))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {method} send in {:?}", update.effects))
}

pub fn reply(driver: &mut Driver, channel_id: u64, request_id: u64, result: Value) -> HostUpdate {
    driver
        .handle(HostEvent::Frame {
            channel_id,
            text: json!({"id":request_id,"result":result}).to_string(),
        })
        .unwrap()
}

pub fn bootstrap(driver: &mut Driver) -> (u64, ClientScope) {
    let connecting = driver
        .connect("profile".into(), "host".into(), Some("provisional".into()))
        .unwrap();
    let open = connecting
        .effects
        .iter()
        .find(|effect| matches!(effect, HostEffect::Open { .. }))
        .unwrap();
    let subscribed = opened(driver, open);
    let (channel, id, _) = send(&subscribed, "subscribe");
    let loading = reply(driver, channel, id, json!({"subscribed":true}));
    let (_, id, _) = send(&loading, "snapshot");
    let live = reply(
        driver,
        channel,
        id,
        serde_json::to_value(snapshot()).unwrap(),
    );
    assert_eq!(live.notice.workspace, WorkspaceStatus::Live);
    (channel, live.notice.scope.unwrap())
}

pub fn ready_lane(driver: &mut Driver, update: &HostUpdate, method: &str) -> (u64, u64) {
    let open = update
        .effects
        .iter()
        .find(|effect| matches!(effect, HostEffect::Open { .. }))
        .unwrap();
    let verification = opened(driver, open);
    let (channel, id, frame) = send(&verification, "snapshot");
    assert_eq!(frame["params"]["messages"], 0);
    assert_eq!(frame["params"]["events"], 0);
    let dispatched = reply(
        driver,
        channel,
        id,
        serde_json::to_value(snapshot()).unwrap(),
    );
    let (_, id, _) = send(&dispatched, method);
    (channel, id)
}

pub fn attach(driver: &mut Driver, scope: &ClientScope, agent: &str) -> (u64, u64) {
    let plan = driver
        .plan(ActionIntent::Attach {
            scope: scope.clone(),
            agent: agent.into(),
            rows: 24,
            cols: 80,
        })
        .unwrap();
    let ActionPlan::Dispatch { operation_id } = plan.plan else {
        panic!("dispatch");
    };
    let (channel, id) = ready_lane(driver, &plan.update, "attach");
    let update = reply(driver, channel, id, json!({"attached":agent}));
    assert_eq!(update.notice.outcomes[0].operation_id, operation_id);
    (operation_id, channel)
}
