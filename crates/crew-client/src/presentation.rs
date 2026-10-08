//! Immutable semantic queries over the authoritative scoped store. No host
//! toolkit, locale, geometry, reducer, or parallel action-policy table.

use crew_protocol::{Agent, AgentStatus, TaskStatus};

use crate::{
    actions::{canonical_resource_key, Actions},
    api::*,
    engine::context,
    state::ClientState,
};
use crate::{current_work, satellite_parent_id, topology, usage};

const AGENT_ACTIONS: [ActionKind; 12] = [
    ActionKind::SendMessage,
    ActionKind::Pause,
    ActionKind::Resume,
    ActionKind::Compact,
    ActionKind::Restart,
    ActionKind::RestartFresh,
    ActionKind::Kill,
    ActionKind::Focus,
    ActionKind::SetModel,
    ActionKind::AgentInput,
    ActionKind::Read,
    ActionKind::Attach,
];

pub(crate) fn view(
    state: &ClientState,
    actions: &Actions,
    query: ViewQuery,
    clock: ClockSample,
) -> Result<ViewData, ClientError> {
    let payload = match query {
        ViewQuery::Features => ViewPayload::Features(state.features.clone()),
        ViewQuery::Epic { key } => feature_view(
            state,
            key,
            crew_protocol::features::Feature::Epics,
            ViewPayload::Epic,
        )?,
        ViewQuery::Asides { key } => feature_view(
            state,
            key,
            crew_protocol::features::Feature::Asides,
            ViewPayload::Asides,
        )?,
        ViewQuery::Docs { key } => feature_view(
            state,
            key,
            crew_protocol::features::Feature::Docs,
            ViewPayload::Docs,
        )?,
        ViewQuery::UsageMetrics { key } => feature_view(
            state,
            key,
            crew_protocol::features::Feature::UsageMetrics,
            ViewPayload::UsageMetrics,
        )?,
        ViewQuery::Availability { intent } => {
            return crate::engine::availability_view(state, actions, &intent, clock)
        }
        ViewQuery::Overview => ViewPayload::Overview(Box::new(overview(state)?)),
        ViewQuery::Team => ViewPayload::Team(Box::new(TeamData {
            agents: state
                .agents
                .iter()
                .map(|agent| agent_data(state, actions, agent, clock))
                .collect(),
            relations: topology::parent_relations(&state.agents),
        })),
        ViewQuery::Tasks => {
            let mut tasks = topology::task_data(&state.tasks)?;
            tasks.epics = state.epics.clone();
            ViewPayload::Tasks(Box::new(tasks))
        }
        ViewQuery::AgentDetail { id } => {
            let scope = selected_scope(state)?;
            let tasks = topology::task_data(&state.tasks)?;
            let mut resources: Vec<_> = state
                .resources
                .values()
                .filter(|resource| {
                    &resource.key.entity.scope == scope
                        && resource.key.entity.entity == Entity::Agent(id.clone())
                })
                .cloned()
                .collect();
            resources.sort_by_key(|resource| {
                (
                    resource_rank(resource.key.kind),
                    serde_json::to_string(&resource.key.options).expect("scalar read options"),
                )
            });
            ViewPayload::AgentDetail(Box::new(AgentDetailData {
                agent: state
                    .agents
                    .iter()
                    .find(|agent| agent.id == id)
                    .map(|agent| agent_data(state, actions, agent, clock)),
                assigned_tasks: tasks
                    .tasks
                    .iter()
                    .filter(|task| task.task.assigned_agent.as_deref() == Some(id.as_str()))
                    .cloned()
                    .collect(),
                review_tasks: tasks
                    .tasks
                    .into_iter()
                    .filter(|task| task.task.reviewer.as_deref() == Some(id.as_str()))
                    .collect(),
                recent_messages: state.messages.page(
                    scope,
                    state.revision,
                    None,
                    state.config.limits.page_size,
                    state.config.limits.page_size,
                    &MessageFilter {
                        endpoints: MessageEndpoints::Agent { id },
                        kind: None,
                        text: String::new(),
                    },
                )?,
                resources,
            }))
        }
        ViewQuery::TaskDetail { id } => {
            let scope = selected_scope(state)?;
            let key = ResourceKey {
                entity: EntityKey {
                    scope: scope.clone(),
                    entity: Entity::Task(id.clone()),
                },
                kind: ResourceKind::TaskCard,
                options: ReadOptions::Default,
            };
            ViewPayload::TaskDetail(Box::new(TaskDetailData {
                task: topology::task_data(&state.tasks)?
                    .tasks
                    .into_iter()
                    .find(|task| task.task.id == id),
                card: state.current_resource(&key),
            }))
        }
        ViewQuery::Messages {
            before,
            limit,
            filter,
        } => ViewPayload::Messages(Box::new(state.messages.page(
            selected_scope(state)?,
            state.revision,
            before.as_ref(),
            limit,
            state.config.limits.page_size,
            &filter,
        )?)),
        ViewQuery::Events {
            before,
            limit,
            filter,
        } => ViewPayload::Events(Box::new(state.events.page(
            selected_scope(state)?,
            state.revision,
            before.as_ref(),
            limit,
            state.config.limits.page_size,
            &filter,
        )?)),
        ViewQuery::Usage => ViewPayload::Usage(Box::new(usage::usage_data(
            &state.agents,
            state.session.as_ref(),
            &state.usage_samples,
            state.server_now(clock),
        ))),
        ViewQuery::Questions => {
            ViewPayload::Questions(state.questions.visible().cloned().collect())
        }
        ViewQuery::Evidence { key } => {
            if &key.entity.scope != selected_scope(state)? {
                return Err(ClientError::InvalidInput {
                    field: "resource.scope".into(),
                    reason: "resource belongs to another workspace or connection attempt".into(),
                });
            }
            let key = canonical_resource_key(&key)?;
            let resource = if let Entity::Event(seq) = key.entity.entity {
                let event = state
                    .events
                    .items()
                    .rev()
                    .find(|event| u64::try_from(event.seq).ok() == Some(seq))
                    .cloned();
                let status = match (&event, state.freshness()) {
                    (None, _) => ResourceStatus::Empty,
                    (Some(_), Freshness::Stale) => ResourceStatus::Stale,
                    (Some(_), _) => ResourceStatus::Ready,
                };
                ResourceState {
                    key,
                    status,
                    request_sequence: 0,
                    error: None,
                    value: Some(ResourceValue::Event(event)),
                }
            } else {
                state.current_resource(&key)
            };
            ViewPayload::Evidence(Box::new(resource))
        }
    };
    Ok(ViewData {
        scope: state.scope.clone(),
        revision: state.revision,
        synchronized_at_ms: state.synchronized_at_ms,
        freshness: state.freshness(),
        payload,
    })
}

