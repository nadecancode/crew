//! The payload of version control events.
//!
//! Every version control action the daemon runs on an agent's behalf (a
//! workspace made, a commit, a cherry-pick, a merge, an integration run, a
//! status or diff) is logged as an event of source [`VCS_SOURCE`] and type
//! `vcs.<action>`, whose `data` is a [`VcsEvent`]. The payload is the same
//! under both backends (jj and git), so the dashboard animates from it
//! without knowing which one ran.
//!
//! Operations that can take seconds emit a `started` event when they begin
//! and exactly one terminal event when they end, both carrying the same
//! `op`. A terminal event carries every field of its action, so one that
//! arrives without its start renders the same.

use serde::{Deserialize, Serialize};

use crate::types::Event;

/// `Event.source` of every version control event.
pub const VCS_SOURCE: &str = "vcs";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VcsAction {
    /// A member's workspace was created.
    Workspace,
    Commit,
    CherryPick,
    Merge,
    /// The integration run, or one branch of it.
    Integrate,
    Status,
    Diff,
}

impl VcsAction {
    pub const ALL: [VcsAction; 7] = [
        VcsAction::Workspace,
        VcsAction::Commit,
        VcsAction::CherryPick,
        VcsAction::Merge,
        VcsAction::Integrate,
        VcsAction::Status,
        VcsAction::Diff,
    ];

