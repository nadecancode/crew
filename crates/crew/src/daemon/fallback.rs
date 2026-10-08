//! What each agent runs on, and a session that no longer runs on the model
//! its member was given.
//!
//! A runtime moves a session to another model on its own: Claude Code when
//! the model's weekly window is used up (`Fable 5.1 requires usage credits`),
//! when the model is overloaded or unavailable, or after a refusal. The
//! session goes on, on a model nobody chose for the work. crew notices from
//! what the session reports it runs on (the crew mod with every request, the
//! transcript otherwise), marks the agent, and tells the orchestrator what
//! happened and what it can do: leave it, put the member on a model of its
//! choosing with revised instructions (`agent_set_model`), or add a member on
//! a model that is not affected and move the work there.

use crew_core::rpc::RpcError;
use crew_core::types::{model_display_name, short_model, ModelFallback};
use crew_core::util::now_ms;
use crew_core::RuntimeKind;

use super::handlers::internal;
use super::Daemon;
use crate::limits::{fmt_when, window_label, ModelSwitch};
use crate::persistence::NewEvent;

/// Longest note handed on with a model change.
const NOTE_CHARS: usize = 2000;

impl Daemon {
    /// What `id`'s session says it runs on; a part the report leaves out
    /// stays as it was. A model other than the one the member was given
    /// is a fallback, and news.
    pub(crate) fn record_running(&self, id: &str, model: Option<String>, effort: Option<String>) {
        let Some(agent) = self.agent(id) else { return };
        let model = model.filter(|m| agent.running_model.as_ref() != Some(m));
        let effort = effort.filter(|e| agent.effort.as_ref() != Some(e));
        if model.is_none() && effort.is_none() {
            return;
        }
        let changed = model.clone();
        self.update_agent(id, |a| {
            if let Some(model) = model {
                // What the session itself says it runs on names it best.
                a.model_name = Some(model_display_name(&model));
                a.running_model = Some(model);
            }
            if let Some(effort) = effort {
                a.effort = Some(effort);
            }
        });
        if let Some(running) = changed {
            self.model_reported(id, &running);
        }
    }

    /// The model the member was given, as an id to compare with: the
    /// catalog's id for the name in the team, or (a member on the runtime's
    /// default) the first model its session reported. A model the catalog
    /// does not know (a proxy's, say) is not checked: what its session
    /// reports it as is anyone's guess.
    fn intended_model(&self, id: &str) -> Option<String> {
        let agent = self.agent(id)?;
        match &agent.model {
            Some(model) => crate::models::Catalog::load()
                .find(model)
                .map(|profile| profile.id.clone())
                .filter(|id| !id.is_empty()),
            None => self.model().rt(id).first_model.clone(),
        }
    }

