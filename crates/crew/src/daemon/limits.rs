//! Usage limits: what the daemon does when an agent's account runs out.
//!
//! The agent is parked, not failed: it keeps its task, nothing is typed at
//! it, reminders and the unresponsive check leave it alone, and messages for
//! it wait on the bus. It goes on again when the runtime does so by itself,
//! when crew resumes it (at the reset time, or on a retry interval when the
//! runtime gave none), or when the operator runs `crew resume`: optionally
//! restarting the session so that a new login or key is picked up.

use std::sync::atomic::Ordering;

use crew_core::rpc::{Push, Quota, RpcError};
use crew_core::util::{fmt_elapsed, now_ms};
use crew_core::{Agent, AgentStatus, RuntimeKind, TaskStatus, UsageLimit};

use super::Daemon;
use crate::agents::runtime_for;
use crate::limits::{self, fmt_when, LimitInfo, LimitKind, QuotaReading};
use crate::persistence::NewEvent;

/// `Agent::block_kind` of an agent parked at a limit.
pub(crate) const BLOCK_KIND: &str = "usage_limit";

const RESUME_PROMPT: &str = "[crew] The usage limit that stopped you has lifted. Continue your \
current task where you left off. If you were in the middle of a step, check its state before \
repeating it.";
const RESTART_NOTICE: &str = "[crew] Your session was restarted after a usage limit; the account \
or key it runs on may have changed. Continue your current task where you left off. If you were \
in the middle of a step, check its state before repeating it.";

/// After a resume the screen still shows the old limit message for a moment.
const SCREEN_GRACE_MS: i64 = 20_000;
/// A session that reported nothing for this long has its screen read.
const SCREEN_QUIET_MS: i64 = 10_000;
const QUOTA_EVERY_MS: i64 = 30_000;
/// How often Claude Code's usage cache is read.
const USAGE_CACHE_EVERY_MS: i64 = 60_000;
/// How often Claude Code is asked to fetch the account's usage afresh (the
/// only source of a model's own weekly window): while a Claude member works,
/// while they all rest, and at most when something says a window just ran
/// out. Asking runs `claude -p /usage`: no model call, a few seconds.
const USAGE_REFRESH_WORKING_MS: i64 = 5 * 60_000;
const USAGE_REFRESH_RESTING_MS: i64 = 20 * 60_000;
const USAGE_REFRESH_SOON_MS: i64 = 60_000;
const USAGE_REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
const MIN_RETRY_MS: i64 = 60_000;
/// Resume a little after the stated reset: the clocks need not agree.
const RESET_MARGIN_MS: i64 = 60_000;
/// How much of a Codex session file is read for its newest quota reading.
const ROLLOUT_TAIL_BYTES: u64 = 256 * 1024;
/// How often a Codex session file is read while a compaction is awaited.
const COMPACT_WATCH_MS: i64 = 4_000;
/// How often it is read while a turn runs: a turn that ends in an error is
/// heard of only there.
const TURN_WATCH_MS: i64 = 10_000;

/// The line shown for a parked agent.
fn describe(limit: &UsageLimit, now: i64) -> String {
    let mut text = LimitKind::label(&limit.kind).to_string();
    if let Some(at) = limit.resets_at {
        text.push_str(&format!(" · resets {}", fmt_when(at, now)));
    }
    match limit.retry_at {
        Some(at) => text.push_str(&format!(" · resumes {}", fmt_when(at, now))),
        None => text.push_str(" · waiting for `crew resume`"),
    }
    text
}

/// By runtime, then session, week, the models' own weeks, the rest.
fn sort_quotas(quotas: &mut [Quota]) {
    quotas.sort_by_key(|q| {
        (
            q.runtime.to_string(),
            limits::window_rank(&q.window),
            q.window.clone(),
        )
    });
}

