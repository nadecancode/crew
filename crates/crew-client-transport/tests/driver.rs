mod common;

use common::*;
use crew_client::api::*;
use crew_client_transport::{HostEffect, HostEvent, HostUpdate, SendDisposition, TransportEnd};
use serde_json::json;
use std::sync::atomic::Ordering;

#[test]
fn canonical_snapshot_precedes_lane_dispatch_and_bootstrap_ids_are_disjoint() {
    let (mut driver, probe, _) = driver();
    let (root, scope) = bootstrap(&mut driver);
    assert_eq!(scope.workspace_id, WORKSPACE);
    assert_eq!(scope.daemon_started_at_ms, Some(STARTED));
    let recorded = probe.lock().unwrap();
    let starts: Vec<_> = recorded
        .inputs
        .iter()
        .filter_map(|input| match input {
            ClientInput::SnapshotStarted { operation_id, .. } => Some(*operation_id),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 2);
    assert!(starts[0] >= (1 << 63));
    assert_eq!(starts[0], starts[1]);
    assert!(
        matches!(recorded.inputs.last(), Some(ClientInput::Snapshot { scope: actual, .. }) if actual == &scope)
    );
    drop(recorded);
    let plan = driver
        .plan(ActionIntent::Agent {
            scope: scope.clone(),
            id: "a".into(),
            action: AgentAction::Pause,
        })
        .unwrap();
    assert!(!plan
        .update
        .effects
        .iter()
        .any(|effect| matches!(effect, HostEffect::Send { .. })));
    let (channel, _) = ready_lane(&mut driver, &plan.update, "agent_pause");
    assert_ne!(channel, root);
}

#[test]
fn old_channel_attempts_frames_and_timers_cannot_change_reconnected_state() {
    let (mut driver, probe, clock) = driver();
    let (old, scope) = bootstrap(&mut driver);
    let pending = driver
        .plan(ActionIntent::Agent {
            scope,
            id: "a".into(),
            action: AgentAction::Pause,
        })
        .unwrap();
    let timer = pending
        .update
        .effects
        .iter()
        .find_map(|effect| match effect {
            HostEffect::Schedule { timer_id, .. } => Some(*timer_id),
            _ => None,
        })
        .unwrap();
    driver.reconnect().unwrap();
    let revision = driver.notice().revision;
    let inputs = probe.lock().unwrap().inputs.len();
    clock.0.store(100_000, Ordering::SeqCst);
    driver
        .handle(HostEvent::Frame {
            channel_id: old,
            text: json!({"push":{"kind":"shutdown"}}).to_string(),
        })
        .unwrap();
    driver.handle(HostEvent::Wake { timer_id: timer }).unwrap();
    assert_eq!(driver.notice().revision, revision);
    assert_eq!(probe.lock().unwrap().inputs.len(), inputs);
    assert!(driver.notice().scope.unwrap().attempt < 2);
}

#[test]
fn lane_snapshot_rejects_same_pid_different_incarnation_before_action_bytes() {
    let (mut driver, probe, _) = driver();
    let (_, scope) = bootstrap(&mut driver);
    let plan = driver
        .plan(ActionIntent::Agent {
            scope,
            id: "a".into(),
            action: AgentAction::Pause,
        })
        .unwrap();
    let open = plan
        .update
        .effects
        .iter()
        .find(|effect| matches!(effect, HostEffect::Open { .. }))
        .unwrap();
    let verifying = opened(&mut driver, open);
    let (channel, id, _) = send(&verifying, "snapshot");
    let mut changed = snapshot();
    changed.session.daemon_started_at += 1;
    let ended = reply(
        &mut driver,
        channel,
        id,
        serde_json::to_value(changed).unwrap(),
    );
    assert!(ended.effects.iter().all(
        |effect| !matches!(effect, HostEffect::Send { text, .. } if text.contains("agent_pause"))
    ));
    assert_eq!(ended.notice.outcomes[0].kind, OperationOutcomeKind::NotSent);
    assert!(probe.lock().unwrap().inputs.iter().any(|input| matches!(
        input,
        ClientInput::OperationFinished {
            outcome: OperationOutcome::NotSent { .. },
            ..
        }
    )));
}

#[test]
fn never_written_partial_write_and_timeout_have_distinct_outcomes() {
    for disposition in [
        SendDisposition::NeverWritten,
        SendDisposition::WriteStarted,
        SendDisposition::FullyWritten,
    ] {
        let (mut driver, _, clock) = driver();
        let (_, scope) = bootstrap(&mut driver);
        let plan = driver
            .plan(ActionIntent::Agent {
                scope,
                id: "a".into(),
                action: AgentAction::Pause,
            })
            .unwrap();
        let (channel, request_id) = ready_lane(&mut driver, &plan.update, "agent_pause");
        if disposition != SendDisposition::NeverWritten {
            driver
                .handle(HostEvent::SendProgress {
                    channel_id: channel,
                    request_id,
                    disposition,
                })
                .unwrap();
        }
        let ended = driver
            .handle(HostEvent::Ended {
                channel_id: channel,
                reason: TransportEnd::Closed,
            })
            .unwrap();
        assert_eq!(ended.notice.outcomes.len(), 1);
        assert_eq!(
            ended.notice.outcomes[0].kind,
            if disposition == SendDisposition::NeverWritten {
                OperationOutcomeKind::NotSent
            } else {
                OperationOutcomeKind::Unknown
            }
        );
        clock.0.store(90_000, Ordering::SeqCst);
        assert!(driver
            .handle(HostEvent::Ended {
                channel_id: channel,
                reason: TransportEnd::Closed
            })
            .unwrap()
            .notice
            .outcomes
            .is_empty());
    }
}

#[test]
fn two_terminals_keep_full_scope_routes_and_local_close_targets_exact_attachment() {
    let (mut driver, probe, _) = driver();
    let (_, scope) = bootstrap(&mut driver);
    let (first, first_channel) = attach(&mut driver, &scope, "a");
    let (second, second_channel) = attach(&mut driver, &scope, "b");
    for (attachment_id, expected_channel) in [(first, first_channel), (second, second_channel)] {
        let planned = driver
            .plan(ActionIntent::AttachResize {
                scope: scope.clone(),
                attachment_id,
                rows: 32,
                cols: 100,
            })
            .unwrap();
        let (actual, request_id, _) = send(&planned.update, "attach_resize");
        assert_eq!(actual, expected_channel);
        let completed = driver
            .handle(HostEvent::SendProgress {
                channel_id: actual,
                request_id,
                disposition: SendDisposition::FullyWritten,
            })
            .unwrap();
        assert_eq!(
            completed.notice.outcomes[0].kind,
            OperationOutcomeKind::SentUnacknowledged
        );
        assert_eq!(
            completed.notice.outcomes[0].attachment_id,
            Some(attachment_id)
        );
    }
    let close = driver
        .plan(ActionIntent::Detach {
            scope: scope.clone(),
            attachment_id: first,
        })
        .unwrap();
    assert!(close.update.effects.iter().any(
        |effect| matches!(effect, HostEffect::Close { channel_id } if *channel_id == first_channel)
    ));
    assert!(!close.update.effects.iter().any(
        |effect| matches!(effect, HostEffect::Close { channel_id } if *channel_id == second_channel)
    ));
    assert_eq!(
        close.update.notice.outcomes[0].kind,
        OperationOutcomeKind::ClosedLocally
    );
    assert_eq!(close.update.notice.outcomes[0].attachment_id, Some(first));
    let ended = probe.lock().unwrap().inputs.iter().filter(|input| matches!(input, ClientInput::AttachmentEnded { attachment_id, .. } if *attachment_id == first)).count();
    assert_eq!(ended, 1);
    let again = driver
        .plan(ActionIntent::Detach {
            scope,
            attachment_id: first,
        })
        .unwrap();
    assert!(again.update.effects.is_empty());
    assert!(again.update.notice.outcomes.is_empty());
}

#[test]
fn terminal_fake_success_is_ignored_and_late_error_ends_once_without_second_outcome() {
    let (mut driver, probe, _) = driver();
    let (_, scope) = bootstrap(&mut driver);
    let (attachment_id, channel) = attach(&mut driver, &scope, "a");
    let planned = driver
        .plan(ActionIntent::AttachInput {
            scope: scope.clone(),
            attachment_id,
            data: b"hello".to_vec(),
        })
        .unwrap();
    let (_, request_id, _) = send(&planned.update, "attach_input");
    let fake = reply(&mut driver, channel, request_id, json!(true));
    assert!(fake.notice.outcomes.is_empty());
    let fake_null = reply(&mut driver, channel, request_id, serde_json::Value::Null);
    assert!(fake_null.notice.outcomes.is_empty());
    let complete = driver
        .handle(HostEvent::SendProgress {
            channel_id: channel,
            request_id,
            disposition: SendDisposition::FullyWritten,
        })
        .unwrap();
    assert_eq!(complete.notice.outcomes.len(), 1);
    assert_eq!(
        complete.notice.outcomes[0].kind,
        OperationOutcomeKind::SentUnacknowledged
    );
    let frame =
        json!({"id":request_id,"error":{"code":"invalid","message":"not attached"}}).to_string();
    let late = driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: frame.clone(),
        })
        .unwrap();
    assert!(late.notice.outcomes.is_empty());
    assert_eq!(late.terminal[0].attachment_id, attachment_id);
    assert!(matches!(
        late.terminal[0].ended,
        Some(TerminalEnd::Failed { .. })
    ));
    let duplicate = driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: frame,
        })
        .unwrap();
    assert!(duplicate.effects.is_empty() && duplicate.terminal.is_empty());
    assert_eq!(
        probe
            .lock()
            .unwrap()
            .inputs
            .iter()
            .filter(|input| matches!(input, ClientInput::AttachmentEnded { .. }))
            .count(),
        1
    );
}

