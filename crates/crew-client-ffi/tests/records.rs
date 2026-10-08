#![cfg(feature = "bindings")]

use crew_client::api as client;
use crew_client_ffi::service::LocalProfileInfo;
use crew_client_ffi::{records::*, UniFfiTag};
use crew_protocol::{rpc, types};
use serde_json::{json, Value};
use uniffi::FfiConverter;

fn roundtrip<T: FfiConverter<UniFfiTag>>(value: T) -> T {
    let mut bytes = Vec::new();
    T::write(value, &mut bytes);
    let mut input = bytes.as_slice();
    let value = T::try_read(&mut input).expect("valid generated record buffer");
    assert!(
        input.is_empty(),
        "converter must consume the complete record"
    );
    value
}

fn scope(attempt: u64) -> ClientScope {
    ClientScope {
        endpoint_id: "host-α".into(),
        workspace_id: "0123456789abcdef".into(),
        generation: Some(u64::MAX),
        daemon_pid: Some(u32::MAX),
        daemon_started_at_ms: Some(i64::MIN),
        attempt,
    }
}

fn message(metadata: Value) -> types::Message {
    types::Message {
        id: "m-wide".into(),
        from: "orchestrator".into(),
        to: "operator".into(),
        kind: MessageType::Question,
        body: "測定 🦀 e\u{301}\n\0 preserved".into(),
        timestamp: i64::MIN,
        correlation_id: None,
        reply_to: Some(String::new()),
        metadata,
    }
}

#[test]
fn full_scope_and_entity_identity_survive_ffi_without_narrowing() {
    let value = scope(u64::MAX);
    assert_eq!(roundtrip(value.clone()), value);
    let key = EntityKey {
        scope: value.clone(),
        entity: Entity::Event(u64::MAX),
    };
    assert_eq!(roundtrip(key.clone()), key);

    let absent = ClientScope {
        generation: None,
        daemon_pid: None,
        daemon_started_at_ms: None,
        ..value
    };
    assert_eq!(roundtrip(absent.clone()), absent);
    assert_ne!(roundtrip(scope(1)), roundtrip(scope(2)));
}

#[test]
fn metadata_preserves_null_empty_objects_unicode_and_integer_precision() {
    let none: Message = message(Value::Null).into();
    assert_eq!(roundtrip(none.clone()), none);
    assert!(none.metadata_json.is_none());

    let empty: Message = message(json!({})).into();
    assert_eq!(empty.metadata_json.as_deref(), Some("{}"));
    let raw = json!({"z": [null, true, u64::MAX, "測定 🦀"], "a": {"z": 2, "a": 1}});
    let converted: Message = message(raw.clone()).into();
    assert_eq!(roundtrip(converted.clone()), converted);
    let encoded = converted.metadata_json.as_ref().unwrap();
    assert!(encoded.starts_with("{\"a\":{\"a\":1,\"z\":2},\"z\":"));
    assert_eq!(serde_json::from_str::<Value>(encoded).unwrap(), raw);
    assert_eq!(converted.timestamp, i64::MIN);
    assert_eq!(converted.reply_to, Some(String::new()));
    assert_eq!(converted.correlation_id, None);
    assert_eq!(converted.body, message(Value::Null).body);
}

#[test]
fn question_facts_delegate_to_existing_protocol_semantics() {
    let source = message(json!({
        "asked_by": "worker-2",
        "options": ["Use A", null, 7, "Use B"],
    }));
    let expected = QuestionInfo {
        is_operator_question: source.is_operator_question(),
        asker: source.asker(),
        asked_by: source.asked_by().map(str::to_owned),
        options: source.options(),
    };
    let facts = message_question_info(source.into()).unwrap();
    assert_eq!(facts, expected);
    assert_eq!(facts.options, ["Use A", "Use B"]);
    assert_eq!(facts.asked_by.as_deref(), Some("worker-2"));
    assert_eq!(roundtrip(facts.clone()), facts);

    let mut malformed: Message = message(Value::Null).into();
    malformed.metadata_json = Some("not JSON: never echo this marker".into());
    let error = message_question_info(malformed).unwrap_err();
    assert!(!error.to_string().contains("never echo"));
    let mut oversized: Message = message(Value::Null).into();
    oversized.metadata_json = Some(" ".repeat(types::MAX_FRAME_BYTES + 1));
    assert!(matches!(
        message_question_info(oversized),
        Err(ClientError::TooLarge { .. })
    ));
}

