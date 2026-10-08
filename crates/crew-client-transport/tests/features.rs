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
    let (channel, id, _) = common::send(&subscribing, "subscribe");
    let requested = frame(driver, channel, id, json!({"subscribed":true}));
    let (_, id, _) = common::send(&requested, "snapshot");
    let mut snapshot = common::snapshot();
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
        options: ReadOptions::Feature(FeatureRead {
            request: FeatureReadRequest::DocsIndex(PageRequest::default()),
            lease: None,
        }),
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
    page.options = ReadOptions::Feature(FeatureRead {
        request: FeatureReadRequest::DocsIndex(PageRequest {
            cursor: Some(Id::new("page-2").unwrap()),
            limit: None,
        }),
        lease: Some(lease.clone()),
    });
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
