//! Claude's persisted prompt snapshot includes MCP schemas. Once an explicit
//! refresh changes them, the same conversation must stop using its old snapshot,
//! including on later automatic recovery. The transcript remains untouched.

use anyhow::{bail, Result};
use crew_core::{Agent, RuntimeKind};

use crate::agents::toolbox::ActiveToolbox;
use crate::endpoints::Endpoint;

use super::Daemon;

impl Daemon {
    /// Persist before attempting the process launch: a failed or superseded
    /// launch must not make its next recovery trust the obsolete native snapshot.
    pub(crate) fn claude_toolbox_prompt_policy(
        &self,
        agent: &Agent,
        previous: Option<&ActiveToolbox>,
        explicit_resumed_refresh: bool,
    ) -> bool {
        if agent.runtime != RuntimeKind::Claude {
            return false;
        }
        let rebuild = {
            let mut model = self.model();
            let local = model.local.entry(agent.id.clone()).or_default();
            if local.claude_rebuild_prompt_session != agent.session_id {
                local.claude_rebuild_prompt_session = None;
            }
            let changed =
                previous
                    .zip(local.toolbox_active.as_ref())
                    .is_some_and(|(before, after)| {
                        before.fingerprint != after.fingerprint
                            || before.definition_digest != after.definition_digest
                    });
            if explicit_resumed_refresh && changed {
                local.claude_rebuild_prompt_session = agent.session_id.clone();
            }
            local.claude_rebuild_prompt_session.is_some()
        };
        self.save_local(&agent.id);
        rebuild
    }

    /// Probe only an explicit resumed refresh, before replacing any pin or
    /// setting the SID marker. Ordinary recovery never depends on this probe.
    pub(crate) async fn check_claude_prompt_snapshot(
        &self,
        agent: &Agent,
        endpoint: &Endpoint,
        scrub: &[String],
        since: u64,
    ) -> Result<()> {
        let settings = self
            .model()
            .local
            .get(&agent.id)
            .map(|local| local.settings.clone())
            .unwrap_or_default();
        let home = self
            .sandboxed()
            .then(|| self.agent_dir(&agent.id).join("rw/home"));
        let env = crate::agents::claude::launch_env(
            &settings,
            endpoint,
            &self.agent_dir(&agent.id).join("identity.json"),
            home.as_deref(),
        );
        let run = super::sandbox::Run {
            agent_id: agent.id.clone(),
            program: "claude".into(),
            env,
            sidecar_dir: None,
            terminal: None,
        };
        let result = self
            .toolbox_command(
                agent,
                run,
                vec!["claude".into(), "--version".into()],
                scrub,
                since,
            )
            .await;
        if self.stops(&agent.id) != since {
            bail!("Claude prompt snapshot probe was superseded by a stop");
        }
        if result.is_ok_and(|(exit, bytes)| exit.success() && supports_snapshot(&bytes)) {
            return Ok(());
        }
        let reason = format!(
            "Refreshing this resumed toolbox needs Claude Code 2.1.257 or newer. \
             Update Claude Code, or start a new conversation with `crew agent restart --fresh {}`.",
            agent.id
        );
        bail!("{reason}")
    }
}

fn supports_snapshot(output: &[u8]) -> bool {
    let Some(version) = std::str::from_utf8(output)
        .ok()
        .and_then(|text| text.split_whitespace().next())
    else {
        return false;
    };
    let parts: Vec<_> = version.split('.').map(str::parse::<u32>).collect();
    matches!(parts.as_slice(), [Ok(major), Ok(minor), Ok(patch)]
        if (*major, *minor, *patch) >= (2, 1, 257))
}

#[cfg(test)]
mod tests {
    use super::supports_snapshot;

    #[test]
    fn only_a_supported_claude_version_can_rebuild_a_resumed_snapshot() {
        for version in ["2.1.257 (Claude Code)", "2.1.291 (Claude Code)", "3.0.0"] {
            assert!(supports_snapshot(version.as_bytes()), "{version}");
        }
        for version in [
            "2.1.256 (Claude Code)",
            "2.0.999",
            "unknown",
            "",
            "2.1",
            "2.1.291-dev",
        ] {
            assert!(!supports_snapshot(version.as_bytes()), "{version}");
        }
    }
}