#[test]
fn null_agent_result_is_distinct_from_an_absent_result_or_ambiguous_envelope() {
    let (mut driver, _, _) = driver();
    let (_, scope) = bootstrap(&mut driver);
    let plan = driver
        .plan(ActionIntent::Agent {
            scope,
            id: "a".into(),
            action: AgentAction::Pause,
        })
        .unwrap();
    let (channel, request_id) = ready_lane(&mut driver, &plan.update, "agent_pause");
    let completed = reply(&mut driver, channel, request_id, serde_json::Value::Null);
    assert_eq!(
        completed.notice.outcomes[0].kind,
        OperationOutcomeKind::Success
    );
}

#[test]
fn core_final_notices_are_held_and_expire_once_while_workspace_is_unbound() {
    let (mut core, probe) = ProbeCore::new();
    core.hold_next_outcome = true;
    let clock = std::sync::Arc::new(TestClock::default());
    let mut driver = crew_client_transport::SessionDriver::with_clock(
        core,
        crew_client_transport::Protocol6Codec,
        crew_client_transport::DriverConfig {
            reconnect: false,
            ..Default::default()
        },
        clock.clone(),
    )
    .unwrap();
    let (_, scope) = bootstrap(&mut driver);
    let plan = driver
        .plan(ActionIntent::SendMessage {
            scope,
            to: "*".into(),
            body: "same submission".into(),
            message_id: "stable-id".into(),
        })
        .unwrap();
    let (channel, request_id) = ready_lane(&mut driver, &plan.update, "send");
    driver
        .handle(HostEvent::SendProgress {
            channel_id: channel,
            request_id,
            disposition: SendDisposition::WriteStarted,
        })
        .unwrap();
    let ended = driver
        .handle(HostEvent::Ended {
            channel_id: channel,
            reason: TransportEnd::Closed,
        })
        .unwrap();
    assert!(
        ended.notice.outcomes.is_empty(),
        "driver must not synthesize a final notice while Core holds it"
    );
    assert!(driver.connection_context().is_none());
    let timer = ended
        .effects
        .iter()
        .find_map(|effect| match effect {
            HostEffect::Schedule {
                timer_id,
                deadline_ms: 30_000,
            } => Some(*timer_id),
            _ => None,
        })
        .unwrap();
    clock.0.store(30_000, Ordering::SeqCst);
    let expired = driver.handle(HostEvent::Wake { timer_id: timer }).unwrap();
    assert_eq!(expired.notice.outcomes.len(), 1);
    assert_eq!(
        expired.notice.outcomes[0].kind,
        OperationOutcomeKind::Unknown
    );
    assert!(probe
        .lock()
        .unwrap()
        .inputs
        .iter()
        .any(|input| matches!(input, ClientInput::Wake { timer_id: 7, .. })));
    assert!(driver
        .handle(HostEvent::Wake { timer_id: timer })
        .unwrap()
        .notice
        .outcomes
        .is_empty());
}

