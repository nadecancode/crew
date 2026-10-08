//! Keeping an idle Claude Code member's prompt cache warm (opt-in,
//! `limits.keep_cache_warm_secs`; notes/prompt-cache.md, D2 revision).
//!
//! Claude Code writes its cache for an hour, and a read renews that hour. A
//! member that rests longer has its whole context written again, at twice
//! the input price, when work comes. Some time after a resting member's last
//! turn, crew puts one line of its own into the session asking for a
//! one-word reply: that turn reads the context (at a twentieth to a fortieth
//! of the input price) and renews it. At most `keep_cache_warm_max` times per
//! rest, since a member that never works again makes every one a loss.
//!
//! A keep-alive is not work and is not a message. It goes through the same
//! path as crew's reminders, so nothing reaches an inbox and nothing is
//! asked of anyone. It neither counts as nor resets a reminder or a nudge.
//! Its turn needs no telling apart from others: only a member Waiting on
//! open work gets one, and such a member rests as Waiting again when any
//! turn ends (its parked card, or the team's open cards, say so).

use crew_core::config::Limits;
use crew_core::util::now_ms;
use crew_core::{AgentStatus, RuntimeKind};

use super::Daemon;
use crate::persistence::NewEvent;

/// What the member is asked: a reply that adds as little as possible to its
/// context, and no action.
pub(crate) const TEXT: &str = "[crew] Cache keep-alive: nothing has changed and nothing is asked \
of you. Reply with just \"ok\", call no tools, and end your turn.";

/// No keep-alive while any Claude Code usage window is this full: the last
/// of a window is for work.
const QUOTA_CEILING: f32 = 80.0;

impl Daemon {
    pub(crate) async fn keepalive_housekeeping(&self) {
        let Some(limits) = self.model().session.team.as_ref().map(|t| t.limits.clone()) else {
            return;
        };
        self.keepalive_at(now_ms(), &limits).await;
    }

    /// Send the keep-alives due at `now`.
    pub(crate) async fn keepalive_at(&self, now: i64, limits: &Limits) {
        if limits.keep_cache_warm_secs == 0 || limits.keep_cache_warm_max == 0 {
            return;
        }
        for (id, rested) in self.keepalives_due(now, limits) {
            let sent = {
                let mut model = self.model();
                let rt = model.rt(&id);
                rt.keepalives += 1;
                rt.keepalive_sent_at = Some(now);
                if rt.keepalives == 1 {
                    rt.keepalive_rest_from = now;
                }
                rt.keepalives
            };
            if !self.inject(&id, TEXT).await {
                let mut model = self.model();
                let rt = model.rt(&id);
                rt.keepalives -= 1;
                rt.keepalive_sent_at = None;
                continue;
            }
            self.emit(
                NewEvent::new(
                    "scheduler",
                    "agent.keepalive",
                    format!(
                        "kept {id}'s prompt cache warm ({sent} of {}) after {} min at rest",
                        limits.keep_cache_warm_max,
                        (now - rested).max(0) / 60_000
                    ),
                )
                .agent(&id),
            );
        }
    }

