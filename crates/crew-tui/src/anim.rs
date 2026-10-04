//! Animations: a pulse travels along the link from sender to receiver, and the
//! link stays highlighted for a while afterwards. Version control actions
//! (`vcs.*` events) add marks on the agent boxes: a sweep around the border for
//! a finished write, a spinner for a running operation, a glance for a read.

use crew_core::types::{Message, OPERATOR_ID, ORCHESTRATOR_ID};
use crew_core::vcs::{VcsAction, VcsEvent, VcsOutcome};

/// How long the pulse takes to travel the link.
pub const PULSE_MS: i64 = 900;
/// How long the link stays highlighted in total.
pub const LINK_MS: i64 = 3000;
/// How long the bright segment takes around a box after a write.
pub const MARK_SWEEP_MS: i64 = 700;
/// How long a write's label stays on the box.
pub const MARK_MS: i64 = 2400;
/// How long the segment of a read takes along the bottom border.
pub const GLANCE_SWEEP_MS: i64 = 300;
/// How long a read's label stays.
pub const GLANCE_MS: i64 = 900;
/// A running operation whose end never came is dropped after this long.
pub const RUNNING_MAX_MS: i64 = 600_000;
const MAX_LINKS: usize = 48;
const MAX_MARKS: usize = 48;

/// How a VCS link or mark is coloured: `theme.ok`, `theme.err`, `theme.warn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Conflict,
    Error,
}