#[test]
fn request_deadline_after_write_reports_unknown_once_and_does_not_replay_action() {
    let (mut driver, _, clock) = driver();
    let (_, scope) = bootstrap(&mut driver);
    let plan = driver
        .plan(ActionIntent::Agent {
            scope,
            id: "a".into(),
            action: AgentAction::Pause,
        })
        .unwrap();
    let timer = plan
        .update
        .effects
        .iter()
        .rev()
        .find_map(|effect| match effect {
            HostEffect::Schedule {
                timer_id,
                deadline_ms: 3000,
            } => Some(*timer_id),
            _ => None,
        })
        .unwrap();
    let (channel, request_id) = ready_lane(&mut driver, &plan.update, "agent_pause");
    driver
        .handle(HostEvent::SendProgress {
            channel_id: channel,
            request_id,
            disposition: SendDisposition::WriteStarted,
        })
        .unwrap();
    clock.0.store(3000, Ordering::SeqCst);
    let timed_out = driver.handle(HostEvent::Wake { timer_id: timer }).unwrap();
    assert_eq!(timed_out.notice.outcomes.len(), 1);
    assert_eq!(
        timed_out.notice.outcomes[0].kind,
        OperationOutcomeKind::Unknown
    );
    assert_eq!(
        timed_out.notice.outcomes[0].reason.as_ref().unwrap().code,
        "timeout"
    );
    assert!(timed_out
        .effects
        .iter()
        .all(|effect| !matches!(effect, HostEffect::Send { .. })));
    assert!(driver
        .handle(HostEvent::Wake { timer_id: timer })
        .unwrap()
        .notice
        .outcomes
        .is_empty());
}

