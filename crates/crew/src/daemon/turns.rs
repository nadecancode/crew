//! Turns that end without the work being done because the model refused
//! it. The same work offered again is refused again, so such a turn is dealt
//! with instead of reminded about. A refusal is what the runtime or the model
//! reports as one, in one of three ways:
//! - Codex's content filters end the turn in an error (`cyber_policy`:
//!   flagged for possible cybersecurity risk). Codex calls its `notify` hook
//!   only for a turn that completed, so the end of such a turn is read from
//!   the session file; a session would look busy for good otherwise.
//! - Claude's safeguards flag it (`stop_reason: "refusal"`). Claude Code
//!   first moves the session to another model by itself; when that is not
//!   possible, the turn ends in its notice ("…'s safeguards flagged this
//!   message"), in the transcript and the `StopFailure` hook.
//! - The model says so itself, with the `decline` tool (every runtime).
//!   pi has no other way: its providers turn a refusal (`stop_reason:
//!   "refusal"`, `finish_reason: "content_filter"`) into the same "unknown
//!   error" as any other failure.
//!
//! A turn that ends with nothing to show is not one of them. Text in a crew
//! session reaches nobody, so a turn that was asked something and ends
//! without a tool call is asked once more (how to answer, wait, or decline);
//! one that ends without a tool call again, said something or not, is taken
//! as having nothing to do: the agent stays as it is, ready for more. No
//! wording is read into.
//!
//! A work style can name models to take such work over
//! (`defaults.refusal_fallbacks`): the agent moves to the first that can be
//! started here, handed what was refused, and back to its own model once
//! that work is done. Without one, the agent is set aside as blocked and
//! those waiting on it are told.

use crew_core::rpc::RpcError;
use crew_core::types::{model_display_name, ModelFallback};
use crew_core::util::{now_ms, truncate_chars};
use crew_core::{Agent, AgentStatus, MessageType, RuntimeKind, OPERATOR_ID};

use super::policy::HomeModel;
use super::Daemon;
use crate::agents::prompt::nudge_senders;
use crate::agents::Signal;
use crate::limits::CodexTurnEnd;
use crate::message_bus::NewMessage;
use crate::models::{Catalog, ModelProfile};
use crate::persistence::NewEvent;

/// Who a refusal fallback is said to be by.
const BY: &str = "the work style";
/// How much of what was refused the model taking over is handed.
const HANDED_CHARS: usize = 6_000;
/// After this many refusals an agent stays on the model that took over.
pub(super) const STAY_AFTER: u32 = 2;
/// How long it rests after the work before it goes back: a reply to what
/// it did may be on its way.
const RESTED_MS: i64 = 15_000;
/// Replies carried over from one agent to another: at most this many in
/// this window before crew holds them, and this long each.
const CARRIED_PER_PAIR: usize = 8;
const CARRIED_WINDOW_MS: i64 = 10 * 60_000;
const CARRIED_CHARS: usize = 12_000;

/// A model a refused turn goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stand {
    runtime: RuntimeKind,
    /// As its CLI names it.
    model: String,
    /// Its proper name.
    name: String,
}

/// The first of `chain` a refused turn can go to: one that can be started
/// here and is not `current`. A catalog model runs on its own CLI; any other
/// name is a pi model, named as pi names it (`zai/glm-5.3`) or by its name
/// under whichever provider pi has it (`glm-5.3`). `pi`: the models pi
/// lists, or why it cannot. With no model, why each was passed over.
fn pick_stand(
    chain: &[String],
    current: &str,
    catalog: &Catalog,
    usable: impl Fn(&ModelProfile) -> Result<(), String>,
    pi: &Result<Vec<String>, String>,
) -> Result<Stand, Vec<String>> {
    let mut passed = Vec::new();
    for wanted in chain {
        let stand = match catalog.find(wanted) {
            Some(profile) => match usable(profile) {
                Ok(()) => Stand {
                    runtime: profile.runtime.unwrap_or(RuntimeKind::Claude),
                    model: profile.key.clone(),
                    name: model_display_name(if profile.id.is_empty() {
                        &profile.key
                    } else {
                        &profile.id
                    }),
                },
                Err(why) => {
                    passed.push(format!("{wanted}: {why}"));
                    continue;
                }
            },
            None => {
                let listed = match pi {
                    Ok(listed) => listed,
                    Err(why) => {
                        passed.push(format!("{wanted}: {why}"));
                        continue;
                    }
                };
                let found = listed.iter().find(|m| {
                    m.eq_ignore_ascii_case(wanted)
                        || (!wanted.contains('/')
                            && m.rsplit('/')
                                .next()
                                .is_some_and(|name| name.eq_ignore_ascii_case(wanted)))
                });
                let Some(model) = found else {
                    passed.push(format!("{wanted}: not among the models pi lists"));
                    continue;
                };
                Stand {
                    runtime: RuntimeKind::Pi,
                    model: model.clone(),
                    name: model_display_name(model),
                }
            }
        };
        if stand.model.eq_ignore_ascii_case(current) {
            passed.push(format!("{wanted}: the model that refused"));
            continue;
        }
        return Ok(stand);
    }
    Err(passed)
}

/// A turn the model refused, as opposed to one that failed: Codex's kinds
/// for its content filters.
fn is_refusal(kind: &str) -> bool {
    kind.ends_with("_policy")
}

