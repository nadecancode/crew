//! Exact intent eligibility tests against the real planner. The legacy corpus
//! remains unchanged: this adapter checks the preflight law, not baseline web
//! dialog behavior or daemon results that the portable client cannot observe.

use super::*;
use crew_protocol::rpc::Snapshot;
use crew_protocol::AgentStatus;
use serde_json::{json, Value};

#[derive(Clone)]
struct Facts {
    connection: Option<ConnectionContext>,
    endpoint: EndpointStatus,
    workspace: WorkspaceStatus,
    agents: Vec<Agent>,
    tasks: Vec<Task>,
    questions: Vec<Message>,
    session: SessionInfo,
}

impl Facts {
    fn new() -> Self {
        let results: Value = serde_json::from_str(include_str!(
            "../../../crew/tests/fixtures/web/results.json"
        ))
        .unwrap();
        let snapshot: Snapshot = serde_json::from_value(results["snapshot"][0].clone()).unwrap();
        let contexts: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/native/actions/contexts.json"
        ))
        .unwrap();
        let terminal: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/native/actions/native/terminal-routing.json"
        ))
        .unwrap();
        let scope: ClientScope =
            serde_json::from_value(terminal["cases"][0]["effects"][0]["data"]["scope"].clone())
                .unwrap();
        let mut agents = snapshot.agents;
        let agent = agents
            .iter_mut()
            .find(|agent| agent.id == "claude-1")
            .unwrap();
        agent.status = AgentStatus::Working;
        agent.runtime = RuntimeKind::Claude;
        agent.backend = "herdr".into();
        agent.limit = Some(UsageLimit::default());
        Self {
            connection: Some(ConnectionContext {
                grant: Grant {
                    grant_id: "operator".into(),
                    tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
                    workspace_ids: vec![scope.workspace_id.clone()],
                    expires_at_ms: None,
                },
                scope,
                mode: ConnectionMode::LocalOperator,
                snapshot_mode: SnapshotMode::OrderedConnection,
            }),
            endpoint: EndpointStatus::Authenticated,
            workspace: WorkspaceStatus::Live,
            agents,
            tasks: snapshot.tasks,
            questions: vec![
                serde_json::from_value(contexts["question"]["question"].clone()).unwrap(),
            ],
            session: snapshot.session,
        }
    }

    fn context(&self) -> ActionContext<'_> {
        ActionContext {
            connection: self.connection.as_ref(),
            endpoint: self.endpoint,
            workspace: self.workspace,
            agents: &self.agents,
            tasks: &self.tasks,
            questions: &self.questions,
            session: Some(&self.session),
        }
    }

    fn scope(&self) -> ClientScope {
        self.connection.as_ref().unwrap().scope.clone()
    }

    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&(
            &self.connection,
            self.endpoint,
            self.workspace,
            &self.agents,
            &self.tasks,
            &self.questions,
            &self.session,
        ))
        .unwrap()
    }
}

fn clock() -> ClockSample {
    ClockSample {
        wall_ms: 1_760_000_000_000,
        monotonic_ms: 1_000,
    }
}

// Destructure every field, so a new piece of operation state requires extending
// both the full-state snapshot and independent execution copy in this test.
fn bytes(actions: &Actions) -> Vec<u8> {
    let Actions {
        maximum,
        next_operation,
        next_confirmation,
        next_timer,
        pending,
        confirmations,
        retries,
        attachments,
    } = actions;
    format!("{maximum:?}|{next_operation:?}|{next_confirmation:?}|{next_timer:?}|{pending:?}|{confirmations:?}|{retries:?}|{attachments:?}").into_bytes()
}

fn execution_copy(actions: &Actions) -> Actions {
    let Actions {
        maximum,
        next_operation,
        next_confirmation,
        next_timer,
        pending,
        confirmations,
        retries,
        attachments,
    } = actions;
    Actions {
        maximum: *maximum,
        next_operation: *next_operation,
        next_confirmation: *next_confirmation,
        next_timer: *next_timer,
        pending: pending.clone(),
        confirmations: confirmations.clone(),
        retries: retries.clone(),
        attachments: attachments.clone(),
    }
}