#[test]
fn signed_event_sequence_and_host_sized_counts_keep_their_values() {
    let source = types::Event {
        seq: i64::MIN,
        timestamp: i64::MAX,
        source: "daemon".into(),
        event_type: "agent.error".into(),
        agent_id: None,
        task_id: Some("task-1".into()),
        message: "diagnostic".into(),
        level: EventLevel::Error,
        data: json!({"value": u64::MAX}),
    };
    let value: Event = source.into();
    assert_eq!(roundtrip(value.clone()), value);
    assert_eq!(value.seq, i64::MIN);

    let value: CommitInfo = rpc::CommitInfo {
        files_changed: usize::MAX,
        change_id: None,
        ..Default::default()
    }
    .into();
    assert_eq!(roundtrip(value.clone()), value);
    assert_eq!(value.files_changed, usize::MAX as u64);
}

#[test]
fn terminal_intent_and_notice_retain_attachment_separate_from_operation() {
    let intent = ActionIntent::AttachInput {
        scope: scope(17),
        attachment_id: 101,
        data: vec![0, 27, 255, b'\r'],
    };
    assert_eq!(roundtrip(intent.clone()), intent);
    let notice = OperationNotice {
        scope: scope(17),
        operation_id: 203,
        attachment_id: Some(101),
        kind: OperationOutcomeKind::ClosedLocally,
        reason: None,
    };
    assert_eq!(roundtrip(notice.clone()), notice);
    let unacknowledged = OperationNotice {
        operation_id: 204,
        kind: OperationOutcomeKind::SentUnacknowledged,
        ..notice.clone()
    };
    assert_eq!(roundtrip(unacknowledged.clone()), unacknowledged);
    assert_eq!(unacknowledged.attachment_id, Some(101));
    let ordinary = OperationNotice {
        attachment_id: None,
        kind: OperationOutcomeKind::Success,
        ..notice
    };
    assert_eq!(roundtrip(ordinary.clone()), ordinary);
    let chunk = TerminalChunk {
        scope: scope(17),
        attachment_id: 101,
        data: vec![0, 27, 255],
        ended: Some(TerminalEnd::Failed {
            reason: ClientReason {
                details: None,
                code: "closed".into(),
                message: "channel ended".into(),
            },
        }),
    };
    assert_eq!(roundtrip(chunk.clone()), chunk);
}

#[test]
fn generated_wire_fixtures_cross_the_real_record_converters() {
    for text in [
        include_str!("../../crew/tests/fixtures/web/snapshot.json"),
        include_str!("../../crew/tests/fixtures/web/team10.json"),
        include_str!("../../crew/tests/fixtures/web/subagents.json"),
    ] {
        let source: rpc::Snapshot = serde_json::from_str(text).unwrap();
        for agent in source.agents {
            assert_eq!(roundtrip(agent.clone()), agent);
        }
        for task in source.tasks {
            assert_eq!(roundtrip(task.clone()), task);
        }
        assert_eq!(roundtrip(source.session.clone()), source.session);
        for message in source.messages.into_iter().chain(source.questions) {
            let expected = message.clone();
            let converted: Message = roundtrip(Message::from(message));
            assert_eq!(converted.id, expected.id);
            assert_eq!(converted.body, expected.body);
            let metadata = converted
                .metadata_json
                .map(|text| serde_json::from_str::<Value>(&text).unwrap())
                .unwrap_or(Value::Null);
            assert_eq!(metadata, expected.metadata);
        }
        for event in source.events {
            let expected = event.clone();
            let converted: Event = roundtrip(Event::from(event));
            assert_eq!(converted.seq, expected.seq);
            assert_eq!(converted.message, expected.message);
            let data = converted
                .data_json
                .map(|text| serde_json::from_str::<Value>(&text).unwrap())
                .unwrap_or(Value::Null);
            assert_eq!(data, expected.data);
        }
    }
}

