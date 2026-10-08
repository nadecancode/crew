//! The work style's model fallbacks: what an agent goes on while its model
//! cannot be used, and back.
//!
//! A style (or team file) says `defaults.model_fallbacks: { fable: [opus] }`.
//! When an agent's model is out — its own weekly window used up (Claude
//! Code's "Fable 5.1 requires usage credits"), or its account's window —
//! crew puts the agent on the first of those models that can be started
//! here and is not out itself, without asking anyone, and back on its own
//! model once the window has reset. It looks:
//! - when the runtime moves a session off its model by itself (crew keeps
//!   it there when that is the fallback, or moves it to the fallback);
//! - when a turn stops at a usage limit (instead of waiting for the reset);
//! - when an agent is started while its model is known to be out.
//!
//! A move within a CLI resumes the session on the other model; a move to
//! another CLI starts a fresh session there, handed the work. The roster
//! keeps the agent's own model: a fallback is for the time being. What is
//! out comes from the accounts' usage windows crew already follows (the
//! sessions' reports and Claude Code's usage cache). From another CLI the
//! agent goes back between tasks, so once its own model can be used again
//! it is handed no new task until it is back: a queue of work would
//! otherwise keep it away for good. The same holds for an agent moved for a
//! refusal (turns.rs), which goes back once that work is done.

use crew_core::rpc::Quota;
use crew_core::types::{model_display_name, ModelFallback};
use crew_core::util::{fmt_when, now_ms};
use crew_core::RuntimeKind;
use serde::{Deserialize, Serialize};

use super::Daemon;
use crate::persistence::NewEvent;

/// What an agent on a fallback goes back to, and when.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct HomeModel {
    pub runtime: RuntimeKind,
    pub model: Option<String>,
    /// The window that was used up, as the dashboard names it (`Fable week`).
    pub window: String,
    /// When it resets, if that is known.
    pub resets_at: Option<i64>,
    pub moved_at: i64,
    /// Moved because its model refused a turn (Codex's kind for it): it goes
    /// back once that work is done, not when a window resets.
    #[serde(default)]
    pub refused: Option<String>,
}

/// Without a reset time, the agent's own model is tried again this long
/// after the move.
const RETRY_HOME_MS: i64 = 60 * 60 * 1000;
/// A window is taken to have reset this long after the time it gave.
const RESET_MARGIN_MS: i64 = 2 * 60 * 1000;
/// A reading at 100% with no reset time counts this long.
const UNDATED_MS: i64 = 6 * 60 * 60 * 1000;
/// An agent due back from another CLI and free of tasks waits for its turn
/// to end, or for its session to be quiet this long: one whose turns crew
/// does not see end (its hooks failing, say) goes back all the same.
const HOME_TURN_GRACE_MS: i64 = 60 * 1000;
/// Who a move is said to be by.
const BY: &str = "the work style";

/// A usage window that rules a model out until it resets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Out {
    /// As the dashboard names it: `Fable week`, `session`.
    pub window: String,
    pub resets_at: Option<i64>,
}

/// The window among `quotas` that rules out `name` (a model's proper name,
/// `Fable 5.1`) on `runtime` at `now`: a model's own weekly window
/// (`week/Fable`), or one of the account's, used up and not reset yet.
pub(crate) fn out_of(
    quotas: &[Quota],
    runtime: RuntimeKind,
    name: Option<&str>,
    now: i64,
) -> Option<Out> {
    let name = name.map(str::to_lowercase);
    quotas
        .iter()
        .filter(|q| q.runtime == runtime && q.percent_used >= 100.0)
        .filter(|q| match q.resets_at {
            Some(at) => at > now,
            None => now - q.updated_at < UNDATED_MS,
        })
        .find(|q| match q.window.strip_prefix("week/") {
            Some(family) => name
                .as_deref()
                .is_some_and(|n| n.starts_with(&family.to_lowercase())),
            // A spend cap stops usage beyond the plan, not the plan's own.
            None => q.window != "spend",
        })
        .map(|q| Out {
            window: crate::limits::window_label(&q.window),
            resets_at: q.resets_at,
        })
}

/// A catalog model, as substitutes are chosen among.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub key: String,
    pub runtime: RuntimeKind,
    pub tier: String,
    /// Its proper name: `Fable 5.1`.
    pub name: String,
}

