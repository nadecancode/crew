//! Questions for the human operator.
//!
//! Nobody watches an agent's terminal, so a question typed there is never
//! seen. Instead a question goes over the bus: a member asks the
//! orchestrator, and what the orchestrator cannot settle it puts to the
//! operator — a `question` message addressed to `operator`. Such a message
//! stays open (unacknowledged in the operator's inbox) until the operator
//! answers it from the dashboard or with `crew answer`; the answer is
//! delivered to the agent that asked and to the member it asked for.

use crew_core::rpc::{RpcError, SendParams, SendResult};
use crew_core::types::MessageType as Kind;
use crew_core::types::{MAX_OPTION_CHARS, MAX_QUESTION_OPTIONS};
use crew_core::util::{now_ms, one_line, truncate_chars};
use crew_core::{Message, MessageType, OPERATOR_ID};
use serde_json::{json, Value};

use super::server::Who;
use super::Daemon;
use crate::message_bus::NewMessage;
use crate::persistence::NewEvent;

/// How much of a question is quoted back with its answer.
const QUOTE_CHARS: usize = 400;
/// How many decisions a final report may leave for the operator.
const MAX_DECISIONS: usize = 12;

/// Shared report normalization; callers retain their own bounded report size.
pub(super) fn check_report_decisions(
    who: &Who,
    decisions: Vec<String>,
    max_decisions: usize,
) -> Result<Vec<String>, RpcError> {
    let mut kept: Vec<String> = Vec::new();
    for decision in decisions {
        let decision = decision.trim().to_string();
        if !decision.is_empty() && !kept.contains(&decision) {
            kept.push(decision);
        }
    }
    if kept.is_empty() {
        return Ok(kept);
    }
    if !matches!(who, Who::Agent(_)) {
        return Err(RpcError::invalid(
            "decisions for the operator come from an agent",
        ));
    }
    if kept.len() > max_decisions {
        return Err(RpcError::invalid(format!(
            "at most {max_decisions} decisions; put related ones into one question"
        )));
    }
    Ok(kept)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}

impl Daemon {
    /// Vet a question on its way to the operator, and tidy what comes with
    /// it. A member of a team that has an orchestrator asks the orchestrator
    /// first: it answers what it can and passes on the rest.
    pub(crate) fn check_operator_question(
        &self,
        who: &Who,
        params: &mut SendParams,
    ) -> Result<(), RpcError> {
        let Who::Agent(asker) = who else {
            return Err(RpcError::invalid(
                "the operator has nobody to put a question to but the agents",
            ));
        };
        let model = self.model();
        let is_orchestrator = model.agent(asker).is_some_and(|a| a.orchestrator);
        if let Some(orchestrator) = model.orchestrator().filter(|_| !is_orchestrator) {
            return Err(RpcError::forbidden(format!(
                "put this question to {id} first: agent_send(to: \"{id}\", type: \"question\"). \
It answers what it can and brings in the operator for what only a human can decide.",
                id = orchestrator.id
            )));
        }
        if params.body.trim().is_empty() {
            return Err(RpcError::invalid("the question is empty"));
        }

        let mut metadata = serde_json::Map::new();
        let options: Vec<String> = params
            .metadata
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|o| one_line(o).trim().to_string())
                    .filter(|o| !o.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if options.len() > MAX_QUESTION_OPTIONS {
            return Err(RpcError::invalid(format!(
                "at most {MAX_QUESTION_OPTIONS} options; the operator can always answer in their own words"
            )));
        }
        if let Some(long) = options
            .iter()
            .find(|o| o.chars().count() > MAX_OPTION_CHARS)
        {
            return Err(RpcError::invalid(format!(
                "an option is a short answer (at most {MAX_OPTION_CHARS} characters); put the \
explanation in the question: {}",
                truncate_chars(long, 60)
            )));
        }
        if !options.is_empty() {
            metadata.insert("options".into(), json!(options));
        }
        if let Some(member) = params.metadata.get("asked_by").and_then(Value::as_str) {
            if member != asker.as_str() {
                if model.agent(member).is_none() {
                    return Err(RpcError::not_found(format!(
                        "asked_by: no agent '{member}' on the team"
                    )));
                }
                metadata.insert("asked_by".into(), json!(member));
            }
        }
        params.metadata = Value::Object(metadata);
        Ok(())
    }

    /// Tidy the decisions a final report leaves for the operator, before
    /// anything of the report is recorded.
    pub(crate) fn check_decisions(
        &self,
        who: &Who,
        decisions: Vec<String>,
    ) -> Result<Vec<String>, RpcError> {
        check_report_decisions(who, decisions, MAX_DECISIONS)
    }

