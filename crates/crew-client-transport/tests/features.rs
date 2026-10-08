mod common;

use std::sync::Arc;

use crew_client::api::*;
use crew_client::features::FeatureAvailability;
use crew_client::policy::NATIVE_LIMITS;
use crew_client_transport::{
    DriverConfig, HostEffect, HostEvent, HostUpdate, Protocol6Codec, SessionDriver,
};
use serde_json::{json, Value};

type Driver = SessionDriver<crew_client::ClientCore, Protocol6Codec>;

fn driver() -> Driver {
    SessionDriver::with_clock(
        crew_client::ClientCore::new(ClientConfig {
            limits: NATIVE_LIMITS,
            snapshot_mode: SnapshotMode::OrderedConnection,
        })
        .unwrap(),
        Protocol6Codec,
        DriverConfig {
            reconnect: false,
            ..Default::default()
        },
        Arc::new(common::TestClock::default()),
    )
    .unwrap()
}

fn open(driver: &mut Driver) -> HostUpdate {
    let update = driver
        .connect("fixture".into(), "host".into(), Some("provisional".into()))
        .unwrap();
    let HostEffect::Open {
        channel_id,
        attempt,
        ..
    } = update
        .effects
        .iter()
        .find(|e| matches!(e, HostEffect::Open { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: *channel_id,
            context: common::context(*attempt),
            peer: PeerInfo {
                version: "fixture".into(),
                protocol: 6,
                pid: common::PID,
                project: None,
            },
        })
        .unwrap()
}

fn frame(driver: &mut Driver, channel: u64, id: u64, result: Value) -> HostUpdate {
    driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: json!({"id":id,"result":result}).to_string(),
        })
        .unwrap()
}

fn finish(driver: &mut Driver, subscribing: HostUpdate, epics: bool) -> u64 {
    finish_with_run(driver, subscribing, epics, epics.then_some("run-1"))
}
fn finish_with_run(
    driver: &mut Driver,
    subscribing: HostUpdate,
    epics: bool,
    run: Option<&str>,
) -> u64 {
    let (channel, id, _) = common::send(&subscribing, "subscribe");
    let requested = frame(driver, channel, id, json!({"subscribed":true}));
    let (_, id, _) = common::send(&requested, "snapshot");
    let mut snapshot = common::snapshot();
    snapshot.session.workspace_run_id = run.map(str::to_owned);
    if epics {
        snapshot.session.workspace_run_id = Some("run-1".into());
        snapshot.epics = Some(serde_json::from_value(board(0, 0)).unwrap());
    }
    frame(driver, channel, id, serde_json::to_value(snapshot).unwrap());
    channel
}

fn board(revision: u128, total: u128) -> Value {
    json!({"generation":"run-1","revision":revision.to_string(),"epics":[],"focus_epic":null,
        "no_epic":{"total":total.to_string(),"queued":total.to_string(),"ready":"0","working":"0",
            "waiting":"0","review":"0","blocked":"0","done":"0","failed":"0"}})
}

fn availability(driver: &Driver) -> FeatureAvailability {
    let ViewPayload::Features(value) = driver.query(ViewQuery::Features).unwrap().payload else {
        panic!("feature view")
    };
    value
}

#[test]
fn discovery_precedes_bootstrap_and_old_peer_errors_keep_legacy_views() {
    for code in ["invalid", "forbidden", "unsupported_feature"] {
        let mut driver = driver();
        let opened = open(&mut driver);
        assert!(!opened
            .effects
            .iter()
            .any(|e| matches!(e,HostEffect::Send{text,..}if text.contains("\"subscribe\""))));
        let (channel, id, wire) = common::send(&opened, "features_negotiate");
        assert_eq!(wire["params"]["versions"].as_object().unwrap().len(), 4);
        let subscribing = driver
            .handle(HostEvent::Frame {
                channel_id: channel,
                text: json!({"id":id,"error":{"code":code,"message":"synthetic"}}).to_string(),
            })
            .unwrap();
        finish(&mut driver, subscribing, false);
        let features = availability(&driver);
        assert!(features.session.is_none());
        assert_eq!(features.reason.unwrap().code, code);
        assert_eq!(driver.notice().workspace, WorkspaceStatus::Live);
    }
}

