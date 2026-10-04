//! Request dispatch: one arm per protocol call, with authorization.

use base64::Engine;
use crew_core::rpc::{
    Call, GitOutcome, HelloInfo, InboxResult, OutputResult, Push, Role, RpcError, SendParams,
    SendResult, ServerFrame, PROTOCOL_VERSION,
};
use crew_core::{AgentStatus, MessageType, BROADCAST, OPERATOR_ID};
use serde::Serialize;
use serde_json::{json, Value};

use super::server::{Conn, Who};
use super::{Daemon, VERSION};
use crate::message_bus::{BusError, NewMessage, SendOutcome};
use crate::persistence::NewEvent;
use crate::process::pty::strip_queries;

type Reply = Result<Value, RpcError>;

fn reply<T: Serialize>(value: T) -> Reply {
    serde_json::to_value(value).map_err(|e| RpcError::internal(e.to_string()))
}

pub(crate) fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}

impl From<BusError> for RpcError {
    fn from(e: BusError) -> Self {
        match &e {
            BusError::Empty | BusError::InvalidId(_) | BusError::NoRecipients => {
                RpcError::invalid(e.to_string())
            }
            BusError::TooLarge { .. } => RpcError::too_large(e.to_string()),
            BusError::IdConflict(_) => RpcError::conflict(e.to_string()),
            BusError::Storage(_) => RpcError::internal(e.to_string()),
        }
    }
}

fn operator_only(who: &Who) -> Result<(), RpcError> {
    match who {
        Who::Operator => Ok(()),
        Who::Agent(_) => Err(RpcError::forbidden("this operation is reserved for the operator")),
    }
}

fn agent_only(who: &Who) -> Result<&str, RpcError> {
    match who {
        Who::Agent(id) => Ok(id),
        Who::Operator => Err(RpcError::forbidden(
            "this operation must be called by an agent (through its MCP tools or `crew ctl`)",
        )),
    }
}

impl Daemon {
    pub(crate) fn is_orchestrator(&self, id: &str) -> bool {
        self.model().agent(id).is_some_and(|a| a.orchestrator)
    }

    /// Operator, or the orchestrator agent.
    fn privileged(&self, who: &Who) -> Result<(), RpcError> {
        match who {
            Who::Operator => Ok(()),
            Who::Agent(id) if self.is_orchestrator(id) => Ok(()),
            Who::Agent(_) => Err(RpcError::forbidden(
                "only the orchestrator or the operator may do this",
            )),
        }
    }

    /// Resolve which agent a call acts on. Agents can only act as themselves;
    /// the operator must name the agent.
    pub(crate) fn acting_agent(&self, who: &Who, named: Option<String>) -> Result<String, RpcError> {
        match (who, named) {
            (Who::Agent(me), None) => Ok(me.clone()),
            (Who::Agent(me), Some(other)) if *me == other => Ok(other),
            (Who::Agent(_), Some(_)) => Err(RpcError::forbidden("agents can only act as themselves")),
            (Who::Operator, Some(agent)) => {
                if self.model().agent(&agent).is_none() {
                    return Err(RpcError::not_found(format!("no agent '{agent}'")));
                }
                Ok(agent)
            }
            (Who::Operator, None) => Err(RpcError::invalid("specify which agent")),
        }
    }