fn parity(actions: &Actions, facts: &Facts, intent: &ActionIntent, label: &str) {
    let before = bytes(actions);
    let before_facts = facts.bytes();
    let before_intent = serde_json::to_vec(intent).unwrap();
    let availability = actions.intent_availability(intent, &facts.context(), clock());
    for _ in 0..3 {
        assert_eq!(
            actions.intent_availability(intent, &facts.context(), clock()),
            availability,
            "{label}: repeated query"
        );
        assert_eq!(bytes(actions), before, "{label}: operation state changed");
        assert_eq!(facts.bytes(), before_facts, "{label}: domain facts changed");
        assert_eq!(serde_json::to_vec(intent).unwrap(), before_intent);
    }
    let mut execution = execution_copy(actions);
    let planned = execution.plan(intent.clone(), &facts.context(), clock());
    match (availability, planned) {
        (Err(query), Err(plan)) => assert_eq!(query, plan, "{label}: error parity"),
        (Ok(query), Ok(plan)) => {
            let encoded_intent = serde_json::to_value(intent).unwrap();
            let expected_kind = if encoded_intent["kind"] == "agent" {
                &encoded_intent["data"]["action"]
            } else {
                &encoded_intent["kind"]
            };
            assert_eq!(
                serde_json::to_value(query.action).unwrap(),
                *expected_kind,
                "{label}: action identity"
            );
            match plan.plan {
                ActionPlan::Unavailable { reason } => {
                    assert!(!query.available, "{label}");
                    assert_eq!(query.reason, Some(reason), "{label}: reason parity");
                    assert_eq!(plan.update, ActionUpdate::default());
                    assert_eq!(bytes(&execution), before, "{label}: refused plan mutated");
                }
                ActionPlan::Dispatch { .. } | ActionPlan::Confirm { .. } => {
                    assert!(query.available, "{label}");
                    assert_eq!(query.reason, None, "{label}");
                }
            }
        }
        (query, plan) => panic!("{label}: query {query:?}, plan {plan:?}"),
    }
}

fn send(facts: &Facts, message_id: &str) -> ActionIntent {
    ActionIntent::SendMessage {
        scope: facts.scope(),
        to: "claude-1".into(),
        body: "Keep the existing reports.\n账目".into(),
        message_id: message_id.into(),
    }
}

fn prompt(facts: &Facts, submit: bool) -> ActionIntent {
    ActionIntent::AgentInput {
        scope: facts.scope(),
        id: "claude-1".into(),
        text: Some(
            if submit {
                "check the report"
            } else {
                "\u{1b}[A"
            }
            .into(),
        ),
        submit,
    }
}

fn attach(facts: &Facts) -> ActionIntent {
    ActionIntent::Attach {
        scope: facts.scope(),
        agent: "claude-1".into(),
        rows: 24,
        cols: 80,
    }
}

fn dispatch(actions: &mut Actions, facts: &Facts, intent: ActionIntent) -> OperationId {
    let decision = actions.plan(intent, &facts.context(), clock()).unwrap();
    let ActionPlan::Dispatch { operation_id } = decision.plan else {
        panic!("setup must dispatch: {:?}", decision.plan);
    };
    operation_id
}

fn prepared(facts: &Facts, state: &str) -> (Actions, AttachmentId) {
    let mut actions = Actions::new(64).unwrap();
    let id = dispatch(&mut actions, facts, attach(facts));
    if state != "attachment_pending" {
        actions
            .finish(
                &facts.scope(),
                id,
                OperationOutcome::Succeeded {
                    result: CommandResult::Attached {
                        agent: "claude-1".into(),
                    },
                },
                &facts.context(),
                clock(),
            )
            .unwrap();
    }
    match state {
        "empty" | "attachment_pending" => {}
        "full" => {
            actions.maximum = 2;
            dispatch(&mut actions, facts, send(facts, "capacity-1"));
            dispatch(&mut actions, facts, send(facts, "capacity-2"));
        }
        "raw" => {
            dispatch(&mut actions, facts, prompt(facts, false));
        }
        "prompt" => {
            dispatch(&mut actions, facts, prompt(facts, true));
        }
        "confirmation" => {
            let decision = actions
                .plan(
                    ActionIntent::Agent {
                        scope: facts.scope(),
                        id: "claude-1".into(),
                        action: AgentAction::Kill,
                    },
                    &facts.context(),
                    clock(),
                )
                .unwrap();
            assert!(matches!(decision.plan, ActionPlan::Confirm { .. }));
        }
        "retry" => {
            let operation_id = dispatch(&mut actions, facts, send(facts, "identified-send"));
            let update = actions
                .finish(
                    &facts.scope(),
                    operation_id,
                    OperationOutcome::OutcomeUnknown {
                        reason: reason("closed", "connection closed"),
                    },
                    &facts.context(),
                    clock(),
                )
                .unwrap();
            assert!(update.completions.is_empty());
            assert_eq!(actions.retries.len(), 1);
        }
        "closing" => {
            dispatch(
                &mut actions,
                facts,
                ActionIntent::Detach {
                    scope: facts.scope(),
                    attachment_id: id,
                },
            );
        }
        _ => panic!("unknown setup {state}"),
    }
    (actions, id)
}

