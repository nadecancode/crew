//! Construction and serialization checks for the shared record boundary.
//! No fixture engine implements or pretends to execute an operator action.

use std::fmt::Debug;

use crew_client::*;
use crew_protocol::rpc::*;
use crew_protocol::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: T) {
    let encoded = serde_json::to_string(&value).unwrap();
    let decoded: T = serde_json::from_str(&encoded).unwrap();
    assert_eq!(value, decoded);
}

fn snapshot() -> Snapshot {
    serde_json::from_str(include_str!("../../crew/tests/fixtures/web/snapshot.json")).unwrap()
}

fn scope() -> ClientScope {
    ClientScope {
        endpoint_id: "local:profile-a".into(),
        workspace_id: "946d4617aa702870".into(),
        generation: None,
        daemon_pid: None,
        daemon_started_at_ms: None,
        attempt: u64::MAX,
    }
}

fn reason() -> ClientReason {
    ClientReason {
        details: None,
        code: "closed".into(),
        message: "connection closed".into(),
    }
}

fn entity(entity: Entity) -> EntityKey {
    EntityKey {
        scope: scope(),
        entity,
    }
}

fn key(kind: ResourceKind, options: ReadOptions) -> ResourceKey {
    ResourceKey {
        entity: entity(Entity::Agent("claude-1".into())),
        kind,
        options,
    }
}

fn resource() -> ResourceState {
    ResourceState {
        key: key(
            ResourceKind::Transcript,
            ReadOptions::Bounded { limit: 400 },
        ),
        status: ResourceStatus::Ready,
        request_sequence: u64::MAX,
        error: None,
        value: Some(ResourceValue::Transcript(TranscriptResult {
            entries: vec![],
            available: true,
            truncated: false,
        })),
    }
}

fn messages() -> MessagePage {
    MessagePage {
        items: snapshot().messages,
        before: Some(HistoryCursor {
            scope: scope(),
            revision: u64::MAX,
            anchor: HistoryAnchor::Message {
                id: "m-0104".into(),
                timestamp_ms: i64::MIN,
                ordinal: u64::MAX,
            },
        }),
        has_older_local: true,
        may_have_older_remote: true,
    }
}

fn task() -> TaskPresentation {
    TaskPresentation {
        task: snapshot().tasks.remove(0),
        dependencies: vec![DependencyState {
            id: "missing".into(),
            status: None,
        }],
        layer: 0,
        in_cycle: false,
    }
}

fn agent() -> AgentPresentation {
    AgentPresentation {
        agent: snapshot().agents.remove(0),
        status_label: "working".into(),
        tone: Tone::Info,
        current_work: Some(CurrentWork {
            task_id: "billing".into(),
            done: false,
            elapsed_ms: Some(0),
        }),
        waiting_task_ids: vec!["parked".into()],
        satellite_parent_id: None,
        subagent_ids: vec![],
        warnings: vec![WarningFact {
            code: "context".into(),
            text: "Context reported".into(),
            entity: Some(entity(Entity::Agent("claude-1".into()))),
        }],
        actions: vec![ActionAvailability {
            action: ActionKind::Pause,
            available: false,
            reason: Some(reason()),
        }],
    }
}

fn changes() -> ChangeSet {
    ChangeSet {
        revision: u64::MAX,
        agents: vec!["claude-1".into()],
        removed: vec![entity(Entity::Task("billing".into()))],
        reset: true,
        ..ChangeSet::default()
    }
}

#[test]
fn missing_identity_and_zero_remain_distinct_at_integer_boundaries() {
    let absent = scope();
    let encoded = serde_json::to_value(&absent).unwrap();
    assert_eq!(encoded["generation"], Value::Null);
    assert_eq!(encoded["daemon_pid"], Value::Null);
    assert_eq!(encoded["daemon_started_at_ms"], Value::Null);
    assert_eq!(encoded["attempt"], json!(u64::MAX));
    round_trip(absent.clone());
    let known = ClientScope {
        generation: Some(0),
        daemon_pid: Some(0),
        daemon_started_at_ms: Some(i64::MIN),
        ..absent.clone()
    };
    assert_ne!(absent, known);
    round_trip(known);
    round_trip(ClockSample {
        wall_ms: i64::MIN,
        monotonic_ms: u64::MAX,
    });
    for invalid in [json!(-1), json!("1"), json!(1.5)] {
        let mut value = encoded.clone();
        value["attempt"] = invalid;
        assert!(serde_json::from_value::<ClientScope>(value).is_err());
    }
    round_trip(CurrentWork {
        task_id: "billing".into(),
        done: false,
        elapsed_ms: None,
    });
    round_trip(CurrentWork {
        task_id: "billing".into(),
        done: true,
        elapsed_ms: Some(0),
    });
}