impl Daemon {
    /// An agent's runtime stopped at a usage limit: park the agent.
    pub(crate) async fn agent_limited(&self, id: &str, info: LimitInfo) {
        let now = now_ms();
        let Some(agent) = self.agent(id) else { return };
        if agent.status.is_terminal() {
            return;
        }
        let (auto_resume, retry_secs) = self
            .model()
            .session
            .team
            .as_ref()
            .map(|t| (t.limits.auto_resume, t.limits.limit_retry_secs))
            .unwrap_or((true, 1800));
        let attempts = {
            let mut model = self.model();
            let rt = model.rt(id);
            rt.turn_active = false;
            rt.parking_turn = false;
            rt.idle_since = None;
            rt.limit_attempts
        };
        let retry_at = |resets_at: Option<i64>| match info.kind {
            // Nothing lifts by waiting when the login itself is the problem.
            LimitKind::Auth => None,
            _ if !auto_resume => None,
            _ => Some(match resets_at {
                Some(at) => (at + RESET_MARGIN_MS).max(now + MIN_RETRY_MS),
                None => now + ((retry_secs as i64) * 1000).max(MIN_RETRY_MS),
            }),
        };
        if let Some(current) = &agent.limit {
            // Reported twice (the hook and the screen): keep the first, but
            // take a reset time if only the second has one.
            if current.resets_at.is_none() && info.resets_at.is_some() {
                let mut better = current.clone();
                better.resets_at = info.resets_at;
                better.retry_at = retry_at(info.resets_at);
                let text = describe(&better, now);
                self.update_agent(id, |a| {
                    a.activity = text;
                    a.limit = Some(better);
                });
            }
            return;
        }
        let limit = UsageLimit {
            kind: info.kind.as_str().to_string(),
            message: info.message.clone(),
            since: now,
            resets_at: info.resets_at,
            retry_at: retry_at(info.resets_at),
            attempts,
        };
        let text = describe(&limit, now);
        self.update_agent(id, |a| {
            a.status = AgentStatus::Blocked;
            a.block_kind = Some(BLOCK_KIND.to_string());
            a.activity = text.clone();
            a.limit = Some(limit.clone());
            a.last_activity_at = now;
        });
        let event = NewEvent::new(
            "daemon",
            "agent.limited",
            format!("{id} is paused: {text} ({})", info.message),
        )
        .agent(id);
        // The first time is news; running into it again after a resume is not.
        self.emit(if attempts == 0 { event.warn() } else { event });

        // The accounts' usage, fresh, says what ran out; a work style that
        // names a fallback for the model moves the agent there instead of
        // waiting for the reset.
        let by_style = info.kind == LimitKind::Usage && self.has_fallbacks(id);
        {
            let daemon = self.clone();
            let id = id.to_string();
            let claude = agent.runtime == RuntimeKind::Claude;
            tokio::spawn(async move {
                if claude {
                    daemon.refresh_claude_usage(true).await;
                }
                let parked = daemon.agent(&id).is_some_and(|a| a.limit.is_some());
                if by_style && parked && !daemon.fall_back_by_style(&id, None, None).await {
                    daemon.emit(
                        NewEvent::new(
                            "daemon",
                            "agent.model_policy",
                            format!("{id} waits at the limit: none of the work style's fallbacks for its model can be used now"),
                        )
                        .agent(&id),
                    );
                }
            });
        }

        if attempts == 0 && !agent.orchestrator && !by_style {
            let orchestrator_free = self
                .model()
                .orchestrator()
                .is_some_and(|o| o.limit.is_none());
            if orchestrator_free {
                let resumes = match limit.retry_at {
                    Some(at) => format!("crew resumes it at {}", fmt_when(at, now)),
                    None => "it waits for the operator to resume it".to_string(),
                };
                self.notify_orchestrator(&format!(
                    "Agent {id} ({}) hit a usage limit: {}. It is paused and keeps its task; \
{resumes}. Nothing needs doing. If the work should not wait, give its task to a member on the \
other model family (task_assign).",
                    crate::models::runtime_label(agent.runtime),
                    info.message
                ))
                .await;
            }
        }
    }