    pub(crate) async fn dispatch(&self, conn: &mut Conn, call: Call) -> Reply {
        if let Call::Ping = call {
            return Ok(json!({ "pong": true }));
        }
        if let Call::Hello(hello) = call {
            if hello.protocol > PROTOCOL_VERSION {
                return Err(RpcError::invalid(format!(
                    "client speaks protocol {}, daemon speaks {PROTOCOL_VERSION}",
                    hello.protocol
                )));
            }
            let who = self.authenticate(hello.role, hello.agent_id, hello.token)?;
            let info = HelloInfo {
                role: hello.role,
                agent_id: match &who {
                    Who::Agent(id) => Some(id.clone()),
                    Who::Operator => None,
                },
                project: self.session_info().project,
                version: VERSION.to_string(),
                protocol: PROTOCOL_VERSION,
                pid: std::process::id(),
            };
            debug_assert!(matches!(
                (&who, hello.role),
                (Who::Operator, Role::Operator) | (Who::Agent(_), Role::Agent)
            ));
            conn.who = Some(who);
            return reply(info);
        }
        let who = conn
            .who
            .clone()
            .ok_or_else(|| RpcError::unauthorized("say hello first"))?;

        match call {
            Call::Ping | Call::Hello(_) => unreachable!("handled above"),

            Call::Snapshot { messages, events } => {
                operator_only(&who)?;
                reply(self.snapshot(messages.unwrap_or(200).min(2000), events.unwrap_or(300).min(5000))?)
            }
            Call::Subscribe => {
                operator_only(&who)?;
                conn.subscribe_after_reply = true;
                Ok(json!({ "subscribed": true }))
            }
            Call::LoadTeam { config, herdr_socket } => {
                operator_only(&who)?;
                self.load_team(*config, herdr_socket).await?;
                reply(self.session_info())
            }
            Call::Shutdown { stop_agents } => {
                operator_only(&who)?;
                self.shutdown(stop_agents).await;
                Ok(json!({ "stopping": true }))
            }

            Call::AgentsList => reply(self.model().agents.clone()),
            Call::AgentAdd { spec } => {
                self.privileged(&who)?;
                // The orchestrator's additions once the team is founded are
                // temporary; the operator's are the operator's to remove.
                let temporary = matches!(&who, Who::Agent(_)) && self.model().session.founded_at.is_some();
                reply(self.add_agent(spec, temporary).await?)
            }
            Call::AgentKill { id } => {
                operator_only(&who)?;
                self.kill_agent(&id).await?;
                reply(self.agent(&id))
            }
            Call::AgentRemove { id } => {
                self.privileged(&who)?;
                self.remove_agent(&id, "").await?;
                Ok(json!({ "removed": id }))
            }
            Call::AgentRestart { id } => {
                operator_only(&who)?;
                self.restart_agent(&id).await?;
                reply(self.agent(&id))
            }
            Call::AgentSetModel { id, model, runtime, note } => {
                self.privileged(&who)?;
                let by_operator = matches!(who, Who::Operator);
                self.set_model(who.sender(), by_operator, &id, &model, runtime, note).await?;
                reply(self.agent(&id))
            }
            Call::UsageStatus => {
                self.privileged(&who)?;
                self.refresh_claude_usage_now().await;
                Ok(json!({ "text": self.usage_report().await }))
            }
            Call::ModelsList => {
                operator_only(&who)?;
                let models = tokio::task::spawn_blocking(|| {
                    crate::models::Catalog::load().choices(
                        &crate::models::Availability::detect(),
                        crate::agents::pi::list_models(),
                    )
                })
                .await
                .map_err(internal)?;
                reply(crew_core::rpc::ModelsResult { models })
            }
            Call::AgentCompact { id, keep } => reply(self.compact_agent(&who, id, keep).await?),
            Call::AgentPause { id } => {
                operator_only(&who)?;
                self.pause_agent(&id, true).await?;
                reply(self.agent(&id))
            }
            Call::Resume { agents, restart } => {
                operator_only(&who)?;
                let resumed = self.resume_limited(&agents, restart, false).await?;
                Ok(json!({ "resumed": resumed }))
            }
            Call::AgentResume { id } => {
                operator_only(&who)?;
                self.pause_agent(&id, false).await?;
                reply(self.agent(&id))
            }
            Call::AgentOutput {
                id,
                lines,
                screen,
                ansi,
            } => {
                operator_only(&who)?;
                if self.model().agent(&id).is_none() {
                    return Err(RpcError::not_found(format!("no agent '{id}'")));
                }
                let procs = self.procs()?;
                let lines = lines.unwrap_or(60).clamp(1, 2000);
                let text = if ansi {
                    procs.styled(&id, lines).await
                } else if screen {
                    procs.visible(&id, lines).await
                } else {
                    procs.screen(&id, lines).await
                }
                .unwrap_or_default();
                reply(OutputResult {
                    text,
                    alive: procs.is_alive(&id),
                })
            }
            Call::AgentTranscript { id, limit } => {
                operator_only(&who)?;
                reply(self.agent_transcript(&id, limit.unwrap_or(400).clamp(1, 2000)).await?)
            }
            Call::AgentInput { id, text, submit } => {
                operator_only(&who)?;
                let procs = self.procs()?;
                let text = text.unwrap_or_default();
                if submit {
                    procs.submit_prompt(&id, &text).await.map_err(internal)?;
                } else {
                    procs.write_raw(&id, text.as_bytes()).await.map_err(internal)?;
                }
                self.emit(
                    NewEvent::new("daemon", "agent.input", format!("operator typed into {id}"))
                        .agent(&id),
                );
                Ok(json!({ "ok": true }))
            }
            Call::AgentFocus { id } => {
                operator_only(&who)?;
                self.procs()?.focus(&id).await.map_err(internal)?;
                Ok(json!({ "ok": true }))
            }
            Call::AgentSpawn {
                id,
                role,
                task,
                runtime,
                model,
            } => {
                let parent = agent_only(&who)?.to_string();
                reply(self.spawn_subagent(&parent, id, role, task, runtime, model).await?)
            }

            Call::Send(params) => reply(self.handle_send(&who, params).await?),
            Call::Answer { id, body } => {
                operator_only(&who)?;
                reply(self.answer_question(&id, &body).await?)
            }
            Call::Inbox {
                agent,
                unread_only,
                ack,
                limit,
            } => {
                let agent = self.acting_agent(&who, agent)?;
                let messages = self
                    .bus
                    .receive(&agent, unread_only, limit.unwrap_or(50).clamp(1, 500))?;
                // Only the agent itself consumes its inbox; the operator just looks.
                if ack && matches!(who, Who::Agent(_)) && !messages.is_empty() {
                    let ids: Vec<String> = messages.iter().map(|m| m.message.id.clone()).collect();
                    self.bus.mark_delivered(&agent, &ids)?;
                    self.bus.acknowledge(&agent, Some(&ids))?;
                    self.refresh_counts(std::slice::from_ref(&agent));
                }
                reply(InboxResult { messages })
            }
            Call::Ack { ids } => {
                let agent = agent_only(&who)?.to_string();
                let n = self.bus.acknowledge(&agent, Some(&ids))?;
                self.refresh_counts(std::slice::from_ref(&agent));
                Ok(json!({ "acknowledged": n }))
            }
            Call::Listen => {
                let agent = agent_only(&who)?.to_string();
                self.listener_joined(&agent, conn.id, conn.tx.clone());
                conn.listening = Some(agent);
                Ok(json!({ "listening": true }))
            }
            Call::Delivered { ids } => {
                let agent = agent_only(&who)?.to_string();
                self.confirm_delivery(&agent, &ids);
                Ok(json!({ "ok": true }))
            }
            Call::Credential => {
                // An agent may fetch its own key and nobody else's: the
                // caller is the authenticated agent, never a parameter.
                let agent = agent_only(&who)?.to_string();
                let (runtime, parent) = {
                    let model = self.model();
                    let a = model
                        .agent(&agent)
                        .ok_or_else(|| RpcError::not_found(format!("no agent '{agent}'")))?;
                    (a.runtime, a.parent.clone())
                };
                let endpoint = crate::endpoints::Layers::load(&self.workspace)
                    .resolve(runtime, &agent, parent.as_deref())
                    .map_err(|e| RpcError::invalid(format!("{e:#}")))?;
                let key = endpoint
                    .api_key
                    .ok_or_else(|| RpcError::not_found("the crew config sets no api key for this agent"))?;
                Ok(json!({ "value": key.expose() }))
            }
            Call::History { agent, limit } => {
                let limit = limit.unwrap_or(100).clamp(1, 2000);
                let scope = match &who {
                    // Agents only see traffic they took part in.
                    Who::Agent(me) => Some(me.clone()),
                    Who::Operator => agent,
                };
                reply(self.bus.history(scope.as_deref(), limit)?)
            }
            Call::MessageGet { id } => reply(self.message_by_id(&who, &id)?),

            Call::StatusUpdate { status, note } => {
                let agent = agent_only(&who)?.to_string();
                self.self_status(&agent, status, note)?;
                reply(self.agent(&agent))
            }
            Call::Decline { reason } => {
                let agent = agent_only(&who)?.to_string();
                self.declined(&agent, reason)?;
                Ok(json!({ "declined": true }))
            }
            Call::Progress {
                percent,
                message,
                task_id,
            } => {
                let agent = agent_only(&who)?.to_string();
                self.report_progress(&agent, percent, message, task_id)?;
                reply(self.agent(&agent))
            }

            Call::TasksList => reply(self.model().tasks.clone()),
            Call::TaskGet { id } => reply(self.task_card(&who, id)?),
            Call::TaskCreate { spec } => {
                self.privileged(&who)?;
                reply(self.create_task(spec)?)
            }
            Call::TaskEdit { id, title, description } => {
                self.privileged(&who)?;
                reply(self.edit_task(&who, &id, title, description).await?)
            }
            Call::TaskAssign { id, agent } => {
                self.privileged(&who)?;
                reply(self.assign_task(&id, &agent)?)
            }
            Call::TaskComplete {
                task_id,
                result,
                artifacts,
            } => reply(self.complete_task(&who, task_id, result, artifacts).await?),
            Call::TaskFail { task_id, reason } => reply(self.fail_task_by(&who, task_id, reason).await?),
            Call::TaskRetry { id } => {
                self.privileged(&who)?;
                reply(self.retry_task(&id)?)
            }

            Call::ReviewRequest {
                agent_id,
                artifact,
                task_id,
                note,
            } => reply(self.request_review(&who, agent_id, artifact, task_id, note).await?),
            Call::ReviewSubmit {
                task_id,
                approve,
                comments,
            } => reply(self.submit_review(&who, &task_id, approve, comments).await?),
            Call::SessionComplete { summary, decisions } => {
                self.privileged(&who)?;
                let decisions = self.check_decisions(&who, decisions)?;
                self.model().session.summary = Some(summary.clone());
                self.save_session();
                self.emit(NewEvent::new("orchestrator", "session.summary", summary));
                let asked = self.ask_decisions(&who, decisions).await?;
                Ok(json!({ "ok": true, "decisions": asked }))
            }

            Call::GitCommit { message, agent } => {
                let agent = self.acting_agent(&who, agent)?;
                reply(self.git_commit(&agent, message).await?)
            }
            Call::GitDiff { against, agent } => {
                let agent = self.acting_agent(&who, agent)?;
                reply(self.git_diff(&agent, against).await?)
            }
            Call::GitCherryPick { commit, agent } => {
                let agent = self.acting_agent(&who, agent)?;
                reply(self.git_cherry_pick(&agent, commit).await?)
            }
            Call::GitMerge { from_agent, agent } => {
                let agent = self.acting_agent(&who, agent)?;
                reply(self.git_merge(&agent, from_agent).await?)
            }
            Call::GitStatus { agent } => {
                let agent = self.acting_agent(&who, agent)?;
                reply(self.git_status(&agent).await?)
            }
            Call::Integrate => {
                self.privileged(&who)?;
                reply(self.integrate().await?)
            }

            Call::Hook { event, payload } => {
                let agent = agent_only(&who)?.to_string();
                self.on_hook(&agent, &event, &payload).await;
                Ok(json!({ "ok": true }))
            }
            Call::Events { agent, limit } => {
                let scope = match &who {
                    Who::Agent(me) => Some(me.clone()),
                    Who::Operator => agent,
                };
                reply(
                    self.store
                        .recent_events(scope.as_deref(), limit.unwrap_or(200).clamp(1, 5000))
                        .map_err(internal)?,
                )
            }

            Call::Attach { agent, rows, cols } => {
                operator_only(&who)?;
                self.attach(conn, &agent, rows, cols).await?;
                Ok(json!({ "attached": agent }))
            }
            Call::AttachInput { data } => {
                operator_only(&who)?;
                let agent = conn
                    .attached
                    .clone()
                    .ok_or_else(|| RpcError::invalid("not attached"))?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|e| RpcError::invalid(format!("bad base64: {e}")))?;
                self.procs()?.write_raw(&agent, &bytes).await.map_err(internal)?;
                Ok(Value::Null)
            }
            Call::AttachResize { rows, cols } => {
                operator_only(&who)?;
                let agent = conn
                    .attached
                    .clone()
                    .ok_or_else(|| RpcError::invalid("not attached"))?;
                self.procs()?.resize(&agent, rows, cols).await.map_err(internal)?;
                Ok(Value::Null)
            }
        }
    }

    pub(crate) async fn handle_send(&self, who: &Who, mut params: SendParams) -> Result<SendResult, RpcError> {
        if params.to == OPERATOR_ID && params.kind == MessageType::Question {
            self.check_operator_question(who, &mut params)?;
        }
        let outcome = self
            .send_message(NewMessage {
                id: params.id,
                from: who.sender().to_string(),
                to: params.to,
                kind: params.kind,
                body: params.body,
                correlation_id: params.correlation_id,
                reply_to: params.reply_to,
                metadata: params.metadata,
            })
            .await?;
        if outcome.message.is_operator_question() && !outcome.duplicate {
            self.question_asked(&outcome.message);
        }
        Ok(SendResult {
            message: outcome.message,
            duplicate: outcome.duplicate,
            recipients: outcome.recipients,
        })
    }

    /// The single entry point for putting a message on the bus: resolves
    /// recipients, persists, publishes, and triggers delivery into each
    /// recipient's session. Direct messages go straight to their recipient —
    /// the orchestrator is never a hop.
    pub(crate) async fn send_message(&self, new: NewMessage) -> Result<SendOutcome, RpcError> {
        let (recipients, notify_p2p) = {
            let model = self.model();
            let recipients: Vec<String> = if new.to == BROADCAST {
                model
                    .agents
                    .iter()
                    .filter(|a| a.id != new.from)
                    .map(|a| a.id.clone())
                    .collect()
            } else if new.to == OPERATOR_ID || model.agent(&new.to).is_some() {
                vec![new.to.clone()]
            } else {
                return Err(RpcError::not_found(format!(
                    "no agent '{}' (agents: {})",
                    new.to,
                    model
                        .agents
                        .iter()
                        .map(|a| a.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            };
            if new.to == new.from {
                return Err(RpcError::invalid("an agent cannot message itself"));
            }
            let notify = model
                .session
                .team
                .as_ref()
                .is_some_and(|t| t.orchestrator.notify_p2p);
            (recipients, notify)
        };
        let outcome = self.bus.send(new, recipients)?;
        if outcome.duplicate {
            return Ok(outcome);
        }
        let message = &outcome.message;
        self.push(Push::Message(message.clone()));
        let arrow = if message.is_p2p() { "→" } else { "⇉" };
        self.emit(
            NewEvent::new(
                "bus",
                "message.sent",
                format!(
                    "{} {arrow} {} [{}] {}",
                    message.from,
                    message.to,
                    message.kind,
                    crew_core::util::truncate_chars(&crew_core::util::one_line(&message.body), 120)
                ),
            )
            .agent(&message.from)
            .data(json!({ "message_id": message.id, "to": message.to, "p2p": message.is_p2p() })),
        );
        let mut touched = outcome.recipients.clone();
        touched.push(message.from.clone());
        self.refresh_counts(&touched);
        self.update_agent(&message.from, |a| a.last_activity_at = message.timestamp);
        for recipient in &outcome.recipients {
            self.request_delivery(recipient);
        }
        // Optional courtesy note: the orchestrator learns that two workers
        // talked, never what they said, and never has to forward anything.
        if notify_p2p && message.is_p2p() {
            let note = format!(
                "{} and {} exchanged a direct message (type {}).",
                message.from, message.to, message.kind
            );
            self.notify_orchestrator(&note).await;
        }
        Ok(outcome)
    }

    /// Send a status line to the orchestrator agent, if there is one.
    pub(crate) async fn notify_orchestrator(&self, text: &str) {
        let Some(orchestrator) = self.model().orchestrator().map(|a| a.id.clone()) else {
            return;
        };
        let message = NewMessage::new("scheduler", &orchestrator, MessageType::Status, text);
        if let Err(e) = self.send_message_quiet(message).await {
            tracing::warn!("notifying the orchestrator failed: {e}");
        }
    }

    /// `send_message` without the p2p courtesy note (used for scheduler mail).
    pub(crate) async fn send_message_quiet(&self, new: NewMessage) -> Result<(), RpcError> {
        self.send_notice(new).await.map(|_| ())
    }

    /// `send_message_quiet`, saying the id of the message sent.
    pub(crate) async fn send_notice(&self, new: NewMessage) -> Result<String, RpcError> {
        let recipients = vec![new.to.clone()];
        let outcome = self.bus.send(new, recipients)?;
        let sent = outcome.message.id.clone();
        if outcome.duplicate {
            return Ok(sent);
        }
        self.push(Push::Message(outcome.message.clone()));
        self.refresh_counts(&outcome.recipients);
        for recipient in &outcome.recipients {
            self.request_delivery(recipient);
        }
        Ok(sent)
    }

    fn self_status(&self, agent: &str, status: AgentStatus, note: Option<String>) -> Result<(), RpcError> {
        if !matches!(
            status,
            AgentStatus::Working | AgentStatus::Waiting | AgentStatus::Blocked | AgentStatus::Idle
        ) {
            return Err(RpcError::invalid(
                "agents may set working, waiting, blocked or idle (finish a task with task_complete)",
            ));
        }
        let note = note.map(|n| crew_core::util::truncate_chars(n.trim(), 300));
        {
            let mut model = self.model();
            let rt = model.rt(agent);
            rt.self_status = match status {
                AgentStatus::Waiting | AgentStatus::Blocked => Some(status),
                _ => None,
            };
        }
        self.update_agent(agent, |a| {
            a.status = status;
            a.block_kind = (status == AgentStatus::Blocked).then(|| "self".to_string());
            if let Some(note) = &note {
                a.activity = note.clone();
            }
            a.last_activity_at = crew_core::util::now_ms();
        });
        self.mirror_status_to_task(agent, status, note.clone());
        self.emit(
            NewEvent::new(
                "agent",
                "agent.self_status",
                format!(
                    "{agent} reports {status}{}",
                    note.map(|n| format!(": {n}")).unwrap_or_default()
                ),
            )
            .agent(agent),
        );
        self.replan();
        Ok(())
    }

    async fn attach(&self, conn: &mut Conn, agent: &str, rows: u16, cols: u16) -> Result<(), RpcError> {
        let procs = self.procs()?;
        // Fit the pty to the client before taking the snapshot.
        procs.resize(agent, rows, cols).await.map_err(internal)?;
        let attachment = procs.attach(agent).map_err(internal)?;
        if let Some(task) = conn.stream_task.take() {
            task.abort();
        }
        conn.attached = Some(agent.to_string());
        let tx = conn.tx.clone();
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let snapshot = encode(&attachment.snapshot);
        let mut output = attachment.output;
        conn.stream_task = Some(tokio::spawn(async move {
            if tx
                .send(ServerFrame::push(Push::Pty { data: snapshot }))
                .await
                .is_err()
            {
                return;
            }
            loop {
                match output.recv().await {
                    Ok(chunk) => {
                        let data = base64::engine::general_purpose::STANDARD.encode(strip_queries(&chunk));
                        if tx.send(ServerFrame::push(Push::Pty { data })).await.is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        let _ = tx.send(ServerFrame::push(Push::PtyClosed)).await;
                        return;
                    }
                }
            }
        }));
        self.emit(
            NewEvent::new("daemon", "agent.attached", format!("operator attached to {agent}"))
                .agent(agent),
        );
        Ok(())
    }

    pub(crate) async fn shutdown(&self, stop_agents: bool) {
        if stop_agents {
            self.stopping_team.store(true, std::sync::atomic::Ordering::SeqCst);
            let ids: Vec<String> = self.model().agents.iter().map(|a| a.id.clone()).collect();
            for id in &ids {
                if let Err(e) = self.kill_agent(id).await {
                    tracing::debug!("stopping {id}: {}", e.message);
                }
            }
            // An agent that was still starting may have been marked as such
            // after it was stopped. Left that way it would look, to the next
            // daemon, like a team that is still coming up.
            for id in &ids {
                self.update_agent(id, |a| {
                    if !a.status.is_terminal() {
                        a.status = AgentStatus::Stopped;
                        a.pid = None;
                        a.activity = "stopped".into();
                    }
                });
            }
        }
        let _ = self.shutdown.send(true);
    }

    /// Git outcome → reply value, logging conflicts as events.
    pub(crate) fn report_git(&self, agent: &str, outcome: &GitOutcome) {
        if let Some(conflict) = &outcome.conflict {
            self.emit(
                NewEvent::new(
                    "git",
                    "git.conflict",
                    format!(
                        "{agent}: {} conflict in {}",
                        conflict.operation,
                        conflict.files.join(", ")
                    ),
                )
                .agent(agent)
                .warn()
                .data(json!({ "files": conflict.files, "operation": conflict.operation })),
            );
        }
    }
}
