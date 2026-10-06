//! Terminal host adapter for the actual portable engine. The terminal keeps
//! navigation and derived render caches; domain reduction remains in ClientCore.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::rc::Rc;

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
        })))
    }

    pub fn read<T>(&self, read: impl FnOnce(&ClientCore) -> T) -> T {
        read(&self.0.borrow().core)
    }

    pub fn last_now(&self) -> i64 {
        self.0.borrow().last_now
    }

    pub fn set_clock(&self, sample: ClockSample) {
        self.0.borrow_mut().monotonic_override = Some(sample.monotonic_ms);
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
