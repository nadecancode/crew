//! Task orchestration: applying the scheduler's plan, the task lifecycle
//! (dispatch, progress, completion, failure, retry) and reviews.

use crew_core::config::TaskSpec;
use crew_core::rpc::{RpcError, SendResult, TaskCard};
use crew_core::util::{now_ms, one_line, truncate_chars};
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

impl Daemon {
    /// Run the scheduler and carry out what it decides. Called whenever task
    /// or agent state changes.
    pub(crate) async fn apply_plan(&self) {
        let _planning = self.plan_lock.lock().await;
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
                    runnable: !a.paused && !a.status.is_terminal(),
                    // An agent parked at a usage limit is not stuck: it
                    // goes on when the limit lifts.
                    active: !a.orchestrator
                        && (matches!(a.status, AgentStatus::Working | AgentStatus::Spawning)
                            || a.limit.is_some()),
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
        for action in actions {
            match action {
                Action::MakeReady { task } => {
                    self.update_task(&task, |t| {
                        t.status = TaskStatus::Ready;
                        t.note = None;
                    });
                    self.emit(
                        NewEvent::new("scheduler", "task.ready", format!("task {task} is ready"))
                            .task(&task),
                    );
                }
                Action::Requeue { task } => {
                    self.update_task(&task, |t| {
                        t.status = TaskStatus::Queued;
                        t.note = None;
                    });
                }
                Action::Assign { task, agent } => {
                    self.update_task(&task, |t| t.assigned_agent = Some(agent.clone()));
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
                    self.update_task(&task, |t| {
                        t.status = TaskStatus::Blocked;
                        t.note = Some(reason.clone());
                    });
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
        if !stalled {
            self.model().stall = None;
        }
    }

    /// Hand a ready task to its agent with a `task` message.
    async fn dispatch_task(&self, task_id: &str, agent_id: &str) {
        let now = now_ms();
        // Check-and-set: only a task that is still ready is handed out, so a
        // task can never be dispatched twice.
        let mut claimed = false;
        let Some(task) = self.update_task(task_id, |t| {
            if t.status == TaskStatus::Ready {
                t.status = TaskStatus::Working;
                t.started_at = Some(now);
                t.progress = Some(0);
                t.note = None;
                claimed = true;
            }
        }) else {
            return;
        };
        if !claimed {
            return;
        }
        let body = prompt::task_dispatch(&task);
        self.update_agent(agent_id, |a| {
            a.task = Some(task.title.clone());
            a.progress = Some(0);
        });
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
        let task = self
            .update_task(id, |t| {
                if let Some(title) = &title {
                    t.title = title.clone();
                }
                if let Some(description) = &description {
                    t.description = description.clone();
                }
            })
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
                self.update_agent(agent, |a| {
                    if a.task.as_deref() == Some(before.title.as_str()) {
                        a.task = Some(task.title.clone());
                    }
                });
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
            model.tasks.push(task.clone());
            // New work reopens a session that had finished.
            model.session.completed_at = None;
            task
        };
        self.store
            .save_task(&task)
            .map_err(|e| RpcError::internal(e.to_string()))?;
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
            .update_task(id, |t| {
                if t.assigned_agent.as_deref() != Some(agent) {
                    t.assigned_agent = Some(agent.to_string());
                    if !matches!(t.status, TaskStatus::Queued | TaskStatus::Ready) {
                        // Moving a started task: the new owner starts over.
                        t.status = TaskStatus::Queued;
                        t.started_at = None;
                        t.note = None;
                    }
                }
            })
            .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
        if let Some(previous) = previous.filter(|p| p != agent) {
            self.update_agent(&previous, |a| {
                if a.task.as_deref() == Some(task.title.as_str()) {
                    a.task = None;
                }
            });
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
        let (status, author_left, reviewer_left) = {
            let model = self.model();
            let task = model
                .task(id)
                .ok_or_else(|| RpcError::not_found(format!("no task '{id}'")))?;
            let left =
                |who: &Option<String>| who.as_deref().is_some_and(|w| model.agent(w).is_none());
            (
                task.status,
                left(&task.assigned_agent),
                left(&task.reviewer),
            )
        };
        if !matches!(status, TaskStatus::Failed | TaskStatus::Blocked) {
            return Err(RpcError::conflict(format!(
                "task {id} is {status}; only failed or blocked tasks can be retried"
            )));
        }
        let task = self
            .update_task(id, |t| {
                t.status = TaskStatus::Queued;
                t.started_at = None;
                t.completed_at = None;
                t.note = None;
                t.result = None;
                t.progress = None;
                if author_left {
                    t.assigned_agent = None;
                }
                if reviewer_left {
                    t.reviewer = None;
                }
            })
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
        let now = now_ms();
        self.update_agent(agent, |a| {
            a.progress = Some(percent);
            if let Some(message) = &message {
                a.activity = message.clone();
            }
            a.last_activity_at = now;
        });
        if let Some(task) = &task {
            self.update_task(task, |t| {
                t.progress = Some(percent);
                if message.is_some() {
                    t.note = message.clone();
                }
            });
        }
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
    ) {
        let task = {
            let model = self.model();
            active_task(&model.tasks, agent).map(|t| t.id.clone())
        };
        let Some(task) = task else { return };
        self.update_task(&task, |t| match status {
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
        });
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
        let reviewer = {
            let model = self.model();
            task.reviewer
                .clone()
                .filter(|r| *r != author && model.agent(r).is_some_and(|a| !a.status.is_terminal()))
        };
        let updated = self
            .update_task(&task.id, |t| {
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
            })
            .ok_or_else(|| RpcError::not_found("task vanished"))?;

        self.model().rt(&author).self_status = None;
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
                // The review request goes straight from author to reviewer;
                // what to review against, and the work, are on the card.
                let id = &updated.id;
                let body = format!(
                    "Review my work on task {id} (\"{}\"): task_get(\"{id}\") reads its card, with my \
result and commits. git_cherry_pick or git_merge brings the work into your worktree; then \
review_submit(task_id=\"{id}\", approve=true|false, comments=…).",
                    updated.title
                );
                let commit = updated
                    .artifacts
                    .iter()
                    .rev()
                    .find(|a| a.kind == "commit")
                    .map(|a| a.value.clone());
                let message = NewMessage::new(&author, reviewer, MessageType::ReviewRequest, body)
                    .metadata(json!({ "task_id": updated.id, "commit": commit }))
                    .correlation(Some(format!("review-{}", updated.id)));
                if let Err(e) = self.send_message(message).await {
                    tracing::warn!("sending the review request failed: {}", e.message);
                }
            }
            None => self.finish_task(&updated, &author).await,
        }
        self.update_agent(&author, |a| {
            a.task = None;
            a.progress = Some(100);
            a.activity = format!("finished {}", updated.title);
        });
        self.settle(&author);
        self.replan();
        Ok(updated)
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

    /// Mark a task failed (crash, timeout) and release its agent.
    pub(crate) async fn fail_task(&self, id: &str, reason: String) {
        let now = now_ms();
        let Some(task) = self.update_task(id, |t| {
            t.status = TaskStatus::Failed;
            t.completed_at = Some(now);
            t.note = Some(reason.clone());
        }) else {
            return;
        };
        let mut event = NewEvent::new(
            "scheduler",
            "task.failed",
            format!("task {id} failed: {reason}"),
        )
        .task(id)
        .error();
        if let Some(agent) = &task.assigned_agent {
            event = event.agent(agent);
            self.update_agent(agent, |a| {
                if a.task.as_deref() == Some(task.title.as_str()) {
                    a.task = None;
                }
            });
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
    }

    pub(crate) async fn fail_task_by(
        &self,
        who: &Who,
        task_id: Option<String>,
        reason: String,
    ) -> Result<Task, RpcError> {
        let task = self.resolve_task(who, task_id)?;
        if task.status.is_terminal() {
            return Err(RpcError::conflict(format!(
                "task {} is already {}",
                task.id, task.status
            )));
        }
        let reason = truncate_chars(reason.trim(), 500);
        if let Some(agent) = &task.assigned_agent {
            self.model().rt(agent).self_status = None;
        }
        self.fail_task(&task.id, format!("{} gave up: {reason}", who.sender()))
            .await;
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
        }
        let reviewer = who.sender().to_string();
        let author = task.assigned_agent.clone();
        let comments = truncate_chars(comments.trim(), MAX_RESULT_CHARS);
        let now = now_ms();
        let updated = if approve {
            self.update_task(task_id, |t| {
                t.status = TaskStatus::Done;
                t.completed_at = Some(now);
                t.note = None;
            })
        } else {
            self.update_task(task_id, |t| {
                t.status = TaskStatus::Working;
                t.progress = None;
                t.note = Some(format!("changes requested by {reviewer}"));
            })
        }
        .ok_or_else(|| RpcError::not_found("task vanished"))?;

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
            let body = if approve {
                format!(
                    "Approved: task \"{}\" (id {task_id}).{}",
                    updated.title,
                    if comments.is_empty() {
                        String::new()
                    } else {
                        format!("\n\n{comments}")
                    }
                )
            } else {
                format!(
                    "Changes requested on task \"{}\" (id {task_id}):\n\n{comments}\n\n\
Address them, git_commit, then call task_complete again.",
                    updated.title
                )
            };
            if author != &reviewer {
                let message = NewMessage::new(&reviewer, author, MessageType::ReviewResult, body)
                    .metadata(json!({ "task_id": task_id, "approved": approve }))
                    .correlation(Some(format!("review-{task_id}")));
                if let Err(e) = self.send_message(message).await {
                    tracing::warn!("sending the review result failed: {}", e.message);
                }
            }
            if approve {
                self.finish_task(&updated, author).await;
                self.settle(author);
            } else {
                self.update_agent(author, |a| {
                    a.task = Some(updated.title.clone());
                    a.progress = None;
                });
            }
        }
        self.replan();
        Ok(updated)
    }
}
