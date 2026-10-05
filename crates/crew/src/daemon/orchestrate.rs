//! Task orchestration: applying the scheduler's plan, the task lifecycle
//! (dispatch, progress, completion, failure, retry) and reviews.

use crew_core::config::TaskSpec;
use crew_core::rpc::{RpcError, SendResult, TaskCard};
use crew_core::util::{is_valid_id, now_ms, one_line, truncate_chars};
use crew_core::{AgentStatus, Artifact, Message, MessageType, Task, TaskStatus, MAX_MESSAGE_BYTES};
use serde_json::json;

use super::server::Who;
use super::Daemon;
use crate::agents::prompt;
use crate::message_bus::NewMessage;
use crate::persistence::NewEvent;
use crate::scheduler::{self, Action, AgentView};
use crate::tasks::{self, active_task};

const MAX_RESULT_CHARS: usize = 4000;
/// How long nothing must be able to progress before it is called a stall.
const STALL_GRACE_MS: i64 = 12_000;
const MAX_ARTIFACTS: usize = 32;
/// How much of a task's title a message about its review quotes: such a
/// message points to the card, so its length must not depend on the card.
const QUOTED_TITLE_CHARS: usize = 80;

/// A task its agent cannot be told about would never start, and one whose
/// reviewer cannot be asked would never be done: refuse one whose task
/// message, or whose review request, would exceed the bus limit.
pub(super) fn check_task_message_size(task: &Task, limit: usize) -> Result<(), RpcError> {
    let request = task.reviewer.as_ref().map(|_| {
        review_request(task, false)
            .len()
            .max(review_request(task, true).len())
    });
    let messages = [
        ("task message", Some(prompt::task_dispatch(task).len())),
        ("review request", request),
    ];
    for (what, size) in messages {
        if let Some(size) = size.filter(|size| *size > limit) {
            return Err(RpcError::invalid(format!(
                "task '{}' is too long: its {what} would be {size} bytes, and \
limits.max_message_bytes is {limit}",
                task.id
            )));
        }
    }
    Ok(())
}

/// The body of the review request for `task`: a pointer to its card, where
/// what to review against, and the work, are. A long title is quoted in
/// part. In a shared workspace (`shared`) nothing is committed: the work is
/// already where the reviewer sees it.
fn review_request(task: &Task, shared: bool) -> String {
    let id = &task.id;
    let work = if shared {
        "result. The work is in the shared workspace"
    } else {
        "result and commits. git_cherry_pick or git_merge brings the work into your worktree"
    };
    format!(
        "Review my work on task {id} (\"{}\"): task_get(\"{id}\") reads its card, with my \
{work}; then review_submit(task_id=\"{id}\", approve=true|false, comments=…).",
        truncate_chars(&task.title, QUOTED_TITLE_CHARS)
    )
}

impl Daemon {
    /// The task an agent is created with (`agent_add` with a task, or a
    /// subagent's), checked before the agent exists: a task that cannot be
    /// created must not leave an agent behind.
    pub(crate) fn own_task(&self, agent: &str, description: String) -> Result<TaskSpec, RpcError> {
        let spec = TaskSpec {
            id: format!("{agent}-task"),
            description,
            agent: Some(agent.to_string()),
            ..TaskSpec::default()
        };
        // Ids are at most 32 characters, and "-task" takes five.
        if !is_valid_id(&spec.id) {
            return Err(RpcError::invalid(format!(
                "agent id '{agent}' is too long for an agent with a task: at most 27 characters \
(its task is '{}')",
                spec.id
            )));
        }
        tasks::validate_new(&self.model().tasks, &spec).map_err(RpcError::invalid)?;
        check_task_message_size(&tasks::task_from_spec(&spec, 0), self.bus.max_bytes())?;
        Ok(spec)
    }