impl Tone {
    fn of(outcome: VcsOutcome) -> Self {
        match outcome {
            VcsOutcome::Conflict => Tone::Conflict,
            VcsOutcome::Error => Tone::Error,
            VcsOutcome::Started | VcsOutcome::Ok => Tone::Ok,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// Worker to worker, bypassing the orchestrator.
    P2p,
    /// Traffic to or from the orchestrator (or the human operator).
    Orchestrator,
    Broadcast,
    /// A version control operation between two members (or one and the hub).
    Vcs(Tone),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub from: String,
    pub to: String,
    pub kind: LinkKind,
    pub started: i64,
    /// The running operation (`VcsEvent.op`) that keeps this link lit.
    pub held: Option<String>,
}

impl Link {
    /// Position of the pulse along the link (0..1, eased) while it travels.
    /// A held link has none.
    pub fn pulse(&self, now: i64) -> Option<f64> {
        if self.held.is_some() {
            return None;
        }
        let dt = now - self.started;
        if !(0..PULSE_MS).contains(&dt) {
            return None;
        }
        let t = dt as f64 / PULSE_MS as f64;
        // Ease in-out so the pulse visibly leaves and arrives.
        Some(if t < 0.5 {
            2.0 * t * t
        } else {
            1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
        })
    }

    pub fn is_live(&self, now: i64) -> bool {
        self.held.is_some() || now - self.started < LINK_MS
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkKind {
    /// A long operation under way; ends with the terminal event of `op`.
    Running { op: String },
    /// A write ended.
    Done,
    /// A read ran.
    Glance,
}

/// What a VCS action leaves on one box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The box it is drawn on (`orchestrator` for the hub).
    pub agent: String,
    pub kind: MarkKind,
    pub tone: Tone,
    /// Without the spinner a running mark adds.
    pub label: String,
    /// When it is drawn from; later than now while the pulse travels.
    pub started: i64,
}

impl Mark {
    fn duration(&self) -> Option<i64> {
        match self.kind {
            MarkKind::Running { .. } => None,
            MarkKind::Done => Some(MARK_MS),
            MarkKind::Glance => Some(GLANCE_MS),
        }
    }

    fn sweep_ms(&self) -> i64 {
        match self.kind {
            MarkKind::Done => MARK_SWEEP_MS,
            MarkKind::Glance => GLANCE_SWEEP_MS,
            MarkKind::Running { .. } => 0,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.kind, MarkKind::Running { .. })
    }

    /// Position of the sweep (done: around the border; glance: along the
    /// bottom) while it runs, 0..1.
    pub fn sweep(&self, now: i64) -> Option<f64> {
        let dt = now - self.started;
        (dt >= 0 && dt < self.sweep_ms()).then(|| dt as f64 / self.sweep_ms() as f64)
    }

    /// Drawn now: started and not yet over (a running mark: until its end).
    pub fn is_live(&self, now: i64) -> bool {
        now >= self.started && self.duration().is_none_or(|d| now - self.started < d)
    }

    /// Not over yet, though it may not have started.
    fn pending_or_live(&self, now: i64) -> bool {
        self.duration().is_none_or(|d| now - self.started < d)
    }

    fn expired(&self, now: i64) -> bool {
        match self.duration() {
            Some(d) => now - self.started >= d,
            None => now - self.started >= RUNNING_MAX_MS,
        }
    }
}

/// The operator has no box of their own; their traffic enters at the hub.
fn endpoint(id: &str) -> &str {
    if id == OPERATOR_ID {
        ORCHESTRATOR_ID
    } else {
        id
    }
}

/// Where a VCS actor is drawn: the integration has no box, it is the hub.
fn vcs_endpoint(id: &str) -> &str {
    if id == "integration" {
        ORCHESTRATOR_ID
    } else {
        endpoint(id)
    }
}

fn action_name(action: VcsAction) -> &'static str {
    match action {
        VcsAction::Workspace => "workspace",
        VcsAction::Commit => "commit",
        VcsAction::CherryPick => "cherry-pick",
        VcsAction::Merge => "merge",
        VcsAction::Integrate => "integrate",
        VcsAction::Status => "status",
        VcsAction::Diff => "diff",
    }
}

fn short(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

#[derive(Clone, Debug, Default)]
pub struct Anims {
    pub links: Vec<Link>,
    pub marks: Vec<Mark>,
}

impl Anims {
    /// Start the animation for a message that just arrived.
    /// `workers` lists the worker ids a broadcast fans out to.
    pub fn on_message(&mut self, msg: &Message, workers: &[String], now: i64) {
        let from = endpoint(&msg.from).to_string();
        if msg.is_broadcast() {
            let kind = if from == ORCHESTRATOR_ID {
                LinkKind::Orchestrator
            } else {
                LinkKind::Broadcast
            };
            for worker in workers {
                if *worker != from {
                    self.start(from.clone(), worker.clone(), kind, now);
                }
            }
            return;
        }
        let to = endpoint(&msg.to).to_string();
        if from == to {
            return;
        }
        let kind = if msg.is_p2p() {
            LinkKind::P2p
        } else {
            LinkKind::Orchestrator
        };
        self.start(from, to, kind, now);
    }

    fn start(&mut self, from: String, to: String, kind: LinkKind, now: i64) {
        self.push_link(Link {
            from,
            to,
            kind,
            started: now,
            held: None,
        });
    }

    fn push_link(&mut self, link: Link) {
        self.links
            .retain(|l| !(l.from == link.from && l.to == link.to));
        self.links.push(link);
        if self.links.len() > MAX_LINKS {
            let excess = self.links.len() - MAX_LINKS;
            self.links.drain(..excess);
        }
    }

    fn push_mark(&mut self, mark: Mark, now: i64) {
        match mark.kind {
            MarkKind::Running { .. } => {}
            // A write's result replaces whatever the box showed.
            MarkKind::Done => self
                .marks
                .retain(|m| m.agent != mark.agent || m.is_running()),
            MarkKind::Glance => {
                let blocked = self.marks.iter().any(|m| {
                    m.agent == mark.agent && m.kind == MarkKind::Done && m.pending_or_live(now)
                });
                if blocked {
                    return;
                }
                self.marks
                    .retain(|m| m.agent != mark.agent || m.kind != MarkKind::Glance);
            }
        }
        self.marks.push(mark);
        if self.marks.len() > MAX_MARKS {
            let excess = self.marks.len() - MAX_MARKS;
            self.marks.drain(..excess);
        }
    }

    /// Start, or end, what §anim says for a VCS event.
    pub fn on_vcs(&mut self, event: &VcsEvent, now: i64) {
        if event.outcome == VcsOutcome::Started {
            self.vcs_started(event, now);
            return;
        }
        // The operation is over: its running mark and held link go. The
        // terminal row starts its own link, along the line already lit.
        self.marks
            .retain(|m| !matches!(&m.kind, MarkKind::Running { op } if *op == event.op));
        self.links.retain(|l| l.held.as_deref() != Some(&event.op));
        self.vcs_ended(event, now);
    }

    fn vcs_started(&mut self, event: &VcsEvent, now: i64) {
        let agent = vcs_endpoint(&event.agent);
        let peer = event
            .peer
            .as_deref()
            .map(vcs_endpoint)
            .filter(|p| *p != agent);
        let (on, label) = match event.action {
            VcsAction::Workspace => (
                peer.unwrap_or(ORCHESTRATOR_ID),
                format!("workspace {}", event.agent),
            ),
            VcsAction::Merge => (
                agent,
                match &event.peer {
                    Some(peer) => format!("merge {peer}"),
                    None => "merge".to_string(),
                },
            ),
            VcsAction::Integrate => (ORCHESTRATOR_ID, "integrating".to_string()),
            other => (agent, action_name(other).to_string()),
        };
        self.push_mark(
            Mark {
                agent: on.to_string(),
                kind: MarkKind::Running {
                    op: event.op.clone(),
                },
                tone: Tone::Ok,
                label,
                started: now,
            },
            now,
        );
        if let (VcsAction::Merge, Some(peer)) = (event.action, peer) {
            self.push_link(Link {
                from: peer.to_string(),
                to: agent.to_string(),
                kind: LinkKind::Vcs(Tone::Ok),
                started: now,
                held: Some(event.op.clone()),
            });
        }
    }

    fn vcs_ended(&mut self, event: &VcsEvent, now: i64) {
        let tone = Tone::of(event.outcome);
        let agent = vcs_endpoint(&event.agent);
        let peer = event
            .peer
            .as_deref()
            .map(vcs_endpoint)
            .filter(|p| *p != agent);
        let action = event.action;
        // The box the result shows on, the link before it, and the label.
        let (on, link_from, label): (&str, Option<&str>, Option<String>) =
            match (action, event.outcome) {
                (VcsAction::Status | VcsAction::Diff, outcome) => {
                    let name = action_name(action);
                    let label = if outcome == VcsOutcome::Error {
                        format!("! {name}")
                    } else {
                        format!("· {name}")
                    };
                    (agent, None, Some(label))
                }
                (VcsAction::Workspace, VcsOutcome::Error) => (
                    peer.unwrap_or(ORCHESTRATOR_ID),
                    None,
                    Some(format!("! workspace {}", event.agent)),
                ),
                (VcsAction::Integrate, VcsOutcome::Error) => {
                    (ORCHESTRATOR_ID, None, Some("! integrate".to_string()))
                }
                (_, VcsOutcome::Error) => (agent, None, Some(format!("! {}", action_name(action)))),
                (VcsAction::Workspace, _) => {
                    let branch = event
                        .branch
                        .clone()
                        .unwrap_or_else(|| format!("agents/{}", event.agent));
                    (agent, peer, Some(format!("+ {branch}")))
                }
                (VcsAction::Commit, _) => (
                    agent,
                    None,
                    Some(match &event.commit {
                        Some(commit) => format!("◆ {}", short(commit)),
                        None => "◆ commit".to_string(),
                    }),
                ),
                (VcsAction::CherryPick | VcsAction::Merge, VcsOutcome::Conflict) => {
                    let n = event.files.len();
                    let label = match n {
                        0 => "✗ conflict".to_string(),
                        1 => "✗ 1 file".to_string(),
                        n => format!("✗ {n} files"),
                    };
                    (agent, peer, Some(label))
                }
                (VcsAction::CherryPick | VcsAction::Merge, _) => (
                    agent,
                    peer,
                    Some(match &event.commit {
                        Some(commit) => format!("◆ {}", short(commit)),
                        None => "◆ done".to_string(),
                    }),
                ),
                (VcsAction::Integrate, _) => match peer {
                    // One branch of the run: the work travels to the hub.
                    Some(peer) => (ORCHESTRATOR_ID, Some(peer), None),
                    // The run's tally.
                    None => (
                        ORCHESTRATOR_ID,
                        None,
                        Some(
                            event
                                .summary
                                .clone()
                                .unwrap_or_else(|| "integrated".to_string()),
                        ),
                    ),
                },
            };
        let linked = link_from.is_some();
        if let Some(from) = link_from {
            self.push_link(Link {
                from: from.to_string(),
                to: on.to_string(),
                kind: LinkKind::Vcs(tone),
                started: now,
                held: None,
            });
        }
        let Some(label) = label else { return };
        let glance = action.is_read();
        self.push_mark(
            Mark {
                agent: on.to_string(),
                kind: if glance {
                    MarkKind::Glance
                } else {
                    MarkKind::Done
                },
                tone,
                label,
                // On arrival, when a pulse comes first.
                started: if linked { now + PULSE_MS } else { now },
            },
            now,
        );
    }

    /// From a snapshot's events, oldest first: restore the operations that
    /// started and have not ended, as running marks and held links. Every VCS
    /// link and mark that was there goes first; nothing else starts, since
    /// events in a snapshot are history. `age` is how long ago each happened.
    pub fn resume<'a>(&mut self, events: impl Iterator<Item = (&'a VcsEvent, i64)>, now: i64) {
        self.links
            .retain(|l| !matches!(l.kind, LinkKind::Vcs(_)) && l.held.is_none());
        self.marks.clear();
        let mut open: Vec<(&VcsEvent, i64)> = Vec::new();
        for (event, age) in events {
            if event.outcome.is_terminal() {
                open.retain(|(e, _)| e.op != event.op);
            } else {
                open.push((event, age));
            }
        }
        for (event, age) in open {
            if age < RUNNING_MAX_MS {
                self.vcs_started(event, now - age.max(0));
            }
        }
    }

    /// Drop what expired. Returns true when something was removed.
    pub fn gc(&mut self, now: i64) -> bool {
        let (links, marks) = (self.links.len(), self.marks.len());
        self.links.retain(|l| {
            if l.held.is_some() {
                now - l.started < RUNNING_MAX_MS
            } else {
                l.is_live(now)
            }
        });
        self.marks.retain(|m| !m.expired(now));
        self.links.len() != links || self.marks.len() != marks
    }

    /// Something is moving: a pulse, a sweep or a glance. The dashboard should
    /// redraw at frame rate.
    pub fn animating(&self, now: i64) -> bool {
        self.links.iter().any(|l| l.pulse(now).is_some())
            || self.marks.iter().any(|m| m.sweep(now).is_some())
    }

    /// The mark a box shows: a write or read while it lives, else the newest
    /// operation still running there.
    pub fn mark_for(&self, id: &str, now: i64) -> Option<&Mark> {
        let mine = || self.marks.iter().filter(|m| m.agent == id);
        mine()
            .rfind(|m| !m.is_running() && m.is_live(now))
            .or_else(|| mine().rfind(|m| m.is_running()))
    }

    /// An operation is running: the spinner turns.
    pub fn running(&self) -> bool {
        self.marks.iter().any(Mark::is_running)
    }

    /// Workers are talking directly right now.
    pub fn p2p_live(&self, now: i64) -> bool {
        self.links
            .iter()
            .any(|l| l.kind == LinkKind::P2p && l.is_live(now))
    }

    /// Milliseconds until the next link or mark changes without an event: a
    /// link expires, a mark starts or ends, a running one reaches its cap.
    /// Held links have no expiry but the cap.
    pub fn next_expiry(&self, now: i64) -> Option<i64> {
        let links = self.links.iter().map(|l| match l.held {
            Some(_) => l.started + RUNNING_MAX_MS - now,
            None => l.started + LINK_MS - now,
        });
        let marks = self.marks.iter().map(|m| {
            if m.started > now {
                m.started - now
            } else {
                match m.duration() {
                    Some(d) => m.started + d - now,
                    None => m.started + RUNNING_MAX_MS - now,
                }
            }
        });
        links.chain(marks).map(|ms| ms.max(0)).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::types::MessageType;

    fn msg(from: &str, to: &str) -> Message {
        Message {
            id: format!("m-{from}-{to}"),
            from: from.into(),
            to: to.into(),
            kind: MessageType::Message,
            body: "x".into(),
            timestamp: 0,
            correlation_id: None,
            reply_to: None,
            metadata: serde_json::Value::Null,
        }
    }

    fn workers() -> Vec<String> {
        (1..=3).map(|i| format!("claude-{i}")).collect()
    }

    #[test]
    fn p2p_links_are_direct_and_expire() {
        let mut a = Anims::default();
        a.on_message(&msg("claude-3", "claude-1"), &workers(), 1000);
        assert_eq!(a.links.len(), 1);
        assert_eq!(a.links[0].kind, LinkKind::P2p);
        assert_eq!(
            (a.links[0].from.as_str(), a.links[0].to.as_str()),
            ("claude-3", "claude-1")
        );
        assert!(a.p2p_live(1000));
        assert!(a.animating(1000 + PULSE_MS - 1));
        assert!(!a.animating(1000 + PULSE_MS));
        assert!(a.p2p_live(1000 + LINK_MS - 1));
        assert!(!a.gc(1000 + LINK_MS - 1));
        assert!(a.gc(1000 + LINK_MS));
        assert!(a.links.is_empty());
        assert!(!a.p2p_live(1000 + LINK_MS));
    }

    #[test]
    fn orchestrator_and_operator_traffic_uses_the_hub() {
        let mut a = Anims::default();
        a.on_message(&msg("orchestrator", "claude-2"), &workers(), 0);
        a.on_message(&msg("operator", "claude-1"), &workers(), 0);
        a.on_message(&msg("claude-1", "orchestrator"), &workers(), 0);
        assert!(a.links.iter().all(|l| l.kind == LinkKind::Orchestrator));
        assert_eq!(a.links[1].from, "orchestrator");
        assert!(!a.p2p_live(0));
        // Operator to orchestrator has no visible link.
        a.on_message(&msg("operator", "orchestrator"), &workers(), 0);
        assert_eq!(a.links.len(), 3);
    }

    #[test]
    fn broadcasts_fan_out_to_every_other_worker() {
        let mut a = Anims::default();
        a.on_message(&msg("claude-2", "*"), &workers(), 0);
        let mut targets: Vec<&str> = a.links.iter().map(|l| l.to.as_str()).collect();
        targets.sort_unstable();
        assert_eq!(targets, ["claude-1", "claude-3"]);
        assert!(a.links.iter().all(|l| l.kind == LinkKind::Broadcast));
        assert!(!a.p2p_live(0));
    }

    #[test]
    fn repeat_messages_restart_the_same_link() {
        let mut a = Anims::default();
        a.on_message(&msg("claude-1", "claude-2"), &workers(), 0);
        a.on_message(&msg("claude-1", "claude-2"), &workers(), 500);
        assert_eq!(a.links.len(), 1);
        assert_eq!(a.links[0].started, 500);
        for i in 0..200 {
            a.on_message(&msg(&format!("a{i}"), "claude-2"), &workers(), 600);
        }
        assert!(a.links.len() <= MAX_LINKS);
    }

    #[test]
    fn pulse_is_monotonic_and_bounded() {
        let l = Link {
            from: "a".into(),
            to: "b".into(),
            kind: LinkKind::P2p,
            started: 0,
            held: None,
        };
        let mut last = -1.0;
        for t in (0..PULSE_MS).step_by(30) {
            let p = l.pulse(t).unwrap();
            assert!((0.0..=1.0).contains(&p) && p >= last);
            last = p;
        }
        assert!(l.pulse(-5).is_none());
        assert!(l.pulse(PULSE_MS).is_none());
    }

    // ---- version control -------------------------------------------------

    use crate::fixture::vcs;

    fn ev(action: VcsAction, outcome: VcsOutcome, op: &str, agent: &str) -> VcsEvent {
        vcs(action, outcome, op, agent)
    }

    fn with_peer(mut e: VcsEvent, peer: &str) -> VcsEvent {
        e.peer = Some(peer.into());
        e
    }

    fn mark_of<'a>(a: &'a Anims, id: &str) -> &'a Mark {
        a.marks
            .iter()
            .find(|m| m.agent == id)
            .unwrap_or_else(|| panic!("no mark on {id}: {:?}", a.marks))
    }