fn selected_scope(state: &ClientState) -> Result<&ClientScope, ClientError> {
    state
        .scope
        .as_ref()
        .ok_or_else(|| ClientError::InvalidInput {
            field: "scope".into(),
            reason: "no workspace selected".into(),
        })
}

fn count(value: usize) -> Result<u32, ClientError> {
    u32::try_from(value).map_err(|_| ClientError::InternalFailure {
        code: "projection_count_overflow".into(),
    })
}

fn overview(state: &ClientState) -> Result<OverviewData, ClientError> {
    let mut warnings = Vec::new();
    if let Some(session) = &state.session {
        for (code, text) in [
            ("socket_warning", session.socket_warning.as_str()),
            ("binary_warning", session.binary_warning.as_str()),
            ("vcs_notice", session.vcs_note.as_str()),
        ] {
            if !text.is_empty() {
                warnings.push(WarningFact {
                    code: code.into(),
                    text: text.into(),
                    entity: None,
                });
            }
        }
        if session.sandbox == "off" {
            warnings.push(WarningFact {
                code: "sandbox_off".into(),
                text: "agents are not sandboxed".into(),
                entity: None,
            });
        }
    }
    if let Some(reason) = &state.reason {
        warnings.push(WarningFact {
            code: reason.code.clone(),
            text: reason.message.clone(),
            entity: None,
        });
    }
    Ok(OverviewData {
        session: state.session.clone(),
        agent_count: count(state.agents.len())?,
        working_count: count(
            state
                .agents
                .iter()
                .filter(|agent| agent.status == AgentStatus::Working)
                .count(),
        )?,
        task_count: count(state.tasks.len())?,
        done_count: count(
            state
                .tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Done)
                .count(),
        )?,
        question_count: count(state.questions.visible().count())?,
        warnings,
    })
}