/// The models closest to `model` among `candidates` that `usable` lets
/// through, best first, at most three: the same tier first (another
/// family's, the model's own being the one that is out), then each tier
/// down, the model's own family before the other; a stronger tier last.
pub(crate) fn closest(
    candidates: &[Candidate],
    model: &Candidate,
    usable: impl Fn(&Candidate) -> bool,
) -> Vec<String> {
    const TIERS: [&str; 4] = ["frontier", "strong", "balanced", "fast"];
    let rank = |tier: &str| TIERS.iter().position(|t| *t == tier).unwrap_or(TIERS.len());
    let home = rank(&model.tier);
    let mut scored: Vec<(usize, bool, usize, &Candidate)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.key != model.key && usable(c))
        .map(|(i, c)| {
            let tier = rank(&c.tier);
            let distance = if tier >= home {
                tier - home
            } else {
                home - tier + TIERS.len()
            };
            let other_family = c.runtime != model.runtime && distance > 0;
            (distance, other_family, i, c)
        })
        .collect();
    scored.sort_by_key(|(distance, other, i, _)| (*distance, *other, *i));
    scored
        .into_iter()
        .take(3)
        .map(|(.., c)| c.key.clone())
        .collect()
}

/// A model to fall back to: its CLI, its name to start it with, its
/// proper name.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    runtime: RuntimeKind,
    model: String,
    name: String,
}

impl Daemon {
    /// The proper name of `model`, through the catalog.
    pub(crate) fn proper_name(model: &str) -> String {
        let catalog = crate::models::Catalog::load();
        let id = catalog
            .find(model)
            .map(|p| p.id.as_str())
            .filter(|id| !id.is_empty())
            .unwrap_or(model);
        model_display_name(id)
    }

    /// The model `id` runs on as the catalog names it: the one it was given,
    /// or (on the runtime's default) the one its session reported first.
    fn model_of(&self, id: &str) -> Option<String> {
        let agent = self.agent(id)?;
        agent.model.clone().or_else(|| {
            let first = self.model().rt(id).first_model.clone()?;
            crate::models::Catalog::load()
                .find(&first)
                .map(|p| p.key.clone())
        })
    }

    /// Whether the work style names a fallback for the model `id` runs on.
    pub(crate) fn has_fallbacks(&self, id: &str) -> bool {
        self.model_of(id)
            .is_some_and(|model| !self.fallbacks_of(&model).is_empty())
    }

    /// Whether the work style names a fallback for `model` that can be
    /// started now.
    pub(crate) async fn has_style_fallback(&self, model: &str, runtime: RuntimeKind) -> bool {
        self.fallback_target(model, runtime, Some(model))
            .await
            .is_some()
    }

    /// The work style's fallbacks for `model`, in order.
    fn fallbacks_of(&self, model: &str) -> Vec<String> {
        let table = self
            .model()
            .session
            .team
            .as_ref()
            .map(|t| t.defaults.model_fallbacks.clone())
            .unwrap_or_default();
        if table.is_empty() {
            return Vec::new();
        }
        let catalog = crate::models::Catalog::load();
        let key = |m: &str| {
            catalog
                .find(m)
                .map_or_else(|| m.to_lowercase(), |p| p.key.to_lowercase())
        };
        let wanted = key(model);
        table
            .into_iter()
            .find(|(from, _)| key(from) == wanted)
            .map(|(_, instead)| instead)
            .unwrap_or_default()
    }

    /// What rules out `model` on `runtime` now, if anything does.
    pub(crate) fn model_out(&self, runtime: RuntimeKind, model: Option<&str>) -> Option<Out> {
        let name = model.map(Self::proper_name);
        let quotas = self.model().quotas.clone();
        out_of(&quotas, runtime, name.as_deref(), now_ms())
    }

    /// The first of `home`'s fallbacks to go on now: one that can be
    /// started here, is not out itself, and is not `current`.
    async fn fallback_target(
        &self,
        home: &str,
        home_runtime: RuntimeKind,
        current: Option<&str>,
    ) -> Option<Target> {
        let chain = self.fallbacks_of(home);
        if chain.is_empty() {
            return None;
        }
        let current = current.map(str::to_lowercase);
        let candidates = tokio::task::spawn_blocking(move || {
            let catalog = crate::models::Catalog::load();
            let available = crate::models::Availability::detect();
            chain
                .into_iter()
                .filter_map(|name| {
                    let profile = catalog.find(&name);
                    let runtime = profile.and_then(|p| p.runtime).unwrap_or(home_runtime);
                    let usable = match profile {
                        Some(profile) => available.check(profile).is_ok(),
                        None => crate::models::Availability::runtime(runtime).is_ok(),
                    };
                    let model = profile.map_or(name.clone(), |p| p.key.clone());
                    let id = profile
                        .map(|p| p.id.as_str())
                        .filter(|id| !id.is_empty())
                        .unwrap_or(&model);
                    let name = model_display_name(id);
                    usable.then_some(Target {
                        runtime,
                        model,
                        name,
                    })
                })
                .collect::<Vec<_>>()
        })
        .await
        .ok()?;
        candidates.into_iter().find(|t| {
            Some(t.model.to_lowercase()) != current
                && self.model_out(t.runtime, Some(&t.model)).is_none()
        })
    }