    fn vcs_link(from: &str, to: &str, tone: Tone) -> Link {
        Link {
            from: from.into(),
            to: to.into(),
            kind: LinkKind::Vcs(tone),
            started: 0,
            held: None,
        }
    }

    #[test]
    fn a_commit_marks_its_agent_with_the_short_id() {
        let mut a = Anims::default();
        let mut e = ev(VcsAction::Commit, VcsOutcome::Ok, "v1", "db");
        e.commit = Some("1a2b3c4d5e6f".into());
        a.on_vcs(&e, 1000);
        assert!(a.links.is_empty());
        let m = mark_of(&a, "db");
        assert_eq!((m.kind.clone(), m.tone), (MarkKind::Done, Tone::Ok));
        assert_eq!(m.label, "◆ 1a2b3c4d");
        assert_eq!(m.started, 1000);
        assert!(m.is_live(1000) && m.is_live(1000 + MARK_MS - 1) && !m.is_live(1000 + MARK_MS));
        assert!(m.sweep(1000).is_some() && m.sweep(1000 + MARK_SWEEP_MS).is_none());
    }

    #[test]
    fn a_pick_or_merge_pulses_from_the_peer_and_marks_on_arrival() {
        for action in [VcsAction::CherryPick, VcsAction::Merge] {
            let mut a = Anims::default();
            let mut e = with_peer(ev(action, VcsOutcome::Ok, "v1", "ledger"), "db");
            e.commit = Some("5e6f7a8b9c".into());
            a.on_vcs(&e, 0);
            assert_eq!(a.links, vec![vcs_link("db", "ledger", Tone::Ok)]);
            let m = mark_of(&a, "ledger");
            assert_eq!(m.label, "◆ 5e6f7a8b");
            // Pending until the pulse lands: not drawn, not animating.
            assert_eq!(m.started, PULSE_MS);
            assert!(!m.is_live(PULSE_MS - 1) && m.is_live(PULSE_MS));
            assert!(m.sweep(PULSE_MS - 1).is_none());
            assert!(a.animating(PULSE_MS - 1));
            assert!(a.animating(PULSE_MS));
            assert!(!a.p2p_live(0));
        }
    }