fn agent_data(
    state: &ClientState,
    actions: &Actions,
    agent: &Agent,
    clock: ClockSample,
) -> AgentPresentation {
    let provider_failure = agent.status == AgentStatus::Blocked
        && matches!(
            agent.block_kind.as_deref(),
            Some("refused" | "provider_error")
        );
    let status_label = if provider_failure {
        if agent.block_kind.as_deref() == Some("refused") {
            "refused"
        } else {
            "error"
        }
    } else if agent.paused {
        "paused"
    } else {
        agent.status.as_str()
    };
    let tone = if provider_failure || agent.status == AgentStatus::Failed {
        Tone::Danger
    } else if agent.paused || agent.status == AgentStatus::Blocked {
        Tone::Warning
    } else {
        match agent.status {
            AgentStatus::Working | AgentStatus::Spawning => Tone::Info,
            AgentStatus::Done => Tone::Success,
            _ => Tone::Neutral,
        }
    };
    let entity = state.scope.as_ref().map(|scope| EntityKey {
        scope: scope.clone(),
        entity: Entity::Agent(agent.id.clone()),
    });
    let mut warnings = Vec::new();
    if let Some(error) = agent
        .error
        .as_deref()
        .filter(|error| !error.trim().is_empty())
    {
        warnings.push(WarningFact {
            code: "agent_error".into(),
            text: error.into(),
            entity: entity.clone(),
        });
    } else if provider_failure {
        warnings.push(WarningFact {
            code: agent.block_kind.clone().expect("provider kind"),
            text: if agent.block_kind.as_deref() == Some("refused") {
                "provider refused the turn"
            } else {
                "provider error"
            }
            .into(),
            entity: entity.clone(),
        });
    }
    if let Some(limit) = &agent.limit {
        warnings.push(WarningFact {
            code: "usage_limit".into(),
            text: limit.message.clone(),
            entity: entity.clone(),
        });
    }
    if let Some(fallback) = &agent.fallback {
        warnings.push(WarningFact {
            code: "model_fallback".into(),
            text: fallback.note.clone(),
            entity: entity.clone(),
        });
    }
    if agent.compact_pending {
        warnings.push(WarningFact {
            code: "compact_pending".into(),
            text: "context compaction queued for the turn boundary".into(),
            entity: entity.clone(),
        });
    }
    // The existing attention marker carries the toolbox's reason: a pending
    // change, or a required item that is not active (notes/toolboxes.md).
    if let Some(text) = agent
        .toolbox
        .as_ref()
        .and_then(|toolbox| toolbox.attention())
    {
        warnings.push(WarningFact {
            code: TOOLBOX_WARNING.into(),
            text,
            entity,
        });
    }
    let action_context = context(state);
    AgentPresentation {
        agent: agent.clone(),
        status_label: status_label.into(),
        tone,
        current_work: current_work(&state.tasks, &agent.id, state.server_now(clock)),
        waiting_task_ids: state
            .tasks
            .iter()
            .filter(|task| {
                task.assigned_agent.as_deref() == Some(agent.id.as_str())
                    && task.status == TaskStatus::Waiting
            })
            .map(|task| task.id.clone())
            .collect(),
        satellite_parent_id: satellite_parent_id(&state.agents, agent).map(str::to_owned),
        subagent_ids: state
            .agents
            .iter()
            .filter(|child| satellite_parent_id(&state.agents, child) == Some(agent.id.as_str()))
            .map(|child| child.id.clone())
            .collect(),
        warnings,
        actions: AGENT_ACTIONS
            .into_iter()
            .map(|kind| actions.availability(kind, Some(&agent.id), &action_context, clock))
            .collect(),
    }
}