    /// The agent is no longer parked. Returns the limit it was parked at.
    pub(crate) fn limit_lifted(&self, id: &str, how: &str) -> Option<UsageLimit> {
        let limit = self.agent(id)?.limit?;
        let now = now_ms();
        self.update_agent(id, |a| {
            a.limit = None;
            if a.block_kind.as_deref() == Some(BLOCK_KIND) {
                a.block_kind = None;
                if a.status == AgentStatus::Blocked {
                    a.status = AgentStatus::Working;
                    a.activity.clear();
                }
            }
            a.last_activity_at = now;
        });
        // The time spent parked does not count against a task's timeout.
        let parked = now - limit.since;
        let timed: Vec<String> = {
            let model = self.model();
            model
                .tasks
                .iter()
                .filter(|t| {
                    t.assigned_agent.as_deref() == Some(id)
                        && t.status == TaskStatus::Working
                        && t.timeout_secs.is_some()
                })
                .map(|t| t.id.clone())
                .collect()
        };
        for task in timed {
            self.update_task(&task, |t| t.started_at = t.started_at.map(|s| s + parked));
        }
        self.emit(
            NewEvent::new(
                "daemon",
                "agent.limit_lifted",
                format!(
                    "{id} goes on after {} at a usage limit ({how})",
                    fmt_elapsed(parked)
                ),
            )
            .agent(id),
        );
        Some(limit)
    }

    /// Continue agents parked at a usage limit: the named ones, or all of
    /// them. `restart` starts each process afresh, resuming its session, so
    /// a changed login or key takes effect; otherwise the running session is
    /// simply prompted to go on.
    pub(crate) async fn resume_limited(
        &self,
        ids: &[String],
        restart: bool,
        automatic: bool,
    ) -> Result<Vec<String>, RpcError> {
        let targets: Vec<Agent> = {
            let model = self.model();
            if let Some(unknown) = ids.iter().find(|id| model.agent(id).is_none()) {
                return Err(RpcError::not_found(format!("no agent '{unknown}'")));
            }
            model
                .agents
                .iter()
                .filter(|a| !a.status.is_terminal())
                .filter(|a| {
                    if ids.is_empty() {
                        a.limit.is_some()
                    } else {
                        ids.contains(&a.id)
                    }
                })
                .cloned()
                .collect()
        };
        let procs = self.procs()?;
        let how = match (automatic, restart) {
            (true, _) => "resumed automatically",
            (false, true) => "restarted by the operator",
            (false, false) => "resumed by the operator",
        };
        let mut resumed = Vec::new();
        for agent in targets {
            let id = agent.id.as_str();
            // Without a restart there is nothing to do for an agent that is
            // not parked.
            if agent.limit.is_none() && !restart {
                continue;
            }
            let now = now_ms();
            let was_parked = self.limit_lifted(id, how).is_some();
            {
                let mut model = self.model();
                let rt = model.rt(id);
                if was_parked {
                    rt.limit_attempts += 1;
                }
                rt.limit_grace_until = now + SCREEN_GRACE_MS;
            }
            if restart && !automatic {
                self.restart_agent_refreshing_toolbox(id, Some(RESTART_NOTICE.to_string()))
                    .await?;
            } else if restart || !procs.is_alive(id) {
                self.restart_agent_with(id, Some(RESTART_NOTICE.to_string()))
                    .await?;
            } else {
                self.model().rt(id).turn_active = true;
                self.update_agent(id, |a| {
                    a.status = AgentStatus::Working;
                    a.activity = "resuming after a usage limit".to_string();
                    a.last_activity_at = now;
                });
                if !self.inject(id, RESUME_PROMPT).await {
                    self.emit(
                        NewEvent::new(
                            "daemon",
                            "agent.resume_failed",
                            format!("could not prompt {id} to continue; try `crew resume --restart {id}`"),
                        )
                        .agent(id)
                        .warn(),
                    );
                }
            }
            self.request_delivery(id);
            resumed.push(agent.id.clone());
        }
        Ok(resumed)
    }