#[test]
fn accepted_epics_apply_coherently_and_a_gap_requires_resynchronization() {
    let mut driver = driver();
    let opened = open(&mut driver);
    let (channel, id, _) = common::send(&opened, "features_negotiate");
    let subscribing = frame(
        &mut driver,
        channel,
        id,
        json!({"accepted":{"epics":1},"connection_epoch":"conn-1","workspace_run_id":"run-1","unavailable":{}}),
    );
    finish(&mut driver, subscribing, true);
    let update = json!({"workspace_run_id":"run-1","from_revision":"0","to_revision":"1","board":board(1,1),
        "tasks":[{"id":"task-1","title":"Task","created_at":1,"status":"queued"}]});
    driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: json!({"push":{"kind":"epic_board_changed","data":update}}).to_string(),
        })
        .unwrap();
    let ViewPayload::Tasks(tasks) = driver.query(ViewQuery::Tasks).unwrap().payload else {
        panic!("tasks")
    };
    assert_eq!(tasks.tasks.len(), 1);
    assert_eq!(tasks.epics.unwrap().no_epic.total.0, 1);
    let revision = driver.notice().revision;
    driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: json!({"push":{"kind":"epic_board_changed","data":update}}).to_string(),
        })
        .unwrap();
    assert_eq!(driver.notice().revision, revision);
    let gap = json!({"workspace_run_id":"run-1","from_revision":"3","to_revision":"4","board":board(4,1),"tasks":[]});
    let result = driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: json!({"push":{"kind":"epic_board_changed","data":gap}}).to_string(),
        })
        .unwrap();
    assert!(result
        .effects
        .iter()
        .any(|e| matches!(e,HostEffect::Send{text,..}if text.contains("\"snapshot\""))));
}

#[test]
fn discovery_cannot_grant_unrequested_versions_or_hide_authentication_loss() {
    let mut driver = driver();
    let opened = open(&mut driver);
    let (channel, id, _) = common::send(&opened, "features_negotiate");
    let result = frame(
        &mut driver,
        channel,
        id,
        json!({"accepted":{"epics":2},"connection_epoch":"conn-1","workspace_run_id":null,"unavailable":{}}),
    );
    assert!(result
        .effects
        .iter()
        .any(|e| matches!(e,HostEffect::Close{channel_id,..}if *channel_id==channel)));
    let mut driver = crate::driver();
    let opened = open(&mut driver);
    let (channel, id, _) = common::send(&opened, "features_negotiate");
    let result = driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: json!({"id":id,"error":{"code":"unauthorized","message":"synthetic"}})
                .to_string(),
        })
        .unwrap();
    assert!(result
        .effects
        .iter()
        .any(|e| matches!(e,HostEffect::Close{channel_id,..}if *channel_id==channel)));
    assert!(!result
        .effects
        .iter()
        .any(|e| matches!(e,HostEffect::Send{text,..}if text.contains("\"subscribe\""))));
}