    /// Run the scheduler and carry out what it decides. Called whenever task
    /// or agent state changes.
    pub(crate) async fn apply_plan(&self) {
        let _planning = self.plan_lock.lock().await;
        let procs = self.procs().ok();
        let homeward = self.homeward();
        self.resume_parked_tasks(procs.as_deref(), &homeward).await;
        let (tasks, agents) = {
            let model = self.model();
            if model.session.team.is_none() {
                return;
            }
            let agents: Vec<AgentView> = model
                .agents
                .iter()
                .map(|a| AgentView {
                    id: a.id.clone(),
                    orchestrator: a.orchestrator,
                    // Its process must be running or starting: a retired
                    // subagent, or a member not restarted after a daemon
                    // restart, would never receive a task.
                    runnable: !a.paused
                        && !a.status.is_terminal()
                        && (a.status == AgentStatus::Spawning
                            || procs.as_ref().is_some_and(|p| p.is_alive(&a.id))),
                    // An agent parked at a usage limit is not stuck: it
                    // goes on when the limit lifts.
                    active: !a.orchestrator
                        && (matches!(a.status, AgentStatus::Working | AgentStatus::Spawning)
                            || a.limit.is_some()),
                    homeward: homeward.contains(&a.id),
                    turn_active: model
                        .rt
                        .get(&a.id)
                        .is_some_and(|rt| rt.turn_active || rt.pending_prompt.is_some())
                        || a.limit.is_some()
                        || a.status == AgentStatus::Blocked,
                })
                .collect();
            (model.tasks.clone(), agents)
        };
        // A message still on its way to an agent also counts as activity.
        let agents: Vec<AgentView> = agents
            .into_iter()
            .map(|mut a| {
                if a.runnable && !a.active && !a.orchestrator {
                    a.active = !self.bus.undelivered(&a.id).unwrap_or_default().is_empty();
                }
                a
            })
            .collect();
        let actions = scheduler::plan(&tasks, &agents);
        let mut stalled = false;
        // Each action builds on the ones before it, so a write the database
        // refuses ends this round: the next one (an event, or the 5 s tick)
        // plans again from what is stored.
        let mut refused = None;
        for action in actions {
            match action {
                Action::MakeReady { task } => {
                    if let Err(e) = self.try_update_task(&task, |t| {
                        t.status = TaskStatus::Ready;
                        t.note = None;
                    }) {
                        refused = Some(e);
                        break;
                    }
                    self.emit(
                        NewEvent::new("scheduler", "task.ready", format!("task {task} is ready"))
                            .task(&task),
                    );
                }
                Action::Requeue { task } => {
                    if let Err(e) = self.try_update_task(&task, |t| {
                        t.status = TaskStatus::Queued;
                        t.note = None;
                    }) {
                        refused = Some(e);
                        break;
                    }
                }
                Action::Assign { task, agent } => {
                    if let Err(e) =
                        self.try_update_task(&task, |t| t.assigned_agent = Some(agent.clone()))
                    {
                        refused = Some(e);
                        break;
                    }
                    self.emit(
                        NewEvent::new(
                            "scheduler",
                            "task.assigned",
                            format!("task {task} assigned to {agent}"),
                        )
                        .task(&task)
                        .agent(&agent),
                    );
                }
                Action::Dispatch { task, agent } => self.dispatch_task(&task, &agent).await,
                Action::Block { task, reason } => {
                    if let Err(e) = self.try_update_task(&task, |t| {
                        t.status = TaskStatus::Blocked;
                        t.note = Some(reason.clone());
                    }) {
                        refused = Some(e);
                        break;
                    }
                    self.emit(
                        NewEvent::new(
                            "scheduler",
                            "task.blocked",
                            format!("task {task} is blocked: {reason}"),
                        )
                        .task(&task)
                        .warn(),
                    );
                    self.notify_orchestrator(&format!("Task {task} is blocked: {reason}."))
                        .await;
                }
                Action::Stall { tasks, reason } => {
                    stalled = true;
                    // Teams pass through momentary lulls (an agent between
                    // two turns), so a stall counts only once it has lasted.
                    let fingerprint = format!("{}|{reason}", tasks.join(","));
                    let now = now_ms();
                    let report = {
                        let mut model = self.model();
                        match &mut model.stall {
                            Some((seen, since, reported)) if *seen == fingerprint => {
                                let due = !*reported && now - *since >= STALL_GRACE_MS;
                                if due {
                                    *reported = true;
                                }
                                due
                            }
                            _ => {
                                model.stall = Some((fingerprint, now, false));
                                false
                            }
                        }
                    };
                    if report {
                        self.emit(
                            NewEvent::new(
                                "scheduler",
                                "scheduler.stall",
                                format!("no task can make progress: {reason}"),
                            )
                            .warn()
                            .data(json!({ "tasks": tasks })),
                        );
                        self.notify_orchestrator(&format!(
                            "The team is stalled — no task can make progress: {reason}. \
Inspect with tasks_list / agents_list and unblock it."
                        ))
                        .await;
                    }
                }
                Action::AllDone => {
                    let (first_time, count) = {
                        let mut model = self.model();
                        let first = model.session.completed_at.is_none();
                        if first {
                            model.session.completed_at = Some(now_ms());
                        }
                        (first, model.tasks.len())
                    };
                    if first_time {
                        self.save_session();
                        self.emit(NewEvent::new(
                            "scheduler",
                            "session.complete",
                            format!("all {count} tasks are done"),
                        ));
                        self.notify_orchestrator(&format!(
                            "All {count} tasks are done. Review the results with tasks_list and \
call session_complete with the final report."
                        ))
                        .await;
                    }
                }
            }
        }
        if let Some(e) = refused {
            return self.plan_write_refused(e).await;
        }
        self.model().plan_refused = None;
        if !stalled {
            self.model().stall = None;
        }
    }

    /// A write the plan needed was refused. Said once (in the log and to the
    /// orchestrator): every round meets the same refusal until the database
    /// takes writes again.
    async fn plan_write_refused(&self, e: RpcError) {
        let first = {
            let mut model = self.model();
            let first = model.plan_refused.as_deref() != Some(e.message.as_str());
            model.plan_refused = Some(e.message.clone());
            first
        };
        if !first {
            tracing::debug!("{}; the plan is applied again later", e.message);
            return;
        }
        self.emit(
            NewEvent::new(
                "scheduler",
                "scheduler.write_failed",
                format!("the plan stopped: {}", e.message),
            )
            .error(),
        );
        self.notify_orchestrator(&format!(
            "The scheduler could not store a change ({}). It tries again every few seconds; \
until the database takes writes again, tasks wait where they are.",
            e.message
        ))
        .await;
    }

    /// Hand a ready task to its agent with a `task` message.
    async fn dispatch_task(&self, task_id: &str, agent_id: &str) {
        let now = now_ms();
        // Check-and-set: only a task that is still ready, and is this agent's,
        // is handed out, so a task is never dispatched twice, nor without its
        // owner (an assignment the database refused).
        let mut claimed = false;
        let Ok(Some(task)) = self.try_update_task_checked(task_id, |model, t| {
            if t.status == TaskStatus::Ready && t.assigned_agent.as_deref() == Some(agent_id) {
                if tasks::executing_task(&model.tasks, agent_id).is_some()
                    || model.rt.get(agent_id).is_some_and(|rt| rt.turn_active)
                    || !t.dependencies.iter().all(|id| {
                        model
                            .task(id)
                            .is_some_and(|dep| dep.status == TaskStatus::Done)
                    })
                {
                    return Ok(());
                }
                t.status = TaskStatus::Working;
                t.started_at = Some(now);
                t.progress = Some(0);
                t.note = None;
                claimed = true;
            }
            Ok(())
        }) else {
            return;
        };
        if !claimed {
            return;
        }
        let body = prompt::task_dispatch(&task);
        self.project_task(agent_id);
        self.model().rt(agent_id).self_status = None;
        self.emit(
            NewEvent::new(
                "scheduler",
                "task.dispatched",
                format!("task {task_id} dispatched to {agent_id}"),
            )
            .task(task_id)
            .agent(agent_id),
        );
        let message = NewMessage::new(
            self.dispatcher_for(agent_id),
            agent_id,
            MessageType::Task,
            body,
        )
        .metadata(json!({ "task_id": task_id }));
        if let Err(e) = self.send_message(message).await {
            self.emit(
                NewEvent::new(
                    "scheduler",
                    "task.dispatch_failed",
                    format!("could not dispatch {task_id} to {agent_id}: {}", e.message),
                )
                .task(task_id)
                .error(),
            );
            // The agent was never told, so the task cannot be left Working.
            // Back to Ready would only fail the same way on the next plan;
            // failing it hands the decision to the orchestrator.
            self.fail_task(
                task_id,
                format!("could not be dispatched to {agent_id}: {}", e.message),
            )
            .await;
        }
    }

    // ---- the board -------------------------------------------------------