fn intents(facts: &Facts, attachment_id: AttachmentId) -> Vec<ActionIntent> {
    let scope = facts.scope();
    let mut intents = vec![
        send(facts, "identified-send"),
        ActionIntent::SendMessage {
            scope: scope.clone(),
            to: BROADCAST.into(),
            body: "Report current work".into(),
            message_id: "broadcast".into(),
        },
        ActionIntent::Answer {
            scope: scope.clone(),
            question_id: "m-9001".into(),
            body: "Convert in the adapter".into(),
        },
        ActionIntent::SetModel {
            scope: scope.clone(),
            id: "claude-1".into(),
            runtime: Some(RuntimeKind::Claude),
            model: "opus".into(),
            note: None,
        },
        prompt(facts, false),
        prompt(facts, true),
        ActionIntent::ResumeLimited {
            scope: scope.clone(),
            restart: false,
        },
        ActionIntent::ResumeLimited {
            scope: scope.clone(),
            restart: true,
        },
        ActionIntent::StopTeam {
            scope: scope.clone(),
        },
        attach(facts),
        ActionIntent::AttachInput {
            scope: scope.clone(),
            attachment_id,
            data: vec![0x1b, 0, 0xff, b'\r'],
        },
        ActionIntent::AttachResize {
            scope: scope.clone(),
            attachment_id,
            rows: 0,
            cols: u16::MAX,
        },
        ActionIntent::Detach {
            scope: scope.clone(),
            attachment_id,
        },
    ];
    for action in [
        AgentAction::Pause,
        AgentAction::Resume,
        AgentAction::Compact,
        AgentAction::Restart,
        AgentAction::RestartFresh,
        AgentAction::Kill,
        AgentAction::Focus,
    ] {
        intents.push(ActionIntent::Agent {
            scope: scope.clone(),
            id: "claude-1".into(),
            action,
        });
    }
    for (kind, entity) in [
        (ResourceKind::TaskCard, Entity::Task("billing".into())),
        (ResourceKind::Message, Entity::Message("m-0104".into())),
        (ResourceKind::Event, Entity::Event(1)),
        (ResourceKind::Models, Entity::Workspace),
        (ResourceKind::Transcript, Entity::Agent("claude-1".into())),
        (ResourceKind::Diff, Entity::Agent("claude-1".into())),
        (ResourceKind::Output, Entity::Agent("claude-1".into())),
        (ResourceKind::Inbox, Entity::Agent("claude-1".into())),
        (ResourceKind::History, Entity::Workspace),
        (ResourceKind::Events, Entity::Workspace),
    ] {
        intents.push(ActionIntent::Read {
            scope: scope.clone(),
            resource: ResourceRequest {
                key: ResourceKey {
                    entity: EntityKey {
                        scope: scope.clone(),
                        entity,
                    },
                    kind,
                    options: ReadOptions::Default,
                },
            },
        });
    }
    intents
}