    /// The periodic part: resume agents whose time has come, read the
    /// screens of sessions that went quiet, refresh Codex's quota figures.
    pub(crate) async fn limit_housekeeping(&self) {
        let now = now_ms();
        let daemon = self.clone();
        tokio::spawn(async move { daemon.refresh_claude_usage(false).await });
        self.usage_cache_housekeeping(now).await;
        self.fallback_housekeeping().await;
        let Ok(procs) = self.procs() else { return };
        let mut due = Vec::new();
        let mut screens = Vec::new();
        let mut rollouts = Vec::new();
        let mut gone = Vec::new();
        {
            let mut model = self.model();
            let agents = model.agents.clone();
            for agent in &agents {
                if agent.status.is_terminal() {
                    if agent.limit.is_some() {
                        gone.push(agent.id.clone());
                    }
                    continue;
                }
                if agent.paused || agent.runtime == RuntimeKind::Shell {
                    continue;
                }
                let codex_parent_confirmed = model.local.get(&agent.id).is_some_and(|local| {
                    local.session_confirmed && local.codex_parent_session == agent.session_id
                }) && model
                    .rt
                    .get(&agent.id)
                    .is_none_or(|rt| rt.codex_pending_thread.is_none());
                let rt = model.rt(&agent.id);
                // A Codex session says how full its context is only in its
                // session file; while a compaction is awaited, look often.
                let every = if agent.compact_pending {
                    COMPACT_WATCH_MS
                } else if rt.turn_active || agent.status == AgentStatus::Working {
                    TURN_WATCH_MS
                } else {
                    QUOTA_EVERY_MS
                };
                if agent.runtime == RuntimeKind::Codex
                    && codex_parent_confirmed
                    && now - rt.quota_checked_at >= every
                {
                    if let Some(thread) = &agent.session_id {
                        rt.quota_checked_at = now;
                        // A compaction that was asked for and typed is looked for.
                        let awaited = (agent.compact_pending && rt.compact_sent_at > 0)
                            .then_some(rt.compact_offset);
                        // A turn that began long ago is not in the tail.
                        let whole = agent.effort.is_none();
                        rollouts.push((
                            agent.id.clone(),
                            thread.clone(),
                            agent.started_at.unwrap_or(now),
                            rt.rollout.clone(),
                            awaited,
                            whole,
                        ));
                    }
                }
                if let Some(limit) = &agent.limit {
                    if limit.retry_at.is_some_and(|at| at <= now) {
                        due.push(agent.id.clone());
                    }
                    continue;
                }
                if agent.status == AgentStatus::Working
                    && rt.ready
                    && now >= rt.limit_grace_until
                    && now - agent.last_activity_at.max(rt.limit_checked_at) >= SCREEN_QUIET_MS
                {
                    rt.limit_checked_at = now;
                    screens.push((agent.id.clone(), agent.runtime));
                }
            }
        }
        for id in gone {
            self.update_agent(&id, |a| a.limit = None);
        }
        for (id, runtime) in screens {
            if !procs.is_alive(&id) {
                continue;
            }
            let screen = procs.visible(&id, 60).await.unwrap_or_default();
            if let Some(info) = runtime_for(runtime).detect_limit(&screen, now) {
                self.agent_limited(&id, info).await;
                continue;
            }
            self.dismiss_notice(&id, &screen).await;
        }
        if !due.is_empty() {
            if let Err(e) = self.resume_limited(&due, false, true).await {
                tracing::warn!("automatic resume failed: {}", e.message);
            }
        }
        for (id, thread, started, known, awaited, whole) in rollouts {
            let Ok(files) = self.agent_files(&id) else {
                continue;
            };
            let read = tokio::task::spawn_blocking(move || {
                // Opened once, through the agent's files: a sandboxed agent's are under
                // its own tree and are untrusted.
                let mut opened = known
                    .and_then(|p| files.open_reported(&p))
                    .or_else(|| files.codex_rollout(&thread, started))?;
                let tail = limits::read_tail_of(&mut opened.file, ROLLOUT_TAIL_BYTES)?;
                let compacted = awaited.is_some_and(|offset| {
                    opened
                        .file
                        .try_clone()
                        .is_ok_and(|f| limits::codex_compacted_since_in(f, offset))
                });
                let running = limits::codex_running(&tail).or_else(|| {
                    whole
                        .then(|| limits::codex_running_of(opened.file.try_clone().ok()?))
                        .flatten()
                });
                let turn = limits::codex_last_turn(&tail);
                let path = opened.path;
                Some((
                    path,
                    limits::codex_quota(&tail),
                    limits::codex_context(&tail),
                    compacted,
                    running,
                    turn,
                ))
            })
            .await;
            if let Ok(Some((path, readings, context, compacted, running, turn))) = read {
                self.model().rt(&id).rollout = Some(path);
                if let Some((model, effort)) = running {
                    self.record_running(&id, model, effort);
                }
                self.record_quota(RuntimeKind::Codex, readings);
                if compacted {
                    self.context_compacted(&id).await;
                }
                if let Some((tokens, window)) = context {
                    self.record_context(&id, tokens, window).await;
                }
                if let Some(turn) = turn {
                    self.codex_turn_seen(&id, turn).await;
                }
            }
        }
    }