    /// Put each decision of a final report to the operator as a question of
    /// its own: asked, it shows on the dashboard until it is answered,
    /// where a line in the report is only read by chance. Returns how many
    /// were asked; a report that is sent again does not ask twice.
    pub(crate) async fn ask_decisions(
        &self,
        who: &Who,
        decisions: Vec<String>,
    ) -> Result<usize, RpcError> {
        if decisions.is_empty() {
            return Ok(0);
        }
        let asked_before: Vec<String> = self
            .store
            .inbox(OPERATOR_ID, false, 500)
            .map_err(internal)?
            .into_iter()
            .map(|entry| entry.message)
            .filter(|m| m.is_operator_question() && m.from == who.sender())
            .map(|m| m.body)
            .collect();
        let mut asked = 0;
        for decision in decisions {
            if asked_before.contains(&decision) {
                continue;
            }
            let question = SendParams {
                to: OPERATOR_ID.to_string(),
                kind: Kind::Question,
                body: decision,
                ..SendParams::default()
            };
            self.handle_send(who, question).await?;
            asked += 1;
        }
        Ok(asked)
    }

    /// Record that a question reached the operator.
    pub(crate) fn question_asked(&self, question: &Message) {
        self.emit(
            NewEvent::new(
                "bus",
                "question.asked",
                format!(
                    "{} asks the operator: {}",
                    question.asker(),
                    truncate_chars(&one_line(&question.body), 160)
                ),
            )
            .agent(&question.from)
            .warn()
            .data(json!({ "message_id": question.id })),
        );
    }

    /// Questions to the operator that are still unanswered, oldest first.
    pub(crate) fn open_questions(&self) -> Result<Vec<Message>, RpcError> {
        let unread = self.store.inbox(OPERATOR_ID, true, 500).map_err(internal)?;
        Ok(unread
            .into_iter()
            .map(|entry| entry.message)
            .filter(Message::is_operator_question)
            .collect())
    }

    /// The operator's answer to an open question: delivered to the agent
    /// that asked, and to the member whose question it passed on.
    pub(crate) async fn answer_question(
        &self,
        id: &str,
        body: &str,
    ) -> Result<SendResult, RpcError> {
        let body = body.trim();
        if body.is_empty() {
            return Err(RpcError::invalid("an answer needs some text"));
        }
        let Some(question) = self.open_questions()?.into_iter().find(|q| q.id == id) else {
            let known = self.store.get_message(id).map_err(internal)?;
            return Err(match known {
                Some(m) if m.is_operator_question() => {
                    RpcError::invalid(format!("question {id} was already answered"))
                }
                _ => RpcError::not_found(format!("no open question '{id}'")),
            });
        };
        let close = || {
            self.store
                .acknowledge(
                    OPERATOR_ID,
                    Some(std::slice::from_ref(&question.id)),
                    now_ms(),
                )
                .map_err(internal)
        };
        if self.model().agent(&question.from).is_none() {
            // Nobody is left to hear the answer; do not keep asking for one.
            close()?;
            return Err(RpcError::invalid(format!(
                "{} is no longer on the team; the question was closed",
                question.from
            )));
        }

        let quote = truncate_chars(&one_line(&question.body), QUOTE_CHARS);
        let outcome = self
            .send_message(NewMessage {
                id: None,
                from: OPERATOR_ID.to_string(),
                to: question.from.clone(),
                kind: MessageType::Answer,
                body: format!("Re: \"{quote}\"\n\n{body}"),
                correlation_id: question.correlation_id.clone(),
                reply_to: Some(question.id.clone()),
                metadata: Value::Null,
            })
            .await?;
        close()?;
        // The member who wanted to know hears it first-hand rather than
        // waiting for the orchestrator to pass it on.
        if let Some(member) = question
            .asked_by()
            .filter(|m| self.model().agent(m).is_some())
        {
            let copy = NewMessage::new(
                OPERATOR_ID,
                member,
                MessageType::Answer,
                format!(
                    "The operator answered a question {} raised for you.\nQuestion: \"{quote}\"\n\n{body}",
                    question.from
                ),
            );
            if let Err(e) = self.send_message(copy).await {
                tracing::warn!("copying an answer to {member} failed: {}", e.message);
            }
        }
        // What the operator decided is what a later session must know.
        self.remember_answer(&question, body);
        self.emit(
            NewEvent::new(
                "bus",
                "question.answered",
                format!(
                    "the operator answered {}: {}",
                    question.asker(),
                    truncate_chars(&one_line(body), 160)
                ),
            )
            .agent(&question.from)
            .data(json!({ "message_id": question.id, "answer_id": outcome.message.id })),
        );
        Ok(SendResult {
            message: outcome.message,
            duplicate: outcome.duplicate,
            recipients: outcome.recipients,
        })
    }
}
