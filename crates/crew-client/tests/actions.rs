//! Real planner conformance, independent of native widgets or a transport.

use crew_client::actions::{ActionContext, ActionDecision, ActionUpdate, Actions};
use crew_client::*;
use crew_protocol::rpc::{SendResult, SessionInfo, Snapshot};
use crew_protocol::{
    Agent, AgentStatus, Message, RuntimeKind, Task, UsageLimit, MAX_MESSAGE_BYTES,
};
use serde_json::Value;

#[derive(Clone)]
struct Facts {
    connection: ConnectionContext,
    endpoint: EndpointStatus,
    workspace: WorkspaceStatus,
    agents: Vec<Agent>,
    tasks: Vec<Task>,
    questions: Vec<Message>,
    session: SessionInfo,
}

impl Facts {
    fn new() -> Self {
        let results: Value =
            serde_json::from_str(include_str!("../../crew/tests/fixtures/web/results.json"))
                .unwrap();
        let snapshot: Snapshot = serde_json::from_value(results["snapshot"][0].clone()).unwrap();
        let contexts: Value = serde_json::from_str(include_str!(
            "../../../fixtures/native/actions/contexts.json"
        ))
        .unwrap();
        let question: Message =
            serde_json::from_value(contexts["question"]["question"].clone()).unwrap();
        let corpus: Value = serde_json::from_str(include_str!(
            "../../../fixtures/native/actions/native/terminal-routing.json"
        ))
        .unwrap();
        let scope: ClientScope =
            serde_json::from_value(corpus["cases"][0]["effects"][0]["data"]["scope"].clone())
                .unwrap();
        let mut agents = snapshot.agents;
        // Terminal vectors refer to these two source agents; other agents
        // remain present to exercise unrelated traffic and roster changes.
        let mut first = agents[0].clone();
        first.id = "claude-1".into();
        first.status = AgentStatus::Working;
        first.runtime = RuntimeKind::Claude;
        first.backend = "herdr".into();
        first.limit = None;
        first.paused = false;
        agents.retain(|agent| agent.id != "claude-1" && agent.id != "claude-2");
        let mut second = first.clone();
        second.id = "claude-2".into();
        second.pid = Some(4243);
        agents.insert(0, second);
        agents.insert(0, first);
        let grant = Grant {
            grant_id: "local-operator:1".into(),
            tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
            workspace_ids: vec![scope.workspace_id.clone()],
            expires_at_ms: None,
        };
        Self {
            connection: ConnectionContext {
                scope,
                mode: ConnectionMode::LocalOperator,
                grant,
                snapshot_mode: SnapshotMode::OrderedConnection,
            },
            endpoint: EndpointStatus::Authenticated,
            workspace: WorkspaceStatus::Live,
            agents,
            tasks: snapshot.tasks,
            questions: vec![question],
            session: snapshot.session,
        }
    }

    fn context(&self) -> ActionContext<'_> {
        ActionContext {
            connection: Some(&self.connection),
            endpoint: self.endpoint,
            workspace: self.workspace,
            agents: &self.agents,
            tasks: &self.tasks,
            questions: &self.questions,
            session: Some(&self.session),
        }
    }

    fn scope(&self) -> ClientScope {
        self.connection.scope.clone()
    }
    fn gateway(&mut self, tiers: Vec<GrantTier>) {
        self.connection.mode = ConnectionMode::ScopedGateway;
        self.connection.grant.grant_id = "device:1/grant:2".into();
        self.connection.grant.tiers = tiers;
    }
}

fn now(monotonic_ms: u64) -> ClockSample {
    ClockSample {
        wall_ms: 1_760_000_000_000,
        monotonic_ms,
    }
}
fn planner() -> Actions {
    Actions::new(64).unwrap()
}
fn why(code: &str) -> ClientReason {
    ClientReason {
        details: None,
        code: code.into(),
        message: code.into(),
    }
}
fn unknown(code: &str) -> OperationOutcome {
    OperationOutcome::OutcomeUnknown { reason: why(code) }
}