#[test]
fn docs_sequence_uses_issuing_read_connection_and_cannot_reopen_its_lease() {
    use crew_client::features::{FeatureRead, FeatureReadRequest, FeatureResourceKind};
    use crew_protocol::features::{Id, PageRequest};
    let mut driver = driver();
    let opened = open(&mut driver);
    let (root, id, _) = common::send(&opened, "features_negotiate");
    let subscribing = frame(
        &mut driver,
        root,
        id,
        json!({"accepted":{"docs":1},"connection_epoch":"root-1","workspace_run_id":null,"unavailable":{}}),
    );
    finish(&mut driver, subscribing, false);
    let scope = driver.notice().scope.unwrap();
    let key = ResourceKey {
        entity: EntityKey {
            scope: scope.clone(),
            entity: Entity::Workspace,
        },
        kind: ResourceKind::Feature(FeatureResourceKind::DocsIndex),
        options: ReadOptions::Feature(Box::new(FeatureRead {
            request: FeatureReadRequest::DocsIndex(PageRequest::default()),
            lease: None,
        })),
    };
    let plan = driver
        .plan(ActionIntent::Read {
            scope: scope.clone(),
            resource: ResourceRequest { key: key.clone() },
        })
        .unwrap();
    let HostEffect::Open {
        channel_id,
        attempt,
        expected_scope,
        ..
    } = plan
        .update
        .effects
        .iter()
        .find(|e| matches!(e, HostEffect::Open { .. }))
        .unwrap()
    else {
        panic!("read lane")
    };
    let channel = *channel_id;
    let mut context = common::context(*attempt);
    context.scope = expected_scope.clone().unwrap();
    context.scope.daemon_started_at_ms = None;
    context.grant.workspace_ids = vec![scope.workspace_id.clone()];
    let discovering = driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: channel,
            context,
            peer: PeerInfo {
                version: "fixture".into(),
                protocol: 6,
                pid: common::PID,
                project: None,
            },
        })
        .unwrap();
    let (_, id, _) = common::send(&discovering, "features_negotiate");
    let verifying = frame(
        &mut driver,
        channel,
        id,
        json!({"accepted":{"docs":1},"connection_epoch":"read-1","workspace_run_id":null,"unavailable":{}}),
    );
    assert!(!verifying
        .effects
        .iter()
        .any(|e| matches!(e,HostEffect::Send{text,..}if text.contains("\"docs_index\""))));
    let (_, id, _) = common::send(&verifying, "snapshot");
    let reading = frame(
        &mut driver,
        channel,
        id,
        serde_json::to_value(common::snapshot()).unwrap(),
    );
    let (_, id, _) = common::send(&reading, "docs_index");
    frame(
        &mut driver,
        channel,
        id,
        json!({"epoch":"docs-1","revision":"1","sources":[],"entries":[],"next_cursor":"page-2",
        "phase":"ready","watch_mode":"events","limited":false,"omitted_entries":null,"reason":null}),
    );
    let ViewPayload::Docs(state) = driver
        .query(ViewQuery::Docs { key: key.clone() })
        .unwrap()
        .payload
    else {
        panic!("docs view")
    };
    let Some(ResourceValue::Feature(value)) = state.value else {
        panic!("typed resource")
    };
    let lease = value.lease.unwrap();
    assert_eq!(lease.connection_epoch.as_str(), "read-1");
    let mut page = key.clone();
    page.options = ReadOptions::Feature(Box::new(FeatureRead {
        request: FeatureReadRequest::DocsIndex(PageRequest {
            cursor: Some(Id::new("page-2").unwrap()),
            limit: None,
        }),
        lease: Some(lease.clone()),
    }));
    let continuation = driver
        .plan(ActionIntent::Read {
            scope: scope.clone(),
            resource: ResourceRequest { key: page.clone() },
        })
        .unwrap();
    assert_eq!(common::send(&continuation.update, "docs_index").0, channel);
    assert!(!continuation
        .update
        .effects
        .iter()
        .any(|e| matches!(e, HostEffect::Open { .. })));
    driver
        .handle(HostEvent::Ended {
            channel_id: channel,
            reason: crew_client_transport::TransportEnd::Closed,
        })
        .unwrap();
    let ViewPayload::Docs(stale) = driver.query(ViewQuery::Docs { key }).unwrap().payload else {
        panic!("docs view")
    };
    assert_eq!(stale.status, ResourceStatus::Stale);
    let rejected = driver
        .plan(ActionIntent::Read {
            scope,
            resource: ResourceRequest { key: page },
        })
        .unwrap();
    assert!(!rejected
        .update
        .effects
        .iter()
        .any(|e| matches!(e, HostEffect::Open { .. } | HostEffect::Send { .. })));
    assert!(rejected.update.notice.outcomes.iter().any(|n| n
        .reason
        .as_ref()
        .is_some_and(|r| r.code == "snapshot_expired")));
}

