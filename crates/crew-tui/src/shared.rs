//! Terminal host adapter for the actual portable engine. The terminal keeps
//! navigation and derived render caches; domain reduction remains in ClientCore.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crew_client::*;
use crew_core::rpc::{Push, Snapshot};

use crate::app::{Command as UiCommand, Modal};
use crate::app::{MAX_EVENTS, MAX_MESSAGES};

/// Closed shared command captured with the scope and logical operation ID.
/// JSON here is the client schema; the live shared encoder supplies wire bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub scope: ClientScope,
    pub operation_id: OperationId,
    pub command: String,
}

impl Planned {
    pub fn typed(&self) -> Result<crew_client::Command, ClientError> {
        serde_json::from_str(&self.command).map_err(|error| ClientError::DecodeFailure {
            message: error.to_string(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct Shared(Rc<RefCell<Host>>);

pub(crate) struct Host {
    pub core: ClientCore,
    epoch: u64,
    bootstrap: u64,
    disconnected: bool,
    last_now: i64,
    operations: BTreeMap<OperationId, UiCommand>,
    requests: VecDeque<Planned>,
    timers: BTreeMap<TimerId, u64>,
    confirmation: Option<(ConfirmationId, UiCommand)>,
    input_epoch: u64,
    monotonic_override: Option<u64>,
    dispatch: Arc<Mutex<Option<ClientScope>>>,
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = self.0.borrow();
        f.debug_struct("Shared")
            .field("revision", &host.core.revision())
            .field("scope", &host.core.scope())
            .finish()
    }
}

impl Shared {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(Host {
            core: ClientCore::new(ClientConfig {
                limits: ClientLimits {
                    messages: MAX_MESSAGES as u32,
                    events: MAX_EVENTS as u32,
                    context_points: 600,
                    buffered_pushes: 256,
                    pending_operations: 64,
                    page_size: 200,
                },
                snapshot_mode: SnapshotMode::BufferedOverlap,
            })
            .expect("fixed terminal limits are valid"),
            epoch: 0,
            bootstrap: 1 << 63,
            disconnected: false,
            last_now: 0,
            operations: BTreeMap::new(),
            requests: VecDeque::new(),
            timers: BTreeMap::new(),
            confirmation: None,
            input_epoch: 0,
            monotonic_override: None,
            dispatch: Arc::new(Mutex::new(None)),
        })))
    }

    pub fn read<T>(&self, read: impl FnOnce(&ClientCore) -> T) -> T {
        read(&self.0.borrow().core)
    }

    pub fn dispatch_guard(&self) -> Arc<Mutex<Option<ClientScope>>> {
        self.0.borrow().dispatch.clone()
    }

    pub fn last_now(&self) -> i64 {
        self.0.borrow().last_now
    }

    pub fn set_clock(&self, sample: ClockSample) {
        self.0.borrow_mut().monotonic_override = Some(sample.monotonic_ms);
    }

    pub fn server_now(&self, now: i64) -> i64 {
        let host = self.0.borrow();
        host.core.server_now(host.clock(now))
    }

    pub fn next_timer(&self, now: i64) -> Option<u64> {
        let host = self.0.borrow();
        let sample = host.clock(now);
        host.timers
            .values()
            .min()
            .map(|at| at.saturating_sub(sample.monotonic_ms))
    }

    pub fn accept_epoch(&self, epoch: u64, new_snapshot: bool) -> bool {
        let mut host = self.0.borrow_mut();
        if epoch < host.input_epoch || (!new_snapshot && epoch != host.input_epoch) {
            return false;
        }
        if epoch > host.input_epoch {
            host.input_epoch = epoch;
            host.disconnected = true;
        }
        true
    }

    pub fn snapshot(&self, snapshot: Snapshot, now: i64) -> Result<ClientUpdate, ClientError> {
        let mut host = self.0.borrow_mut();
        let sample = host.clock(now);
        host.last_now = now;
        let mut update = host.noop();
        let workspace = if snapshot.session.workspace.is_empty() {
            "tui"
        } else {
            &snapshot.session.workspace
        };
        let changed = host.core.scope().is_none_or(|old| {
            old.workspace_id != workspace
                || old.daemon_pid != Some(snapshot.session.daemon_pid)
                || old.daemon_started_at_ms != Some(snapshot.session.daemon_started_at)
        });
        if changed || host.disconnected {
            host.epoch = host
                .epoch
                .checked_add(1)
                .ok_or_else(|| ClientError::InternalFailure {
                    code: "tui_epoch_overflow".into(),
                })?;
            let scope = ClientScope {
                endpoint_id: "tui:local".into(),
                workspace_id: workspace.into(),
                generation: None,
                daemon_pid: Some(snapshot.session.daemon_pid),
                daemon_started_at_ms: Some(snapshot.session.daemon_started_at),
                attempt: host.epoch,
            };
            let bound = host.core.apply(
                ClientInput::Bind {
                    scope: scope.clone(),
                    mode: ConnectionMode::LocalOperator,
                    grant: Grant {
                        grant_id: format!("tui:{}", scope.workspace_id),
                        tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
                        workspace_ids: vec![scope.workspace_id.clone()],
                        expires_at_ms: None,
                    },
                    snapshot_mode: SnapshotMode::BufferedOverlap,
                },
                sample,
            )?;
            host.accept(&bound)?;
            merge(&mut update, bound);
        }
        host.disconnected = false;
        let scope = host
            .core
            .scope()
            .expect("snapshot established a binding")
            .clone();
        let id = host.bootstrap;
        host.bootstrap = id
            .checked_add(1)
            .ok_or_else(|| ClientError::InternalFailure {
                code: "tui_bootstrap_overflow".into(),
            })?;
        let started = host.core.apply(
            ClientInput::SnapshotStarted {
                scope: scope.clone(),
                operation_id: id,
            },
            sample,
        )?;
        host.accept(&started)?;
        merge(&mut update, started);
        let snapshot = host.core.apply(
            ClientInput::Snapshot {
                scope,
                operation_id: id,
                snapshot: Box::new(snapshot),
            },
            sample,
        )?;
        host.accept(&snapshot)?;
        merge(&mut update, snapshot);
        Ok(update)
    }

    pub fn push(&self, push: Push, now: i64) -> Result<Option<ClientUpdate>, ClientError> {
        let mut host = self.0.borrow_mut();
        let sample = host.clock(now);
        host.last_now = now;
        let Some(scope) = host.core.scope().cloned() else {
            return Ok(None);
        };
        let update = host.core.apply(ClientInput::Push { scope, push }, sample)?;
        host.accept(&update)?;
        Ok(Some(update))
    }

    pub fn disconnect(&self, now: i64) -> Result<Option<ClientUpdate>, ClientError> {
        let mut host = self.0.borrow_mut();
        let sample = host.clock(now);
        host.last_now = now;
        host.disconnected = true;
        let Some(scope) = host.core.scope().cloned() else {
            return Ok(None);
        };
        let update = host.core.apply(
            ClientInput::Unbind {
                scope,
                reason: ClientReason {
                    code: "closed".into(),
                    message: "daemon connection closed".into(),
                },
            },
            sample,
        )?;
        host.accept(&update)?;
        Ok(Some(update))
    }

    pub fn take_requests(&self) -> Vec<Planned> {
        self.0.borrow_mut().requests.drain(..).collect()
    }

    pub fn retired(&self, update: &ClientUpdate) -> Vec<(UiCommand, OperationOutcome)> {
        let mut host = self.0.borrow_mut();
        update
            .outcomes
            .iter()
            .filter_map(|notice| {
                host.operations
                    .remove(&notice.operation_id)
                    .map(|command| (command, notice_outcome(notice)))
            })
            .collect()
    }

    pub fn wake(&self, now: i64) -> Result<Vec<ClientUpdate>, ClientError> {
        let mut host = self.0.borrow_mut();
        let sample = host.clock(now);
        let due: Vec<_> = host
            .timers
            .iter()
            .filter(|(_, deadline)| **deadline <= sample.monotonic_ms)
            .map(|(id, _)| *id)
            .collect();
        let mut updates = Vec::new();
        for timer_id in due {
            let update = host.core.apply(
                ClientInput::Wake {
                    timer_id,
                    monotonic_ms: sample.monotonic_ms,
                },
                sample,
            )?;
            host.accept(&update)?;
            updates.push(update);
        }
        Ok(updates)
    }

    pub fn plan(
        &self,
        command: UiCommand,
        now: i64,
    ) -> Result<(ActionPlan, ClientUpdate), ClientError> {
        let mut host = self.0.borrow_mut();
        let sample = host.clock(now);
        let scope = host
            .core
            .scope()
            .cloned()
            .ok_or_else(|| ClientError::InvalidInput {
                field: "scope".into(),
                reason: "no authenticated snapshot".into(),
            })?;
        let intent = intent(&command, scope)?;
        let planned = host.core.plan(intent, sample)?;
        if let ActionPlan::Dispatch { operation_id } = planned.plan {
            host.operations.insert(operation_id, command);
        }
        host.accept(&planned.update)?;
        Ok((planned.plan, planned.update))
    }

    pub fn modal(&self, modal: Option<&Modal>, now: i64) -> Result<ClientUpdate, ClientError> {
        let next = match modal {
            Some(Modal::ConfirmKill(id)) => Some(UiCommand::Kill(id.clone())),
            Some(Modal::ConfirmRestartFresh(id)) => Some(UiCommand::RestartFresh(id.clone())),
            Some(Modal::ConfirmRestartPaused(_)) => {
                Some(UiCommand::ResumeLimited { restart: true })
            }
            Some(Modal::ConfirmStop) => Some(UiCommand::StopTeam),
            _ => None,
        };
        let mut host = self.0.borrow_mut();
        let sample = host.clock(now);
        if host.confirmation.as_ref().map(|(_, command)| command) == next.as_ref() {
            return Ok(host.noop());
        }
        let mut canceled = host.noop();
        if let Some((id, _)) = host.confirmation.take() {
            canceled = host.core.cancel_confirmation(id)?;
        }
        let Some(command) = next else {
            host.accept(&canceled)?;
            return Ok(canceled);
        };
        let scope = host
            .core
            .scope()
            .cloned()
            .ok_or_else(|| ClientError::InvalidInput {
                field: "scope".into(),
                reason: "no authenticated snapshot".into(),
            })?;
        let planned = host.core.plan(intent(&command, scope)?, sample)?;
        if let ActionPlan::Confirm {
            confirmation_id, ..
        } = planned.plan
        {
            host.confirmation = Some((confirmation_id, command));
        }
        host.accept(&planned.update)?;
        Ok(planned.update)
    }

    pub fn dispatch(
        &self,
        command: UiCommand,
        now: i64,
    ) -> Result<(ActionPlan, ClientUpdate), ClientError> {
        let confirmation = self.0.borrow().confirmation.clone();
        if let Some((id, expected)) = confirmation {
            if expected == command {
                let mut host = self.0.borrow_mut();
                let sample = host.clock(now);
                host.confirmation = None;
                let update = host.core.confirm(id, sample)?;
                let operation_id = update.effects.iter().find_map(|effect| match effect {
                    ClientEffect::Request { operation_id, .. } => Some(*operation_id),
                    _ => None,
                });
                if let Some(operation_id) = operation_id {
                    host.operations.insert(operation_id, command);
                }
                host.accept(&update)?;
                return Ok((
                    operation_id.map_or_else(
                        || ActionPlan::Unavailable {
                            reason: ClientReason {
                                code: "stale_confirmation".into(),
                                message: "confirmation no longer applies".into(),
                            },
                        },
                        |operation_id| ActionPlan::Dispatch { operation_id },
                    ),
                    update,
                ));
            }
        }
        self.plan(command, now)
    }

    pub fn finished(
        &self,
        scope: ClientScope,
        id: OperationId,
        outcome: OperationOutcome,
        now: i64,
    ) -> Result<(ClientUpdate, Vec<(UiCommand, OperationOutcome)>), ClientError> {
        let mut host = self.0.borrow_mut();
        let sample = host.clock(now);
        let update = host.core.apply(
            ClientInput::OperationFinished {
                scope,
                operation_id: id,
                outcome: outcome.clone(),
            },
            sample,
        )?;
        host.accept(&update)?;
        let mut completed = Vec::new();
        for notice in &update.outcomes {
            if let Some(command) = host.operations.remove(&notice.operation_id) {
                completed.push((
                    command,
                    if notice.operation_id == id {
                        outcome.clone()
                    } else {
                        notice_outcome(notice)
                    },
                ));
            }
        }
        Ok((update, completed))
    }
}

impl Host {
    fn clock(&self, now: i64) -> ClockSample {
        ClockSample {
            wall_ms: now,
            monotonic_ms: self.monotonic_override.unwrap_or(now.max(0) as u64),
        }
    }
    fn noop(&self) -> ClientUpdate {
        ClientUpdate {
            revision: self.core.revision(),
            changes: ChangeSet {
                revision: self.core.revision(),
                ..ChangeSet::default()
            },
            effects: Vec::new(),
            outcomes: Vec::new(),
        }
    }