fn agent_action(facts: &Facts, action: AgentAction) -> ActionIntent {
    ActionIntent::Agent {
        scope: facts.scope(),
        id: "claude-1".into(),
        action,
    }
}
fn message(facts: &Facts) -> ActionIntent {
    ActionIntent::SendMessage {
        scope: facts.scope(),
        to: "claude-1".into(),
        body: "  Keep the rounding rule.\n账目  ".into(),
        message_id: "w-0102030405060708".into(),
    }
}
fn read(facts: &Facts, kind: ResourceKind) -> ActionIntent {
    ActionIntent::Read {
        scope: facts.scope(),
        resource: ResourceRequest {
            key: ResourceKey {
                entity: EntityKey {
                    scope: facts.scope(),
                    entity: Entity::Agent("claude-1".into()),
                },
                kind,
                options: ReadOptions::Default,
            },
        },
    }
}
fn plan(actions: &mut Actions, facts: &Facts, intent: ActionIntent) -> ActionDecision {
    actions.plan(intent, &facts.context(), now(1_000)).unwrap()
}
fn request(decision: &ActionDecision) -> (OperationId, &ClientScope, RequestLane, &Command) {
    let ActionPlan::Dispatch { operation_id } = decision.plan else {
        panic!("not dispatched: {:?}", decision.plan);
    };
    let requests: Vec<_> = decision
        .update
        .effects
        .iter()
        .filter_map(|effect| match effect {
            ClientEffect::Request {
                scope,
                operation_id: id,
                lane,
                command,
            } => Some((*id, scope, *lane, command)),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, operation_id);
    requests[0]
}
fn confirmation(decision: &ActionDecision) -> (ConfirmationId, ConfirmationKind) {
    let ActionPlan::Confirm {
        confirmation_id,
        kind,
        ..
    } = decision.plan
    else {
        panic!("not confirmed: {:?}", decision.plan);
    };
    assert!(!decision
        .update
        .effects
        .iter()
        .any(|effect| matches!(effect, ClientEffect::Request { .. })));
    (confirmation_id, kind)
}
fn unavailable(decision: &ActionDecision) -> &str {
    let ActionPlan::Unavailable { reason } = &decision.plan else {
        panic!("available: {:?}", decision.plan);
    };
    assert!(decision.update.effects.is_empty());
    &reason.code
}
fn no_request(update: &ActionUpdate) {
    assert!(!update
        .effects
        .iter()
        .any(|effect| matches!(effect, ClientEffect::Request { .. })));
}

#[test]
fn exactly_four_actions_confirm_and_every_other_existing_control_dispatches_directly() {
    let mut facts = Facts::new();
    facts.agents[0].limit = Some(UsageLimit {
        kind: "usage".into(),
        since: 123,
        ..UsageLimit::default()
    });
    let expected = [
        (
            agent_action(&facts, AgentAction::Kill),
            ConfirmationKind::KillAgent,
        ),
        (
            agent_action(&facts, AgentAction::RestartFresh),
            ConfirmationKind::RestartFresh,
        ),
        (
            ActionIntent::ResumeLimited {
                scope: facts.scope(),
                restart: true,
            },
            ConfirmationKind::RestartLimited,
        ),
        (
            ActionIntent::StopTeam {
                scope: facts.scope(),
            },
            ConfirmationKind::StopTeam,
        ),
    ];
    for (intent, expected) in expected {
        let mut actions = planner();
        let decision = plan(&mut actions, &facts, intent);
        let (id, kind) = confirmation(&decision);
        assert_eq!(kind, expected);
        assert_eq!(actions.pending_count(), 1);
        let dispatched = actions.confirm(id, &facts.context(), now(1_001)).unwrap();
        assert_eq!(
            dispatched
                .effects
                .iter()
                .filter(|effect| matches!(effect, ClientEffect::Request { .. }))
                .count(),
            1
        );
        assert_eq!(actions.pending_count(), 1);
        assert!(matches!(
            actions.confirm(id, &facts.context(), now(1_002)),
            Err(ClientError::StaleConfirmation)
        ));
    }
    for action in [
        AgentAction::Pause,
        AgentAction::Resume,
        AgentAction::Compact,
        AgentAction::Restart,
        AgentAction::Focus,
    ] {
        request(&plan(&mut planner(), &facts, agent_action(&facts, action)));
    }
    for intent in [
        message(&facts),
        ActionIntent::SetModel {
            scope: facts.scope(),
            id: "claude-1".into(),
            runtime: Some(RuntimeKind::Codex),
            model: String::new(),
            note: None,
        },
        ActionIntent::AgentInput {
            scope: facts.scope(),
            id: "claude-1".into(),
            text: Some("/compact".into()),
            submit: true,
        },
        ActionIntent::ResumeLimited {
            scope: facts.scope(),
            restart: false,
        },
    ] {
        request(&plan(&mut planner(), &facts, intent));
    }
}

#[test]
fn cancel_removes_a_bounded_confirmation_and_later_confirm_never_dispatches() {
    let facts = Facts::new();
    let mut actions = Actions::new(1).unwrap();
    let decision = plan(
        &mut actions,
        &facts,
        agent_action(&facts, AgentAction::Kill),
    );
    let (id, _) = confirmation(&decision);
    assert_eq!(
        unavailable(&plan(&mut actions, &facts, message(&facts))),
        "operation_capacity"
    );
    let canceled = actions.cancel_confirmation(id);
    no_request(&canceled);
    assert!(canceled.changes.pending);
    assert_eq!(actions.pending_count(), 0);
    assert_eq!(actions.cancel_confirmation(id), ActionUpdate::default());
    assert!(matches!(
        actions.confirm(id, &facts.context(), now(1_001)),
        Err(ClientError::StaleConfirmation)
    ));
    request(&plan(&mut actions, &facts, message(&facts)));
    assert!(Actions::new(0).is_err());
    assert!(Actions::new(65).is_err());
}

#[test]
fn confirmations_revalidate_whole_scope_grant_and_target_incarnation_without_ui_retargeting() {
    let original = Facts::new();
    for mutation in 0..10 {
        let mut actions = planner();
        let decision = plan(
            &mut actions,
            &original,
            agent_action(&original, AgentAction::RestartFresh),
        );
        let (id, _) = confirmation(&decision);
        let mut changed = original.clone();
        match mutation {
            0 => changed.connection.scope.workspace_id = "other-workspace".into(),
            1 => changed.connection.scope.endpoint_id = "other-host".into(),
            2 => changed.connection.scope.attempt += 1,
            3 => changed.connection.scope.generation = Some(2),
            4 => changed.connection.grant.grant_id = "revoked-generation".into(),
            5 => changed.agents[0].pid = Some(9999),
            6 => changed.agents[0].session_id = Some("replacement-session".into()),
            7 => {
                changed.agents.remove(0);
            }
            8 => changed.agents[0].backend_ref = Some("replacement-pane".into()),
            9 => changed.agents[0].backend = "pty".into(),
            _ => unreachable!(),
        };
        let update = actions.confirm(id, &changed.context(), now(1_001)).unwrap();
        no_request(&update);
        assert!(update.effects.iter().any(|effect| matches!(effect, ClientEffect::Notice { code, .. } if code == "stale_confirmation")));
        assert_eq!(actions.pending_count(), 0);
    }
    let mut actions = planner();
    let (id, _) = confirmation(&plan(
        &mut actions,
        &original,
        agent_action(&original, AgentAction::Kill),
    ));
    let mut unrelated = original.clone();
    unrelated.agents[1].activity = "another operation progressed".into();
    unrelated.agents[0].activity = "a turn progressed".into();
    unrelated.agents[0].status = AgentStatus::Idle;
    assert!(actions.confirm(id, &unrelated.context(), now(1_001)).unwrap().effects.iter().any(|effect| matches!(effect, ClientEffect::Request { command: Command::AgentKill { id }, .. } if id == "claude-1")));
}

#[test]
fn limit_continue_and_restart_capture_explicit_ids_and_changed_sets_invalidate_confirmation() {
    let mut facts = Facts::new();
    facts.agents[0].limit = Some(UsageLimit {
        since: 1,
        ..UsageLimit::default()
    });
    let direct = plan(
        &mut planner(),
        &facts,
        ActionIntent::ResumeLimited {
            scope: facts.scope(),
            restart: false,
        },
    );
    assert_eq!(
        request(&direct).3,
        &Command::Resume {
            agents: vec!["claude-1".into()],
            restart: false
        }
    );
    let mut actions = planner();
    let (id, _) = confirmation(&plan(
        &mut actions,
        &facts,
        ActionIntent::ResumeLimited {
            scope: facts.scope(),
            restart: true,
        },
    ));
    facts.agents[1].limit = Some(UsageLimit {
        since: 2,
        ..UsageLimit::default()
    });
    no_request(&actions.confirm(id, &facts.context(), now(1_001)).unwrap());
    let (id, _) = confirmation(&plan(
        &mut actions,
        &facts,
        ActionIntent::ResumeLimited {
            scope: facts.scope(),
            restart: true,
        },
    ));
    facts.agents.swap(0, 1); // same set; display order does not change consequences
    let update = actions.confirm(id, &facts.context(), now(1_002)).unwrap();
    let command = update
        .effects
        .iter()
        .find_map(|effect| match effect {
            ClientEffect::Request { command, .. } => Some(command.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        command,
        Command::Resume {
            agents: vec!["claude-1".into(), "claude-2".into()],
            restart: true
        }
    );
    // A later arrival cannot enter an already dispatched command.
    facts.agents[2].limit = Some(UsageLimit::default());
    assert_eq!(command.encode(1).unwrap(), "{\"id\":1,\"method\":\"resume\",\"params\":{\"agents\":[\"claude-1\",\"claude-2\"],\"restart\":true}}");
    for agent in &mut facts.agents {
        agent.limit = None;
    }
    assert_eq!(
        unavailable(&plan(
            &mut planner(),
            &facts,
            ActionIntent::ResumeLimited {
                scope: facts.scope(),
                restart: false
            }
        )),
        "no_limited_agents"
    );
}

#[test]
fn gateway_read_control_and_terminal_are_independent_and_focus_stays_local() {
    let mut facts = Facts::new();
    facts.gateway(vec![GrantTier::Read]);
    request(&plan(
        &mut planner(),
        &facts,
        read(&facts, ResourceKind::Transcript),
    ));
    request(&plan(
        &mut planner(),
        &facts,
        read(&facts, ResourceKind::Inbox),
    ));
    assert_eq!(
        unavailable(&plan(&mut planner(), &facts, message(&facts))),
        "grant_required"
    );
    assert_eq!(
        unavailable(&plan(
            &mut planner(),
            &facts,
            agent_action(&facts, AgentAction::Kill)
        )),
        "grant_required"
    );
    let prompt = ActionIntent::AgentInput {
        scope: facts.scope(),
        id: "claude-1".into(),
        text: Some("hello".into()),
        submit: true,
    };
    assert_eq!(
        unavailable(&plan(&mut planner(), &facts, prompt.clone())),
        "grant_required"
    );
    facts.connection.grant.tiers.push(GrantTier::Terminal);
    request(&plan(&mut planner(), &facts, prompt));
    assert_eq!(
        unavailable(&plan(
            &mut planner(),
            &facts,
            agent_action(&facts, AgentAction::Kill)
        )),
        "grant_required"
    );
    facts.connection.grant.tiers = vec![GrantTier::Read, GrantTier::Control];
    request(&plan(&mut planner(), &facts, message(&facts)));
    assert_eq!(
        unavailable(&plan(
            &mut planner(),
            &facts,
            ActionIntent::Attach {
                scope: facts.scope(),
                agent: "claude-1".into(),
                rows: 24,
                cols: 80
            }
        )),
        "grant_required"
    );
    facts.connection.grant.tiers.push(GrantTier::Terminal);
    assert_eq!(
        unavailable(&plan(
            &mut planner(),
            &facts,
            agent_action(&facts, AgentAction::Focus)
        )),
        "local_only"
    );
    facts.connection.grant.workspace_ids.clear();
    assert_eq!(
        unavailable(&plan(
            &mut planner(),
            &facts,
            read(&facts, ResourceKind::Transcript)
        )),
        "workspace_not_granted"
    );
}

#[test]
fn model_evidence_requires_control_action_lane_and_still_registers_resource_lifecycle() {
    let mut facts = Facts::new();
    let intent = ActionIntent::Read {
        scope: facts.scope(),
        resource: ResourceRequest {
            key: ResourceKey {
                entity: EntityKey {
                    scope: facts.scope(),
                    entity: Entity::Workspace,
                },
                kind: ResourceKind::Models,
                options: ReadOptions::Default,
            },
        },
    };
    facts.gateway(vec![GrantTier::Read]);
    assert_eq!(
        unavailable(&plan(&mut planner(), &facts, intent.clone())),
        "grant_required"
    );
    facts.connection.grant.tiers.push(GrantTier::Control);
    let result = plan(&mut planner(), &facts, intent);
    assert_eq!(request(&result).2, RequestLane::Action);
    assert_eq!(request(&result).3, &Command::ModelsList);
    assert_eq!(result.update.reads_started.len(), 1);
    assert_eq!(
        result.update.reads_started[0].key.kind,
        ResourceKind::Models
    );
}

#[test]
fn authenticated_endpoint_without_live_snapshot_or_expired_authority_cannot_mutate() {
    let mut facts = Facts::new();
    for status in [
        WorkspaceStatus::Connecting,
        WorkspaceStatus::Reconnecting,
        WorkspaceStatus::AccessRevoked,
        WorkspaceStatus::Stopped,
    ] {
        facts.workspace = status;
        assert_eq!(
            unavailable(&plan(&mut planner(), &facts, message(&facts))),
            "not_live"
        );
    }
    facts.workspace = WorkspaceStatus::Live;
    facts.endpoint = EndpointStatus::SignedOut;
    assert_eq!(
        unavailable(&plan(&mut planner(), &facts, message(&facts))),
        "not_live"
    );
    facts.endpoint = EndpointStatus::Authenticated;
    facts.connection.grant.expires_at_ms = Some(now(0).wall_ms);
    assert_eq!(
        unavailable(&plan(&mut planner(), &facts, message(&facts))),
        "grant_expired"
    );
}

#[test]
fn message_validation_preserves_utf8_bytes_and_never_sends_unknown_slash_as_objective() {
    let facts = Facts::new();
    for text in ["", "   \n", "/stop", "/unknown argument"] {
        let intent = ActionIntent::SendMessage {
            scope: facts.scope(),
            to: "claude-1".into(),
            body: text.into(),
            message_id: "w-id".into(),
        };
        let mut actions = planner();
        assert!(matches!(
            actions.plan(intent, &facts.context(), now(0)),
            Err(ClientError::InvalidInput { .. })
        ));
        assert_eq!(actions.pending_count(), 0);
    }
    for text in [" /stop", "/src/main.rs is slow", "é账目"] {
        let intent = ActionIntent::SendMessage {
            scope: facts.scope(),
            to: "claude-1".into(),
            body: text.into(),
            message_id: "w-id".into(),
        };
        let result = plan(&mut planner(), &facts, intent);
        let Command::Send(sent) = request(&result).3 else {
            panic!()
        };
        assert_eq!(sent.body, text);
        assert_eq!(sent.kind, crew_protocol::MessageType::Message);
    }
    let too_large = ActionIntent::SendMessage {
        scope: facts.scope(),
        to: "*".into(),
        body: "é".repeat(MAX_MESSAGE_BYTES / 2 + 1),
        message_id: "w-id".into(),
    };
    assert!(
        matches!(planner().plan(too_large, &facts.context(), now(0)), Err(ClientError::TooLarge { actual, limit }) if actual > limit && limit == MAX_MESSAGE_BYTES as u64)
    );
    let exact = ActionIntent::SendMessage {
        scope: facts.scope(),
        to: "*".into(),
        body: "a".repeat(MAX_MESSAGE_BYTES),
        message_id: "w-id".into(),
    };
    request(&plan(&mut planner(), &facts, exact));
    for id in ["", "bad/id", "é", &"a".repeat(65)] {
        let intent = ActionIntent::SendMessage {
            scope: facts.scope(),
            to: "*".into(),
            body: "body".into(),
            message_id: id.into(),
        };
        assert!(planner().plan(intent, &facts.context(), now(0)).is_err());
    }
}

#[test]
fn done_agent_still_compacts_but_stopped_failed_and_shell_have_source_grounded_reasons() {
    let mut facts = Facts::new();
    facts.agents[0].status = AgentStatus::Done;
    request(&plan(
        &mut planner(),
        &facts,
        agent_action(&facts, AgentAction::Compact),
    ));
    for status in [AgentStatus::Stopped, AgentStatus::Failed] {
        facts.agents[0].status = status;
        assert_eq!(
            unavailable(&plan(
                &mut planner(),
                &facts,
                agent_action(&facts, AgentAction::Compact)
            )),
            "agent_not_running"
        );
    }
    facts.agents[0].status = AgentStatus::Working;
    facts.agents[0].runtime = RuntimeKind::Shell;
    assert_eq!(
        unavailable(&plan(
            &mut planner(),
            &facts,
            agent_action(&facts, AgentAction::Compact)
        )),
        "compact_unsupported"
    );
}

#[test]
fn send_retries_once_same_logical_id_and_payload_only_after_verified_same_incarnation_snapshot() {
    let mut facts = Facts::new();
    let original = facts.scope();
    let mut actions = planner();
    let first = plan(&mut actions, &facts, message(&facts));
    let (id, _, _, command) = request(&first);
    let bytes = command.encode(77).unwrap();
    let waiting = actions
        .finish(
            &original,
            id,
            unknown("closed"),
            &facts.context(),
            now(2_000),
        )
        .unwrap();
    assert!(waiting.completions.is_empty());
    assert_eq!(waiting.effects.len(), 1);
    assert!(matches!(
        waiting.effects[0],
        ClientEffect::Schedule {
            deadline_ms: 32_000,
            ..
        }
    ));
    assert_eq!(actions.pending_count(), 1);
    no_request(
        &actions
            .resynchronized(&facts.context(), now(2_001))
            .unwrap(),
    );
    no_request(
        &actions
            .retire(&original, why("closed"), now(2_002))
            .unwrap(),
    );
    facts.connection.scope.attempt += 1;
    let replay = actions
        .resynchronized(&facts.context(), now(2_010))
        .unwrap();
    let replay_request = replay
        .effects
        .iter()
        .find_map(|effect| match effect {
            ClientEffect::Request {
                scope,
                operation_id,
                command,
                ..
            } => Some((scope, operation_id, command)),
            _ => None,
        })
        .unwrap();
    assert_eq!(*replay_request.1, id);
    assert_eq!(replay_request.0, &facts.scope());
    assert_eq!(replay_request.2.encode(77).unwrap(), bytes);
    assert_eq!(
        actions
            .finish(
                &original,
                id,
                OperationOutcome::Rejected {
                    details: None,
                    code: "invalid".into(),
                    message: "old reply".into()
                },
                &facts.context(),
                now(2_011)
            )
            .unwrap(),
        ActionUpdate::default()
    );
    let final_update = actions
        .finish(
            &facts.scope(),
            id,
            unknown("closed"),
            &facts.context(),
            now(2_012),
        )
        .unwrap();
    assert_eq!(final_update.completions.len(), 1);
    assert_eq!(
        final_update.completions[0].notice().kind,
        OperationOutcomeKind::Unknown
    );
    assert_eq!(final_update.completions[0].scope, facts.scope());
    assert!(final_update.effects.is_empty());
    assert_eq!(actions.pending_count(), 0);
    assert_eq!(
        actions
            .finish(
                &facts.scope(),
                id,
                unknown("closed"),
                &facts.context(),
                now(2_013)
            )
            .unwrap(),
        ActionUpdate::default()
    );
}

#[test]
fn retry_expiry_deadline_never_extends_and_clock_boundaries_do_not_panic() {
    let facts = Facts::new();
    let mut actions = planner();
    let id = request(&plan(&mut actions, &facts, message(&facts))).0;
    let waiting = actions
        .finish(
            &facts.scope(),
            id,
            unknown("closed"),
            &facts.context(),
            now(u64::MAX - 10),
        )
        .unwrap();
    let ClientEffect::Schedule {
        timer_id,
        deadline_ms,
    } = waiting.effects[0]
    else {
        panic!()
    };
    assert_eq!(deadline_ms, u64::MAX);
    assert_eq!(
        actions.wake(timer_id, now(u64::MAX - 1)),
        ActionUpdate::default()
    );
    actions
        .retire(&facts.scope(), why("closed"), now(u64::MAX - 5))
        .unwrap();
    let expired = actions.wake(timer_id, now(u64::MAX));
    assert_eq!(expired.completions.len(), 1);
    assert_eq!(expired.completions[0].operation_id, id);
    assert_eq!(
        expired.completions[0].notice().kind,
        OperationOutcomeKind::Unknown
    );
    no_request(&expired);
    assert_eq!(
        actions.wake(timer_id, now(u64::MAX)),
        ActionUpdate::default()
    );
}

#[test]
fn a_new_attempt_cannot_replace_an_identified_send_payload_while_its_retry_is_held() {
    let mut facts = Facts::new();
    let mut actions = planner();
    let first = plan(&mut actions, &facts, message(&facts));
    actions
        .finish(
            &facts.scope(),
            request(&first).0,
            unknown("closed"),
            &facts.context(),
            now(2_000),
        )
        .unwrap();
    facts.connection.scope.attempt += 1;
    let mut changed = message(&facts);
    let ActionIntent::SendMessage { body, to, .. } = &mut changed else {
        panic!()
    };
    *body = "a different objective".into();
    *to = "claude-2".into();
    assert_eq!(
        unavailable(&plan(&mut actions, &facts, changed)),
        "already_pending"
    );
    let replay = actions
        .resynchronized(&facts.context(), now(2_001))
        .unwrap();
    let Command::Send(params) = replay
        .effects
        .iter()
        .find_map(|effect| match effect {
            ClientEffect::Request { command, .. } => Some(command),
            _ => None,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(params.to, "claude-1");
    assert_eq!(params.body, "  Keep the rounding rule.\n账目  ");
}

#[test]
fn changed_incarnation_scope_or_grant_and_missing_identity_never_authorize_retry() {
    for mutation in 0..8 {
        let mut facts = Facts::new();
        let mut actions = planner();
        let id = request(&plan(&mut actions, &facts, message(&facts))).0;
        actions
            .finish(
                &facts.scope(),
                id,
                unknown("closed"),
                &facts.context(),
                now(2_000),
            )
            .unwrap();
        facts.connection.scope.attempt += 1;
        match mutation {
            0 => facts.connection.scope.endpoint_id = "other".into(),
            1 => facts.connection.scope.workspace_id = "other".into(),
            2 => facts.connection.scope.generation = Some(3),
            3 => facts.connection.scope.daemon_pid = Some(3),
            4 => facts.connection.scope.daemon_started_at_ms = Some(3),
            5 => facts.connection.grant.grant_id = "new-generation".into(),
            6 => facts.connection.grant.expires_at_ms = Some(now(0).wall_ms),
            7 => {
                facts.agents.remove(0);
            }
            _ => unreachable!(),
        };
        let final_update = actions
            .resynchronized(&facts.context(), now(2_001))
            .unwrap();
        no_request(&final_update);
        assert_eq!(final_update.completions.len(), 1);
        assert_eq!(actions.pending_count(), 0);
    }
    let mut facts = Facts::new();
    facts.connection.scope.daemon_started_at_ms = None;
    let mut actions = planner();
    let id = request(&plan(&mut actions, &facts, message(&facts))).0;
    let final_update = actions
        .finish(
            &facts.scope(),
            id,
            unknown("closed"),
            &facts.context(),
            now(2_000),
        )
        .unwrap();
    assert!(final_update.effects.is_empty());
    assert_eq!(final_update.completions.len(), 1);
}

#[test]
fn timeout_refusal_cancel_and_auth_loss_finish_without_replay() {
    let facts = Facts::new();
    for outcome in [
        unknown("timeout"),
        unknown("partial_write"),
        unknown("unauthorized"),
        OperationOutcome::Canceled,
        OperationOutcome::NotSent {
            reason: why("not_sent"),
        },
        OperationOutcome::Rejected {
            details: None,
            code: "forbidden".into(),
            message: "not permitted".into(),
        },
    ] {
        let mut actions = planner();
        let id = request(&plan(&mut actions, &facts, message(&facts))).0;
        let final_update = actions
            .finish(
                &facts.scope(),
                id,
                outcome.clone(),
                &facts.context(),
                now(2_000),
            )
            .unwrap();
        assert_eq!(final_update.completions[0].outcome, outcome);
        assert_eq!(actions.pending_count(), 0);
        assert!(final_update.effects.is_empty());
    }
    let mut actions = planner();
    let id = request(&plan(&mut actions, &facts, message(&facts))).0;
    actions
        .finish(
            &facts.scope(),
            id,
            unknown("closed"),
            &facts.context(),
            now(2_000),
        )
        .unwrap();
    let retired = actions
        .retire(&facts.scope(), why("revoked"), now(2_001))
        .unwrap();
    assert_eq!(retired.completions.len(), 1);
    no_request(&retired);
    assert_eq!(actions.pending_count(), 0);
}

#[test]
fn answer_pending_restores_on_failure_and_neither_answers_nor_prompts_replay_on_close() {
    let facts = Facts::new();
    assert!(!facts.questions.is_empty());
    let question = &facts.questions[0];
    let mut actions = planner();
    let answer = ActionIntent::Answer {
        scope: facts.scope(),
        question_id: question.id.clone(),
        body: "Keep the adapter".into(),
    };
    let first = plan(&mut actions, &facts, answer.clone());
    let id = request(&first).0;
    assert_eq!(
        actions.pending_answer_ids().collect::<Vec<_>>(),
        [question.id.as_str()]
    );
    assert!(first.update.changes.questions);
    assert_eq!(
        unavailable(&plan(&mut actions, &facts, answer)),
        "already_pending"
    );
    let failed = actions
        .finish(
            &facts.scope(),
            id,
            unknown("closed"),
            &facts.context(),
            now(2_000),
        )
        .unwrap();
    assert_eq!(failed.completions.len(), 1);
    assert!(failed.changes.questions);
    assert!(failed.effects.is_empty());
    assert_eq!(actions.pending_answer_ids().count(), 0);
    let prompt = ActionIntent::AgentInput {
        scope: facts.scope(),
        id: "claude-1".into(),
        text: Some("body".into()),
        submit: true,
    };
    let id = request(&plan(&mut actions, &facts, prompt)).0;
    assert!(actions
        .finish(
            &facts.scope(),
            id,
            unknown("closed"),
            &facts.context(),
            now(2_001)
        )
        .unwrap()
        .effects
        .is_empty());
}

#[test]
fn slow_mutation_does_not_block_read_or_terminal_lanes_and_reads_keep_captured_key() {
    let facts = Facts::new();
    let mut actions = planner();
    let (confirmation, _) = confirmation(&plan(
        &mut actions,
        &facts,
        agent_action(&facts, AgentAction::Kill),
    ));
    let slow = actions
        .confirm(confirmation, &facts.context(), now(1_001))
        .unwrap();
    assert!(matches!(
        slow.effects[0],
        ClientEffect::Request {
            lane: RequestLane::Action,
            ..
        }
    ));
    let evidence = plan(&mut actions, &facts, read(&facts, ResourceKind::Transcript));
    assert_eq!(request(&evidence).2, RequestLane::Read);
    assert_eq!(
        evidence.update.reads_started[0].key.options,
        ReadOptions::Bounded { limit: 400 }
    );
    let terminal = plan(
        &mut actions,
        &facts,
        ActionIntent::Attach {
            scope: facts.scope(),
            agent: "claude-2".into(),
            rows: 24,
            cols: 80,
        },
    );
    let id = request(&terminal).0;
    assert_eq!(
        request(&terminal).2,
        RequestLane::Terminal { attachment_id: id }
    );
    assert_eq!(actions.pending_count(), 3);
    let retired = actions
        .retire(&facts.scope(), why("workspace_changed"), now(2_000))
        .unwrap();
    assert_eq!(retired.completions.len(), 3);
    let read_completion = retired
        .completions
        .iter()
        .find(|completion| completion.resource.is_some())
        .unwrap();
    assert_eq!(
        read_completion.resource,
        Some(evidence.update.reads_started[0].key.clone())
    );
    assert_eq!(read_completion.outcome, OperationOutcome::Canceled);
    assert_eq!(actions.pending_count(), 0);
}

#[test]
fn method_results_can_finish_once_and_notices_keep_explicit_daemon_or_local_meaning() {
    let facts = Facts::new();
    let mut actions = planner();
    let id = request(&plan(&mut actions, &facts, message(&facts))).0;
    let fixtures: Value =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/results.json")).unwrap();
    let mut result: SendResult = serde_json::from_value(fixtures["send"][0].clone()).unwrap();
    result.message.id = "w-0102030405060708".into();
    let outcome = OperationOutcome::Succeeded {
        result: CommandResult::Sent(Box::new(result)),
    };
    let update = actions
        .finish(
            &facts.scope(),
            id,
            outcome.clone(),
            &facts.context(),
            now(1_001),
        )
        .unwrap();
    assert_eq!(update.completions.len(), 1);
    assert_eq!(
        update.completions[0].notice().kind,
        OperationOutcomeKind::Success
    );
    assert_eq!(update.completions[0].notice().attachment_id, None);
    assert_eq!(
        actions
            .finish(&facts.scope(), id, outcome, &facts.context(), now(1_002))
            .unwrap(),
        ActionUpdate::default()
    );
}

fn native_case(id: &str) -> Value {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../fixtures/native/actions/native/terminal-routing.json"
    ))
    .unwrap();
    corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap()
        .clone()
}

fn two_attachments(actions: &mut Actions, facts: &Facts) -> (AttachmentId, AttachmentId) {
    let case = native_case("native-terminal-two-attach");
    let intents: Vec<ActionIntent> = serde_json::from_value(case["intents"].clone()).unwrap();
    let mut ids = Vec::new();
    for intent in intents {
        let decision = plan(actions, facts, intent);
        let (id, scope, lane, command) = request(&decision);
        assert_eq!(scope, &facts.scope());
        assert_eq!(lane, RequestLane::Terminal { attachment_id: id });
        assert!(!actions.attachment_is_live(scope, id));
        let Command::Attach { agent, .. } = command else {
            panic!()
        };
        let acknowledged = actions
            .finish(
                scope,
                id,
                OperationOutcome::Succeeded {
                    result: CommandResult::Attached {
                        agent: agent.clone(),
                    },
                },
                &facts.context(),
                now(1_001),
            )
            .unwrap();
        assert_eq!(acknowledged.completions.len(), 1);
        assert!(actions.attachment_is_live(scope, id));
        ids.push(id);
    }
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    (ids[0], ids[1])
}

fn remap_attachment(
    mut intent: ActionIntent,
    first: AttachmentId,
    second: AttachmentId,
) -> ActionIntent {
    let id = match &mut intent {
        ActionIntent::AttachInput { attachment_id, .. }
        | ActionIntent::AttachResize { attachment_id, .. }
        | ActionIntent::Detach { attachment_id, .. } => attachment_id,
        _ => panic!("not attachment traffic"),
    };
    *id = match *id {
        101 => first,
        102 => second,
        _ => *id,
    };
    intent
}

#[test]
fn native_terminal_two_attachment_input_resize_and_one_way_write_cases_run_real_planner() {
    let facts = Facts::new();
    let mut actions = planner();
    let (first, second) = two_attachments(&mut actions, &facts);
    for (case_id, expected_attachment) in [
        ("native-terminal-route-input", first),
        ("native-terminal-route-resize", second),
    ] {
        let case = native_case(case_id);
        let intent: ActionIntent = serde_json::from_value(case["intents"][0].clone()).unwrap();
        let result = plan(
            &mut actions,
            &facts,
            remap_attachment(intent, first, second),
        );
        let (operation_id, _, lane, command) = request(&result);
        assert_eq!(
            lane,
            RequestLane::Terminal {
                attachment_id: expected_attachment
            }
        );
        assert_ne!(operation_id, expected_attachment);
        assert_eq!(
            serde_json::to_value(command).unwrap(),
            case["effects"][0]["data"]["command"]
        );
        let update = actions
            .finish(
                &facts.scope(),
                operation_id,
                OperationOutcome::SentUnacknowledged {
                    attachment_id: expected_attachment,
                },
                &facts.context(),
                now(1_002),
            )
            .unwrap();
        assert_eq!(update.completions.len(), 1);
        let notice = update.completions[0].notice();
        assert_eq!(notice.kind, OperationOutcomeKind::SentUnacknowledged);
        assert_eq!(notice.attachment_id, Some(expected_attachment));
        assert!(update.effects.is_empty());
        assert_eq!(actions.pending_count(), 0);
        assert!(actions.attachment_is_live(&facts.scope(), first));
        assert!(actions.attachment_is_live(&facts.scope(), second));
    }
    let case = native_case("native-terminal-input-unacked");
    let intent: ActionIntent = serde_json::from_value(case["intents"][0].clone()).unwrap();
    let result = plan(
        &mut actions,
        &facts,
        remap_attachment(intent, first, second),
    );
    let id = request(&result).0;
    let update = actions
        .finish(
            &facts.scope(),
            id,
            OperationOutcome::SentUnacknowledged {
                attachment_id: first,
            },
            &facts.context(),
            now(1_003),
        )
        .unwrap();
    assert_eq!(update.completions[0].notice().attachment_id, Some(first));
    assert_eq!(
        update.completions[0].notice().kind,
        OperationOutcomeKind::SentUnacknowledged
    );
    assert_eq!(actions.pending_count(), 0);
    assert!(actions
        .finish(
            &facts.scope(),
            id,
            OperationOutcome::Succeeded {
                result: CommandResult::Ok
            },
            &facts.context(),
            now(1_004)
        )
        .unwrap()
        .completions
        .is_empty());
}

#[test]
fn native_terminal_close_one_stale_id_and_duplicate_close_cases_run_real_planner() {
    let facts = Facts::new();
    let mut actions = planner();
    let (first, second) = two_attachments(&mut actions, &facts);
    let case = native_case("native-terminal-close-one");
    let intent: ActionIntent = serde_json::from_value(case["intents"][0].clone()).unwrap();
    let decision = plan(
        &mut actions,
        &facts,
        remap_attachment(intent, first, second),
    );
    let ActionPlan::Dispatch { operation_id } = decision.plan else {
        panic!()
    };
    assert_eq!(
        decision.update.effects,
        [ClientEffect::CloseAttachment {
            scope: facts.scope(),
            operation_id,
            attachment_id: first
        }]
    );
    assert!(!actions.attachment_is_live(&facts.scope(), first));
    assert!(actions.attachment_is_live(&facts.scope(), second));
    let outcome = OperationOutcome::ClosedLocally {
        attachment_id: first,
    };
    let update = actions
        .finish(
            &facts.scope(),
            operation_id,
            outcome.clone(),
            &facts.context(),
            now(1_005),
        )
        .unwrap();
    assert_eq!(update.completions.len(), 1);
    assert_eq!(
        update.completions[0].notice().kind,
        OperationOutcomeKind::ClosedLocally
    );
    assert_eq!(update.completions[0].notice().attachment_id, Some(first));
    assert_ne!(operation_id, first);
    native_case("native-terminal-duplicate-close");
    assert_eq!(
        actions
            .finish(
                &facts.scope(),
                operation_id,
                outcome,
                &facts.context(),
                now(1_006)
            )
            .unwrap(),
        ActionUpdate::default()
    );
    assert_eq!(
        actions.attachment_ended(&facts.scope(), first),
        ActionUpdate::default()
    );
    assert_eq!(
        actions.attachment_ended(&facts.scope(), first),
        ActionUpdate::default()
    );
    let case = native_case("native-terminal-stale-id");
    for value in case["intents"].as_array().unwrap() {
        let intent: ActionIntent = serde_json::from_value(value.clone()).unwrap();
        assert!(matches!(
            plan(&mut actions, &facts, intent).plan,
            ActionPlan::Unavailable { .. }
        ));
    }
    for intent in [
        ActionIntent::AttachInput {
            scope: facts.scope(),
            attachment_id: first,
            data: vec![1],
        },
        ActionIntent::AttachResize {
            scope: facts.scope(),
            attachment_id: first,
            rows: 20,
            cols: 80,
        },
        ActionIntent::Detach {
            scope: facts.scope(),
            attachment_id: first,
        },
    ] {
        assert_eq!(
            unavailable(&plan(&mut actions, &facts, intent)),
            "attachment_unavailable"
        );
    }
}

#[test]
fn native_terminal_unacked_attach_and_scope_retirement_cases_run_real_planner() {
    let facts = Facts::new();
    let case = native_case("native-terminal-unacked-attach");
    for value in case["outcomes"].as_array().unwrap() {
        let outcome: OperationOutcome = serde_json::from_value(value.clone()).unwrap();
        let mut actions = planner();
        let attach = plan(
            &mut actions,
            &facts,
            ActionIntent::Attach {
                scope: facts.scope(),
                agent: "claude-1".into(),
                rows: 0,
                cols: u16::MAX,
            },
        );
        let (id, _, _, command) = request(&attach);
        assert_eq!(
            command,
            &Command::Attach {
                agent: "claude-1".into(),
                rows: 5,
                cols: 1_000
            }
        );
        assert_eq!(
            unavailable(&plan(
                &mut actions,
                &facts,
                ActionIntent::AttachInput {
                    scope: facts.scope(),
                    attachment_id: id,
                    data: vec![1]
                }
            )),
            "attachment_unavailable"
        );
        let update = actions
            .finish(&facts.scope(), id, outcome, &facts.context(), now(1_002))
            .unwrap();
        assert_eq!(update.completions.len(), 1);
        assert_eq!(update.effects.iter().filter(|effect| matches!(effect, ClientEffect::CloseAttachment { attachment_id, .. } if *attachment_id == id)).count(), 1);
        assert!(!actions.attachment_is_live(&facts.scope(), id));
        assert_eq!(actions.pending_count(), 0);
    }
    native_case("native-terminal-retire-scope");
    let mut actions = planner();
    let (first, second) = two_attachments(&mut actions, &facts);
    let update = actions
        .retire(&facts.scope(), why("revoked"), now(2_000))
        .unwrap();
    assert_eq!(update.effects.iter().filter(|effect| matches!(effect, ClientEffect::CloseAttachment { scope, .. } if scope == &facts.scope())).count(), 2);
    assert!(!actions.attachment_is_live(&facts.scope(), first));
    assert!(!actions.attachment_is_live(&facts.scope(), second));
    assert_eq!(
        actions
            .retire(&facts.scope(), why("revoked"), now(2_001))
            .unwrap(),
        ActionUpdate::default()
    );
}

#[test]
fn native_terminal_input_error_ends_only_captured_attachment_and_never_emits_second_outcome() {
    let facts = Facts::new();
    let mut actions = planner();
    let (first, second) = two_attachments(&mut actions, &facts);
    let case = native_case("native-terminal-input-error-ends");
    assert_eq!(case["expected_behavior"]["second_operation_outcome"], false);
    let input = plan(
        &mut actions,
        &facts,
        ActionIntent::AttachInput {
            scope: facts.scope(),
            attachment_id: first,
            data: vec![0, 27, 255],
        },
    );
    let id = request(&input).0;
    let written = actions
        .finish(
            &facts.scope(),
            id,
            OperationOutcome::SentUnacknowledged {
                attachment_id: first,
            },
            &facts.context(),
            now(1_003),
        )
        .unwrap();
    assert_eq!(written.completions.len(), 1);
    let ended = actions.attachment_ended(&facts.scope(), first);
    assert!(ended.changes.availability);
    assert!(ended.completions.is_empty());
    assert_eq!(
        actions.attachment_ended(&facts.scope(), first),
        ActionUpdate::default()
    );
    let late_error = OperationOutcome::Rejected {
        details: None,
        code: "unavailable".into(),
        message: "terminal attachment ended".into(),
    };
    assert_eq!(
        actions
            .finish(&facts.scope(), id, late_error, &facts.context(), now(1_004))
            .unwrap(),
        ActionUpdate::default()
    );
    assert!(actions.attachment_is_live(&facts.scope(), second));
    assert_eq!(
        unavailable(&plan(
            &mut actions,
            &facts,
            ActionIntent::AttachInput {
                scope: facts.scope(),
                attachment_id: first,
                data: vec![1]
            }
        )),
        "attachment_unavailable"
    );
    // A partial write is uncertain, never a reply and never a replay trigger.
    let input = plan(
        &mut actions,
        &facts,
        ActionIntent::AttachInput {
            scope: facts.scope(),
            attachment_id: second,
            data: vec![1],
        },
    );
    let update = actions
        .finish(
            &facts.scope(),
            request(&input).0,
            unknown("partial_write"),
            &facts.context(),
            now(1_005),
        )
        .unwrap();
    assert_eq!(
        update.completions[0].notice().kind,
        OperationOutcomeKind::Unknown
    );
    assert!(update.effects.is_empty());
}

#[test]
fn canonical_read_keys_preserve_scope_and_options_and_reject_mismatched_entities() {
    use crew_client::actions::{canonical_resource_key, resource_command};
    let facts = Facts::new();
    let base = ResourceKey {
        entity: EntityKey {
            scope: facts.scope(),
            entity: Entity::Agent("claude-1".into()),
        },
        kind: ResourceKind::Transcript,
        options: ReadOptions::Default,
    };
    let canonical = canonical_resource_key(&base).unwrap();
    assert_eq!(canonical.options, ReadOptions::Bounded { limit: 400 });
    assert_eq!(canonical_resource_key(&canonical).unwrap(), canonical);
    let mut actions = planner();
    let first = plan(
        &mut actions,
        &facts,
        ActionIntent::Read {
            scope: facts.scope(),
            resource: ResourceRequest { key: base.clone() },
        },
    );
    let changed = ResourceKey {
        options: ReadOptions::Bounded { limit: 401 },
        ..base.clone()
    };
    let second = plan(
        &mut actions,
        &facts,
        ActionIntent::Read {
            scope: facts.scope(),
            resource: ResourceRequest { key: changed },
        },
    );
    assert_ne!(
        first.update.reads_started[0].key,
        second.update.reads_started[0].key
    );
    for limit in [0, 2_001, u32::MAX] {
        assert!(canonical_resource_key(&ResourceKey {
            options: ReadOptions::Bounded { limit },
            ..base.clone()
        })
        .is_err());
    }
    assert!(canonical_resource_key(&ResourceKey {
        options: ReadOptions::Diff { against: None },
        ..base.clone()
    })
    .is_err());
    assert!(canonical_resource_key(&ResourceKey {
        entity: EntityKey {
            scope: facts.scope(),
            entity: Entity::Workspace
        },
        ..base
    })
    .is_err());
    let output = ResourceKey {
        entity: EntityKey {
            scope: facts.scope(),
            entity: Entity::Agent("claude-1".into()),
        },
        kind: ResourceKind::Output,
        options: ReadOptions::Output {
            lines: 300,
            screen: false,
            ansi: true,
        },
    };
    assert_eq!(
        canonical_resource_key(&output).unwrap().options,
        ReadOptions::Output {
            lines: 300,
            screen: true,
            ansi: true
        }
    );
    let inbox = ResourceKey {
        kind: ResourceKind::Inbox,
        options: ReadOptions::Default,
        ..output.clone()
    };
    assert!(resource_command(&inbox)
        .unwrap()
        .unwrap()
        .encode(1)
        .unwrap()
        .contains("\"unread_only\":false,\"ack\":false"));
    let mut wrong_scope = inbox;
    wrong_scope.entity.scope.workspace_id = "other".into();
    assert_eq!(
        unavailable(&plan(
            &mut actions,
            &facts,
            ActionIntent::Read {
                scope: facts.scope(),
                resource: ResourceRequest { key: wrong_scope }
            }
        )),
        "stale_scope"
    );
    let event = ResourceKey {
        entity: EntityKey {
            scope: facts.scope(),
            entity: Entity::Event(1),
        },
        kind: ResourceKind::Event,
        options: ReadOptions::Default,
    };
    assert_eq!(resource_command(&event).unwrap(), None);
    assert_eq!(
        unavailable(&plan(
            &mut actions,
            &facts,
            ActionIntent::Read {
                scope: facts.scope(),
                resource: ResourceRequest { key: event }
            }
        )),
        "local_evidence"
    );
}

#[test]
fn availability_reasons_follow_planner_scope_and_grants_without_synthetic_model_status() {
    let mut facts = Facts::new();
    let mut actions = planner();
    let available = actions.availability(
        ActionKind::Compact,
        Some("claude-1"),
        &facts.context(),
        now(0),
    );
    assert!(available.available);
    assert!(available.reason.is_none());
    facts.gateway(vec![GrantTier::Read]);
    let unavailable = actions.availability(
        ActionKind::SetModel,
        Some("claude-1"),
        &facts.context(),
        now(0),
    );
    assert!(!unavailable.available);
    assert_eq!(unavailable.reason.unwrap().code, "grant_required");
    facts.connection.mode = ConnectionMode::LocalOperator;
    let first = plan(
        &mut actions,
        &facts,
        agent_action(&facts, AgentAction::Compact),
    );
    let unavailable = actions.availability(
        ActionKind::Compact,
        Some("claude-1"),
        &facts.context(),
        now(0),
    );
    assert_eq!(unavailable.reason.unwrap().code, "already_pending");
    actions
        .finish(
            &facts.scope(),
            request(&first).0,
            OperationOutcome::Rejected {
                details: None,
                code: "invalid".into(),
                message: "compaction refused".into(),
            },
            &facts.context(),
            now(1),
        )
        .unwrap();
    facts.workspace = WorkspaceStatus::AccessRevoked;
    let unavailable = actions.availability(
        ActionKind::Compact,
        Some("claude-1"),
        &facts.context(),
        now(2),
    );
    assert_eq!(unavailable.reason.unwrap().code, "not_live");
}

#[test]
fn trusted_invalidation_of_a_new_provisional_attempt_finalizes_old_retry_on_abandonment() {
    for code in [
        "revoked",
        "access_revoked",
        "unauthorized",
        "signed_out",
        "binding_changed",
        "workspace_changed",
        "inactive",
        "disposed",
    ] {
        let mut facts = Facts::new();
        let proven = facts.scope();
        let mut actions = planner();
        let id = request(&plan(&mut actions, &facts, message(&facts))).0;
        let held = actions
            .finish(&proven, id, unknown("closed"), &facts.context(), now(2_000))
            .unwrap();
        let ClientEffect::Schedule {
            timer_id,
            deadline_ms,
        } = held.effects[0]
        else {
            panic!()
        };
        facts.connection.scope.attempt += 1;
        facts.connection.scope.daemon_started_at_ms = None;
        facts.workspace = WorkspaceStatus::Connecting;
        let retired = actions.invalidate_retries(&facts.scope(), why(code));
        assert_eq!(retired.completions.len(), 1, "{code}");
        assert_eq!(retired.completions[0].operation_id, id);
        assert_eq!(retired.completions[0].scope, proven);
        assert_eq!(retired.completions[0].outcome, unknown(code));
        assert_eq!(retired.effects, [ClientEffect::CancelTimer { timer_id }]);
        assert_eq!(actions.pending_count(), 0);
        assert_eq!(
            actions.wake(timer_id, now(deadline_ms)),
            ActionUpdate::default()
        );
        assert_eq!(
            actions.invalidate_retries(&facts.scope(), why(code)),
            ActionUpdate::default()
        );
        facts.connection.scope.attempt += 1;
        facts.connection.scope.daemon_started_at_ms = proven.daemon_started_at_ms;
        facts.workspace = WorkspaceStatus::Live;
        assert_eq!(
            actions
                .resynchronized(&facts.context(), now(2_003))
                .unwrap(),
            ActionUpdate::default()
        );
    }
}

#[test]
fn verification_normalization_preserves_held_retry_and_its_original_deadline() {
    let mut facts = Facts::new();
    let proven = facts.scope();
    let mut actions = planner();
    let first = plan(&mut actions, &facts, message(&facts));
    let id = request(&first).0;
    let held = actions
        .finish(&proven, id, unknown("closed"), &facts.context(), now(2_000))
        .unwrap();
    let ClientEffect::Schedule {
        timer_id,
        deadline_ms,
    } = held.effects[0]
    else {
        panic!()
    };
    assert_eq!(deadline_ms, 32_000);
    // Normalization of a fresh provisional attempt is not abandonment of the
    // older proven scope. Ordinary exact-scope retirement stays unchanged.
    for code in ["scope_normalized", "canonical_scope", "closed"] {
        facts.connection.scope.attempt += 1;
        facts.connection.scope.daemon_started_at_ms = None;
        assert_eq!(
            actions
                .retire(&facts.scope(), why(code), now(2_002))
                .unwrap(),
            ActionUpdate::default()
        );
        assert_eq!(actions.pending_count(), 1);
    }
    assert_eq!(
        actions.wake(timer_id, now(deadline_ms - 1)),
        ActionUpdate::default()
    );
    facts.connection.scope.daemon_started_at_ms = proven.daemon_started_at_ms;
    let replay = actions
        .resynchronized(&facts.context(), now(deadline_ms - 1))
        .unwrap();
    let replay_request = replay
        .effects
        .iter()
        .find_map(|effect| match effect {
            ClientEffect::Request {
                operation_id,
                command,
                ..
            } => Some((operation_id, command)),
            _ => None,
        })
        .unwrap();
    assert_eq!(*replay_request.0, id);
    assert_eq!(replay_request.1, request(&first).3);
    assert!(replay.completions.is_empty());
    assert_eq!(
        actions.wake(timer_id, now(deadline_ms)),
        ActionUpdate::default()
    );
}

#[test]
fn trusted_retry_invalidation_never_cancels_a_different_endpoint_or_workspace() {
    let facts = Facts::new();
    for different_endpoint in [true, false] {
        let mut actions = planner();
        let id = request(&plan(&mut actions, &facts, message(&facts))).0;
        actions
            .finish(
                &facts.scope(),
                id,
                unknown("closed"),
                &facts.context(),
                now(2_000),
            )
            .unwrap();
        let mut unrelated = facts.scope();
        unrelated.attempt += 1;
        if different_endpoint {
            unrelated.endpoint_id = "other-endpoint".into();
        } else {
            unrelated.workspace_id = "other-workspace".into();
        }
        assert_eq!(
            actions.invalidate_retries(&unrelated, why("revoked")),
            ActionUpdate::default()
        );
        assert_eq!(actions.pending_count(), 1);
    }
}

#[test]
fn logical_retry_invalidation_leaves_exact_scope_work_to_existing_retirement_guards() {
    let mut facts = Facts::new();
    let proven = facts.scope();
    let mut actions = planner();
    let send = request(&plan(&mut actions, &facts, message(&facts))).0;
    actions
        .finish(
            &proven,
            send,
            unknown("closed"),
            &facts.context(),
            now(2_000),
        )
        .unwrap();
    facts.connection.scope.attempt += 1;
    let pause = request(&plan(
        &mut actions,
        &facts,
        agent_action(&facts, AgentAction::Pause),
    ))
    .0;
    let (confirmation, _) = confirmation(&plan(
        &mut actions,
        &facts,
        agent_action(&facts, AgentAction::Kill),
    ));
    let attach = request(&plan(
        &mut actions,
        &facts,
        ActionIntent::Attach {
            scope: facts.scope(),
            agent: "claude-2".into(),
            rows: 24,
            cols: 80,
        },
    ))
    .0;
    actions
        .finish(
            &facts.scope(),
            attach,
            OperationOutcome::Succeeded {
                result: CommandResult::Attached {
                    agent: "claude-2".into(),
                },
            },
            &facts.context(),
            now(2_001),
        )
        .unwrap();
    assert_eq!(actions.pending_count(), 3); // held Send, Pause and confirmation
    let invalidated = actions.invalidate_retries(&facts.scope(), why("revoked"));
    assert_eq!(invalidated.completions.len(), 1);
    assert_eq!(invalidated.completions[0].operation_id, send);
    assert_eq!(invalidated.completions[0].scope, proven);
    assert_eq!(actions.pending_count(), 2);
    assert!(actions.attachment_is_live(&facts.scope(), attach));
    assert!(!invalidated.effects.iter().any(|effect| matches!(
        effect,
        ClientEffect::CloseAttachment { .. } | ClientEffect::Request { .. }
    )));
    // A stale asynchronous outcome still cannot finish new-attempt work.
    assert_eq!(
        actions
            .finish(
                &proven,
                pause,
                unknown("closed"),
                &facts.context(),
                now(2_002)
            )
            .unwrap(),
        ActionUpdate::default()
    );
    assert!(actions.cancel_confirmation(confirmation).changes.pending);
    let retired = actions
        .retire(&facts.scope(), why("revoked"), now(2_003))
        .unwrap();
    assert_eq!(retired.completions.len(), 1);
    assert_eq!(retired.completions[0].operation_id, pause);
    assert!(retired.effects.iter().any(|effect| matches!(effect, ClientEffect::CloseAttachment { attachment_id, .. } if *attachment_id == attach)));
    assert_eq!(actions.pending_count(), 0);
}

fn input(facts: &Facts, text: &str, submit: bool) -> ActionIntent {
    ActionIntent::AgentInput {
        scope: facts.scope(),
        id: "claude-1".into(),
        text: Some(text.into()),
        submit,
    }
}

#[test]
fn successive_outstanding_raw_batches_dispatch_with_distinct_ids_and_captured_bytes() {
    let facts = Facts::new();
    let mut actions = planner();
    let first = plan(&mut actions, &facts, input(&facts, "\u{1b}[A", false));
    let second = plan(&mut actions, &facts, input(&facts, "账目\r", false));
    let (first_id, first_scope, first_lane, first_command) = request(&first);
    let (second_id, second_scope, second_lane, second_command) = request(&second);
    assert_ne!(first_id, second_id);
    assert_eq!(
        (first_scope, second_scope),
        (&facts.scope(), &facts.scope())
    );
    assert_eq!(
        (first_lane, second_lane),
        (RequestLane::Action, RequestLane::Action)
    );
    assert_eq!(
        first_command,
        &Command::AgentInput {
            id: "claude-1".into(),
            text: Some("\u{1b}[A".into()),
            submit: false
        }
    );
    assert_eq!(
        second_command,
        &Command::AgentInput {
            id: "claude-1".into(),
            text: Some("账目\r".into()),
            submit: false
        }
    );
    assert_eq!(actions.pending_count(), 2);
    for id in [first_id, second_id] {
        let finished = actions
            .finish(
                &facts.scope(),
                id,
                OperationOutcome::Succeeded {
                    result: CommandResult::Ok,
                },
                &facts.context(),
                now(1_001),
            )
            .unwrap();
        assert_eq!(finished.completions.len(), 1);
        assert_eq!(finished.completions[0].operation_id, id);
        assert_eq!(
            finished.completions[0].notice().kind,
            OperationOutcomeKind::Success
        );
    }
    assert_eq!(actions.pending_count(), 0);
}

#[test]
fn raw_batches_and_submitted_prompts_never_block_each_other_in_either_order() {
    let facts = Facts::new();
    for raw_first in [true, false] {
        let mut actions = planner();
        let first = plan(&mut actions, &facts, input(&facts, "first", !raw_first));
        if raw_first {
            let available = actions.availability(
                ActionKind::AgentInput,
                Some("claude-1"),
                &facts.context(),
                now(1_001),
            );
            assert!(available.available && available.reason.is_none());
        }
        let second = plan(&mut actions, &facts, input(&facts, "second", raw_first));
        assert_ne!(request(&first).0, request(&second).0);
        assert_eq!(actions.pending_count(), 2);
        let unavailable = actions.availability(
            ActionKind::AgentInput,
            Some("claude-1"),
            &facts.context(),
            now(1_001),
        );
        assert_eq!(unavailable.reason.unwrap().code, "already_pending");
        // The availability row describes the prompt button; a full raw intent
        // still dispatches while that button reports a pending prompt.
        request(&plan(&mut actions, &facts, input(&facts, "raw", false)));
    }
}

#[test]
fn a_second_submitted_prompt_keeps_its_existing_duplicate_guard() {
    let facts = Facts::new();
    let mut actions = planner();
    let first = plan(&mut actions, &facts, input(&facts, "first prompt", true));
    assert_eq!(
        unavailable(&plan(
            &mut actions,
            &facts,
            input(&facts, "second prompt", true)
        )),
        "already_pending"
    );
    assert_eq!(actions.pending_count(), 1);
    actions
        .finish(
            &facts.scope(),
            request(&first).0,
            OperationOutcome::Succeeded {
                result: CommandResult::Ok,
            },
            &facts.context(),
            now(1_001),
        )
        .unwrap();
    request(&plan(
        &mut actions,
        &facts,
        input(&facts, "second prompt", true),
    ));
}

#[test]
fn raw_batches_keep_capacity_utf8_byte_bounds_and_terminal_grant_preflight() {
    let mut facts = Facts::new();
    let mut actions = Actions::new(2).unwrap();
    request(&plan(&mut actions, &facts, input(&facts, "\r\n", false)));
    request(&plan(&mut actions, &facts, input(&facts, " ", false)));
    assert_eq!(
        unavailable(&plan(&mut actions, &facts, input(&facts, "third", false))),
        "operation_capacity"
    );
    let exact = "é".repeat(MAX_MESSAGE_BYTES / 2);
    request(&plan(&mut planner(), &facts, input(&facts, &exact, false)));
    let too_large = format!("{exact}é");
    assert!(
        matches!(planner().plan(input(&facts, &too_large, false), &facts.context(), now(1_001)), Err(ClientError::TooLarge { actual, limit }) if actual == (MAX_MESSAGE_BYTES + 2) as u64 && limit == MAX_MESSAGE_BYTES as u64)
    );
    facts.gateway(vec![GrantTier::Read, GrantTier::Control]);
    assert_eq!(
        unavailable(&plan(&mut planner(), &facts, input(&facts, "x", false))),
        "grant_required"
    );
    facts.connection.grant.tiers.push(GrantTier::Terminal);
    request(&plan(&mut planner(), &facts, input(&facts, "x", false)));
}

#[test]
fn raw_closed_and_retired_outcomes_are_unknown_once_without_replay_or_stale_attribution() {
    let mut facts = Facts::new();
    let proven = facts.scope();
    let mut actions = planner();
    let first = request(&plan(&mut actions, &facts, input(&facts, "a", false))).0;
    let second = request(&plan(&mut actions, &facts, input(&facts, "b", false))).0;
    let closed = actions
        .finish(
            &proven,
            first,
            unknown("closed"),
            &facts.context(),
            now(1_001),
        )
        .unwrap();
    assert_eq!(closed.completions.len(), 1);
    assert_eq!(
        closed.completions[0].notice().kind,
        OperationOutcomeKind::Unknown
    );
    assert!(closed.effects.is_empty());
    let mut stale = proven.clone();
    stale.attempt += 1;
    assert_eq!(
        actions
            .finish(
                &stale,
                second,
                OperationOutcome::Succeeded {
                    result: CommandResult::Ok
                },
                &facts.context(),
                now(1_002)
            )
            .unwrap(),
        ActionUpdate::default()
    );
    let retired = actions.retire(&proven, why("revoked"), now(1_003)).unwrap();
    assert_eq!(retired.completions.len(), 1);
    assert_eq!(retired.completions[0].operation_id, second);
    assert_eq!(
        retired.completions[0].notice().kind,
        OperationOutcomeKind::Unknown
    );
    assert!(retired.effects.is_empty());
    assert_eq!(
        actions
            .finish(
                &proven,
                second,
                OperationOutcome::Succeeded {
                    result: CommandResult::Ok
                },
                &facts.context(),
                now(1_004)
            )
            .unwrap(),
        ActionUpdate::default()
    );
    facts.connection.scope.attempt += 1;
    assert_eq!(
        actions
            .resynchronized(&facts.context(), now(1_005))
            .unwrap(),
        ActionUpdate::default()
    );
}

#[test]
fn attach_success_notice_identifies_the_live_attachment_before_any_terminal_chunk_exists() {
    let facts = Facts::new();
    let mut actions = planner();
    let decision = plan(
        &mut actions,
        &facts,
        ActionIntent::Attach {
            scope: facts.scope(),
            agent: "claude-1".into(),
            rows: 24,
            cols: 80,
        },
    );
    let (id, scope, lane, _) = request(&decision);
    assert_eq!(lane, RequestLane::Terminal { attachment_id: id });
    assert!(decision.update.completions.is_empty());
    assert!(!actions.attachment_is_live(scope, id));
    let finished = actions
        .finish(
            scope,
            id,
            OperationOutcome::Succeeded {
                result: CommandResult::Attached {
                    agent: "claude-1".into(),
                },
            },
            &facts.context(),
            now(1_001),
        )
        .unwrap();
    assert_eq!(finished.completions.len(), 1);
    let notice = finished.completions[0].notice();
    assert_eq!(notice.scope, facts.scope());
    assert_eq!(notice.operation_id, id);
    assert_eq!(notice.kind, OperationOutcomeKind::Success);
    assert_eq!(notice.attachment_id, Some(id));
    assert!(notice.reason.is_none());
    assert!(actions.attachment_is_live(&notice.scope, notice.attachment_id.unwrap()));
    assert_eq!(
        actions
            .finish(
                scope,
                id,
                OperationOutcome::Succeeded {
                    result: CommandResult::Attached {
                        agent: "claude-1".into()
                    },
                },
                &facts.context(),
                now(1_002)
            )
            .unwrap(),
        ActionUpdate::default()
    );
}

#[test]
fn send_and_answer_success_notices_keep_no_attachment_identity() {
    let facts = Facts::new();
    let fixtures: Value =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/results.json")).unwrap();
    for (intent, method) in [
        (message(&facts), "send"),
        (
            ActionIntent::Answer {
                scope: facts.scope(),
                question_id: facts.questions[0].id.clone(),
                body: "Use the adapter".into(),
            },
            "answer",
        ),
    ] {
        let mut actions = planner();
        let id = request(&plan(&mut actions, &facts, intent)).0;
        let result: SendResult = serde_json::from_value(fixtures[method][0].clone()).unwrap();
        let finished = actions
            .finish(
                &facts.scope(),
                id,
                OperationOutcome::Succeeded {
                    result: CommandResult::Sent(Box::new(result)),
                },
                &facts.context(),
                now(1_001),
            )
            .unwrap();
        assert_eq!(finished.completions.len(), 1);
        let notice = finished.completions[0].notice();
        assert_eq!(notice.operation_id, id);
        assert_eq!(notice.kind, OperationOutcomeKind::Success);
        assert_eq!(notice.attachment_id, None);
    }
}

#[test]
fn stale_failed_or_ended_attach_cannot_publish_an_identified_success_notice() {
    let facts = Facts::new();
    let attach = || ActionIntent::Attach {
        scope: facts.scope(),
        agent: "claude-1".into(),
        rows: 24,
        cols: 80,
    };
    for outcome in [
        OperationOutcome::Rejected {
            details: None,
            code: "unavailable".into(),
            message: "terminal unavailable".into(),
        },
        OperationOutcome::NotSent {
            reason: why("not_sent"),
        },
        OperationOutcome::Canceled,
        unknown("closed"),
    ] {
        let mut actions = planner();
        let id = request(&plan(&mut actions, &facts, attach())).0;
        let finished = actions
            .finish(&facts.scope(), id, outcome, &facts.context(), now(1_001))
            .unwrap();
        assert_eq!(finished.completions.len(), 1);
        assert_ne!(
            finished.completions[0].notice().kind,
            OperationOutcomeKind::Success
        );
        assert_eq!(finished.completions[0].notice().attachment_id, None);
        assert!(!actions.attachment_is_live(&facts.scope(), id));
    }
    let mut actions = planner();
    let id = request(&plan(&mut actions, &facts, attach())).0;
    let mut stale = facts.scope();
    stale.attempt += 1;
    let ack = || OperationOutcome::Succeeded {
        result: CommandResult::Attached {
            agent: "claude-1".into(),
        },
    };
    assert_eq!(
        actions
            .finish(&stale, id, ack(), &facts.context(), now(1_001))
            .unwrap(),
        ActionUpdate::default()
    );
    assert!(!actions.attachment_is_live(&facts.scope(), id));
    actions.attachment_ended(&facts.scope(), id);
    let late = actions
        .finish(&facts.scope(), id, ack(), &facts.context(), now(1_002))
        .unwrap();
    assert_eq!(late.completions.len(), 1);
    assert_eq!(
        late.completions[0].notice().kind,
        OperationOutcomeKind::Unknown
    );
    assert_eq!(late.completions[0].notice().attachment_id, None);
    assert!(!actions.attachment_is_live(&facts.scope(), id));
}