    /// A task's card, by id or (for an agent) its task in progress. Every
    /// card is open to the whole team: the board is how work is referred to.
    pub(crate) fn task_card(&self, who: &Who, id: Option<String>) -> Result<TaskCard, RpcError> {
        let model = self.model();
        let task = match (&id, who) {
            (Some(id), _) => model
                .task(id)
                .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?,
            (None, Who::Agent(agent)) => active_task(&model.tasks, agent).ok_or_else(|| {
                RpcError::not_found("you have no task in progress; name a task by its id")
            })?,
            (None, Who::Operator) => return Err(RpcError::invalid("specify the task id")),
        };
        let dependencies = task
            .dependencies
            .iter()
            .filter_map(|d| model.task(d))
            .cloned()
            .collect();
        Ok(TaskCard {
            task: task.clone(),
            dependencies,
        })
    }

    /// Revise a card. Whoever is at work on it (or reviewing it) is pointed
    /// back to it, rather than sent the new text: by the scheduler, as news,
    /// so its reply is not carried back to whoever revised it.
    pub(crate) async fn edit_task(
        &self,
        who: &Who,
        id: &str,
        title: Option<String>,
        description: Option<String>,
    ) -> Result<Task, RpcError> {
        let title = title
            .map(|t| truncate_chars(&one_line(t.trim()), 200))
            .filter(|t| !t.is_empty());
        let description = description
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty());
        if title.is_none() && description.is_none() {
            return Err(RpcError::invalid(
                "give the card a new title or description",
            ));
        }
        if description
            .as_ref()
            .is_some_and(|d| d.len() > MAX_MESSAGE_BYTES)
        {
            return Err(RpcError::invalid(format!(
                "a description is limited to {MAX_MESSAGE_BYTES} bytes"
            )));
        }
        let before = self
            .model()
            .task(id)
            .cloned()
            .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
        if before.status == TaskStatus::Done {
            return Err(RpcError::conflict(format!(
                "task {id} is done; add a task for further work"
            )));
        }
        let revise = |t: &mut Task| {
            if let Some(title) = &title {
                t.title = title.clone();
            }
            if let Some(description) = &description {
                t.description = description.clone();
            }
        };
        // As at creation: a card whose task message would not fit could
        // never be handed out.
        let mut revised = before.clone();
        revise(&mut revised);
        check_task_message_size(&revised, self.bus.max_bytes())?;
        let task = self
            .try_update_task(id, revise)?
            .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
        let changed: Vec<&str> = [
            (task.title != before.title).then_some("title"),
            (task.description != before.description).then_some("description"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if changed.is_empty() {
            return Ok(task);
        }
        let changed = changed.join(" and ");
        self.emit(
            NewEvent::new(
                "scheduler",
                "task.edited",
                format!("task {id}: {changed} revised by {}", who.sender()),
            )
            .task(id),
        );
        if task.title != before.title {
            if let Some(agent) = &task.assigned_agent {
                self.project_task(agent);
            }
        }
        let reader = match task.status {
            TaskStatus::Working | TaskStatus::Waiting | TaskStatus::Blocked => {
                task.assigned_agent.clone()
            }
            TaskStatus::Review => task.reviewer.clone(),
            _ => None,
        };
        if let Some(reader) = reader {
            let body = format!(
                "Card {id} revised by {} ({changed}): task_get(\"{id}\") reads it.",
                who.sender()
            );
            let message = NewMessage::new("scheduler", &reader, MessageType::Status, body)
                .metadata(json!({ "task_id": id }));
            if let Err(e) = self.send_message(message).await {
                tracing::warn!("telling {reader} that card {id} changed: {}", e.message);
            }
        }
        Ok(task)
    }

    /// A message by id, for whoever sent or received it (the operator: any).
    /// To anyone else it does not exist.
    pub(crate) fn message_by_id(&self, who: &Who, id: &str) -> Result<Message, RpcError> {
        let found = self.bus.get(id)?.filter(|(message, recipients)| match who {
            Who::Agent(me) => message.from == *me || recipients.iter().any(|r| r == me),
            Who::Operator => true,
        });
        found.map(|(message, _)| message).ok_or_else(|| {
            RpcError::not_found(format!("no message '{id}' sent or received by you"))
        })
    }

    // ---- task graph edits ----------------------------------------------

    pub(crate) fn create_task(&self, spec: TaskSpec) -> Result<Task, RpcError> {
        let task = {
            let mut model = self.model();
            tasks::validate_new(&model.tasks, &spec).map_err(RpcError::invalid)?;
            for who in [&spec.agent, &spec.reviewer].into_iter().flatten() {
                if model.agent(who).is_none() {
                    return Err(RpcError::not_found(format!("no agent '{who}'")));
                }
            }
            let task = tasks::task_from_spec(&spec, now_ms());
            check_task_message_size(&task, self.bus.max_bytes())?;
            // Stored first: a task the database refused does not exist.
            self.store
                .save_task(&task)
                .map_err(|e| RpcError::internal(e.to_string()))?;
            model.tasks.push(task.clone());
            // New work reopens a session that had finished.
            model.session.completed_at = None;
            task
        };
        self.push(crew_core::rpc::Push::Task(Box::new(task.clone())));
        self.emit(
            NewEvent::new(
                "scheduler",
                "task.created",
                format!("task {} created: {}", task.id, task.title),
            )
            .task(&task.id),
        );
        self.save_session();
        self.replan();
        Ok(task)
    }

    pub(crate) fn assign_task(&self, id: &str, agent: &str) -> Result<Task, RpcError> {
        {
            let model = self.model();
            let task = model
                .task(id)
                .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
            if model.agent(agent).is_none() {
                return Err(RpcError::not_found(format!("no agent '{agent}'")));
            }
            if task.status.is_terminal() {
                return Err(RpcError::conflict(format!(
                    "task {id} is already {}",
                    task.status
                )));
            }
            // Nobody reviews their own work (see tasks::check_reviewer).
            if task.reviewer.as_deref() == Some(agent) {
                return Err(RpcError::invalid(format!(
                    "task '{id}' cannot be reviewed by its own agent '{agent}'; it is the reviewer"
                )));
            }
            if matches!(
                task.status,
                TaskStatus::Working | TaskStatus::Waiting | TaskStatus::Review
            ) && task.assigned_agent.as_deref() != Some(agent)
            {
                let holder = task.assigned_agent.as_deref().unwrap_or("?");
                let holder_alive = model.agent(holder).is_some_and(|a| !a.status.is_terminal());
                if holder_alive {
                    return Err(RpcError::conflict(format!(
                        "task {id} is in progress with {holder}; stop that agent or wait"
                    )));
                }
            }
        }
        let previous = self.model().task(id).and_then(|t| t.assigned_agent.clone());
        let task = self
            .try_update_task(id, |t| {
                if t.assigned_agent.as_deref() != Some(agent) {
                    t.assigned_agent = Some(agent.to_string());
                    if !matches!(t.status, TaskStatus::Queued | TaskStatus::Ready) {
                        // Moving a started task: the new owner starts over.
                        t.status = TaskStatus::Queued;
                        t.started_at = None;
                        t.note = None;
                    }
                }
            })?
            .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
        if let Some(previous) = previous.filter(|p| p != agent) {
            self.clear_task_wakeups(&previous, id);
            self.project_task(&previous);
        }
        self.emit(
            NewEvent::new(
                "scheduler",
                "task.assigned",
                format!("task {id} assigned to {agent}"),
            )
            .task(id)
            .agent(agent),
        );
        self.replan();
        Ok(task)
    }

    pub(crate) fn retry_task(&self, id: &str) -> Result<Task, RpcError> {
        // A member who has left the team since cannot take it up again.
        let (status, author_left) = {
            let model = self.model();
            let task = model
                .task(id)
                .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
            let left =
                |who: &Option<String>| who.as_deref().is_some_and(|w| model.agent(w).is_none());
            (task.status, left(&task.assigned_agent))
        };
        if !matches!(status, TaskStatus::Failed | TaskStatus::Blocked) {
            return Err(RpcError::conflict(format!(
                "task {id} is {status}; only failed or blocked tasks can be retried"
            )));
        }
        let task = self
            .try_update_task(id, |t| {
                t.status = TaskStatus::Queued;
                t.started_at = None;
                t.completed_at = None;
                t.note = None;
                t.result = None;
                t.progress = None;
                if author_left {
                    t.assigned_agent = None;
                }
                // A reviewer who has left stays its reviewer: a configured
                // review is not dropped with the member. The finished work
                // then waits in Review, and the orchestrator is asked for
                // the verdict (complete_task).
            })?
            .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
        let unassigned = if author_left {
            "; its member has left the team, so it goes to whoever is free"
        } else {
            ""
        };
        self.emit(
            NewEvent::new(
                "scheduler",
                "task.retry",
                format!("task {id} queued again{unassigned}"),
            )
            .task(id),
        );
        self.replan();
        Ok(task)
    }

    // ---- progress and status -------------------------------------------

    pub(crate) fn report_progress(
        &self,
        agent: &str,
        percent: u8,
        message: Option<String>,
        task_id: Option<String>,
    ) -> Result<(), RpcError> {
        if percent > 100 {
            return Err(RpcError::invalid("percent must be between 0 and 100"));
        }
        let message = message.map(|m| truncate_chars(&one_line(&m), 200));
        let task = {
            let model = self.model();
            match &task_id {
                Some(id) => {
                    let task = model
                        .task(id)
                        .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
                    if task.assigned_agent.as_deref() != Some(agent) {
                        return Err(RpcError::forbidden(format!("task {id} is not yours")));
                    }
                    Some(task.id.clone())
                }
                None => active_task(&model.tasks, agent).map(|t| t.id.clone()),
            }
        };
        // The task first: a refused save is the caller's error, and the
        // agent then shows no progress the task does not have.
        if let Some(task) = &task {
            self.try_update_task(task, |t| {
                t.progress = Some(percent);
                if message.is_some() {
                    t.note = message.clone();
                }
            })?;
        }
        let now = now_ms();
        let is_current = task
            .as_deref()
            .is_none_or(|id| active_task(&self.model().tasks, agent).is_some_and(|t| t.id == id));
        self.update_agent(agent, |a| {
            if is_current {
                a.progress = Some(percent);
                if let Some(message) = &message {
                    a.activity = message.clone();
                }
            }
            a.last_activity_at = now;
        });
        let mut event = NewEvent::new(
            "agent",
            "task.progress",
            format!(
                "{agent} {percent}%{}",
                message.map(|m| format!(" — {m}")).unwrap_or_default()
            ),
        )
        .agent(agent);
        if let Some(task) = &task {
            event = event.task(task);
        }
        self.emit(event);
        Ok(())
    }

    /// An agent's self-reported status is reflected on its current task.
    pub(crate) fn mirror_status_to_task(
        &self,
        agent: &str,
        status: AgentStatus,
        note: Option<String>,
    ) -> Result<(), RpcError> {
        self.mirror_status_to_task_id(agent, status, note, None)
            .map(|_| ())
    }

    pub(crate) fn mirror_status_to_task_id(
        &self,
        agent: &str,
        status: AgentStatus,
        note: Option<String>,
        task_id: Option<String>,
    ) -> Result<Option<String>, RpcError> {
        let explicit = task_id.is_some();
        let task = {
            let model = self.model();
            task_id.or_else(|| active_task(&model.tasks, agent).map(|t| t.id.clone()))
        };
        let Some(task) = task else { return Ok(None) };
        self.try_update_task_checked(&task, |model, t| {
            if t.assigned_agent.as_deref() != Some(agent) {
                return Err(RpcError::forbidden(format!("task {} is not yours", t.id)));
            }
            if !explicit
                && active_task(&model.tasks, agent).is_none_or(|current| current.id != t.id)
            {
                return Err(RpcError::conflict(
                    "current task changed; retry the status report",
                ));
            }
            if !matches!(
                t.status,
                TaskStatus::Working | TaskStatus::Waiting | TaskStatus::Blocked
            ) || (t.status == TaskStatus::Blocked && t.started_at.is_none())
            {
                return Err(RpcError::conflict(format!(
                    "task {} has no started work to report on",
                    t.id
                )));
            }
            if status != AgentStatus::Waiting {
                if let Some(current) =
                    tasks::executing_task(&model.tasks, agent).filter(|current| current.id != t.id)
                {
                    return Err(RpcError::conflict(format!(
                        "another task is executing: {}. Finish or park it first to resume {}; \
if no more work is needed, task_complete(task_id=\"{}\") can finish the parked card directly",
                        current.id, t.id, t.id
                    )));
                }
            }
            match status {
                AgentStatus::Waiting => {
                    t.status = TaskStatus::Waiting;
                    t.note = note.clone();
                }
                AgentStatus::Blocked => {
                    t.status = TaskStatus::Blocked;
                    t.note = note.clone();
                }
                _ => {
                    if matches!(t.status, TaskStatus::Waiting | TaskStatus::Blocked) {
                        t.status = TaskStatus::Working;
                        t.note = note.clone();
                    }
                }
            }
            Ok(())
        })?
        .ok_or_else(|| RpcError::not_found(format!("no task '{task}'")))?;
        self.clear_task_wakeups(agent, &task);
        self.project_task(agent);
        Ok(Some(task))
    }

    // ---- completion -----------------------------------------------------

    /// Find the task a call refers to and check the caller may act on it.
    fn resolve_task(&self, who: &Who, task_id: Option<String>) -> Result<Task, RpcError> {
        let model = self.model();
        match (who, task_id) {
            (Who::Agent(agent), None) => active_task(&model.tasks, agent)
                .cloned()
                .ok_or_else(|| RpcError::not_found("you have no task in progress")),
            (Who::Agent(agent), Some(id)) => {
                let task = model
                    .task(&id)
                    .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
                let is_orchestrator = model.agent(agent).is_some_and(|a| a.orchestrator);
                if task.assigned_agent.as_deref() != Some(agent.as_str()) && !is_orchestrator {
                    return Err(RpcError::forbidden(format!("task {id} is not yours")));
                }
                Ok(task.clone())
            }
            (Who::Operator, Some(id)) => model
                .task(&id)
                .cloned()
                .ok_or_else(|| RpcError::not_found(format!("no task '{id}'"))),
            (Who::Operator, None) => Err(RpcError::invalid("specify the task id")),
        }
    }

    pub(crate) async fn complete_task(
        &self,
        who: &Who,
        task_id: Option<String>,
        result: String,
        artifacts: Vec<Artifact>,
    ) -> Result<Task, RpcError> {
        let task = self.resolve_task(who, task_id)?;
        let was_current = task.assigned_agent.as_deref().is_some_and(|agent| {
            active_task(&self.model().tasks, agent).is_some_and(|current| current.id == task.id)
        });
        match task.status {
            // Completing twice is harmless.
            TaskStatus::Done | TaskStatus::Review => return Ok(task),
            TaskStatus::Failed => {
                return Err(RpcError::conflict(format!(
                    "task {} has failed; it must be retried first",
                    task.id
                )))
            }
            TaskStatus::Queued | TaskStatus::Ready => {
                return Err(RpcError::conflict(format!(
                    "task {} has not been started yet",
                    task.id
                )))
            }
            // Blocked before it started: a dependency failed, so it was
            // never handed out.
            TaskStatus::Blocked if task.started_at.is_none() => {
                return Err(RpcError::conflict(format!(
                    "task {} has not been started yet (it is blocked: {})",
                    task.id,
                    task.note.as_deref().unwrap_or("a dependency failed")
                )))
            }
            TaskStatus::Working | TaskStatus::Waiting | TaskStatus::Blocked => {}
        }
        let author = task
            .assigned_agent
            .clone()
            .unwrap_or_else(|| who.sender().to_string());
        let now = now_ms();
        let artifacts: Vec<Artifact> = artifacts
            .into_iter()
            .take(MAX_ARTIFACTS)
            .map(|mut a| {
                a.kind = truncate_chars(a.kind.trim(), 32);
                a.value = truncate_chars(a.value.trim(), 500);
                a.description = truncate_chars(a.description.trim(), 300);
                a.agent = Some(author.clone());
                a.created_at = now;
                a
            })
            .filter(|a| !a.value.is_empty())
            .collect();
        let result = truncate_chars(result.trim(), MAX_RESULT_CHARS);
        // A configured review is never skipped. When its reviewer cannot do
        // it, the task waits in Review and the orchestrator decides.
        // `ask_reviewer`: the reviewer gets the review request.
        let reviewer = task.reviewer.clone();
        let (unavailable, ask_reviewer) = match &reviewer {
            None => (None, false),
            Some(r) => match self.model().agent(r) {
                _ if *r == author => (Some(format!("{r} is its author")), false),
                None => (Some(format!("{r} is not on the team")), false),
                Some(a) if a.status.is_terminal() => (Some(format!("{r} is {}", a.status)), true),
                Some(_) => (None, true),
            },
        };
        let mut claimed = false;
        let updated = self
            .try_update_task_checked(&task.id, |_, t| {
                if matches!(t.status, TaskStatus::Done | TaskStatus::Review) {
                    return Ok(());
                }
                if t.status != task.status {
                    return Err(RpcError::conflict(
                        "task changed before completion; read its card again",
                    ));
                }
                claimed = true;
                t.result = Some(if result.is_empty() {
                    "done".into()
                } else {
                    result.clone()
                });
                for artifact in artifacts {
                    if !t
                        .artifacts
                        .iter()
                        .any(|a| a.kind == artifact.kind && a.value == artifact.value)
                    {
                        t.artifacts.push(artifact);
                    }
                }
                t.progress = Some(100);
                match &reviewer {
                    Some(reviewer) => {
                        t.status = TaskStatus::Review;
                        t.note = Some(format!("awaiting review by {reviewer}"));
                    }
                    None => {
                        t.status = TaskStatus::Done;
                        t.completed_at = Some(now);
                        t.note = None;
                    }
                }
                Ok(())
            })?
            .ok_or_else(|| RpcError::not_found("task vanished"))?;

        if !claimed {
            return Ok(updated);
        }
        self.clear_task_wakeups(&author, &task.id);
        if was_current {
            self.model().rt(&author).self_status = None;
        }
        match &reviewer {
            Some(reviewer) => {
                self.emit(
                    NewEvent::new(
                        "scheduler",
                        "task.review",
                        format!(
                            "task {} by {author} awaits review by {reviewer}",
                            updated.id
                        ),
                    )
                    .task(&updated.id)
                    .agent(&author),
                );
                // A stopped reviewer finds the request when it is restarted.
                let asked =
                    ask_reviewer && self.send_review_request(&updated, &author, reviewer).await;
                // (A request that could not be sent has been said by now.)
                if let Some(why) = unavailable.filter(|_| asked || !ask_reviewer) {
                    let next = if asked {
                        format!(
                            "Restart {reviewer} (the review request waits for it), or review it \
yourself"
                        )
                    } else {
                        "Review it yourself".to_string()
                    };
                    self.review_blocked(&updated, &author, &why, &next).await;
                }
            }
            None => self.finish_task(&updated, &author).await,
        }
        if was_current && tasks::executing_task(&self.model().tasks, &author).is_none() {
            self.update_agent(&author, |a| {
                a.progress = Some(100);
                a.activity = format!("finished {}", updated.title);
            });
        }
        self.project_task(&author);
        if was_current {
            self.settle(&author);
        }
        self.replan();
        Ok(updated)
    }

    /// Ask `reviewer`, straight from the author, to review `task`. Says
    /// whether the request was sent. One that could not be is not passed
    /// over: the task stays in Review and the orchestrator hears of it.
    pub(crate) async fn send_review_request(
        &self,
        task: &Task,
        author: &str,
        reviewer: &str,
    ) -> bool {
        let shared = self.model().session.isolation == "shared";
        let commit = task
            .artifacts
            .iter()
            .rev()
            .find(|a| a.kind == "commit")
            .map(|a| a.value.clone());
        let body = review_request(task, shared);
        let message = NewMessage::new(author, reviewer, MessageType::ReviewRequest, body)
            .metadata(json!({ "task_id": task.id, "commit": commit }))
            .correlation(Some(format!("review-{}", task.id)));
        let Err(e) = self.send_message(message).await else {
            return true;
        };
        tracing::warn!("sending the review request failed: {}", e.message);
        let why = format!(
            "the review request to {reviewer} could not be sent ({})",
            truncate_chars(&one_line(&e.message), 200)
        );
        self.review_blocked(task, author, &why, "Review it yourself")
            .await;
        false
    }

    /// A review that cannot go ahead: the task stays in Review, and the
    /// orchestrator is asked to see to it (`next`: what it can do).
    async fn review_blocked(&self, task: &Task, author: &str, why: &str, next: &str) {
        self.emit(
            NewEvent::new(
                "scheduler",
                "task.review_blocked",
                format!("task {} cannot be reviewed now: {why}", task.id),
            )
            .task(&task.id)
            .warn(),
        );
        self.notify_orchestrator(&format!(
            "Task {} (\"{}\") by {author} is waiting for review, but {why}. {next} \
with review_submit(task_id=\"{}\", approve=true|false).",
            task.id,
            truncate_chars(&task.title, QUOTED_TITLE_CHARS),
            task.id
        ))
        .await;
    }

    /// Bookkeeping once a task is really done (immediately, or after approval).
    async fn finish_task(&self, task: &Task, author: &str) {
        self.model()
            .local
            .entry(author.to_string())
            .or_default()
            .tasks_completed += 1;
        self.save_local(author);
        self.emit(
            NewEvent::new(
                "scheduler",
                "task.done",
                format!(
                    "task {} done by {author}: {}",
                    task.id,
                    truncate_chars(&one_line(task.result.as_deref().unwrap_or("")), 160)
                ),
            )
            .task(&task.id)
            .agent(author),
        );
        let mut note = format!(
            "Task {} ({}) was completed by {author}: {}",
            task.id,
            task.title,
            truncate_chars(&one_line(task.result.as_deref().unwrap_or("")), 400)
        );
        let commits: Vec<&str> = task
            .artifacts
            .iter()
            .filter(|a| a.kind == "commit")
            .map(|a| a.value.as_str())
            .collect();
        if !commits.is_empty() {
            note.push_str(&format!(" Commits: {}.", commits.join(", ")));
        }
        note.push_str(&self.leaving_note(author));
        self.notify_orchestrator(&note).await;
        self.report_to_parent(task, author).await;
    }

    /// Mark a task failed (crash, timeout) and release its agent. A refused
    /// save is logged; nothing changed then.
    pub(crate) async fn fail_task(&self, id: &str, reason: String) {
        if let Err(e) = self.try_fail_task(id, reason).await {
            tracing::error!("{}", e.message);
        }
    }

    /// `fail_task`, returning a refused save to the caller.
    async fn try_fail_task(&self, id: &str, reason: String) -> Result<(), RpcError> {
        let now = now_ms();
        let mut claimed = false;
        let Some(task) = self.try_update_task(id, |t| {
            if t.status.is_terminal() {
                return;
            }
            claimed = true;
            t.status = TaskStatus::Failed;
            t.completed_at = Some(now);
            t.note = Some(reason.clone());
        })?
        else {
            return Ok(());
        };
        if !claimed {
            return Ok(());
        }
        let mut event = NewEvent::new(
            "scheduler",
            "task.failed",
            format!("task {id} failed: {reason}"),
        )
        .task(id)
        .error();
        if let Some(agent) = &task.assigned_agent {
            event = event.agent(agent);
            self.clear_task_wakeups(agent, id);
            self.project_task(agent);
            self.settle(agent);
        }
        self.emit(event);
        let leaving = task
            .assigned_agent
            .as_deref()
            .map(|agent| self.leaving_note(agent))
            .unwrap_or_default();
        self.notify_orchestrator(&format!(
            "Task {id} failed: {reason}. Decide whether to retry it (task_retry), reassign it \
(task_assign) or leave it.{leaving}"
        ))
        .await;
        self.replan();
        Ok(())
    }

    pub(crate) async fn fail_task_by(
        &self,
        who: &Who,
        task_id: Option<String>,
        reason: String,
    ) -> Result<Task, RpcError> {
        let task = self.resolve_task(who, task_id)?;
        let was_current = task.assigned_agent.as_deref().is_some_and(|agent| {
            active_task(&self.model().tasks, agent).is_some_and(|current| current.id == task.id)
        });
        if task.status.is_terminal() {
            return Err(RpcError::conflict(format!(
                "task {} is already {}",
                task.id, task.status
            )));
        }
        let reason = truncate_chars(reason.trim(), 500);
        self.try_fail_task(&task.id, format!("{} gave up: {reason}", who.sender()))
            .await?;
        // Only once it is stored: giving up ends what the agent said about
        // itself (waiting, blocked), so it settles afresh.
        if let Some(agent) = task.assigned_agent.as_ref().filter(|_| was_current) {
            self.model().rt(agent).self_status = None;
            self.settle(agent);
        }
        self.model()
            .task(&task.id)
            .cloned()
            .ok_or_else(|| RpcError::not_found("task vanished"))
    }

    // ---- reviews --------------------------------------------------------

    pub(crate) async fn request_review(
        &self,
        who: &Who,
        agent_id: String,
        artifact: String,
        task_id: Option<String>,
        note: Option<String>,
    ) -> Result<SendResult, RpcError> {
        let mut body = format!("Please review: {}", artifact.trim());
        if let Some(note) = note.filter(|n| !n.trim().is_empty()) {
            body.push_str(&format!("\n\n{}", note.trim()));
        }
        body.push_str("\n\nReply with agent_send (type \"review_result\").");
        let message = NewMessage::new(who.sender(), &agent_id, MessageType::ReviewRequest, body)
            .metadata(json!({ "artifact": artifact, "task_id": task_id }));
        let outcome = self.send_message(message).await?;
        Ok(SendResult {
            message: outcome.message,
            duplicate: outcome.duplicate,
            recipients: outcome.recipients,
        })
    }

    pub(crate) async fn submit_review(
        &self,
        who: &Who,
        task_id: &str,
        approve: bool,
        comments: String,
    ) -> Result<Task, RpcError> {
        // One verdict at a time, and none beside the scheduler: the task is
        // read, checked and changed under the plan lock, so the scheduler
        // cannot hand the author something else in between, and two
        // verdicts cannot both find it awaiting review.
        let planning = self.plan_lock.lock().await;
        let task = self
            .model()
            .task(task_id)
            .cloned()
            .ok_or_else(|| RpcError::not_found(format!("no task '{task_id}'")))?;
        if task.status != TaskStatus::Review {
            return Err(RpcError::conflict(format!(
                "task {task_id} is {}, not awaiting review",
                task.status
            )));
        }
        if let Who::Agent(agent) = who {
            let is_orchestrator = self.model().agent(agent).is_some_and(|a| a.orchestrator);
            if task.reviewer.as_deref() != Some(agent.as_str()) && !is_orchestrator {
                return Err(RpcError::forbidden(format!(
                    "task {task_id} is to be reviewed by {}",
                    task.reviewer.as_deref().unwrap_or("someone else")
                )));
            }
            // Only the orchestrator (or the operator) may waive a review of its
            // own work.
            if task.assigned_agent.as_deref() == Some(agent.as_str()) && !is_orchestrator {
                return Err(RpcError::forbidden(format!(
                    "task {task_id} is your own work; its review is not yours to give"
                )));
            }
        }
        let reviewer = who.sender().to_string();
        let author = task.assigned_agent.clone();
        let comments = truncate_chars(comments.trim(), MAX_RESULT_CHARS);
        let now = now_ms();
        // An author already on another task gets the rework back once that
        // task is finished: an agent works on one task at a time.
        let queued = !approve
            && author
                .as_deref()
                .is_some_and(|a| active_task(&self.model().tasks, a).is_some());
        // The verdict, made to the task as it is when it is stored (a card
        // edit or a progress report meanwhile is kept), if it is still
        // awaiting review.
        let verdict = |t: &mut Task| -> Result<(), RpcError> {
            if t.status != TaskStatus::Review {
                return Err(RpcError::conflict(format!(
                    "task {task_id} is {}, not awaiting review",
                    t.status
                )));
            }
            if approve {
                t.status = TaskStatus::Done;
                t.completed_at = Some(now);
                t.note = None;
            } else {
                t.status = if queued {
                    TaskStatus::Ready
                } else {
                    TaskStatus::Working
                };
                t.progress = None;
                t.note = Some(format!("changes requested by {reviewer}"));
            }
            Ok(())
        };
        // The verdict is told to the author in the same write that applies
        // it: both are stored, or neither is. One that cannot be (the bus or
        // the database refuses) is not applied: the task stays in Review,
        // and the reviewer and the orchestrator hear of it.
        let updated = match author.as_deref().filter(|a| *a != reviewer) {
            Some(author) => {
                let message =
                    self.review_result(&task, &reviewer, author, approve, &comments, queued);
                let mut refused = None;
                let stored = self.recipients_of(&message).and_then(|(recipients, p2p)| {
                    let apply =
                        |t: &mut Task| verdict(t).inspect_err(|e| refused = Some(e.clone()));
                    let told = self.try_update_task_telling(task_id, apply, message, recipients)?;
                    Ok(told.map(|(updated, outcome)| (updated, outcome, p2p)))
                });
                if let Some(e) = refused {
                    return Err(e);
                }
                match stored {
                    Ok(Some((updated, outcome, p2p))) => {
                        drop(planning);
                        self.message_sent(&outcome, p2p).await;
                        updated
                    }
                    Ok(None) => return Err(RpcError::not_found("task vanished")),
                    Err(e) => {
                        drop(planning);
                        return Err(self
                            .review_result_unsent(&task, &reviewer, author, approve, &comments, &e)
                            .await);
                    }
                }
            }
            None => {
                let mut refused = None;
                let saved = self.try_update_task(task_id, |t| {
                    if let Err(e) = verdict(t) {
                        refused = Some(e);
                    }
                });
                drop(planning);
                if let Some(e) = refused {
                    return Err(e);
                }
                saved?.ok_or_else(|| RpcError::not_found("task vanished"))?
            }
        };

        self.emit(
            NewEvent::new(
                "scheduler",
                if approve {
                    "task.approved"
                } else {
                    "task.changes_requested"
                },
                format!(
                    "{reviewer} {} task {task_id}",
                    if approve {
                        "approved"
                    } else {
                        "requested changes on"
                    }
                ),
            )
            .task(task_id)
            .agent(&reviewer),
        );
        if let Some(author) = &author {
            if approve {
                self.finish_task(&updated, author).await;
                self.settle(author);
            } else if !queued {
                self.update_agent(author, |a| {
                    a.task = Some(updated.title.clone());
                    a.progress = None;
                });
            }
        }
        self.replan();
        Ok(updated)
    }

    /// The message telling `author` the verdict on `task` (`queued`: the
    /// author is on another task, which it finishes first).
    fn review_result(
        &self,
        task: &Task,
        reviewer: &str,
        author: &str,
        approve: bool,
        comments: &str,
        queued: bool,
    ) -> NewMessage {
        let task_id = &task.id;
        let body = if approve {
            format!(
                "Approved: task \"{}\" (id {task_id}).{}",
                task.title,
                if comments.is_empty() {
                    String::new()
                } else {
                    format!("\n\n{comments}")
                }
            )
        } else {
            // In a shared workspace there is nothing to commit: the work is
            // already where everyone sees it.
            let commit = if self.model().session.isolation == "shared" {
                ""
            } else {
                "git_commit, "
            };
            let next = if queued {
                format!(
                    "Finish your current task first: this one is handed back to you after \
it. Then address them, {commit}and call task_complete again."
                )
            } else {
                format!("Address them, {commit}then call task_complete again.")
            };
            format!(
                "Changes requested on task \"{}\" (id {task_id}):\n\n{comments}\n\n{next}",
                task.title
            )
        };
        NewMessage::new(reviewer, author, MessageType::ReviewResult, body)
            .metadata(json!({ "task_id": task_id, "approved": approve }))
            .correlation(Some(format!("review-{task_id}")))
    }

    /// A verdict on `task` that could not be told to its author: it is not
    /// applied, the event says why, and the orchestrator is given the
    /// verdict. Returns the error for the one who gave it, to give it again.
    async fn review_result_unsent(
        &self,
        task: &Task,
        reviewer: &str,
        author: &str,
        approve: bool,
        comments: &str,
        e: &RpcError,
    ) -> RpcError {
        tracing::warn!("sending the review result failed: {}", e.message);
        let why = format!(
            "the review result could not be sent to {author} ({})",
            truncate_chars(&one_line(&e.message), 200)
        );
        let verdict = if approve {
            "approved"
        } else {
            "changes requested"
        };
        self.emit(
            NewEvent::new(
                "scheduler",
                "task.review_result_unsent",
                format!(
                    "task {}: {reviewer}'s verdict ({verdict}) is not applied: {why}",
                    task.id
                ),
            )
            .task(&task.id)
            .agent(reviewer)
            .warn(),
        );
        let said = if comments.is_empty() {
            String::new()
        } else {
            format!(
                " Its comments: \"{}\"",
                truncate_chars(&one_line(comments), 500)
            )
        };
        self.notify_orchestrator(&format!(
            "Task {} (\"{}\") by {author} stays in Review: {reviewer}'s verdict ({verdict}) was not \
applied, because {why}.{said} Ask {reviewer} to give it again, or tell {author} yourself and give \
it with review_submit(task_id=\"{}\", approve=true|false).",
            task.id,
            truncate_chars(&task.title, 80),
            task.id
        ))
        .await;
        RpcError::new(
            &e.code,
            format!(
                "{why}; task {} stays in Review: submit the review again",
                task.id
            ),
        )
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::message_bus::{BusError, BusEvent, MessageBus, SendOutcome};
    use crate::persistence::MessageCounts;
    use crew_core::InboxEntry;
    use std::sync::{mpsc, Arc, Mutex};

    type Bus<T> = std::result::Result<T, BusError>;

    /// A bus whose send_with_task waits until it is released (rev-core's
    /// fixture, m-4db439acbc56): a verdict can be held between reading its
    /// task and storing it.
    struct GatedBus {
        bus: Arc<dyn MessageBus>,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl MessageBus for GatedBus {
        fn send(&self, m: NewMessage, r: Vec<String>) -> Bus<SendOutcome> {
            self.bus.send(m, r)
        }
        fn send_with_task(&self, m: NewMessage, r: Vec<String>, t: &Task) -> Bus<SendOutcome> {
            self.entered.send(()).unwrap();
            let release = self.release.lock().unwrap();
            release
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            self.bus.send_with_task(m, r, t)
        }
        fn receive(&self, a: &str, u: bool, l: usize) -> Bus<Vec<InboxEntry>> {
            self.bus.receive(a, u, l)
        }
        fn undelivered(&self, a: &str) -> Bus<Vec<Message>> {
            self.bus.undelivered(a)
        }
        fn mark_delivered(&self, a: &str, i: &[String]) -> Bus<()> {
            self.bus.mark_delivered(a, i)
        }
        fn acknowledge(&self, a: &str, i: Option<&[String]>) -> Bus<usize> {
            self.bus.acknowledge(a, i)
        }
        fn history(&self, a: Option<&str>, l: usize) -> Bus<Vec<Message>> {
            self.bus.history(a, l)
        }
        fn get(&self, i: &str) -> Bus<Option<(Message, Vec<String>)>> {
            self.bus.get(i)
        }
        fn counts(&self, a: &str) -> Bus<MessageCounts> {
            self.bus.counts(a)
        }
        fn subscribe(&self) -> tokio::sync::broadcast::Receiver<BusEvent> {
            self.bus.subscribe()
        }
        fn set_max_bytes(&self, l: usize) {
            self.bus.set_max_bytes(l)
        }
        fn max_bytes(&self) -> usize {
            self.bus.max_bytes()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_card_edit_made_while_a_verdict_is_stored_is_kept() {
        let temp = tempfile::tempdir().unwrap();
        let mut running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let original = running.daemon.bus.clone();
        Arc::get_mut(&mut running.daemon.0).unwrap().bus = Arc::new(GatedBus {
            bus: original,
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let daemon = running.daemon.clone();
        for id in ["author", "critic"] {
            let agent = serde_json::from_value(json!({
                "id": id, "name": id, "role": "work", "status": "idle", "runtime": "shell",
                "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
            }))
            .unwrap();
            daemon.model().agents.push(agent);
        }
        let task: Task = serde_json::from_value(json!({
            "id": "t", "title": "original", "status": "review", "created_at": 0,
            "assigned_agent": "author", "reviewer": "critic"
        }))
        .unwrap();
        daemon.store.save_task(&task).unwrap();
        daemon.model().tasks.push(task);

        // The critic's verdict is being stored; meanwhile the operator edits
        // the card.
        let review = tokio::spawn({
            let daemon = daemon.clone();
            async move {
                daemon
                    .submit_review(&Who::Agent("critic".into()), "t", false, "revise".into())
                    .await
            }
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let title = "edited while the verdict was stored";
        let edit = tokio::spawn({
            let daemon = daemon.clone();
            async move {
                daemon
                    .edit_task(&Who::Operator, "t", Some(title.into()), None)
                    .await
            }
        });
        // (On this thread: the workers may all be held, one by the verdict,
        // one by the edit waiting for it.)
        std::thread::sleep(std::time::Duration::from_millis(200));
        release.send(()).unwrap();
        review.await.unwrap().unwrap();
        edit.await.unwrap().unwrap();

        // Both hold: the verdict and the edit.
        let in_memory = daemon.model().task("t").unwrap().clone();
        let stored = daemon
            .store
            .load_tasks()
            .unwrap()
            .into_iter()
            .find(|t| t.id == "t")
            .unwrap();
        for (copy, task) in [("in memory", &in_memory), ("stored", &stored)] {
            assert_eq!(
                (task.title.as_str(), task.status),
                (title, TaskStatus::Working),
                "{copy}: the verdict's earlier copy of the task overwrote the edit"
            );
        }
    }
}