#[test]
fn suspend_preserves_core_retry_deadline_but_explicit_dispose_cancels_it() {
    for dispose in [false, true] {
        let (mut core, _) = ProbeCore::new();
        core.hold_next_outcome = true;
        let clock = std::sync::Arc::new(TestClock::default());
        let mut driver = crew_client_transport::SessionDriver::with_clock(
            core,
            crew_client_transport::Protocol6Codec,
            crew_client_transport::DriverConfig {
                reconnect: false,
                ..Default::default()
            },
            clock.clone(),
        )
        .unwrap();
        let (_, scope) = bootstrap(&mut driver);
        let plan = driver
            .plan(ActionIntent::SendMessage {
                scope,
                to: "*".into(),
                body: "preserved submission".into(),
                message_id: "stable-suspend".into(),
            })
            .unwrap();
        let (channel, request_id) = ready_lane(&mut driver, &plan.update, "send");
        driver
            .handle(HostEvent::SendProgress {
                channel_id: channel,
                request_id,
                disposition: SendDisposition::WriteStarted,
            })
            .unwrap();
        let suspended = driver
            .handle(HostEvent::Lifecycle { active: false })
            .unwrap();
        assert!(suspended.notice.outcomes.is_empty());
        assert_eq!(driver.channel_count(), 0);
        let timer = suspended
            .effects
            .iter()
            .find_map(|effect| match effect {
                HostEffect::Schedule {
                    timer_id,
                    deadline_ms: 30_000,
                } => Some(*timer_id),
                _ => None,
            })
            .unwrap();
        assert_eq!(driver.timer_count(), 1);
        if dispose {
            driver.close().unwrap();
            assert_eq!(driver.timer_count(), 0);
        }
        clock.0.store(30_000, Ordering::SeqCst);
        let expiry = driver.handle(HostEvent::Wake { timer_id: timer }).unwrap();
        assert_eq!(expiry.notice.outcomes.len(), usize::from(!dispose));
        assert!(expiry
            .effects
            .iter()
            .all(|effect| !matches!(effect, HostEffect::Send { .. })));
    }
}

#[test]
fn duplicate_authenticated_open_is_noop_and_changed_live_authority_is_retired() {
    let (mut driver, probe, _) = driver();
    let (channel, _) = bootstrap(&mut driver);
    let context = driver.connection_context().unwrap();
    let peer = PeerInfo {
        version: "0.1.0".into(),
        protocol: 6,
        pid: PID,
        project: None,
    };
    let before = probe.lock().unwrap().inputs.len();
    let duplicate = driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: channel,
            context: context.clone(),
            peer: peer.clone(),
        })
        .unwrap();
    assert!(duplicate.effects.is_empty());
    assert_eq!(duplicate.notice.workspace, WorkspaceStatus::Live);
    assert_eq!(probe.lock().unwrap().inputs.len(), before);
    let mut changed = context;
    changed.grant.tiers = vec![GrantTier::Read];
    let retired = driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: channel,
            context: changed,
            peer,
        })
        .unwrap();
    assert!(retired.effects.iter().any(
        |effect| matches!(effect, HostEffect::Close { channel_id } if *channel_id == channel)
    ));
    assert!(driver.connection_context().is_none());
}