    #[test]
    fn a_conflict_is_a_distinct_tone_with_its_file_count() {
        let mut a = Anims::default();
        let mut e = with_peer(
            ev(VcsAction::CherryPick, VcsOutcome::Conflict, "v1", "ledger"),
            "rival",
        );
        e.files = vec!["schema.sql".into()];
        a.on_vcs(&e, 0);
        assert_eq!(a.links, vec![vcs_link("rival", "ledger", Tone::Conflict)]);
        let m = mark_of(&a, "ledger");
        assert_eq!((m.tone, m.label.as_str()), (Tone::Conflict, "✗ 1 file"));
        e.files.push("b.rs".into());
        a.on_vcs(&e, 5000);
        assert_eq!(mark_of(&a, "ledger").label, "✗ 2 files");
        assert_eq!(a.marks.len(), 1);
    }

    #[test]
    fn a_workspace_links_parent_to_member_and_marks_the_member() {
        let mut a = Anims::default();
        let mut e = with_peer(
            ev(VcsAction::Workspace, VcsOutcome::Ok, "v1", "ledger-s1"),
            "ledger",
        );
        e.branch = Some("agents/ledger-s1".into());
        a.on_vcs(&e, 0);
        assert_eq!(a.links, vec![vcs_link("ledger", "ledger-s1", Tone::Ok)]);
        assert_eq!(mark_of(&a, "ledger-s1").label, "+ agents/ledger-s1");
        // No parent: no link, mark at once; branch falls back to agents/<id>.
        let mut b = Anims::default();
        b.on_vcs(&ev(VcsAction::Workspace, VcsOutcome::Ok, "v2", "db"), 700);
        assert!(b.links.is_empty());
        assert_eq!(mark_of(&b, "db").label, "+ agents/db");
        assert_eq!(mark_of(&b, "db").started, 700);
    }