fn start_feature(name: &str) -> (Driver, u64) {
    let mut driver = driver();
    let opened = open(&mut driver);
    let (channel, id, _) = common::send(&opened, "features_negotiate");
    let subscribing = frame(
        &mut driver,
        channel,
        id,
        json!({"accepted":{name:1},"connection_epoch":"root-1","workspace_run_id":"run-1","unavailable":{}}),
    );
    finish_with_run(&mut driver, subscribing, false, Some("run-1"));
    (driver, channel)
}

fn feature_action(driver: &mut Driver, action: crew_client::features::FeatureAction) -> HostUpdate {
    let planned = driver
        .plan(ActionIntent::Feature {
            scope: driver.notice().scope.unwrap(),
            action,
        })
        .unwrap();
    assert!(
        matches!(planned.plan, ActionPlan::Dispatch { .. }),
        "{:?}; feature {:?}",
        planned.plan,
        availability(driver)
    );
    planned.update
}

fn ready_feature_lane(
    driver: &mut Driver,
    opening: &HostUpdate,
    feature: &str,
    epoch: &str,
) -> HostUpdate {
    let HostEffect::Open {
        channel_id,
        attempt,
        expected_scope,
        ..
    } = opening
        .effects
        .iter()
        .find(|e| matches!(e, HostEffect::Open { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    let mut context = common::context(*attempt);
    context.scope = expected_scope.clone().unwrap();
    context.scope.daemon_started_at_ms = None;
    context.grant.workspace_ids = vec![context.scope.workspace_id.clone()];
    let opened = driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: *channel_id,
            context,
            peer: PeerInfo {
                version: "fixture".into(),
                protocol: 6,
                pid: common::PID,
                project: None,
            },
        })
        .unwrap();
    let (_, id, _) = common::send(&opened, "features_negotiate");
    let verifying = frame(
        driver,
        *channel_id,
        id,
        json!({"accepted":{feature:1},"connection_epoch":epoch,"workspace_run_id":"run-1","unavailable":{}}),
    );
    let (_, id, _) = common::send(&verifying, "snapshot");
    let mut snapshot = common::snapshot();
    snapshot.session.workspace_run_id = Some("run-1".into());
    frame(
        driver,
        *channel_id,
        id,
        serde_json::to_value(snapshot).unwrap(),
    )
}

fn aside_key(driver: &Driver) -> ResourceKey {
    use crew_client::features::*;
    ResourceKey {
        entity: EntityKey {
            scope: driver.notice().scope.unwrap(),
            entity: Entity::Workspace,
        },
        kind: ResourceKind::Feature(FeatureResourceKind::AsideDetail),
        options: ReadOptions::Feature(Box::new(FeatureRead {
            request: FeatureReadRequest::AsideDetail(
                serde_json::from_value(
                    json!({"workspace_run_id":"run-1","id":"aside-1","agent":null}),
                )
                .unwrap(),
            ),
            lease: None,
        })),
    }
}

fn read_feature(driver: &mut Driver, key: ResourceKey) -> HostUpdate {
    driver
        .plan(ActionIntent::Read {
            scope: driver.notice().scope.unwrap(),
            resource: ResourceRequest { key },
        })
        .unwrap()
        .update
}

fn observe_asides(driver: &mut Driver) {
    let request = feature_action(
        driver,
        crew_client::features::FeatureAction::WatchAsides(
            serde_json::from_value(json!({"workspace_run_id":"run-1","page":{}})).unwrap(),
        ),
    );
    let (channel, id, _) = common::send(&request, "aside_watch");
    frame(
        driver,
        channel,
        id,
        json!({"watch_id":"watch-1","initial":{"workspace_run_id":"run-1","aside_epoch":"aside-epoch","revision":"0","rows":[],"next_cursor":null,"evicted":false}}),
    );
}

fn aside_changed(driver: &mut Driver, channel: u64, from: u64, to: u64, gone: Value) {
    driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: json!({"push":{"kind":"aside","data":{
        "watch_id":"watch-1","workspace_run_id":"run-1","aside_epoch":"aside-epoch",
        "from_revision":from.to_string(),"to_revision":to.to_string(),"changed_ids":[],"gone":gone,
        "availability_changed":false,"resync_required":false}}})
            .to_string(),
        })
        .unwrap();
}

