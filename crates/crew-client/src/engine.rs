//! Serialized orchestration of real domain reduction and action planning.
//! Hosts execute returned effects; the core never performs IO or callbacks.

use crate::actions::{ActionContext, ActionUpdate, Actions};
use crate::api::*;
use crate::state::{ClientState, StateUpdate};

pub struct ClientCore {
    pub(crate) state: ClientState,
    pub(crate) actions: Actions,
}

impl ClientCore {
    pub fn new(config: ClientConfig) -> Result<Self, ClientError> {
        Ok(Self {
            state: ClientState::new(config)?,
            actions: Actions::new(config.limits.pending_operations)?,
        })
    }

    pub fn apply(
        &mut self,
        input: ClientInput,
        clock: ClockSample,
    ) -> Result<ClientUpdate, ClientError> {
        self.state.ensure_writable()?;
        let mut action = ActionUpdate::default();
        let mut data = StateUpdate::default();
        match input {
            ClientInput::OperationFinished {
                scope,
                operation_id,
                outcome,
            } => {
                let context = context(&self.state);
                action = self
                    .actions
                    .finish(&scope, operation_id, outcome, &context, clock)?;
            }
            ClientInput::AttachmentEnded {
                scope,
                attachment_id,
                ..
            } => {
                action = self.actions.attachment_ended(&scope, attachment_id);
            }
            ClientInput::Wake {
                timer_id,
                monotonic_ms,
            } => {
                action = self.actions.wake(
                    timer_id,
                    ClockSample {
                        monotonic_ms,
                        ..clock
                    },
                );
            }
            ClientInput::Bind {
                scope,
                mode,
                grant,
                snapshot_mode,
            } => {
                if self.state.stale_bind(&scope) {
                    return self.finish(StateUpdate::default(), ActionUpdate::default());
                }
                self.state
                    .validate_active_bind(&scope, mode, &grant, snapshot_mode)?;
                if let Some(verified) = &self.state.verified_binding {
                    let changed = verified.scope.endpoint_id != scope.endpoint_id
                        || verified.scope.workspace_id != scope.workspace_id
                        || known_differs(verified.scope.generation, scope.generation)
                        || known_differs(verified.scope.daemon_pid, scope.daemon_pid)
                        || known_differs(
                            verified.scope.daemon_started_at_ms,
                            scope.daemon_started_at_ms,
                        )
                        || verified.grant != grant
                        || verified.mode != mode
                        || grant
                            .expires_at_ms
                            .is_some_and(|expiry| clock.wall_ms >= expiry);
                    if changed {
                        action.append(self.actions.invalidate_retries(
                            &verified.scope,
                            why(
                                "binding_changed",
                                "captured workspace identity or authority changed",
                            ),
                        ));
                    }
                }
                if let Some(old) = &self.state.connection {
                    if old.scope != scope
                        || old.grant != grant
                        || old.mode != mode
                        || old.snapshot_mode != snapshot_mode
                    {
                        let compatible = old.scope.endpoint_id == scope.endpoint_id
                            && old.scope.workspace_id == scope.workspace_id
                            && !known_differs(old.scope.generation, scope.generation)
                            && !known_differs(old.scope.daemon_pid, scope.daemon_pid)
                            && !known_differs(
                                old.scope.daemon_started_at_ms,
                                scope.daemon_started_at_ms,
                            )
                            && old.grant == grant
                            && old.mode == mode;
                        let reason = if compatible {
                            why("closed", "workspace connection replaced")
                        } else {
                            why("binding_changed", "workspace binding changed")
                        };
                        action.append(self.actions.retire(&old.scope, reason, clock)?);
                    }
                } else if let Some(old) = &self.state.scope {
                    if old.endpoint_id != scope.endpoint_id
                        || old.workspace_id != scope.workspace_id
                    {
                        // A disconnected core can still hold the sole message
                        // retry. Explicit navigation abandons that submission;
                        // verification-only normalization keeps its workspace.
                        action.append(self.actions.retire(
                            old,
                            why("binding_changed", "selected endpoint or workspace changed"),
                            clock,
                        )?);
                    }
                }
                data = self.state.apply_data(
                    ClientInput::Bind {
                        scope,
                        mode,
                        grant,
                        snapshot_mode,
                    },
                    clock,
                )?;
            }
            ClientInput::Unbind { scope, reason } => {
                if self.state.accepts_unbind(&scope, &reason) {
                    if reason.code == "canonical_scope" && self.state.loaded {
                        return Err(ClientError::InvalidInput {
                            field: "scope".into(),
                            reason:
                                "canonical refinement must precede the attempt's first snapshot"
                                    .into(),
                        });
                    }
                    if !matches!(reason.code.as_str(), "closed" | "canonical_scope") {
                        action.append(self.actions.invalidate_retries(&scope, reason.clone()));
                    }
                    action.append(self.actions.retire(&scope, reason.clone(), clock)?);
                }
                data = self
                    .state
                    .apply_data(ClientInput::Unbind { scope, reason }, clock)?;
            }
            other => {
                let abandonment = match &other {
                    ClientInput::EndpointChanged {
                        endpoint_id,
                        attempt,
                        status,
                    } if self.state.accepts_endpoint_event(endpoint_id, *attempt) => {
                        if self
                            .state
                            .scope
                            .as_ref()
                            .is_some_and(|scope| &scope.endpoint_id != endpoint_id)
                        {
                            Some(why("endpoint_changed", "selected endpoint changed"))
                        } else {
                            match status {
                                EndpointStatus::SignedOut => {
                                    Some(why("signed_out", "endpoint authority ended"))
                                }
                                EndpointStatus::Incompatible => {
                                    Some(why("protocol", "endpoint protocol changed"))
                                }
                                EndpointStatus::Stopped => Some(why("stopped", "endpoint stopped")),
                                _ => None,
                            }
                        }
                    }
                    _ => None,
                };
                let held_scope = self
                    .state
                    .verified_binding
                    .as_ref()
                    .map(|binding| binding.scope.clone())
                    .or_else(|| self.state.scope.clone());
                let old = self
                    .state
                    .connection
                    .as_ref()
                    .map(|connection| connection.scope.clone());
                let was_live = self.state.live();
                data = self.state.apply_data(other, clock)?;
                if let (Some(scope), Some(reason)) = (&held_scope, &abandonment) {
                    action.append(self.actions.invalidate_retries(scope, reason.clone()));
                }
                if let Some(scope) = &held_scope {
                    if data.changes.availability && self.state.listing_abandons(scope) {
                        action.append(self.actions.invalidate_retries(
                            scope,
                            why("workspace_changed", "joined workspace identity changed"),
                        ));
                    }
                }
                if was_live && !self.state.live() {
                    if let Some(scope) = old {
                        let reason = abandonment.unwrap_or_else(|| match self.state.workspace {
                            WorkspaceStatus::AccessRevoked => {
                                why("revoked", "workspace access revoked")
                            }
                            WorkspaceStatus::Stopped => why("stopped", "workspace stopped"),
                            WorkspaceStatus::Removed | WorkspaceStatus::NeverJoined => {
                                why("removed", "workspace no longer joined")
                            }
                            WorkspaceStatus::Incompatible => {
                                why("protocol", "workspace protocol changed")
                            }
                            _ => why("closed", "workspace connection closed"),
                        });
                        action.append(self.actions.retire(&scope, reason, clock)?);
                    }
                }
                if data.resynchronized {
                    action.append(self.actions.resynchronized(&context(&self.state), clock)?);
                }
            }
        }
        self.finish(data, action)
    }