    #[test]
    fn integration_branches_run_to_the_hub_and_the_tally_marks_it() {
        let mut a = Anims::default();
        a.on_vcs(
            &with_peer(
                ev(VcsAction::Integrate, VcsOutcome::Ok, "v2", "integration"),
                "db",
            ),
            0,
        );
        a.on_vcs(
            &with_peer(
                ev(
                    VcsAction::Integrate,
                    VcsOutcome::Conflict,
                    "v3",
                    "integration",
                ),
                "rival",
            ),
            0,
        );
        assert_eq!(
            a.links,
            vec![
                vcs_link("db", "orchestrator", Tone::Ok),
                vcs_link("rival", "orchestrator", Tone::Conflict)
            ]
        );
        assert!(a.marks.is_empty());
        let mut tally = ev(
            VcsAction::Integrate,
            VcsOutcome::Conflict,
            "v1",
            "integration",
        );
        tally.summary = Some("1 merged, 1 conflict".into());
        a.on_vcs(&tally, 100);
        let m = mark_of(&a, "orchestrator");
        assert_eq!(
            (m.tone, m.label.as_str(), m.started),
            (Tone::Conflict, "1 merged, 1 conflict", 100)
        );
        tally.outcome = VcsOutcome::Ok;
        a.on_vcs(&tally, 5000);
        assert_eq!(mark_of(&a, "orchestrator").tone, Tone::Ok);
    }

