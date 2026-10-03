//! Context windows: how full each agent's is, and compacting them.
//!
//! A session that fills its window is compacted by its runtime anyway, at
//! whatever moment that happens and with no word on what matters. So crew
//! keeps the figure in view (`Agent::context`), tells the orchestrator when
//! an agent's window is getting full, and lets it (or the operator, or the
//! agent itself) queue a compaction with a note on what the summary must
//! keep. A queued compaction runs at the agent's next turn boundary; when
//! it is done the agent is told where it stands, so it picks its work up
//! again instead of waiting for a prompt.

use crew_core::rpc::{Push, RpcError, ServerFrame, TranscriptResult};
use crew_core::types::ContextUsage;
use crew_core::util::{now_ms, one_line, truncate_chars};
use crew_core::{Agent, AgentStatus, MessageType, RuntimeKind, TaskStatus};
use serde_json::json;

use super::server::Who;
use super::Daemon;
use crate::agents::runtime_for;
use crate::message_bus::NewMessage;
use crate::persistence::NewEvent;

/// How often a Claude session's transcript is read while something about
/// the session is still unknown, and once everything is (the session may
/// have been moved to another model since).
const TRANSCRIPT_EVERY_MS: i64 = 20_000;
const TRANSCRIPT_RECHECK_MS: i64 = 60_000;
pub(crate) const TRANSCRIPT_TAIL_BYTES: u64 = 1024 * 1024;
/// The size of the 1M-token context window, and of the standard one.
const LONG_CONTEXT_TOKENS: u64 = 1_000_000;
const STANDARD_CONTEXT_TOKENS: u64 = 200_000;

/// Longest note on what a summary must keep.
const KEEP_CHARS: usize = 1000;
/// Ask a session's mod again when a queued compaction has not happened.
const SOCKET_RETRY_MS: i64 = 10_000;
/// Type the command again only after a compaction had ample time to finish.
const TERMINAL_RETRY_MS: i64 = 240_000;
/// A second report of the same compaction (the mod's and the hook's).
const SAME_COMPACTION_MS: i64 = 5_000;
/// Once warned about, an agent is warned about again only after its window
/// has been this many points below the threshold.
const WARN_HYSTERESIS: u8 = 10;
/// A Codex window that shrinks to under half was compacted. Below this many
/// tokens a drop says nothing.
const CODEX_DROP_FLOOR: u64 = 20_000;

fn thousands(tokens: u64) -> String {
    format!("{}k", tokens.div_ceil(1000))
}