#[test]
fn every_query_and_payload_constructs_with_the_actual_protocol_records() {
    let message_filter = MessageFilter {
        endpoints: MessageEndpoints::Pair {
            first: "claude-1".into(),
            second: "claude-2".into(),
        },
        kind: Some(MessageType::Message),
        text: "résumé\n账目".into(),
    };
    let event_filter = EventFilter {
        agent_id: None,
        level: Some(EventLevel::Warn),
        text: "bus".into(),
        vcs_only: true,
    };
    for query in [
        ViewQuery::Availability {
            intent: Box::new(ActionIntent::StopTeam { scope: scope() }),
        },
        ViewQuery::Overview,
        ViewQuery::Team,
        ViewQuery::Tasks,
        ViewQuery::AgentDetail {
            id: "claude-1".into(),
        },
        ViewQuery::TaskDetail {
            id: "billing".into(),
        },
        ViewQuery::Messages {
            before: messages().before,
            limit: 200,
            filter: message_filter,
        },
        ViewQuery::Events {
            before: Some(HistoryCursor {
                scope: scope(),
                revision: 0,
                anchor: HistoryAnchor::Event {
                    seq: 0,
                    ordinal: u64::MAX,
                },
            }),
            limit: 1,
            filter: event_filter,
        },
        ViewQuery::Usage,
        ViewQuery::Questions,
        ViewQuery::Evidence {
            key: resource().key,
        },
    ] {
        round_trip(query);
    }
    let data = snapshot();
    let usage = UsagePresentation {
        total: Some(Usage::default()),
        reporters: 1,
        eligible: 2,
        overflowed: false,
        agents: vec![AgentUsage {
            agent_id: "claude-1".into(),
            usage: None,
            context: None,
            samples: vec![ContextPoint {
                at_ms: i64::MIN,
                percent: 0,
            }],
            compactions: 0,
        }],
        groups: vec![RuntimeQuotaGroup {
            runtime: RuntimeKind::Claude,
            quotas: vec![QuotaRow {
                quota: Quota {
                    runtime: RuntimeKind::Claude,
                    window: "5h".into(),
                    percent_used: 0.0,
                    resets_at: None,
                    updated_at: 0,
                },
                stale: true,
            }],
            paused_agent_ids: vec![],
        }],
    };
    for payload in [
        ViewPayload::Availability(ActionAvailability {
            action: ActionKind::StopTeam,
            available: false,
            reason: Some(reason()),
        }),
        ViewPayload::Overview(Box::new(OverviewData {
            session: Some(data.session),
            agent_count: 7,
            working_count: 1,
            task_count: 6,
            done_count: 5,
            question_count: 0,
            warnings: vec![],
        })),
        ViewPayload::Team(Box::new(TeamData {
            agents: vec![agent()],
            relations: vec![AgentRelation {
                parent_id: "orchestrator".into(),
                child_id: "claude-1".into(),
            }],
        })),
        ViewPayload::Tasks(Box::new(TasksData {
            epics: None,
            tasks: vec![task()],
            layers: vec![TaskLayer {
                index: 0,
                task_ids: vec!["billing".into()],
            }],
        })),
        ViewPayload::AgentDetail(Box::new(AgentDetailData {
            agent: Some(agent()),
            assigned_tasks: vec![task()],
            review_tasks: vec![],
            recent_messages: messages(),
            resources: vec![resource()],
        })),
        ViewPayload::TaskDetail(Box::new(TaskDetailData {
            task: Some(task()),
            card: resource(),
        })),
        ViewPayload::Messages(Box::new(messages())),
        ViewPayload::Events(Box::new(EventPage {
            items: data.events,
            before: None,
            has_older_local: false,
            may_have_older_remote: true,
        })),
        ViewPayload::Usage(Box::new(usage)),
        ViewPayload::Questions(data.questions),
        ViewPayload::Evidence(Box::new(resource())),
    ] {
        round_trip(ViewData {
            scope: Some(scope()),
            revision: u64::MAX,
            synchronized_at_ms: None,
            freshness: Freshness::Stale,
            payload,
        });
    }
    for endpoints in [
        MessageEndpoints::All,
        MessageEndpoints::Agent {
            id: "claude-1".into(),
        },
    ] {
        round_trip(endpoints);
    }
    for value in [
        Entity::Workspace,
        Entity::Agent("a".into()),
        Entity::Task("t".into()),
        Entity::Message("m-1".into()),
        Entity::Event(u64::MAX),
    ] {
        round_trip(entity(value));
    }
}