    #[test]
    fn reads_glance_and_never_hide_a_write() {
        let mut a = Anims::default();
        a.on_vcs(&ev(VcsAction::Status, VcsOutcome::Ok, "v1", "db"), 0);
        let m = mark_of(&a, "db");
        assert_eq!(
            (m.kind.clone(), m.label.as_str()),
            (MarkKind::Glance, "· status")
        );
        assert!(m.sweep(GLANCE_SWEEP_MS - 1).is_some() && m.sweep(GLANCE_SWEEP_MS).is_none());
        assert!(m.is_live(GLANCE_MS - 1) && !m.is_live(GLANCE_MS));
        // A newer glance replaces it.
        a.on_vcs(&ev(VcsAction::Diff, VcsOutcome::Ok, "v2", "db"), 100);
        assert_eq!(a.marks.len(), 1);
        assert_eq!(a.marks[0].label, "· diff");
        // A write replaces the glance ...
        let mut c = ev(VcsAction::Commit, VcsOutcome::Ok, "v3", "db");
        c.commit = Some("abcdef0123".into());
        a.on_vcs(&c, 200);
        assert_eq!(a.marks.len(), 1);
        assert_eq!(a.marks[0].kind, MarkKind::Done);
        // ... and a read does not replace the write, live or pending.
        a.on_vcs(&ev(VcsAction::Status, VcsOutcome::Ok, "v4", "db"), 300);
        assert_eq!(a.marks.len(), 1);
        assert_eq!(a.marks[0].kind, MarkKind::Done);
        let mut p = Anims::default();
        p.on_vcs(
            &with_peer(ev(VcsAction::Merge, VcsOutcome::Ok, "v5", "db"), "ledger"),
            0,
        );
        p.on_vcs(&ev(VcsAction::Status, VcsOutcome::Ok, "v6", "db"), 10);
        assert_eq!(p.marks.len(), 1);
        // Once the write's mark is over, a read shows again.
        p.on_vcs(&ev(VcsAction::Status, VcsOutcome::Ok, "v7", "db"), 5000);
        assert_eq!(p.marks.len(), 2);
    }

    #[test]
    fn read_errors_glance_in_warn_and_other_errors_mark_done() {
        let mut a = Anims::default();
        a.on_vcs(&ev(VcsAction::Diff, VcsOutcome::Error, "v1", "db"), 0);
        let m = mark_of(&a, "db");
        assert_eq!(
            (m.kind.clone(), m.tone, m.label.as_str()),
            (MarkKind::Glance, Tone::Error, "! diff")
        );
        a.on_vcs(&ev(VcsAction::Commit, VcsOutcome::Error, "v2", "db"), 10);
        let m = mark_of(&a, "db");
        assert_eq!(
            (m.kind.clone(), m.tone, m.label.as_str()),
            (MarkKind::Done, Tone::Error, "! commit")
        );
        // A workspace error shows where the workspace ran: the parent, else the hub.
        a.on_vcs(
            &with_peer(
                ev(VcsAction::Workspace, VcsOutcome::Error, "v3", "s1"),
                "ledger",
            ),
            0,
        );
        assert_eq!(mark_of(&a, "ledger").label, "! workspace s1");
        a.on_vcs(&ev(VcsAction::Workspace, VcsOutcome::Error, "v4", "s2"), 0);
        assert_eq!(mark_of(&a, "orchestrator").label, "! workspace s2");
        a.on_vcs(
            &ev(VcsAction::Integrate, VcsOutcome::Error, "v5", "integration"),
            0,
        );
        assert_eq!(mark_of(&a, "orchestrator").label, "! integrate");
        assert!(a.links.is_empty());
    }

