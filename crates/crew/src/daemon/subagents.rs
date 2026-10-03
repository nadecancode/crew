//! Subagents: agents launched at runtime by other agents.
//!
//! A subagent is a full team member — its own process, terminal, worktree
//! and inbox — that happens to have a parent. It starts from the parent's
//! branch, gets one task, reports the result straight to its parent when it
//! finishes, and is retired once it is done.

use crew_core::config::{validate_agent_spec, AgentSpec};
use crew_core::rpc::RpcError;
use crew_core::util::{is_valid_id, one_line, truncate_chars};
use crew_core::{Agent, AgentStatus, MessageType, RuntimeKind, Task};
use serde_json::json;

use super::handlers::internal;
use super::{Daemon, IdReservation};
use crate::message_bus::NewMessage;
use crate::persistence::NewEvent;

const MAX_TASK_CHARS: usize = 8000;

impl Daemon {
    pub(crate) async fn spawn_subagent(
        &self,
        parent_id: &str,
        id: Option<String>,
        role: String,
        task: String,
        runtime: Option<RuntimeKind>,
        model: Option<String>,
    ) -> Result<Agent, RpcError> {
        let task = task.trim().to_string();
        if task.is_empty() {
            return Err(RpcError::invalid("a subagent needs a task"));
        }
        if task.chars().count() > MAX_TASK_CHARS {
            return Err(RpcError::too_large(format!(
                "the task description is limited to {MAX_TASK_CHARS} characters"
            )));
        }
        let (team, parent, worktrees, backend, id, command) = {
            let mut model_guard = self.model();
            let team = model_guard
                .session
                .team
                .clone()
                .ok_or_else(|| RpcError::unavailable("no team is loaded"))?;
            let parent = model_guard
                .agent(parent_id)
                .cloned()
                .ok_or_else(|| RpcError::not_found(format!("no agent '{parent_id}'")))?;
            if parent.parent.is_some() {
                return Err(RpcError::forbidden(
                    "subagents cannot launch subagents of their own; ask your parent instead",
                ));
            }
            let running = model_guard
                .agents
                .iter()
                .filter(|a| {
                    a.parent.is_some() && !a.status.is_terminal() && a.status != AgentStatus::Done
                })
                .count();
            if running >= team.limits.max_subagents {
                return Err(RpcError::conflict(format!(
                    "the team already has {running} subagent(s) running (limit {}); wait for one \
to finish",
                    team.limits.max_subagents
                )));
            }
            let id = match id {
                Some(id) => {
                    if !is_valid_id(&id) {
                        return Err(RpcError::invalid(format!(
                            "invalid id '{id}' (lowercase letters, digits, - and _; at most 32 chars)"
                        )));
                    }
                    if model_guard.id_taken(&id) {
                        return Err(RpcError::conflict(format!("agent '{id}' already exists")));
                    }
                    id
                }
                None => {
                    (1..)
                        .map(|n| {
                            // Room for the suffix, and for "-task" in its task id.
                            let suffix = format!("-s{n}");
                            let stem: String = parent_id.chars().take(27 - suffix.len()).collect();
                            format!("{stem}{suffix}")
                        })
                        .find(|candidate| !model_guard.id_taken(candidate))
                        .expect("an unused subagent id exists")
                }
            };
            let command = model_guard
                .local
                .get(parent_id)
                .map(|l| l.settings.command.clone())
                .unwrap_or_default();
            // Taken before anything is awaited (see add_agent).
            model_guard.reserved.insert(id.clone());
            (
                team,
                parent,
                model_guard.session.isolation == "worktree",
                model_guard.session.backend.clone(),
                id,
                command,
            )
        };
        let _reserved = IdReservation::new(self, &id);

        let runtime = runtime.unwrap_or(parent.runtime);
        let spec = AgentSpec {
            id: id.clone(),
            role: if role.trim().is_empty() {
                format!("subagent of {parent_id}")
            } else {
                truncate_chars(role.trim(), 80)
            },
            runtime: Some(runtime),
            model: model.or_else(|| parent.model.clone()),
            // A scripted parent's subagents run the same script.
            command: if runtime == RuntimeKind::Shell {
                command
            } else {
                Vec::new()
            },
            ..AgentSpec::default()
        };
        validate_agent_spec(&spec).map_err(|e| RpcError::invalid(e.to_string()))?;
        if runtime == RuntimeKind::Shell && spec.command.is_empty() {
            return Err(RpcError::invalid("a shell subagent needs a command"));
        }
        let own_task = self.own_task(&id, task)?;

        let daemon = self.clone();
        let parent_for_task = parent.clone();
        let spec_for_task = spec.clone();
        let agent = tokio::task::spawn_blocking(move || {
            daemon.create_agent(
                &spec_for_task,
                &team,
                false,
                worktrees,
                &backend,
                Some(&parent_for_task),
            )
        })
        .await
        .map_err(internal)?
        .map_err(|e| RpcError::internal(format!("creating the subagent: {e:#}")))?;

        self.emit(
            NewEvent::new(
                "daemon",
                "agent.subagent",
                format!(
                    "{parent_id} launched subagent {id}: {}",
                    truncate_chars(&one_line(&own_task.description), 120)
                ),
            )
            .agent(&id)
            .data(json!({ "parent": parent_id })),
        );
        self.create_task(own_task)?;
        // Dispatch first so the subagent's first prompt is its task.
        self.apply_plan().await;
        self.spawn_in_background(&id, false, None);
        Ok(agent)
    }

