//! Codex CLI adapter.
//!
//! Codex is configured entirely through `-c key=value` overrides, so nothing
//! is written to the user's `~/.codex`. The crew MCP server, the
//! turn-complete notifier and, for a model that has it, the 1M-token context
//! window are injected per session. The standing instructions are the
//! session's developer instructions: part of its system prompt, so they are
//! cached with it and Codex puts them back after a compaction.

use anyhow::{bail, Result};
use crew_core::guardrails::ToolClass;
use crew_core::paths::IDENTITY_ENV;
use crew_core::RuntimeKind;
use serde_json::{json, Value};

use std::path::{Path, PathBuf};

use super::{AgentRuntime, Dialog, LaunchContext, LaunchSpec, Sidecar, Signal};
use crate::endpoints::CODEX_KEY_ENV;

/// Where Codex keeps its configuration, credentials and sessions.
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// Name of the model provider crew defines for an overridden endpoint.
const PROVIDER: &str = "crew";
const OPENAI_API: &str = "https://api.openai.com/v1";
/// The 1M-token context window, as the Codex team documents asking for it:
/// the window, and the point at which Codex compacts the session on its own.
/// Codex caps both at what the account's model allows (`max_context_window`
/// in its model list), so asking is safe for a model that has less.
const LONG_CONTEXT_WINDOW: u64 = 1_000_000;
const LONG_CONTEXT_COMPACT_AT: u64 = 900_000;

pub struct CodexRuntime;

pub(crate) mod toolbox;

/// The developer instructions the user's own Codex config sets: a
/// `developer_instructions` override replaces them, so crew's are added to
/// them instead.
fn own_developer_instructions(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let config: toml::Table = text.parse().ok()?;
    let own = config.get("developer_instructions")?.as_str()?.trim();
    (!own.is_empty()).then(|| own.to_string())
}

/// The directory an agent's app-server socket lives in: its own, under
/// `runtime`, named by a hash of the agent's directory.
fn sidecar_dir(runtime: &Path, agent_dir: &Path) -> PathBuf {
    runtime.join(format!(
        "codex-{:016x}",
        crew_core::util::fnv1a64(agent_dir.to_string_lossy().as_bytes())
    ))
}