impl Daemon {
    /// Whether `error` is the account refusing the 1M-token context window
    /// of a session crew started with it. If so the agent is restarted on
    /// the standard window, and the variant is not asked for again.
    pub(crate) fn long_context_refused(&self, id: &str, error: &str) -> bool {
        if !error.to_lowercase().contains("long context") {
            return false;
        }
        {
            let mut model = self.model();
            match model.local.get_mut(id) {
                Some(local) if local.long_context => {
                    local.long_context = false;
                    local.no_long_context = true;
                }
                _ => return false,
            }
        }
        self.save_local(id);
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.long_context_refused",
                format!(
                    "{id}: this account has no 1M-token context for its model ({}); restarting it on the standard window",
                    truncate_chars(&one_line(error), 160)
                ),
            )
            .agent(id)
            .warn(),
        );
        let daemon = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            if let Err(e) = daemon.restart_agent_with(&id, None).await {
                tracing::warn!(
                    "restarting {id} without the 1M context failed: {}",
                    e.message
                );
            }
        });
        true
    }

    /// A new reading of how full `id`'s context window is.
    pub(crate) async fn record_context(&self, id: &str, tokens: u64, window: u64) {
        let Some(agent) = self.agent(id) else { return };
        let reading = ContextUsage::new(tokens, window, now_ms());
        // Codex says nothing when it compacts; its window shrinking does.
        let shrank = agent.runtime == RuntimeKind::Codex
            && agent.context.as_ref().is_some_and(|before| {
                before.tokens >= CODEX_DROP_FLOOR && tokens * 2 < before.tokens
            });
        if shrank {
            self.context_compacted(id).await;
        }
        let unchanged = agent
            .context
            .as_ref()
            .is_some_and(|c| c.tokens == tokens && c.window == window);
        if !unchanged || shrank {
            let percent = reading.percent;
            self.update_agent(id, |a| a.context = Some(reading));
            self.warn_when_full(&agent, percent, tokens, window).await;
        }
    }

    /// Tell the orchestrator, once per filling, that an agent's window has
    /// passed the threshold; it decides when a compaction fits.
    async fn warn_when_full(&self, agent: &Agent, percent: u8, tokens: u64, window: u64) {
        let threshold = self
            .model()
            .session
            .team
            .as_ref()
            .map_or(0, |t| t.limits.context_warn_percent);
        if threshold == 0 {
            return;
        }
        let warn = {
            let mut model = self.model();
            let pending = model.agent(&agent.id).is_some_and(|a| a.compact_pending);
            let rt = model.rt(&agent.id);
            if percent < threshold.saturating_sub(WARN_HYSTERESIS) {
                rt.context_warned = false;
            }
            let warn = percent >= threshold && !rt.context_warned && !pending;
            if warn {
                rt.context_warned = true;
            }
            warn
        };
        if !warn {
            return;
        }
        let figures = format!(
            "{percent}% full ({} of {} tokens)",
            thousands(tokens),
            thousands(window)
        );
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.context_full",
                format!("{}'s context window is {figures}", agent.id),
            )
            .agent(&agent.id)
            .warn()
            .data(json!({ "percent": percent, "tokens": tokens, "window": window })),
        );
        let note = if agent.orchestrator {
            format!(
                "Your own context window is {figures}. At a quiet moment, compact it with \
agent_compact(keep: \"...\"), naming what the summary must keep: the plan, who is doing what, \
decisions made, questions still open. Left alone, the runtime compacts it when it is nearly \
full, at whatever point that is."
            )
        } else {
            format!(
                "{id}'s context window is {figures}. At a natural break in its work (between \
tasks, after a commit) queue a compaction with agent_compact(id: \"{id}\", keep: \"...\"), naming \
what its summary must keep: the task, decisions made, files in play. Left alone, its runtime \
compacts it when it is nearly full, at whatever point that is.",
                id = agent.id
            )
        };
        self.notify_orchestrator(&note).await;
    }

    /// Queue a compaction of an agent's context. An agent may ask for its
    /// own; the orchestrator and the operator for anyone's.
    pub(crate) async fn compact_agent(
        &self,
        who: &Who,
        id: Option<String>,
        keep: Option<String>,
    ) -> Result<Agent, RpcError> {
        let target = match (who, id) {
            (Who::Agent(me), None) => me.clone(),
            (Who::Agent(me), Some(id)) if id == *me => id,
            (Who::Agent(me), Some(id)) => {
                if !self.model().agent(me).is_some_and(|a| a.orchestrator) {
                    return Err(RpcError::forbidden(
                        "you can compact your own context (leave the id out); only the orchestrator \
compacts a teammate's",
                    ));
                }
                id
            }
            (Who::Operator, Some(id)) => id,
            (Who::Operator, None) => return Err(RpcError::invalid("name the agent to compact")),
        };
        let agent = self
            .agent(&target)
            .ok_or_else(|| RpcError::not_found(format!("no agent '{target}'")))?;
        if agent.status.is_terminal() {
            return Err(RpcError::invalid(format!("{target} is not running")));
        }
        if runtime_for(agent.runtime).compact_command(None).is_none() {
            return Err(RpcError::invalid(format!(
                "{target} runs on {}, which has no context to compact",
                agent.runtime
            )));
        }
        let keep = keep
            .map(|k| truncate_chars(&one_line(&k), KEEP_CHARS))
            .filter(|k| !k.is_empty());
        {
            let mut model = self.model();
            let rt = model.rt(&target);
            rt.compact_keep = keep.clone();
            rt.compact_sent_at = 0;
        }
        self.update_agent(&target, |a| a.compact_pending = true);
        let by = who.sender();
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.compact_queued",
                match &keep {
                    Some(keep) => format!(
                        "{by} queued a compaction of {target}'s context, keeping: {}",
                        truncate_chars(keep, 160)
                    ),
                    None => format!("{by} queued a compaction of {target}'s context"),
                },
            )
            .agent(&target),
        );
        self.try_compact(&target).await;
        self.agent(&target)
            .ok_or_else(|| RpcError::not_found(format!("no agent '{target}'")))
    }

    /// Start a queued compaction if the session can take it now: at once
    /// through the session's mod (which waits for the turn to end itself),
    /// otherwise by typing the runtime's command while the agent is at rest.
    pub(crate) async fn try_compact(&self, id: &str) {
        let now = now_ms();
        let Some(agent) = self.agent(id) else { return };
        if !agent.compact_pending || agent.status.is_terminal() {
            return;
        }
        if agent.paused
            || agent.limit.is_some()
            || matches!(agent.block_kind.as_deref(), Some("permission" | "dialog"))
        {
            return;
        }
        let (keep, sent_at, ready, turn_active) = {
            let mut model = self.model();
            let rt = model.rt(id);
            (
                rt.compact_keep.clone(),
                rt.compact_sent_at,
                rt.ready,
                rt.turn_active,
            )
        };
        if let Some((_, tx)) = self.listener(id) {
            if now - sent_at < SOCKET_RETRY_MS {
                return;
            }
            self.model().rt(id).compact_sent_at = now;
            let frame = ServerFrame::push(Push::Compact { instructions: keep });
            let _ = tx.send(frame).await;
            return;
        }
        if !ready || turn_active || now - sent_at < TERMINAL_RETRY_MS {
            return;
        }
        let Some(command) = runtime_for(agent.runtime).compact_command(keep.as_deref()) else {
            return;
        };
        let Ok(procs) = self.procs() else { return };
        {
            let mut model = self.model();
            let rt = model.rt(id);
            rt.compact_sent_at = now;
            // Codex records the compaction in its session file, after here.
            rt.compact_offset = rt
                .rollout
                .as_ref()
                .and_then(|path| std::fs::metadata(path).ok())
                .map_or(0, |meta| meta.len());
        }
        if let Err(e) = procs.submit_prompt(id, &command).await {
            tracing::warn!("asking {id} to compact failed: {e}");
        }
    }

    /// A compaction crew asked for was turned down by the runtime.
    pub(crate) fn compact_skipped(&self, id: &str, why: &str) {
        {
            let mut model = self.model();
            let rt = model.rt(id);
            rt.compact_keep = None;
            rt.compact_sent_at = 0;
        }
        self.update_agent(id, |a| a.compact_pending = false);
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.compact_skipped",
                format!(
                    "{id}'s context was not compacted: {}",
                    truncate_chars(&one_line(why), 200)
                ),
            )
            .agent(id)
            .warn(),
        );
    }

    /// The session's context was compacted: by crew's request, or by the
    /// runtime because the window was full. Either way the agent now works
    /// from a summary, so it is told where it stands.
    pub(crate) async fn context_compacted(&self, id: &str) {
        let now = now_ms();
        let Some(agent) = self.agent(id) else { return };
        let (asked, keep) = {
            let mut model = self.model();
            let rt = model.rt(id);
            if now - rt.compacted_at < SAME_COMPACTION_MS {
                return;
            }
            rt.compacted_at = now;
            rt.compact_sent_at = 0;
            rt.context_warned = false;
            (agent.compact_pending, rt.compact_keep.take())
        };
        self.update_agent(id, |a| {
            a.compact_pending = false;
            a.compactions += 1;
            // The old figure is no longer true; the next reading replaces it.
            a.context = None;
        });
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.compacted",
                format!(
                    "{id}'s context was compacted ({})",
                    if asked { "as queued" } else { "by its runtime" }
                ),
            )
            .agent(id)
            .data(json!({ "requested": asked, "compactions": agent.compactions + 1 })),
        );
        // The standing instructions are in the system prompt of every
        // runtime, which a compaction keeps: only where the work stands is told.
        let note = self.reorientation(&agent, keep.as_deref());
        let message = NewMessage::new("scheduler", id, MessageType::Status, note);
        if let Err(e) = self.send_message_quiet(message).await {
            tracing::warn!("telling {id} about its compaction failed: {}", e.message);
        }
        if !asked && !agent.orchestrator {
            let note = format!(
                "{id}'s context was compacted by its runtime (the window was full). It works from \
a summary now and was pointed back at its task."
            );
            self.notify_orchestrator(&note).await;
        }
    }

    /// What an agent is told after a compaction: who it is, what it was
    /// doing, and where the detail it lost can be read again.
    fn reorientation(&self, agent: &Agent, keep: Option<&str>) -> String {
        let mut note = format!(
            "Your conversation was just compacted: what came before is now a summary. You are \
still {} on this team.",
            agent.id
        );
        let task = {
            let model = self.model();
            model
                .tasks
                .iter()
                .find(|t| {
                    t.assigned_agent.as_deref() == Some(agent.id.as_str())
                        && matches!(t.status, TaskStatus::Working | TaskStatus::Blocked)
                })
                .cloned()
        };
        match &task {
            Some(task) => note.push_str(&format!(
                " Your open task is {} (\"{}\"): task_get(\"{}\") reads its card.",
                task.id, task.title, task.id
            )),
            None if agent.orchestrator => note.push_str(
                " You are the orchestrator: tasks_list and agents_list show where the plan stands.",
            ),
            None => note.push_str(" You have no open task right now."),
        }
        if let Some(keep) = keep {
            note.push_str(&format!("\nWhat was to be kept in mind: {keep}"));
        }
        note.push_str(
            "\nRead again what you need rather than guessing: tasks_list for the plan, task_get for a card, \
agent_inbox(all: true) for earlier messages, git_status and git_diff for work in progress. Then \
carry on where you left off; if nothing was in progress, there is nothing to do now.",
        );
        note
    }

    /// Claude Code sessions: read from the transcript what the hooks and the
    /// crew mod have not said. That is the case after a daemon restart (an
    /// idle session fires no hook), and wherever the mod is not loaded.
    /// What an agent was told, wrote and did: the newest `limit` entries
    /// of the session file its runtime keeps. Empty for an agent that has no
    /// such file (a shell agent, a session that has not started).
    pub(crate) async fn agent_transcript(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<TranscriptResult, RpcError> {
        let (agent, transcript, rollout) = {
            let mut model = self.model();
            let Some(agent) = model.agent(id).cloned() else {
                return Err(RpcError::not_found(format!("no agent '{id}'")));
            };
            let rt = model.rt(id);
            (agent, rt.transcript.clone(), rt.rollout.clone())
        };
        let Some(session) = agent.session_id.clone() else {
            return Ok(TranscriptResult::default());
        };
        let runtime = agent.runtime;
        let started = agent.started_at.unwrap_or_else(now_ms);
        let files = self
            .agent_files(id)
            .map_err(|e| RpcError::internal(format!("{e:#}")))?;
        let read = tokio::task::spawn_blocking(move || {
            let reported = |p: String| files.open_reported(std::path::Path::new(&p));
            let mut opened = match runtime {
                RuntimeKind::Claude => transcript
                    .and_then(reported)
                    .or_else(|| files.claude_transcript(&session)),
                RuntimeKind::Codex => rollout
                    .and_then(|p| files.open_reported(&p))
                    .or_else(|| files.codex_rollout(&session, started)),
                // The crew extension says where pi keeps the session.
                RuntimeKind::Pi => transcript.and_then(reported),
                RuntimeKind::Shell => None,
            }?;
            Some(crate::transcript::read_file(
                &mut opened.file,
                runtime,
                limit,
            ))
        })
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
        Ok(read.unwrap_or_default())
    }

    pub(crate) async fn transcript_housekeeping(&self) {
        let now = now_ms();
        let mut due = Vec::new();
        {
            let mut model = self.model();
            let agents = model.agents.clone();
            for agent in agents {
                if agent.runtime != RuntimeKind::Claude || agent.status.is_terminal() {
                    continue;
                }
                let Some(session) = agent.session_id.clone() else {
                    continue;
                };
                let rt = model.rt(&agent.id);
                let complete =
                    agent.running_model.is_some() && agent.effort.is_some() && rt.context_reported;
                let every = if complete {
                    TRANSCRIPT_RECHECK_MS
                } else {
                    TRANSCRIPT_EVERY_MS
                };
                if now - rt.facts_checked_at < every {
                    continue;
                }
                rt.facts_checked_at = now;
                due.push((agent.id.clone(), session, rt.transcript.clone()));
            }
        }
        for (id, session, known) in due {
            let Ok(files) = self.agent_files(&id) else {
                continue;
            };
            let read = tokio::task::spawn_blocking(move || {
                let mut opened = known
                    .and_then(|p| files.open_reported(std::path::Path::new(&p)))
                    .or_else(|| files.claude_transcript(&session))?;
                let tail = crate::limits::read_tail_of(&mut opened.file, TRANSCRIPT_TAIL_BYTES)?;
                Some((opened.path, crate::limits::claude_session_facts(&tail)?))
            })
            .await;
            let Ok(Some((path, facts))) = read else {
                // No transcript yet (the session is just starting): look
                // again on the next round rather than a whole interval on.
                self.model().rt(&id).facts_checked_at = 0;
                continue;
            };
            self.model().rt(&id).transcript = Some(path.to_string_lossy().into_owned());
            self.record_running(&id, facts.model, facts.effort);
            if let Some(switch) = &facts.switch {
                self.note_switch(&id, switch);
            }
            let Some(tokens) = facts.context_tokens else {
                continue;
            };
            // The transcript says how many tokens, not out of how many: the
            // window is the 1M one when crew started the session in it, else
            // whatever was last known (0: not known).
            let window = {
                let mut model = self.model();
                if model.rt(&id).context_reported {
                    continue;
                }
                let long = model.local.get(&id).is_some_and(|l| l.long_context);
                let known = model
                    .agent(&id)
                    .and_then(|a| a.context.as_ref())
                    .map_or(0, |c| c.window);
                // More in the window than the standard one holds: it can
                // only be the 1M one.
                if long || tokens > STANDARD_CONTEXT_TOKENS {
                    LONG_CONTEXT_TOKENS.max(known)
                } else {
                    known
                }
            };
            self.record_context(&id, tokens, window).await;
        }
    }

    /// The periodic part: start queued compactions whose moment has come.
    pub(crate) async fn context_housekeeping(&self) {
        let pending: Vec<String> = self
            .model()
            .agents
            .iter()
            .filter(|a| a.compact_pending && a.status != AgentStatus::Spawning)
            .map(|a| a.id.clone())
            .collect();
        for id in pending {
            self.try_compact(&id).await;
        }
    }
}