#[test]
fn explicit_removal_purges_cached_and_late_detail_and_fences_an_unsent_ask() {
    use crew_client::features::FeatureAction;
    let (mut driver, root) = start_feature("asides");
    observe_asides(&mut driver);
    let key = aside_key(&driver);
    let reading = read_feature(&mut driver, key.clone());
    let request = ready_feature_lane(&mut driver, &reading, "asides", "reader-1");
    let (read_channel, id, _) = common::send(&request, "aside_get");
    let detail = json!({"state":"available","detail":{"workspace_run_id":"run-1","summary_id":"aside-1",
        "aside_epoch":"aside-epoch","revision":"0","question":"private question", "answer_meta":null,"answer_text":null}});
    frame(&mut driver, read_channel, id, detail.clone());
    let late = read_feature(&mut driver, key.clone());
    let (_, late_id, _) = common::send(&late, "aside_get");
    let queued = feature_action(
        &mut driver,
        FeatureAction::AskAside(
            serde_json::from_value(json!({
        "workspace_run_id":"run-1","request_id":"aside-1","question":"private question",
        "targets":[{"agent_id":"agent-1","incarnation":"inc-1"}]}))
            .unwrap(),
        ),
    );
    aside_changed(
        &mut driver,
        root,
        0,
        1,
        json!([{"id":"aside-1","cause":"removed"}]),
    );
    frame(&mut driver, read_channel, late_id, detail);
    let ViewPayload::Asides(value) = driver.query(ViewQuery::Asides { key }).unwrap().payload
    else {
        panic!("aside view")
    };
    assert!(value.value.is_none());
    assert_eq!(value.error.unwrap().code, "removed");
    let released = ready_feature_lane(&mut driver, &queued, "asides", "writer-1");
    assert!(!released.effects.iter().any(|e| matches!(e,HostEffect::Send{text,..} if text.contains("aside_ask") || text.contains("private question"))));
}

#[test]
fn observer_gap_hides_local_aside_text_until_authoritative_reconciliation() {
    let (mut driver, root) = start_feature("asides");
    observe_asides(&mut driver);
    let key = aside_key(&driver);
    let reading = read_feature(&mut driver, key.clone());
    let request = ready_feature_lane(&mut driver, &reading, "asides", "reader-1");
    let (channel, id, _) = common::send(&request, "aside_get");
    frame(
        &mut driver,
        channel,
        id,
        json!({"state":"available","detail":{"workspace_run_id":"run-1","summary_id":"aside-1",
        "aside_epoch":"aside-epoch","revision":"0","question":"private question","answer_meta":null,"answer_text":null}}),
    );
    aside_changed(&mut driver, root, 5, 6, json!([]));
    let ViewPayload::Asides(value) = driver.query(ViewQuery::Asides { key }).unwrap().payload
    else {
        panic!("aside view")
    };
    assert!(value.value.is_none());
    let ViewPayload::FeatureWatches(watches) = driver
        .query(ViewQuery::FeatureWatches {
            feature: crew_protocol::features::Feature::Asides,
        })
        .unwrap()
        .payload
    else {
        panic!("watch")
    };
    assert!(watches[0].stale);
    assert!(watches[0].change.is_some());
}

