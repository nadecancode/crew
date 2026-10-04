//! Generic adapter: run any command as an agent.
//!
//! The process gets `CREW_IDENTITY` in its environment and coordinates through
//! `crew ctl …`. Incoming messages are announced with a single line on its
//! stdin; the process then fetches them with `crew ctl inbox`. This is what
//! the scripted agent (`crew scripted-agent`) and the test suite use, and it
//! is the template for wiring up other agent CLIs.

use anyhow::{bail, Result};
use crew_core::paths::IDENTITY_ENV;
use crew_core::{Message, RuntimeKind};
use serde_json::Value;

use super::{AgentRuntime, LaunchContext, LaunchSpec, Signal};

pub struct ShellRuntime;

impl AgentRuntime for ShellRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Shell
    }

    fn launch(&self, ctx: &LaunchContext<'_>) -> Result<LaunchSpec> {
        let Some((program, args)) = ctx.settings.command.split_first() else {
            bail!("agent '{}' has no command", ctx.agent.id);
        };
        // `{crew}` stands for this binary, so team files stay portable.
        let crew = ctx.crew_bin.to_string_lossy();
        let expand = |s: &String| s.replace("{crew}", &crew);
        Ok(LaunchSpec {
            agent_id: ctx.agent.id.clone(),
            kind: RuntimeKind::Shell,
            program: expand(program),
            args: args.iter().map(expand).collect(),
            env: vec![
                (
                    IDENTITY_ENV.to_string(),
                    ctx.identity_path.to_string_lossy().into_owned(),
                ),
                ("CREW_AGENT_ID".to_string(), ctx.agent.id.clone()),
                ("CREW_BIN".to_string(), crew.into_owned()),
            ],
            cwd: ctx.agent.workdir.clone().into(),
            prompt: None,
            prompt_as_arg: false,
            sidecar: None,
        })
    }

    fn on_hook(&self, event: &str, _payload: &Value) -> Vec<Signal> {
        match event {
            "ready" => vec![Signal::Ready],
            "turn-start" => vec![Signal::TurnStarted],
            "turn-end" => vec![Signal::TurnEnded],
            _ => Vec::new(),
        }
    }

    /// One short line: a wake-up, not the payload.
    fn nudge(&self, messages: &[Message]) -> String {
        format!(
            "[crew] {} new message(s); run: crew ctl inbox",
            messages.len()
        )
    }
}
