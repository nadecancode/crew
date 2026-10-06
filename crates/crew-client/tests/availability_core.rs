//! Full intent queries through the real public Core. No test engine or plan
//! probe implements the availability query.

use crew_client::policy::NATIVE_LIMITS;
use crew_client::*;
use crew_protocol::rpc::Snapshot;
use crew_protocol::{AgentStatus, MessageType, OPERATOR_ID};

fn clock() -> ClockSample {
    ClockSample {
        wall_ms: 1_000_000,
        monotonic_ms: 100,
    }
}
fn scope() -> ClientScope {
    ClientScope {
        endpoint_id: "gateway:test".into(),
        workspace_id: "w".into(),
        generation: Some(1),
        daemon_pid: Some(4000),
        daemon_started_at_ms: Some(1),
        attempt: 1,
    }
}

fn core(tiers: Vec<GrantTier>) -> ClientCore {
    let mut core = ClientCore::new(ClientConfig {
        limits: NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap();
    core.apply(
        ClientInput::Bind {
            scope: scope(),
            mode: ConnectionMode::ScopedGateway,
            grant: Grant {
                grant_id: "device:grant".into(),
                tiers,
                workspace_ids: vec!["w".into()],
                expires_at_ms: None,
            },
            snapshot_mode: SnapshotMode::OrderedConnection,
        },
        clock(),
    )
    .unwrap();
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/snapshot.json")).unwrap();
    snapshot
        .agents
        .iter_mut()
        .find(|agent| agent.id == "claude-1")
        .unwrap()
        .status = AgentStatus::Working;
    let mut question = snapshot.messages[0].clone();
    question.id = "question".into();
    question.from = "claude-1".into();
    question.to = OPERATOR_ID.into();
    question.kind = MessageType::Question;
    snapshot.questions = vec![question];
    core.apply(
        ClientInput::SnapshotStarted {
            scope: scope(),
            operation_id: 1 << 63,
        },
        clock(),
    )
    .unwrap();
    core.apply(
        ClientInput::Snapshot {
            scope: scope(),
            operation_id: 1 << 63,
            snapshot: Box::new(snapshot),
        },
        clock(),
    )
    .unwrap();
    core
}

fn query(core: &dyn ClientApi, intent: ActionIntent) -> Result<ViewData, ClientError> {
    core.view(
        ViewQuery::Availability {
            intent: Box::new(intent),
        },
        clock(),
    )
}
fn payload(view: ViewData) -> ActionAvailability {
    let ViewPayload::Availability(value) = view.payload else {
        panic!("availability payload");
    };
    value
}
fn send() -> ActionIntent {
    ActionIntent::SendMessage {
        scope: scope(),
        to: "claude-1".into(),
        body: "message".into(),
        message_id: "m-stable".into(),
    }
}

fn observed(core: &ClientCore) -> Vec<u8> {
    let key = ResourceKey {
        entity: EntityKey {
            scope: scope(),
            entity: Entity::Agent("claude-1".into()),
        },
        kind: ResourceKind::Output,
        options: ReadOptions::Default,
    };
    let views: Vec<_> = [
        ViewQuery::Overview,
        ViewQuery::Team,
        ViewQuery::Tasks,
        ViewQuery::Usage,
        ViewQuery::Questions,
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
        ViewQuery::Evidence { key },
    ]
    .into_iter()
    .map(|query| core.view(query, clock()).unwrap())
    .collect();
    serde_json::to_vec(&views).unwrap()
}
fn input(submit: bool) -> ActionIntent {
    ActionIntent::AgentInput {
        scope: scope(),
        id: "claude-1".into(),
        text: Some("日本".into()),
        submit,
    }
}

#[test]
fn full_intent_reasons_and_malformed_errors_match_planning_and_keep_captured_scope() {
    for tiers in [
        vec![GrantTier::Read],
        vec![GrantTier::Read, GrantTier::Control],
    ] {
        for intent in [
            send(),
            input(true),
            ActionIntent::Answer {
                scope: scope(),
                question_id: "question".into(),
                body: "yes".into(),
            },
            ActionIntent::StopTeam { scope: scope() },
            ActionIntent::ResumeLimited {
                scope: scope(),
                restart: false,
            },
        ] {
            let mut core = core(tiers.clone());
            let revision = core.revision();
            let available = payload(query(&core, intent.clone()).unwrap());
            assert_eq!(core.revision(), revision);
            match core.plan(intent, clock()).unwrap().plan {
                ActionPlan::Dispatch { .. } | ActionPlan::Confirm { .. } => {
                    assert!(available.available)
                }
                ActionPlan::Unavailable { reason } => {
                    assert!(!available.available);
                    assert_eq!(available.reason.unwrap().code, reason.code);
                }
            }
        }
    }
    let mut core = core(vec![GrantTier::Read, GrantTier::Control]);
    let mut stale = scope();
    stale.attempt = 0;
    let intent = ActionIntent::StopTeam {
        scope: stale.clone(),
    };
    let view = query(&core, intent.clone()).unwrap();
    assert_eq!(view.scope, Some(stale));
    assert_eq!(payload(view).reason.unwrap().code, "stale_scope");
    let malformed = ActionIntent::SendMessage {
        scope: scope(),
        to: "claude-1".into(),
        body: "".into(),
        message_id: "m-id".into(),
    };
    assert_eq!(
        query(&core, malformed.clone()).unwrap_err(),
        core.plan(malformed, clock()).unwrap_err()
    );
    core.apply(
        ClientInput::Unbind {
            scope: scope(),
            reason: ClientReason {
                code: "closed".into(),
                message: "closed".into(),
            },
        },
        clock(),
    )
    .unwrap();
    assert_eq!(
        payload(query(&core, send()).unwrap()).reason.unwrap().code,
        "not_connected"
    );
}

#[test]
fn repeated_queries_preserve_state_revision_resources_operation_ids_and_confirmations() {
    let tiers = vec![GrantTier::Read, GrantTier::Control];
    let mut queried = core(tiers.clone());
    let mut untouched = core(tiers);
    let initial = observed(&queried);
    for _ in 0..50 {
        assert!(payload(query(&queried, send()).unwrap()).available);
        assert!(
            payload(
                query(
                    &queried,
                    ActionIntent::Agent {
                        scope: scope(),
                        id: "claude-1".into(),
                        action: AgentAction::Kill
                    }
                )
                .unwrap()
            )
            .available
        );
    }
    assert_eq!(observed(&queried), initial);
    let read = ActionIntent::Read {
        scope: scope(),
        resource: ResourceRequest {
            key: ResourceKey {
                entity: EntityKey {
                    scope: scope(),
                    entity: Entity::Agent("claude-1".into()),
                },
                kind: ResourceKind::Output,
                options: ReadOptions::Default,
            },
        },
    };
    assert_eq!(
        queried.plan(read.clone(), clock()).unwrap(),
        untouched.plan(read, clock()).unwrap()
    );
    let loading = observed(&queried);
    for _ in 0..20 {
        query(&queried, input(false)).unwrap();
    }
    assert_eq!(
        observed(&queried),
        loading,
        "queries preserve pending resource state"
    );
    assert_eq!(
        queried.plan(send(), clock()).unwrap(),
        untouched.plan(send(), clock()).unwrap()
    );
    assert!(!payload(query(&queried, send()).unwrap()).available);
    let kill = ActionIntent::Agent {
        scope: scope(),
        id: "claude-1".into(),
        action: AgentAction::Kill,
    };
    let actual = queried.plan(kill.clone(), clock()).unwrap();
    assert_eq!(actual, untouched.plan(kill, clock()).unwrap());
    let ActionPlan::Confirm {
        confirmation_id, ..
    } = actual.plan
    else {
        panic!("confirmation");
    };
    assert_eq!(
        queried.confirm(confirmation_id, clock()).unwrap(),
        untouched.confirm(confirmation_id, clock()).unwrap()
    );
}

#[test]
fn scope_grants_capacity_and_question_identity_are_checked_by_the_same_preflight() {
    let mut core = core(vec![
        GrantTier::Read,
        GrantTier::Control,
        GrantTier::Terminal,
    ]);
    let missing = ActionIntent::Answer {
        scope: scope(),
        question_id: "missing".into(),
        body: "yes".into(),
    };
    let queried = payload(query(&core, missing.clone()).unwrap());
    let ActionPlan::Unavailable { reason } = core.plan(missing, clock()).unwrap().plan else {
        panic!("missing question");
    };
    assert_eq!(queried.reason.unwrap().code, reason.code);
    let answer = ActionIntent::Answer {
        scope: scope(),
        question_id: "question".into(),
        body: "yes".into(),
    };
    assert!(payload(query(&core, answer.clone()).unwrap()).available);
    core.plan(answer.clone(), clock()).unwrap();
    assert!(!payload(query(&core, answer).unwrap()).available);
    for _ in 1..64 {
        assert!(matches!(
            core.plan(input(false), clock()).unwrap().plan,
            ActionPlan::Dispatch { .. }
        ));
    }
    let full = payload(query(&core, send()).unwrap());
    assert_eq!(full.reason.unwrap().code, "operation_capacity");
}

#[test]
fn pending_raw_does_not_block_a_prompt_but_an_existing_prompt_does() {
    let mut core = core(vec![
        GrantTier::Read,
        GrantTier::Control,
        GrantTier::Terminal,
    ]);
    assert!(matches!(
        core.plan(input(false), clock()).unwrap().plan,
        ActionPlan::Dispatch { .. }
    ));
    assert!(payload(query(&core, input(true)).unwrap()).available);
    assert!(matches!(
        core.plan(input(true), clock()).unwrap().plan,
        ActionPlan::Dispatch { .. }
    ));
    assert!(!payload(query(&core, input(true)).unwrap()).available);
    assert!(payload(query(&core, input(false)).unwrap()).available);
}