    /// Put `id` on its work style's fallback when its model is out and the
    /// style names one that is not. `running`: the model the runtime has
    /// already moved the session to, if it did. `starting`: the agent is
    /// about to be started (`Some(fresh)`: without a session to resume), so
    /// nothing is restarted. Returns whether it moved.
    ///
    /// Boxed: a move restarts the agent, and a start looks here again.
    pub(crate) fn fall_back_by_style<'a>(
        &'a self,
        id: &'a str,
        running: Option<&'a str>,
        starting: Option<bool>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(self.fall_back(id, running, starting))
    }

    async fn fall_back(&self, id: &str, running: Option<&str>, starting: Option<bool>) -> bool {
        let Some(agent) = self.agent(id) else {
            return false;
        };
        if agent.runtime == RuntimeKind::Shell {
            return false;
        }
        let Some(current) = self.model_of(id) else {
            return false;
        };
        let Some(out) = self.model_out(agent.runtime, Some(&current)) else {
            return false;
        };
        let home = self.model().local.get(id).and_then(|l| l.home.clone());
        let (home_runtime, home_model) = match &home {
            Some(home) => (
                home.runtime,
                home.model.clone().unwrap_or_else(|| current.clone()),
            ),
            None => (agent.runtime, current.clone()),
        };
        let Some(target) = self
            .fallback_target(&home_model, home_runtime, Some(&current))
            .await
        else {
            return false;
        };
        let other_cli = target.runtime != agent.runtime;
        if other_cli && starting == Some(false) {
            // About to resume its session: that cannot happen on another CLI.
            return false;
        }
        if let Err(error) = self.toolbox_runtime_check(&agent, target.runtime) {
            self.notify_orchestrator(&format!(
                "{id} stays on its current runtime: {}",
                error.message
            ))
            .await;
            return false;
        }
        let target_settings = if other_cli {
            let resolved = {
                let model = self.model();
                match (model.session.team.as_ref(), model.local.get(id)) {
                    (Some(team), Some(local)) => {
                        let mut spec = local.spec.clone();
                        spec.permission_mode = None;
                        spec.allowed_tools.clear();
                        spec.extra_args.clear();
                        spec.runtime = Some(target.runtime);
                        Some(super::supervisor::agent_settings(
                            team,
                            &spec,
                            agent.orchestrator,
                        ))
                    }
                    _ => None,
                }
            };
            match resolved {
                Some(Ok(settings)) => Some(settings),
                Some(Err(error)) => {
                    self.notify_orchestrator(&format!("{id} cannot change runtime: {error:#}"))
                        .await;
                    return false;
                }
                None => None,
            }
        } else {
            None
        };
        let now = now_ms();
        let home_name = Self::proper_name(&home_model);
        let resets = out
            .resets_at
            .map(|at| format!(" (it resets {})", fmt_when(at, now)))
            .unwrap_or_default();
        let note = format!(
            "{} is used up{resets}; the work style puts it on {} until then",
            out.window, target.name
        );
        // Where it goes back to stays the model it was given, through every
        // fallback; the latest reset is the one to wait for.
        let record = HomeModel {
            runtime: home_runtime,
            model: Some(home_model.clone()),
            window: out.window.clone(),
            resets_at: match (home.as_ref().and_then(|h| h.resets_at), out.resets_at) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            },
            moved_at: home.as_ref().map_or(now, |h| h.moved_at),
            refused: home.as_ref().and_then(|h| h.refused.clone()),
        };
        let marker = ModelFallback {
            from: home_name.clone(),
            note: note.clone(),
            since: now,
            policy: true,
        };
        // The runtime may have moved the session there already.
        let target_id = crate::models::Catalog::load()
            .find(&target.model)
            .map(|p| p.id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| target.model.clone());
        let short = crew_core::types::short_model;
        let already_there = running.is_some_and(|r| short(r) == short(&target_id));
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.model_policy",
                format!(
                    "{id} moves from {} to {} for now: {note}",
                    Self::proper_name(&current),
                    target.name
                ),
            )
            .agent(id),
        );
        if other_cli && starting.is_none() {
            let _ = self
                .move_to_runtime(
                    BY,
                    &agent,
                    target.runtime,
                    Some(target.model.clone()),
                    Some(target.name.clone()),
                    Some(format!("{note}.")),
                    false,
                )
                .await;
        } else {
            self.update_agent(id, |a| {
                if other_cli {
                    a.runtime = target.runtime;
                    a.session_id = matches!(target.runtime, RuntimeKind::Claude | RuntimeKind::Pi)
                        .then(|| uuid::Uuid::new_v4().to_string());
                    a.context = None;
                }
                a.model = Some(target.model.clone());
                a.model_name = Some(target.name.clone());
                if !already_there {
                    a.running_model = None;
                    a.effort = None;
                }
            });
            {
                let mut m = self.model();
                if let Some(local) = m.local.get_mut(id) {
                    local.no_long_context = false;
                    if other_cli {
                        if let Some(settings) = &target_settings {
                            local.settings = settings.clone();
                        }
                        local.session_confirmed = false;
                    }
                }
                if !already_there {
                    m.rt(id).first_model = None;
                }
            }
        }
        {
            let mut m = self.model();
            if let Some(local) = m.local.get_mut(id) {
                local.home = Some(record);
            }
        }
        self.save_local(id);
        // The roster keeps the model it was given (a move to another CLI
        // wrote it out before this was known).
        self.persist_roster();
        self.update_agent(id, |a| a.fallback = Some(marker));
        if !agent.orchestrator {
            self.notify_orchestrator(&format!(
                "[crew] Agent {id} moves from {} to {} for now, as the work style says: {} is used \
up{resets}. It goes back to {home_name} by itself once that resets. Nothing needs deciding.",
                Self::proper_name(&current),
                target.name,
                out.window,
            ))
            .await;
        }
        if starting.is_none() && !other_cli && !already_there {
            let notice = format!(
                "[crew] Your session was restarted on {}: {note}. Continue your current task where \
you left off; if you were in the middle of a step, check its state before repeating it.",
                target.name
            );
            let _ = self.restart_agent_with(id, Some(notice)).await;
        }
        true
    }

    /// The catalog's models that can be started here, as substitutes are
    /// chosen among.
    async fn candidates(&self) -> Vec<Candidate> {
        tokio::task::spawn_blocking(|| {
            let catalog = crate::models::Catalog::load();
            let available = crate::models::Availability::detect();
            catalog
                .models
                .iter()
                .map(|(m, _)| m)
                .filter(|m| !m.disabled && m.runtime.is_some() && available.check(m).is_ok())
                .map(|m| Candidate {
                    key: m.key.clone(),
                    runtime: m.runtime.unwrap_or(RuntimeKind::Claude),
                    tier: m.tier.clone(),
                    name: model_display_name(if m.id.is_empty() { &m.key } else { &m.id }),
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Why `model` (on `runtime`) cannot be started now and what to take
    /// instead, or `None` when it can.
    pub(crate) async fn out_now(
        &self,
        runtime: RuntimeKind,
        model: Option<&str>,
    ) -> Option<(Out, Vec<String>)> {
        let out = self.model_out(runtime, model)?;
        let candidates = self.candidates().await;
        let quotas = self.model().quotas.clone();
        let now = now_ms();
        let free = |c: &Candidate| out_of(&quotas, c.runtime, Some(&c.name), now).is_none();
        let key = model.map(|m| {
            crate::models::Catalog::load()
                .find(m)
                .map_or(m.to_string(), |p| p.key.clone())
        });
        let substitutes = candidates
            .iter()
            .find(|c| Some(&c.key) == key.as_ref())
            .map(|me| closest(&candidates, me, free))
            .unwrap_or_default();
        Some((out, substitutes))
    }

    /// The accounts' usage as the orchestrator is told it when it staffs the
    /// team: each window, which of the catalog's models are out until their
    /// window resets, and the closest models to take instead.
    pub(crate) async fn usage_report(&self) -> String {
        use std::fmt::Write as _;
        let candidates = self.candidates().await;
        let quotas = self.model().quotas.clone();
        let now = now_ms();
        let mut windows: Vec<String> = Vec::new();
        for runtime in [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Pi] {
            let mut parts: Vec<String> = Vec::new();
            for q in quotas.iter().filter(|q| q.runtime == runtime) {
                let mut part = format!(
                    "{} {:.0}%",
                    crate::limits::window_label(&q.window),
                    q.percent_used.max(0.0)
                );
                if let Some(at) = q.resets_at.filter(|at| *at > now && q.percent_used >= 90.0) {
                    let _ = write!(part, " (resets {})", fmt_when(at, now));
                }
                parts.push(part);
            }
            if !parts.is_empty() {
                windows.push(format!(
                    "{} {}",
                    crate::models::runtime_label(runtime),
                    parts.join(" · ")
                ));
            }
        }
        let mut out = if windows.is_empty() {
            String::from("Usage now: not known yet.")
        } else {
            format!("Usage now: {}.", windows.join("; "))
        };
        let free = |c: &Candidate| out_of(&quotas, c.runtime, Some(&c.name), now).is_none();
        let used_up: Vec<(&Candidate, Out)> = candidates
            .iter()
            .filter_map(|c| out_of(&quotas, c.runtime, Some(&c.name), now).map(|o| (c, o)))
            .collect();
        if used_up.is_empty() {
            if !windows.is_empty() {
                out.push_str(" Every model in the catalog can be started.");
            }
            return out;
        }
        out.push_str(
            "\nOut until their window resets — staff none of these now; for a role that needs one, \
take the first model after it:",
        );
        for (model, why) in used_up {
            let instead = closest(&candidates, model, free);
            let resets = why
                .resets_at
                .map(|at| format!(", resets {}", fmt_when(at, now)))
                .unwrap_or_default();
            let instead = if instead.is_empty() {
                "nothing comparable is free".to_string()
            } else {
                instead.join(", then ")
            };
            let _ = write!(
                out,
                "\n- {} ({} used up{resets}): {instead}",
                model.key, why.window
            );
        }
        out
    }

    /// Keep agents off a model that is out and put them back once it is
    /// not: an agent whose model is known to be out moves to the work
    /// style's fallback between turns, before it runs into the limit; one
    /// on a fallback goes back once a reading since the move (or the reset
    /// time) says its own model can be used — within a CLI between turns
    /// (the session resumes), from another CLI between tasks (a fresh
    /// session again, handed the work).
    pub(crate) async fn fallback_housekeeping(&self) {
        let now = now_ms();
        let (quotas, between_turns, away) = {
            let mut m = self.model();
            let quotas = m.quotas.clone();
            let agents = m.agents.clone();
            let between_turns: Vec<String> = agents
                .iter()
                .filter(|a| a.runtime != RuntimeKind::Shell && !a.status.is_terminal() && !a.paused)
                .filter(|a| a.status != crew_core::AgentStatus::Spawning && a.limit.is_none())
                .filter(|a| !m.rt(&a.id).turn_active)
                .map(|a| a.id.clone())
                .collect();
            let away: Vec<(String, HomeModel)> = m
                .local
                .iter()
                .filter_map(|(id, local)| local.home.clone().map(|home| (id.clone(), home)))
                .collect();
            (quotas, between_turns, away)
        };
        // Nothing can be out while no window is used up.
        if quotas.iter().any(|q| q.percent_used >= 100.0) {
            for id in &between_turns {
                let home = away.iter().any(|(away, _)| away == id);
                let runtime = self.agent(id).map(|a| a.runtime);
                if !home
                    && runtime.is_some_and(|r| {
                        quotas
                            .iter()
                            .any(|q| q.runtime == r && q.percent_used >= 100.0)
                    })
                {
                    self.fall_back_by_style(id, None, None).await;
                }
            }
        }
        for (id, home) in away {
            let Some(agent) = self.agent(&id) else {
                continue;
            };
            let on_task = crate::tasks::active_task(&self.model().tasks, &id).is_some();
            let waits = self.waits_home(&agent, &home, &quotas, now);
            // Going back between tasks and free of them (the scheduler hands
            // it none now): since when, and since when quiet.
            let quiet_since = {
                let mut m = self.model();
                let rt = m.rt(&id);
                rt.home_free_since = (waits && !on_task).then(|| rt.home_free_since.unwrap_or(now));
                rt.home_free_since
                    .map(|since| since.max(rt.last_output_at).max(agent.last_activity_at))
            };
            // It goes once its turn is over; if crew does not see that turn
            // end, once the session has been quiet a while all the same.
            let waited_out = !agent.status.is_terminal()
                && !agent.paused
                && agent.status != crew_core::AgentStatus::Spawning
                && quiet_since.is_some_and(|since| now - since >= HOME_TURN_GRACE_MS);
            // Moved for a refusal: back once that work is done.
            if home.refused.is_some() {
                if waited_out || self.refused_work_done(&id, &home, now) {
                    self.go_home(&agent, home).await;
                }
                continue;
            }
            let other_cli = home.runtime != agent.runtime;
            let due = if other_cli {
                waits
            } else {
                self.home_due(&home, &quotas, now)
            };
            if !due || agent.status.is_terminal() || agent.paused || other_cli && on_task {
                continue;
            }
            let resting = between_turns.contains(&id) || agent.limit.is_some();
            if !resting && !waited_out {
                continue;
            }
            self.go_home(&agent, home).await;
        }
    }

    /// Whether `agent`, on a fallback, goes back to `home` before it takes
    /// another task: from another CLI once its own model can be used again;
    /// after a refusal (on any CLI) once the work it took over is done (a
    /// turn of its since the move has ended, or a task of its was finished),
    /// unless it stays for refusing again and again.
    fn waits_home(
        &self,
        agent: &crew_core::Agent,
        home: &HomeModel,
        quotas: &[Quota],
        now: i64,
    ) -> bool {
        match home.refused {
            Some(_) => {
                let (refusals, done) = {
                    let m = self.model();
                    let id = agent.id.as_str();
                    let turned =
                        m.rt.get(id)
                            .is_some_and(|rt| rt.turn_ended_at > home.moved_at);
                    let finished = m.tasks.iter().any(|t| {
                        t.assigned_agent.as_deref() == Some(id)
                            && t.completed_at.is_some_and(|at| at > home.moved_at)
                    });
                    let refusals = m.local.get(id).map_or(0, |l| l.refusals);
                    (refusals, turned || finished)
                };
                done && refusals < super::turns::STAY_AFTER
                    && self
                        .model_out(home.runtime, home.model.as_deref())
                        .is_none()
            }
            None => home.runtime != agent.runtime && self.home_due(home, quotas, now),
        }
    }

    /// Whether the model an agent on a fallback goes back to can be used
    /// again: its window has reset, or a reading of its CLI's windows came
    /// since the move, or (with no reset time known) a while has passed;
    /// and the readings do not say it is out.
    fn home_due(&self, home: &HomeModel, quotas: &[Quota], now: i64) -> bool {
        let reset = home.resets_at.is_some_and(|at| now >= at + RESET_MARGIN_MS);
        let read_since = quotas
            .iter()
            .any(|q| q.runtime == home.runtime && q.updated_at > home.moved_at);
        let waited = home.resets_at.is_none() && now >= home.moved_at + RETRY_HOME_MS;
        (reset || read_since || waited)
            && self
                .model_out(home.runtime, home.model.as_deref())
                .is_none()
    }

    /// The members free of tasks that go back to their own model before
    /// they take another (`waits_home`), so the scheduler hands them none
    /// until they are back.
    pub(crate) fn homeward(&self) -> Vec<String> {
        let now = now_ms();
        let (quotas, away) = {
            let m = self.model();
            let away: Vec<(crew_core::Agent, HomeModel)> = m
                .agents
                .iter()
                .filter(|a| !a.orchestrator && crate::tasks::active_task(&m.tasks, &a.id).is_none())
                .filter_map(|a| Some((a.clone(), m.local.get(&a.id)?.home.clone()?)))
                .collect();
            (m.quotas.clone(), away)
        };
        away.into_iter()
            .filter(|(agent, home)| self.waits_home(agent, home, &quotas, now))
            .map(|(agent, _)| agent.id)
            .collect()
    }

    async fn go_home(&self, agent: &crew_core::Agent, home: HomeModel) {
        let id = agent.id.as_str();
        let name = home.model.as_deref().map(Self::proper_name);
        let shown = name
            .clone()
            .unwrap_or_else(|| "its default model".to_string());
        let reason = match &home.refused {
            Some(_) => "the work it refused is done".to_string(),
            None => format!("{} has reset", home.window),
        };
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.model_restored",
                format!("{id} is back on {shown}: {reason}"),
            )
            .agent(id),
        );
        {
            let mut m = self.model();
            if let Some(local) = m.local.get_mut(id) {
                local.home = None;
            }
            m.rt(id).home_free_since = None;
        }
        self.save_local(id);
        if home.runtime != agent.runtime {
            let note = match &home.refused {
                Some(_) => format!("The work {shown} refused is done, so you are back on {shown}."),
                None => format!("{reason}, so you are back on {shown}."),
            };
            let _ = self
                .move_to_runtime(
                    BY,
                    agent,
                    home.runtime,
                    home.model.clone(),
                    name,
                    Some(note),
                    false,
                )
                .await;
        } else {
            self.update_agent(id, |a| {
                a.model = home.model.clone();
                a.model_name = name.clone();
                a.running_model = None;
                a.effort = None;
                a.fallback = None;
            });
            self.model().rt(id).first_model = None;
            let notice = format!(
                "[crew] {reason}: your session was restarted on {shown}, your own model again. \
Continue your current task where you left off; if you were in the middle of a step, check its \
state before repeating it."
            );
            let _ = self.restart_agent_with(id, Some(notice)).await;
        }
        if !agent.orchestrator {
            self.notify_orchestrator(&format!("[crew] Agent {id} is back on {shown}: {reason}."))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota(runtime: RuntimeKind, window: &str, percent: f32, resets_at: Option<i64>) -> Quota {
        Quota {
            runtime,
            window: window.to_string(),
            percent_used: percent,
            resets_at,
            updated_at: 1_000,
        }
    }

    #[test]
    fn the_closest_free_model_is_the_same_tier_of_the_other_family_then_the_next_tier_down() {
        let model = |key: &str, runtime, tier: &str| Candidate {
            key: key.into(),
            runtime,
            tier: tier.into(),
            name: key.into(),
        };
        let (claude, codex) = (RuntimeKind::Claude, RuntimeKind::Codex);
        let catalog = vec![
            model("fable", claude, "frontier"),
            model("opus", claude, "strong"),
            model("sonnet", claude, "balanced"),
            model("haiku", claude, "fast"),
            model("gpt-6-astra", codex, "frontier"),
            model("gpt-6.1-sol", codex, "balanced"),
            model("gpt-6-luna", codex, "fast"),
        ];
        let all = |_: &Candidate| true;
        // Fable out: Astra is as strong; Opus is the next tier of its own family.
        assert_eq!(
            closest(&catalog, &catalog[0], all),
            ["gpt-6-astra", "opus", "sonnet"]
        );
        // Codex out too: only Claude models are left.
        assert_eq!(
            closest(&catalog, &catalog[0], |c| c.runtime == claude),
            ["opus", "sonnet", "haiku"]
        );
        // Astra out: Fable is as strong; Codex has no model of the next tier,
        // so Opus comes before Sol.
        assert_eq!(
            closest(&catalog, &catalog[4], all),
            ["fable", "opus", "gpt-6.1-sol"]
        );
        // Sonnet out: Sol is the same tier; nothing weaker beats something as strong.
        assert_eq!(
            closest(&catalog, &catalog[2], all),
            ["gpt-6.1-sol", "haiku", "gpt-6-luna"]
        );
        // The fastest tier has nothing below: a stronger model, last.
        assert_eq!(
            closest(&catalog, &catalog[3], |c| c.key != "gpt-6-luna"),
            ["sonnet", "gpt-6.1-sol", "opus"]
        );
    }

    #[test]
    fn a_model_is_out_while_its_own_window_or_its_accounts_is_used_up() {
        let now = 10_000;
        let claude = RuntimeKind::Claude;
        let quotas = vec![
            quota(claude, "5h", 40.0, Some(now + 1)),
            quota(claude, "week", 82.0, Some(now + 1)),
            quota(claude, "week/Fable", 100.0, Some(now + 5_000)),
            quota(RuntimeKind::Codex, "week", 100.0, Some(now + 9_000)),
        ];
        // Fable's own window rules Fable out, not Opus.
        assert_eq!(
            out_of(&quotas, claude, Some("Fable 5.1"), now),
            Some(Out {
                window: "Fable week".into(),
                resets_at: Some(now + 5_000)
            })
        );
        assert_eq!(out_of(&quotas, claude, Some("Opus 5.5"), now), None);
        // Codex's account window rules out every Codex model.
        assert_eq!(
            out_of(&quotas, RuntimeKind::Codex, Some("GPT 6 Astra"), now)
                .unwrap()
                .window,
            "week"
        );
        // Once its reset time has passed, a reading at 100% no longer counts.
        assert_eq!(
            out_of(&quotas, claude, Some("Fable 5.1"), now + 6_000),
            None
        );
        // An account window at its limit rules out every model of the CLI;
        // a spend cap does not.
        let spent = vec![
            quota(claude, "spend", 100.0, None),
            quota(claude, "5h", 100.0, Some(now + 1)),
        ];
        assert_eq!(
            out_of(&spent, claude, Some("Opus 5.5"), now)
                .unwrap()
                .window,
            "session"
        );
        assert_eq!(out_of(&spent[..1], claude, Some("Opus 5.5"), now), None);
        // A reading with no reset time counts for a while.
        let undated = vec![quota(claude, "week/Fable", 100.0, None)];
        assert!(out_of(&undated, claude, Some("Fable 5.1"), 2_000).is_some());
        assert!(out_of(&undated, claude, Some("Fable 5.1"), 1_000 + UNDATED_MS).is_none());
    }
}

#[cfg(test)]
mod homeward_tests {
    use super::*;

    const MINUTE: i64 = 60 * 1000;

    /// A daemon with `x` on Codex for now, its own CLI (a shell, so going
    /// back needs nothing installed) usable again since its window reset,
    /// and in a turn crew has not seen end.
    async fn away_on_codex() -> (tempfile::TempDir, super::super::Running) {
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let agent: crew_core::Agent = serde_json::from_value(serde_json::json!({
            "id": "x", "name": "x", "role": "work", "status": "working", "runtime": "codex",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        let now = now_ms();
        let daemon = &running.daemon;
        let mut m = daemon.model();
        m.agents.push(agent);
        m.local.entry("x".into()).or_default().home = Some(HomeModel {
            runtime: RuntimeKind::Shell,
            model: None,
            window: "week".into(),
            resets_at: Some(now - 5 * MINUTE),
            moved_at: now - 30 * MINUTE,
            refused: None,
        });
        m.rt("x").turn_active = true;
        drop(m);
        (temp, running)
    }

    fn restored(daemon: &Daemon) -> bool {
        daemon
            .store
            .recent_events(Some("x"), 100)
            .unwrap()
            .iter()
            .any(|e| e.event_type == "agent.model_restored")
    }

    #[tokio::test]
    async fn an_agent_due_back_from_another_cli_takes_no_new_task_and_goes_once_free() {
        let (_temp, running) = away_on_codex().await;
        let daemon = running.daemon.clone();
        // Due back: the scheduler holds new tasks for it.
        assert_eq!(daemon.homeward(), vec!["x".to_string()]);

        // Free of tasks, its turn not seen to end: it waits for that turn.
        daemon.fallback_housekeeping().await;
        assert!(!restored(&daemon));
        let since = daemon.model().rt("x").home_free_since;
        assert!(since.is_some(), "free since the first look");

        // A session still putting out output is taken to be in its turn.
        let long_ago = now_ms() - HOME_TURN_GRACE_MS - 1;
        {
            let mut m = daemon.model();
            let rt = m.rt("x");
            rt.home_free_since = Some(long_ago);
            rt.last_output_at = now_ms();
        }
        daemon.fallback_housekeeping().await;
        assert!(!restored(&daemon));

        // A turn crew never sees end (its hooks failing, say) does not keep
        // it away for good: quiet that long, it goes.
        daemon.model().rt("x").last_output_at = long_ago;
        daemon.fallback_housekeeping().await;
        assert!(restored(&daemon), "back once quiet");
        assert!(daemon.model().local["x"].home.is_none());
        assert!(daemon.homeward().is_empty());
    }

    #[tokio::test]
    async fn an_agent_moved_for_a_refusal_waits_for_new_tasks_once_that_work_is_done() {
        let (_temp, running) = away_on_codex().await;
        let daemon = running.daemon.clone();
        let moved_at = now_ms() - 30 * MINUTE;
        if let Some(home) = daemon.model().local.get_mut("x").unwrap().home.as_mut() {
            home.refused = Some("cyber_policy".into());
        }
        // It has not done the work it took over yet: it takes its tasks.
        assert!(daemon.homeward().is_empty());

        // A task of its finished since the move: the next waits for it.
        let task: crew_core::Task = serde_json::from_value(serde_json::json!({
            "id": "t", "title": "t", "description": "", "assigned_agent": "x", "status": "done",
            "dependencies": [], "created_at": 0, "completed_at": moved_at + MINUTE
        }))
        .unwrap();
        daemon.model().tasks.push(task);
        assert_eq!(daemon.homeward(), vec!["x".to_string()]);

        // One that refused again and again stays where it is.
        daemon.model().local.get_mut("x").unwrap().refusals = super::super::turns::STAY_AFTER;
        assert!(daemon.homeward().is_empty());
    }
}
