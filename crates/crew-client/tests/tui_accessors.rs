//! Rust-only immutable host access shares the same bounded authoritative store.

use crew_client::*;
use crew_protocol::rpc::Snapshot;
use crew_protocol::{MessageType, OPERATOR_ID};

#[test]
fn borrowed_domain_and_question_views_match_the_engine_and_keep_tui_bounds() {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/snapshot.json")).unwrap();
    let message = snapshot.messages[0].clone();
    snapshot.messages = (1..=550)
        .map(|n| {
            let mut item = message.clone();
            item.id = format!("m-{n}");
            item.timestamp = n;
            item
        })
        .collect();
    let event = snapshot.events[0].clone();
    snapshot.events = (1..=2050)
        .map(|n| {
            let mut item = event.clone();
            item.seq = n;
            item.timestamp = n;
            item
        })
        .collect();
    let mut question = message;
    question.id = "question".into();
    question.from = "claude-1".into();
    question.to = OPERATOR_ID.into();
    question.kind = MessageType::Question;
    snapshot.questions = vec![question];
    let scope = ClientScope {
        endpoint_id: "tui:test".into(),
        workspace_id: snapshot.session.workspace.clone(),
        generation: None,
        daemon_pid: Some(snapshot.session.daemon_pid),
        daemon_started_at_ms: Some(snapshot.session.daemon_started_at),
        attempt: 1,
    };
    let clock = ClockSample {
        wall_ms: 1000,
        monotonic_ms: 1000,
    };
    let mut core = ClientCore::new(ClientConfig {
        limits: ClientLimits {
            messages: 500,
            events: 2000,
            context_points: 600,
            buffered_pushes: 256,
            pending_operations: 64,
            page_size: 200,
        },
        snapshot_mode: SnapshotMode::BufferedOverlap,
    })
    .unwrap();
    core.apply(
        ClientInput::Bind {
            scope: scope.clone(),
            mode: ConnectionMode::LocalOperator,
            grant: Grant {
                grant_id: "local".into(),
                tiers: vec![GrantTier::Read, GrantTier::Control],
                workspace_ids: vec![scope.workspace_id.clone()],
                expires_at_ms: None,
            },
            snapshot_mode: SnapshotMode::BufferedOverlap,
        },
        clock,
    )
    .unwrap();
    core.apply(
        ClientInput::SnapshotStarted {
            scope: scope.clone(),
            operation_id: 1 << 63,
        },
        clock,
    )
    .unwrap();
    core.apply(
        ClientInput::Snapshot {
            scope: scope.clone(),
            operation_id: 1 << 63,
            snapshot: Box::new(snapshot.clone()),
        },
        clock,
    )
    .unwrap();
    assert_eq!(core.session(), Some(&snapshot.session));
    let ViewPayload::Team(team) = core.view(ViewQuery::Team, clock).unwrap().payload else {
        panic!("team");
    };
    assert_eq!(
        core.agents(),
        team.agents
            .iter()
            .map(|p| p.agent.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(core.tasks(), snapshot.tasks);
    assert_eq!(
        core.messages().cloned().collect::<Vec<_>>(),
        snapshot.messages[50..]
    );
    assert_eq!(
        core.events().cloned().collect::<Vec<_>>(),
        snapshot.events[50..]
    );
    assert_eq!(core.events().next_back().unwrap().seq, 2050);
    assert_eq!(core.questions(), snapshot.questions);
    assert_eq!(core.visible_questions().count(), 1);
    let planned = core
        .plan(
            ActionIntent::Answer {
                scope,
                question_id: "question".into(),
                body: "Proceed".into(),
            },
            clock,
        )
        .unwrap();
    assert!(matches!(planned.plan, ActionPlan::Dispatch { .. }));
    assert_eq!(
        core.questions().len(),
        1,
        "the authoritative question remains until answered"
    );
    assert_eq!(core.pending_answer_ids().collect::<Vec<_>>(), ["question"]);
    assert_eq!(core.visible_questions().count(), 0);
}
