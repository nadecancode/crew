//! Message animations: a pulse travels along the link from sender to
//! receiver, and the link stays highlighted for a while afterwards.

use crew_core::types::{Message, OPERATOR_ID, ORCHESTRATOR_ID};

/// How long the pulse takes to travel the link.
pub const PULSE_MS: i64 = 900;
/// How long the link stays highlighted in total.
pub const LINK_MS: i64 = 3000;
const MAX_LINKS: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// Worker to worker, bypassing the orchestrator.
    P2p,
    /// Traffic to or from the orchestrator (or the human operator).
    Orchestrator,
    Broadcast,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub from: String,
    pub to: String,
    pub kind: LinkKind,
    pub started: i64,
}

impl Link {
    /// Position of the pulse along the link (0..1, eased) while it travels.
    pub fn pulse(&self, now: i64) -> Option<f64> {
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
        now - self.started < LINK_MS
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

#[derive(Clone, Debug, Default)]
pub struct Anims {
    pub links: Vec<Link>,
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
        self.links.retain(|l| !(l.from == from && l.to == to));
        self.links.push(Link {
            from,
            to,
            kind,
            started: now,
        });
        if self.links.len() > MAX_LINKS {
            let excess = self.links.len() - MAX_LINKS;
            self.links.drain(..excess);
        }
    }

    /// Drop expired links. Returns true when something was removed.
    pub fn gc(&mut self, now: i64) -> bool {
        let before = self.links.len();
        self.links.retain(|l| l.is_live(now));
        self.links.len() != before
    }

    /// A pulse is in flight: the dashboard should redraw at frame rate.
    pub fn pulse_live(&self, now: i64) -> bool {
        self.links.iter().any(|l| l.pulse(now).is_some())
    }

    /// Workers are talking directly right now.
    pub fn p2p_live(&self, now: i64) -> bool {
        self.links
            .iter()
            .any(|l| l.kind == LinkKind::P2p && l.is_live(now))
    }

    /// Milliseconds until the next link expires, if any is live.
    pub fn next_expiry(&self, now: i64) -> Option<i64> {
        self.links
            .iter()
            .map(|l| (l.started + LINK_MS - now).max(0))
            .min()
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
        assert!(a.pulse_live(1000 + PULSE_MS - 1));
        assert!(!a.pulse_live(1000 + PULSE_MS));
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
}