    /// The members whose keep-alive is due at `now`, with when their cache
    /// was last read.
    fn keepalives_due(&self, now: i64, limits: &Limits) -> Vec<(String, i64)> {
        let mut model = self.model();
        // A finished team gives no more work.
        if model.session.summary.is_some() {
            return Vec::new();
        }
        let quota_full = model
            .quotas
            .iter()
            .any(|q| q.runtime == RuntimeKind::Claude && q.percent_used >= QUOTA_CEILING);
        if quota_full {
            return Vec::new();
        }
        let agents = model.agents.clone();
        let mut due = Vec::new();
        for agent in &agents {
            // Claude Code only: Codex's retention is unknown and its replay
            // loses at every cap; pi is unmeasured.
            if agent.runtime != RuntimeKind::Claude
                || agent.status.is_terminal()
                || agent.paused
                || agent.limit.is_some()
                || agent.compact_pending
                || agent.temporary
            {
                continue;
            }
            // Waiting on open work only: a member with a parked card, or the
            // orchestrator while the team has cards open. A member with a
            // card to do is reminded instead; one with no open card (Done,
            // Idle) may never be given another.
            let open_work = if agent.orchestrator {
                model.tasks.iter().any(|t| !t.status.is_terminal())
            } else {
                crate::tasks::active_task(&model.tasks, &agent.id).is_some()
                    && crate::tasks::executing_task(&model.tasks, &agent.id).is_none()
            };
            if agent.status != AgentStatus::Waiting || !open_work {
                continue;
            }
            let Some(context) = agent.context.clone() else {
                continue;
            };
            if context.tokens < limits.keep_cache_warm_min_tokens {
                continue;
            }
            let rt = model.rt(&agent.id);
            // A message put into its session since the rest's first
            // keep-alive was work: the rest starts again.
            if rt.keepalives > 0 && rt.delivered_at > rt.keepalive_rest_from {
                rt.keepalives = 0;
            }
            if !rt.ready
                || rt.turn_active
                || rt.pending_prompt.is_some()
                || !rt.in_flight.is_empty()
                || rt.keepalives >= limits.keep_cache_warm_max
            {
                continue;
            }
            // Its cache was last read at its last request: the end of its
            // last turn, or the context reading that came with it. A
            // keep-alive the session has not run yet counts from when it
            // was sent, so none piles up behind it.
            let last = rt
                .turn_ended_at
                .max(context.updated_at)
                .max(rt.keepalive_sent_at.unwrap_or(0));
            if last == 0 || now - last < (limits.keep_cache_warm_secs as i64) * 1000 {
                continue;
            }
            due.push((agent.id.clone(), last));
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::{Agent, Task, TaskStatus};
    use serde_json::json;

    const HOUR: i64 = 3_600_000;

    fn limits() -> Limits {
        Limits {
            keep_cache_warm_secs: 3300,
            ..Limits::default()
        }
    }

    async fn fixture(
        status: &str,
        task: Option<TaskStatus>,
    ) -> (
        tempfile::TempDir,
        crate::daemon::Running,
        tokio::sync::mpsc::Receiver<crew_core::rpc::ServerFrame>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let running = crate::daemon::start(crate::daemon::DaemonOptions {
            workspace: temp.path().into(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        let agent: Agent = serde_json::from_value(json!({
            "id":"owner", "name":"owner", "role":"worker", "status":status,
            "runtime":"claude", "workdir":temp.path(), "backend":"pty", "last_activity_at":0,
            "context": { "tokens": 300_000, "window": 1_000_000, "percent": 30, "updated_at": HOUR },
        }))
        .unwrap();
        daemon.model().agents.push(agent);
        if let Some(status) = task {
            let task: Task = serde_json::from_value(json!({
                "id":"card", "title":"Card", "assigned_agent":"owner", "status":status,
                "created_at":0, "started_at":1
            }))
            .unwrap();
            daemon.model().tasks.push(task);
        }
        {
            let mut model = daemon.model();
            let rt = model.rt("owner");
            rt.ready = true;
            rt.turn_ended_at = HOUR;
        }
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        daemon
            .listeners
            .lock()
            .unwrap()
            .insert("owner".into(), crate::daemon::Listener { conn: 1, tx });
        (temp, running, rx)
    }

    fn sent(receiver: &mut tokio::sync::mpsc::Receiver<crew_core::rpc::ServerFrame>) -> bool {
        match receiver.try_recv() {
            Ok(frame) => {
                let text = serde_json::to_string(&frame).unwrap();
                assert!(text.contains("Cache keep-alive"), "{text}");
                true
            }
            Err(_) => false,
        }
    }

    /// The turn a keep-alive starts, as the session would run it.
    fn answer(daemon: &Daemon, at: i64) {
        daemon.model().rt("owner").turn_ended_at = at;
    }

    #[tokio::test(start_paused = true)]
    async fn off_unless_the_team_asks_for_it() {
        let (_temp, running, mut receiver) = fixture("waiting", Some(TaskStatus::Waiting)).await;
        assert_eq!(Limits::default().keep_cache_warm_secs, 0);
        running
            .daemon
            .keepalive_at(10 * HOUR, &Limits::default())
            .await;
        assert!(!sent(&mut receiver));
    }

    #[tokio::test(start_paused = true)]
    async fn a_resting_member_is_kept_warm_on_time_up_to_the_cap() {
        let (_temp, running, mut receiver) = fixture("waiting", Some(TaskStatus::Waiting)).await;
        let daemon = &running.daemon;
        let limits = limits();
        let every = 3_300_000;
        let mut at = HOUR;
        for n in 1..=limits.keep_cache_warm_max {
            daemon.keepalive_at(at + every - 1, &limits).await;
            assert!(!sent(&mut receiver), "keep-alive {n} early");
            daemon.keepalive_at(at + every, &limits).await;
            assert!(sent(&mut receiver), "keep-alive {n} due");
            // One at a time: the next counts from this one.
            daemon.keepalive_at(at + 2 * every - 1, &limits).await;
            assert!(!sent(&mut receiver), "keep-alive {n} repeated");
            at += every + 5_000;
            answer(daemon, at);
            assert_eq!(daemon.model().rt("owner").keepalives, n);
        }
        // The cap reached, the cache is let go.
        daemon.keepalive_at(at + 10 * every, &limits).await;
        assert!(!sent(&mut receiver));
        let events = daemon.store.recent_events(Some("owner"), 1000).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == "agent.keepalive")
                .count(),
            limits.keep_cache_warm_max as usize
        );
    }

    #[tokio::test(start_paused = true)]
    async fn work_starts_the_count_again() {
        let (_temp, running, mut receiver) = fixture("waiting", Some(TaskStatus::Waiting)).await;
        let daemon = &running.daemon;
        let limits = Limits {
            keep_cache_warm_max: 1,
            ..limits()
        };
        daemon.keepalive_at(2 * HOUR, &limits).await;
        assert!(sent(&mut receiver));
        answer(daemon, 2 * HOUR + 5_000);
        daemon.keepalive_at(4 * HOUR, &limits).await;
        assert!(!sent(&mut receiver));
        // A message put into its session is work.
        daemon.model().rt("owner").delivered_at = 5 * HOUR - 60_000;
        daemon.model().rt("owner").turn_ended_at = 5 * HOUR;
        daemon.keepalive_at(6 * HOUR, &limits).await;
        assert!(sent(&mut receiver));
        assert_eq!(daemon.model().rt("owner").keepalives, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_keep_alive_turn_leaves_the_member_waiting() {
        use crate::agents::Signal;
        let (_temp, running, mut receiver) = fixture("blocked", Some(TaskStatus::Waiting)).await;
        let daemon = &running.daemon;
        // A blocked member is not woken.
        daemon.keepalive_at(2 * HOUR, &limits()).await;
        assert!(!sent(&mut receiver));
        daemon.update_agent("owner", |a| a.status = AgentStatus::Waiting);
        daemon.model().rt("owner").self_status = Some(AgentStatus::Waiting);
        daemon.keepalive_at(2 * HOUR, &limits()).await;
        assert!(sent(&mut receiver));
        // The session runs it as a turn: working while it lasts, then at
        // rest as before, with its card still parked.
        daemon.on_signal("owner", Signal::TurnStarted).await;
        assert_eq!(daemon.agent("owner").unwrap().status, AgentStatus::Working);
        daemon.on_signal("owner", Signal::TurnEnded).await;
        assert_eq!(daemon.agent("owner").unwrap().status, AgentStatus::Waiting);
        assert_eq!(
            daemon.model().task("card").unwrap().status,
            TaskStatus::Waiting
        );
        assert_eq!(daemon.model().rt("owner").keepalives, 1);
        // It counted as no reminder.
        assert_eq!(daemon.model().rt("owner").nudges, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_orchestrator_is_kept_warm_while_the_team_has_open_cards() {
        let (_temp, running, mut receiver) = fixture("waiting", Some(TaskStatus::Done)).await;
        let daemon = &running.daemon;
        daemon.update_agent("owner", |a| a.orchestrator = true);
        daemon.keepalive_at(2 * HOUR, &limits()).await;
        assert!(!sent(&mut receiver));
        let task: Task = serde_json::from_value(json!({
            "id":"other", "title":"Other", "assigned_agent":"worker", "status":"working",
            "created_at":0, "started_at":1
        }))
        .unwrap();
        daemon.model().tasks.push(task);
        daemon.keepalive_at(2 * HOUR, &limits()).await;
        assert!(sent(&mut receiver));
    }

    #[tokio::test(start_paused = true)]
    async fn members_with_work_or_without_a_claude_session_are_left_alone() {
        // Without open work there is nothing to keep warm for: a member
        // between cards (Done or Idle), one that says it waits on no card,
        // and anyone once the team is done.
        for (status, task) in [
            ("done", Some(TaskStatus::Done)),
            ("idle", None),
            ("waiting", None),
            ("waiting", Some(TaskStatus::Done)),
        ] {
            let (_temp, running, mut receiver) = fixture(status, task).await;
            running.daemon.keepalive_at(2 * HOUR, &limits()).await;
            assert!(!sent(&mut receiver), "{status} {task:?}");
        }
        let (_temp, running, mut receiver) = fixture("waiting", Some(TaskStatus::Waiting)).await;
        running.daemon.model().session.summary = Some("all done".into());
        running.daemon.keepalive_at(2 * HOUR, &limits()).await;
        assert!(!sent(&mut receiver));

        // An executing card: the member is reminded, not kept warm.
        let (_temp, running, mut receiver) = fixture("idle", Some(TaskStatus::Working)).await;
        running.daemon.keepalive_at(2 * HOUR, &limits()).await;
        assert!(!sent(&mut receiver));

        for change in [
            "codex", "paused", "limited", "compact", "small", "turn", "quota",
        ] {
            let (_temp, running, mut receiver) =
                fixture("waiting", Some(TaskStatus::Waiting)).await;
            let daemon = &running.daemon;
            let agent = |f: &dyn Fn(&mut Agent)| {
                daemon.update_agent("owner", f);
            };
            match change {
                "codex" => agent(&|a| a.runtime = RuntimeKind::Codex),
                "paused" => agent(&|a| a.paused = true),
                "limited" => agent(&|a| {
                    a.limit = serde_json::from_value(
                        json!({"kind": "usage", "message": "limit", "since": 0}),
                    )
                    .ok()
                }),
                "compact" => agent(&|a| a.compact_pending = true),
                "small" => agent(&|a| a.context.as_mut().unwrap().tokens = 10_000),
                "turn" => daemon.model().rt("owner").turn_active = true,
                "quota" => daemon.model().quotas.push(
                    serde_json::from_value(json!({
                        "runtime": "claude", "window": "5h", "percent_used": 85.0,
                        "updated_at": HOUR,
                    }))
                    .unwrap(),
                ),
                _ => unreachable!(),
            }
            daemon.keepalive_at(2 * HOUR, &limits()).await;
            assert!(!sent(&mut receiver), "{change}");
        }
    }
}