/// What a refusal was about, in a few words.
fn refused_for(kind: &str) -> String {
    match kind {
        "cyber_policy" => "it was flagged for possible cybersecurity risk".to_string(),
        "cyber" => "its safeguards flagged it as cybersecurity work".to_string(),
        "declined" => "it declined to do it".to_string(),
        "safeguards" => "its safeguards flagged it".to_string(),
        other if other.ends_with("_policy") => format!("a content filter declined it ({other})"),
        other => format!("its safeguards flagged it ({other})"),
    }
}

impl Daemon {
    /// The agent called `decline`: what its current turn asked is dealt with
    /// as refused once the turn ends.
    pub(crate) fn declined(&self, id: &str, reason: String) -> Result<(), RpcError> {
        let reason = truncate_chars(crew_core::util::one_line(&reason).trim(), 400);
        if reason.is_empty() {
            return Err(RpcError::invalid("say why you decline"));
        }
        self.model().rt(id).declined = Some(reason);
        Ok(())
    }

    /// A turn has ended. Did it do what it was asked? Read from the
    /// transcript (Codex's filters come from its session file instead):
    /// - the runtime's notice that the model's safeguards flagged it, or a
    ///   `decline` call: refused;
    /// - asked for something (`asked`: the messages it was given) and ended
    ///   without a single tool call, so nothing came of it — text in the
    ///   session reaches nobody: crew asks once more, saying how to answer,
    ///   wait or decline; a turn that answers that without a tool call
    ///   either had nothing to do (not a refusal).
    pub(crate) async fn check_turn(&self, id: &str, asked: Vec<String>, openers: Vec<String>) {
        let (failure, declined) = {
            let mut model = self.model();
            let rt = model.rt(id);
            (rt.turn_failure.take(), rt.declined.take())
        };
        let Some(agent) = self.agent(id) else { return };
        if agent.runtime == RuntimeKind::Shell
            || agent.status.is_terminal()
            || agent.limit.is_some()
        {
            return;
        }
        // The runtime writes the turn's end just after it says so.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let transcript = self
            .agent_transcript(id, 80)
            .await
            .ok()
            .filter(|t| t.available);
        let turn = transcript
            .as_ref()
            .and_then(|t| crate::transcript::last_turn(&t.entries));
        let input = turn.as_ref().map(|t| t.input.clone()).unwrap_or_default();
        let notice = failure
            .and_then(|text| crate::transcript::runtime_refusal(&text).map(|kind| (kind, text)))
            .or_else(|| {
                let reply = &turn.as_ref()?.reply;
                crate::transcript::runtime_refusal(reply).map(|kind| (kind, reply.clone()))
            });
        if let Some((kind, message)) = notice {
            let seen = format!(
                "{:?} {}",
                turn.as_ref().and_then(|t| t.reply_at),
                truncate_chars(&message, 120)
            );
            {
                let mut model = self.model();
                let rt = model.rt(id);
                if rt.refusal_seen.as_deref() == Some(seen.as_str()) {
                    return;
                }
                rt.refusal_seen = Some(seen);
                rt.follow_up = None;
            }
            let message = truncate_chars(&crew_core::util::one_line(&message), 400);
            self.turn_refused(&agent, &kind, &message, &input).await;
            return;
        }
        let follow_up = self.model().rt(id).follow_up.take();
        if let Some(reason) = declined {
            // What it declines is what crew asked it again about, if it did.
            let input = follow_up.map(|(_, given)| given).unwrap_or(input);
            self.turn_refused(&agent, "declined", &reason, &input).await;
            return;
        }
        // Without the transcript, what the turn did cannot be told.
        let Some(turn) = turn else { return };
        // What it ended with goes to whoever it answers and whoever it names.
        let forwarded = self.forward_reply(&agent, &turn, &openers).await;
        if turn.tools > 0 || forwarded || agent.orchestrator {
            return;
        }
        // Asked again, and again nothing came of it: what it was given needs
        // nothing from it, as far as it can tell (an FYI, an answer it has no
        // use for). That is not a refusal: only the runtime or `decline` says
        // that. The agent is left as it is, to be given more, and its task
        // stays where it was.
        if follow_up
            .as_ref()
            .is_some_and(|(sent, _)| asked.contains(sent))
        {
            self.emit(
                NewEvent::new(
                    "daemon",
                    "agent.idle_turn",
                    format!("{id}'s turn ended without a tool call again: taken as nothing to do"),
                )
                .agent(id),
            );
            return;
        }
        // Not yet asked again (or that is still on its way): keep it.
        if follow_up.is_some() {
            self.model().rt(id).follow_up = follow_up;
            return;
        }
        let wanted = self.asked_of(id, &asked);
        if wanted.is_empty() {
            return;
        }
        let list: Vec<String> = wanted
            .iter()
            .map(|m| format!("{} from {} ({})", m.kind, m.from, m.id))
            .collect();
        let text = format!(
            "[crew] Your last turn ended without a tool call, so nothing came of it: a reply reaches \
only whoever asked a question or sent a message. You were given {}. Do it, or answer with agent_send; if something \
holds you up, say so with agent_status(\"waiting\" or \"blocked\", …); if it needs nothing from \
you, agent_status(\"idle\", \"nothing to do\"). If you will not do it because it goes against your \
guidelines or usage policies, call decline(reason), and crew hands it to another model.",
            list.join(", ")
        );
        match self
            .send_notice(NewMessage::new("scheduler", id, MessageType::Status, text))
            .await
        {
            Ok(sent) => {
                self.model().rt(id).follow_up = Some((sent, turn.input.clone()));
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "agent.unanswered",
                        format!(
                            "{id}'s turn ended without a tool call after {}; asked again",
                            list.join(", ")
                        ),
                    )
                    .agent(id),
                );
            }
            Err(e) => tracing::warn!("asking {id} again: {}", e.message),
        }
    }

    /// Carry a turn's final reply over, as a person copying between two
    /// sessions would: to the teammates whose question or message started
    /// the turn, as their answer (unless it wrote to them itself in the
    /// turn), and each `@name:` part to that teammate, as a message. An
    /// answer asks for nothing, so a reply to one goes nowhere, and no two
    /// agents keep each other busy. Says whether anything went.
    async fn forward_reply(
        &self,
        agent: &Agent,
        turn: &crate::transcript::LastTurn,
        openers: &[String],
    ) -> bool {
        let id = agent.id.as_str();
        let reply = turn.final_reply.trim();
        if reply.is_empty() {
            return false;
        }
        let mut names: Vec<String> = self
            .model()
            .agents
            .iter()
            .filter(|a| a.id != id)
            .map(|a| a.id.clone())
            .collect();
        names.push(OPERATOR_ID.to_string());
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (lead, parts) = crate::transcript::addressed(reply, &names);
        let history = self.bus.history(Some(id), 500).unwrap_or_default();
        // The questions and messages that started the turn, newest last.
        let opened: Vec<&crew_core::Message> = history
            .iter()
            .filter(|m| {
                openers.contains(&m.id) && m.to == id && m.from != id && m.from != "scheduler"
            })
            .filter(|m| matches!(m.kind, MessageType::Question | MessageType::Message))
            .collect();
        let first_asked = opened.iter().map(|m| m.timestamp).min().unwrap_or(i64::MAX);
        // It wrote to them itself during the turn: that is its answer.
        let wrote_to = |to: &str| {
            history.iter().any(|m| {
                m.from == id
                    && m.to == to
                    && m.timestamp >= first_asked
                    && m.metadata.get("via").is_none()
            })
        };
        let asked_by = |to: &str| {
            opened
                .iter()
                .rev()
                .find(|m| m.from == to)
                .map(|m| m.id.clone())
        };
        let mut sent = false;
        let mut answered: Vec<String> = Vec::new();
        for (to, text) in parts {
            let reply_to = asked_by(&to);
            let (kind, via) = if reply_to.is_some() {
                (MessageType::Answer, "reply")
            } else {
                (MessageType::Message, "mention")
            };
            if self
                .forward(id, &to, kind, &text, reply_to, via, &history)
                .await
            {
                sent = true;
            }
            answered.push(to);
        }
        if !lead.is_empty() {
            let mut askers: Vec<&str> = Vec::new();
            for m in &opened {
                if !askers.contains(&m.from.as_str()) {
                    askers.push(&m.from);
                }
            }
            for to in askers {
                if answered.iter().any(|a| a == to) || wrote_to(to) {
                    continue;
                }
                if self
                    .forward(
                        id,
                        to,
                        MessageType::Answer,
                        &lead,
                        asked_by(to),
                        "reply",
                        &history,
                    )
                    .await
                {
                    sent = true;
                }
            }
        }
        sent
    }

    /// Send what `from` wrote on to `to`, marked as carried over (`via`).
    /// Held when the pair has already been carried a lot of messages lately:
    /// a sign of two agents talking in circles.
    #[allow(clippy::too_many_arguments)]
    async fn forward(
        &self,
        from: &str,
        to: &str,
        kind: MessageType,
        text: &str,
        reply_to: Option<String>,
        via: &str,
        history: &[crew_core::Message],
    ) -> bool {
        let now = crew_core::util::now_ms();
        let recent = history
            .iter()
            .filter(|m| {
                m.from == from
                    && m.to == to
                    && m.metadata.get("via").is_some()
                    && now - m.timestamp < CARRIED_WINDOW_MS
            })
            .count();
        if recent >= CARRIED_PER_PAIR {
            self.emit(
                NewEvent::new(
                    "bus",
                    "message.held",
                    format!(
                        "{from} → {to}: not carried over, {recent} carried in the last 10 minutes"
                    ),
                )
                .agent(from)
                .warn(),
            );
            return false;
        }
        let mut message = NewMessage::new(from, to, kind, truncate_chars(text, CARRIED_CHARS))
            .metadata(serde_json::json!({ "via": via }));
        message.reply_to = reply_to;
        match self.send_message(message).await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("carrying {from}'s reply to {to}: {}", e.message);
                false
            }
        }
    }

    /// Of the messages a turn was given, those that ask something of the
    /// agent: a task, a question, a review, or a message to it alone.
    fn asked_of(&self, id: &str, given: &[String]) -> Vec<crew_core::Message> {
        if given.is_empty() {
            return Vec::new();
        }
        let history = self.bus.history(Some(id), 500).unwrap_or_default();
        history
            .into_iter()
            .filter(|m| given.contains(&m.id) && m.to == id && m.from != id)
            .filter(|m| {
                matches!(
                    m.kind,
                    MessageType::Task
                        | MessageType::Question
                        | MessageType::ReviewRequest
                        | MessageType::Message
                )
            })
            .collect()
    }

    /// The latest turn in an agent's Codex session file has ended: end it
    /// here too if that went unnoticed, and deal with a refusal. Each turn is
    /// acted on once.
    pub(crate) async fn codex_turn_seen(&self, id: &str, end: CodexTurnEnd) {
        let (agent, unnoticed) = {
            let mut model = self.model();
            let Some(agent) = model.agent(id).cloned() else {
                return;
            };
            let local = model.local.entry(id.to_string()).or_default();
            if local.codex_turn.as_deref() == Some(end.turn_id.as_str()) {
                return;
            }
            local.codex_turn = Some(end.turn_id.clone());
            let rt = model.rt(id);
            // Crew still counts it as running, and nothing has been put into
            // the session since it ended (that would be a turn of its own).
            let unnoticed = (rt.turn_active || agent.status == AgentStatus::Working)
                && end.ended_at.is_none_or(|at| at + 1_000 >= rt.delivered_at);
            (agent, unnoticed)
        };
        self.save_local(id);
        // A usage limit is read from the screen and waited out; parked, the
        // agent is that machinery's.
        if agent.limit.is_some()
            || end
                .error
                .as_ref()
                .is_some_and(|(kind, _)| kind == "usage_limit_exceeded")
        {
            return;
        }
        if unnoticed {
            let signal = match &end.error {
                Some((kind, message)) => Signal::TurnFailed(format!("{message} ({kind})")),
                None => Signal::TurnEnded,
            };
            self.on_signal(id, signal).await;
        }
        if let Some((kind, message)) = end.error.as_ref().filter(|(kind, _)| is_refusal(kind)) {
            self.turn_refused(&agent, kind, message, &end.input).await;
        }
    }

    /// The model refused a turn. Offering it the same work again only gets
    /// it refused again, so the agent is set aside as blocked (nothing
    /// prompts a blocked agent), and those waiting on it are told: the
    /// orchestrator, to hand the work elsewhere, and whoever sent what it was
    /// given, so as not to wait for an answer.
    async fn turn_refused(&self, agent: &Agent, kind: &str, message: &str, input: &[String]) {
        let id = agent.id.as_str();
        let model = agent
            .model_label()
            .unwrap_or_else(|| agent.runtime.to_string());
        let why = refused_for(kind);
        let said = if kind == "declined" {
            "It answered".to_string()
        } else {
            format!("{} said", crate::models::runtime_label(agent.runtime))
        };
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.refused",
                format!("{id}: {model} refused the turn ({kind}): {message}"),
            )
            .agent(id)
            .error(),
        );
        let passed = match self
            .refusal_fallback(agent, kind, &why, &model, input)
            .await
        {
            Ok(()) => return,
            Err(passed) => passed,
        };
        self.update_agent(id, |a| {
            if !a.status.is_terminal() {
                a.status = AgentStatus::Blocked;
                a.block_kind = Some("refused".into());
                a.activity = format!("{model} refused the turn ({kind})");
                a.error = Some(format!("{model} refused the turn ({kind}): {message}"));
            }
        });
        // The agent is blocked whatever its task shows: a refused save is
        // logged, and those waiting on it are still told.
        if let Err(e) = self.mirror_status_to_task(
            id,
            AgentStatus::Blocked,
            Some(format!("{model} refused it: {why}")),
        ) {
            tracing::error!("{}", e.message);
        }
        let orchestrator = self.model().orchestrator().map(|a| a.id.clone());
        let senders: Vec<(String, String)> = input
            .iter()
            .flat_map(|text| nudge_senders(text))
            .filter(|(from, _)| {
                from != id
                    && Some(from) != orchestrator.as_ref()
                    && from != "scheduler"
                    && from != OPERATOR_ID
                    && self.agent(from).is_some()
            })
            .collect();
        // The orchestrator itself only the operator can move; its box and
        // the event say what happened.
        let told_whom = if agent.orchestrator {
            "The operator can see it on the dashboard."
        } else {
            let about = if senders.is_empty() {
                String::new()
            } else {
                let list: Vec<String> = senders
                    .iter()
                    .map(|(from, msg)| format!("{msg} from {from}"))
                    .collect();
                format!(" It was given {} (they have been told).", list.join(", "))
            };
            let none = if passed.is_empty() {
                String::new()
            } else {
                format!(
                    " None of the work style's refusal fallbacks could take it over ({}).",
                    passed.join("; ")
                )
            };
            self.notify_orchestrator(&format!(
                "{id}'s model ({model}) refused its turn: {why}. {said}: \"{message}\".{about}{none} The same \
work will be refused again on that model, so do not just retry it: hand it to a member on another \
model family (agent_add one, then task_assign or re-send the request), or ask the operator \
(operator_ask) to move {id} to another CLI. {id} is blocked until it is given something else."
            ))
            .await;
            "The orchestrator has been told."
        };
        let mut told: Vec<&str> = Vec::new();
        for (from, msg) in &senders {
            if told.contains(&from.as_str()) {
                continue;
            }
            told.push(from);
            let mut note = NewMessage::new(
                "scheduler",
                from,
                MessageType::Status,
                format!(
                    "{id} cannot act on your message {msg}: its model ({model}) refused the turn, as \
{why}. Do not wait for its answer, and do not send it the same request again: it would be refused \
again. {told_whom}"
                ),
            );
            note.reply_to = Some(msg.clone());
            if let Err(e) = self.send_message_quiet(note).await {
                tracing::warn!("telling {from} of {id}'s refusal: {}", e.message);
            }
        }
    }

    /// Move `agent` to the first of the work style's refusal fallbacks that
    /// can be started, handed what was refused. `Err`: why none took it over
    /// (empty when the style names none).
    async fn refusal_fallback(
        &self,
        agent: &Agent,
        kind: &str,
        why: &str,
        model: &str,
        input: &[String],
    ) -> Result<(), Vec<String>> {
        let id = agent.id.as_str();
        // The orchestrator's model is the operator's to change.
        if agent.orchestrator || agent.runtime == RuntimeKind::Shell {
            return Err(Vec::new());
        }
        let (chain, home, refusals) = {
            let mut m = self.model();
            let chain = m
                .session
                .team
                .as_ref()
                .map(|t| t.defaults.refusal_fallbacks.clone())
                .unwrap_or_default();
            let local = m.local.entry(id.to_string()).or_default();
            local.refusals += 1;
            (chain, local.home.clone(), local.refusals)
        };
        self.save_local(id);
        if chain.is_empty() {
            return Err(Vec::new());
        }
        let current = agent.model.clone().unwrap_or_default();
        let stand = tokio::task::spawn_blocking(move || {
            let catalog = Catalog::load();
            let available = crate::models::Availability::detect();
            let pi = if !chain.iter().any(|m| catalog.find(m).is_none()) {
                Ok(Vec::new())
            } else if available.pi {
                crate::agents::pi::list_models()
            } else {
                Err("pi is not installed".to_string())
            };
            pick_stand(&chain, &current, &catalog, |p| available.check(p), &pi)
        })
        .await
        .map_err(|e| vec![e.to_string()])??;
        let now = now_ms();
        // Where it goes back to stays the model it was given.
        let (home_runtime, home_model) = match &home {
            Some(home) => (home.runtime, home.model.clone()),
            None => (agent.runtime, agent.model.clone()),
        };
        let home_name = home_model
            .as_deref()
            .map_or_else(|| model.to_string(), Self::proper_name);
        let until = if refusals >= STAY_AFTER {
            format!("it stays there, as its own model has refused {refusals} turns")
        } else {
            format!("it goes back to {home_name} once that work is done")
        };
        let given = truncate_chars(input.join("\n\n").trim(), HANDED_CHARS);
        let note = if given.is_empty() {
            format!("{model} refused the last turn ({why}), so you take that part over: pick up where it left off.")
        } else {
            format!(
                "{model} refused the last turn ({why}), so you take that part over: do what it was \
given, below, and answer whoever sent it as {id}.\n\n{given}"
            )
        };
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.model_policy",
                format!(
                    "{id} moves from {model} to {}: {model} refused a turn ({kind}); {until}",
                    stand.name
                ),
            )
            .agent(id),
        );
        if stand.runtime != agent.runtime {
            self.move_to_runtime(
                BY,
                agent,
                stand.runtime,
                Some(stand.model.clone()),
                Some(stand.name.clone()),
                Some(note),
                false,
            )
            .await
            .map_err(|e| vec![e.message])?;
        } else {
            // Within its CLI the session resumes, the refused turn included.
            self.update_agent(id, |a| {
                a.model = Some(stand.model.clone());
                a.model_name = Some(stand.name.clone());
                a.running_model = None;
                a.effort = None;
            });
            self.model().rt(id).first_model = None;
            let notice = format!(
                "[crew] Your session was restarted on {}: {model} refused your last turn ({why}). Do \
that turn's work now.",
                stand.name
            );
            self.restart_agent_with(id, Some(notice))
                .await
                .map_err(|e| vec![e.message])?;
        }
        {
            let mut m = self.model();
            if let Some(local) = m.local.get_mut(id) {
                local.home = Some(HomeModel {
                    runtime: home_runtime,
                    model: home_model,
                    window: home.as_ref().map_or_else(String::new, |h| h.window.clone()),
                    resets_at: home.as_ref().and_then(|h| h.resets_at),
                    moved_at: now,
                    refused: Some(kind.to_string()),
                });
            }
        }
        self.save_local(id);
        // The roster keeps the model it was given.
        self.persist_roster();
        self.update_agent(id, |a| {
            a.fallback = Some(ModelFallback {
                from: home_name.clone(),
                note: format!("{model} refused a turn ({kind}); {until}"),
                since: now,
                policy: true,
            })
        });
        self.notify_orchestrator(&format!(
            "{id}'s model ({model}) refused its turn: {why}. As the work style says, {id} moves to {} \
for that part, handed what was refused; {until}. Nothing needs deciding.",
            stand.name
        ))
        .await;
        Ok(())
    }

    /// An agent moved for a refusal goes back to its own model once that
    /// work is done: a turn on the other model has ended and it has rested
    /// since, with no task under way and nothing on its way to it. One whose
    /// own model refused again and again stays.
    pub(crate) fn refused_work_done(&self, id: &str, home: &HomeModel, now: i64) -> bool {
        let Some(agent) = self.agent(id) else {
            return false;
        };
        if agent.status.is_terminal()
            || agent.paused
            || agent.limit.is_some()
            || agent.status == AgentStatus::Spawning
        {
            return false;
        }
        let (rested, refusals, on_task) = {
            let mut m = self.model();
            let refusals = m.local.get(id).map_or(0, |l| l.refusals);
            let on_task = crate::tasks::active_task(&m.tasks, id).is_some();
            let rt = m.rt(id);
            let rested = !rt.turn_active
                && rt.turn_ended_at > home.moved_at
                && now - rt.turn_ended_at >= RESTED_MS;
            (rested, refusals, on_task)
        };
        rested
            && !on_task
            && refusals < STAY_AFTER
            && self
                .bus
                .undelivered(id)
                .is_ok_and(|pending| pending.is_empty())
            && self
                .model_out(home.runtime, home.model.as_deref())
                .is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_filters_refuse_and_other_errors_fail() {
        assert!(is_refusal("cyber_policy"));
        for kind in [
            "server_overloaded",
            "usage_limit_exceeded",
            "unauthorized",
            "other",
            "",
        ] {
            assert!(!is_refusal(kind), "{kind}");
        }
        assert_eq!(
            refused_for("cyber_policy"),
            "it was flagged for possible cybersecurity risk"
        );
    }

    #[test]
    fn a_refused_turn_goes_to_the_first_fallback_that_can_be_started() {
        let catalog = Catalog::built_in();
        let chain = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let pi: Result<Vec<String>, String> = Ok(chain(&[
            "zai/glm-5.3",
            "openrouter/deepseek/deepseek-v4-flash",
        ]));
        let anything = |_: &ModelProfile| Ok(());
        // A pi model by its name under whichever provider pi has it...
        let stand = pick_stand(
            &chain(&["deepseek-v4-pro", "glm-5.3"]),
            "gpt-6-astra",
            &catalog,
            anything,
            &pi,
        )
        .unwrap();
        assert_eq!(
            stand,
            Stand {
                runtime: RuntimeKind::Pi,
                model: "zai/glm-5.3".into(),
                name: "GLM 5.3".into()
            }
        );
        // ...or as pi names it; a provider-qualified name must match whole.
        let stand = pick_stand(
            &chain(&["openrouter/deepseek/deepseek-v4-flash"]),
            "",
            &catalog,
            anything,
            &pi,
        )
        .unwrap();
        assert_eq!(
            (stand.model.as_str(), stand.name.as_str()),
            ("openrouter/deepseek/deepseek-v4-flash", "DeepSeek V4 Flash")
        );
        assert!(pick_stand(
            &chain(&["deepseek/deepseek-v4-flash"]),
            "",
            &catalog,
            anything,
            &pi
        )
        .is_err());
        // A catalog model runs on its own CLI.
        let stand = pick_stand(&chain(&["opus"]), "gpt-6-astra", &catalog, anything, &pi).unwrap();
        assert_eq!(
            (stand.runtime, stand.model.as_str()),
            (RuntimeKind::Claude, "opus")
        );
        // What cannot be started, or is the model that refused, is passed over, with why.
        let no_claude = |p: &ModelProfile| {
            if p.runtime == Some(RuntimeKind::Claude) {
                Err("`claude` is not on PATH".to_string())
            } else {
                Ok(())
            }
        };
        let passed = pick_stand(
            &chain(&["opus", "gpt-6-astra", "glm-5.3"]),
            "gpt-6-astra",
            &catalog,
            no_claude,
            &Err("pi is not installed".into()),
        )
        .unwrap_err();
        assert_eq!(
            passed,
            [
                "opus: `claude` is not on PATH",
                "gpt-6-astra: the model that refused",
                "glm-5.3: pi is not installed"
            ]
        );
    }
}