    /// The session reports it runs on `running`.
    fn model_reported(&self, id: &str, running: &str) {
        {
            let mut model = self.model();
            let rt = model.rt(id);
            if rt.first_model.is_none() {
                rt.first_model = Some(running.to_string());
            }
        }
        let Some(intended) = self.intended_model(id) else {
            return;
        };
        let Some(agent) = self.agent(id) else { return };
        if short_model(running) == short_model(&intended) {
            // A work style's fallback stays marked until crew moves it back.
            if agent.fallback.as_ref().is_some_and(|f| !f.policy) {
                self.update_agent(id, |a| a.fallback = None);
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "agent.model_restored",
                        format!("{id} is back on {}", model_display_name(&intended)),
                    )
                    .agent(id),
                );
                let daemon = self.clone();
                let text = format!(
                    "[crew] Agent {id} is back on {}.",
                    model_display_name(&intended)
                );
                tokio::spawn(async move { daemon.notify_orchestrator(&text).await });
            }
            return;
        }
        if agent.fallback.as_ref().is_some_and(|f| !f.policy) {
            // Still off its model (on a third one, say): noted once already.
            return;
        }
        let fallback = ModelFallback {
            from: model_display_name(&intended),
            note: String::new(),
            since: now_ms(),
            policy: false,
        };
        self.update_agent(id, |a| a.fallback = Some(fallback));
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.model_fallback",
                format!(
                    "{id} runs on {} instead of {}",
                    model_display_name(running),
                    model_display_name(&intended)
                ),
            )
            .agent(id)
            .warn(),
        );
        // Why, in the runtime's words, comes from the transcript; then the
        // orchestrator hears.
        let daemon = self.clone();
        let id = id.to_string();
        let running = running.to_string();
        tokio::spawn(async move {
            daemon.read_switch_note(&id).await;
            // The model's window likely just ran out: what the orchestrator
            // hears about the accounts' usage should say so.
            daemon.refresh_claude_usage(true).await;
            // The work style may already say what the agent goes on.
            if daemon.fall_back_by_style(&id, Some(&running), None).await {
                return;
            }
            daemon.brief_orchestrator_on_fallback(&id).await;
        });
    }

    /// Fetch the runtime's own word on the switch into the fallback note,
    /// when the transcript has one about the model the session now runs on.
    pub(crate) async fn read_switch_note(&self, id: &str) {
        let Some(agent) = self.agent(id) else { return };
        if agent.runtime != RuntimeKind::Claude {
            return;
        }
        let (session, known) = {
            let mut model = self.model();
            (agent.session_id.clone(), model.rt(id).transcript.clone())
        };
        let Some(session) = session else { return };
        let Ok(files) = self.agent_files(id) else {
            return;
        };
        let read = tokio::task::spawn_blocking(move || {
            let mut opened = known
                .and_then(|p| files.open_reported(std::path::Path::new(&p)))
                .or_else(|| files.claude_transcript(&session))?;
            let tail = crate::limits::read_tail_of(
                &mut opened.file,
                super::context::TRANSCRIPT_TAIL_BYTES,
            )?;
            crate::limits::claude_model_switch(&tail)
        })
        .await;
        if let Ok(Some(switch)) = read {
            self.note_switch(id, &switch);
        }
    }

    /// Keep the runtime's note on a switch, when it is about the model the
    /// session now runs on and nothing was noted yet.
    pub(crate) fn note_switch(&self, id: &str, switch: &ModelSwitch) {
        let Some(agent) = self.agent(id) else { return };
        let Some(fallback) = &agent.fallback else {
            return;
        };
        let running = agent.running_model.as_deref().unwrap_or_default();
        if !fallback.note.is_empty() || short_model(&switch.to) != short_model(running) {
            return;
        }
        let note = crew_core::util::one_line(&switch.note);
        if note.is_empty() {
            return;
        }
        self.update_agent(id, |a| {
            if let Some(fallback) = a.fallback.as_mut() {
                fallback.note = note;
            }
        });
    }

    /// The accounts' usage windows in one line, for a decision on where
    /// work should go: `Fable week 100% (resets Oct 4, 12:00 AM) · week 74% ·
    /// Codex week 62%`.
    fn usage_summary(&self) -> String {
        let now = now_ms();
        let quotas = self.model().quotas.clone();
        let mut parts: Vec<String> = Vec::new();
        for runtime in [RuntimeKind::Claude, RuntimeKind::Codex] {
            for q in quotas
                .iter()
                .filter(|q| q.runtime == runtime && q.window != "5h")
            {
                let mut part = format!(
                    "{}{} {:.0}%",
                    if runtime == RuntimeKind::Codex {
                        "Codex "
                    } else {
                        ""
                    },
                    window_label(&q.window),
                    q.percent_used.max(0.0)
                );
                if let Some(at) = q.resets_at.filter(|at| *at > now) {
                    if q.percent_used >= 90.0 {
                        part.push_str(&format!(" (resets {})", fmt_when(at, now)));
                    }
                }
                parts.push(part);
            }
        }
        parts.join(" · ")
    }

    /// Tell the orchestrator that a member runs on another model, and what
    /// it can do about it.
    async fn brief_orchestrator_on_fallback(&self, id: &str) {
        let Some(agent) = self.agent(id) else { return };
        let Some(fallback) = agent.fallback.clone() else {
            return;
        };
        let orchestrator_free = self
            .model()
            .orchestrator()
            .is_some_and(|o| o.limit.is_none());
        if !orchestrator_free {
            return;
        }
        let running = agent
            .model_name
            .clone()
            .or_else(|| agent.running_model.as_deref().map(model_display_name))
            .unwrap_or_else(|| "another model".to_string());
        let why = if fallback.note.is_empty() {
            String::new()
        } else {
            format!(" ({})", fallback.note)
        };
        let usage = self.usage_summary();
        let usage = if usage.is_empty() {
            String::new()
        } else {
            format!(" Usage now: {usage}.")
        };
        let text = if agent.orchestrator {
            format!(
                "[crew] You now run on {running} instead of {}{why}. Carry on. If the judgment calls \
ahead need the stronger model, say so to the operator with operator_ask; the operator can \
put you on another model or CLI from the dashboard (O) or with `crew agent model {id} <model>`.{usage}",
                fallback.from
            )
        } else {
            format!(
                "[crew] Agent {id} ({}) now runs on {running} instead of {}{why}. It keeps its task \
and goes on; its work from here is {running}'s. Decide: (1) let it go on, if {running} is good \
enough for its role; (2) agent_set_model(id=\"{id}\", model=..., note=...) to put it on a model \
you choose, with instructions revised for that model (asking for {} again before its window \
resets only falls back again); or (3) agent_add a member on a model that is not affected (the \
other model family, say) and task_assign its task there.{usage}",
                crate::models::runtime_label(agent.runtime),
                fallback.from,
                fallback.from,
            )
        };
        self.notify_orchestrator(&text).await;
    }

    /// Put an agent on another model, with `note` as the first thing it
    /// hears. On its own CLI the session is restarted on the model, resumed.
    /// A model of another CLI (or another `runtime`) moves the agent there:
    /// a session cannot follow it to another CLI, so a fresh one starts,
    /// handed where the work stands. An empty `model` with a `runtime` is
    /// that CLI's own default. Either way the roster keeps the choice.
    pub(crate) async fn set_model(
        &self,
        who: &str,
        by_operator: bool,
        id: &str,
        model: &str,
        runtime: Option<RuntimeKind>,
        note: Option<String>,
    ) -> Result<(), RpcError> {
        let agent = self
            .agent(id)
            .ok_or_else(|| RpcError::not_found(format!("no agent '{id}'")))?;
        if agent.runtime == RuntimeKind::Shell {
            return Err(RpcError::invalid(format!(
                "{id} is a shell agent; it runs no model"
            )));
        }
        if runtime == Some(RuntimeKind::Shell) {
            return Err(RpcError::invalid("an agent cannot be moved onto a shell"));
        }
        // What the orchestrator runs on, and which CLI anyone runs on, is
        // the operator's call; the orchestrator may only move a member to
        // another model of the member's own CLI.
        if !by_operator && agent.orchestrator {
            return Err(RpcError::forbidden(
                "only the operator can change the orchestrator's model or CLI; ask with operator_ask",
            ));
        }
        let model = model.trim();
        let malformed =
            model.len() > 120 || model.chars().any(|c| c.is_whitespace() || c.is_control());
        if malformed || (model.is_empty() && runtime.is_none()) {
            return Err(RpcError::invalid("not a model name"));
        }
        let note = note
            .map(|n| crew_core::util::truncate_chars(n.trim(), NOTE_CHARS))
            .filter(|n| !n.is_empty());
        let (current, wanted) = (agent.runtime, model.to_string());
        let target = runtime.unwrap_or(current);
        let (assigned, name) = tokio::task::spawn_blocking(move || {
            let catalog = crate::models::Catalog::load();
            let wanted = Some(wanted.as_str()).filter(|m| !m.is_empty());
            let assigned = catalog.assign(Some(target), wanted, target, None);
            let name = assigned.model.as_deref().map(|m| {
                let id = catalog
                    .find(m)
                    .map(|profile| profile.id.as_str())
                    .filter(|id| !id.is_empty())
                    .unwrap_or(m);
                model_display_name(id)
            });
            (assigned, name)
        })
        .await
        .map_err(internal)?;
        // A catalog model runs on its own CLI, whatever was asked.
        if let (Some(_), Some(mismatch)) = (runtime, &assigned.note) {
            return Err(RpcError::invalid(mismatch.clone()));
        }
        if assigned.runtime != current {
            if !by_operator {
                return Err(RpcError::forbidden(format!(
                    "{} runs on {}, not {}; only the operator can move a member to another CLI — \
agent_add a member for it instead",
                    if model.is_empty() { "that" } else { model },
                    crate::models::runtime_label(assigned.runtime),
                    crate::models::runtime_label(current)
                )));
            }
            // The toolbox is mapped onto the new CLI: an item it requires that
            // would not load there refuses the move.
            self.toolbox_switch_check(&agent, assigned.runtime)?;
            return self
                .move_to_runtime(
                    who,
                    &agent,
                    assigned.runtime,
                    assigned.model,
                    name,
                    note,
                    true,
                )
                .await;
        }
        let new = assigned.model;
        let before = agent.model.clone();
        self.update_agent(id, |a| {
            a.model = new.clone();
            a.model_name = name.clone();
            a.running_model = None;
            a.effort = None;
            a.fallback = None;
        });
        {
            let mut m = self.model();
            if let Some(local) = m.local.get_mut(id) {
                local.spec.model = new.clone();
                // A model that was refused the 1M window is not this one.
                local.no_long_context = false;
                // Chosen, not fallen back to: nothing to go back to.
                local.home = None;
            }
            if agent.orchestrator {
                if let Some(team) = m.session.team.as_mut() {
                    team.orchestrator.model = new.clone();
                }
            }
            m.rt(id).first_model = None;
        }
        self.save_local(id);
        if agent.orchestrator {
            self.save_session();
        }
        self.persist_roster();
        let shown = |model: Option<&str>| model.unwrap_or("the default model").to_string();
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.model_set",
                format!(
                    "{id} moves from {} to {} ({who}){}",
                    shown(before.as_deref()),
                    shown(new.as_deref()),
                    note.as_deref()
                        .map(|n| format!(": {}", crew_core::util::one_line(n)))
                        .unwrap_or_default()
                ),
            )
            .agent(id),
        );
        let notice = format!(
            "[crew] Your session was restarted on {} by {who}.{} Continue your current task where you \
left off; if you were in the middle of a step, check its state before repeating it.",
            name.or(new).unwrap_or_else(|| "its default model".to_string()),
            note.map(|n| format!(" {n}")).unwrap_or_default()
        );
        self.restart_agent_with(id, Some(notice)).await
    }

    /// Move an agent to another CLI. Its session stays behind (each CLI
    /// keeps its own), so the agent starts afresh there with a handoff: the
    /// plan, the team, its task and its predecessor's last replies.
    /// `persist`: the agent is declared on the new CLI from now on (the
    /// roster and the team say so); otherwise it is there for a while (a
    /// work style's fallback) and its declaration stays as it was.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn move_to_runtime(
        &self,
        who: &str,
        agent: &crew_core::Agent,
        runtime: RuntimeKind,
        model: Option<String>,
        name: Option<String>,
        note: Option<String>,
        persist: bool,
    ) -> Result<(), RpcError> {
        let id = agent.id.as_str();
        self.toolbox_runtime_check(agent, runtime)?;
        let settings = {
            let model_guard = self.model();
            match (model_guard.session.team.as_ref(), model_guard.local.get(id)) {
                (Some(team), Some(local)) => {
                    let declared = local.spec.runtime.unwrap_or(agent.runtime);
                    let mut spec = local.spec.clone();
                    if runtime != declared || persist {
                        spec.permission_mode = None;
                        spec.allowed_tools.clear();
                        spec.extra_args.clear();
                    }
                    spec.runtime = Some(runtime);
                    spec.model = model.clone();
                    Some(
                        super::supervisor::agent_settings(team, &spec, agent.orchestrator)
                            .map_err(|error| RpcError::invalid(format!("{error:#}")))?,
                    )
                }
                _ => None,
            }
        };
        // Refused here, not as a failed agent after the old session is gone.
        tokio::task::spawn_blocking(move || crate::models::Availability::runtime(runtime))
            .await
            .map_err(internal)?
            .map_err(|why| {
                RpcError::invalid(format!(
                    "cannot move {id} to {}: {why}",
                    crate::models::runtime_label(runtime)
                ))
            })?;
        // Read before anything changes.
        let last_words = self
            .agent_transcript(id, 200)
            .await
            .map(|t| crate::agents::prompt::last_words(&t.entries))
            .unwrap_or_default();
        let describe = |runtime: RuntimeKind, model: Option<&str>| {
            let cli = crate::models::runtime_label(runtime);
            match model {
                Some(model) => format!("{cli} on {model}"),
                None => format!("{cli} on its default model"),
            }
        };
        let from = describe(
            agent.runtime,
            agent.model_name.as_deref().or(agent.model.as_deref()),
        );
        let to = describe(runtime, name.as_deref().or(model.as_deref()));
        // Before the model guard: `vcs()` may lock the model itself.
        let vcs = self.vcs().kind();
        let brief = {
            let m = self.model();
            crate::agents::prompt::handoff(&crate::agents::prompt::Handoff {
                agent,
                from: &from,
                to: &to,
                who,
                note: note.as_deref(),
                tasks: &m.tasks,
                agents: &m.agents,
                last_words: &last_words,
                vcs,
            })
        };
        self.update_agent(id, |a| {
            a.runtime = runtime;
            a.model = model.clone();
            a.model_name = name.clone();
            a.running_model = None;
            a.effort = None;
            a.fallback = None;
            a.context = None;
            a.compact_pending = false;
            a.compactions = 0;
            a.subagents = 0;
            // Claude Code and pi take the id crew chooses; Codex reports its own.
            a.session_id = matches!(runtime, RuntimeKind::Claude | RuntimeKind::Pi)
                .then(|| uuid::Uuid::new_v4().to_string());
        });
        {
            let mut m = self.model();
            if let Some(local) = m.local.get_mut(id) {
                // Permission modes, tool names and flags are one CLI's own:
                // they stay with the declaration, on the CLI it names.
                let declared = local.spec.runtime.unwrap_or(agent.runtime);
                let mut spec = local.spec.clone();
                if runtime != declared || persist {
                    spec.permission_mode = None;
                    spec.allowed_tools.clear();
                    spec.extra_args.clear();
                }
                spec.runtime = Some(runtime);
                spec.model = model.clone();
                if let Some(settings) = &settings {
                    local.settings = settings.clone();
                }
                if persist {
                    local.spec = spec;
                    local.home = None;
                }
                local.session_confirmed = false;
                local.no_long_context = false;
                local.long_context = false;
                local.handoff = Some(brief);
            }
            if agent.orchestrator && persist {
                if let Some(team) = m.session.team.as_mut() {
                    team.orchestrator.runtime = Some(runtime);
                    team.orchestrator.model = model.clone();
                }
            }
            let rt = m.rt(id);
            rt.first_model = None;
            rt.transcript = None;
            rt.rollout = None;
        }
        self.save_local(id);
        if agent.orchestrator && persist {
            self.save_session();
        }
        self.persist_roster();
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.runtime_set",
                format!(
                    "{id} moves from {from} to {to} ({who}), in a fresh session handed the work{}",
                    note.as_deref()
                        .map(|n| format!(": {}", crew_core::util::one_line(n)))
                        .unwrap_or_default()
                ),
            )
            .agent(id),
        );
        self.restart_agent_with(id, None).await
    }
}

#[cfg(test)]
mod vcs_lock_tests {
    use crew_core::RuntimeKind;

    /// `Daemon::vcs()` locks the model while it restores the backend after a
    /// restart: the handoff must not read it with the model already held.
    #[tokio::test]
    async fn a_handoff_with_the_backend_not_yet_restored_does_not_lock_the_model_twice() {
        let temp = tempfile::tempdir().unwrap();
        let running = crate::daemon::start(crate::daemon::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = running.daemon.clone();
        let agent: crew_core::Agent = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "role": "work", "status": "working", "runtime": "claude",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        daemon.model().agents.push(agent.clone());
        assert!(daemon.vcs.read().unwrap().is_none(), "nothing restored yet");
        let moved = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            daemon.move_to_runtime(
                "operator",
                &agent,
                RuntimeKind::Shell,
                None,
                None,
                None,
                false,
            ),
        )
        .await;
        assert!(moved.is_ok(), "the handoff deadlocked on the model lock");
    }
}