#[test]
fn typed_admission_failure_survives_transport_without_replaying_paid_ask() {
    use crew_client::features::{FeatureAction, FeatureRejection};
    let (mut driver, _) = start_feature("asides");
    let action = feature_action(
        &mut driver,
        FeatureAction::AskAside(
            serde_json::from_value(json!({
        "workspace_run_id":"run-1","request_id":"aside-1","question":"question",
        "targets":[{"agent_id":"agent-1","incarnation":"inc-1"}]}))
            .unwrap(),
        ),
    );
    let request = ready_feature_lane(&mut driver, &action, "asides", "writer-1");
    let (channel, id, _) = common::send(&request, "aside_ask");
    let update=driver.handle(HostEvent::Frame{channel_id:channel,text:json!({"id":id,"error":{
        "code":"too_large","message":"Question too long", "data":{"reason":"question_limit","max_bytes":8192}}}).to_string()}).unwrap();
    assert!(matches!(
        update.notice.outcomes[0].reason.as_ref().unwrap().details,
        Some(FeatureRejection::AsideAdmission(
            crew_protocol::asides::AsideAdmissionFailure::QuestionLimit { max_bytes: 8192 }
        ))
    ));
    assert!(!update
        .effects
        .iter()
        .any(|e| matches!(e, HostEffect::Send { .. })));
}

#[test]
fn docs_expiry_invalidates_unselected_retained_version_and_watch_survives_snapshot() {
    use crew_client::features::*;
    use crew_protocol::features::{Id, List, Wide};
    let (mut driver, root) = start_feature("docs");
    let watched = feature_action(
        &mut driver,
        FeatureAction::WatchDocs(crew_protocol::docs::DocsWatch {
            selected: List::default(),
        }),
    );
    let (_, id, _) = common::send(&watched, "docs_watch");
    frame(
        &mut driver,
        root,
        id,
        json!({"watch_id":"docs-watch","initial_index_page":{
        "epoch":"docs-epoch","revision":"0","sources":[],"entries":[],"next_cursor":null,
        "phase":"ready","watch_mode":"events","limited":false,"omitted_entries":null,"reason":null}}),
    );
    let request=FeatureRead {lease:Some(DocLease{connection_epoch:Id::new("root-1").unwrap(),docs_epoch:Id::new("docs-epoch").unwrap()}),
        request:FeatureReadRequest::Document(serde_json::from_value(json!({"document":{"doc_id":"d","source_id":"s","source_generation":"1"},"version":{"kind":"id","id":"v"}})).unwrap())};
    let key = ResourceKey {
        entity: EntityKey {
            scope: driver.notice().scope.unwrap(),
            entity: Entity::Workspace,
        },
        kind: ResourceKind::Feature(request.kind()),
        options: ReadOptions::Feature(Box::new(request)),
    };
    let reading = read_feature(&mut driver, key.clone());
    let (_, id, _) = common::send(&reading, "docs_read");
    frame(
        &mut driver,
        root,
        id,
        json!({"state":"available","data":{"epoch":"docs-epoch","snapshot_id":"snapshot-1",
        "version":{"id":"v","document":{"doc_id":"d","source_id":"s","source_generation":"1"},"kind":"seen","markdown_hash":"h","bytes":1,"observed_at_ms":10,"recorded_at_ms":null,"commit_id":null,"change_id":null,"author":null,"subject":null},
        "markdown":"x","blocks":[],"outline":[],"images":[],"render_hash":"r","notices":[]}}),
    );
    let ViewPayload::Docs(before) = driver
        .query(ViewQuery::Docs { key: key.clone() })
        .unwrap()
        .payload
    else {
        panic!("docs")
    };
    assert!(before.value.is_some());
    // A normal legacy stream resync starts a Snapshot; the Docs stream remains
    // independently subscribed and must still consume expiry during that barrier.
    driver
        .handle(HostEvent::Frame {
            channel_id: root,
            text: json!({"push":{"kind":"resync"}}).to_string(),
        })
        .unwrap();
    driver
        .handle(HostEvent::Frame {
            channel_id: root,
            text: json!({"push":{"kind":"docs_changed","data":{
        "watch_id":"docs-watch","epoch":"docs-epoch","from_revision":"0","to_revision":"1",
        "upserts":[],"removed_doc_ids":[],"changed_selected":[],"history_sources_changed":[],
        "expired_seen_versions":["v"],"resync_required":false}}})
            .to_string(),
        })
        .unwrap();
    let ViewPayload::Docs(after) = driver.query(ViewQuery::Docs { key }).unwrap().payload else {
        panic!("docs")
    };
    assert!(after.value.is_none());
    assert_eq!(after.error.unwrap().code, "version_expired");
    let ViewPayload::FeatureWatches(w) = driver
        .query(ViewQuery::FeatureWatches {
            feature: crew_protocol::features::Feature::Docs,
        })
        .unwrap()
        .payload
    else {
        panic!("watch")
    };
    assert_eq!(w[0].revision, Wide(1));
}