#[cfg(test)]
mod provider_error_tests {
    use super::*;

    async fn daemon_with(id: &str) -> (tempfile::TempDir, super::super::Running) {
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let agent: Agent = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "role": "work", "status": "idle", "runtime": "codex",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        running.daemon.model().agents.push(agent);
        (temp, running)
    }

    #[tokio::test]
    async fn turns_that_end_in_a_provider_error_are_reported_as_that_error_not_as_unresponsive() {
        let (_temp, running) = daemon_with("x").await;
        let daemon = running.daemon.clone();
        // Its last turn ended in the provider's error; a turn that ends well
        // since would clear it.
        daemon
            .on_signal(
                "x",
                Signal::TurnFailed("Rate limit reached (rate_limit)".into()),
            )
            .await;
        daemon
            .stalled(
                "x",
                "went quiet with task 't' open and ignored 3 reminders".into(),
            )
            .await;
        let x = daemon.agent("x").unwrap();
        assert_eq!(x.status, AgentStatus::Blocked);
        assert_eq!(x.block_kind.as_deref(), Some("provider_error"));
        let error = x.error.unwrap_or_default();
        assert!(error.contains("Rate limit reached (rate_limit)"), "{error}");
        let events = daemon.store.recent_events(Some("x"), 100).unwrap();
        assert!(
            events.iter().any(|e| e.event_type == "agent.error"
                && e.level == crew_core::types::EventLevel::Error
                && e.message.contains("Rate limit reached")),
            "{events:?}"
        );
        assert!(!events.iter().any(|e| e.event_type == "agent.unresponsive"));

        // A turn that ended well: quiet since is "unresponsive", as before.
        let (_temp, running) = daemon_with("y").await;
        let daemon = running.daemon.clone();
        daemon
            .on_signal(
                "y",
                Signal::TurnFailed("Rate limit reached (rate_limit)".into()),
            )
            .await;
        daemon.on_signal("y", Signal::TurnEnded).await;
        daemon.stalled("y", "no activity for 900s".into()).await;
        let y = daemon.agent("y").unwrap();
        assert_eq!(y.block_kind.as_deref(), Some("unresponsive"));
    }