#[test]
fn resources_preserve_options_and_protocol_metadata_values() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/results.json")).unwrap();
    let mut message = snapshot().messages.remove(0);
    message.metadata = json!({"unicode":"λ你好", "nested":[null,{},0,u64::MAX], "empty":{}});
    let mut event = snapshot().events.remove(0);
    event.data = json!([null, {"z":false,"a":"résumé"}, i64::MIN]);
    for value in [
        ResourceValue::TaskCard(serde_json::from_value(fixture["task_get"][0].clone()).unwrap()),
        ResourceValue::Message(Some(message.clone())),
        ResourceValue::Message(None),
        ResourceValue::Event(Some(event.clone())),
        ResourceValue::Event(None),
        ResourceValue::Transcript(TranscriptResult::default()),
        ResourceValue::Diff(GitOutcome::default()),
        ResourceValue::Output(OutputResult::default()),
        ResourceValue::Inbox(InboxResult::default()),
        ResourceValue::Models(ModelsResult::default()),
        ResourceValue::History(vec![message]),
        ResourceValue::Events(vec![event]),
    ] {
        round_trip(value);
    }
    let first = key(
        ResourceKind::Transcript,
        ReadOptions::Bounded { limit: 400 },
    );
    let second = key(
        ResourceKind::Transcript,
        ReadOptions::Bounded { limit: 200 },
    );
    assert_ne!(first, second);
    for options in [
        ReadOptions::Default,
        ReadOptions::Bounded { limit: u32::MAX },
        ReadOptions::Output {
            lines: 300,
            screen: true,
            ansi: false,
        },
        ReadOptions::Diff {
            against: Some("main".into()),
        },
    ] {
        round_trip(options);
    }
    for status in [
        ResourceStatus::Idle,
        ResourceStatus::Loading,
        ResourceStatus::Ready,
        ResourceStatus::Empty,
        ResourceStatus::Unsupported,
        ResourceStatus::Failed,
        ResourceStatus::Stale,
    ] {
        round_trip(status);
    }
    round_trip(resource());
}

