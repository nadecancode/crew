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
//! A status the member set for itself before it is kept (signals.rs).

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

/// A keep-alive not followed by a turn's end within this long was not
/// taken up by the session; the next turn to end is not its.
const ANSWER_WITHIN_MS: i64 = 10 * 60_000;

/// A keep-alive sent and not yet answered.
#[derive(Debug, Clone)]
pub(crate) struct Sent {
    pub at: i64,
    /// The status the member had set for itself when it was sent.
    pub self_status: Option<AgentStatus>,
}

impl Sent {
    /// Whether the turn ending at `now` is the keep-alive's.
    pub(crate) fn answered_by_turn_ending_at(&self, now: i64) -> bool {
        now - self.at <= ANSWER_WITHIN_MS
    }
}

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
                rt.keepalive = Some(Sent {
                    at: now,
                    self_status: rt.self_status,
                });
                rt.keepalives
            };
            if !self.inject(&id, TEXT).await {
                let mut model = self.model();
                let rt = model.rt(&id);
                rt.keepalives -= 1;
                rt.keepalive = None;
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
            // At rest only: waiting on someone, or between cards (Done is a
            // member whose cards are all finished, until it is given the
            // next). A member with a card to do is reminded instead.
            let resting = match agent.status {
                AgentStatus::Waiting | AgentStatus::Done => true,
                AgentStatus::Idle => {
                    crate::tasks::executing_task(&model.tasks, &agent.id).is_none()
                }
                _ => false,
            };
            if !resting {
                continue;
            }
            let Some(context) = agent.context.clone() else {
                continue;
            };
            if context.tokens < limits.keep_cache_warm_min_tokens {
                continue;
            }
            let rt = model.rt(&agent.id);
            if !rt.ready
                || rt.turn_active
                || rt.pending_prompt.is_some()
                || !rt.in_flight.is_empty()
                || rt.keepalive.is_some()
                || rt.keepalives >= limits.keep_cache_warm_max
            {
                continue;
            }
            // Its cache was last read at its last request: the end of its
            // last turn, or the context reading that came with it.
            let last = rt.turn_ended_at.max(context.updated_at);
            if last == 0 || now - last < (limits.keep_cache_warm_secs as i64) * 1000 {
                continue;
            }
            due.push((agent.id.clone(), last));
        }
        due
    }

    /// A turn of `id` ended. If it was a keep-alive's, the status the member
    /// set for itself before it is put back. A turn that was given a
    /// message, or in which the member set a status, is work: the count of
    /// keep-alives starts again, as after any other turn.
    pub(crate) fn keepalive_turn_ended(&self, id: &str, now: i64) {
        let mut model = self.model();
        let rt = model.rt(id);
        match rt.keepalive.take() {
            Some(sent)
                if sent.answered_by_turn_ending_at(now)
                    && rt.turn_messages.is_empty()
                    && rt.self_status.is_none() =>
            {
                rt.self_status = sent.self_status;
            }
            _ => rt.keepalives = 0,
        }
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
        daemon.keepalive_turn_ended("owner", at);
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
            // Not again before the session has answered.
            daemon.keepalive_at(at + 2 * every, &limits).await;
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
        // A turn that was given a message is work.
        daemon.model().rt("owner").turn_messages = vec!["m-1".into()];
        daemon.keepalive_turn_ended("owner", 5 * HOUR);
        daemon.model().rt("owner").turn_messages.clear();
        daemon.model().rt("owner").turn_ended_at = 5 * HOUR;
        assert_eq!(daemon.model().rt("owner").keepalives, 0);
        daemon.keepalive_at(6 * HOUR, &limits).await;
        assert!(sent(&mut receiver));
    }

    #[tokio::test(start_paused = true)]
    async fn the_status_a_member_set_survives_its_keep_alive() {
        let (_temp, running, mut receiver) = fixture("blocked", None).await;
        let daemon = &running.daemon;
        // A blocked member is not woken.
        daemon.keepalive_at(2 * HOUR, &limits()).await;
        assert!(!sent(&mut receiver));
        daemon.update_agent("owner", |a| a.status = AgentStatus::Waiting);
        daemon.model().rt("owner").self_status = Some(AgentStatus::Waiting);
        daemon.keepalive_at(2 * HOUR, &limits()).await;
        assert!(sent(&mut receiver));
        // The keep-alive's turn started (which clears it) and ended.
        daemon.model().rt("owner").self_status = None;
        daemon.keepalive_turn_ended("owner", 2 * HOUR + 5_000);
        assert_eq!(
            daemon.model().rt("owner").self_status,
            Some(AgentStatus::Waiting)
        );
        assert_eq!(daemon.model().rt("owner").keepalives, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn members_with_work_or_without_a_claude_session_are_left_alone() {
        // A member between cards is kept warm; once the team is done, no one.
        let (_temp, running, mut receiver) = fixture("done", Some(TaskStatus::Done)).await;
        running.daemon.keepalive_at(2 * HOUR, &limits()).await;
        assert!(sent(&mut receiver));
        let (_temp, running, mut receiver) = fixture("done", Some(TaskStatus::Done)).await;
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