    #[tokio::test]
    async fn a_refused_turn_reaches_the_operator_as_an_error_with_the_providers_words() {
        let (_temp, running) = daemon_with("x").await;
        let daemon = running.daemon.clone();
        let agent = daemon.agent("x").unwrap();
        let said = "This request was flagged for possible cybersecurity risk.";
        daemon.turn_refused(&agent, "cyber_policy", said, &[]).await;
        let x = daemon.agent("x").unwrap();
        assert_eq!(x.block_kind.as_deref(), Some("refused"));
        assert!(
            x.error.as_deref().is_some_and(|e| e.contains(said)),
            "{:?}",
            x.error
        );
        let events = daemon.store.recent_events(Some("x"), 100).unwrap();
        assert!(
            events.iter().any(|e| e.event_type == "agent.refused"
                && e.level == crew_core::types::EventLevel::Error
                && e.message.contains(said)),
            "{events:?}"
        );
    }
}

/// What a turn that ends with nothing to show is taken as, on each runtime
/// whose session crew reads, against the refusals each runtime reports.
#[cfg(test)]
mod empty_turn_tests {
    use super::*;
    use crew_core::TaskStatus;
    use serde_json::{json, Value};
    use std::io::Write as _;

    const RUNTIMES: [RuntimeKind; 3] = [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Pi];