    pub fn plan(
        &mut self,
        intent: ActionIntent,
        clock: ClockSample,
    ) -> Result<PlanUpdate, ClientError> {
        self.state.ensure_writable()?;
        let decision = self.actions.plan(intent, &context(&self.state), clock)?;
        let update = self.finish(StateUpdate::default(), decision.update)?;
        Ok(PlanUpdate {
            plan: decision.plan,
            update,
        })
    }

    pub fn view(&self, query: ViewQuery, clock: ClockSample) -> Result<ViewData, ClientError> {
        if let ViewQuery::Availability { intent } = query {
            return availability_view(&self.state, &self.actions, &intent, clock);
        }
        crate::presentation::view(&self.state, &self.actions, query, clock)
    }

    pub fn confirm(
        &mut self,
        id: ConfirmationId,
        clock: ClockSample,
    ) -> Result<ClientUpdate, ClientError> {
        self.state.ensure_writable()?;
        let action = self.actions.confirm(id, &context(&self.state), clock)?;
        self.finish(StateUpdate::default(), action)
    }

    pub fn cancel_confirmation(&mut self, id: ConfirmationId) -> Result<ClientUpdate, ClientError> {
        self.state.ensure_writable()?;
        let action = self.actions.cancel_confirmation(id);
        self.finish(StateUpdate::default(), action)
    }

    fn finish(
        &mut self,
        mut data: StateUpdate,
        action: ActionUpdate,
    ) -> Result<ClientUpdate, ClientError> {
        merge_changes(&mut data.changes, action.changes);
        data.effects.extend(action.effects);
        for read in action.reads_started {
            self.state.begin_read(read.operation_id, read.key)?;
            data.changes.pending = true;
        }
        let mut outcomes = Vec::with_capacity(action.completions.len());
        for completed in action.completions {
            if let (
                Some(Command::Answer { id, .. }),
                OperationOutcome::Succeeded {
                    result: CommandResult::Sent(sent),
                },
            ) = (&completed.command, &completed.outcome)
            {
                if sent.message.reply_to.as_ref() == Some(id)
                    && self.state.matches(&completed.scope)
                {
                    // The returned daemon message is authoritative, even if
                    // the subscribed answer push has not arrived yet.
                    data.changes.questions |= self.state.questions.on_message(&sent.message);
                }
            }
            if self
                .state
                .complete_read(completed.operation_id, &completed.outcome)?
            {
                data.changes.pending = true;
                if let Some(resource) = &completed.resource {
                    data.changes.messages |= resource.kind == ResourceKind::History;
                    data.changes.events |= resource.kind == ResourceKind::Events;
                }
            }
            outcomes.push(completed.notice());
        }
        data.changes.questions |= self
            .state
            .questions
            .sync_answering(self.actions.pending_answer_ids());
        let revision = self.state.commit(&mut data.changes)?;
        Ok(ClientUpdate {
            revision,
            changes: data.changes,
            effects: data.effects,
            outcomes,
        })
    }

    pub fn revision(&self) -> ClientRevision {
        self.state.revision
    }
    pub fn scope(&self) -> Option<&ClientScope> {
        self.state.scope.as_ref()
    }
    pub fn endpoint_status(&self) -> EndpointStatus {
        self.state.endpoint
    }
    pub fn workspace_status(&self) -> WorkspaceStatus {
        self.state.workspace
    }

    pub fn reason(&self) -> Option<&ClientReason> {
        self.state.reason.as_ref()
    }

    /// Server time from the accepted snapshot clock, with the same wide offset
    /// and clamp used by all shared projections.
    pub fn server_now(&self, clock: ClockSample) -> i64 {
        self.state.server_now(clock)
    }

    /// Immutable domain access for Rust hosts with an existing presentation
    /// model. These borrows expose no reducers, queues, cursors or credentials.
    pub fn session(&self) -> Option<&crew_protocol::rpc::SessionInfo> {
        self.state.session.as_ref()
    }

    pub fn agents(&self) -> &[crew_protocol::Agent] {
        &self.state.agents
    }