#[test]
fn boxed_view_payloads_become_typed_native_values() {
    let source = client::ViewData {
        scope: Some(scope(3)),
        revision: u64::MAX,
        synchronized_at_ms: None,
        freshness: Freshness::Stale,
        payload: client::ViewPayload::Messages(Box::new(client::MessagePage {
            items: vec![message(json!({"options": ["yes", "no"]}))],
            before: Some(HistoryCursor {
                scope: scope(3),
                revision: u64::MAX,
                anchor: HistoryAnchor::Message {
                    id: "m-wide".into(),
                    timestamp_ms: i64::MIN,
                    ordinal: u64::MAX,
                },
            }),
            has_older_local: false,
            may_have_older_remote: true,
        })),
    };
    let native: ViewData = source.into();
    assert_eq!(roundtrip(native.clone()), native);
    let ViewPayload::Messages(page) = native.payload else {
        panic!("typed page expected")
    };
    assert_eq!(page.items.len(), 1);
    assert!(page.may_have_older_remote);
    assert!(!page.has_older_local);
}

#[test]
fn malformed_ffi_records_fail_without_panicking() {
    let mut truncated: &[u8] = &[0, 0, 0];
    assert!(<ClientScope as FfiConverter<UniFfiTag>>::try_read(&mut truncated).is_err());
    let mut invalid_variant: &[u8] = &[0xff, 0xff, 0xff, 0xff];
    assert!(<Entity as FfiConverter<UniFfiTag>>::try_read(&mut invalid_variant).is_err());
}

#[test]
fn local_discovery_projection_has_only_picker_metadata() {
    for project in [None, Some(String::new()), Some("project-α".into())] {
        let source = LocalProfileInfo {
            profile_id: "opaque-instance-profile".into(),
            workspace: "/work/checkout".into(),
            project,
        };
        let decoded = roundtrip(source.clone());
        assert_eq!(decoded, source);
        // An added field makes this exhaustive pattern fail. Discovery PID,
        // start time, socket and credentials must not creep into this record.
        let LocalProfileInfo {
            profile_id,
            workspace,
            project,
        } = decoded;
        assert_eq!(profile_id, source.profile_id);
        assert_eq!(workspace, source.workspace);
        assert_eq!(project, source.project);
    }
}