#[test]
fn metrics_keep_explicit_historical_query_and_reject_cross_workspace_resources() {
    use crew_client::features::*;
    let (mut driver, _) = start_feature("usage_metrics");
    let fixtures: Vec<Value> = serde_json::from_str(include_str!(
        "../../crew-core/tests/fixtures/features/requests-v1.json"
    ))
    .unwrap();
    let mut query: crew_protocol::usage_metrics::UsageQuery = serde_json::from_value(
        fixtures
            .iter()
            .find(|v| v["method"] == "usage_observations")
            .unwrap()["params"]
            .clone(),
    )
    .unwrap();
    query.scope.workspace_run_id = crew_protocol::features::Id::new("historical-run").unwrap();
    query.from_ms = Some(crew_protocol::features::Timestamp::new(100).unwrap());
    query.to_ms = Some(crew_protocol::features::Timestamp::new(200).unwrap());
    let read = FeatureRead {
        request: FeatureReadRequest::UsageObservations(query.clone()),
        lease: None,
    };
    let key = ResourceKey {
        entity: EntityKey {
            scope: driver.notice().scope.unwrap(),
            entity: Entity::Workspace,
        },
        kind: ResourceKind::Feature(read.kind()),
        options: ReadOptions::Feature(Box::new(read.clone())),
    };
    let mut wrong = key.clone();
    wrong.entity.scope.workspace_id = "other-workspace".into();
    let denied = read_feature(&mut driver, wrong);
    assert!(!denied
        .effects
        .iter()
        .any(|e| matches!(e, HostEffect::Open { .. } | HostEffect::Send { .. })));
    let opening = read_feature(&mut driver, key.clone());
    let request = ready_feature_lane(&mut driver, &opening, "usage_metrics", "metrics-read");
    let (channel, id, wire) = common::send(&request, "usage_observations");
    assert_eq!(
        wire["params"]["scope"]["workspace_run_id"],
        "historical-run"
    );
    let book = query
        .price_book_id
        .clone()
        .unwrap_or_else(|| crew_protocol::features::Id::new("book-1").unwrap());
    let canonical = query.resolved_shape(book, 200, false).unwrap();
    frame(
        &mut driver,
        channel,
        id,
        json!({"revision":"1","query":canonical,"rows":[],"next_cursor":null}),
    );
    let ViewPayload::UsageMetrics(ready) = driver
        .query(ViewQuery::UsageMetrics { key: key.clone() })
        .unwrap()
        .payload
    else {
        panic!("metrics")
    };
    assert!(ready.value.is_some());
    let again = read_feature(&mut driver, key.clone());
    let (_, id, _) = common::send(&again, "usage_observations");
    let mut changed = canonical;
    changed.from_ms = Some(crew_protocol::features::Timestamp::new(101).unwrap());
    frame(
        &mut driver,
        channel,
        id,
        json!({"revision":"1","query":changed,"rows":[],"next_cursor":null}),
    );
    let ViewPayload::UsageMetrics(value) = driver
        .query(ViewQuery::UsageMetrics { key })
        .unwrap()
        .payload
    else {
        panic!("metrics")
    };
    assert!(value.value.is_none());
    assert_eq!(value.error.unwrap().code, "decode");
}