    #[test]
    fn a_running_operation_holds_its_link_until_the_end_then_pulses_it() {
        let mut a = Anims::default();
        let mut start = with_peer(
            ev(VcsAction::Merge, VcsOutcome::Started, "v1", "ledger"),
            "db",
        );
        start.source = Some("agents/db".into());
        a.on_vcs(&start, 0);
        assert!(a.running());
        let m = mark_of(&a, "ledger");
        assert_eq!(m.kind, MarkKind::Running { op: "v1".into() });
        assert_eq!(m.label, "merge db");
        let held = a.links[0].clone();
        assert_eq!((held.from.as_str(), held.to.as_str()), ("db", "ledger"));
        assert_eq!(held.held.as_deref(), Some("v1"));
        // Lit with no pulse and no expiry, however long it runs.
        assert!(held.pulse(0).is_none() && held.is_live(LINK_MS * 100));
        assert!(!a.animating(5000));
        assert!(!a.gc(LINK_MS * 100));
        // The end: running mark gone, the same pair pulses, mark on arrival.
        let mut end = with_peer(ev(VcsAction::Merge, VcsOutcome::Ok, "v1", "ledger"), "db");
        end.commit = Some("5e6f7a8b9c".into());
        a.on_vcs(&end, 20_000);
        assert!(!a.running());
        assert_eq!(a.links.len(), 1);
        assert_eq!(a.links[0].held, None);
        assert_eq!(a.links[0].kind, LinkKind::Vcs(Tone::Ok));
        assert!(a.links[0].pulse(20_000).is_some());
        assert_eq!(a.marks.len(), 1);
        assert_eq!(a.marks[0].kind, MarkKind::Done);
        assert_eq!(a.marks[0].started, 20_000 + PULSE_MS);
        assert!(a.gc(20_000 + LINK_MS));
    }

    #[test]
    fn started_shows_a_mark_for_each_action_that_has_one() {
        let mut a = Anims::default();
        a.on_vcs(
            &ev(VcsAction::CherryPick, VcsOutcome::Started, "v1", "ledger"),
            0,
        );
        assert_eq!(mark_of(&a, "ledger").label, "cherry-pick");
        assert!(a.links.is_empty());
        a.on_vcs(
            &ev(
                VcsAction::Integrate,
                VcsOutcome::Started,
                "v2",
                "integration",
            ),
            0,
        );
        assert_eq!(mark_of(&a, "orchestrator").label, "integrating");
        a.on_vcs(
            &with_peer(
                ev(VcsAction::Workspace, VcsOutcome::Started, "v3", "s1"),
                "ledger",
            ),
            10,
        );
        // The parent's box shows two running marks; the newest is the one drawn.
        assert_eq!(
            a.mark_for("ledger", 10).map(|m| m.label.as_str()),
            Some("workspace s1")
        );
        a.on_vcs(
            &ev(VcsAction::Workspace, VcsOutcome::Started, "v4", "s2"),
            10,
        );
        assert_eq!(
            a.mark_for("orchestrator", 10).map(|m| m.label.as_str()),
            Some("workspace s2")
        );
        // Error ends it where it ran.
        a.on_vcs(
            &ev(VcsAction::CherryPick, VcsOutcome::Error, "v1", "ledger"),
            100,
        );
        assert!(a.marks.iter().all(|m| m.label != "cherry-pick"));
        assert_eq!(a.marks.iter().filter(|m| m.is_running()).count(), 3);
    }

    #[test]
    fn a_terminal_event_alone_renders_like_one_after_a_start() {
        let mut end = with_peer(ev(VcsAction::Merge, VcsOutcome::Ok, "v1", "ledger"), "db");
        end.commit = Some("5e6f7a8b9c".into());
        let mut alone = Anims::default();
        alone.on_vcs(&end, 50);
        let mut after = Anims::default();
        after.on_vcs(
            &with_peer(
                ev(VcsAction::Merge, VcsOutcome::Started, "v1", "ledger"),
                "db",
            ),
            0,
        );
        after.on_vcs(&end, 50);
        assert_eq!(alone.links, after.links);
        assert_eq!(alone.marks, after.marks);
    }

    #[test]
    fn integration_and_operator_map_to_the_hub_and_self_links_are_skipped() {
        let mut a = Anims::default();
        a.on_vcs(
            &with_peer(
                ev(VcsAction::Merge, VcsOutcome::Ok, "v1", "ledger"),
                "ledger",
            ),
            0,
        );
        assert!(a.links.is_empty());
        assert_eq!(mark_of(&a, "ledger").started, 0);
        a.on_vcs(
            &with_peer(ev(VcsAction::Merge, VcsOutcome::Ok, "v2", "operator"), "db"),
            0,
        );
        assert_eq!(a.links, vec![vcs_link("db", "orchestrator", Tone::Ok)]);
    }

