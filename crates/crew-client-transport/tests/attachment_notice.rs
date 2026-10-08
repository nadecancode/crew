//! Real Core and native dispatcher composition with a synthetic IO peer. The
//! peer deliberately supplies PTY bytes before the Attach reply to exercise
//! buffering and the identified success notice's publication order.
#![cfg(all(feature = "runtime", feature = "real-core-tests"))]

use std::future::Future;
use std::pin::Pin;
use std::sync::{mpsc as sync, Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use crew_client::api::*;
use crew_client::policy::NATIVE_LIMITS;
use crew_protocol::rpc::{Push, RpcError, ServerFrame, Snapshot};
use crew_protocol::{AgentStatus, RuntimeKind};
use serde_json::{json, Value};
use tokio::sync::watch;

use crew_client_transport::queue::{ChannelSink, EventQueue};
use crew_client_transport::service::{ChannelHandle, Connector};
use crew_client_transport::{
    ClientObserver, DriverConfig, HostEffect, HostEvent, NativeSession, Protocol6Codec,
    SendDisposition, TransportEnd,
};

const WAIT: Duration = Duration::from_secs(5);

fn fixture() -> Snapshot {
    let results: Value =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/results.json")).unwrap();
    let mut snapshot: Snapshot = serde_json::from_value(results["snapshot"][0].clone()).unwrap();
    snapshot.session.daemon_pid = 77;
    snapshot.session.daemon_started_at = 7_777;
    let mut agent = snapshot.agents[0].clone();
    agent.id = "claude-1".into();
    agent.status = AgentStatus::Working;
    agent.runtime = RuntimeKind::Claude;
    agent.backend = "herdr".into();
    agent.limit = None;
    agent.paused = false;
    let mut second = agent.clone();
    second.id = "claude-2".into();
    snapshot.agents = vec![agent, second];
    snapshot
}

fn context(attempt: u64) -> ConnectionContext {
    let scope = ClientScope {
        endpoint_id: "fixture-host".into(),
        workspace_id: fixture().session.workspace,
        generation: None,
        daemon_pid: Some(77),
        daemon_started_at_ms: Some(7_777),
        attempt,
    };
    ConnectionContext {
        mode: ConnectionMode::LocalOperator,
        snapshot_mode: SnapshotMode::OrderedConnection,
        grant: Grant {
            grant_id: "fixture-grant".into(),
            tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
            workspace_ids: vec![scope.workspace_id.clone()],
            expires_at_ms: None,
        },
        scope,
    }
}

fn core() -> crew_client::ClientCore {
    crew_client::ClientCore::new(ClientConfig {
        limits: NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap()
}

#[test]
fn real_core_attach_completion_identifies_attachment_in_exactly_one_final_outcome() {
    let mut core = core();
    let context = context(1);
    let clock = ClockSample {
        wall_ms: 1_760_000_000_000,
        monotonic_ms: 1,
    };
    let bound = core
        .apply(
            ClientInput::Bind {
                scope: context.scope.clone(),
                mode: context.mode,
                grant: context.grant,
                snapshot_mode: context.snapshot_mode,
            },
            clock,
        )
        .unwrap();
    assert!(bound.outcomes.is_empty());
    core.apply(
        ClientInput::SnapshotStarted {
            scope: context.scope.clone(),
            operation_id: 1 << 63,
        },
        clock,
    )
    .unwrap();
    let loaded = core
        .apply(
            ClientInput::Snapshot {
                scope: context.scope.clone(),
                operation_id: 1 << 63,
                snapshot: Box::new(fixture()),
            },
            clock,
        )
        .unwrap();
    assert!(loaded.outcomes.is_empty());
    let planned = core
        .plan(
            ActionIntent::Attach {
                scope: context.scope.clone(),
                agent: "claude-1".into(),
                rows: 24,
                cols: 80,
            },
            clock,
        )
        .unwrap();
    let ActionPlan::Dispatch { operation_id } = planned.plan else {
        panic!("attach dispatch");
    };
    assert!(planned.update.outcomes.is_empty());
    let input = ClientInput::OperationFinished {
        scope: context.scope.clone(),
        operation_id,
        outcome: OperationOutcome::Succeeded {
            result: CommandResult::Attached {
                agent: "claude-1".into(),
            },
        },
    };
    let completed = core.apply(input.clone(), clock).unwrap();
    assert_eq!(completed.outcomes.len(), 1);
    let notice = &completed.outcomes[0];
    assert_eq!(notice.scope, context.scope);
    assert_eq!(notice.operation_id, operation_id);
    assert_eq!(notice.attachment_id, Some(operation_id));
    assert_eq!(notice.kind, OperationOutcomeKind::Success);
    assert!(core.apply(input, clock).unwrap().outcomes.is_empty());
}

struct Peer {
    snapshot: Snapshot,
    behavior: Behavior,
    sinks: Arc<Mutex<Vec<ChannelSink>>>,
}
#[derive(Clone, Copy)]
enum Behavior {
    Output,
    NoOutput,
    Reject,
}
struct Channel {
    id: u64,
    sink: ChannelSink,
    snapshot: Snapshot,
    finished: watch::Sender<bool>,
    behavior: Behavior,
}

impl Channel {
    fn frame(&self, frame: ServerFrame) -> Result<(), TransportEnd> {
        self.sink.try_send(HostEvent::Frame {
            channel_id: self.id,
            text: serde_json::to_string(&frame).unwrap(),
        })
    }
    fn bytes(&self, text: &str) -> Result<(), TransportEnd> {
        self.frame(ServerFrame::push(Push::Pty {
            data: base64::engine::general_purpose::STANDARD.encode(text),
        }))
    }
}

impl ChannelHandle for Channel {
    fn send(&self, id: u64, text: String) -> Result<(), TransportEnd> {
        for disposition in [SendDisposition::WriteStarted, SendDisposition::FullyWritten] {
            self.sink.try_send(HostEvent::SendProgress {
                channel_id: self.id,
                request_id: id,
                disposition,
            })?;
        }
        let request: Value = serde_json::from_str(&text).unwrap();
        match request["method"].as_str().unwrap() {
            "features_negotiate" => self.frame(ServerFrame::err(
                id,
                crew_protocol::rpc::RpcError {
                    code: "invalid".into(),
                    message: "old fixture daemon".into(),
                    data: None,
                },
            )),
            "subscribe" => self.frame(ServerFrame::ok(id, json!({"subscribed":true}))),
            "snapshot" => self.frame(ServerFrame::ok(
                id,
                serde_json::to_value(&self.snapshot).unwrap(),
            )),
            "attach" => {
                let agent = request["params"]["agent"].as_str().unwrap();
                if matches!(self.behavior, Behavior::Reject) {
                    return self.frame(ServerFrame::err(
                        id,
                        RpcError::new("not_attached", "fixture attach refused"),
                    ));
                }
                if matches!(self.behavior, Behavior::Output) {
                    self.bytes(&format!("{agent}:before"))?;
                }
                self.frame(ServerFrame::ok(id, json!({"attached":agent})))?;
                if matches!(self.behavior, Behavior::Output) {
                    self.bytes(&format!("{agent}:after"))
                } else {
                    Ok(())
                }
            }
            _ => Err(TransportEnd::Malformed),
        }
    }
    fn close(&self) {
        self.finished.send_replace(true);
        self.sink.end(TransportEnd::Stopped);
    }
    fn fail(&self, reason: TransportEnd) {
        self.sink.end(reason);
        self.close();
    }
    fn wait_closed(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

impl Connector for Peer {
    fn open(
        &self,
        effect: &HostEffect,
        events: &EventQueue,
    ) -> Result<Box<dyn ChannelHandle>, TransportEnd> {
        let HostEffect::Open {
            channel_id,
            attempt,
            ..
        } = effect
        else {
            return Err(TransportEnd::Permission);
        };
        let sink = events.register(*channel_id)?;
        self.sinks.lock().unwrap().push(sink.clone());
        let context = context(*attempt);
        sink.try_send(HostEvent::WorkspaceOpened {
            channel_id: *channel_id,
            context,
            peer: PeerInfo {
                version: "fixture".into(),
                protocol: 6,
                pid: 77,
                project: None,
            },
        })?;
        let (finished, _) = watch::channel(false);
        Ok(Box::new(Channel {
            id: *channel_id,
            sink,
            snapshot: self.snapshot.clone(),
            finished,
            behavior: self.behavior,
        }))
    }
}

#[derive(Debug)]
enum Callback {
    Notice(ClientNotice),
    Chunk(TerminalChunk),
}
struct Observer(sync::Sender<Callback>);
impl ClientObserver for Observer {
    fn changed(&self, notice: ClientNotice) {
        let _ = self.0.send(Callback::Notice(notice));
    }
    fn terminal(&self, chunk: TerminalChunk) {
        let _ = self.0.send(Callback::Chunk(chunk));
    }
}

#[test]
fn native_session_identified_attach_notice_precedes_first_buffered_and_live_chunks_for_two_attachments(
) {
    let session = NativeSession::with_connector(
        core(),
        Protocol6Codec,
        Peer {
            snapshot: fixture(),
            behavior: Behavior::Output,
            sinks: Default::default(),
        },
        DriverConfig::default(),
    )
    .unwrap();
    let (sender, receiver) = sync::channel();
    session.observe(Arc::new(Observer(sender))).unwrap();
    session
        .connect(
            "fixture-profile".into(),
            "fixture-host".into(),
            Some(context(1).scope.workspace_id),
        )
        .unwrap();
    let scope = loop {
        if let Callback::Notice(notice) = receiver.recv_timeout(WAIT).unwrap() {
            if notice.workspace == WorkspaceStatus::Live {
                break notice.scope.unwrap();
            }
        }
    };
    let mut ids = Vec::new();
    for agent in ["claude-1", "claude-2"] {
        let plan = session
            .plan(ActionIntent::Attach {
                scope: scope.clone(),
                agent: agent.into(),
                rows: 24,
                cols: 80,
            })
            .unwrap();
        let ActionPlan::Dispatch { operation_id } = plan else {
            panic!("attach dispatch");
        };
        ids.push(operation_id);
    }
    assert_ne!(ids[0], ids[1]);
    let mut noticed = std::collections::BTreeMap::new();
    let mut received = std::collections::BTreeMap::<u64, Vec<u8>>::new();
    while received.values().map(Vec::len).sum::<usize>()
        < "claude-1:beforeclaude-1:afterclaude-2:beforeclaude-2:after".len()
    {
        match receiver.recv_timeout(WAIT).unwrap() {
            Callback::Notice(notice) => {
                for outcome in notice.outcomes {
                    if ids.contains(&outcome.operation_id) {
                        assert_eq!(outcome.kind, OperationOutcomeKind::Success);
                        assert_eq!(outcome.attachment_id, Some(outcome.operation_id));
                        assert_eq!(outcome.scope, scope);
                        assert!(
                            noticed
                                .insert(outcome.operation_id, outcome.scope)
                                .is_none(),
                            "single final notice"
                        );
                    }
                }
            }
            Callback::Chunk(chunk) => {
                assert_eq!(chunk.scope, scope);
                assert!(ids.contains(&chunk.attachment_id));
                assert_eq!(
                    noticed.get(&chunk.attachment_id),
                    Some(&scope),
                    "identified notice must precede bytes"
                );
                received
                    .entry(chunk.attachment_id)
                    .or_default()
                    .extend(chunk.data);
            }
        }
    }
    assert_eq!(received[&ids[0]], b"claude-1:beforeclaude-1:after");
    assert_eq!(received[&ids[1]], b"claude-2:beforeclaude-2:after");
    assert_eq!(noticed.len(), 2);
    session.close().unwrap();
}

fn boot(
    behavior: Behavior,
) -> (
    NativeSession,
    sync::Receiver<Callback>,
    ClientScope,
    Arc<Mutex<Vec<ChannelSink>>>,
) {
    let sinks = Arc::new(Mutex::new(Vec::new()));
    let session = NativeSession::with_connector(
        core(),
        Protocol6Codec,
        Peer {
            snapshot: fixture(),
            behavior,
            sinks: sinks.clone(),
        },
        DriverConfig::default(),
    )
    .unwrap();
    let (sender, receiver) = sync::channel();
    session.observe(Arc::new(Observer(sender))).unwrap();
    session
        .connect(
            "fixture-profile".into(),
            "fixture-host".into(),
            Some(context(1).scope.workspace_id),
        )
        .unwrap();
    let scope = live(&receiver);
    (session, receiver, scope, sinks)
}

fn live(receiver: &sync::Receiver<Callback>) -> ClientScope {
    loop {
        if let Callback::Notice(notice) = receiver.recv_timeout(WAIT).unwrap() {
            if notice.workspace == WorkspaceStatus::Live {
                return notice.scope.unwrap();
            }
        }
    }
}

fn completion(receiver: &sync::Receiver<Callback>, id: u64) -> OperationNotice {
    loop {
        match receiver.recv_timeout(WAIT).unwrap() {
            Callback::Notice(notice) => {
                if let Some(outcome) = notice
                    .outcomes
                    .into_iter()
                    .find(|outcome| outcome.operation_id == id)
                {
                    return outcome;
                }
            }
            Callback::Chunk(chunk) => {
                assert!(chunk.data.is_empty(), "no output before identified success")
            }
        }
    }
}

fn attach(session: &NativeSession, scope: &ClientScope) -> u64 {
    match session
        .plan(ActionIntent::Attach {
            scope: scope.clone(),
            agent: "claude-1".into(),
            rows: 24,
            cols: 80,
        })
        .unwrap()
    {
        ActionPlan::Dispatch { operation_id } => operation_id,
        other => panic!("attach: {other:?}"),
    }
}

#[test]
fn no_output_attach_still_identifies_success_and_local_detach_and_stale_scope_do_not_reopen_it() {
    let (session, receiver, scope, sinks) = boot(Behavior::NoOutput);
    let id = attach(&session, &scope);
    let success = completion(&receiver, id);
    assert_eq!(success.kind, OperationOutcomeKind::Success);
    assert_eq!(success.attachment_id, Some(id));
    let ActionPlan::Dispatch {
        operation_id: detach,
    } = session
        .plan(ActionIntent::Detach {
            scope: scope.clone(),
            attachment_id: id,
        })
        .unwrap()
    else {
        panic!("detach");
    };
    assert_ne!(detach, id);
    let closed = completion(&receiver, detach);
    assert_eq!(closed.kind, OperationOutcomeKind::ClosedLocally);
    assert_eq!(closed.attachment_id, Some(id));
    let terminal = loop {
        if let Callback::Chunk(chunk) = receiver.recv_timeout(WAIT).unwrap() {
            break chunk;
        }
    };
    assert_eq!(terminal.scope, scope);
    assert_eq!(terminal.attachment_id, id);
    assert_eq!(terminal.ended, Some(TerminalEnd::Detach));
    assert!(matches!(
        session
            .plan(ActionIntent::Detach {
                scope: scope.clone(),
                attachment_id: id
            })
            .unwrap(),
        ActionPlan::Unavailable { .. }
    ));
    session.reconnect().unwrap();
    let newer = loop {
        let candidate = live(&receiver);
        if candidate.attempt > scope.attempt {
            break candidate;
        }
    };
    assert!(newer.attempt > scope.attempt);
    assert!(matches!(
        session
            .plan(ActionIntent::AttachInput {
                scope: scope.clone(),
                attachment_id: id,
                data: b"stale".to_vec()
            })
            .unwrap(),
        ActionPlan::Unavailable { .. }
    ));
    let old = sinks.lock().unwrap()[1].clone();
    assert!(old
        .try_send(HostEvent::Frame {
            channel_id: 2,
            text: serde_json::to_string(&ServerFrame::push(Push::Pty {
                data: base64::engine::general_purpose::STANDARD.encode(b"stale bytes")
            }))
            .unwrap()
        })
        .is_err());
    assert_eq!(session.connection_context().unwrap().unwrap().scope, newer);
    session.close().unwrap();
}

#[test]
fn failed_attach_never_publishes_success_identity_and_closes_its_provisional_attachment() {
    let (session, receiver, scope, _) = boot(Behavior::Reject);
    let id = attach(&session, &scope);
    let rejected = completion(&receiver, id);
    assert_eq!(rejected.kind, OperationOutcomeKind::Rejected);
    assert_eq!(rejected.attachment_id, None);
    let terminal = loop {
        match receiver.recv_timeout(WAIT).unwrap() {
            Callback::Chunk(chunk) => break chunk,
            Callback::Notice(notice) => assert!(!notice
                .outcomes
                .iter()
                .any(|outcome| outcome.operation_id == id
                    && outcome.kind == OperationOutcomeKind::Success)),
        }
    };
    assert_eq!(terminal.attachment_id, id);
    assert_eq!(terminal.scope, scope);
    assert!(terminal.data.is_empty());
    assert!(terminal.ended.is_some());
    assert!(matches!(
        session
            .plan(ActionIntent::AttachInput {
                scope,
                attachment_id: id,
                data: b"no replay".to_vec()
            })
            .unwrap(),
        ActionPlan::Unavailable { .. }
    ));
    session.close().unwrap();
}