    /// Keep the newest usage figures of a runtime's account for the dashboard.
    pub(crate) fn record_quota(&self, runtime: RuntimeKind, readings: Vec<QuotaReading>) {
        if readings.is_empty() {
            return;
        }
        let now = now_ms();
        let changed = {
            let mut model = self.model();
            let same = |q: &Quota, r: &QuotaReading| {
                q.window == r.window
                    && q.percent_used == r.percent_used
                    && q.resets_at == r.resets_at
            };
            // A model's own window (`week/<model>`) is never reported this
            // way; it comes from Claude Code's usage cache and stays.
            let own = |q: &Quota| q.runtime == runtime && !q.window.contains('/');
            let before: Vec<&Quota> = model.quotas.iter().filter(|q| own(q)).collect();
            let changed = before.len() != readings.len()
                || before.iter().zip(&readings).any(|(q, r)| !same(q, r));
            if changed {
                model.quotas.retain(|q| !own(q));
                model.quotas.extend(readings.into_iter().map(|r| Quota {
                    runtime,
                    window: r.window,
                    percent_used: r.percent_used,
                    resets_at: r.resets_at,
                    updated_at: now,
                }));
                sort_quotas(&mut model.quotas);
            }
            changed
        };
        if changed {
            self.save_quotas();
            self.push(Push::Session(Box::new(self.session_info())));
        }
    }

    /// Claude Code's own usage cache, read now and then. It has the windows
    /// the sessions do not report (a model's own weekly one), and all of
    /// them before any session has reported.
    /// Have Claude Code fetch the account's usage, which it writes to its
    /// usage cache, and read the cache at once. The sessions themselves
    /// report only the session and all-models weekly windows; a model's own
    /// weekly window (Fable's) is only ever in that cache, and Claude Code
    /// writes it only when it shows usage. `soon`: something says a window
    /// just ran out (a session fell back to another model, or stopped at a
    /// limit), so ask now unless that was done in the last minute.
    pub(crate) async fn refresh_claude_usage(&self, soon: bool) {
        self.refresh_usage_cache(soon, false).await;
    }

    /// Ask Claude Code for the account's usage now (unless that was done in
    /// the last minute), whether or not a Claude agent is running: before
    /// the team is staffed, what each model has left decides who goes on it.
    pub(crate) async fn refresh_claude_usage_now(&self) {
        if crate::models::Availability::runtime(RuntimeKind::Claude).is_ok() {
            self.refresh_usage_cache(true, true).await;
        }
    }