    #[test]
    fn a_vcs_link_replaces_a_live_link_between_the_same_pair() {
        let mut a = Anims::default();
        a.on_message(&msg("db", "ledger"), &workers(), 0);
        assert!(a.p2p_live(0));
        a.on_vcs(
            &with_peer(ev(VcsAction::Merge, VcsOutcome::Ok, "v1", "ledger"), "db"),
            100,
        );
        assert_eq!(a.links.len(), 1);
        assert!(!a.p2p_live(100));
    }

    #[test]
    fn a_running_mark_without_an_end_is_dropped_at_the_cap() {
        let mut a = Anims::default();
        let mut start = with_peer(
            ev(VcsAction::Merge, VcsOutcome::Started, "v1", "ledger"),
            "db",
        );
        start.source = Some("agents/db".into());
        a.on_vcs(&start, 0);
        assert_eq!(a.next_expiry(0), Some(RUNNING_MAX_MS));
        assert!(!a.gc(RUNNING_MAX_MS - 1));
        assert!(a.running());
        assert!(a.gc(RUNNING_MAX_MS));
        assert!(!a.running() && a.links.is_empty() && a.marks.is_empty());
    }

    #[test]
    fn animating_is_only_a_pulse_a_sweep_or_a_glance() {
        let mut a = Anims::default();
        let mut e = ev(VcsAction::Commit, VcsOutcome::Ok, "v1", "db");
        e.commit = Some("1a2b3c4d".into());
        a.on_vcs(&e, 0);
        assert!(a.animating(0) && a.animating(MARK_SWEEP_MS - 1));
        // Between the end of the sweep and the expiry: lit, not moving.
        assert!(!a.animating(MARK_SWEEP_MS));
        assert!(!a.animating(MARK_MS - 1));
        assert!(!a.running());
        assert_eq!(a.next_expiry(MARK_SWEEP_MS), Some(MARK_MS - MARK_SWEEP_MS));
        assert!(!a.gc(MARK_MS - 1));
        assert!(a.gc(MARK_MS));
        assert!(a.marks.is_empty());
        let mut r = Anims::default();
        r.on_vcs(&ev(VcsAction::Status, VcsOutcome::Ok, "v2", "db"), 0);
        assert!(r.animating(0) && !r.animating(GLANCE_SWEEP_MS));
        // A pending mark: one wake at its start.
        let mut p = Anims::default();
        p.on_vcs(
            &with_peer(ev(VcsAction::Merge, VcsOutcome::Ok, "v3", "ledger"), "db"),
            0,
        );
        assert!(p.animating(PULSE_MS - 1));
        assert_eq!(p.next_expiry(PULSE_MS - 100), Some(100));
        assert_eq!(p.next_expiry(0), Some(PULSE_MS));
    }

    #[test]
    fn marks_are_bounded() {
        let mut a = Anims::default();
        for i in 0..200 {
            let mut e = ev(
                VcsAction::Commit,
                VcsOutcome::Ok,
                &format!("v{i}"),
                &format!("a{i}"),
            );
            e.commit = Some("1a2b3c4d".into());
            a.on_vcs(&e, 0);
        }
        assert!(a.marks.len() <= MAX_MARKS);
    }

    #[test]
    fn resume_restores_only_what_still_runs() {
        let mut a = Anims::default();
        // Old state is replaced: a stale done mark and vcs link go.
        a.on_vcs(
            &with_peer(ev(VcsAction::Merge, VcsOutcome::Ok, "v0", "x"), "y"),
            0,
        );
        a.on_message(&msg("p", "q"), &workers(), 0);
        let started = with_peer(
            ev(VcsAction::Merge, VcsOutcome::Started, "v1", "ledger"),
            "db",
        );
        let done_start = ev(VcsAction::CherryPick, VcsOutcome::Started, "v2", "db");
        let done_end = ev(VcsAction::CherryPick, VcsOutcome::Ok, "v2", "db");
        let read = ev(VcsAction::Status, VcsOutcome::Ok, "v3", "db");
        let history = [
            (&done_start, 9000),
            (&done_end, 8000),
            (&started, 5000),
            (&read, 100),
        ];
        a.resume(history.into_iter(), 100_000);
        assert_eq!(a.marks.len(), 1);
        assert_eq!(a.marks[0].kind, MarkKind::Running { op: "v1".into() });
        assert_eq!(a.marks[0].started, 95_000);
        let held: Vec<_> = a.links.iter().filter(|l| l.held.is_some()).collect();
        assert_eq!(held.len(), 1);
        // The message link is not VCS: it stays.
        assert!(a.links.iter().any(|l| l.from == "p"));
        assert!(!a.animating(100_000));
        // One that began long ago is not shown.
        let mut b = Anims::default();
        b.resume([(&started, RUNNING_MAX_MS + 1)].into_iter(), 0);
        assert!(b.marks.is_empty() && b.links.is_empty());
    }
}