    /// Its name on the wire: `cherry_pick`, the suffix of `vcs.cherry_pick`.
    pub fn as_str(self) -> &'static str {
        match self {
            VcsAction::Workspace => "workspace",
            VcsAction::Commit => "commit",
            VcsAction::CherryPick => "cherry_pick",
            VcsAction::Merge => "merge",
            VcsAction::Integrate => "integrate",
            VcsAction::Status => "status",
            VcsAction::Diff => "diff",
        }
    }

    /// Status and diff: they change nothing.
    pub fn is_read(self) -> bool {
        matches!(self, VcsAction::Status | VcsAction::Diff)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VcsOutcome {
    /// The operation began; its terminal event follows with the same `op`.
    Started,
    Ok,
    Conflict,
    Error,
}

impl VcsOutcome {
    /// Everything but `Started`: the operation is over.
    pub fn is_terminal(self) -> bool {
        !matches!(self, VcsOutcome::Started)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VcsEvent {
    pub action: VcsAction,
    pub outcome: VcsOutcome,
    /// The operation this event belongs to: on every event, one id per
    /// operation. A `started` event and the terminal event of the same
    /// operation carry the same id.
    pub op: String,
    /// "jj" or "git".
    pub backend: String,
    /// Who acted: an agent id, or "integration".
    pub agent: String,
    /// Whose work moved: the author of a picked commit, the agent merged, a
    /// subagent's parent. None when nobody else is involved or it is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// The commit that resulted (full commit id): the new commit, the applied
    /// copy, the merge commit; for a workspace, the commit it starts from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// jj's change id of `commit`. None under git.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    /// What was brought in or looked at: the picked commit's id (on a
    /// `started` event: the revision as the agent gave it), the merged branch,
    /// the revision a diff is against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The branch that was created or moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Conflicted files (outcome conflict).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// The commit's subject; an error's text; the integration's tally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// The agent id under which integration events are emitted.
pub const INTEGRATION_AGENT: &str = "integration";

impl VcsEvent {
    /// An event with only the required fields set.
    pub fn new(
        action: VcsAction,
        outcome: VcsOutcome,
        op: &str,
        backend: &str,
        agent: &str,
    ) -> Self {
        Self {
            action,
            outcome,
            op: op.to_string(),
            backend: backend.to_string(),
            agent: agent.to_string(),
            peer: None,
            commit: None,
            change: None,
            source: None,
            branch: None,
            files: Vec::new(),
            summary: None,
        }
    }

    /// "vcs.commit", "vcs.cherry_pick", …
    pub fn event_type(&self) -> String {
        format!("{VCS_SOURCE}.{}", self.action.as_str())
    }

    /// The first 8 characters of `commit`.
    pub fn short(&self) -> Option<&str> {
        self.commit.as_deref().map(|c| c.get(..8).unwrap_or(c))
    }

    /// The payload of a VCS event; None for any other event, or for a payload
    /// this build does not know (an action or outcome added later).
    pub fn from_event(event: &Event) -> Option<Self> {
        if event.source != VCS_SOURCE || !event.event_type.starts_with("vcs.") {
            return None;
        }
        let parsed: Self = serde_json::from_value(event.data.clone()).ok()?;
        (parsed.event_type() == event.event_type).then_some(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EventLevel;
    use serde_json::json;

    fn event(source: &str, event_type: &str, data: serde_json::Value) -> Event {
        Event {
            seq: 1,
            timestamp: 0,
            source: source.to_string(),
            event_type: event_type.to_string(),
            agent_id: None,
            task_id: None,
            message: String::new(),
            level: EventLevel::Info,
            data,
        }
    }

    #[test]
    fn a_vcs_event_round_trips_through_an_event() {
        let mut merge = VcsEvent::new(VcsAction::Merge, VcsOutcome::Ok, "v17", "jj", "ledger");
        merge.peer = Some("db".into());
        merge.source = Some("agents/db".into());
        merge.branch = Some("agents/ledger".into());
        merge.commit = Some("5e6f7a8b9c0d".into());
        merge.change = Some("kzvsvsvnkzvs".into());
        merge.summary = Some("Merge agents/db".into());
        let data = serde_json::to_value(&merge).unwrap();
        assert_eq!(data["action"], "merge");
        assert_eq!(data["outcome"], "ok");
        assert_eq!(merge.event_type(), "vcs.merge");
        assert_eq!(merge.short(), Some("5e6f7a8b"));
        let back = VcsEvent::from_event(&event("vcs", "vcs.merge", data)).unwrap();
        assert_eq!(back, merge);
        // Absent optional fields are left out and read back as absent.
        let pick = VcsEvent::new(
            VcsAction::CherryPick,
            VcsOutcome::Started,
            "v18",
            "git",
            "a",
        );
        let data = serde_json::to_value(&pick).unwrap();
        assert_eq!(
            data,
            json!({ "action": "cherry_pick", "outcome": "started", "op": "v18",
                    "backend": "git", "agent": "a" })
        );
        assert_eq!(
            VcsEvent::from_event(&event("vcs", "vcs.cherry_pick", data)),
            Some(pick)
        );
    }

    #[test]
    fn only_known_vcs_payloads_parse() {
        let known = json!({ "action": "commit", "outcome": "ok", "op": "v1",
                            "backend": "jj", "agent": "a" });
        assert!(VcsEvent::from_event(&event("vcs", "vcs.commit", known.clone())).is_some());
        // Another source, another type, or a type that does not match the
        // payload's action.
        assert!(VcsEvent::from_event(&event("git", "vcs.commit", known.clone())).is_none());
        assert!(VcsEvent::from_event(&event("vcs", "vcs.merge", known.clone())).is_none());
        assert!(VcsEvent::from_event(&event("daemon", "session.started", known)).is_none());
        // An action or outcome this build does not know.
        let later = json!({ "action": "rebase", "outcome": "ok", "op": "v1",
                            "backend": "jj", "agent": "a" });
        assert!(VcsEvent::from_event(&event("vcs", "vcs.rebase", later)).is_none());
        let later = json!({ "action": "commit", "outcome": "queued", "op": "v1",
                            "backend": "jj", "agent": "a" });
        assert!(VcsEvent::from_event(&event("vcs", "vcs.commit", later)).is_none());
        assert!(VcsEvent::from_event(&event("vcs", "vcs.commit", json!(null))).is_none());
    }

    #[test]
    fn reads_and_terminal_outcomes() {
        let reads: Vec<_> = VcsAction::ALL.into_iter().filter(|a| a.is_read()).collect();
        assert_eq!(reads, [VcsAction::Status, VcsAction::Diff]);
        assert!(!VcsOutcome::Started.is_terminal());
        for outcome in [VcsOutcome::Ok, VcsOutcome::Conflict, VcsOutcome::Error] {
            assert!(outcome.is_terminal());
        }
        for action in VcsAction::ALL {
            let wire = serde_json::to_value(action).unwrap();
            assert_eq!(wire, action.as_str());
        }
    }
}