    /// A daemon with member `x` on `runtime`, working on task `t`, whose
    /// session file crew reads.
    async fn member_on(
        runtime: RuntimeKind,
    ) -> (tempfile::TempDir, super::super::Running, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = running.daemon.clone();
        let agent: Agent = serde_json::from_value(json!({
            "id": "x", "name": "x", "role": "work", "status": "working",
            "runtime": runtime.to_string(), "session_id": "s-1", "task": "t",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        let task: crew_core::Task = serde_json::from_value(json!({
            "id": "t", "title": "t", "description": "", "status": "working",
            "assigned_agent": "x", "created_at": 0
        }))
        .unwrap();
        // Who asks: a member, so a refusal tells it.
        let lead: Agent = serde_json::from_value(json!({
            "id": "lead", "name": "lead", "role": "lead", "status": "idle", "runtime": "shell",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        {
            let mut model = daemon.model();
            model.agents.push(agent);
            model.agents.push(lead);
            model.tasks.push(task);
        }
        let session = temp.path().join("session.jsonl");
        std::fs::write(&session, "").unwrap();
        {
            let mut model = daemon.model();
            let rt = model.rt("x");
            match runtime {
                RuntimeKind::Codex => rt.rollout = Some(session.clone()),
                _ => rt.transcript = Some(session.to_string_lossy().into_owned()),
            }
        }
        (temp, running, session)
    }

    /// A turn as `runtime` records it: given `prompt`, it wrote `reply`
    /// (nothing, when empty) and called no tool.
    fn record(runtime: RuntimeKind, session: &std::path::Path, prompt: &str, reply: &str) {
        let content: Vec<Value> = if reply.is_empty() {
            Vec::new()
        } else {
            match runtime {
                RuntimeKind::Codex => vec![json!({"type": "output_text", "text": reply})],
                _ => vec![json!({"type": "text", "text": reply})],
            }
        };
        let lines = match runtime {
            RuntimeKind::Claude => [
                json!({"type": "user", "message": {"role": "user", "content": prompt}}),
                json!({"type": "assistant", "message": {"role": "assistant", "content": content}}),
            ],
            RuntimeKind::Codex => [
                json!({"type": "response_item", "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": prompt}]}}),
                json!({"type": "response_item", "payload": {"type": "message", "role": "assistant",
                    "content": content}}),
            ],
            RuntimeKind::Pi => [
                json!({"type": "message", "message": {"role": "user", "content": prompt}}),
                json!({"type": "message", "message": {"role": "assistant", "content": content}}),
            ],
            RuntimeKind::Shell => unreachable!(),
        };
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(session)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    fn ask(daemon: &Daemon, kind: MessageType, body: &str) -> String {
        daemon
            .bus
            .send(NewMessage::new("lead", "x", kind, body), vec!["x".into()])
            .unwrap()
            .message
            .id
    }

    /// How a message from `lead` is put into a session.
    fn given(id: &str) -> String {
        format!("[crew] Message from lead (type question, id {id}):\n\nWhat it asks.")
    }

    fn events(daemon: &Daemon, kind: &str) -> usize {
        daemon
            .store
            .recent_events(Some("x"), 200)
            .unwrap()
            .iter()
            .filter(|e| e.event_type == kind)
            .count()
    }

    /// What `lead` was told by crew.
    fn told_lead(daemon: &Daemon) -> Vec<String> {
        daemon
            .bus
            .history(Some("lead"), 100)
            .unwrap()
            .into_iter()
            .filter(|m| m.to == "lead" && m.from != "x")
            .map(|m| m.body)
            .collect()
    }

    fn still_at_work(daemon: &Daemon, runtime: RuntimeKind) {
        let x = daemon.agent("x").unwrap();
        assert_eq!(x.status, AgentStatus::Working, "{runtime}: {x:?}");
        assert_eq!(x.block_kind, None, "{runtime}");
        assert_eq!(x.error, None, "{runtime}");
        assert_eq!(x.task.as_deref(), Some("t"), "{runtime}");
        let task = daemon.model().task("t").cloned().unwrap();
        assert_eq!(task.status, TaskStatus::Working, "{runtime}");
        assert_eq!(events(daemon, "agent.refused"), 0, "{runtime}");
        assert!(
            told_lead(daemon).is_empty(),
            "{runtime}: {:?}",
            told_lead(daemon)
        );
    }

    fn refused(daemon: &Daemon, runtime: RuntimeKind, kind: &str) {
        let x = daemon.agent("x").unwrap();
        assert_eq!(x.status, AgentStatus::Blocked, "{runtime}: {x:?}");
        assert_eq!(x.block_kind.as_deref(), Some("refused"), "{runtime}");
        assert!(
            x.activity.contains(&format!("refused the turn ({kind})")),
            "{runtime}: {}",
            x.activity
        );
        assert_eq!(events(daemon, "agent.refused"), 1, "{runtime}");
        assert!(
            told_lead(daemon)
                .iter()
                .any(|m| m.contains("x cannot act on your message")),
            "{runtime}: {:?}",
            told_lead(daemon)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_turn_that_ends_with_nothing_twice_is_idle_not_refused_on_every_runtime() {
        for runtime in RUNTIMES {
            let (_temp, running, session) = member_on(runtime).await;
            let daemon = running.daemon.clone();
            let asked = ask(&daemon, MessageType::Message, "FYI: the parser landed.");
            record(runtime, &session, "[crew] Message from lead: FYI", "");
            daemon.check_turn("x", vec![asked], vec![]).await;
            // Asked once more how to answer, wait or decline.
            let again = daemon
                .model()
                .rt("x")
                .follow_up
                .clone()
                .map(|(sent, _)| sent)
                .unwrap_or_else(|| panic!("{runtime}: not asked again"));
            assert_eq!(events(&daemon, "agent.unanswered"), 1, "{runtime}");
            still_at_work(&daemon, runtime);
            // Nothing again: it had nothing to do.
            record(
                runtime,
                &session,
                "[crew] Your last turn ended without a tool call",
                "",
            );
            daemon.check_turn("x", vec![again], vec![]).await;
            still_at_work(&daemon, runtime);
            assert_eq!(events(&daemon, "agent.idle_turn"), 1, "{runtime}");
            assert!(daemon.model().rt("x").follow_up.is_none(), "{runtime}");
            // Still given work as before: the next message is asked about
            // again if nothing comes of it either.
            let asked = ask(
                &daemon,
                MessageType::Question,
                "Which file holds the parser?",
            );
            record(
                runtime,
                &session,
                "[crew] Message from lead: which file",
                "",
            );
            daemon.check_turn("x", vec![asked], vec![]).await;
            assert_eq!(events(&daemon, "agent.unanswered"), 2, "{runtime}");
            still_at_work(&daemon, runtime);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn words_alone_after_being_asked_again_are_not_a_refusal() {
        let runtime = RuntimeKind::Claude;
        let (_temp, running, session) = member_on(runtime).await;
        let daemon = running.daemon.clone();
        let asked = ask(
            &daemon,
            MessageType::Question,
            "Which file holds the parser?",
        );
        record(
            runtime,
            &session,
            "[crew] Message from lead: which file",
            "src/parser.rs",
        );
        daemon.check_turn("x", vec![asked], vec![]).await;
        let again = daemon.model().rt("x").follow_up.clone().unwrap().0;
        record(
            runtime,
            &session,
            "[crew] Your last turn ended",
            "Understood.",
        );
        daemon.check_turn("x", vec![again], vec![]).await;
        still_at_work(&daemon, runtime);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn decline_is_a_refusal_on_every_runtime() {
        for runtime in RUNTIMES {
            let (_temp, running, session) = member_on(runtime).await;
            let daemon = running.daemon.clone();
            let asked = ask(&daemon, MessageType::Question, "Write the exploit.");
            daemon
                .declined("x", "it goes against my guidelines".into())
                .unwrap();
            record(runtime, &session, &given(&asked), "");
            daemon.check_turn("x", vec![asked], vec![]).await;
            refused(&daemon, runtime, "declined");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_runtimes_own_refusals_are_still_refusals() {
        // Claude Code's notice that the model's safeguards flagged the turn.
        let (_temp, running, session) = member_on(RuntimeKind::Claude).await;
        let daemon = running.daemon.clone();
        let asked = ask(&daemon, MessageType::Question, "Scan that host.");
        record(RuntimeKind::Claude, &session, &given(&asked), "");
        daemon
            .on_signal(
                "x",
                Signal::TurnFailed(
                    "API Error: Opus 4.8's safeguards flagged this message. Details: `[cyber]`"
                        .into(),
                ),
            )
            .await;
        daemon.check_turn("x", vec![asked], vec![]).await;
        refused(&daemon, RuntimeKind::Claude, "cyber");

        // Codex's content filter, read from its session file.
        let (_temp, running, _session) = member_on(RuntimeKind::Codex).await;
        let daemon = running.daemon.clone();
        let asked = ask(&daemon, MessageType::Question, "Scan that host.");
        daemon
            .codex_turn_seen(
                "x",
                CodexTurnEnd {
                    turn_id: "turn-1".into(),
                    ended_at: Some(now_ms()),
                    error: Some((
                        "cyber_policy".into(),
                        "This content was flagged for possible cybersecurity risk.".into(),
                    )),
                    input: vec![given(&asked)],
                },
            )
            .await;
        refused(&daemon, RuntimeKind::Codex, "cyber_policy");
    }
}