    /// Tell a subagent's parent, directly, that its task finished.
    pub(crate) async fn report_to_parent(&self, task: &Task, author: &str) {
        let Some(parent) = self.agent(author).and_then(|a| a.parent) else {
            return;
        };
        let mut body = format!(
            "Subagent {author} finished its task \"{}\": {}",
            task.title,
            task.result.as_deref().unwrap_or("done").trim()
        );
        let commits: Vec<&str> = task
            .artifacts
            .iter()
            .filter(|a| a.kind == "commit")
            .map(|a| a.value.as_str())
            .collect();
        if !commits.is_empty() {
            body.push_str(&format!(
                "\nCommits: {}. Use git_cherry_pick or git_merge(from_agent=\"{author}\") to take them.",
                commits.join(", ")
            ));
        }
        let kind = if commits.is_empty() {
            MessageType::Status
        } else {
            MessageType::Artifact
        };
        let message = NewMessage::new(author, &parent, kind, body)
            .metadata(json!({ "task_id": task.id, "commits": commits }));
        if let Err(e) = self.send_message(message).await {
            tracing::warn!("reporting to the parent failed: {}", e.message);
        }
    }

    /// A subagent whose work is done no longer needs its process.
    pub(crate) fn retire_if_finished(&self, id: &str) {
        let Some(agent) = self.agent(id) else { return };
        if agent.parent.is_none() || agent.status != AgentStatus::Done {
            return;
        }
        let daemon = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            // Leave a moment for the final output to settle.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let still_done = daemon
                .agent(&id)
                .is_some_and(|a| a.status == AgentStatus::Done);
            if !still_done || !daemon.bus.undelivered(&id).unwrap_or_default().is_empty() {
                return;
            }
            let Ok(procs) = daemon.procs() else { return };
            if !procs.is_alive(&id) {
                return;
            }
            daemon.model().rt(&id).expected_exit = true;
            if procs.kill(&id).await.is_ok() {
                daemon.update_agent(&id, |a| {
                    a.pid = None;
                    a.activity = "retired".into();
                });
                daemon.emit(
                    NewEvent::new("daemon", "agent.retired", format!("subagent {id} retired"))
                        .agent(&id),
                );
            }
        });
    }
}