    fn accept(&mut self, update: &ClientUpdate) -> Result<(), ClientError> {
        *self.dispatch.lock().expect("dispatch scope lock") =
            if self.core.workspace_status() == WorkspaceStatus::Live {
                self.core.scope().cloned()
            } else {
                None
            };
        for effect in &update.effects {
            match effect {
                ClientEffect::Request {
                    scope,
                    operation_id,
                    command,
                    ..
                } => {
                    self.requests.push_back(Planned {
                        scope: scope.clone(),
                        operation_id: *operation_id,
                        command: serde_json::to_string(command).map_err(|error| {
                            ClientError::DecodeFailure {
                                message: error.to_string(),
                            }
                        })?,
                    });
                }
                ClientEffect::Schedule {
                    timer_id,
                    deadline_ms,
                } => {
                    self.timers.insert(*timer_id, *deadline_ms);
                }
                ClientEffect::CancelTimer { timer_id } => {
                    self.timers.remove(timer_id);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn intent(command: &UiCommand, scope: ClientScope) -> Result<ActionIntent, ClientError> {
    let read = |entity, kind, options| ActionIntent::Read {
        scope: scope.clone(),
        resource: ResourceRequest {
            key: ResourceKey {
                entity: EntityKey {
                    scope: scope.clone(),
                    entity,
                },
                kind,
                options,
            },
        },
    };
    Ok(match command {
        UiCommand::FetchOutput { id } => read(
            Entity::Agent(id.clone()),
            ResourceKind::Output,
            ReadOptions::Output {
                lines: 300,
                screen: true,
                ansi: true,
            },
        ),
        UiCommand::FetchTranscript { id } => read(
            Entity::Agent(id.clone()),
            ResourceKind::Transcript,
            ReadOptions::Bounded { limit: 400 },
        ),
        UiCommand::FetchInbox { id } => read(
            Entity::Agent(id.clone()),
            ResourceKind::Inbox,
            ReadOptions::Bounded { limit: 100 },
        ),
        UiCommand::FetchEvents { id } => read(
            Entity::Agent(id.clone()),
            ResourceKind::Events,
            ReadOptions::Bounded { limit: 200 },
        ),
        UiCommand::FetchDiff { id } => read(
            Entity::Agent(id.clone()),
            ResourceKind::Diff,
            ReadOptions::Diff { against: None },
        ),
        UiCommand::FetchModels => read(
            Entity::Workspace,
            ResourceKind::Models,
            ReadOptions::Default,
        ),
        UiCommand::Kill(id) => ActionIntent::Agent {
            scope,
            id: id.clone(),
            action: AgentAction::Kill,
        },
        UiCommand::Restart(id) => ActionIntent::Agent {
            scope,
            id: id.clone(),
            action: AgentAction::Restart,
        },
        UiCommand::RestartFresh(id) => ActionIntent::Agent {
            scope,
            id: id.clone(),
            action: AgentAction::RestartFresh,
        },
        UiCommand::Pause(id) => ActionIntent::Agent {
            scope,
            id: id.clone(),
            action: AgentAction::Pause,
        },
        UiCommand::Resume(id) => ActionIntent::Agent {
            scope,
            id: id.clone(),
            action: AgentAction::Resume,
        },
        UiCommand::Focus(id) => ActionIntent::Agent {
            scope,
            id: id.clone(),
            action: AgentAction::Focus,
        },
        UiCommand::Compact(id) => ActionIntent::Agent {
            scope,
            id: id.clone(),
            action: AgentAction::Compact,
        },
        UiCommand::ResumeLimited { restart } => ActionIntent::ResumeLimited {
            scope,
            restart: *restart,
        },
        UiCommand::StopTeam => ActionIntent::StopTeam { scope },
        UiCommand::Answer { id, body } => ActionIntent::Answer {
            scope,
            question_id: id.clone(),
            body: body.clone(),
        },
        UiCommand::Send { to, body } => ActionIntent::SendMessage {
            scope,
            to: to.clone(),
            body: body.clone(),
            message_id: crew_core::util::new_id("m"),
        },
        UiCommand::SetModel {
            id, model, runtime, ..
        } => ActionIntent::SetModel {
            scope,
            id: id.clone(),
            model: model.clone(),
            runtime: Some(*runtime),
            note: None,
        },
        UiCommand::Type { id, text } => ActionIntent::AgentInput {
            scope,
            id: id.clone(),
            text: Some(if text.is_empty() {
                "\r".into()
            } else {
                text.clone()
            }),
            submit: !text.is_empty(),
        },
        UiCommand::Keys { id, text } => ActionIntent::AgentInput {
            scope,
            id: id.clone(),
            text: Some(text.clone()),
            submit: false,
        },
        _ => {
            return Err(ClientError::InvalidInput {
                field: "command".into(),
                reason: "host command has no daemon action".into(),
            })
        }
    })
}

fn notice_outcome(notice: &OperationNotice) -> OperationOutcome {
    match notice.kind {
        OperationOutcomeKind::Canceled => OperationOutcome::Canceled,
        _ => OperationOutcome::OutcomeUnknown {
            reason: notice.reason.clone().unwrap_or_else(|| ClientReason {
                code: "closed".into(),
                message: "operation retired".into(),
            }),
        },
    }
}

fn merge(target: &mut ClientUpdate, next: ClientUpdate) {
    target.revision = next.revision;
    target.changes.revision = next.revision;
    target.changes.agents.extend(next.changes.agents);
    target.changes.tasks.extend(next.changes.tasks);
    target.changes.removed.extend(next.changes.removed);
    target.changes.session |= next.changes.session;
    target.changes.questions |= next.changes.questions;
    target.changes.messages |= next.changes.messages;
    target.changes.events |= next.changes.events;
    target.changes.usage |= next.changes.usage;
    target.changes.availability |= next.changes.availability;
    target.changes.pending |= next.changes.pending;
    target.changes.reset |= next.changes.reset;
    target.effects.extend(next.effects);
    target.outcomes.extend(next.outcomes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::App, fixture};

    fn loaded() -> App {
        let mut app = App::new();
        app.apply_snapshot(fixture::snapshot(1_000_000), 1_000_000);
        app
    }

    #[test]
    fn reviewed_state_vectors_use_the_real_terminal_adapter_and_keep_native_work_semantics() {
        use crew_core::rpc::ServerFrame;
        use serde_json::{json, Value};
        for text in [
            include_str!("../../../fixtures/native/state/work-choice.case.json"),
            include_str!("../../../fixtures/native/state/parked-resumption.case.json"),
            include_str!("../../../fixtures/native/state/completed-ties.case.json"),
            include_str!("../../../fixtures/native/state/optional-values.case.json"),
            include_str!("../../../fixtures/native/state/snapshot-replacement.case.json"),
        ] {
            let case: Value = serde_json::from_str(text).unwrap();
            let mut app = App::new();
            for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let at = step["at_ms"].as_i64().unwrap();
                if let Some(snapshot) = step.get("snapshot") {
                    app.apply_snapshot(serde_json::from_value(snapshot.clone()).unwrap(), at);
                } else {
                    let frame: ServerFrame = serde_json::from_value(step["frame"].clone()).unwrap();
                    app.apply_push(frame.push.unwrap(), at);
                }
                let mut agent_ids: Vec<_> = app.agents.iter().map(|agent| &agent.id).collect();
                agent_ids.sort();
                let agents: BTreeMap<_, _> =
                    app.agents.iter().map(|agent| (&agent.id, agent)).collect();
                let tasks: BTreeMap<_, _> = app.tasks.iter().map(|task| (&task.id, task)).collect();
                let work: BTreeMap<_, _> = app
                    .agents
                    .iter()
                    .map(|agent| {
                        (
                            &agent.id,
                            crew_client::current_work(&app.tasks, &agent.id, app.server_now(at)),
                        )
                    })
                    .collect();
                let actual = json!({"agent_ids":agent_ids,"agents":agents,"task_ids":app.tasks.iter().map(|task|&task.id).collect::<Vec<_>>(),"tasks":tasks,
                    "messages":app.messages,"message_ids":app.messages.iter().map(|message|&message.id).collect::<Vec<_>>(),
                    "events":app.events,"event_seqs":app.events.iter().map(|event|event.seq).collect::<Vec<_>>(),
                    "question_ids":app.open_questions().iter().map(|question|&question.id).collect::<Vec<_>>(),"work":work,"clock_offset_ms":app.clock_offset});
                let mut expected = step["expect"].clone();
                expected
                    .as_object_mut()
                    .unwrap()
                    .remove("selection_validity");
                subset(&actual, &expected, &format!("{}:{index}", case["id"]));
            }
        }
    }

    fn subset(actual: &serde_json::Value, expected: &serde_json::Value, path: &str) {
        use serde_json::Value;
        match expected {
            Value::Object(fields) => {
                for (key, expected) in fields {
                    subset(
                        actual
                            .get(key)
                            .unwrap_or_else(|| panic!("{path}.{key}:missing")),
                        expected,
                        &format!("{path}.{key}"),
                    );
                }
            }
            Value::Array(items) => {
                let values = actual.as_array().unwrap();
                assert_eq!(values.len(), items.len(), "{path}");
                for (index, (actual, expected)) in values.iter().zip(items).enumerate() {
                    subset(actual, expected, &format!("{path}[{index}]"));
                }
            }
            _ => assert_eq!(actual, expected, "{path}"),
        }
    }

    #[test]
    fn four_confirmations_capture_scope_and_agent_facts_before_the_dialog_answer() {
        let mut app = loaded();
        app.modal = Some(Modal::ConfirmKill("claude-1".into()));
        app.capture_confirmation(1_000_000);
        let mut agent = app.shared.read(|core| {
            core.agents()
                .iter()
                .find(|a| a.id == "claude-1")
                .unwrap()
                .clone()
        });
        agent.pid = Some(9999);
        app.apply_push(Push::Agent(Box::new(agent)), 1_000_001);
        assert!(app
            .prepare(UiCommand::Kill("claude-1".into()), 1_000_002)
            .is_empty());
        assert!(app.status.as_ref().unwrap().is_error);
        app.modal = Some(Modal::ConfirmStop);
        app.capture_confirmation(1_000_003);
        app.disconnected_at(1_000_004);
        assert!(app.prepare(UiCommand::StopTeam, 1_000_005).is_empty());
        assert!(!app
            .shared
            .read(|core| core.workspace_status() == WorkspaceStatus::Live));
    }

    #[test]
    fn raw_bursts_and_prompts_coexist_in_both_orders_without_replaying_unknown_input() {
        for raw_first in [false, true] {
            let mut app = loaded();
            let raw = UiCommand::Keys {
                id: "claude-1".into(),
                text: "日本".into(),
            };
            let prompt = UiCommand::Type {
                id: "claude-1".into(),
                text: "inspect".into(),
            };
            let (first, second) = if raw_first {
                (raw, prompt)
            } else {
                (prompt, raw)
            };
            let first = app.prepare(first, 1_000_001);
            assert_eq!(first.len(), 1);
            let second = app.prepare(second, 1_000_002);
            assert_eq!(second.len(), 1);
            assert_ne!(first[0].operation_id, second[0].operation_id);
            let repeated = app.prepare(
                UiCommand::Type {
                    id: "claude-1".into(),
                    text: "second prompt".into(),
                },
                1_000_003,
            );
            assert!(repeated.is_empty());
            for planned in first.into_iter().chain(second) {
                app.apply_reply(
                    crate::app::Reply::Shared {
                        scope: planned.scope,
                        operation_id: planned.operation_id,
                        outcome: OperationOutcome::OutcomeUnknown {
                            reason: ClientReason {
                                code: "closed".into(),
                                message: "closed".into(),
                            },
                        },
                    },
                    1_000_004,
                );
            }
            app.disconnected_at(1_000_005);
            app.apply_snapshot(fixture::snapshot(1_000_000), 1_000_006);
            assert!(
                app.shared.take_requests().is_empty(),
                "written input has no automatic replay"
            );
        }
    }

    #[test]
    fn stale_input_epochs_and_old_replies_cannot_change_the_reconnected_projection() {
        let mut app = loaded();
        assert!(app.shared.accept_epoch(1, true));
        let planned = app
            .prepare(
                UiCommand::FetchOutput {
                    id: "claude-1".into(),
                },
                1_000_001,
            )
            .remove(0);
        app.disconnected_at(1_000_002);
        assert!(app.shared.accept_epoch(2, true));
        let mut snapshot = fixture::snapshot(1_000_000);
        snapshot.session.daemon_pid = 9000;
        snapshot.session.daemon_started_at = 1_000_000;
        snapshot.agents[0].activity = "new incarnation".into();
        app.apply_snapshot(snapshot, 1_000_003);
        assert!(!app.shared.accept_epoch(1, false));
        assert!(!app.shared.accept_epoch(1, true));
        let before = app.shared.read(ClientCore::revision);
        app.apply_reply(
            crate::app::Reply::Shared {
                scope: planned.scope,
                operation_id: planned.operation_id,
                outcome: OperationOutcome::Succeeded {
                    result: CommandResult::Output(crew_core::rpc::OutputResult {
                        text: "old process".into(),
                        alive: true,
                    }),
                },
            },
            1_000_004,
        );
        assert_eq!(app.shared.read(ClientCore::revision), before);
        assert!(app.shared.read(|core| core
            .agents()
            .iter()
            .any(|agent| agent.activity == "new incarnation")));
    }

    #[test]
    fn a_held_send_has_one_final_notice_and_expires_on_monotonic_time() {
        let mut app = loaded();
        app.shared.set_clock(ClockSample {
            wall_ms: 1_000_000,
            monotonic_ms: 10,
        });
        let planned = app
            .prepare(
                UiCommand::Send {
                    to: "claude-1".into(),
                    body: "hello".into(),
                },
                1_000_000,
            )
            .remove(0);
        app.shared.set_clock(ClockSample {
            wall_ms: 1_000_001,
            monotonic_ms: 11,
        });
        app.apply_reply(
            crate::app::Reply::Shared {
                scope: planned.scope,
                operation_id: planned.operation_id,
                outcome: OperationOutcome::OutcomeUnknown {
                    reason: ClientReason {
                        code: "closed".into(),
                        message: "closed".into(),
                    },
                },
            },
            1_000_001,
        );
        assert!(
            !app.status
                .as_ref()
                .is_some_and(|status| status.text.contains("closed")),
            "held send has no final notice yet"
        );
        app.disconnected_at(1_000_002);
        app.shared.set_clock(ClockSample {
            wall_ms: -10,
            monotonic_ms: 30_011,
        });
        app.tick(-10);
        assert!(app.status.as_ref().unwrap().text.contains("closed"));
        app.status = None;
        app.tick(-10);
        assert!(app.status.is_none(), "expiry emits only once");
    }
}