fn resource_rank(kind: ResourceKind) -> u8 {
    match kind {
        ResourceKind::Feature(_) => 10,
        ResourceKind::TaskCard => 0,
        ResourceKind::Message => 1,
        ResourceKind::Event => 2,
        ResourceKind::Transcript => 3,
        ResourceKind::Diff => 4,
        ResourceKind::Output => 5,
        ResourceKind::Inbox => 6,
        ResourceKind::Models => 7,
        ResourceKind::History => 8,
        ResourceKind::Events => 9,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ClientCore;
    use crew_protocol::rpc::Snapshot;
    use crew_protocol::Task;

    fn clock() -> ClockSample {
        ClockSample {
            wall_ms: 1_000,
            monotonic_ms: 1_000,
        }
    }
    fn fixture() -> Snapshot {
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/snapshot.json")).unwrap()
    }
    fn scope() -> ClientScope {
        ClientScope {
            endpoint_id: "local:fixture".into(),
            workspace_id: "w".into(),
            generation: None,
            daemon_pid: None,
            daemon_started_at_ms: None,
            attempt: 1,
        }
    }
    fn new_core() -> ClientCore {
        let mut core = ClientCore::new(ClientConfig {
            limits: ClientLimits {
                messages: 10,
                events: 10,
                context_points: 3,
                buffered_pushes: 8,
                pending_operations: 8,
                page_size: 10,
            },
            snapshot_mode: SnapshotMode::OrderedConnection,
        })
        .unwrap();
        core.apply(
            ClientInput::Bind {
                scope: scope(),
                mode: ConnectionMode::LocalOperator,
                grant: Grant {
                    grant_id: "g".into(),
                    tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
                    workspace_ids: vec!["w".into()],
                    expires_at_ms: None,
                },
                snapshot_mode: SnapshotMode::OrderedConnection,
            },
            clock(),
        )
        .unwrap();
        core
    }
    fn load(core: &mut ClientCore, snapshot: Snapshot, operation_id: u64) {
        core.apply(
            ClientInput::SnapshotStarted {
                scope: scope(),
                operation_id,
            },
            clock(),
        )
        .unwrap();
        core.apply(
            ClientInput::Snapshot {
                scope: scope(),
                operation_id,
                snapshot: Box::new(snapshot),
            },
            clock(),
        )
        .unwrap();
    }
    fn project(core: &ClientCore, query: ViewQuery) -> Result<ViewData, ClientError> {
        view(&core.state, &core.actions, query, clock())
    }
    fn task(id: &str, status: TaskStatus, start: Option<i64>) -> Task {
        Task {
            epic: None,
            id: id.into(),
            title: id.into(),
            description: String::new(),
            assigned_agent: Some("claude-1".into()),
            status,
            dependencies: Vec::new(),
            created_at: 0,
            started_at: start,
            completed_at: None,
            artifacts: Vec::new(),
            reviewer: None,
            result: None,
            progress: None,
            note: None,
            timeout_secs: None,
        }
    }

    #[test]
    fn executing_and_waiting_work_agree_across_team_and_inspector_then_replacement() {
        let mut core = new_core();
        let mut snapshot = fixture();
        snapshot.server_time = 1_000;
        snapshot.tasks = vec![
            task("older", TaskStatus::Working, Some(100)),
            task("parked", TaskStatus::Waiting, Some(500)),
            task("unknown", TaskStatus::Waiting, None),
        ];
        load(&mut core, snapshot.clone(), 1);
        let ViewPayload::Team(team) = project(&core, ViewQuery::Team).unwrap().payload else {
            panic!("team payload")
        };
        let member = team
            .agents
            .iter()
            .find(|agent| agent.agent.id == "claude-1")
            .unwrap();
        assert_eq!(member.current_work.as_ref().unwrap().task_id, "older");
        assert_eq!(member.current_work.as_ref().unwrap().elapsed_ms, Some(900));
        assert_eq!(member.waiting_task_ids, ["parked", "unknown"]);
        let ViewPayload::AgentDetail(detail) = project(
            &core,
            ViewQuery::AgentDetail {
                id: "claude-1".into(),
            },
        )
        .unwrap()
        .payload
        else {
            panic!("detail payload")
        };
        assert_eq!(
            detail.agent.as_ref().unwrap().current_work,
            member.current_work
        );
        assert_eq!(detail.assigned_tasks.len(), 3);
        snapshot.tasks = vec![task("unknown", TaskStatus::Working, None)];
        load(&mut core, snapshot, 2);
        let ViewPayload::Team(replaced) = project(&core, ViewQuery::Team).unwrap().payload else {
            panic!("team payload")
        };
        let member = replaced
            .agents
            .iter()
            .find(|agent| agent.agent.id == "claude-1")
            .unwrap();
        assert_eq!(member.current_work.as_ref().unwrap().task_id, "unknown");
        assert_eq!(member.current_work.as_ref().unwrap().elapsed_ms, None);
        assert!(member.waiting_task_ids.is_empty());
    }

    #[test]
    fn replacement_expires_real_history_cursors_and_missing_entities_stay_missing() {
        let mut core = new_core();
        load(&mut core, fixture(), 1);
        let filter = MessageFilter {
            endpoints: MessageEndpoints::All,
            kind: None,
            text: String::new(),
        };
        let ViewPayload::Messages(first) = project(
            &core,
            ViewQuery::Messages {
                before: None,
                limit: 1,
                filter: filter.clone(),
            },
        )
        .unwrap()
        .payload
        else {
            panic!("messages payload")
        };
        assert!(first.has_older_local);
        assert!(first.may_have_older_remote);
        load(&mut core, fixture(), 2);
        assert_eq!(
            project(
                &core,
                ViewQuery::Messages {
                    before: first.before,
                    limit: 1,
                    filter
                }
            ),
            Err(ClientError::CursorExpired)
        );
        let ViewPayload::TaskDetail(detail) = project(
            &core,
            ViewQuery::TaskDetail {
                id: "missing".into(),
            },
        )
        .unwrap()
        .payload
        else {
            panic!("task detail payload")
        };
        assert_eq!(detail.task, None);
        assert_eq!(detail.card.status, ResourceStatus::Idle);
        assert_eq!(
            detail.card.key.entity.entity,
            Entity::Task("missing".into())
        );
    }

    #[test]
    fn evidence_keeps_exact_options_errors_and_sequences_and_rejects_foreign_scope() {
        let mut core = new_core();
        load(&mut core, fixture(), 1);
        let key = ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Agent("claude-1".into()),
            },
            kind: ResourceKind::Output,
            options: ReadOptions::Output {
                lines: 50,
                screen: false,
                ansi: false,
            },
        };
        let expected = ResourceState {
            key: key.clone(),
            status: ResourceStatus::Failed,
            request_sequence: 9,
            error: Some(ClientReason {
                details: None,
                code: "read_failed".into(),
                message: "unavailable evidence".into(),
            }),
            value: None,
        };
        core.state.resources.insert(key.clone(), expected.clone());
        assert_eq!(
            project(&core, ViewQuery::Evidence { key: key.clone() })
                .unwrap()
                .payload,
            ViewPayload::Evidence(Box::new(expected))
        );
        let mut other_options = key.clone();
        other_options.options = ReadOptions::Output {
            lines: 20,
            screen: false,
            ansi: false,
        };
        let ViewPayload::Evidence(unread) =
            project(&core, ViewQuery::Evidence { key: other_options })
                .unwrap()
                .payload
        else {
            panic!("evidence payload")
        };
        assert_eq!(unread.status, ResourceStatus::Idle);
        let mut foreign = key.clone();
        foreign.entity.scope.attempt += 1;
        assert!(
            matches!(project(&core, ViewQuery::Evidence { key: foreign }), Err(ClientError::InvalidInput { field, .. }) if field == "resource.scope")
        );
        core.apply(
            ClientInput::Unbind {
                scope: scope(),
                reason: ClientReason {
                    details: None,
                    code: "closed".into(),
                    message: "connection closed".into(),
                },
            },
            clock(),
        )
        .unwrap();
        let retained = project(&core, ViewQuery::Evidence { key }).unwrap();
        assert_eq!(retained.freshness, Freshness::Stale);
    }

    #[test]
    fn action_flags_are_the_actual_planners_and_unknown_usage_is_not_a_report() {
        let mut core = new_core();
        let mut snapshot = fixture();
        for agent in &mut snapshot.agents {
            agent.usage = None;
            agent.context = None;
        }
        load(&mut core, snapshot, 1);
        let ViewPayload::Team(team) = project(&core, ViewQuery::Team).unwrap().payload else {
            panic!("team payload")
        };
        let member = &team.agents[0];
        let ctx = context(&core.state);
        for availability in &member.actions {
            assert_eq!(
                *availability,
                core.actions.availability(
                    availability.action,
                    Some(&member.agent.id),
                    &ctx,
                    clock()
                )
            );
        }
        let ViewPayload::Usage(usage) = project(&core, ViewQuery::Usage).unwrap().payload else {
            panic!("usage payload")
        };
        assert_eq!(usage.total, None);
        assert_eq!(usage.reporters, 0);
        assert!(usage.agents.iter().all(|agent| agent.context.is_none()));
    }

    #[test]
    fn no_selection_never_manufactures_a_scope_and_inactive_queries_keep_retained_data() {
        let state = ClientState::new(ClientConfig {
            limits: ClientLimits {
                messages: 10,
                events: 10,
                context_points: 3,
                buffered_pushes: 8,
                pending_operations: 8,
                page_size: 10,
            },
            snapshot_mode: SnapshotMode::OrderedConnection,
        })
        .unwrap();
        let actions = Actions::new(8).unwrap();
        let data = view(&state, &actions, ViewQuery::Overview, clock()).unwrap();
        assert_eq!(data.scope, None);
        assert_eq!(data.freshness, Freshness::Unavailable);
        assert!(
            matches!(view(&state, &actions, ViewQuery::TaskDetail { id: "missing".into() }, clock()), Err(ClientError::InvalidInput { field, .. }) if field == "scope")
        );
        let mut core = new_core();
        load(&mut core, fixture(), 1);
        core.apply(ClientInput::Lifecycle { active: false }, clock())
            .unwrap();
        let retained = project(&core, ViewQuery::Team).unwrap();
        assert_eq!(retained.freshness, Freshness::Stale);
        let ViewPayload::Team(team) = retained.payload else {
            panic!("team payload")
        };
        assert_eq!(team.agents.len(), 7);
    }

    #[test]
    fn retained_read_values_become_stale_without_losing_json_or_option_identity() {
        let mut core = new_core();
        let mut snapshot = fixture();
        snapshot.messages[0].metadata = serde_json::json!({"nested":{"unknown":null,"count":0}});
        let message = snapshot.messages[0].clone();
        load(&mut core, snapshot, 1);
        let key = ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Message(message.id.clone()),
            },
            kind: ResourceKind::Message,
            options: ReadOptions::Default,
        };
        core.state.resources.insert(
            key.clone(),
            ResourceState {
                key: key.clone(),
                status: ResourceStatus::Ready,
                request_sequence: 4,
                error: None,
                value: Some(ResourceValue::Message(Some(message.clone()))),
            },
        );
        core.apply(
            ClientInput::Unbind {
                scope: scope(),
                reason: ClientReason {
                    details: None,
                    code: "closed".into(),
                    message: "closed".into(),
                },
            },
            clock(),
        )
        .unwrap();
        let ViewPayload::Evidence(read) =
            project(&core, ViewQuery::Evidence { key }).unwrap().payload
        else {
            panic!("evidence payload")
        };
        assert_eq!(read.status, ResourceStatus::Stale);
        assert_eq!(read.request_sequence, 4);
        assert_eq!(read.value, Some(ResourceValue::Message(Some(message))));
    }

    #[test]
    fn inspector_resources_have_deterministic_order_and_keep_other_options_separate() {
        let mut core = new_core();
        load(&mut core, fixture(), 1);
        let entity = EntityKey {
            scope: scope(),
            entity: Entity::Agent("claude-1".into()),
        };
        let key = |kind, options| ResourceKey {
            entity: entity.clone(),
            kind,
            options,
        };
        for key in [
            key(
                ResourceKind::Output,
                ReadOptions::Output {
                    lines: 50,
                    screen: false,
                    ansi: false,
                },
            ),
            key(ResourceKind::Transcript, ReadOptions::Bounded { limit: 10 }),
            key(
                ResourceKind::Output,
                ReadOptions::Output {
                    lines: 20,
                    screen: false,
                    ansi: false,
                },
            ),
        ] {
            core.state.resources.insert(
                key.clone(),
                ResourceState {
                    key,
                    status: ResourceStatus::Loading,
                    request_sequence: 1,
                    error: None,
                    value: None,
                },
            );
        }
        let ViewPayload::AgentDetail(detail) = project(
            &core,
            ViewQuery::AgentDetail {
                id: "claude-1".into(),
            },
        )
        .unwrap()
        .payload
        else {
            panic!("agent detail payload")
        };
        assert_eq!(
            detail
                .resources
                .iter()
                .map(|resource| resource.key.clone())
                .collect::<Vec<_>>(),
            [
                key(ResourceKind::Transcript, ReadOptions::Bounded { limit: 10 }),
                key(
                    ResourceKind::Output,
                    ReadOptions::Output {
                        lines: 20,
                        screen: false,
                        ansi: false
                    }
                ),
                key(
                    ResourceKind::Output,
                    ReadOptions::Output {
                        lines: 50,
                        screen: false,
                        ansi: false
                    }
                ),
            ]
        );
    }

    #[test]
    fn filtered_event_pages_keep_zero_sequence_arrival_identity_and_scope() {
        let mut core = new_core();
        let mut snapshot = fixture();
        let mut older = snapshot.events[0].clone();
        older.seq = 0;
        older.event_type = "test.notice".into();
        older.agent_id = Some("claude-1".into());
        older.level = crew_protocol::EventLevel::Info;
        older.message = "older evidence".into();
        let mut newer = older.clone();
        newer.message = "newer evidence".into();
        snapshot.events = vec![older, newer];
        load(&mut core, snapshot, 1);
        let filter = EventFilter {
            agent_id: Some("claude-1".into()),
            level: Some(crew_protocol::EventLevel::Info),
            text: "EVIDENCE".into(),
            vcs_only: false,
        };
        let ViewPayload::Events(page) = project(
            &core,
            ViewQuery::Events {
                before: None,
                limit: 1,
                filter: filter.clone(),
            },
        )
        .unwrap()
        .payload
        else {
            panic!("events payload")
        };
        assert_eq!(page.items[0].message, "newer evidence");
        assert!(page.has_older_local);
        let cursor = page.before.unwrap();
        assert_eq!(cursor.scope, scope());
        assert!(matches!(cursor.anchor, HistoryAnchor::Event { seq: 0, .. }));
        let ViewPayload::Events(older) = project(
            &core,
            ViewQuery::Events {
                before: Some(cursor),
                limit: 1,
                filter,
            },
        )
        .unwrap()
        .payload
        else {
            panic!("events payload")
        };
        assert_eq!(older.items[0].message, "older evidence");
        assert!(!older.has_older_local);
        assert!(older.may_have_older_remote);
    }

    #[test]
    fn default_read_options_query_the_same_loading_and_completed_resource_as_planning() {
        let mut core = new_core();
        load(&mut core, fixture(), 1);
        let key = ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Agent("claude-1".into()),
            },
            kind: ResourceKind::Output,
            options: ReadOptions::Default,
        };
        let planned = core
            .plan(
                ActionIntent::Read {
                    scope: scope(),
                    resource: ResourceRequest { key: key.clone() },
                },
                clock(),
            )
            .unwrap();
        let ActionPlan::Dispatch { operation_id } = planned.plan else {
            panic!("read dispatch")
        };
        let ViewPayload::Evidence(loading) =
            project(&core, ViewQuery::Evidence { key: key.clone() })
                .unwrap()
                .payload
        else {
            panic!("evidence payload")
        };
        assert_eq!(loading.status, ResourceStatus::Loading);
        assert_eq!(
            loading.key.options,
            ReadOptions::Output {
                lines: 300,
                screen: false,
                ansi: false
            }
        );
        core.apply(
            ClientInput::OperationFinished {
                scope: scope(),
                operation_id,
                outcome: OperationOutcome::Succeeded {
                    result: CommandResult::Output(crew_protocol::rpc::OutputResult {
                        text: "retained output".into(),
                        alive: true,
                    }),
                },
            },
            clock(),
        )
        .unwrap();
        let ViewPayload::Evidence(ready) =
            project(&core, ViewQuery::Evidence { key }).unwrap().payload
        else {
            panic!("evidence payload")
        };
        assert_eq!(ready.status, ResourceStatus::Ready);
        assert_eq!(
            ready.value,
            Some(ResourceValue::Output(crew_protocol::rpc::OutputResult {
                text: "retained output".into(),
                alive: true
            }))
        );
        let invalid = ResourceKey {
            entity: ready.key.entity.clone(),
            kind: ResourceKind::Transcript,
            options: ReadOptions::Output {
                lines: 20,
                screen: false,
                ansi: false,
            },
        };
        assert!(matches!(
            project(&core, ViewQuery::Evidence { key: invalid }),
            Err(ClientError::InvalidInput { .. })
        ));
    }

    #[test]
    fn local_event_evidence_uses_checked_sequences_and_latest_zero_without_a_read_command() {
        let mut core = new_core();
        let mut snapshot = fixture();
        let mut first = snapshot.events[0].clone();
        first.seq = 0;
        first.message = "first zero".into();
        let mut latest = first.clone();
        latest.message = "latest zero".into();
        snapshot.events = vec![first, latest.clone()];
        load(&mut core, snapshot, 1);
        let key = |seq| ResourceKey {
            entity: EntityKey {
                scope: scope(),
                entity: Entity::Event(seq),
            },
            kind: ResourceKind::Event,
            options: ReadOptions::Default,
        };
        assert_eq!(crate::actions::resource_command(&key(0)).unwrap(), None);
        let ViewPayload::Evidence(event) = project(&core, ViewQuery::Evidence { key: key(0) })
            .unwrap()
            .payload
        else {
            panic!("event evidence")
        };
        assert_eq!(event.status, ResourceStatus::Ready);
        assert_eq!(event.value, Some(ResourceValue::Event(Some(latest))));
        let ViewPayload::Evidence(missing) =
            project(&core, ViewQuery::Evidence { key: key(u64::MAX) })
                .unwrap()
                .payload
        else {
            panic!("event evidence")
        };
        assert_eq!(missing.status, ResourceStatus::Empty);
        assert_eq!(missing.value, Some(ResourceValue::Event(None)));
        assert_eq!(missing.request_sequence, 0);
    }

    #[test]
    fn toolbox_attention_is_an_agent_warning_with_its_reason() {
        let mut core = new_core();
        let mut snapshot = fixture();
        let mut agents: Vec<Agent> =
            serde_json::from_str(include_str!("../../crew/tests/fixtures/web/toolbox.json"))
                .unwrap();
        // A required item that is not active needs attention too.
        let mut blocked = agents[0].clone();
        blocked.id = "qa-2".into();
        let item = &mut blocked.toolbox.as_mut().unwrap().items[1];
        assert!(item.required);
        item.status.code = crew_protocol::ToolboxItemCode::Needs;
        item.status.text = "Needs $PLAYWRIGHT_TOKEN".into();
        agents.push(blocked);
        snapshot.agents = agents;
        load(&mut core, snapshot, 1);
        let ViewPayload::Team(team) = project(&core, ViewQuery::Team).unwrap().payload else {
            panic!("team payload")
        };
        let toolbox = |id: &str| {
            team.agents
                .iter()
                .find(|member| member.agent.id == id)
                .unwrap()
                .warnings
                .iter()
                .filter(|warning| warning.code == TOOLBOX_WARNING)
                .map(|warning| warning.text.as_str())
                .collect::<Vec<_>>()
        };
        assert!(toolbox("qa").is_empty(), "all required items are active");
        assert_eq!(toolbox("release"), [crew_protocol::TOOLBOX_PENDING]);
        assert_eq!(
            toolbox("qa-2"),
            ["Required playwright: Needs $PLAYWRIGHT_TOKEN"]
        );
    }
}

fn feature_view(
    state: &ClientState,
    key: ResourceKey,
    feature: crew_protocol::features::Feature,
    wrap: fn(Box<ResourceState>) -> ViewPayload,
) -> Result<ViewPayload, ClientError> {
    if &key.entity.scope != selected_scope(state)? {
        return Err(ClientError::InvalidInput {
            field: "resource.scope".into(),
            reason: "feature belongs to another workspace or attempt".into(),
        });
    }
    let key = canonical_resource_key(&key)?;
    let ReadOptions::Feature(read) = &key.options else {
        return Err(ClientError::InvalidInput {
            field: "resource.options".into(),
            reason: "expected feature read".into(),
        });
    };
    if read.command().required_feature() != Some(feature) {
        return Err(ClientError::InvalidInput {
            field: "resource.kind".into(),
            reason: "feature view differs from resource".into(),
        });
    }
    Ok(wrap(Box::new(state.current_resource(&key))))
}