    async fn refresh_usage_cache(&self, soon: bool, without_agents: bool) {
        let now = now_ms();
        let (claude, working) = {
            let model = self.model();
            let live: Vec<&Agent> = model
                .agents
                .iter()
                .filter(|a| a.runtime == RuntimeKind::Claude && !a.status.is_terminal())
                .collect();
            (
                !live.is_empty(),
                live.iter().any(|a| a.status == AgentStatus::Working),
            )
        };
        if !claude && !without_agents {
            return;
        }
        let every = match (soon, working) {
            (true, _) => USAGE_REFRESH_SOON_MS,
            (false, true) => USAGE_REFRESH_WORKING_MS,
            (false, false) => USAGE_REFRESH_RESTING_MS,
        };
        if now - self.usage_refreshed_at.load(Ordering::Relaxed) < every
            || self.usage_refreshing.swap(true, Ordering::AcqRel)
        {
            return;
        }
        // A local command: Claude Code answers it without the model, so it
        // costs no usage. No MCP servers, no session file left behind.
        let mut cmd = tokio::process::Command::new("claude");
        cmd.args([
            "-p",
            "/usage",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--mcp-config",
            r#"{"mcpServers":{}}"#,
        ])
        .current_dir(&self.workspace)
        .env_remove(crew_core::paths::IDENTITY_ENV)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
        let ran = match cmd.spawn() {
            Ok(mut child) => tokio::time::timeout(USAGE_REFRESH_TIMEOUT, child.wait())
                .await
                .is_ok_and(|r| r.is_ok()),
            Err(_) => false,
        };
        self.usage_refreshed_at.store(now_ms(), Ordering::Relaxed);
        self.usage_refreshing.store(false, Ordering::Release);
        if ran {
            // Read what it wrote now rather than on the next round.
            self.usage_cache_checked_at.store(0, Ordering::Relaxed);
            self.usage_cache_housekeeping(now_ms()).await;
        }
    }

    async fn usage_cache_housekeeping(&self, now: i64) {
        use std::sync::atomic::Ordering;
        if now - self.usage_cache_checked_at.load(Ordering::Relaxed) < USAGE_CACHE_EVERY_MS {
            return;
        }
        self.usage_cache_checked_at.store(now, Ordering::Relaxed);
        let Some(path) = limits::claude_usage_cache_path() else {
            return;
        };
        let read = tokio::task::spawn_blocking(move || {
            let text = std::fs::read_to_string(path).ok()?;
            limits::claude_cached_quota(&text)
        })
        .await;
        if let Ok(Some((fetched_at, readings))) = read {
            self.merge_cached_quota(fetched_at, readings);
        }
    }

    /// Take the cache's figures where they are the freshest there are: a
    /// window a session reported since keeps the session's figure.
    fn merge_cached_quota(&self, fetched_at: i64, readings: Vec<QuotaReading>) {
        let runtime = RuntimeKind::Claude;
        let changed = {
            let mut model = self.model();
            let before = model.quotas.len();
            // A model's own window that the cache no longer lists is gone.
            let listed: Vec<&str> = readings.iter().map(|r| r.window.as_str()).collect();
            model.quotas.retain(|q| {
                !(q.runtime == runtime
                    && q.window.contains('/')
                    && !listed.contains(&q.window.as_str()))
            });
            let mut changed = model.quotas.len() != before;
            for reading in readings {
                let quota = Quota {
                    runtime,
                    window: reading.window,
                    percent_used: reading.percent_used,
                    resets_at: reading.resets_at,
                    updated_at: fetched_at,
                };
                match model
                    .quotas
                    .iter_mut()
                    .find(|q| q.runtime == runtime && q.window == quota.window)
                {
                    Some(known) if known.updated_at >= fetched_at => {}
                    Some(known) => {
                        *known = quota;
                        changed = true;
                    }
                    None => {
                        model.quotas.push(quota);
                        changed = true;
                    }
                }
            }
            if changed {
                sort_quotas(&mut model.quotas);
            }
            changed
        };
        if changed {
            self.save_quotas();
            self.push(Push::Session(Box::new(self.session_info())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parked_agent_says_when_it_goes_on() {
        let now = now_ms();
        let mut limit = UsageLimit {
            kind: "usage".into(),
            message: "You've hit your usage limit.".into(),
            since: now,
            resets_at: None,
            retry_at: None,
            attempts: 0,
        };
        assert_eq!(
            describe(&limit, now),
            "usage limit reached · waiting for `crew resume`"
        );
        limit.resets_at = Some(now + 3_600_000);
        limit.retry_at = Some(now + 3_660_000);
        let text = describe(&limit, now);
        assert!(
            text.starts_with("usage limit reached · resets ") && text.contains(" · resumes "),
            "{text}"
        );
        limit.kind = "auth".into();
        limit.resets_at = None;
        limit.retry_at = None;
        assert_eq!(
            describe(&limit, now),
            "sign-in needed · waiting for `crew resume`"
        );
    }
}