#[test]
fn complete_intents_match_plan_across_grants_capacity_duplicates_and_attachment_lifetimes() {
    let operator = Facts::new();
    let mut contexts = vec![operator.clone()];
    for tiers in [
        vec![],
        vec![GrantTier::Read],
        vec![GrantTier::Control],
        vec![GrantTier::Terminal],
        vec![GrantTier::Read, GrantTier::Control],
        vec![GrantTier::Read, GrantTier::Terminal],
        vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
    ] {
        let mut facts = operator.clone();
        let connection = facts.connection.as_mut().unwrap();
        connection.mode = ConnectionMode::ScopedGateway;
        connection.grant.tiers = tiers;
        contexts.push(facts);
    }
    let states = [
        "empty",
        "full",
        "raw",
        "prompt",
        "confirmation",
        "retry",
        "attachment_pending",
        "closing",
    ];
    let mut comparisons = 0;
    for state in states {
        let (actions, attachment) = prepared(&operator, state);
        for facts in &contexts {
            for intent in intents(facts, attachment) {
                parity(&actions, facts, &intent, &format!("{state}: {intent:?}"));
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 30 * 8 * 8);
}

#[test]
fn malformed_intents_and_stale_or_unavailable_facts_keep_exact_reason_and_error_parity() {
    let facts = Facts::new();
    let (actions, attachment) = prepared(&facts, "empty");
    for mutation in [
        "closed",
        "snapshot_pending",
        "expired",
        "ungranted_workspace",
        "missing_agent",
        "missing_question",
        "missing_task",
        "stopped_agent",
        "shell",
        "backend",
        "no_limits",
    ] {
        let mut changed = facts.clone();
        match mutation {
            "closed" => changed.connection = None,
            "snapshot_pending" => changed.workspace = WorkspaceStatus::Connecting,
            "expired" => {
                changed.connection.as_mut().unwrap().grant.expires_at_ms = Some(clock().wall_ms)
            }
            "ungranted_workspace" => {
                let connection = changed.connection.as_mut().unwrap();
                connection.mode = ConnectionMode::ScopedGateway;
                connection.grant.workspace_ids.clear();
            }
            "missing_agent" => changed.agents.retain(|agent| agent.id != "claude-1"),
            "missing_question" => changed.questions.clear(),
            "missing_task" => changed.tasks.clear(),
            "stopped_agent" => {
                changed
                    .agents
                    .iter_mut()
                    .find(|agent| agent.id == "claude-1")
                    .unwrap()
                    .status = AgentStatus::Stopped
            }
            "shell" => {
                changed
                    .agents
                    .iter_mut()
                    .find(|agent| agent.id == "claude-1")
                    .unwrap()
                    .runtime = RuntimeKind::Shell
            }
            "backend" => {
                changed
                    .agents
                    .iter_mut()
                    .find(|agent| agent.id == "claude-1")
                    .unwrap()
                    .backend = "pty".into()
            }
            "no_limits" => changed
                .agents
                .iter_mut()
                .for_each(|agent| agent.limit = None),
            _ => unreachable!(),
        }
        for intent in intents(&facts, attachment) {
            parity(&actions, &changed, &intent, mutation);
        }
    }
    let mut malformed = vec![send(&facts, "invalid message id")];
    for text in [" \n ".into(), "/unknown".into(), "é".repeat(32_769)] {
        let mut message = send(&facts, "message");
        if let ActionIntent::SendMessage { body, .. } = &mut message {
            *body = text;
        }
        malformed.push(message);
    }
    malformed.push(ActionIntent::AgentInput {
        scope: facts.scope(),
        id: "claude-1".into(),
        text: Some("é".repeat(32_769)),
        submit: false,
    });
    malformed.push(ActionIntent::Answer {
        scope: facts.scope(),
        question_id: "m-9001".into(),
        body: "/stop".into(),
    });
    malformed.push(ActionIntent::AttachInput {
        scope: facts.scope(),
        attachment_id: attachment,
        data: vec![0; policy::VALIDATION_LIMITS.frame_bytes / 4 * 3 + 1],
    });
    let mut invalid_agent = prompt(&facts, true);
    if let ActionIntent::AgentInput { id, .. } = &mut invalid_agent {
        *id = "Bad ID".into();
    }
    malformed.push(invalid_agent);
    for mut intent in intents(&facts, attachment) {
        replace_scope(
            &mut intent,
            ClientScope {
                attempt: facts.scope().attempt + 1,
                ..facts.scope()
            },
        );
        malformed.push(intent);
    }
    for intent in malformed {
        parity(&actions, &facts, &intent, "malformed or stale intent");
    }
}

#[test]
fn raw_prompt_and_specific_question_duplicates_match_execution_without_reserving_an_id() {
    let facts = Facts::new();
    let (raw, _) = prepared(&facts, "raw");
    assert!(
        raw.intent_availability(&prompt(&facts, true), &facts.context(), clock())
            .unwrap()
            .available
    );
    let (mut submitted, _) = prepared(&facts, "prompt");
    let prompt_availability = submitted
        .intent_availability(&prompt(&facts, true), &facts.context(), clock())
        .unwrap();
    assert_eq!(prompt_availability.reason.unwrap().code, "already_pending");
    assert!(
        submitted
            .intent_availability(&prompt(&facts, false), &facts.context(), clock())
            .unwrap()
            .available
    );
    let answer = ActionIntent::Answer {
        scope: facts.scope(),
        question_id: "m-9001".into(),
        body: "Adapter".into(),
    };
    dispatch(&mut submitted, &facts, answer.clone());
    let availability = submitted
        .intent_availability(&answer, &facts.context(), clock())
        .unwrap();
    assert_eq!(availability.reason.unwrap().code, "already_pending");
    parity(&submitted, &facts, &answer, "exact question pending");

    let empty = Actions::new(64).unwrap();
    let intent = send(&facts, "not-reserved");
    for _ in 0..100 {
        assert!(
            empty
                .intent_availability(&intent, &facts.context(), clock())
                .unwrap()
                .available
        );
    }
    let mut actual = execution_copy(&empty);
    assert_eq!(dispatch(&mut actual, &facts, intent.clone()), 1);
    let mut changed = facts.clone();
    changed.workspace = WorkspaceStatus::Reconnecting;
    let before = bytes(&empty);
    parity(
        &empty,
        &changed,
        &intent,
        "availability is not a reservation",
    );
    assert_eq!(bytes(&empty), before);
}

#[test]
fn relevant_id_exhaustion_is_checked_without_allocating_and_preserves_confirmation_choice() {
    let facts = Facts::new();
    let mut actions = Actions::new(64).unwrap();
    actions.next_operation = CORE_OPERATION_ID_MAX;
    parity(
        &actions,
        &facts,
        &send(&facts, "last-id"),
        "last operation ID",
    );
    actions.next_operation = DRIVER_OPERATION_ID_START;
    parity(
        &actions,
        &facts,
        &send(&facts, "exhausted"),
        "operation exhausted",
    );
    let kill = ActionIntent::Agent {
        scope: facts.scope(),
        id: "claude-1".into(),
        action: AgentAction::Kill,
    };
    parity(&actions, &facts, &kill, "confirmation still allocatable");
    actions.next_confirmation = DRIVER_OPERATION_ID_START;
    parity(&actions, &facts, &kill, "confirmation exhausted");
}

const CORPUS: &[&str] = &[
    include_str!("../../../../fixtures/native/actions/answers.jsonl"),
    include_str!("../../../../fixtures/native/actions/commands.jsonl"),
    include_str!("../../../../fixtures/native/actions/controls.jsonl"),
    include_str!("../../../../fixtures/native/actions/messaging.jsonl"),
    include_str!("../../../../fixtures/native/actions/wire.jsonl"),
    include_str!("../../../../fixtures/native/actions/workspaces.jsonl"),
];

fn corpus_intent(case: &Value, facts: &Facts) -> Result<ActionIntent, &'static str> {
    let scope = facts.scope();
    let id = case["id"].as_str().unwrap();
    let input = &case["input"];
    // These cases have no complete native ActionIntent: raw protocol defaults
    // and rejects, agent credentials, navigation, modal cancellation/opening,
    // daemon storage/idempotency or explicit resume lists. Never fabricate an
    // intent to turn these baseline-only observations into native behavior.
    match case["layer"].as_str().unwrap() {
        "wire" => return Err("wire-only"),
        "slash" => return Err("slash"),
        "workspace" | "index" | "admission" => return Err("navigation/index"),
        "model_dialog" => return Err("dialog-only"),
        "confirmation" if input["decision"] == "cancel" => return Err("dialog-only"),
        "question_dialog" if input["entry"] == "later" => return Err("dialog-only"),
        _ => {}
    }
    if case["context_ref"] == "daemon_agent"
        || matches!(
            id,
            "pause-process-ended"
                | "resume-explicit-unknown-agent"
                | "resume-unlimited-no-restart"
                | "resume-terminal-agent-skipped"
                | "task-card-no-id"
                | "send-task-message"
                | "send-utf8-limit"
                | "send-same-id-changed-content"
                | "send-same-id-another-sender"
        )
    {
        return Err("wire-only");
    }
    if case["layer"] == "compose" {
        return Ok(ActionIntent::SendMessage {
            scope,
            to: input["to"].as_str().unwrap().into(),
            body: input["text"].as_str().unwrap().into(),
            message_id: "w-0102030405060708".into(),
        });
    }
    if case["layer"] == "question_dialog" {
        let body = if input["entry"] == "option" {
            let index = input["index"].as_u64().unwrap() as usize;
            facts.questions[0].metadata["options"][index]
                .as_str()
                .unwrap()
        } else {
            input["body"].as_str().unwrap()
        };
        return Ok(ActionIntent::Answer {
            scope,
            question_id: facts.questions[0].id.clone(),
            body: body.into(),
        });
    }
    if case["layer"] == "confirmation" {
        return Ok(match input["kind"].as_str().unwrap() {
            "kill" | "restart-fresh" => ActionIntent::Agent {
                scope,
                id: input["agent"].as_str().unwrap().into(),
                action: if input["kind"] == "kill" {
                    AgentAction::Kill
                } else {
                    AgentAction::RestartFresh
                },
            },
            "restart-paused" => ActionIntent::ResumeLimited {
                scope,
                restart: true,
            },
            "stop" => ActionIntent::StopTeam { scope },
            _ => panic!("new confirmation case needs an explicit adapter: {id}"),
        });
    }
    if case["layer"] == "command" {
        let effect = &case["expected"]["effect"];
        let Some(kind) = effect["kind"].as_str() else {
            return Err("slash");
        };
        let action = match kind {
            "pause" => AgentAction::Pause,
            "unpause" => AgentAction::Resume,
            "restart" => AgentAction::Restart,
            "confirm-kill" => AgentAction::Kill,
            "confirm-restart-fresh" => AgentAction::RestartFresh,
            "confirm-stop" => return Ok(ActionIntent::StopTeam { scope }),
            "resume" => {
                return Ok(ActionIntent::ResumeLimited {
                    scope,
                    restart: false,
                })
            }
            "answer" | "model" | "note" => return Err("dialog-only"),
            "view" => return Err("navigation/index"),
            _ => panic!("new command case needs an explicit adapter: {id}"),
        };
        return Ok(ActionIntent::Agent {
            scope,
            id: effect["agent"].as_str().unwrap().into(),
            action,
        });
    }
    let call = if case["layer"] == "daemon" {
        input.clone()
    } else if !case["expected"]["call"].is_null() {
        case["expected"]["call"].clone()
    } else if !case["expected"]["calls"].is_null() {
        case["expected"]["calls"][0].clone()
    } else {
        let args = &input["args"];
        match input["entry"].as_str().unwrap() {
            "actions.send" => json!({"method":"send","params":{"to":args[0],"body":args[1]}}),
            "actions.pause" => json!({"method":"agent_pause","params":{"id":args[0]}}),
            "operations.resumePaused" => json!({"method":"resume","params":{"restart":false}}),
            "operations.focusPane" => json!({"method":"agent_focus","params":{"id":args[0]}}),
            _ => panic!("new action case needs an explicit adapter: {id}"),
        }
    };
    let params = &call["params"];
    Ok(match call["method"].as_str().unwrap() {
        "send" => ActionIntent::SendMessage {
            scope,
            to: params["to"].as_str().unwrap().into(),
            body: params["body"].as_str().unwrap().into(),
            message_id: params["id"].as_str().unwrap_or("w-0102030405060708").into(),
        },
        "answer" => ActionIntent::Answer {
            scope,
            question_id: params["id"].as_str().unwrap().into(),
            body: params["body"].as_str().unwrap().into(),
        },
        "agent_pause"
        | "agent_resume"
        | "agent_restart"
        | "agent_restart_fresh"
        | "agent_kill"
        | "agent_compact"
        | "agent_focus" => ActionIntent::Agent {
            scope,
            id: params["id"].as_str().unwrap().into(),
            action: match call["method"].as_str().unwrap() {
                "agent_pause" => AgentAction::Pause,
                "agent_resume" => AgentAction::Resume,
                "agent_restart" => AgentAction::Restart,
                "agent_restart_fresh" => AgentAction::RestartFresh,
                "agent_kill" => AgentAction::Kill,
                "agent_compact" => AgentAction::Compact,
                "agent_focus" => AgentAction::Focus,
                _ => unreachable!(),
            },
        },
        "agent_input" => ActionIntent::AgentInput {
            scope,
            id: params["id"].as_str().unwrap().into(),
            text: params["text"].as_str().map(String::from),
            submit: params["submit"].as_bool().unwrap_or(false),
        },
        "agent_set_model" => ActionIntent::SetModel {
            scope,
            id: params["id"].as_str().unwrap().into(),
            model: params["model"].as_str().unwrap().into(),
            runtime: serde_json::from_value(params["runtime"].clone()).unwrap(),
            note: params["note"].as_str().map(String::from),
        },
        "resume" => ActionIntent::ResumeLimited {
            scope,
            restart: params["restart"].as_bool().unwrap_or(false),
        },
        "shutdown" => ActionIntent::StopTeam { scope },
        "task_get" => ActionIntent::Read {
            resource: ResourceRequest {
                key: ResourceKey {
                    entity: EntityKey {
                        scope: scope.clone(),
                        entity: Entity::Task(params["id"].as_str().unwrap().into()),
                    },
                    kind: ResourceKind::TaskCard,
                    options: ReadOptions::Default,
                },
            },
            scope,
        },
        _ => panic!("new method case needs an explicit adapter: {id}"),
    })
}

fn given_facts(facts: &mut Facts, context: &Value) {
    if context["hub_live"] == false {
        facts.endpoint = EndpointStatus::Disconnected;
    }
    if context["workspace_shown"]
        .as_str()
        .is_some_and(|state| state != "live")
    {
        facts.workspace = WorkspaceStatus::Reconnecting;
    }
    if let Some(ids) = context["agent_ids"].as_array() {
        facts
            .agents
            .retain(|agent| ids.iter().any(|id| id == &agent.id));
    }
    if context["target_on_team"] == false {
        facts.agents.retain(|agent| agent.id != "claude-1");
    }
    if context["asker_on_team"] == false {
        let from = facts.questions[0].from.clone();
        facts.agents.retain(|agent| agent.id != from);
    }
    if let Some(statuses) = context["agent_status"].as_object() {
        for agent in &mut facts.agents {
            if let Some(status) = statuses.get(&agent.id) {
                agent.status = serde_json::from_value(status.clone()).unwrap();
            }
        }
    }
    if let Some(backend) = context["backend"].as_str() {
        facts
            .agents
            .iter_mut()
            .find(|agent| agent.id == "claude-1")
            .unwrap()
            .backend = backend.into();
    }
    if context["limited_agents"] == json!([]) {
        facts.agents.iter_mut().for_each(|agent| agent.limit = None);
    }
    if let Some(paused) = context["paused"].as_bool() {
        facts
            .agents
            .iter_mut()
            .find(|agent| agent.id == "claude-1")
            .unwrap()
            .paused = paused;
    }
}

#[test]
fn every_legacy_case_is_either_preflight_conformant_or_explicitly_not_a_native_intent() {
    let contexts: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/native/actions/contexts.json"
    ))
    .unwrap();
    let mut applicable = Vec::new();
    let mut excluded = Vec::new();
    for line in CORPUS.iter().flat_map(|file| file.lines()) {
        let case: Value = serde_json::from_str(line).unwrap();
        let id = case["id"].as_str().unwrap();
        let mut facts = Facts::new();
        let intent = match corpus_intent(&case, &facts) {
            Ok(intent) => intent,
            Err(class) => {
                excluded.push((id.to_owned(), class));
                continue;
            }
        };
        let mut context = contexts[case["context_ref"].as_str().unwrap()].clone();
        if let Some(given) = case["given"].as_object() {
            context.as_object_mut().unwrap().extend(given.clone());
        }
        given_facts(&mut facts, &context);
        let mut actions = Actions::new(64).unwrap();
        if context["sending"] == true {
            let mut pending_answer = intent.clone();
            if let ActionIntent::Answer { body, .. } = &mut pending_answer {
                *body = "already submitted".into();
            }
            dispatch(&mut actions, &facts, pending_answer);
        }
        if context["question_open"] == false {
            facts.questions.clear();
        }
        parity(&actions, &facts, &intent, id);
        applicable.push(id.to_owned());
    }
    assert_eq!(applicable.len(), 60);
    assert_eq!(excluded.len(), 67);
    eprintln!(
        "availability corpus: {} applicable / {} excluded; applicable={} excluded={}",
        applicable.len(),
        excluded.len(),
        serde_json::to_string(&applicable).unwrap(),
        serde_json::to_string(&excluded).unwrap()
    );
}
