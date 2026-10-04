//! Agent supervision: starting, stopping and restarting processes, turning
//! runtime signals into agent status, delivering messages into sessions, and
//! recovering after a daemon restart.

use crew_core::config::{AgentSpec, TeamConfig};
use crew_core::{RuntimeKind, ORCHESTRATOR_ID};

use super::Daemon;
use crate::agents::AgentSettings;

#[cfg(test)]
mod binary_tests;
mod control;
mod creation;
mod dialogs;
mod exits;
mod health;
mod launch;
mod recovery;
mod signals;

/// What a start does with the agent's session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// Starts it in the session it has, not resuming one (a new agent).
    New,
    /// Resumes its session, once that is confirmed.
    Resume,
    /// Leaves its session for a new one, never resuming it.
    Fresh,
}

/// The session id a new session of `runtime` starts with: Claude Code and
/// pi let us choose it, which makes resuming exact.
fn chosen_session_id(runtime: RuntimeKind) -> Option<String> {
    matches!(runtime, RuntimeKind::Claude | RuntimeKind::Pi)
        .then(|| uuid::Uuid::new_v4().to_string())
}

/// How an agent declared by `spec` is run: the team's defaults with the
/// declaration's own settings over them.
pub(crate) fn agent_settings(
    team: &TeamConfig,
    spec: &AgentSpec,
    orchestrator: bool,
) -> AgentSettings {
    let mut settings = AgentSettings::resolve(&team.defaults, spec);
    if orchestrator && spec.permission_mode.is_none() {
        // Guarded mode: the orchestrator coordinates and never needs to
        // edit files, and it must never stall on a permission prompt.
        settings.permission_mode = "dontAsk".to_string();
    }
    settings
}

impl Daemon {
    /// The id the scheduler signs task messages with.
    pub(crate) fn dispatcher_for(&self, agent: &str) -> &'static str {
        if agent == ORCHESTRATOR_ID {
            "scheduler"
        } else {
            ORCHESTRATOR_ID
        }
    }
}