    pub fn tasks(&self) -> &[crew_protocol::Task] {
        &self.state.tasks
    }

    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &crew_protocol::Message> + '_ {
        self.state.messages.items()
    }

    pub fn events(&self) -> impl DoubleEndedIterator<Item = &crew_protocol::Event> + '_ {
        self.state.events.items()
    }

    pub fn visible_questions(&self) -> impl Iterator<Item = &crew_protocol::Message> + '_ {
        self.state.questions.visible()
    }

    pub fn questions(&self) -> &[crew_protocol::Message] {
        &self.state.questions.open
    }

    pub fn pending_answer_ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.actions.pending_answer_ids()
    }
}

impl ClientApi for ClientCore {
    fn apply(
        &mut self,
        input: ClientInput,
        clock: ClockSample,
    ) -> Result<ClientUpdate, ClientError> {
        ClientCore::apply(self, input, clock)
    }

    fn view(&self, query: ViewQuery, clock: ClockSample) -> Result<ViewData, ClientError> {
        ClientCore::view(self, query, clock)
    }

    fn plan(
        &mut self,
        intent: ActionIntent,
        clock: ClockSample,
    ) -> Result<PlanUpdate, ClientError> {
        ClientCore::plan(self, intent, clock)
    }

    fn confirm(
        &mut self,
        id: ConfirmationId,
        clock: ClockSample,
    ) -> Result<ClientUpdate, ClientError> {
        ClientCore::confirm(self, id, clock)
    }

    fn cancel_confirmation(&mut self, id: ConfirmationId) -> Result<ClientUpdate, ClientError> {
        ClientCore::cancel_confirmation(self, id)
    }
}

pub(crate) fn context(state: &ClientState) -> ActionContext<'_> {
    ActionContext {
        connection: state.connection.as_ref(),
        endpoint: state.endpoint,
        workspace: state.workspace,
        agents: &state.agents,
        tasks: &state.tasks,
        questions: &state.questions.open,
        session: state.session.as_ref(),
    }
}

/// Both the public Core route and the projection module use the same borrowed
/// preflight. The result stays tied to the caller's captured intent scope.
pub(crate) fn availability_view(
    state: &ClientState,
    actions: &Actions,
    intent: &ActionIntent,
    clock: ClockSample,
) -> Result<ViewData, ClientError> {
    let availability = actions.intent_availability(intent, &context(state), clock)?;
    Ok(ViewData {
        scope: Some(crate::actions::action_scope(intent).clone()),
        revision: state.revision,
        synchronized_at_ms: state.synchronized_at_ms,
        freshness: state.freshness(),
        payload: ViewPayload::Availability(availability),
    })
}

fn why(code: &str, message: &str) -> ClientReason {
    ClientReason {
        code: code.into(),
        message: message.into(),
    }
}

fn known_differs<T: PartialEq>(old: Option<T>, new: Option<T>) -> bool {
    matches!((old,new), (Some(old),Some(new)) if old != new)
}