/// A TOML value literal. In particular, tables inside `skills.config` use
/// equals signs, not JSON's colons.
fn toml_value(value: &Value) -> String {
    match value {
        Value::Object(table) => format!(
            "{{{}}}",
            table
                .iter()
                .map(|(key, value)| format!("{}={}", json!(key), toml_value(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(toml_value).collect::<Vec<_>>().join(",")
        ),
        _ => value.to_string(),
    }
}

/// Whether a menu's cursor (`›`) sits on the option `option` picks out (it
/// is given the line in lowercase). Codex's input box starts with `›` too,
/// so the line that holds the option is the one to look at.
fn cursor_on(screen: &str, option: impl Fn(&str) -> bool) -> bool {
    screen.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with('›') && option(&line.to_lowercase())
    })
}

impl AgentRuntime for CodexRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Codex
    }

    fn launch(&self, ctx: &LaunchContext<'_>) -> Result<LaunchSpec> {
        let crew_bin = ctx.crew_bin.to_string_lossy().into_owned();
        let identity = ctx.identity_path.to_string_lossy().into_owned();
        let cwd = ctx.agent.workdir.clone();

        // Session configuration, as `-c` overrides. With an app-server the
        // server owns the session, so it gets them too.
        let mut overrides: Vec<String> = Vec::new();
        let mut set = |key: &str, value: Value| {
            overrides.push("-c".into());
            overrides.push(format!("{key}={}", toml_value(&value)));
        };
        let toolbox = ctx.settings.toolbox_launch.as_ref();
        if let Some(toolbox) = toolbox {
            // A whole-table CLI override replaces earlier CLI leaves before
            // Codex merges it with native configuration. Apply the closing
            // patch first, then insert crew and the selected proxy leaves.
            for (key, value) in &toolbox.codex_overrides {
                set(key, value.clone());
            }
        }
        set("mcp_servers.crew.command", json!(crew_bin));
        set("mcp_servers.crew.enabled", json!(true));
        set(
            "mcp_servers.crew.args",
            json!(["mcp", "--identity", identity]),
        );
        // Agents run with approval policy "never", under which Codex refuses
        // MCP tools that would need approval; the crew tools are the agent's
        // own coordination channel, so they are approved up front.
        set(
            "mcp_servers.crew.default_tools_approval_mode",
            json!("approve"),
        );
        if let Some(toolbox) = toolbox {
            for name in &toolbox.servers {
                // Toolbox names are validated bare identifiers. Codex's -c
                // key parser splits on dots and does not unquote segments.
                let key = format!("mcp_servers.{name}");
                set(&format!("{key}.command"), json!(crew_bin));
                set(
                    &format!("{key}.args"),
                    json!(["toolbox", "mcp", name, "--identity", identity]),
                );
                set(&format!("{key}.enabled"), json!(true));
                set(
                    &format!("{key}.default_tools_approval_mode"),
                    json!("approve"),
                );
            }
        }
        // Codex appends the event JSON as the final argument.
        set(
            "notify",
            json!([crew_bin, "hook", "codex-notify", "--identity", identity]),
        );
        // The style's cut of the built-in tools, as far as Codex has
        // switches: no web search; no writes means a read-only sandbox
        // (below). Its shell is how it reads files, so that stays, as do its
        // own subagents (agents::unenforced names what the member is told).
        let tools = ctx.settings.tools.as_deref();
        if tools.is_some_and(|classes| !classes.contains(&ToolClass::Web)) {
            set("web_search", json!("disabled"));
        }
        let read_only = tools.is_some_and(|classes| !classes.contains(&ToolClass::Write));
        // The standing instructions (who the agent is, its role, the team,
        // the tools) go into the system prompt, after whatever the user's own
        // config puts there, rather than into a prompt of their own.
        let mut instructions: Vec<String> = if toolbox.is_none_or(|toolbox| toolbox.inherit_memory)
        {
            own_developer_instructions(&super::home::Operator::detect(ctx.endpoint).codex)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        if !ctx.briefing.trim().is_empty() {
            instructions.push(ctx.briefing.trim_end().to_string());
        }
        // A member writes for other models; keep its prose to the minimum
        // (a model without the setting ignores it). The orchestrator is read
        // by the operator, so it keeps the user's own setting.
        if !ctx.agent.orchestrator {
            set("model_verbosity", json!("low"));
            // Codex's own instructions are written for a person following
            // the session (progress commentary, a final answer to read,
            // markdown and file links); a member's are not read by one.
            instructions.push(super::prompt::member_tone(ctx.settings.voice));
        }
        if !instructions.is_empty() {
            set("developer_instructions", json!(instructions.join("\n\n")));
        }
        if ctx.long_context {
            set("model_context_window", json!(LONG_CONTEXT_WINDOW));
            set(
                "model_auto_compact_token_limit",
                json!(LONG_CONTEXT_COMPACT_AT),
            );
        }
        // An endpoint or key from the crew config becomes a model provider of
        // its own, so the user's Codex login and providers play no part. The
        // key itself travels in the environment of the process that talks to
        // the model, never on the command line.
        let endpoint = ctx.endpoint;
        let mut env: Vec<(String, String)> = endpoint
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Tokens the operator keeps in files: the TUI and the app-server both call
        // the model, so both get them.
        env.extend(
            endpoint
                .secret_env
                .iter()
                .map(|(name, secret)| (name.clone(), secret.expose().to_string())),
        );
        // Shared by the TUI and the app-server (which owns the session and writes
        // its rollout): in a sandbox both keep Codex's home in the private one.
        if let Some(home) = ctx.private_home {
            env.push((
                CODEX_HOME_ENV.to_string(),
                home.join("codex").to_string_lossy().into_owned(),
            ));
        }
        if endpoint.base_url.is_some() || endpoint.api_key.is_some() {
            let base_url = endpoint.base_url.as_deref().unwrap_or(OPENAI_API);
            let wire_api = endpoint.wire_api.as_deref().unwrap_or("responses");
            set("model_provider", json!(PROVIDER));
            set(&format!("model_providers.{PROVIDER}.name"), json!(PROVIDER));
            set(
                &format!("model_providers.{PROVIDER}.base_url"),
                json!(base_url),
            );
            set(
                &format!("model_providers.{PROVIDER}.wire_api"),
                json!(wire_api),
            );
            if let Some(key) = &endpoint.api_key {
                set(
                    &format!("model_providers.{PROVIDER}.env_key"),
                    json!(CODEX_KEY_ENV),
                );
                env.push((CODEX_KEY_ENV.to_string(), key.expose().to_string()));
            }
        }

        // The same permissions as configuration, for the app-server: it owns
        // the session when there is one, and Codex refuses permission flags
        // on a TUI that resumes a session held by a server.
        let sandbox = if read_only {
            "read-only"
        } else if ctx.settings.bypass_permissions {
            "danger-full-access"
        } else if ctx.settings.sandbox.is_empty() {
            "workspace-write"
        } else {
            ctx.settings.sandbox.as_str()
        };
        let server_permissions = [
            "-c".to_string(),
            format!("approval_policy={}", toml_value(&json!("never"))),
            "-c".to_string(),
            format!("sandbox_mode={}", toml_value(&json!(sandbox))),
        ];

        // Socket delivery: run a private app-server and attach the TUI to it,
        // so messages can be queued into the session over its Unix socket.
        let sidecar = ctx.settings.socket_delivery.then(|| -> Result<Sidecar> {
            // In the runtime directory under a hash, not in the agent's state
            // directory: `sun_path` holds about 100 bytes and a nested workspace
            // would overflow it.
            // A directory of its own per agent: a sandbox binds exactly this
            // directory (the agent can reach its own socket and no other).
            let dir = sidecar_dir(&crew_core::paths::runtime_dir(), ctx.agent_dir);
            let socket = dir.join(format!("{}.sock", crew_core::util::now_ms()));
            if socket.as_os_str().len() >= 104 {
                bail!(
                    "the app-server socket path {} is too long for a Unix socket; set a shorter XDG_RUNTIME_DIR",
                    socket.display()
                );
            }
            crew_core::paths::ensure_private_dir(&dir)?;
            let mut args = vec![
                "app-server".to_string(),
                "--listen".to_string(),
                format!("unix://{}", socket.display()),
            ];
            args.extend(overrides.iter().cloned());
            args.extend(server_permissions.iter().cloned());
            args.extend(ctx.settings.extra_args.iter().cloned());
            Ok(Sidecar {
                scrub_env: toolbox.map(|toolbox| toolbox.scrub_env.clone()).unwrap_or_default(),
                program: "codex".into(),
                args,
                cwd: cwd.clone().into(),
                // Codex's tool shells run under the sidecar: it carries the agent's
                // identity like the TUI does.
                env: env
                    .iter()
                    .cloned()
                    .chain(std::iter::once((IDENTITY_ENV.to_string(), identity.clone())))
                    .collect(),
                socket,
            })
        })
        .transpose()?;

        let mut args: Vec<String> = Vec::new();
        if let Some(sidecar) = &sidecar {
            args.push("--remote".into());
            args.push(format!("unix://{}", sidecar.socket.display()));
        }
        args.extend(overrides);
        if ctx.resume && sidecar.is_some() {
            // The server already holds the session with its permissions.
        } else if ctx.settings.bypass_permissions && !read_only {
            // Codex's "yolo" mode: no approvals and no sandbox.
            args.push("--dangerously-bypass-approvals-and-sandbox".into());
        } else {
            if read_only {
                args.push("--sandbox".into());
                args.push("read-only".into());
            } else if !ctx.settings.sandbox.is_empty() {
                args.push("--sandbox".into());
                args.push(ctx.settings.sandbox.clone());
            }
            // Agents run unattended: failures go back to the model instead of
            // a prompt.
            args.push("--ask-for-approval".into());
            args.push("never".into());
        }
        args.push("--cd".into());
        args.push(cwd.clone());
        // The endpoint decides which models exist.
        if let Some(model) = endpoint.model.as_ref().or(ctx.agent.model.as_ref()) {
            args.push("--model".into());
            args.push(model.clone());
        }
        args.extend(ctx.settings.extra_args.iter().cloned());
        if ctx.resume {
            args.push("resume".into());
            match &ctx.agent.session_id {
                Some(thread) => args.push(thread.clone()),
                None => args.push("--last".into()),
            }
        }

        Ok(LaunchSpec {
            scrub_env: toolbox
                .map(|toolbox| toolbox.scrub_env.clone())
                .unwrap_or_default(),
            agent_id: ctx.agent.id.clone(),
            kind: RuntimeKind::Codex,
            program: "codex".into(),
            args,
            env: std::iter::once((IDENTITY_ENV.to_string(), identity))
                .chain(env)
                .collect(),
            cwd: cwd.into(),
            prompt: None,
            // After `resume` a trailing argument would be read as a session id.
            prompt_as_arg: !ctx.resume,
            sidecar,
            sandbox: None,
        })
    }

    fn queue_command(&self, socket: &Path, session: &str, text: &str) -> Option<Vec<String>> {
        Some(vec![
            "codex".into(),
            "queue".into(),
            "--remote".into(),
            format!("unix://{}", socket.display()),
            "--thread".into(),
            session.to_string(),
            "--message".into(),
            text.to_string(),
        ])
    }

    fn on_hook(&self, event: &str, payload: &Value) -> Vec<Signal> {
        match event {
            "codex-notify" => {
                let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
                if kind == "agent-turn-complete" {
                    let id = payload
                        .get("thread-id")
                        .or_else(|| payload.get("thread_id"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    vec![
                        Signal::Session {
                            id,
                            transcript: None,
                        },
                        Signal::TurnEnded,
                    ]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    // Codex's command takes no instructions; what to keep reaches the
    // session in the note crew sends once the compaction is done.
    fn compact_command(&self, _keep: Option<&str>) -> Option<String> {
        Some("/compact".to_string())
    }

    fn detect_limit(&self, screen: &str, now_ms: i64) -> Option<crate::limits::LimitInfo> {
        crate::limits::codex_screen(screen, now_ms)
    }

    fn detect_dialog(&self, screen: &str) -> Option<Dialog> {
        let lower = screen.to_lowercase();
        // A slow request: Codex offers to stop telling about it. Nothing
        // waits on the answer, but the menu holds the keyboard. Enter on the
        // preselected "Dismiss" — never Esc, which would interrupt the turn;
        // and "1" only when the cursor was moved, as a stray "1" would land
        // in the composer if the menu closed by itself meanwhile.
        if lower.contains("dismiss and keep waiting") {
            let on_dismiss = cursor_on(screen, |l| l.contains("dismiss and keep waiting"));
            return Some(Dialog {
                kind: "notice",
                description: "Codex says it is giving the request a little extra thought".into(),
                accept_keys: if on_dismiss { vec!["enter"] } else { vec!["1"] },
            });
        }
        let asks = lower.contains("trust this folder?")
            || lower.contains("do you trust the contents of this directory");
        if asks && (lower.contains("trust and continue") || lower.contains("yes")) {
            // The affirmative option is normally preselected; if the cursor
            // sits elsewhere, pick option 1 by number instead.
            let on_trust = cursor_on(screen, |l| {
                l.contains("trust and continue") || l.contains("yes")
            });
            return Some(Dialog {
                kind: "trust",
                description: "Codex is asking whether to trust this folder".into(),
                accept_keys: if on_trust { vec!["enter"] } else { vec!["1"] },
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn table_overrides_are_native_toml_literals() {
        let value = serde_json::json!([{"path":"/a path/SKILL.md","enabled":false}]);
        let text = format!("skills.config={}", super::toml_value(&value));
        let parsed: toml::Value = text.parse().unwrap();
        assert_eq!(
            serde_json::to_value(&parsed["skills"]["config"]).unwrap(),
            value
        );
    }

    use super::*;
    use crate::agents::AgentSettings;
    use crate::endpoints::{Endpoint, Secret};
    use crew_core::{Agent, AgentStatus};

    fn agent() -> Agent {
        Agent {
            id: "codex-1".into(),
            name: "codex-1".into(),
            role: "db".into(),
            task: None,
            status: AgentStatus::Spawning,
            activity: String::new(),
            runtime: RuntimeKind::Codex,
            model: None,
            workdir: "/w/it's here".into(),
            branch: None,
            backend: "pty".into(),
            backend_ref: None,
            pid: None,
            session_id: None,
            orchestrator: false,
            parent: None,
            subagents: 0,
            temporary: false,
            delivery: String::new(),
            started_at: None,
            last_activity_at: 0,
            progress: None,
            paused: false,
            block_kind: None,
            limit: None,
            context: None,
            compact_pending: false,
            compactions: 0,
            running_model: None,
            model_name: None,
            effort: None,
            fallback: None,
            exit_code: None,
            restarts: 0,
            inbox_unread: 0,
            inbox_total: 0,
            outbox_total: 0,
            usage: None,
            error: None,
            toolbox: None,
        }
    }

    #[test]
    fn launch_uses_config_overrides_only() {
        let agent = agent();
        let settings = AgentSettings {
            sandbox: "workspace-write".into(),
            auto_trust: true,
            ..AgentSettings::default()
        };
        let ctx = LaunchContext {
            agent: &agent,
            settings: &settings,
            crew_bin: Path::new("/bin/crew"),
            agent_dir: Path::new("/state/agents/codex-1"),
            identity_path: Path::new("/state/agents/codex-1/identity.json"),
            briefing: "standing instructions",
            resume: false,
            endpoint: &Endpoint::default(),
            long_context: false,
            private_home: None,
        };
        let spec = CodexRuntime.launch(&ctx).unwrap();
        assert_eq!(spec.program, "codex");
        assert!(spec
            .args
            .contains(&r#"mcp_servers.crew.command="/bin/crew""#.to_string()));
        assert!(spec.args.contains(
            &r#"mcp_servers.crew.args=["mcp","--identity","/state/agents/codex-1/identity.json"]"#
                .to_string()
        ));
        let joined = spec.args.join(" ");
        assert!(joined.contains("--sandbox workspace-write"));
        assert!(joined.contains("--ask-for-approval never"));
        assert!(spec.prompt_as_arg);
        // The standing instructions are in the system prompt, and a member
        // writes for other models: Codex's guidance on writing for a person
        // gives way, in the team's voice.
        assert!(spec.args.contains(&r#"model_verbosity="low""#.to_string()));
        let developer = spec
            .args
            .iter()
            .find(|a| a.starts_with("developer_instructions="))
            .expect("a member's instructions");
        assert!(
            developer.contains("standing instructions\\n\\n# Tone and style"),
            "{developer}"
        );
        assert!(
            developer.contains("No person reads this session")
                && developer.contains("Voice: ultracave"),
            "{developer}"
        );
        // The orchestrator is read by the operator: Codex's own guidance on
        // writing stays, and its standing instructions are all crew adds.
        let orchestrator = Agent {
            orchestrator: true,
            ..agent.clone()
        };
        let lead = CodexRuntime
            .launch(&LaunchContext {
                agent: &orchestrator,
                ..ctx
            })
            .unwrap();
        let developer = lead
            .args
            .iter()
            .find(|a| a.starts_with("developer_instructions="))
            .expect("the orchestrator's instructions");
        assert!(
            developer.contains("standing instructions") && !developer.contains("Tone and style"),
            "{developer}"
        );
        assert!(!lead.args.iter().any(|a| a.starts_with("model_verbosity=")));

        let resumed = CodexRuntime
            .launch(&LaunchContext {
                resume: true,
                ..ctx
            })
            .unwrap();
        assert_eq!(
            &resumed.args[resumed.args.len() - 2..],
            ["resume", "--last"]
        );
        assert!(!resumed.prompt_as_arg);
        assert!(spec.sidecar.is_none());

        // The 1M-token context window is two more overrides, asked for only
        // when the model has it; the app-server, which owns the session
        // when there is one, gets them too.
        let window = [
            "model_context_window=1000000",
            "model_auto_compact_token_limit=900000",
        ];
        assert!(!spec
            .args
            .iter()
            .any(|a| a.starts_with("model_context_window")));
        let long = CodexRuntime
            .launch(&LaunchContext {
                long_context: true,
                private_home: None,
                settings: &AgentSettings {
                    socket_delivery: true,
                    ..settings.clone()
                },
                ..ctx
            })
            .unwrap();
        let sidecar = long.sidecar.expect("an app-server sidecar");
        for key in window {
            assert!(
                long.args.contains(&key.to_string()),
                "{key}: {:?}",
                long.args
            );
            assert!(
                sidecar.args.contains(&key.to_string()),
                "{key}: {:?}",
                sidecar.args
            );
        }
        let at = long.args.iter().position(|a| a == window[0]).unwrap();
        assert_eq!(long.args[at - 1], "-c");
    }

    #[test]
    fn the_users_own_developer_instructions_come_before_the_standing_instructions() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "model = \"gpt-6\"\ndeveloper_instructions = \"Use pnpm, not npm.\"\n",
        )
        .unwrap();
        let endpoint = Endpoint {
            env: [(
                "CODEX_HOME".to_string(),
                home.path().to_string_lossy().into_owned(),
            )]
            .into(),
            ..Endpoint::default()
        };
        let settings = AgentSettings::default();
        let agent = agent();
        let spec = CodexRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &settings,
                crew_bin: Path::new("/bin/crew"),
                agent_dir: Path::new("/state/agents/codex-1"),
                identity_path: Path::new("/state/agents/codex-1/identity.json"),
                briefing: "standing instructions",
                resume: false,
                endpoint: &endpoint,
                long_context: false,
                private_home: None,
            })
            .unwrap();
        let developer = spec
            .args
            .iter()
            .find(|a| a.starts_with("developer_instructions="))
            .unwrap();
        assert!(
            developer.starts_with(
                r#"developer_instructions="Use pnpm, not npm.\n\nstanding instructions\n\n# Tone"#
            ),
            "{developer}"
        );
        // A config without any, or none at all: crew's alone.
        std::fs::write(home.path().join("config.toml"), "model = \"gpt-6\"\n").unwrap();
        assert_eq!(own_developer_instructions(home.path()), None);
        assert_eq!(
            own_developer_instructions(&home.path().join("nowhere")),
            None
        );
    }

    #[test]
    fn the_users_own_developer_instructions_are_never_read_from_an_agents_private_home() {
        // The operator's Codex home, and a sandboxed agent's private one, which
        // the agent can write.
        let operator = tempfile::tempdir().unwrap();
        std::fs::write(
            operator.path().join("config.toml"),
            "developer_instructions = \"Use pnpm, not npm.\"\n",
        )
        .unwrap();
        let private = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(private.path().join("codex")).unwrap();
        std::fs::write(
            private.path().join("codex/config.toml"),
            "developer_instructions = \"written by the agent\"\n",
        )
        .unwrap();
        let endpoint = Endpoint {
            env: [(
                "CODEX_HOME".to_string(),
                operator.path().to_string_lossy().into_owned(),
            )]
            .into(),
            ..Endpoint::default()
        };
        let settings = AgentSettings::default();
        let agent = agent();
        let spec = CodexRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &settings,
                crew_bin: Path::new("/bin/crew"),
                agent_dir: Path::new("/state/agents/codex-1"),
                identity_path: Path::new("/state/agents/codex-1/identity.json"),
                briefing: "standing instructions",
                resume: false,
                endpoint: &endpoint,
                long_context: false,
                private_home: Some(private.path()),
            })
            .unwrap();
        // What the daemon puts in the system prompt comes from the operator's
        // own configuration, whatever the agent wrote in its home.
        let developer = spec
            .args
            .iter()
            .find(|a| a.starts_with("developer_instructions="))
            .unwrap();
        assert!(developer.contains("Use pnpm, not npm."), "{developer}");
        assert!(!developer.contains("written by the agent"), "{developer}");
        // The session itself runs with the private home: of the two values the
        // launch names, the later one is the one the process gets.
        let session_home = spec
            .env
            .iter()
            .rev()
            .find(|(name, _)| name == CODEX_HOME_ENV)
            .map(|(_, value)| value.clone());
        assert_eq!(
            session_home,
            Some(private.path().join("codex").to_string_lossy().into_owned())
        );
    }

    #[test]
    fn an_endpoint_becomes_a_private_model_provider_with_its_key_in_the_environment() {
        let agent = agent();
        let launch = |endpoint: &Endpoint, socket_delivery: bool| {
            CodexRuntime
                .launch(&LaunchContext {
                    agent: &agent,
                    settings: &AgentSettings {
                        socket_delivery,
                        ..AgentSettings::default()
                    },
                    crew_bin: Path::new("/bin/crew"),
                    agent_dir: Path::new("/state/agents/codex-1"),
                    identity_path: Path::new("/state/agents/codex-1/identity.json"),
                    briefing: "",
                    resume: false,
                    endpoint,
                    long_context: false,
                    private_home: None,
                })
                .unwrap()
        };
        // Nothing configured: no provider is defined, the user's Codex setup applies.
        let plain = launch(&Endpoint::default(), false);
        assert!(!plain.args.iter().any(|a| a.contains("model_provider")));
        assert_eq!(plain.env.len(), 1);

        let endpoint = Endpoint {
            base_url: Some("https://gateway.example/v1".into()),
            api_key: Some(Secret::new("sk-crew-codex-secret")),
            model: Some("gateway-model".into()),
            wire_api: Some("chat".into()),
            ..Endpoint::default()
        };
        let spec = launch(&endpoint, false);
        for expected in [
            r#"model_provider="crew""#,
            r#"model_providers.crew.base_url="https://gateway.example/v1""#,
            r#"model_providers.crew.wire_api="chat""#,
            r#"model_providers.crew.env_key="CREW_CODEX_API_KEY""#,
        ] {
            assert!(
                spec.args.contains(&expected.to_string()),
                "{expected}: {:?}",
                spec.args
            );
        }
        assert!(spec.args.join(" ").contains("--model gateway-model"));
        // The key is in the environment, not in an argument.
        assert!(!spec.args.iter().any(|a| a.contains("sk-crew-codex-secret")));
        assert!(spec.env.contains(&(
            "CREW_CODEX_API_KEY".to_string(),
            "sk-crew-codex-secret".to_string()
        )));

        // With an app-server the server makes the model calls: it gets the
        // provider and the key too.
        let spec = launch(&endpoint, true);
        let sidecar = spec.sidecar.unwrap();
        assert!(sidecar
            .args
            .contains(&r#"model_provider="crew""#.to_string()));
        assert!(!sidecar
            .args
            .iter()
            .any(|a| a.contains("sk-crew-codex-secret")));
        assert_eq!(sidecar.env[0].0, "CREW_CODEX_API_KEY");

        // A key alone moves the session onto the OpenAI API with that key.
        let key_only = Endpoint {
            api_key: Some(Secret::new("sk-crew-codex-secret")),
            ..Endpoint::default()
        };
        let spec = launch(&key_only, false);
        assert!(spec
            .args
            .contains(&r#"model_providers.crew.base_url="https://api.openai.com/v1""#.to_string()));
        assert!(spec
            .args
            .contains(&r#"model_providers.crew.wire_api="responses""#.to_string()));
    }

    #[test]
    fn the_sidecar_socket_path_fits_sun_path_for_a_deep_workspace() {
        let agent = agent();
        let deep = format!("/state/{}/agents/codex-1", "nested/".repeat(18));
        assert!(deep.len() > 120);
        let spec = CodexRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &AgentSettings {
                    socket_delivery: true,
                    ..AgentSettings::default()
                },
                crew_bin: Path::new("/bin/crew"),
                agent_dir: Path::new(&deep),
                identity_path: &Path::new(&deep).join("identity.json"),
                briefing: "standing instructions",
                resume: false,
                endpoint: &Endpoint::default(),
                long_context: false,
                private_home: None,
            })
            .unwrap();
        let socket = spec.sidecar.expect("an app-server sidecar").socket;
        assert!(socket.as_os_str().len() < 104, "{}", socket.display());
    }

    #[test]
    fn sidecar_env_has_crew_identity() {
        let agent = agent();
        let spec = CodexRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &AgentSettings {
                    socket_delivery: true,
                    ..AgentSettings::default()
                },
                crew_bin: Path::new("/bin/crew"),
                agent_dir: Path::new("/state/agents/codex-1"),
                identity_path: Path::new("/state/agents/codex-1/identity.json"),
                briefing: "standing instructions",
                resume: false,
                endpoint: &Endpoint::default(),
                long_context: false,
                private_home: None,
            })
            .unwrap();
        let sidecar = spec.sidecar.expect("an app-server sidecar");
        let identity = |env: &[(String, String)]| {
            env.iter()
                .find(|(k, _)| k == IDENTITY_ENV)
                .map(|(_, v)| v.clone())
        };
        // Codex tool shells run under the sidecar, so it needs the identity
        // the TUI has.
        assert_eq!(
            identity(&sidecar.env).as_deref(),
            Some("/state/agents/codex-1/identity.json")
        );
        assert_eq!(identity(&spec.env), identity(&sidecar.env));
    }

    #[test]
    fn the_sidecar_socket_has_a_private_directory_of_its_own() {
        use std::os::unix::fs::PermissionsExt;
        let run = tempfile::tempdir().unwrap();
        // The runtime directory is passed in: the test process shares its
        // environment with every other test, so it is not set here.
        let runtime = run.path().join("crew");
        let dir_a = sidecar_dir(&runtime, Path::new("/state/agents/a"));
        let dir_b = sidecar_dir(&runtime, Path::new("/state/agents/b"));
        assert_ne!(dir_a, dir_b, "agents share a socket directory");
        assert_ne!(dir_a, runtime, "the socket is in the runtime dir itself");
        assert!(dir_a.starts_with(&runtime));
        crew_core::paths::ensure_private_dir(&dir_a).unwrap();
        let mode = std::fs::metadata(&dir_a).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn a_token_from_a_file_goes_into_the_process_environment_and_nowhere_else() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent();
        let identity = dir.path().join("identity.json");
        let endpoint = Endpoint {
            secret_env: vec![(
                "OPENAI_API_KEY".into(),
                crate::endpoints::Secret::new("sk-codex-value"),
            )],
            ..Endpoint::default()
        };
        let spec = CodexRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &AgentSettings {
                    socket_delivery: true,
                    ..AgentSettings::default()
                },
                crew_bin: Path::new("/opt/crew"),
                agent_dir: dir.path(),
                identity_path: &identity,
                briefing: "be helpful",
                resume: false,
                endpoint: &endpoint,
                long_context: false,
                private_home: None,
            })
            .unwrap();
        assert!(spec
            .env
            .contains(&("OPENAI_API_KEY".into(), "sk-codex-value".into())));
        // Not on a command line, not in the files the daemon writes for the session.
        assert!(!spec.args.iter().any(|a| a.contains("sk-codex-value")));
        // The app-server calls the model too: it gets the token, and no command line has it.
        let sidecar = spec.sidecar.as_ref().expect("an app-server");
        assert!(sidecar
            .env
            .contains(&("OPENAI_API_KEY".into(), "sk-codex-value".into())));
        assert!(!sidecar.args.iter().any(|a| a.contains("sk-codex-value")));
    }

    #[test]
    fn a_member_without_writes_runs_in_a_read_only_sandbox_whatever_the_permission_policy() {
        let agent = agent();
        let launch = |tools: Option<Vec<ToolClass>>, bypass: bool| {
            CodexRuntime
                .launch(&LaunchContext {
                    agent: &agent,
                    settings: &AgentSettings {
                        bypass_permissions: bypass,
                        sandbox: "workspace-write".into(),
                        socket_delivery: true,
                        tools,
                        ..AgentSettings::default()
                    },
                    crew_bin: Path::new("/bin/crew"),
                    agent_dir: Path::new("/state/agents/codex-1"),
                    identity_path: Path::new("/state/agents/codex-1/identity.json"),
                    briefing: "standing instructions",
                    resume: false,
                    endpoint: &Endpoint::default(),
                    long_context: false,
                    private_home: None,
                })
                .unwrap()
        };
        let reader = launch(Some(vec![ToolClass::Read, ToolClass::Shell]), true);
        let joined = reader.args.join(" ");
        assert!(
            !joined.contains("--dangerously-bypass-approvals-and-sandbox"),
            "{joined}"
        );
        assert!(joined.contains("--sandbox read-only"), "{joined}");
        assert!(joined.contains("--ask-for-approval never"), "{joined}");
        let server = reader.sidecar.unwrap().args.join(" ");
        assert!(server.contains(r#"sandbox_mode="read-only""#), "{server}");
        assert!(server.contains(r#"web_search="disabled""#), "{server}");

        // With writes and the web, the policy decides as before.
        let writer = launch(Some(ToolClass::ALL.to_vec()), true);
        let joined = writer.args.join(" ");
        assert!(
            joined.contains("--dangerously-bypass-approvals-and-sandbox"),
            "{joined}"
        );
        assert!(!joined.contains("read-only"), "{joined}");
        let server = writer.sidecar.unwrap().args.join(" ");
        assert!(!server.contains("web_search"), "{server}");
        assert!(
            server.contains(r#"sandbox_mode="danger-full-access""#),
            "{server}"
        );
        let guarded = launch(Some(vec![ToolClass::Write, ToolClass::Shell]), false);
        let joined = guarded.args.join(" ");
        assert!(joined.contains("--sandbox workspace-write"), "{joined}");
        assert!(guarded
            .sidecar
            .unwrap()
            .args
            .join(" ")
            .contains(r#"web_search="disabled""#));
    }

    #[test]
    fn a_sandboxed_launch_keeps_codexs_home_in_the_private_home_for_the_tui_and_the_sidecar() {
        let agent = agent();
        let home = Path::new("/state/agents/codex-1/rw/home");
        let launch = |private_home: Option<&Path>| {
            CodexRuntime
                .launch(&LaunchContext {
                    agent: &agent,
                    settings: &AgentSettings {
                        socket_delivery: true,
                        ..AgentSettings::default()
                    },
                    crew_bin: Path::new("/bin/crew"),
                    agent_dir: Path::new("/state/agents/codex-1"),
                    identity_path: Path::new("/state/agents/codex-1/identity.json"),
                    briefing: "standing instructions",
                    resume: false,
                    endpoint: &Endpoint::default(),
                    long_context: false,
                    private_home,
                })
                .unwrap()
        };
        let var = |env: &[(String, String)]| {
            env.iter()
                .find(|(k, _)| k == CODEX_HOME_ENV)
                .map(|(_, v)| v.clone())
        };
        // The app-server owns the session and writes its rollout, so it needs the
        // same home as the TUI that attaches to it.
        let spec = launch(Some(home));
        let want = Some("/state/agents/codex-1/rw/home/codex".to_string());
        assert_eq!(var(&spec.env), want);
        assert_eq!(var(&spec.sidecar.as_ref().unwrap().env), want);
        let plain = launch(None);
        assert_eq!(var(&plain.env), None);
        assert_eq!(var(&plain.sidecar.unwrap().env), None);
    }

    #[test]
    fn socket_delivery_attaches_the_tui_to_a_private_app_server() {
        let agent = agent();
        let settings = AgentSettings {
            sandbox: "workspace-write".into(),
            socket_delivery: true,
            ..AgentSettings::default()
        };
        let spec = CodexRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &settings,
                crew_bin: Path::new("/bin/crew"),
                agent_dir: Path::new("/state/agents/codex-1"),
                identity_path: Path::new("/state/agents/codex-1/identity.json"),
                briefing: "standing instructions",
                resume: false,
                endpoint: &Endpoint::default(),
                long_context: false,
                private_home: None,
            })
            .unwrap();
        let sidecar = spec.sidecar.expect("an app-server sidecar");
        assert_eq!(sidecar.program, "codex");
        assert_eq!(sidecar.args[0], "app-server");
        let listen = format!("unix://{}", sidecar.socket.display());
        assert_eq!(sidecar.args[2], listen);
        // The server owns the session, so it carries the MCP configuration.
        assert!(sidecar
            .args
            .contains(&r#"mcp_servers.crew.command="/bin/crew""#.to_string()));
        assert!(spec.args.contains(&"--sandbox".to_string()));

        let yolo = CodexRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &AgentSettings {
                    bypass_permissions: true,
                    sandbox: "workspace-write".into(),
                    ..AgentSettings::default()
                },
                crew_bin: Path::new("/bin/crew"),
                agent_dir: Path::new("/state/agents/codex-1"),
                identity_path: Path::new("/state/agents/codex-1/identity.json"),
                briefing: "standing instructions",
                resume: false,
                endpoint: &Endpoint::default(),
                long_context: false,
                private_home: None,
            })
            .unwrap();
        assert!(yolo
            .args
            .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(!yolo.args.contains(&"--sandbox".to_string()));
        assert!(!yolo.args.contains(&"--ask-for-approval".to_string()));
        assert_eq!(&spec.args[..2], ["--remote".to_string(), listen]);

        // The server carries the permissions as configuration…
        assert!(sidecar
            .args
            .contains(&r#"approval_policy="never""#.to_string()));
        assert!(sidecar
            .args
            .contains(&r#"sandbox_mode="workspace-write""#.to_string()));
        // …so a TUI that resumes the server's session passes no permission
        // flags, which Codex would refuse ("Permission overrides are not
        // supported when resuming a remote task").
        let mut with_session = agent.clone();
        with_session.session_id = Some("thread-9".into());
        for bypass in [true, false] {
            let resumed = CodexRuntime
                .launch(&LaunchContext {
                    agent: &with_session,
                    settings: &AgentSettings {
                        bypass_permissions: bypass,
                        sandbox: "workspace-write".into(),
                        socket_delivery: true,
                        ..AgentSettings::default()
                    },
                    crew_bin: Path::new("/bin/crew"),
                    agent_dir: Path::new("/state/agents/codex-1"),
                    identity_path: Path::new("/state/agents/codex-1/identity.json"),
                    briefing: "standing instructions",
                    resume: true,
                    endpoint: &Endpoint::default(),
                    long_context: false,
                    private_home: None,
                })
                .unwrap();
            assert_eq!(
                &resumed.args[resumed.args.len() - 2..],
                ["resume", "thread-9"]
            );
            for flag in [
                "--dangerously-bypass-approvals-and-sandbox",
                "--sandbox",
                "--ask-for-approval",
            ] {
                assert!(
                    !resumed.args.contains(&flag.to_string()),
                    "{flag}: {:?}",
                    resumed.args
                );
            }
            let server = resumed.sidecar.unwrap();
            let expected = if bypass {
                "danger-full-access"
            } else {
                "workspace-write"
            };
            assert!(
                server
                    .args
                    .contains(&format!("sandbox_mode=\"{expected}\"")),
                "{:?}",
                server.args
            );
        }

        let queue = CodexRuntime
            .queue_command(&sidecar.socket, "thread-1", "hello; rm -rf /")
            .unwrap();
        assert_eq!(queue[0..2], ["codex", "queue"]);
        // The message is one argument, never a shell string.
        assert_eq!(queue.last().unwrap(), "hello; rm -rf /");
        assert!(queue.contains(&"thread-1".to_string()));
    }

    #[test]
    fn detects_the_folder_trust_dialog() {
        let screen = "  Trust this folder? Codex can read, edit, and run files here.\n\
› 1. Trust and continue\n  2. Back to Agent Command Center\n  enter continue · esc back";
        let dialog = CodexRuntime.detect_dialog(screen).unwrap();
        assert_eq!(dialog.kind, "trust");
        assert_eq!(dialog.accept_keys, vec!["enter"]);
        assert!(CodexRuntime
            .detect_dialog("› Ask Codex to do anything")
            .is_none());
    }

    #[test]
    fn detects_the_extra_thought_notice_and_dismisses_it_with_enter() {
        // The input box's own `›` is on screen too.
        let screen =
            "› Ask Codex to do anything\n\n  Giving this request a little extra thought\n\n\
› 1. Dismiss and keep waiting\n  2. Learn more\n\n  No action is required. Codex will keep \
waiting, and this menu will close when the response is ready.";
        let dialog = CodexRuntime.detect_dialog(screen).unwrap();
        assert_eq!(
            (dialog.kind, dialog.accept_keys.as_slice()),
            ("notice", &["enter"][..])
        );
        // The cursor moved to "Learn more": the dismiss option by its number.
        let moved = screen
            .replace("› 1. Dismiss", "  1. Dismiss")
            .replace("  2. Learn", "› 2. Learn");
        assert_eq!(
            CodexRuntime.detect_dialog(&moved).unwrap().accept_keys,
            vec!["1"]
        );
    }

    #[test]
    fn turn_complete_notification_ends_the_turn() {
        let signals = CodexRuntime.on_hook(
            "codex-notify",
            &json!({"type": "agent-turn-complete", "thread-id": "t-1"}),
        );
        assert_eq!(
            signals,
            vec![
                Signal::Session {
                    id: Some("t-1".into()),
                    transcript: None
                },
                Signal::TurnEnded
            ]
        );
        assert!(CodexRuntime
            .on_hook("codex-notify", &json!({"type": "something-else"}))
            .is_empty());
    }
}