#[test]
fn known_route_startup_time_cannot_be_replaced_during_first_snapshot() {
    let (mut driver, probe, _) = driver();
    let connecting = driver
        .connect("profile".into(), "host".into(), Some("provisional".into()))
        .unwrap();
    let (channel, attempt) = connecting
        .effects
        .iter()
        .find_map(|effect| match effect {
            HostEffect::Open {
                channel_id,
                attempt,
                ..
            } => Some((*channel_id, *attempt)),
            _ => None,
        })
        .unwrap();
    let mut context = context(attempt);
    context.mode = ConnectionMode::ScopedGateway;
    context.scope.daemon_started_at_ms = Some(STARTED);
    let loading = driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: channel,
            context,
            peer: PeerInfo {
                version: "0.1.0".into(),
                protocol: 6,
                pid: PID,
                project: None,
            },
        })
        .unwrap();
    let (_, request, _) = send(&loading, "subscribe");
    let loading = reply(&mut driver, channel, request, json!({"subscribed":true}));
    let (_, request, _) = send(&loading, "snapshot");
    let mut conflicting = snapshot();
    conflicting.session.daemon_started_at += 1;
    let rejected = reply(
        &mut driver,
        channel,
        request,
        serde_json::to_value(conflicting).unwrap(),
    );
    assert_ne!(rejected.notice.workspace, WorkspaceStatus::Live);
    assert!(!probe
        .lock()
        .unwrap()
        .inputs
        .iter()
        .any(|input| matches!(input, ClientInput::Snapshot { .. })));
}