#[test]
fn scoped_intents_have_no_credential_or_unscoped_variant() {
    let intents = [
        ActionIntent::SendMessage {
            scope: scope(),
            to: "claude-1".into(),
            body: "  résumé\n ".into(),
            message_id: "w-0102030405060708".into(),
        },
        ActionIntent::Answer {
            scope: scope(),
            question_id: "m-9001".into(),
            body: "Use the adapter".into(),
        },
        ActionIntent::Agent {
            scope: scope(),
            id: "claude-1".into(),
            action: AgentAction::Kill,
        },
        ActionIntent::SetModel {
            scope: scope(),
            id: "claude-1".into(),
            runtime: None,
            model: "".into(),
            note: None,
        },
        ActionIntent::AgentInput {
            scope: scope(),
            id: "claude-1".into(),
            text: None,
            submit: false,
        },
        ActionIntent::ResumeLimited {
            scope: scope(),
            restart: true,
        },
        ActionIntent::StopTeam { scope: scope() },
        ActionIntent::Read {
            scope: scope(),
            resource: ResourceRequest {
                key: resource().key,
            },
        },
        ActionIntent::Attach {
            scope: scope(),
            agent: "claude-1".into(),
            rows: 5,
            cols: 20,
        },
        ActionIntent::AttachInput {
            scope: scope(),
            attachment_id: u64::MAX,
            data: vec![0, 27, 128, 255],
        },
        ActionIntent::AttachResize {
            scope: scope(),
            attachment_id: u64::MAX,
            rows: 500,
            cols: 1000,
        },
        ActionIntent::Detach {
            scope: scope(),
            attachment_id: u64::MAX,
        },
    ];
    for intent in intents {
        let mut value = serde_json::to_value(&intent).unwrap();
        assert_eq!(
            value["data"]["scope"],
            serde_json::to_value(scope()).unwrap()
        );
        value["data"].as_object_mut().unwrap().remove("scope");
        assert!(serde_json::from_value::<ActionIntent>(value).is_err());
        round_trip(intent);
    }
    for action in [
        AgentAction::Pause,
        AgentAction::Resume,
        AgentAction::Compact,
        AgentAction::Restart,
        AgentAction::RestartFresh,
        AgentAction::Kill,
        AgentAction::Focus,
    ] {
        round_trip(action);
    }
}

#[test]
fn input_outcome_notice_and_terminal_records_construct_without_an_engine() {
    let grant = Grant {
        grant_id: "opaque-grant-generation".into(),
        tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
        workspace_ids: vec![scope().workspace_id],
        expires_at_ms: None,
    };
    round_trip(ConnectionContext {
        mode: ConnectionMode::LocalOperator,
        grant: grant.clone(),
        snapshot_mode: SnapshotMode::OrderedConnection,
        scope: scope(),
    });
    round_trip(PeerInfo {
        version: "0.1.0".into(),
        protocol: 6,
        pid: 1,
        project: None,
    });
    round_trip(ClientConfig {
        limits: ClientLimits {
            messages: 1000,
            events: 5000,
            context_points: 600,
            buffered_pushes: 256,
            pending_operations: 64,
            page_size: 200,
        },
        snapshot_mode: SnapshotMode::BufferedOverlap,
    });
    let update = ClientUpdate {
        revision: 1,
        changes: changes(),
        effects: vec![
            ClientEffect::Request {
                scope: scope(),
                operation_id: u64::MAX,
                lane: RequestLane::Action,
                command: Command::AgentPause {
                    id: "claude-1".into(),
                },
            },
            ClientEffect::Schedule {
                timer_id: 0,
                deadline_ms: u64::MAX,
            },
            ClientEffect::CancelTimer { timer_id: 0 },
            ClientEffect::Resynchronize { scope: scope() },
            ClientEffect::Notice {
                code: "changed".into(),
                entity: None,
            },
        ],
        outcomes: vec![],
    };
    round_trip(update.clone());
    for plan in [
        ActionPlan::Unavailable { reason: reason() },
        ActionPlan::Confirm {
            confirmation_id: u64::MAX,
            kind: ConfirmationKind::KillAgent,
            scope: scope(),
            target_ids: vec!["claude-1".into()],
        },
        ActionPlan::Dispatch {
            operation_id: u64::MAX,
        },
    ] {
        round_trip(PlanUpdate {
            plan,
            update: update.clone(),
        });
    }
    for outcome in [
        OperationOutcome::Succeeded {
            result: CommandResult::Pong,
        },
        OperationOutcome::Rejected {
            details: None,
            code: "forbidden".into(),
            message: "Denied".into(),
        },
        OperationOutcome::NotSent { reason: reason() },
        OperationOutcome::Canceled,
        OperationOutcome::OutcomeUnknown { reason: reason() },
    ] {
        round_trip(outcome);
    }
    for input in [
        ClientInput::Bind {
            scope: scope(),
            mode: ConnectionMode::ScopedGateway,
            grant,
            snapshot_mode: SnapshotMode::OrderedConnection,
        },
        ClientInput::Unbind {
            scope: scope(),
            reason: reason(),
        },
        ClientInput::EndpointChanged {
            endpoint_id: "local:profile-a".into(),
            attempt: u64::MAX,
            status: EndpointStatus::Authenticated,
        },
        ClientInput::Listing {
            endpoint_id: "local:profile-a".into(),
            attempt: u64::MAX,
            listing: Workspaces::default(),
        },
        ClientInput::SnapshotStarted {
            scope: scope(),
            operation_id: u64::MAX,
        },
        ClientInput::Snapshot {
            scope: scope(),
            operation_id: u64::MAX,
            snapshot: Box::new(snapshot()),
        },
        ClientInput::Push {
            scope: scope(),
            push: Push::Resync,
        },
        ClientInput::OperationFinished {
            scope: scope(),
            operation_id: u64::MAX,
            outcome: OperationOutcome::OutcomeUnknown { reason: reason() },
        },
        ClientInput::Wake {
            timer_id: 0,
            monotonic_ms: u64::MAX,
        },
        ClientInput::Lifecycle { active: false },
    ] {
        round_trip(input);
    }
    round_trip(ClientNotice {
        scope: None,
        revision: u64::MAX,
        changes: changes(),
        endpoint: EndpointStatus::Reconnecting,
        workspace: WorkspaceStatus::AccessRevoked,
        reason: Some(reason()),
        outcomes: vec![OperationNotice {
            scope: scope(),
            operation_id: u64::MAX,
            attachment_id: None,
            kind: OperationOutcomeKind::Unknown,
            reason: Some(reason()),
        }],
    });
    for ended in [
        TerminalEnd::Normal,
        TerminalEnd::Detach,
        TerminalEnd::Disconnect,
        TerminalEnd::Revocation,
        TerminalEnd::Overflow,
        TerminalEnd::Unsupported,
        TerminalEnd::Failed { reason: reason() },
    ] {
        round_trip(TerminalChunk {
            scope: scope(),
            attachment_id: u64::MAX,
            data: vec![0, 27, 128, 255],
            ended: Some(ended),
        });
    }
}

