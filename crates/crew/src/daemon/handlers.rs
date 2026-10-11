//! Request dispatch: one arm per protocol call, with authorization.

use base64::Engine;
use crew_core::rpc::{
    Call, Hello, HelloInfo, InboxResult, OutputResult, Push, Role, RpcError, SendParams,
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
        Who::Agent(_) => Err(RpcError::forbidden(
            "this operation is reserved for the operator",
        )),
    }
}

/// Calls a web session may not make: making codes and ending sessions stay
/// with whoever holds the operator token file, and the browser does not
/// launch teams.
fn not_on_the_web(conn: &Conn, method: &str) -> Result<(), RpcError> {
    if conn.web {
        return Err(RpcError::forbidden(format!(
            "{method} is not available to the web dashboard"
        )));
    }
    Ok(())
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
    /// An explicit id-scoped inbox read returns the full body. Operator and
    /// outbox views remain observations of someone else's unread state.
    pub(crate) fn read_message_by_id(
        &self,
        who: &Who,
        id: &str,
    ) -> Result<crew_core::Message, RpcError> {
        let message = self.message_by_id(who, id)?;
        if let Who::Agent(agent) = who {
            if self
                .bus
                .get(id)?
                .is_some_and(|(_, recipients)| recipients.contains(agent))
            {
                self.acknowledge_received(agent, &[id.to_string()], true)?;
            }
        }
        Ok(message)
    }

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
    pub(crate) fn acting_agent(
        &self,
        who: &Who,
        named: Option<String>,
    ) -> Result<String, RpcError> {
        match (who, named) {
            (Who::Agent(me), None) => Ok(me.clone()),
            (Who::Agent(me), Some(other)) if *me == other => Ok(other),
            (Who::Agent(_), Some(_)) => {
                Err(RpcError::forbidden("agents can only act as themselves"))
            }
            (Who::Operator, Some(agent)) => {
                if self.model().agent(&agent).is_none() {
                    return Err(RpcError::not_found(format!("no agent '{agent}'")));
                }
                Ok(agent)
            }
            (Who::Operator, None) => Err(RpcError::invalid("specify which agent")),
        }
    }

    /// An agent's crew tools said `hello` in another protocol: they run
    /// another build of crew than this daemon (notes/binary-pinning.md B8).
    /// Said as a warn event, at most once a minute per agent, so the
    /// operator learns of it even when the agent says nothing; only for a
    /// hello that carries the agent's own token.
    fn protocol_refused(&self, hello: &Hello) {
        const EVERY_MS: i64 = 60_000;
        if hello.role != Role::Agent {
            return;
        }
        let Ok(Who::Agent(id)) =
            self.authenticate(hello.role, hello.agent_id.clone(), hello.token.clone())
        else {
            return;
        };
        let now = crew_core::util::now_ms();
        {
            let mut model = self.model();
            let rt = model.rt(&id);
            if rt.protocol_warned_at != 0 && now - rt.protocol_warned_at < EVERY_MS {
                return;
            }
            rt.protocol_warned_at = now;
        }
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.protocol_mismatch",
                format!(
                    "{id}'s crew tools speak protocol {}, this daemon speaks \
{PROTOCOL_VERSION}: restart the agent",
                    hello.protocol
                ),
            )
            .agent(&id)
            .warn()
            .data(json!({ "client": hello.client, "protocol": hello.protocol })),
        );
    }

    pub(crate) async fn dispatch(&self, conn: &mut Conn, call: Call) -> Reply {
        let call_name = call.method();
        if let Call::Ping = call {
            return Ok(json!({ "pong": true }));
        }
        // The dashboard server's, since it serves the dashboard.
        if let Call::WebPair { .. } = call {
            return Err(RpcError::invalid(
                "web_pair is served by the dashboard server: open the address `crew web` prints",
            ));
        }
        if let Call::Hello(hello) = call {
            // A connection is who it first said it was: an agent's code must
            // not turn its own connection into the operator's (B3a). A failed
            // first hello sets nothing, so it can be retried.
            if let Some(who) = &conn.who {
                return Err(RpcError::forbidden(format!(
                    "this connection is already authenticated as {}; open a new connection to \
change identity",
                    who.label()
                )));
            }
            // Exactly this version: an older client misreads newer frames as
            // surely as a newer one. The code lets clients say "restart".
            if hello.protocol != PROTOCOL_VERSION {
                self.protocol_refused(&hello);
                return Err(RpcError::new(
                    "protocol",
                    format!(
                        "client speaks protocol {}, daemon speaks {PROTOCOL_VERSION}",
                        hello.protocol
                    ),
                )
                .with_data(json!({
                    "version": VERSION,
                    "protocol": PROTOCOL_VERSION,
                    "workspace": self.workspace.to_string_lossy(),
                })));
            }
            let who = self.authenticate(hello.role, hello.agent_id, hello.token.clone())?;
            let mut feature_context = super::features::ConnCtx::new(
                crew_core::rpc::features::Id::new(uuid::Uuid::new_v4().to_string())
                    .map_err(RpcError::invalid)?,
                self.run_context()?,
                match &who {
                    Who::Operator => super::features::Caller::Operator,
                    Who::Agent(id) => super::features::Caller::Agent { id: id.clone() },
                },
            );
            if let Who::Agent(id) = &who {
                let model = self.model();
                if model.agent(id).is_none()
                    || !model.tokens.get(id).is_some_and(|token| {
                        hello
                            .token
                            .as_ref()
                            .is_some_and(|given| crew_core::util::ct_eq(token, given))
                    })
                {
                    return Err(RpcError::unauthorized("unknown agent or bad token"));
                }
                feature_context.member_incarnation = self
                    .feature_db
                    .incarnation(&feature_context.run.workspace_run_id, id)?;
            }
            // The dashboard server's mark on a browser's connection: it only
            // takes away (W7).
            conn.web = hello.web;
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
                web_proof: None,
            };
            debug_assert!(matches!(
                (&who, hello.role),
                (Who::Operator, Role::Operator) | (Who::Agent(_), Role::Agent)
            ));
            conn.who = Some(who);
            self.feature_services
                .connection_opened(feature_context.clone());
            conn.feature_context = Some(feature_context);
            return reply(info);
        }
        let who = conn
            .who
            .clone()
            .ok_or_else(|| RpcError::unauthorized("say hello first"))?;

        if let Call::FeaturesNegotiate(request) = call {
            return self.negotiate_features(conn, request);
        }
        if crew_core::rpc::features::feature_method(call.method()).is_some() {
            return self.dispatch_feature(conn, call).await;
        }
        // The style's cut of a member's crew tools holds here, whatever
        // asked (its MCP server, `crew ctl`).
        self.check_crew_tool(&who, &call)?;

        match call {
            Call::Ping | Call::Hello(_) => unreachable!("handled above"),

            Call::Snapshot { messages, events } => {
                conn.representation_started = true;
                operator_only(&who)?;
                if conn.subscribed {
                    // A barrier in this connection's push stream: the
                    // marker first, then the old forwarder is gone, then the
                    // capture. Pushes before the reply are older than it;
                    // every push sent after the marker follows it.
                    let marker = self.push.subscribe();
                    self.stop_stream(conn).await;
                    conn.subscribe_after_reply = Some(marker);
                }
                // No feature is negotiated in P1. P2 chooses the connection's view.
                reply(self.feature_snapshot(
                    conn,
                    messages.unwrap_or(200).min(2000),
                    events.unwrap_or(300).min(5000),
                )?)
            }
            Call::Subscribe => {
                conn.representation_started = true;
                operator_only(&who)?;
                conn.subscribe_after_reply = Some(self.push.subscribe());
                Ok(json!({ "subscribed": true }))
            }
            Call::LoadTeam {
                config,
                herdr_socket,
            } => {
                operator_only(&who)?;
                not_on_the_web(conn, "load_team")?;
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
                // The orchestrator chooses bundles by name; only the team file
                // defines what they hold (notes/toolboxes.md, Orchestrator
                // tools). The operator's are checked against the team in add_agent.
                if matches!(&who, Who::Agent(_)) {
                    self.check_bundle_selection(&spec.toolbox)?;
                }
                // The orchestrator's additions once the team is founded are
                // temporary; the operator's are the operator's to remove.
                let temporary =
                    matches!(&who, Who::Agent(_)) && self.model().session.founded_at.is_some();
                reply(self.add_agent(*spec, temporary).await?)
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
            Call::AgentRestartFresh { id } => {
                operator_only(&who)?;
                self.restart_agent_fresh(&id).await?;
                reply(self.agent(&id))
            }
            Call::AgentSetModel {
                id,
                model,
                runtime,
                note,
            } => {
                self.privileged(&who)?;
                let by_operator = matches!(who, Who::Operator);
                self.set_model(who.sender(), by_operator, &id, &model, runtime, note)
                    .await?;
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
            Call::AgentClear { id, note } => reply(self.clear_agent(&who, id, note).await?),
            Call::AgentPause { id } => {
                operator_only(&who)?;
                self.pause_agent(&id, true).await?;
                reply(self.agent(&id))
            }
            Call::Resume { agents, restart } => {
                operator_only(&who)?;
                let resumed = self.resume_limited(&agents, restart, false).await?;
                let restarted: Vec<&str> = resumed
                    .iter()
                    .filter(|(_, restarted)| *restarted)
                    .map(|(id, _)| id.as_str())
                    .collect();
                let resumed: Vec<&str> = resumed.iter().map(|(id, _)| id.as_str()).collect();
                Ok(json!({ "resumed": resumed, "restarted": restarted }))
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
                reply(
                    self.agent_transcript(&id, limit.unwrap_or(400).clamp(1, 2000))
                        .await?,
                )
            }
            Call::AgentInput { id, text, submit } => {
                operator_only(&who)?;
                let procs = self.procs()?;
                let text = text.unwrap_or_default();
                if submit {
                    procs.submit_prompt(&id, &text).await.map_err(internal)?;
                } else {
                    procs
                        .write_raw(&id, text.as_bytes())
                        .await
                        .map_err(internal)?;
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
                toolbox,
            } => {
                let parent = agent_only(&who)?.to_string();
                reply(
                    self.spawn_subagent(&parent, id, role, task, runtime, model, toolbox)
                        .await?,
                )
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
                let messages =
                    self.bus
                        .receive(&agent, unread_only, limit.unwrap_or(50).clamp(1, 500))?;
                // Only the agent itself consumes its inbox; the operator just looks.
                if ack && matches!(who, Who::Agent(_)) && !messages.is_empty() {
                    let ids: Vec<String> = messages.iter().map(|m| m.message.id.clone()).collect();
                    self.acknowledge_received(&agent, &ids, true)?;
                }
                reply(InboxResult { messages })
            }
            Call::Ack { ids } => {
                let agent = agent_only(&who)?.to_string();
                let n = self.acknowledge_received(&agent, &ids, false)?;
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
            Call::ToolboxServer { name, preflight } => {
                // An agent reaches its own servers and nobody else's: the
                // caller is the authenticated agent, never a parameter.
                let agent = agent_only(&who)?.to_string();
                reply(self.toolbox_server_launch(&agent, &name, preflight)?)
            }
            Call::ToolboxReport { name, kind, detail } => {
                let agent = agent_only(&who)?.to_string();
                self.toolbox_report(&agent, &name, kind, &detail).await;
                Ok(json!({ "ok": true }))
            }
            Call::Toolbox { agent, runtime } => {
                self.privileged(&who)?;
                let agents: Vec<crew_core::Agent> = match &agent {
                    Some(id) => vec![self
                        .agent(id)
                        .ok_or_else(|| RpcError::not_found(format!("no agent '{id}'")))?],
                    None => self.model().agents.clone(),
                };
                let views: Result<Vec<_>, RpcError> = agents
                    .iter()
                    .map(|a| self.toolbox_view(a, runtime))
                    .collect();
                reply(views?)
            }
            Call::ToolboxCheck { config } => {
                operator_only(&who)?;
                reply(self.toolbox_check(&config)?)
            }
            Call::ToolboxReload { config } => {
                operator_only(&who)?;
                reply(self.toolbox_reload_call(&config)?)
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
                let key = endpoint.api_key.ok_or_else(|| {
                    RpcError::not_found("the crew config sets no api key for this agent")
                })?;
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
            Call::MessageGet { id } => reply(self.read_message_by_id(&who, &id)?),

            Call::StatusUpdate {
                status,
                note,
                task_id,
            } => {
                let agent = agent_only(&who)?.to_string();
                if status == AgentStatus::Working {
                    if let Some(task) = &task_id {
                        self.start_ready_task(task, &agent).await?;
                    }
                }
                self.self_status(&agent, status, note, task_id)?;
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
            Call::TaskEdit {
                id,
                title,
                description,
            } => {
                self.privileged(&who)?;
                reply(self.edit_task(&who, &id, title, description).await?)
            }
            Call::TaskAssign { id, agent } => {
                self.privileged(&who)?;
                reply(self.assign_task(&id, &agent)?)
            }
            Call::MemoryList => reply(self.memory_list()?),
            Call::MemoryGet { name } => reply(self.memory_get(&name)?),
            Call::MemorySearch { query, limit } => reply(self.memory_search(&query, limit)?),
            Call::MemoryAdd {
                name,
                description,
                kind,
                body,
                source,
            } => reply(self.memory_add(&who, name, description, kind, body, source)?),
            Call::MemoryForget { name } => {
                self.privileged(&who)?;
                reply(self.memory_forget(&who, &name)?)
            }
            Call::TaskComplete {
                task_id,
                result,
                artifacts,
            } => reply(self.complete_task(&who, task_id, result, artifacts).await?),
            Call::TaskFail { task_id, reason } => {
                reply(self.fail_task_by(&who, task_id, reason).await?)
            }
            Call::TaskRetry { id } => {
                self.privileged(&who)?;
                reply(self.retry_task(&id)?)
            }
            Call::WorkflowRun {
                phase,
                title,
                brief,
            } => {
                self.privileged(&who)?;
                reply(self.run_workflow(&phase, &title, &brief)?)
            }
            Call::WorkflowSlices { phase, slices } => {
                self.workflow_caller(&who, Some(&phase))?;
                reply(self.declare_slices(&phase, slices)?)
            }
            Call::WorkflowPhases { phases } => {
                self.workflow_caller(&who, None)?;
                reply(self.queue_phases(phases)?)
            }

            Call::ReviewRequest {
                agent_id,
                artifact,
                task_id,
                note,
            } => reply(
                self.request_review(&who, agent_id, artifact, task_id, note)
                    .await?,
            ),
            Call::ReviewSubmit {
                task_id,
                approve,
                comments,
            } => reply(
                self.submit_review(&who, &task_id, approve, comments)
                    .await?,
            ),
            Call::SessionComplete { summary, decisions } => {
                self.privileged(&who)?;
                let decisions = self.check_decisions(&who, decisions)?;
                self.model().session.summary = Some(summary.clone());
                self.save_session();
                // The next session reads how this one ended.
                self.remember_session(&summary);
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

            Call::Hook {
                event,
                payload,
                launch,
            } => {
                let agent = agent_only(&who)?.to_string();
                if super::delivery::native_hook(&event) {
                    return self
                        .native_delivery_hook(&agent, launch.as_deref(), &event, &payload)
                        .await;
                }
                self.on_hook(&agent, launch.as_deref(), &event, &payload)
                    .await;
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
                self.procs()?
                    .write_raw(&agent, &bytes)
                    .await
                    .map_err(internal)?;
                Ok(Value::Null)
            }
            Call::AttachResize { rows, cols } => {
                operator_only(&who)?;
                let agent = conn
                    .attached
                    .clone()
                    .ok_or_else(|| RpcError::invalid("not attached"))?;
                self.procs()?
                    .resize(&agent, rows, cols)
                    .await
                    .map_err(internal)?;
                Ok(Value::Null)
            }

            // The dashboard is the dashboard server's (notes/web-multi.md
            // §multi-migration).
            Call::WebOpen { .. } => Err(RpcError::invalid(
                "web_open is served by the dashboard server: run `crew web`",
            )),
            Call::WebClose { .. } => Err(RpcError::invalid(
                "web_close is served by the dashboard server: run `crew web --stop`",
            )),
            Call::WebPair { .. } => unreachable!("handled above"),
            Call::WebPrefs { set } => {
                operator_only(&who)?;
                self.web_prefs(set)
            }
            Call::Workspaces | Call::WebRescan => Err(RpcError::invalid(format!(
                "{} is the dashboard server's: run `crew web`",
                call_name
            ))),
            Call::Summary { watch } => {
                operator_only(&who)?;
                reply(self.summary(conn, watch).await)
            }
            Call::WebAttach { url } => {
                operator_only(&who)?;
                not_on_the_web(conn, "web_attach")?;
                self.web_attach(conn, url)?;
                Ok(json!({ "attached": true }))
            }
            // P1 defines the wire vocabulary without enabling any backend.
            Call::FeaturesNegotiate(_)
            | Call::EpicsList(_)
            | Call::EpicGet(_)
            | Call::TasksQuery(_)
            | Call::TaskGetScoped(_)
            | Call::EpicCreate(_)
            | Call::EpicEdit(_)
            | Call::TaskCreateScoped(_)
            | Call::TaskRevise(_)
            | Call::EpicComplete(_)
            | Call::AsideMembers(_)
            | Call::AsideList(_)
            | Call::AsideGet(_)
            | Call::AsideAsk(_)
            | Call::AsideCancel(_)
            | Call::AsideRemove(_)
            | Call::AsideWatch(_)
            | Call::AsideUnwatch(_)
            | Call::DocsIndex(_)
            | Call::DocsRead(_)
            | Call::DocsAsset(_)
            | Call::DocsHistory(_)
            | Call::DocsDiff(_)
            | Call::DocsWatch(_)
            | Call::DocsUnwatch(_)
            | Call::UsageSummary(_)
            | Call::UsageSeries(_)
            | Call::UsageBreakdown(_)
            | Call::UsageObservations(_)
            | Call::UsageRuns(_)
            | Call::UsagePrices(_)
            | Call::UsageWatch(_)
            | Call::UsageUnwatch(_) => Err(RpcError::new(
                crew_core::rpc::features::UNSUPPORTED_FEATURE,
                "feature backend is not enabled",
            )),
            Call::WebEvent { kind, count } => {
                operator_only(&who)?;
                not_on_the_web(conn, "web_event")?;
                self.web_event(conn, kind, count)?;
                Ok(json!({ "ok": true }))
            }
        }
    }

    pub(crate) async fn handle_send(
        &self,
        who: &Who,
        mut params: SendParams,
    ) -> Result<SendResult, RpcError> {
        if let Who::Agent(id) = who {
            self.may_write(id, &params.to)
                .map_err(RpcError::forbidden)?;
        }
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
    pub(crate) async fn send_message(&self, mut new: NewMessage) -> Result<SendOutcome, RpcError> {
        // Replies can identify a parked card even after the sender has
        // started independent work. Preserve caller-supplied task scope.
        let from_task =
            crate::tasks::active_task(&self.model().tasks, &new.from).map(|t| t.id.clone());
        if let Some(task) = from_task {
            if new.metadata.is_null() {
                new.metadata = json!({});
            }
            if let Some(metadata) = new.metadata.as_object_mut() {
                metadata
                    .entry("from_task_id")
                    .or_insert_with(|| json!(task));
            }
        }
        let (recipients, notify_p2p) = self.recipients_of(&new)?;
        let outcome = self.bus.send(new, recipients)?;
        if outcome.duplicate {
            return Ok(outcome);
        }
        self.message_sent(&outcome, notify_p2p).await;
        Ok(outcome)
    }

    /// Whom `new` goes to, and whether the orchestrator hears of it as a
    /// direct message between workers (send_message).
    pub(crate) fn recipients_of(&self, new: &NewMessage) -> Result<(Vec<String>, bool), RpcError> {
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
        Ok((recipients, notify))
    }

    /// What follows a message the bus accepted (send_message): the push,
    /// the event, the counters, delivery to each recipient.
    pub(crate) async fn message_sent(&self, outcome: &SendOutcome, notify_p2p: bool) {
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
        for recipient in &outcome.recipients {
            if let Err(e) = self.record_task_wakeups(recipient, std::slice::from_ref(message)) {
                tracing::error!("{}", e.message);
            }
        }
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

    pub(crate) fn self_status(
        &self,
        agent: &str,
        status: AgentStatus,
        note: Option<String>,
        task_id: Option<String>,
    ) -> Result<(), RpcError> {
        if !matches!(
            status,
            AgentStatus::Working | AgentStatus::Waiting | AgentStatus::Blocked | AgentStatus::Idle
        ) {
            return Err(RpcError::invalid(
                "agents may set working, waiting, blocked or idle (finish a task with task_complete)",
            ));
        }
        let note = note.map(|n| crew_core::util::truncate_chars(n.trim(), 300));
        if status == AgentStatus::Waiting
            && self.acknowledges_review_waiting(agent, task_id.as_deref())
        {
            // Acknowledgement only. In particular, do not retain a Waiting
            // self-status that could hide changes requested after this check.
            return Ok(());
        }
        // The task first: a refused save is the caller's error, and nothing
        // else has changed then.
        let selected = self.mirror_status_to_task_id(agent, status, note.clone(), task_id)?;
        let is_current = selected.as_deref().is_none_or(|id| {
            crate::tasks::active_task(&self.model().tasks, agent).is_some_and(|t| t.id == id)
        });
        self.project_task(agent);
        if !is_current {
            self.replan();
            return Ok(());
        }
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

    async fn attach(
        &self,
        conn: &mut Conn,
        agent: &str,
        rows: u16,
        cols: u16,
    ) -> Result<(), RpcError> {
        let procs = self.procs()?;
        // Fit the pty to the client before taking the snapshot.
        procs.resize(agent, rows, cols).await.map_err(internal)?;
        let attachment = procs.attach(agent).map_err(internal)?;
        if let Some(task) = conn.stream_task.take() {
            task.abort();
        }
        conn.subscribed = false;
        conn.attached = Some(agent.to_string());
        let tx = conn.tx.clone();
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let snapshot = encode(&attachment.snapshot);
        let mut output = attachment.output;
        conn.stream_task = Some(conn.spawn(async move {
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
                        let data =
                            base64::engine::general_purpose::STANDARD.encode(strip_queries(&chunk));
                        if tx
                            .send(ServerFrame::push(Push::Pty { data }))
                            .await
                            .is_err()
                        {
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
            NewEvent::new(
                "daemon",
                "agent.attached",
                format!("operator attached to {agent}"),
            )
            .agent(agent),
        );
        Ok(())
    }

    pub(crate) async fn shutdown(&self, stop_agents: bool) {
        self.feature_services.shutdown().await;
        if stop_agents {
            // Not halfway through a load: its agents would be missed.
            let _lifecycle = self.team_lock.lock().await;
            self.stopping_team
                .store(true, std::sync::atomic::Ordering::SeqCst);
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
}

impl Daemon {
    /// The dashboard's preferences for this workspace: replaced by `set`
    /// first, when given. One JSON object, at most 16 KiB.
    fn web_prefs(&self, set: Option<Value>) -> Reply {
        use crate::web::{MAX_PREFS_BYTES, PREFS_KEY};
        if let Some(prefs) = set {
            if !prefs.is_object() {
                return Err(RpcError::invalid("web preferences are a JSON object"));
            }
            let text = serde_json::to_string(&prefs).map_err(internal)?;
            if text.len() > MAX_PREFS_BYTES {
                return Err(RpcError::invalid(format!(
                    "web preferences are at most {MAX_PREFS_BYTES} bytes"
                )));
            }
            self.store.set_meta(PREFS_KEY, &text).map_err(internal)?;
        }
        let prefs = self
            .store
            .get_meta(PREFS_KEY)
            .map_err(internal)?
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        Ok(json!({ "prefs": prefs }))
    }
}

#[cfg(test)]
mod review_waiting_tests {
    use super::*;
    use crew_core::{Agent, Task, TaskStatus};

    async fn fixture() -> (tempfile::TempDir, crate::daemon::Running) {
        let temp = tempfile::tempdir().unwrap();
        let running = crate::daemon::start(crate::daemon::DaemonOptions {
            workspace: temp.path().into(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        for id in ["owner", "reviewer"] {
            let agent: Agent = serde_json::from_value(json!({
                "id": id, "name": id, "role": id, "runtime": "claude", "status": "idle",
                "workdir": temp.path(), "backend": "pty", "last_activity_at": 0
            }))
            .unwrap();
            daemon.store.save_agent(&agent).unwrap();
            daemon.model().agents.push(agent);
        }
        let card: Task = serde_json::from_value(json!({
            "id": "review", "title": "Review card", "assigned_agent": "owner",
            "reviewer": "reviewer", "status": "review", "created_at": 0, "started_at": 1,
            "progress": 100, "result": "implementation ready", "note": "awaiting reviewer"
        }))
        .unwrap();
        daemon.store.save_task(&card).unwrap();
        daemon.model().tasks.push(card);
        (temp, running)
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_review_waiting_acknowledgements_change_no_state_or_prompts() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        let tasks = daemon.model().tasks.clone();
        let owner = serde_json::to_value(daemon.agent("owner")).unwrap();
        let reviewer = serde_json::to_value(daemon.agent("reviewer")).unwrap();
        let before = daemon.bus.history(None, 100).unwrap();
        for task_id in [None, Some("review".into()), Some("review".into()), None] {
            daemon
                .self_status(
                    "owner",
                    AgentStatus::Waiting,
                    Some("awaiting review".into()),
                    task_id,
                )
                .unwrap();
        }
        assert_eq!(daemon.model().tasks, tasks);
        assert_eq!(serde_json::to_value(daemon.agent("owner")).unwrap(), owner);
        assert_eq!(
            serde_json::to_value(daemon.agent("reviewer")).unwrap(),
            reviewer
        );
        assert_eq!(
            serde_json::to_value(daemon.bus.history(None, 100).unwrap()).unwrap(),
            serde_json::to_value(before).unwrap()
        );
        assert!(crate::tasks::executing_task(&daemon.model().tasks, "owner").is_none());
        assert_eq!(daemon.model().rt("owner").self_status, None);
        assert!(!daemon.model().rt("owner").parking_turn);
    }

    #[tokio::test(start_paused = true)]
    async fn explicit_review_waiting_preserves_another_card_and_idless_waiting_parks_it() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        let mut current = daemon.model().task("review").cloned().unwrap();
        current.id = "current".into();
        current.title = "Current card".into();
        current.status = TaskStatus::Working;
        daemon.store.save_task(&current).unwrap();
        daemon.model().tasks.push(current.clone());
        daemon.project_task("owner");
        daemon.update_agent("owner", |a| a.status = AgentStatus::Working);
        daemon.model().rt("owner").turn_active = true;
        let owner = serde_json::to_value(daemon.agent("owner")).unwrap();
        daemon
            .self_status("owner", AgentStatus::Waiting, None, Some("review".into()))
            .unwrap();
        assert_eq!(daemon.model().task("current"), Some(&current));
        assert_eq!(serde_json::to_value(daemon.agent("owner")).unwrap(), owner);
        assert_eq!(daemon.model().rt("owner").self_status, None);
        assert!(!daemon.model().rt("owner").parking_turn);
        daemon
            .self_status(
                "owner",
                AgentStatus::Waiting,
                Some("needs schema".into()),
                None,
            )
            .unwrap();
        assert_eq!(
            daemon.model().task("current").unwrap().status,
            TaskStatus::Waiting
        );
        assert_eq!(
            daemon.model().task("review").unwrap().status,
            TaskStatus::Review
        );
        assert!(daemon.model().rt("owner").parking_turn);
    }

    #[tokio::test(start_paused = true)]
    async fn acknowledgement_does_not_reclaim_review_or_suppress_requested_changes() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        daemon
            .self_status("owner", AgentStatus::Waiting, None, Some("review".into()))
            .unwrap();
        for status in [
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Idle,
        ] {
            assert!(daemon
                .self_status("owner", status, None, Some("review".into()))
                .is_err());
        }
        assert!(daemon
            .self_status(
                "reviewer",
                AgentStatus::Waiting,
                None,
                Some("review".into())
            )
            .is_err());
        assert!(daemon
            .self_status("owner", AgentStatus::Waiting, None, Some("missing".into()))
            .is_err());
        daemon
            .submit_review(
                &Who::Agent("reviewer".into()),
                "review",
                false,
                "Fix the missing check".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            daemon.model().task("review").unwrap().status,
            TaskStatus::Working
        );
        assert_eq!(daemon.model().rt("owner").self_status, None);
        assert_eq!(
            crate::tasks::executing_task(&daemon.model().tasks, "owner")
                .unwrap()
                .id,
            "review"
        );
        assert!(daemon
            .bus
            .history(Some("owner"), 100)
            .unwrap()
            .iter()
            .any(|m| m.kind == MessageType::ReviewResult && m.metadata["approved"] == false));
    }
}