#[test]
fn workspace_reconnect_keeps_authenticated_index_attempt_and_filtered_listing() {
    let (core, probe) = ProbeCore::new();
    let clock = std::sync::Arc::new(TestClock::default());
    let mut driver = crew_client_transport::SessionDriver::with_clock(
        core,
        crew_client_transport::Protocol6Codec,
        crew_client_transport::DriverConfig::default(),
        clock.clone(),
    )
    .unwrap();
    let connecting = driver
        .connect("profile".into(), "host".into(), None)
        .unwrap();
    let (index, index_attempt) = connecting
        .effects
        .iter()
        .find_map(|effect| match effect {
            HostEffect::Open {
                channel_id,
                attempt,
                ..
            } => Some((*channel_id, *attempt)),
            _ => None,
        })
        .unwrap();
    let opened = driver
        .handle(HostEvent::EndpointOpened {
            channel_id: index,
            endpoint_id: "host".into(),
            attempt: index_attempt,
        })
        .unwrap();
    let (_, request, _) = send(&opened, "workspaces");
    let listing = crew_protocol::rpc::Workspaces {
        rev: 1,
        joined: vec![crew_protocol::rpc::WorkspaceEntry {
            id: "0123456789abcdef".into(),
            generation: 1,
            daemon: Some(crew_protocol::rpc::DaemonRef {
                pid: PID,
                started_at: STARTED,
            }),
            project: Some("fixture".into()),
            workspace: WORKSPACE.into(),
            state: crew_protocol::WorkspaceState::Running,
            detail: String::new(),
            summary: None,
            joined_at: 1,
        }],
        others: Vec::new(),
    };
    driver
        .handle(HostEvent::Listing {
            channel_id: index,
            request_id: Some(request),
            listing: listing.clone(),
        })
        .unwrap();
    let selected = driver.select_workspace("0123456789abcdef".into()).unwrap();
    let (root, expected) = selected
        .effects
        .iter()
        .find_map(|effect| match effect {
            HostEffect::Open {
                channel_id,
                expected_scope: Some(scope),
                ..
            } => Some((*channel_id, scope.clone())),
            _ => None,
        })
        .unwrap();
    let mut context = context(expected.attempt);
    context.mode = ConnectionMode::ScopedGateway;
    context.scope = expected;
    context.grant.workspace_ids = vec![context.scope.workspace_id.clone()];
    let open = driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: root,
            context,
            peer: PeerInfo {
                version: "0.1.0".into(),
                protocol: 6,
                pid: PID,
                project: None,
            },
        })
        .unwrap();
    let (_, request, _) = send(&open, "subscribe");
    let loading = reply(&mut driver, root, request, json!({"subscribed":true}));
    let (_, request, _) = send(&loading, "snapshot");
    reply(
        &mut driver,
        root,
        request,
        serde_json::to_value(snapshot()).unwrap(),
    );
    let ended = driver
        .handle(HostEvent::Ended {
            channel_id: root,
            reason: TransportEnd::Closed,
        })
        .unwrap();
    assert_eq!(ended.notice.endpoint, EndpointStatus::Authenticated);
    let timer = ended
        .effects
        .iter()
        .find_map(|effect| match effect {
            HostEffect::Schedule {
                timer_id,
                deadline_ms: 1000,
            } => Some(*timer_id),
            _ => None,
        })
        .unwrap();
    clock.0.store(1000, Ordering::SeqCst);
    let reconnect = driver.handle(HostEvent::Wake { timer_id: timer }).unwrap();
    assert!(reconnect.effects.iter().any(|effect| matches!(
        effect,
        HostEffect::Open {
            purpose: crew_client_transport::ChannelPurpose::Workspace,
            ..
        }
    )));
    driver
        .handle(HostEvent::Listing {
            channel_id: index,
            request_id: None,
            listing,
        })
        .unwrap();
    assert!(driver.workspaces().is_some());
    let recorded = probe.lock().unwrap();
    assert!(recorded
        .inputs
        .iter()
        .filter_map(|input| match input {
            ClientInput::EndpointChanged { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .all(|attempt| attempt == index_attempt));
    assert!(
        matches!(recorded.inputs.last(), Some(ClientInput::Listing { attempt, .. }) if *attempt == index_attempt)
    );
}

#[test]
fn terminal_output_chunks_preserve_bytes_boundaries_buffered_order_and_overflow_end() {
    use base64::Engine as _;
    let (mut driver, _, _) = driver();
    let (_, scope) = bootstrap(&mut driver);
    let (attachment_id, channel) = attach(&mut driver, &scope, "a");
    for (length, expected) in [(65_536, vec![65_536]), (65_537, vec![65_536, 1])] {
        let bytes: Vec<u8> = (0..length).map(|ordinal| (ordinal % 256) as u8).collect();
        let text = json!({"push":{"kind":"pty","data":{"data":base64::engine::general_purpose::STANDARD.encode(&bytes)}}}).to_string();
        let update = driver
            .handle(HostEvent::Frame {
                channel_id: channel,
                text,
            })
            .unwrap();
        assert_eq!(
            update
                .terminal
                .iter()
                .map(|chunk| chunk.data.len())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            update
                .terminal
                .iter()
                .flat_map(|chunk| chunk.data.clone())
                .collect::<Vec<_>>(),
            bytes
        );
        assert!(update.terminal.iter().all(|chunk| chunk.scope == scope
            && chunk.attachment_id == attachment_id
            && chunk.ended.is_none()));
    }
    let overflow = driver
        .handle(HostEvent::Ended {
            channel_id: channel,
            reason: TransportEnd::Overflow,
        })
        .unwrap();
    assert_eq!(overflow.terminal.len(), 1);
    assert_eq!(overflow.terminal[0].ended, Some(TerminalEnd::Overflow));

    let plan = driver
        .plan(ActionIntent::Attach {
            scope: scope.clone(),
            agent: "b".into(),
            rows: 24,
            cols: 80,
        })
        .unwrap();
    let ActionPlan::Dispatch {
        operation_id: buffered_id,
    } = plan.plan
    else {
        panic!("dispatch");
    };
    let (buffered_channel, request_id) = ready_lane(&mut driver, &plan.update, "attach");
    let records = [vec![0xff; 65_537], vec![0; 131_073], vec![0x1b; 65_536]];
    for bytes in &records {
        let text = json!({"push":{"kind":"pty","data":{"data":base64::engine::general_purpose::STANDARD.encode(bytes)}}}).to_string();
        assert!(driver
            .handle(HostEvent::Frame {
                channel_id: buffered_channel,
                text
            })
            .unwrap()
            .terminal
            .is_empty());
    }
    let flushed = reply(
        &mut driver,
        buffered_channel,
        request_id,
        json!({"attached":"b"}),
    );
    assert_eq!(
        flushed
            .terminal
            .iter()
            .map(|chunk| chunk.data.len())
            .collect::<Vec<_>>(),
        [65_536, 1, 65_536, 65_536, 1, 65_536]
    );
    assert_eq!(
        flushed
            .terminal
            .iter()
            .flat_map(|chunk| chunk.data.clone())
            .collect::<Vec<_>>(),
        records.concat()
    );
    assert!(flushed.terminal.iter().all(|chunk| chunk.scope == scope
        && chunk.attachment_id == buffered_id
        && chunk.ended.is_none()));
    let ended = driver
        .handle(HostEvent::Ended {
            channel_id: buffered_channel,
            reason: TransportEnd::Overflow,
        })
        .unwrap();
    assert_eq!(ended.terminal[0].ended, Some(TerminalEnd::Overflow));
}

#[test]
fn long_terminal_stream_retires_send_deadlines_and_keeps_bounded_error_correlation() {
    let (mut driver, _, _) = driver();
    let (_, scope) = bootstrap(&mut driver);
    let (attachment_id, channel) = attach(&mut driver, &scope, "a");
    let mut last = 0;
    for _ in 0..10_000 {
        let planned = driver
            .plan(ActionIntent::AttachInput {
                scope: scope.clone(),
                attachment_id,
                data: vec![b'x'],
            })
            .unwrap();
        let (_, request_id, _) = send(&planned.update, "attach_input");
        last = request_id;
        driver
            .handle(HostEvent::SendProgress {
                channel_id: channel,
                request_id,
                disposition: SendDisposition::FullyWritten,
            })
            .unwrap();
        assert_eq!(driver.pending_count(), 0);
        assert_eq!(driver.timer_count(), 0);
    }
    let ended = driver
        .handle(HostEvent::Frame {
            channel_id: channel,
            text: json!({"id":last,"error":{"code":"unavailable","message":"write failed"}})
                .to_string(),
        })
        .unwrap();
    assert_eq!(ended.terminal[0].attachment_id, attachment_id);
}

#[test]
fn close_and_background_leave_team_running_and_never_replay_terminal_bytes() {
    let (mut driver, _, _) = driver();
    let (_, scope) = bootstrap(&mut driver);
    let (attachment_id, _) = attach(&mut driver, &scope, "a");
    let pending = driver
        .plan(ActionIntent::AttachInput {
            scope,
            attachment_id,
            data: vec![b'x'],
        })
        .unwrap();
    assert!(pending
        .update
        .effects
        .iter()
        .any(|effect| matches!(effect, HostEffect::Send { .. })));
    let inactive = driver
        .handle(HostEvent::Lifecycle { active: false })
        .unwrap();
    assert_eq!(driver.channel_count(), 0);
    assert_eq!(driver.pending_count(), 0);
    assert!(inactive
        .effects
        .iter()
        .all(|effect| !matches!(effect, HostEffect::Send { .. })));
    let active = driver
        .handle(HostEvent::Lifecycle { active: true })
        .unwrap();
    assert!(active
        .effects
        .iter()
        .all(|effect| !matches!(effect, HostEffect::Send { .. })));
    let closed = driver.close().unwrap();
    assert!(closed
        .effects
        .iter()
        .all(|effect| !matches!(effect, HostEffect::Send { .. })));
    assert_eq!(driver.timer_count(), 0);
}

fn opens(update: &HostUpdate) -> Vec<(u64, crew_client_transport::ChannelPurpose, u64)> {
    update
        .effects
        .iter()
        .filter_map(|effect| match effect {
            HostEffect::Open {
                channel_id,
                purpose,
                attempt,
                ..
            } => Some((*channel_id, *purpose, *attempt)),
            _ => None,
        })
        .collect()
}

fn gateway_listing(ids: &[&str]) -> crew_protocol::rpc::Workspaces {
    crew_protocol::rpc::Workspaces {
        rev: 1,
        joined: ids
            .iter()
            .map(|id| crew_protocol::rpc::WorkspaceEntry {
                id: (*id).into(),
                generation: 1,
                daemon: Some(crew_protocol::rpc::DaemonRef {
                    pid: PID,
                    started_at: STARTED,
                }),
                project: None,
                workspace: format!("{WORKSPACE}/{id}"),
                state: crew_protocol::WorkspaceState::Running,
                detail: String::new(),
                summary: None,
                joined_at: 1,
            })
            .collect(),
        others: Vec::new(),
    }
}

/// Authenticate a gateway index and open the first listed workspace route.
/// Returns the index channel and the live route channel.
fn gateway_live(driver: &mut Driver) -> (u64, u64) {
    let connecting = driver
        .connect("profile".into(), "host".into(), None)
        .unwrap();
    let [(index, crew_client_transport::ChannelPurpose::Index, attempt)] = opens(&connecting)[..]
    else {
        panic!("a gateway selection starts with its index")
    };
    let opened = driver
        .handle(HostEvent::EndpointOpened {
            channel_id: index,
            endpoint_id: "host".into(),
            attempt,
        })
        .unwrap();
    let (_, request, _) = send(&opened, "workspaces");
    driver
        .handle(HostEvent::Listing {
            channel_id: index,
            request_id: Some(request),
            listing: gateway_listing(&["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"]),
        })
        .unwrap();
    let selected = driver.select_workspace("aaaaaaaaaaaaaaaa".into()).unwrap();
    let root = gateway_opened(driver, &selected);
    (index, root)
}

fn gateway_opened(driver: &mut Driver, update: &HostUpdate) -> u64 {
    let (root, expected) = update
        .effects
        .iter()
        .find_map(|effect| match effect {
            HostEffect::Open {
                channel_id,
                purpose: crew_client_transport::ChannelPurpose::Workspace,
                expected_scope: Some(scope),
                ..
            } => Some((*channel_id, scope.clone())),
            _ => None,
        })
        .unwrap();
    let mut context = context(expected.attempt);
    context.mode = ConnectionMode::ScopedGateway;
    context.scope = expected;
    context.scope.daemon_pid = Some(PID);
    context.scope.daemon_started_at_ms = Some(STARTED);
    context.grant.workspace_ids = vec![context.scope.workspace_id.clone()];
    let open = driver
        .handle(HostEvent::WorkspaceOpened {
            channel_id: root,
            context,
            peer: PeerInfo {
                version: "0.1.0".into(),
                protocol: 6,
                pid: PID,
                project: None,
            },
        })
        .unwrap();
    let (_, request, _) = send(&open, "subscribe");
    let loading = reply(driver, root, request, json!({"subscribed":true}));
    let (_, request, _) = send(&loading, "snapshot");
    let live = reply(
        driver,
        root,
        request,
        serde_json::to_value(snapshot()).unwrap(),
    );
    assert_eq!(live.notice.workspace, WorkspaceStatus::Live);
    root
}

#[test]
fn gateway_reconnect_reopens_the_index_beside_the_selected_workspace() {
    let (mut driver, probe, _) = driver();
    let (_, _) = gateway_live(&mut driver);
    let reconnect = driver.reconnect().unwrap();
    let opened = opens(&reconnect);
    assert_eq!(opened.len(), 2, "index and workspace: {opened:?}");
    let (index, purpose, index_attempt) = opened[0];
    assert_eq!(purpose, crew_client_transport::ChannelPurpose::Index);
    let (_, purpose, root_attempt) = opened[1];
    assert_eq!(purpose, crew_client_transport::ChannelPurpose::Workspace);
    assert!(index_attempt < root_attempt);
    // The endpoint attempt is the index's, so the route binds under it.
    assert!(matches!(
        probe.lock().unwrap().inputs.last(),
        Some(ClientInput::EndpointChanged { attempt, status: EndpointStatus::Connecting, .. })
            if *attempt == index_attempt
    ));
    let root = gateway_opened(&mut driver, &reconnect);
    driver
        .handle(HostEvent::EndpointOpened {
            channel_id: index,
            endpoint_id: "host".into(),
            attempt: index_attempt,
        })
        .unwrap();
    // A workspace-only revocation now leaves the device signed in.
    let ended = driver
        .handle(HostEvent::Ended {
            channel_id: root,
            reason: TransportEnd::GrantChanged,
        })
        .unwrap();
    assert_eq!(ended.notice.endpoint, EndpointStatus::Authenticated);
    assert_eq!(ended.notice.workspace, WorkspaceStatus::AccessRevoked);
    assert!(opens(&ended).is_empty());
}

#[test]
fn gateway_route_refusal_with_index_keeps_the_device_signed_in() {
    let (mut driver, _, _) = driver();
    let (_, root) = gateway_live(&mut driver);
    // The gateway refuses an ungranted workspace route generically.
    let ended = driver
        .handle(HostEvent::Ended {
            channel_id: root,
            reason: TransportEnd::Authentication,
        })
        .unwrap();
    assert_eq!(ended.notice.endpoint, EndpointStatus::Authenticated);
    assert_eq!(ended.notice.workspace, WorkspaceStatus::AccessRevoked);
    assert_eq!(driver.workspaces().unwrap().joined.len(), 2);
}

#[test]
fn local_route_refusal_still_signs_out_without_opening_an_index() {
    let (mut driver, _, _) = driver();
    let (root, _) = bootstrap(&mut driver);
    let ended = driver
        .handle(HostEvent::Ended {
            channel_id: root,
            reason: TransportEnd::Authentication,
        })
        .unwrap();
    assert_eq!(ended.notice.endpoint, EndpointStatus::SignedOut);
    assert!(opens(&ended).is_empty());
    let reconnect = driver.reconnect().unwrap();
    assert!(opens(&reconnect)
        .iter()
        .all(|(_, purpose, _)| *purpose == crew_client_transport::ChannelPurpose::Workspace));
}