fn fixture_core(scope: &ClientScope, tiers: Vec<GrantTier>) -> crew_client::ClientCore {
    let snapshot: rpc::Snapshot =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/snapshot.json")).unwrap();
    let clock = fixture_clock();
    let mut core = crew_client::ClientCore::new(ClientConfig {
        limits: crew_client::policy::NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap();
    for input in [
        client::ClientInput::Bind {
            scope: scope.clone(),
            mode: ConnectionMode::ScopedGateway,
            grant: Grant {
                grant_id: "device:grant".into(),
                tiers,
                workspace_ids: vec![scope.workspace_id.clone()],
                expires_at_ms: None,
            },
            snapshot_mode: SnapshotMode::OrderedConnection,
        },
        client::ClientInput::SnapshotStarted {
            scope: scope.clone(),
            operation_id: 1 << 63,
        },
        client::ClientInput::Snapshot {
            scope: scope.clone(),
            operation_id: 1 << 63,
            snapshot: Box::new(snapshot),
        },
    ] {
        core.apply(input, clock).unwrap();
    }
    core
}

fn fixture_clock() -> ClockSample {
    ClockSample {
        wall_ms: 1_760_000_000_000,
        monotonic_ms: 100,
    }
}

#[test]
fn availability_query_and_reasons_cross_the_real_converters() {
    for intent in [
        ActionIntent::SendMessage {
            scope: scope(7),
            to: "claude-1".into(),
            body: "測定 🦀 e\u{301}\n\0 preserved".into(),
            message_id: "m-stable".into(),
        },
        ActionIntent::AttachInput {
            scope: scope(u64::MAX),
            attachment_id: u64::MAX,
            data: vec![0, 27, 255, b'\r'],
        },
        ActionIntent::StopTeam { scope: scope(0) },
    ] {
        let query = ViewQuery::Availability {
            intent: Box::new(intent),
        };
        assert_eq!(roundtrip(query.clone()), query);
    }
    for reason in [
        None,
        Some(ClientReason {
            details: None,
            code: "stale_scope".into(),
            message: "測定 connection changed".into(),
        }),
    ] {
        let availability = ActionAvailability {
            action: ActionKind::AttachInput,
            available: reason.is_none(),
            reason,
        };
        let native: ViewData = client::ViewData {
            scope: Some(scope(0)),
            revision: u64::MAX,
            synchronized_at_ms: Some(i64::MIN),
            freshness: Freshness::Stale,
            payload: client::ViewPayload::Availability(availability.clone()),
        }
        .into();
        let native = roundtrip(native);
        assert_eq!(native.scope, Some(scope(0)));
        assert_eq!(native.revision, u64::MAX);
        assert_eq!(native.synchronized_at_ms, Some(i64::MIN));
        assert_eq!(native.freshness, Freshness::Stale);
        assert_eq!(native.payload, ViewPayload::Availability(availability));
    }
}

/// Every Core view crosses the native converters with its own variant. The
/// exhaustive matches stop compiling when the Core adds a query or payload, so
/// a new view cannot ship without a binding sample here.
#[test]
fn every_core_view_query_and_payload_keeps_its_variant_across_the_binding() {
    let captured = scope(1);
    let core = fixture_core(&captured, vec![GrantTier::Read, GrantTier::Control]);
    let evidence = ResourceKey {
        entity: EntityKey {
            scope: captured.clone(),
            entity: Entity::Agent("claude-1".into()),
        },
        kind: ResourceKind::Output,
        options: ReadOptions::Default,
    };
    let queries = [
        ViewQuery::Availability {
            intent: Box::new(ActionIntent::StopTeam {
                scope: captured.clone(),
            }),
        },
        ViewQuery::Overview,
        ViewQuery::Team,
        ViewQuery::Tasks,
        ViewQuery::AgentDetail {
            id: "claude-1".into(),
        },
        ViewQuery::TaskDetail { id: "db".into() },
        ViewQuery::Messages {
            before: None,
            limit: 200,
            filter: MessageFilter {
                endpoints: MessageEndpoints::All,
                kind: None,
                text: String::new(),
            },
        },
        ViewQuery::Events {
            before: None,
            limit: 200,
            filter: EventFilter {
                agent_id: None,
                level: None,
                text: String::new(),
                vcs_only: false,
            },
        },
        ViewQuery::Usage,
        ViewQuery::Questions,
        ViewQuery::Evidence { key: evidence },
    ];
    let mut kinds = std::collections::BTreeSet::new();
    for query in queries {
        let kind = match &query {
            ViewQuery::Availability { .. } => "availability",
            ViewQuery::Overview => "overview",
            ViewQuery::Team => "team",
            ViewQuery::Tasks => "tasks",
            ViewQuery::AgentDetail { .. } => "agent_detail",
            ViewQuery::TaskDetail { .. } => "task_detail",
            ViewQuery::Messages { .. } => "messages",
            ViewQuery::Events { .. } => "events",
            ViewQuery::Usage => "usage",
            ViewQuery::Questions => "questions",
            ViewQuery::Evidence { .. } => "evidence",
        };
        assert!(kinds.insert(kind), "duplicate sample {kind}");
        assert_eq!(serde_json::to_value(&query).unwrap()["kind"], kind);
        assert_eq!(roundtrip(query.clone()), query);

        let source = core.view(query, fixture_clock()).unwrap();
        assert_eq!(serde_json::to_value(&source.payload).unwrap()["kind"], kind);
        let native = roundtrip(ViewData::from(source.clone()));
        let native_kind = match &native.payload {
            ViewPayload::Availability(value) => {
                let client::ViewPayload::Availability(expected) = &source.payload else {
                    unreachable!("kind checked above")
                };
                assert_eq!(value, expected);
                "availability"
            }
            ViewPayload::Overview(_) => "overview",
            ViewPayload::Team(_) => "team",
            ViewPayload::Tasks(_) => "tasks",
            ViewPayload::AgentDetail(_) => "agent_detail",
            ViewPayload::TaskDetail(_) => "task_detail",
            ViewPayload::Messages(_) => "messages",
            ViewPayload::Events(_) => "events",
            ViewPayload::Usage(_) => "usage",
            ViewPayload::Questions(_) => "questions",
            ViewPayload::Evidence(_) => "evidence",
        };
        assert_eq!(native_kind, kind);
        assert_eq!(native.scope, source.scope);
        assert_eq!(native.revision, source.revision);
        assert_eq!(native.synchronized_at_ms, source.synchronized_at_ms);
        assert_eq!(native.freshness, source.freshness);
    }
    assert_eq!(kinds.len(), 11, "one sample per Core view");
}