#[test]
fn finite_client_errors_and_object_safe_trait_signatures_are_available() {
    for error in [
        ClientError::InvalidInput {
            field: "limit".into(),
            reason: "outside range".into(),
        },
        ClientError::DecodeFailure {
            message: "missing body".into(),
        },
        ClientError::ProtocolMismatch {
            expected: 6,
            actual: None,
            version: None,
        },
        ClientError::TooLarge {
            actual: u64::MAX,
            limit: 65536,
        },
        ClientError::CursorExpired,
        ClientError::StaleConfirmation,
        ClientError::ClosedClient,
        ClientError::InternalFailure {
            code: "poisoned".into(),
        },
    ] {
        assert!(!error.to_string().is_empty());
        round_trip(error);
    }
    let _: fn(&mut dyn ClientApi, ClientInput, ClockSample) -> Result<ClientUpdate, ClientError> =
        |api, input, clock| api.apply(input, clock);
    let _: fn(&dyn ClientApi, ViewQuery, ClockSample) -> Result<ViewData, ClientError> =
        |api, query, clock| api.view(query, clock);
    let _: fn(&mut dyn ClientApi, ActionIntent, ClockSample) -> Result<PlanUpdate, ClientError> =
        |api, intent, clock| api.plan(intent, clock);
    let _: fn(&mut dyn ClientApi, u64, ClockSample) -> Result<ClientUpdate, ClientError> =
        |api, id, clock| api.confirm(id, clock);
    let _: fn(&mut dyn ClientApi, u64) -> Result<ClientUpdate, ClientError> =
        |api, id| api.cancel_confirmation(id);
}

#[test]
fn terminal_agent_fixture_cases_use_real_terminal_states() {
    let text = include_str!("../../../fixtures/native/actions/controls.jsonl");
    for case in text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
    {
        if ["compact-terminal-agent", "resume-terminal-agent-skipped"]
            .contains(&case["id"].as_str().unwrap())
        {
            let status: AgentStatus =
                serde_json::from_value(case["given"]["agent_status"]["claude-2"].clone()).unwrap();
            assert!(status.is_terminal());
        }
    }
    assert!(!AgentStatus::Done.is_terminal());
}
