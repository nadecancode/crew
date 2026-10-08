//! Composition regression reviewed against exact shared Core 93266067.
//! Final transport validation repeats this on the accepted integrated engine.

#![cfg(feature = "real-core-tests")]

mod common;

use crew_client::api::*;
use crew_client::policy::NATIVE_LIMITS;
use crew_client_transport::{
    HostEffect, HostEvent, HostUpdate, Protocol6Codec, SendDisposition, SessionDriver,
};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;

type Driver = SessionDriver<crew_client::ClientCore, Protocol6Codec>;

fn frame(driver: &mut Driver, channel: u64, request: u64, result: Value) -> HostUpdate {
    driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: json!({"id":request,"result":result}).to_string(),
        })
        .unwrap()
}

fn open(driver: &mut Driver, effect: &HostEffect) -> HostUpdate {
    let HostEffect::Open {
        channel_id,
        attempt,
        expected_scope,
        ..
    } = effect
    else {
        panic!("open");
    };
    let mut context = common::context(*attempt);
    if let Some(expected) = expected_scope {
        context.scope.workspace_id = expected.workspace_id.clone();
        context.grant.workspace_ids = vec![expected.workspace_id.clone()];
    }
    let opened = driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: *channel_id,
            context,
            peer: PeerInfo {
                version: "0.1.0".into(),
                protocol: 6,
                pid: common::PID,
                project: None,
            },
        })
        .unwrap();
    let discovery = opened.effects.iter().find_map(|effect| match effect {
        HostEffect::Send {
            channel_id,
            request_id,
            text,
        } if text.contains("features_negotiate") => Some((*channel_id, *request_id)),
        _ => None,
    });
    if let Some((channel_id, id)) = discovery {
        driver
            .handle(HostEvent::Frame {
                channel_id,
                text: json!({"id":id,"error":{"code":"invalid","message":"old fixture daemon"}})
                    .to_string(),
            })
            .unwrap()
    } else {
        opened
    }
}

fn state_barrier(driver: &mut Driver, connecting: &HostUpdate) -> HostUpdate {
    let effect = connecting
        .effects
        .iter()
        .find(|effect| matches!(effect, HostEffect::Open { .. }))
        .unwrap();
    let subscribed = open(driver, effect);
    let (channel, request, _) = common::send(&subscribed, "subscribe");
    let snapshot = frame(driver, channel, request, json!({"subscribed":true}));
    let (_, request, _) = common::send(&snapshot, "snapshot");
    frame(
        driver,
        channel,
        request,
        serde_json::to_value(common::snapshot()).unwrap(),
    )
}

fn mutation(driver: &mut Driver, planned: &HostUpdate) -> (u64, u64, Value) {
    let effect = planned
        .effects
        .iter()
        .find(|effect| matches!(effect, HostEffect::Open { .. }))
        .unwrap();
    let verifying = open(driver, effect);
    let (channel, request, _) = common::send(&verifying, "snapshot");
    let dispatched = frame(
        driver,
        channel,
        request,
        serde_json::to_value(common::snapshot()).unwrap(),
    );
    common::send(&dispatched, "send")
}

#[test]
fn real_core_background_foreground_replays_identified_submission_once_after_fresh_proof() {
    let clock = Arc::new(common::TestClock::default());
    let core = crew_client::ClientCore::new(ClientConfig {
        limits: NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap();
    let mut driver =
        SessionDriver::with_clock(core, Protocol6Codec, Default::default(), clock.clone()).unwrap();
    let connecting = driver
        .connect("profile".into(), "host".into(), Some("provisional".into()))
        .unwrap();
    let loaded = state_barrier(&mut driver, &connecting);
    assert_eq!(loaded.notice.workspace, WorkspaceStatus::Live);
    let before = loaded.notice.scope.unwrap();
    let planned = driver
        .plan(ActionIntent::SendMessage {
            scope: before.clone(),
            to: "*".into(),
            body: "same scoped submission".into(),
            message_id: "stable-foreground-id".into(),
        })
        .unwrap();
    let ActionPlan::Dispatch { operation_id } = planned.plan else {
        panic!("live send");
    };
    let (old_channel, old_request, first_bytes) = mutation(&mut driver, &planned.update);
    driver
        .handle(HostEvent::SendProgress {
            channel_id: old_channel,
            request_id: old_request,
            disposition: SendDisposition::FullyWritten,
        })
        .unwrap();
    clock.0.store(5, Ordering::SeqCst);
    let suspended = driver
        .handle(HostEvent::Lifecycle { active: false })
        .unwrap();
    assert!(
        suspended.notice.outcomes.is_empty(),
        "held submission has no final notice"
    );
    assert_eq!(driver.channel_count(), 0);
    assert!(driver.timer_count() > 0);
    clock.0.store(10, Ordering::SeqCst);
    let resumed = driver
        .handle(HostEvent::Lifecycle { active: true })
        .unwrap();
    assert!(resumed
        .effects
        .iter()
        .all(|effect| !matches!(effect, HostEffect::Send { .. })));
    let proven = state_barrier(&mut driver, &resumed);
    let after = proven.notice.scope.clone().unwrap();
    assert!(after.attempt > before.attempt);
    assert_eq!(after.daemon_pid, before.daemon_pid);
    assert_eq!(after.daemon_started_at_ms, before.daemon_started_at_ms);
    assert!(proven.notice.outcomes.is_empty());
    let (new_channel, new_request, retried_bytes) = mutation(&mut driver, &proven);
    assert_ne!((old_channel, old_request), (new_channel, new_request));
    assert_eq!(retried_bytes["params"], first_bytes["params"]);
    let result = json!({"message":{"id":"stable-foreground-id","from":"operator","to":"*","type":"message","body":"same scoped submission","timestamp":common::STARTED,"metadata":null},"duplicate":true,"recipients":[]});
    let success = frame(&mut driver, new_channel, new_request, result);
    assert_eq!(success.notice.outcomes.len(), 1);
    assert_eq!(success.notice.outcomes[0].operation_id, operation_id);
    assert_eq!(success.notice.outcomes[0].scope, after);
    assert_eq!(
        success.notice.outcomes[0].kind,
        OperationOutcomeKind::Success
    );
    clock.0.store(40_000, Ordering::SeqCst);
    for effect in suspended.effects {
        if let HostEffect::Schedule { timer_id, .. } = effect {
            assert!(driver
                .handle(HostEvent::Wake { timer_id })
                .unwrap()
                .notice
                .outcomes
                .is_empty());
        }
    }
    assert_eq!(driver.pending_count(), 0);
}