fn merge_changes(target: &mut ChangeSet, other: ChangeSet) {
    target.agents.extend(other.agents);
    target.tasks.extend(other.tasks);
    target.removed.extend(other.removed);
    target.session |= other.session;
    target.questions |= other.questions;
    target.messages |= other.messages;
    target.events |= other.events;
    target.usage |= other.usage;
    target.availability |= other.availability;
    target.pending |= other.pending;
    target.reset |= other.reset;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::NATIVE_LIMITS;
    use crate::state::changes_visible;
    use crew_protocol::rpc::{Push, Snapshot};

    fn scope(attempt: u64) -> ClientScope {
        ClientScope {
            endpoint_id: "local:e".into(),
            workspace_id: "w".into(),
            generation: None,
            daemon_pid: Some(1),
            daemon_started_at_ms: Some(1),
            attempt,
        }
    }
    fn clock(at: u64) -> ClockSample {
        ClockSample {
            wall_ms: at as i64,
            monotonic_ms: at,
        }
    }
    fn snapshot() -> Snapshot {
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/snapshot.json")).unwrap()
    }
    fn bind(core: &mut ClientCore, attempt: u64) {
        core.apply(
            ClientInput::Bind {
                scope: scope(attempt),
                mode: ConnectionMode::LocalOperator,
                grant: Grant {
                    grant_id: "g".into(),
                    tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
                    workspace_ids: vec!["w".into()],
                    expires_at_ms: None,
                },
                snapshot_mode: SnapshotMode::OrderedConnection,
            },
            clock(0),
        )
        .unwrap();
    }
    fn load(core: &mut ClientCore, attempt: u64, id: u64, at: u64) -> ClientUpdate {
        core.apply(
            ClientInput::SnapshotStarted {
                scope: scope(attempt),
                operation_id: id,
            },
            clock(at),
        )
        .unwrap();
        core.apply(
            ClientInput::Snapshot {
                scope: scope(attempt),
                operation_id: id,
                snapshot: Box::new(snapshot()),
            },
            clock(at),
        )
        .unwrap()
    }
    fn core() -> ClientCore {
        let mut core = ClientCore::new(ClientConfig {
            limits: NATIVE_LIMITS,
            snapshot_mode: SnapshotMode::OrderedConnection,
        })
        .unwrap();
        bind(&mut core, 1);
        load(&mut core, 1, 1 << 63, 0);
        core
    }

    #[test]
    fn immutable_server_clock_getter_preserves_the_extreme_snapshot_sample() {
        let mut core = core();
        let extreme = ClockSample {
            wall_ms: i64::MIN,
            monotonic_ms: 0,
        };
        core.apply(
            ClientInput::SnapshotStarted {
                scope: scope(1),
                operation_id: (1 << 63) + 1,
            },
            extreme,
        )
        .unwrap();
        let mut snapshot = snapshot();
        snapshot.server_time = i64::MAX;
        core.apply(
            ClientInput::Snapshot {
                scope: scope(1),
                operation_id: (1 << 63) + 1,
                snapshot: Box::new(snapshot),
            },
            extreme,
        )
        .unwrap();
        let revision = core.revision();
        for wall_ms in [i64::MIN, i64::MAX] {
            let sample = ClockSample {
                wall_ms,
                monotonic_ms: 0,
            };
            assert_eq!(core.server_now(sample), core.state.server_now(sample));
            assert_eq!(core.server_now(sample), i64::MAX);
        }
        assert_eq!(core.revision(), revision);
    }

    #[test]
    fn attached_completion_carries_its_lifetime_id_through_the_actual_client_api_once() {
        let mut core = core();
        let api: &mut dyn ClientApi = &mut core;
        let planned = api
            .plan(
                ActionIntent::Attach {
                    scope: scope(1),
                    agent: "claude-1".into(),
                    rows: 24,
                    cols: 80,
                },
                clock(1),
            )
            .unwrap();
        let ActionPlan::Dispatch { operation_id } = planned.plan else {
            panic!("live attachment dispatch");
        };
        assert!(planned.update.outcomes.is_empty());
        let complete = ClientInput::OperationFinished {
            scope: scope(1),
            operation_id,
            outcome: OperationOutcome::Succeeded {
                result: CommandResult::Attached {
                    agent: "claude-1".into(),
                },
            },
        };
        let update = api.apply(complete.clone(), clock(2)).unwrap();
        assert_eq!(
            update.outcomes,
            [OperationNotice {
                scope: scope(1),
                operation_id,
                attachment_id: Some(operation_id),
                kind: OperationOutcomeKind::Success,
                reason: None,
            }]
        );
        let repeated = api.apply(complete, clock(3)).unwrap();
        assert!(repeated.outcomes.is_empty());
        assert_eq!(repeated.revision, update.revision);

        // The successful notice identifies the established lifetime, rather
        // than a later silent write's fresh logical operation.
        let input = api
            .plan(
                ActionIntent::AttachInput {
                    scope: scope(1),
                    attachment_id: operation_id,
                    data: b"pwd\r".to_vec(),
                },
                clock(4),
            )
            .unwrap();
        let ActionPlan::Dispatch {
            operation_id: write,
        } = input.plan
        else {
            panic!("established lifetime accepts input");
        };
        assert_ne!(write, operation_id);
        let sent = api
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(1),
                    operation_id: write,
                    outcome: OperationOutcome::SentUnacknowledged {
                        attachment_id: operation_id,
                    },
                },
                clock(5),
            )
            .unwrap();
        assert_eq!(sent.outcomes.len(), 1);
        assert_eq!(sent.outcomes[0].operation_id, write);
        assert_eq!(sent.outcomes[0].attachment_id, Some(operation_id));
        assert_eq!(
            sent.outcomes[0].kind,
            OperationOutcomeKind::SentUnacknowledged
        );
    }

    fn send(core: &mut ClientCore) -> OperationId {
        match core
            .plan(
                ActionIntent::SendMessage {
                    scope: scope(1),
                    to: "claude-1".into(),
                    body: "message".into(),
                    message_id: "stable-id".into(),
                },
                clock(1),
            )
            .unwrap()
            .plan
        {
            ActionPlan::Dispatch { operation_id } => operation_id,
            _ => panic!("live authorized send"),
        }
    }

    fn disconnect_with_held_send(core: &mut ClientCore) -> (OperationId, TimerId) {
        let id = send(core);
        let held = core
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(1),
                    operation_id: id,
                    outcome: OperationOutcome::OutcomeUnknown {
                        reason: why("closed", "closed"),
                    },
                },
                clock(5),
            )
            .unwrap();
        let timer = held
            .effects
            .iter()
            .find_map(|effect| match effect {
                ClientEffect::Schedule { timer_id, .. } => Some(*timer_id),
                _ => None,
            })
            .unwrap();
        core.apply(
            ClientInput::Unbind {
                scope: scope(1),
                reason: why("closed", "closed"),
            },
            clock(6),
        )
        .unwrap();
        (id, timer)
    }

    #[test]
    fn every_trusted_abandonment_finalizes_a_held_send_once_even_when_already_unbound() {
        for case in 0..8 {
            let mut core = core();
            let (id, timer) = disconnect_with_held_send(&mut core);
            let input = match case {
                0 => ClientInput::Unbind {
                    scope: scope(1),
                    reason: why("revoked", "revoked"),
                },
                1..=3 => ClientInput::EndpointChanged {
                    endpoint_id: scope(1).endpoint_id,
                    attempt: 2,
                    status: match case {
                        1 => EndpointStatus::SignedOut,
                        2 => EndpointStatus::Incompatible,
                        _ => EndpointStatus::Stopped,
                    },
                },
                4 => ClientInput::Unbind {
                    scope: scope(1),
                    reason: why("disposed", "client disposed"),
                },
                5 => {
                    let mut provisional = scope(2);
                    provisional.daemon_started_at_ms = None;
                    core.apply(
                        ClientInput::Bind {
                            scope: provisional.clone(),
                            mode: ConnectionMode::LocalOperator,
                            grant: Grant {
                                grant_id: "g".into(),
                                tiers: vec![
                                    GrantTier::Read,
                                    GrantTier::Control,
                                    GrantTier::Terminal,
                                ],
                                workspace_ids: vec!["w".into()],
                                expires_at_ms: None,
                            },
                            snapshot_mode: SnapshotMode::OrderedConnection,
                        },
                        clock(7),
                    )
                    .unwrap();
                    ClientInput::Unbind {
                        scope: provisional,
                        reason: why("revoked", "revoked"),
                    }
                }
                _ => {
                    let mut next = scope(2);
                    let mut grant = Grant {
                        grant_id: "g".into(),
                        tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
                        workspace_ids: vec!["w".into()],
                        expires_at_ms: None,
                    };
                    if case == 6 {
                        next.daemon_pid = Some(99);
                    } else {
                        grant.grant_id = "new-authority".into();
                    }
                    ClientInput::Bind {
                        scope: next,
                        mode: ConnectionMode::LocalOperator,
                        grant,
                        snapshot_mode: SnapshotMode::OrderedConnection,
                    }
                }
            };
            let retired = core.apply(input.clone(), clock(8)).unwrap();
            assert_eq!(retired.outcomes.len(), 1, "case{case}");
            assert_eq!(retired.outcomes[0].operation_id, id);
            assert_eq!(retired.outcomes[0].scope, scope(1));
            assert_eq!(retired.outcomes[0].kind, OperationOutcomeKind::Unknown);
            assert!(retired
                .effects
                .contains(&ClientEffect::CancelTimer { timer_id: timer }));
            assert!(core.apply(input, clock(9)).unwrap().outcomes.is_empty());
            let expired = core
                .apply(
                    ClientInput::Wake {
                        timer_id: timer,
                        monotonic_ms: 50_000,
                    },
                    clock(50_000),
                )
                .unwrap();
            assert!(expired.outcomes.is_empty());
        }
    }

    #[test]
    fn an_active_equal_attempt_change_cannot_replace_authority_or_retire_pending_operations() {
        let mut core = core();
        send(&mut core);
        let binding = core.state.connection.as_ref().unwrap().clone();
        let revision = core.revision();
        for case in 0..4 {
            let mut changed = binding.clone();
            match case {
                0 => changed.scope.workspace_id = "other".into(),
                1 => changed.grant.grant_id = "other".into(),
                2 => changed.mode = ConnectionMode::ScopedGateway,
                _ => changed.snapshot_mode = SnapshotMode::BufferedOverlap,
            }
            assert!(core
                .apply(
                    ClientInput::Bind {
                        scope: changed.scope,
                        mode: changed.mode,
                        grant: changed.grant,
                        snapshot_mode: changed.snapshot_mode
                    },
                    clock(2)
                )
                .is_err());
            assert_eq!(core.scope(), Some(&scope(1)));
            assert_eq!(core.revision(), revision);
            assert_eq!(core.actions.pending_count(), 1);
        }
    }

    #[test]
    fn a_late_unbind_cannot_revoke_the_new_attempts_held_submission() {
        let mut core = core();
        core.apply(
            ClientInput::Unbind {
                scope: scope(1),
                reason: why("closed", "closed"),
            },
            clock(1),
        )
        .unwrap();
        bind(&mut core, 2);
        load(&mut core, 2, (1 << 63) + 1, 2);
        let planned = core
            .plan(
                ActionIntent::SendMessage {
                    scope: scope(2),
                    to: "claude-1".into(),
                    body: "new message".into(),
                    message_id: "new-id".into(),
                },
                clock(3),
            )
            .unwrap();
        let ActionPlan::Dispatch { operation_id } = planned.plan else {
            panic!("authorized");
        };
        core.apply(
            ClientInput::OperationFinished {
                scope: scope(2),
                operation_id,
                outcome: OperationOutcome::OutcomeUnknown {
                    reason: why("closed", "closed"),
                },
            },
            clock(4),
        )
        .unwrap();
        core.apply(
            ClientInput::Unbind {
                scope: scope(2),
                reason: why("closed", "closed"),
            },
            clock(5),
        )
        .unwrap();
        let revision = core.revision();
        let stale = core
            .apply(
                ClientInput::Unbind {
                    scope: scope(1),
                    reason: why("revoked", "old authority revoked"),
                },
                clock(6),
            )
            .unwrap();
        assert!(stale.outcomes.is_empty());
        assert!(stale.effects.is_empty());
        assert_eq!(core.revision(), revision);
        assert_eq!(core.actions.pending_count(), 1);
    }

    #[test]
    fn selecting_a_new_endpoint_before_its_workspace_bind_retires_old_evidence_and_retry() {
        let mut core = core();
        let (id, timer) = disconnect_with_held_send(&mut core);
        let selected = core
            .apply(
                ClientInput::EndpointChanged {
                    endpoint_id: "gateway:other".into(),
                    attempt: 2,
                    status: EndpointStatus::Connecting,
                },
                clock(7),
            )
            .unwrap();
        assert_eq!(selected.outcomes.len(), 1);
        assert_eq!(selected.outcomes[0].operation_id, id);
        assert!(selected
            .effects
            .contains(&ClientEffect::CancelTimer { timer_id: timer }));
        assert!(core.scope().is_none());
        let ViewPayload::Team(team) = core.view(ViewQuery::Team, clock(8)).unwrap().payload else {
            panic!("team");
        };
        assert!(team.agents.is_empty());
        let revision = core.revision();
        core.apply(
            ClientInput::EndpointChanged {
                endpoint_id: scope(1).endpoint_id,
                attempt: 1,
                status: EndpointStatus::Authenticated,
            },
            clock(9),
        )
        .unwrap();
        assert_eq!(core.revision(), revision);
        assert_eq!(core.endpoint_status(), EndpointStatus::Connecting);
        core.apply(
            ClientInput::EndpointChanged {
                endpoint_id: "gateway:other".into(),
                attempt: 2,
                status: EndpointStatus::Authenticated,
            },
            clock(10),
        )
        .unwrap();
        let mut next = scope(3);
        next.endpoint_id = "gateway:other".into();
        core.apply(
            ClientInput::Bind {
                scope: next.clone(),
                mode: ConnectionMode::ScopedGateway,
                grant: Grant {
                    grant_id: "other".into(),
                    tiers: vec![GrantTier::Read],
                    workspace_ids: vec!["w".into()],
                    expires_at_ms: None,
                },
                snapshot_mode: SnapshotMode::OrderedConnection,
            },
            clock(11),
        )
        .unwrap();
        assert_eq!(core.scope(), Some(&next));
    }

    #[test]
    fn pre_live_local_canonical_refinement_can_normalize_its_single_workspace_grant() {
        let mut core = ClientCore::new(ClientConfig {
            limits: NATIVE_LIMITS,
            snapshot_mode: SnapshotMode::OrderedConnection,
        })
        .unwrap();
        let mut provisional = scope(1);
        provisional.workspace_id = "local:profile".into();
        provisional.daemon_started_at_ms = None;
        let grant = |workspace: &str| Grant {
            grant_id: "g".into(),
            tiers: vec![GrantTier::Read],
            workspace_ids: vec![workspace.into()],
            expires_at_ms: None,
        };
        core.apply(
            ClientInput::Bind {
                scope: provisional.clone(),
                mode: ConnectionMode::LocalOperator,
                grant: grant(&provisional.workspace_id),
                snapshot_mode: SnapshotMode::OrderedConnection,
            },
            clock(0),
        )
        .unwrap();
        core.apply(
            ClientInput::SnapshotStarted {
                scope: provisional.clone(),
                operation_id: 1 << 63,
            },
            clock(0),
        )
        .unwrap();
        core.apply(
            ClientInput::Unbind {
                scope: provisional,
                reason: why("canonical_scope", "canonical scope established"),
            },
            clock(1),
        )
        .unwrap();
        let mut canonical = scope(1);
        canonical.workspace_id = "/work/w".into();
        core.apply(
            ClientInput::Bind {
                scope: canonical.clone(),
                mode: ConnectionMode::LocalOperator,
                grant: grant(&canonical.workspace_id),
                snapshot_mode: SnapshotMode::OrderedConnection,
            },
            clock(1),
        )
        .unwrap();
        assert_eq!(core.scope(), Some(&canonical));
        assert_eq!(core.workspace_status(), WorkspaceStatus::Connecting);
        let mut conflicting = canonical.clone();
        conflicting.daemon_pid = Some(99);
        core.apply(
            ClientInput::Unbind {
                scope: canonical.clone(),
                reason: why("canonical_scope", "canonical scope established"),
            },
            clock(2),
        )
        .unwrap();
        assert!(core
            .apply(
                ClientInput::Bind {
                    scope: conflicting,
                    mode: ConnectionMode::LocalOperator,
                    grant: grant(&canonical.workspace_id),
                    snapshot_mode: SnapshotMode::OrderedConnection
                },
                clock(2)
            )
            .is_err());
        assert_eq!(core.scope(), Some(&canonical));
    }

    #[test]
    fn send_retry_has_one_logical_id_and_one_final_notice_after_a_real_new_barrier() {
        let mut core = core();
        let id = send(&mut core);
        let held = core
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(1),
                    operation_id: id,
                    outcome: OperationOutcome::OutcomeUnknown {
                        reason: why("closed", "connection closed"),
                    },
                },
                clock(5),
            )
            .unwrap();
        assert!(held.outcomes.is_empty());
        assert!(held
            .effects
            .iter()
            .any(|e| matches!(e, ClientEffect::Schedule { .. })));
        core.apply(
            ClientInput::Unbind {
                scope: scope(1),
                reason: why("closed", "closed"),
            },
            clock(6),
        )
        .unwrap();
        bind(&mut core, 2);
        assert!(core
            .apply(
                ClientInput::Push {
                    scope: scope(2),
                    push: Push::Resync
                },
                clock(7)
            )
            .unwrap()
            .effects
            .is_empty());
        let resynced = load(&mut core, 2, (1 << 63) + 1, 8);
        assert!(resynced.outcomes.is_empty());
        assert!(resynced.effects.iter().any(|e| matches!(e,ClientEffect::Request{operation_id,scope,command:Command::Send(params),..} if *operation_id==id && scope.attempt==2 && params.id.as_deref()==Some("stable-id"))));
        let sent = core
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(2),
                    operation_id: id,
                    outcome: OperationOutcome::Succeeded {
                        result: CommandResult::Sent(Box::new(crew_protocol::rpc::SendResult {
                            message: snapshot().messages.remove(0),
                            duplicate: false,
                            recipients: vec!["claude-1".into()],
                        })),
                    },
                },
                clock(9),
            )
            .unwrap();
        assert_eq!(sent.outcomes.len(), 1);
        assert_eq!(sent.outcomes[0].operation_id, id);
        assert!(core
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(2),
                    operation_id: id,
                    outcome: OperationOutcome::Canceled
                },
                clock(10)
            )
            .unwrap()
            .outcomes
            .is_empty());
    }

    #[test]
    fn held_send_expiry_survives_unbind_and_emits_only_one_unknown_notice() {
        let mut core = core();
        let id = send(&mut core);
        let held = core
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(1),
                    operation_id: id,
                    outcome: OperationOutcome::OutcomeUnknown {
                        reason: why("closed", "closed"),
                    },
                },
                clock(5),
            )
            .unwrap();
        let (timer, deadline) = held
            .effects
            .iter()
            .find_map(|e| {
                if let ClientEffect::Schedule {
                    timer_id,
                    deadline_ms,
                } = e
                {
                    Some((*timer_id, *deadline_ms))
                } else {
                    None
                }
            })
            .unwrap();
        core.apply(
            ClientInput::Unbind {
                scope: scope(1),
                reason: why("closed", "closed"),
            },
            clock(6),
        )
        .unwrap();
        let expired = core
            .apply(
                ClientInput::Wake {
                    timer_id: timer,
                    monotonic_ms: deadline,
                },
                clock(deadline),
            )
            .unwrap();
        assert_eq!(expired.outcomes.len(), 1);
        assert_eq!(expired.outcomes[0].kind, OperationOutcomeKind::Unknown);
        assert_eq!(expired.outcomes[0].operation_id, id);
        assert!(core
            .apply(
                ClientInput::Wake {
                    timer_id: timer,
                    monotonic_ms: deadline + 1
                },
                clock(deadline + 1)
            )
            .unwrap()
            .outcomes
            .is_empty());
    }

    #[test]
    fn background_then_foreground_preserves_the_deadline_and_replays_once_after_the_new_barrier() {
        let mut core = core();
        let id = send(&mut core);
        let background = core
            .apply(ClientInput::Lifecycle { active: false }, clock(5))
            .unwrap();
        assert!(background.outcomes.is_empty());
        let timer = background
            .effects
            .iter()
            .find_map(|effect| match effect {
                ClientEffect::Schedule { timer_id, .. } => Some(*timer_id),
                _ => None,
            })
            .unwrap();
        assert!(!background
            .effects
            .contains(&ClientEffect::CancelTimer { timer_id: timer }));
        let retained = core.view(ViewQuery::Team, clock(6)).unwrap();
        assert_eq!(retained.freshness, Freshness::Stale);
        let ViewPayload::Team(team) = retained.payload else {
            panic!("team");
        };
        assert!(!team.agents.is_empty());
        let foreground = core
            .apply(ClientInput::Lifecycle { active: true }, clock(7))
            .unwrap();
        assert!(foreground.outcomes.is_empty());
        assert!(!foreground
            .effects
            .iter()
            .any(|effect| matches!(effect, ClientEffect::Request { .. })));
        bind(&mut core, 2);
        let replay = load(&mut core, 2, (1 << 63) + 1, 8);
        assert!(replay.outcomes.is_empty());
        assert_eq!(replay.effects.iter().filter(|effect| matches!(effect,
            ClientEffect::Request { operation_id,scope,command:Command::Send(params),.. }
                if *operation_id==id && scope.attempt==2 && params.id.as_deref()==Some("stable-id"))).count(), 1);
        assert!(replay
            .effects
            .contains(&ClientEffect::CancelTimer { timer_id: timer }));
        let repeated = load(&mut core, 2, (1 << 63) + 2, 9);
        assert!(!repeated.effects.iter().any(|effect| matches!(effect,
            ClientEffect::Request { operation_id,.. } if *operation_id==id)));
        assert_eq!(core.workspace_status(), WorkspaceStatus::Live);
    }

    #[test]
    fn a_verification_only_provisional_scope_does_not_retire_the_held_send() {
        let mut core = core();
        let id = send(&mut core);
        let held = core
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(1),
                    operation_id: id,
                    outcome: OperationOutcome::OutcomeUnknown {
                        reason: why("closed", "closed"),
                    },
                },
                clock(5),
            )
            .unwrap();
        let timer = held
            .effects
            .iter()
            .find_map(|effect| match effect {
                ClientEffect::Schedule { timer_id, .. } => Some(*timer_id),
                _ => None,
            })
            .unwrap();
        core.apply(
            ClientInput::Unbind {
                scope: scope(1),
                reason: why("closed", "closed"),
            },
            clock(6),
        )
        .unwrap();
        let mut provisional = scope(2);
        provisional.daemon_started_at_ms = None;
        let grant = Grant {
            grant_id: "g".into(),
            tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
            workspace_ids: vec!["w".into()],
            expires_at_ms: None,
        };
        let bound = core
            .apply(
                ClientInput::Bind {
                    scope: provisional.clone(),
                    mode: ConnectionMode::LocalOperator,
                    grant,
                    snapshot_mode: SnapshotMode::OrderedConnection,
                },
                clock(7),
            )
            .unwrap();
        assert!(bound.outcomes.is_empty());
        core.apply(
            ClientInput::SnapshotStarted {
                scope: provisional.clone(),
                operation_id: (1 << 63) + 1,
            },
            clock(7),
        )
        .unwrap();
        let normalized = core
            .apply(
                ClientInput::Unbind {
                    scope: provisional,
                    reason: why("canonical_scope", "startup proof established"),
                },
                clock(8),
            )
            .unwrap();
        assert!(normalized.outcomes.is_empty());
        assert!(!normalized
            .effects
            .contains(&ClientEffect::CancelTimer { timer_id: timer }));
        bind(&mut core, 2);
        let replay = load(&mut core, 2, (1 << 63) + 1, 9);
        assert!(replay.outcomes.is_empty());
        assert!(replay.effects.iter().any(|effect| matches!(effect,ClientEffect::Request { operation_id,scope,.. } if *operation_id==id && scope.attempt==2 && scope.daemon_started_at_ms==Some(1))));
        assert!(replay
            .effects
            .contains(&ClientEffect::CancelTimer { timer_id: timer }));
    }

    #[test]
    fn switching_away_while_disconnected_abandons_the_held_submission() {
        let mut core = core();
        let id = send(&mut core);
        let held = core
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(1),
                    operation_id: id,
                    outcome: OperationOutcome::OutcomeUnknown {
                        reason: why("closed", "closed"),
                    },
                },
                clock(5),
            )
            .unwrap();
        let timer = held
            .effects
            .iter()
            .find_map(|effect| match effect {
                ClientEffect::Schedule { timer_id, .. } => Some(*timer_id),
                _ => None,
            })
            .unwrap();
        core.apply(
            ClientInput::Unbind {
                scope: scope(1),
                reason: why("closed", "closed"),
            },
            clock(6),
        )
        .unwrap();
        let mut other = scope(2);
        other.workspace_id = "other".into();
        let switched = core
            .apply(
                ClientInput::Bind {
                    scope: other.clone(),
                    mode: ConnectionMode::LocalOperator,
                    grant: Grant {
                        grant_id: "other".into(),
                        tiers: vec![GrantTier::Read],
                        workspace_ids: vec!["other".into()],
                        expires_at_ms: None,
                    },
                    snapshot_mode: SnapshotMode::OrderedConnection,
                },
                clock(7),
            )
            .unwrap();
        assert_eq!(switched.outcomes.len(), 1);
        assert_eq!(switched.outcomes[0].operation_id, id);
        assert_eq!(switched.outcomes[0].kind, OperationOutcomeKind::Unknown);
        assert!(switched
            .effects
            .contains(&ClientEffect::CancelTimer { timer_id: timer }));
        core.apply(
            ClientInput::Unbind {
                scope: other,
                reason: why("closed", "closed"),
            },
            clock(8),
        )
        .unwrap();
        bind(&mut core, 3);
        let returned = load(&mut core, 3, (1 << 63) + 2, 9);
        assert!(returned.outcomes.is_empty());
        assert!(!returned.effects.iter().any(
            |effect| matches!(effect,ClientEffect::Request { operation_id,.. } if *operation_id==id)
        ));
    }

    #[test]
    fn a_late_bind_does_not_retire_the_new_scopes_pending_action() {
        let mut core = core();
        core.apply(
            ClientInput::Unbind {
                scope: scope(1),
                reason: why("closed", "closed"),
            },
            clock(1),
        )
        .unwrap();
        bind(&mut core, 2);
        load(&mut core, 2, (1 << 63) + 1, 2);
        let dispatched = core
            .plan(
                ActionIntent::SendMessage {
                    scope: scope(2),
                    to: "claude-1".into(),
                    body: "new context".into(),
                    message_id: "new-id".into(),
                },
                clock(3),
            )
            .unwrap();
        let ActionPlan::Dispatch { operation_id } = dispatched.plan else {
            panic!("authorized action");
        };
        let revision = core.revision();
        bind(&mut core, 1);
        assert_eq!(core.revision(), revision);
        assert_eq!(core.actions.pending_count(), 1);
        let finished = core
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(2),
                    operation_id,
                    outcome: OperationOutcome::Canceled,
                },
                clock(4),
            )
            .unwrap();
        assert_eq!(finished.outcomes.len(), 1);
    }

    #[test]
    fn invalid_binding_and_exhausted_revision_leave_pending_actions_untouched() {
        let mut core = core();
        let id = send(&mut core);
        let pending = core.actions.pending_count();
        let revision = core.revision();
        let mut invalid = scope(2);
        invalid.workspace_id.clear();
        assert!(core
            .apply(
                ClientInput::Bind {
                    scope: invalid,
                    mode: ConnectionMode::LocalOperator,
                    grant: core.state.connection.as_ref().unwrap().grant.clone(),
                    snapshot_mode: SnapshotMode::OrderedConnection,
                },
                clock(2)
            )
            .is_err());
        assert_eq!(core.actions.pending_count(), pending);
        assert_eq!(core.revision(), revision);
        assert_eq!(core.scope(), Some(&scope(1)));
        core.state.revision = u64::MAX;
        assert_eq!(
            core.apply(
                ClientInput::OperationFinished {
                    scope: scope(1),
                    operation_id: id,
                    outcome: OperationOutcome::Canceled,
                },
                clock(3)
            ),
            Err(ClientError::InternalFailure {
                code: "revision_overflow".into()
            })
        );
        assert_eq!(core.actions.pending_count(), pending);
        assert_eq!(core.state.revision, u64::MAX);
    }

    #[test]
    fn answer_failure_restores_and_the_authoritative_result_retires_the_question() {
        use crew_protocol::{Message, MessageType, OPERATOR_ID};
        let mut core = core();
        let question = Message {
            id: "question".into(),
            from: "claude-1".into(),
            to: OPERATOR_ID.into(),
            kind: MessageType::Question,
            body: "Choose".into(),
            timestamp: 0,
            correlation_id: None,
            reply_to: None,
            metadata: serde_json::Value::Null,
        };
        core.apply(
            ClientInput::Push {
                scope: scope(1),
                push: Push::Message(question.clone()),
            },
            clock(1),
        )
        .unwrap();
        for succeed in [false, true] {
            let dispatched = core
                .plan(
                    ActionIntent::Answer {
                        scope: scope(1),
                        question_id: question.id.clone(),
                        body: "Proceed".into(),
                    },
                    clock(2),
                )
                .unwrap();
            let ActionPlan::Dispatch { operation_id } = dispatched.plan else {
                panic!("live question");
            };
            assert!(dispatched.update.changes.questions);
            assert_eq!(core.state.questions.visible().count(), 0);
            assert_eq!(core.state.questions.open.len(), 1);
            let outcome = if succeed {
                OperationOutcome::Succeeded {
                    result: CommandResult::Sent(Box::new(crew_protocol::rpc::SendResult {
                        message: Message {
                            id: "answer".into(),
                            from: OPERATOR_ID.into(),
                            to: question.from.clone(),
                            kind: MessageType::Answer,
                            reply_to: Some(question.id.clone()),
                            body: "Proceed".into(),
                            ..question.clone()
                        },
                        duplicate: false,
                        recipients: vec![question.from.clone()],
                    })),
                }
            } else {
                OperationOutcome::Rejected {
                    code: "closed".into(),
                    message: "not delivered".into(),
                }
            };
            let completed = core
                .apply(
                    ClientInput::OperationFinished {
                        scope: scope(1),
                        operation_id,
                        outcome,
                    },
                    clock(3),
                )
                .unwrap();
            assert!(completed.changes.questions);
            assert_eq!(completed.outcomes.len(), 1);
            assert_eq!(
                core.state.questions.visible().count(),
                usize::from(!succeed)
            );
            assert_eq!(core.state.questions.open.len(), usize::from(!succeed));
        }
    }

    #[test]
    fn unrelated_or_stale_input_has_no_revision_and_a_combined_plan_updates_once() {
        let mut core = core();
        let before = core.revision();
        let id = send(&mut core);
        assert_eq!(core.revision(), before + 1);
        let before = core.revision();
        let stale = core
            .apply(
                ClientInput::OperationFinished {
                    scope: scope(2),
                    operation_id: id,
                    outcome: OperationOutcome::Canceled,
                },
                clock(2),
            )
            .unwrap();
        assert!(stale.outcomes.is_empty());
        assert_eq!(stale.revision, before);
        assert!(!changes_visible(&stale.changes));
    }
}
