//! Claude Code adapter.
//!
//! An agent is an ordinary interactive `claude` session. Crew wires it up
//! with three generated files in the agent's private directory:
//! - `mcp.json`: the `crew` MCP server (this binary in `mcp` mode),
//! - `settings.json`: hooks that report lifecycle events back to the daemon,
//! - `briefing.md`: standing instructions appended to the system prompt.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Result;
use crew_core::guardrails::ToolClass;
use crew_core::paths::{write_private_file, IDENTITY_ENV};
use crew_core::util::{one_line, truncate_chars};
use crew_core::{RuntimeKind, Usage};
use serde_json::{json, Value};

use super::{shell_quote, AgentRuntime, Dialog, LaunchContext, LaunchSpec, Signal};
use crate::endpoints::Endpoint;
use crate::limits;

pub struct ClaudeRuntime;

/// Hook events crew listens to, as `(Claude Code event, crew hook name)`.
const HOOKS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "user-prompt-submit"),
    ("PreToolUse", "pre-tool-use"),
    ("PostToolUse", "post-tool-use"),
    ("Notification", "notification"),
    ("Stop", "stop"),
    // Fires instead of Stop when an API error (a usage limit among them)
    // ended the turn.
    ("StopFailure", "stop-failure"),
    ("SubagentStart", "subagent-start"),
    ("SubagentStop", "subagent-stop"),
    ("SessionEnd", "session-end"),
    // After any compaction: one crew asked for, a manual one, or the
    // engine's own when the window is full.
    ("PostCompact", "post-compact"),
];

/// Source of the mod that receives messages over the daemon socket.
const BRIDGE_MOD: &str = include_str!("../../assets/crew-bridge.register.ts");
const BRIDGE_MOD_NAME: &str = "crew-bridge";

/// Write the socket-delivery mod into the agent's directory and return the
/// plugin folder to pass to `--plugin-dir`.
fn write_bridge_mod(
    agent_dir: &Path,
    crew_bin: &Path,
    identity: &Path,
    deliver: bool,
    tone: Option<&str>,
) -> Result<std::path::PathBuf> {
    let root = agent_dir.join("mod").join(BRIDGE_MOD_NAME);
    crew_core::paths::ensure_private_dir(&root.join(".claude-plugin"))?;
    crew_core::paths::ensure_private_dir(&root.join("hooks"))?;
    let manifest = json!({
        "name": BRIDGE_MOD_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Delivers crew team messages into this session over the crew daemon socket",
        "author": { "name": "crew" },
    });
    write_private_file(
        &root.join(".claude-plugin").join("plugin.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    write_private_file(
        &root.join("hooks").join("hooks.json"),
        &serde_json::to_vec_pretty(&json!({ "modules": ["./register.ts"] }))?,
    )?;
    // The values are embedded as JSON literals, which are valid TypeScript
    // literals.
    let source = BRIDGE_MOD
        .replace(
            "__CREW_BIN__",
            &json!(crew_bin.to_string_lossy()).to_string(),
        )
        .replace(
            "__IDENTITY__",
            &json!(identity.to_string_lossy()).to_string(),
        )
        .replace("__DELIVER__", &json!(deliver).to_string())
        .replace("__TONE__", &json!(tone).to_string());
    write_private_file(&root.join("hooks").join("register.ts"), source.as_bytes())?;
    Ok(root)
}

/// What Claude Code appends to a model's name for its 1M-token context
/// variant (`opus[1m]`).
const LONG_CONTEXT_TAG: &str = "[1m]";
/// Set, Claude Code never substitutes another model for the session's (when
/// its usage window is used up, or it is overloaded); the turn fails instead.
const NO_FALLBACK_ENV: &str = "CLAUDE_CODE_NO_MODEL_FALLBACK";

/// Claude Code's built-in tools, by class (Claude Code 2.1.295). Whatever
/// the classes, a member keeps its skills, its task list and the tool
/// search; it never gets the ask-the-user dialog (nobody watches its
/// terminal) or the plan-mode switches.
const CLAUDE_TOOLS_ALWAYS: &[&str] = &["Skill", "TodoWrite", "ToolSearch"];
const CLAUDE_TOOLS: &[(ToolClass, &[&str])] = &[
    (
        ToolClass::Read,
        &[
            "Read",
            "Glob",
            "Grep",
            "ListMcpResourcesTool",
            "ReadMcpResourceTool",
        ],
    ),
    (
        ToolClass::Write,
        &["Write", "Edit", "MultiEdit", "NotebookEdit"],
    ),
    (
        ToolClass::Shell,
        &["Bash", "BashOutput", "KillShell", "TaskOutput", "TaskStop"],
    ),
    (ToolClass::Web, &["WebFetch", "WebSearch"]),
    (ToolClass::Agents, &["Agent", "SendMessage", "ListAgents"]),
];

/// The `--tools` list for a member that gets `classes` of the built-ins.
pub(crate) fn claude_tools(classes: &[ToolClass]) -> Vec<&'static str> {
    let mut tools: Vec<&'static str> = CLAUDE_TOOLS_ALWAYS.to_vec();
    for (class, names) in CLAUDE_TOOLS {
        if classes.contains(class) {
            tools.extend(names.iter().copied());
        }
    }
    tools
}
/// Where Claude Code keeps its configuration, credentials, sessions and
/// transcripts (and `.claude.json`, which moves with it).
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Claude Code's tool for putting a multiple-choice question to the person at
/// the terminal.
const ASK_USER_TOOL: &str = "AskUserQuestion";

fn hooks_settings(crew_bin: &Path, identity: &Path) -> Value {
    let mut hooks = serde_json::Map::new();
    for (event, name) in HOOKS {
        // Claude Code runs hook commands through a shell, so both paths are quoted.
        let command = format!(
            "{} hook {name} --identity {}",
            shell_quote(&crew_bin.to_string_lossy()),
            shell_quote(&identity.to_string_lossy()),
        );
        let mut entry = json!({
            "hooks": [{ "type": "command", "command": command, "timeout": 10 }]
        });
        if matches!(*event, "PreToolUse" | "PostToolUse") {
            entry["matcher"] = json!("*");
        }
        hooks.insert((*event).to_string(), json!([entry]));
    }
    json!({ "hooks": hooks })
}

/// Point the session at the endpoint from the crew config. Everything goes
/// into the session's private settings file, never onto the command line:
/// Claude Code applies `env` from there over its own environment.
fn apply_endpoint(settings: &mut Value, endpoint: &Endpoint, crew_bin: &Path, identity: &Path) {
    let mut env = serde_json::Map::new();
    if let Some(url) = &endpoint.base_url {
        env.insert("ANTHROPIC_BASE_URL".into(), json!(url));
    }
    if endpoint.api_key.is_some() || endpoint.auth_token.is_some() {
        // Blank whatever the session would inherit from the operator's
        // shell: Claude Code would otherwise send that key alongside, or
        // instead of, the configured one.
        env.insert("ANTHROPIC_API_KEY".into(), json!(""));
        let token = endpoint.auth_token.as_ref().map_or("", |t| t.expose());
        env.insert("ANTHROPIC_AUTH_TOKEN".into(), json!(token));
    }
    for (name, value) in &endpoint.env {
        env.insert(name.clone(), json!(value));
    }
    if !env.is_empty() {
        settings["env"] = Value::Object(env);
    }
    if endpoint.api_key.is_some() {
        // The session asks the daemon for the key when it needs it, so the
        // key sits in no file and no argument. Claude Code runs the helper
        // through a shell, hence the quoting.
        settings["apiKeyHelper"] = json!(format!(
            "{} credential --identity {}",
            shell_quote(&crew_bin.to_string_lossy()),
            shell_quote(&identity.to_string_lossy()),
        ));
    }
}

pub(crate) fn launch_env(
    settings: &super::AgentSettings,
    endpoint: &Endpoint,
    identity: &Path,
    private_home: Option<&Path>,
) -> Vec<(String, String)> {
    let mut env = vec![(
        IDENTITY_ENV.to_string(),
        identity.to_string_lossy().into_owned(),
    )];
    if !settings.model_fallback {
        env.push((NO_FALLBACK_ENV.to_string(), "1".to_string()));
    }
    // File-backed model credentials stay in process environments.
    env.extend(
        endpoint
            .secret_env
            .iter()
            .map(|(name, secret)| (name.clone(), secret.expose().to_string())),
    );
    if let Some(home) = private_home {
        env.push((
            CONFIG_DIR_ENV.to_string(),
            home.join("claude").to_string_lossy().into_owned(),
        ));
    }
    env
}

impl AgentRuntime for ClaudeRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Claude
    }

    fn launch(&self, ctx: &LaunchContext<'_>) -> Result<LaunchSpec> {
        let mcp_path = ctx.agent_dir.join("mcp.json");
        let settings_path = ctx.agent_dir.join("settings.json");
        let briefing_path = ctx.agent_dir.join("briefing.md");

        let crew = json!({
            "command": ctx.crew_bin.to_string_lossy(),
            "args": ["mcp", "--identity", ctx.identity_path.to_string_lossy()],
        });
        let toolbox = ctx.settings.toolbox_launch.as_ref();
        let servers = toolbox.into_iter().flat_map(|toolbox| toolbox.servers.iter()).map(|name| {
            (name.clone(), json!({"command":ctx.crew_bin.to_string_lossy(),"args":["toolbox","mcp",name,"--identity",ctx.identity_path.to_string_lossy()]}))
        }).collect();
        write_private_file(
            &mcp_path,
            &super::toolbox::claude_mcp_bytes(&crew, &servers)?,
        )?;
        let mut settings = hooks_settings(ctx.crew_bin, ctx.identity_path);
        if let Some(preferences) =
            toolbox.and_then(|toolbox| toolbox.claude_preferences.as_object())
        {
            for (name, value) in preferences {
                settings[name] = value.clone();
            }
        }
        // Nobody sits at this terminal: a built-in question dialog would hold
        // the agent until someone attached to it. Questions go over the bus
        // (the orchestrator, then `operator_ask`), where the operator sees them.
        settings["permissions"] = json!({ "deny": [ASK_USER_TOOL] });
        // When the model's safeguards flag a turn, Claude Code moves the
        // session to another model by itself instead of asking at the
        // terminal (its "Ask each time" setting would hold the agent there).
        settings["switchModelsOnFlag"] = json!(true);
        // Claude Code's own memory is the team's: one directory for every
        // member and worktree, which crew indexes for the whole team. Left
        // alone, the orchestrator would read and write the operator's own
        // memory of the workspace, and a member one of its worktree's.
        match ctx.memory_dir {
            Some(dir) => {
                settings["autoMemoryEnabled"] = json!(true);
                settings["autoMemoryDirectory"] = json!(dir.to_string_lossy());
            }
            None => settings["autoMemoryEnabled"] = json!(false),
        }
        apply_endpoint(&mut settings, ctx.endpoint, ctx.crew_bin, ctx.identity_path);
        write_private_file(&settings_path, &serde_json::to_vec_pretty(&settings)?)?;
        write_private_file(&briefing_path, ctx.briefing.as_bytes())?;

        let mut args: Vec<String> = Vec::new();
        if toolbox.is_some_and(|toolbox| toolbox.claude_rebuild_prompt) {
            args.extend(["--system-prompt-snapshot".into(), "off".into()]);
        }
        match (&ctx.agent.session_id, ctx.resume) {
            (Some(id), true) => {
                args.push("--resume".into());
                args.push(id.clone());
            }
            (Some(id), false) => {
                args.push("--session-id".into());
                args.push(id.clone());
            }
            (None, _) => {}
        }
        args.push("--name".into());
        args.push(ctx.agent.id.clone());
        args.push("--mcp-config".into());
        args.push(mcp_path.to_string_lossy().into_owned());
        let inherit_mcp = toolbox.map_or(ctx.settings.inherit_mcp, |toolbox| {
            toolbox.claude_inherit_mcp
        });
        if !inherit_mcp {
            args.push("--strict-mcp-config".into());
        }
        if let Some(toolbox) = toolbox {
            args.extend(["--setting-sources".into(), toolbox.claude_sources.clone()]);
        }
        args.push("--settings".into());
        args.push(settings_path.to_string_lossy().into_owned());
        args.push("--append-system-prompt-file".into());
        args.push(briefing_path.to_string_lossy().into_owned());
        // crew's mod: socket delivery, and for a member the system prompt's
        // guidance on writing for a person replaced by its own.
        let tone =
            (!ctx.agent.orchestrator).then(|| super::prompt::member_tone(ctx.settings.voice));
        if ctx.settings.socket_delivery || tone.is_some() {
            let mod_dir = write_bridge_mod(
                ctx.agent_dir,
                ctx.crew_bin,
                ctx.identity_path,
                ctx.settings.socket_delivery,
                tone.as_deref(),
            )?;
            args.push("--plugin-dir".into());
            args.push(mod_dir.to_string_lossy().into_owned());
        }
        if let Some(toolbox) = toolbox {
            for plugin in &toolbox.claude_plugins {
                args.extend(["--plugin-dir".into(), plugin.to_string_lossy().into_owned()]);
            }
        }
        if ctx.settings.bypass_permissions {
            args.push("--dangerously-skip-permissions".into());
        } else if !ctx.settings.permission_mode.is_empty() {
            args.push("--permission-mode".into());
            args.push(ctx.settings.permission_mode.clone());
        }
        // The crew tools must never raise a permission prompt.
        let mut allowed = vec!["mcp__crew".to_string()];
        if let Some(toolbox) = toolbox {
            allowed.extend(toolbox.servers.iter().map(|name| format!("mcp__{name}")));
        }
        allowed.extend(ctx.settings.allowed_tools.iter().cloned());
        args.push("--allowedTools".into());
        args.push(allowed.join(","));
        // The style's cut of the built-in tools (MCP tools are not among
        // them, so the crew tools and the toolbox stay).
        if let Some(classes) = &ctx.settings.tools {
            args.push("--tools".into());
            args.push(claude_tools(classes).join(","));
        }
        // The endpoint decides which models exist.
        if let Some(model) = ctx.endpoint.model.as_ref().or(ctx.agent.model.as_ref()) {
            args.push("--model".into());
            // The 1M-token window is a variant of the model, asked for by a
            // tag on its name.
            args.push(if ctx.long_context && !model.contains('[') {
                format!("{model}{LONG_CONTEXT_TAG}")
            } else {
                model.clone()
            });
        }
        args.extend(ctx.settings.extra_args.iter().cloned());
        // The first prompt follows as an argument of its own. Options such as
        // `--allowedTools` take any number of values and would swallow it
        // (Claude Code 2.1.288 does): the options end here.
        args.push("--".into());
        let env = launch_env(
            ctx.settings,
            ctx.endpoint,
            ctx.identity_path,
            ctx.private_home,
        );

        Ok(LaunchSpec {
            scrub_env: toolbox
                .map(|toolbox| toolbox.scrub_env.clone())
                .unwrap_or_default(),
            agent_id: ctx.agent.id.clone(),
            kind: RuntimeKind::Claude,
            program: "claude".into(),
            args,
            env,
            cwd: ctx.agent.workdir.clone().into(),
            prompt: None,
            prompt_as_arg: true,
            sidecar: None,
            sandbox: None,
        })
    }

    fn on_hook(&self, event: &str, payload: &Value) -> Vec<Signal> {
        match event {
            "stop-failure" if payload.get("agent_id").is_none_or(|v| v.is_null()) => {
                return vec![
                    match limits::claude_stop_failure(payload, crew_core::util::now_ms()) {
                        limits::TurnFailure::Limited(info) => Signal::Limited(info),
                        limits::TurnFailure::Other(what) => Signal::TurnFailed(what),
                    },
                ];
            }
            // Sent by the crew mod when the engine measures the session.
            "usage" => {
                let mut signals = Vec::new();
                let figure = |key: &str| {
                    payload
                        .pointer(&format!("/context/{key}"))
                        .and_then(Value::as_u64)
                };
                if let (Some(tokens), Some(window)) = (figure("tokens"), figure("window")) {
                    signals.push(Signal::Context { tokens, window });
                }
                let quota = limits::claude_quota(payload);
                if !quota.is_empty() {
                    signals.push(Signal::Quota(quota));
                }
                return signals;
            }
            // Sent by the crew mod with each request of the main loop that
            // names another model or effort than the one before: the
            // session's, or the one the engine fell back to.
            "step" => {
                let effort = match payload.get("effort") {
                    Some(Value::String(level)) if !level.is_empty() => Some(level.clone()),
                    Some(Value::Number(n)) => Some(n.to_string()),
                    _ => None,
                };
                return match (str_field(payload, "model"), effort) {
                    (None, None) => Vec::new(),
                    (model, effort) => vec![Signal::Model { model, effort }],
                };
            }
            // Sent by the crew mod about a compaction the daemon asked for.
            "compacted" => return vec![Signal::Compacted],
            "compact-skipped" => {
                let why =
                    str_field(payload, "reason").unwrap_or_else(|| "no reason given".to_string());
                return vec![Signal::CompactSkipped(why)];
            }
            "subagent-start" => return vec![Signal::Subagents(1)],
            "subagent-stop" => return vec![Signal::Subagents(-1)],
            _ => {}
        }
        // Hooks also fire inside subagents; only the main session drives state.
        if payload.get("agent_id").is_some_and(|v| !v.is_null()) {
            return Vec::new();
        }
        // The session says what it runs on: its model when it starts, the
        // effort of the turn with every hook inside one.
        let running = Signal::Model {
            model: str_field(payload, "model").filter(|_| event == "session-start"),
            effort: payload
                .pointer("/effort/level")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        let mut signals = match &running {
            Signal::Model {
                model: None,
                effort: None,
            } => Vec::new(),
            _ => vec![running],
        };
        signals.extend(match event {
            "session-start" => vec![
                Signal::Session {
                    id: str_field(payload, "session_id"),
                    transcript: str_field(payload, "transcript_path"),
                },
                Signal::Ready,
            ],
            "user-prompt-submit" => vec![Signal::TurnStarted],
            "post-compact" => vec![Signal::Compacted],
            "pre-tool-use" => vec![Signal::Activity(describe_tool(payload))],
            "post-tool-use" => vec![Signal::Unblocked],
            "notification" => {
                let message = str_field(payload, "message").unwrap_or_default();
                let kind = str_field(payload, "notification_type").unwrap_or_default();
                let lower = message.to_lowercase();
                if kind == "permission_prompt"
                    || lower.contains("permission")
                    || lower.contains("needs your approval")
                {
                    vec![Signal::Blocked(if message.is_empty() {
                        "waiting for a permission decision".to_string()
                    } else {
                        message
                    })]
                } else {
                    Vec::new()
                }
            }
            "stop" => vec![
                Signal::Session {
                    id: str_field(payload, "session_id"),
                    transcript: str_field(payload, "transcript_path"),
                },
                Signal::TurnEnded,
            ],
            "session-end" => vec![Signal::Exiting],
            _ => Vec::new(),
        });
        signals
    }

    fn detect_dialog(&self, screen: &str) -> Option<Dialog> {
        let lower = screen.to_lowercase();
        if lower.contains("trust the files in this folder")
            || lower.contains("yes, i trust this folder")
            || (lower.contains("trust") && lower.contains("yes, proceed"))
            || lower.contains("is this a project you created or one you trust")
        {
            // Which option the cursor is on differs between versions: move to
            // the affirmative one first unless it is already selected.
            let on_yes = screen
                .lines()
                .find(|l| l.contains('❯'))
                .is_some_and(|l| l.to_lowercase().contains("yes"));
            return Some(Dialog {
                kind: "trust",
                description: "Claude Code is asking whether to trust this workspace".into(),
                accept_keys: if on_yes {
                    vec!["enter"]
                } else {
                    vec!["down", "enter"]
                },
            });
        }
        if limit_menu_open(screen) {
            // Esc is its cancel, as "Stop and wait for limit to reset" is:
            // whichever order the options come in (an account may list
            // "Switch to usage credits" first), nothing is bought.
            return Some(Dialog {
                kind: "notice",
                description: "Claude Code's usage-limit menu (crew waits the limit out itself)"
                    .into(),
                accept_keys: vec!["esc"],
            });
        }
        if lower.contains("bypass permissions mode") && lower.contains("yes, i accept") {
            return Some(Dialog {
                kind: "dialog",
                description: "Claude Code is asking to confirm Bypass Permissions mode".into(),
                accept_keys: Vec::new(),
            });
        }
        None
    }

    fn compact_command(&self, keep: Option<&str>) -> Option<String> {
        Some(match keep {
            Some(keep) => format!("/compact {keep}"),
            None => "/compact".to_string(),
        })
    }

    fn detect_limit(&self, screen: &str, now_ms: i64) -> Option<limits::LimitInfo> {
        limits::claude_screen(screen, now_ms)
    }

    fn usage(&self, transcript: &Path) -> Option<Usage> {
        read_usage(transcript)
    }
}

/// The options of the menu Claude Code opens by itself when a session runs
/// into a usage limit (`/rate-limit-options`, Claude Code 2.1.295), less
/// the plain "Stop" a usage-billed account gets.
const LIMIT_MENU_OPTIONS: &[&str] = &[
    "stop and wait for limit to reset",
    "wait here, then continue automatically",
    "don't continue automatically",
    "wait for limit to reset",
    "switch to usage",
    "add funds to continue with",
    "ask your admin for more usage",
    "upgrade your plan",
    "adjust monthly spend limit",
];

/// Whether Claude Code's usage-limit menu is open: its question, one of its
/// options and its "Esc to cancel" at the bottom of the screen. While it is,
/// the session takes nothing typed into it but as an answer. Text that only
/// quotes the menu has the input box and status lines below it, never the
/// menu's own last line.
fn limit_menu_open(screen: &str) -> bool {
    let lines: Vec<String> = screen
        .lines()
        .map(|l| l.trim().replace('\u{2019}', "'").to_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    let Some(cancel) = lines.iter().rposition(|l| l.contains("esc to cancel")) else {
        return false;
    };
    let Some(question) = lines[..cancel]
        .iter()
        .rposition(|l| l.starts_with("what do you want to do?"))
    else {
        return false;
    };
    lines.len() - cancel <= 2
        && cancel - question <= 14
        && lines[question + 1..cancel].iter().any(|l| {
            let option = l.trim_start_matches(|c: char| {
                c == '❯' || c == '.' || c == ' ' || c.is_ascii_digit()
            });
            LIMIT_MENU_OPTIONS.iter().any(|o| option.starts_with(o))
        })
}

fn str_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A short human-readable label for a tool call, e.g. `Edit ledger.rs`.
fn describe_tool(payload: &Value) -> String {
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let input = payload.get("tool_input").unwrap_or(&Value::Null);
    let file = |key: &str| {
        input.get(key).and_then(Value::as_str).map(|p| {
            Path::new(p)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string())
        })
    };
    let label = if let Some(rest) = tool.strip_prefix("mcp__crew__") {
        format!("crew: {rest}")
    } else if let Some(rest) = tool.strip_prefix("mcp__") {
        rest.replace("__", ": ")
    } else if tool == "Bash" {
        match input.get("command").and_then(Value::as_str) {
            Some(cmd) => format!("$ {}", one_line(cmd)),
            None => "Bash".into(),
        }
    } else if let Some(name) = file("file_path").or_else(|| file("notebook_path")) {
        format!("{tool} {name}")
    } else if let Some(pattern) = input.get("pattern").and_then(Value::as_str) {
        format!("{tool} {pattern}")
    } else {
        tool.to_string()
    };
    truncate_chars(&label, 60)
}

/// Sum token usage over the assistant messages of a session transcript.
/// Streaming writes several records per message id; the last one wins.
fn read_usage(transcript: &Path) -> Option<Usage> {
    let file = std::fs::File::open(transcript).ok()?;
    let mut per_message: HashMap<String, (u64, u64, u64, u64)> = HashMap::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("line-{}", per_message.len()));
        let n = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        per_message.insert(
            id,
            (
                n("input_tokens"),
                n("output_tokens"),
                n("cache_read_input_tokens"),
                n("cache_creation_input_tokens"),
            ),
        );
    }
    if per_message.is_empty() {
        return None;
    }
    let mut usage = Usage {
        turns: per_message.len() as u64,
        ..Usage::default()
    };
    for (input, output, cache_read, cache_creation) in per_message.values() {
        usage.input_tokens += input;
        usage.output_tokens += output;
        usage.cache_read_tokens += cache_read;
        usage.cache_creation_tokens += cache_creation;
    }
    Some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentSettings;
    use crew_core::{Agent, AgentStatus};

    fn agent(dir: &Path) -> Agent {
        Agent {
            id: "claude-3".into(),
            name: "claude-3".into(),
            role: "ledger".into(),
            task: None,
            status: AgentStatus::Spawning,
            activity: String::new(),
            runtime: RuntimeKind::Claude,
            model: Some("sonnet".into()),
            workdir: dir.to_string_lossy().into_owned(),
            branch: None,
            backend: "pty".into(),
            backend_ref: None,
            pid: None,
            session_id: Some("11111111-2222-3333-4444-555555555555".into()),
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
    fn a_token_from_a_file_goes_into_the_process_environment_and_nowhere_else() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let identity = dir.path().join("identity.json");
        let endpoint = Endpoint {
            secret_env: vec![(
                "CLAUDE_CODE_OAUTH_TOKEN".into(),
                crate::endpoints::Secret::new("oauth-token-value"),
            )],
            ..Endpoint::default()
        };
        let spec = ClaudeRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &AgentSettings::default(),
                crew_bin: Path::new("/opt/crew"),
                agent_dir: dir.path(),
                identity_path: &identity,
                briefing: "be helpful",
                resume: false,
                endpoint: &endpoint,
                long_context: false,
                private_home: None,
                memory_dir: None,
            })
            .unwrap();
        assert!(spec
            .env
            .contains(&("CLAUDE_CODE_OAUTH_TOKEN".into(), "oauth-token-value".into())));
        // Not on a command line, not in the files the daemon writes for the session.
        assert!(!spec.args.iter().any(|a| a.contains("oauth-token-value")));
        for file in ["settings.json", "mcp.json", "briefing.md"] {
            let text = std::fs::read_to_string(dir.path().join(file)).unwrap();
            assert!(!text.contains("oauth-token-value"), "{file}");
        }
    }

    #[test]
    fn a_sandboxed_launch_keeps_claudes_configuration_in_the_private_home() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let identity = dir.path().join("identity.json");
        let home = dir.path().join("rw/home");
        let launch = |private_home: Option<&Path>| {
            ClaudeRuntime
                .launch(&LaunchContext {
                    agent: &agent,
                    settings: &AgentSettings::default(),
                    crew_bin: Path::new("/opt/crew"),
                    agent_dir: dir.path(),
                    identity_path: &identity,
                    briefing: "be helpful",
                    resume: false,
                    endpoint: &Endpoint::default(),
                    long_context: false,
                    private_home,
                    memory_dir: None,
                })
                .unwrap()
        };
        let sandboxed = launch(Some(&home));
        let config_dir = sandboxed
            .env
            .iter()
            .find(|(k, _)| k == CONFIG_DIR_ENV)
            .map(|(_, v)| v.as_str());
        assert_eq!(config_dir, home.join("claude").to_str());
        // Not sandboxed, Claude Code keeps using the operator's own.
        assert!(!launch(None).env.iter().any(|(k, _)| k == CONFIG_DIR_ENV));
    }

    #[test]
    fn the_styles_cut_of_the_built_in_tools_is_the_tools_flag() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let identity = dir.path().join("identity.json");
        let launch = |tools: Option<Vec<ToolClass>>| {
            let settings = AgentSettings {
                tools,
                ..AgentSettings::default()
            };
            ClaudeRuntime
                .launch(&LaunchContext {
                    agent: &agent,
                    settings: &settings,
                    crew_bin: Path::new("/opt/crew"),
                    agent_dir: dir.path(),
                    identity_path: &identity,
                    briefing: "be helpful",
                    resume: false,
                    endpoint: &Endpoint::default(),
                    long_context: false,
                    private_home: None,
                    memory_dir: None,
                })
                .unwrap()
                .args
        };
        // Left alone, the built-ins are not mentioned.
        assert!(!launch(None).contains(&"--tools".to_string()));
        let args = launch(Some(vec![ToolClass::Read, ToolClass::Web]));
        let at = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(
            args[at + 1],
            "Skill,TodoWrite,ToolSearch,Read,Glob,Grep,ListMcpResourcesTool,ReadMcpResourceTool,\
WebFetch,WebSearch"
        );
        // The crew tools are MCP tools, outside the built-in set: still allowed.
        assert!(args.iter().any(|a| a.starts_with("mcp__crew")));
        // Nothing of the runtime's own: skills and the task list remain.
        assert_eq!(claude_tools(&[]), ["Skill", "TodoWrite", "ToolSearch"]);
        assert!(claude_tools(&ToolClass::ALL).contains(&"Agent"));
        assert!(!claude_tools(&[ToolClass::Shell]).contains(&"Edit"));
    }

    #[test]
    fn the_teams_memory_is_claude_codes_own_memory_directory() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let settings = AgentSettings::default();
        let identity = dir.path().join("identity.json");
        let memory = dir.path().join("memory");
        let ctx = LaunchContext {
            agent: &agent,
            settings: &settings,
            crew_bin: Path::new("/opt/crew"),
            agent_dir: dir.path(),
            identity_path: &identity,
            briefing: "be helpful",
            resume: false,
            endpoint: &Endpoint::default(),
            long_context: false,
            private_home: None,
            memory_dir: Some(&memory),
        };
        ClaudeRuntime.launch(&ctx).unwrap();
        let settings_file: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings_file["autoMemoryEnabled"], json!(true));
        assert_eq!(
            settings_file["autoMemoryDirectory"],
            json!(memory.to_string_lossy())
        );
        // A team that keeps no memory: the runtime's own is off too, so a
        // member neither reads nor writes the operator's.
        ClaudeRuntime
            .launch(&LaunchContext {
                memory_dir: None,
                ..ctx
            })
            .unwrap();
        let settings_file: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings_file["autoMemoryEnabled"], json!(false));
        assert!(settings_file.get("autoMemoryDirectory").is_none());
    }

    #[test]
    fn launch_builds_structured_arguments_and_private_files() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let settings = AgentSettings {
            permission_mode: "acceptEdits".into(),
            allowed_tools: vec!["Bash(cargo test:*)".into()],
            ..AgentSettings::default()
        };
        let identity = dir.path().join("identity.json");
        let ctx = LaunchContext {
            agent: &agent,
            settings: &settings,
            crew_bin: Path::new("/opt/my tools/crew"),
            agent_dir: dir.path(),
            identity_path: &identity,
            briefing: "be helpful",
            resume: false,
            endpoint: &Endpoint::default(),
            long_context: false,
            private_home: None,
            memory_dir: None,
        };
        let spec = ClaudeRuntime.launch(&ctx).unwrap();
        assert_eq!(spec.program, "claude");
        assert!(spec.prompt_as_arg);
        let joined = spec.args.join(" ");
        assert!(joined.contains("--session-id 11111111-2222-3333-4444-555555555555"));
        assert!(joined.contains("--strict-mcp-config"));
        assert!(joined.contains("--permission-mode acceptEdits"));
        assert!(!joined.contains("--dangerously-skip-permissions"));
        let bypass = AgentSettings {
            bypass_permissions: true,
            permission_mode: "acceptEdits".into(),
            ..AgentSettings::default()
        };
        let bypassed = ClaudeRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &bypass,
                crew_bin: Path::new("/opt/my tools/crew"),
                agent_dir: dir.path(),
                identity_path: &identity,
                briefing: "be helpful",
                resume: false,
                endpoint: &Endpoint::default(),
                // The 1M-token window is a tag on the model's name.
                long_context: true,
                private_home: None,
                memory_dir: None,
            })
            .unwrap()
            .args
            .join(" ");
        assert!(bypassed.contains("--dangerously-skip-permissions"));
        assert!(!bypassed.contains("--permission-mode"));
        assert!(
            bypassed.contains("--model sonnet[1m] ") || bypassed.ends_with("--model sonnet[1m]"),
            "{bypassed}"
        );
        assert!(joined.contains("--allowedTools mcp__crew,Bash(cargo test:*)"));
        // The options end before the prompt that follows them, which
        // `--allowedTools` would otherwise take for one more tool.
        assert_eq!(spec.args.last().map(String::as_str), Some("--"));
        assert!(joined.contains("--model sonnet") && !joined.contains("[1m]"));
        assert_eq!(spec.env[0].0, "CREW_IDENTITY");
        // The team may forbid the engine substituting another model for the
        // session's; by default it may (and crew tells the orchestrator).
        assert!(
            spec.env
                .iter()
                .any(|(k, v)| k == NO_FALLBACK_ENV && v == "1"),
            "{:?}",
            spec.env
        );
        let allowed = ClaudeRuntime
            .launch(&LaunchContext {
                settings: &AgentSettings {
                    model_fallback: true,
                    ..settings.clone()
                },
                ..ctx
            })
            .unwrap();
        assert!(
            !allowed.env.iter().any(|(k, _)| k == NO_FALLBACK_ENV),
            "{:?}",
            allowed.env
        );

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["permissions"]["deny"], json!(["AskUserQuestion"]));
        assert_eq!(settings["switchModelsOnFlag"], json!(true));
        let cmd = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        // The binary path contains a space and must be quoted for the hook shell.
        assert!(
            cmd.starts_with("'/opt/my tools/crew' hook stop --identity "),
            "{cmd}"
        );
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "*");
        let mcp: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("mcp.json")).unwrap())
                .unwrap();
        assert_eq!(mcp["mcpServers"]["crew"]["args"][0], "mcp");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("briefing.md")).unwrap(),
            "be helpful"
        );

        let resumed = ClaudeRuntime
            .launch(&LaunchContext {
                resume: true,
                ..ctx
            })
            .unwrap();
        assert_eq!(resumed.args[0], "--resume");
        // A member loads crew's mod for its tone even without socket delivery.
        let at = resumed
            .args
            .iter()
            .position(|a| a == "--plugin-dir")
            .expect("the mod, for the tone");
        let source =
            std::fs::read_to_string(Path::new(&resumed.args[at + 1]).join("hooks/register.ts"))
                .unwrap();
        assert!(
            source.contains("const DELIVER: boolean = false"),
            "{source}"
        );
        assert!(
            source.contains(
                "const TONE: string | null = \"# Tone and style\\nYou are a member of a team"
            ),
            "{source}"
        );
        // The orchestrator's output is the operator's to read: no mod without delivery.
        let orchestrator = Agent {
            orchestrator: true,
            ..agent.clone()
        };
        let plain = ClaudeRuntime
            .launch(&LaunchContext {
                agent: &orchestrator,
                resume: true,
                ..ctx
            })
            .unwrap();
        assert!(!plain.args.contains(&"--plugin-dir".to_string()));

        // With socket delivery the bridge mod is generated and loaded.
        let settings = AgentSettings {
            socket_delivery: true,
            ..AgentSettings::default()
        };
        let with_mod = ClaudeRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &settings,
                crew_bin: Path::new("/opt/my tools/crew"),
                agent_dir: dir.path(),
                identity_path: &identity,
                briefing: "be helpful",
                resume: false,
                endpoint: &Endpoint::default(),
                long_context: false,
                private_home: None,
                memory_dir: None,
            })
            .unwrap();
        let at = with_mod
            .args
            .iter()
            .position(|a| a == "--plugin-dir")
            .unwrap();
        let mod_dir = Path::new(&with_mod.args[at + 1]);
        let source = std::fs::read_to_string(mod_dir.join("hooks/register.ts")).unwrap();
        assert!(source.contains(r#"const CREW_BIN: string = "/opt/my tools/crew""#));
        assert!(source.contains("const DELIVER: boolean = true"));
        assert!(!source.contains("__IDENTITY__") && !source.contains("__TONE__"));
        assert!(mod_dir.join(".claude-plugin/plugin.json").exists());
    }

    #[test]
    fn the_endpoint_goes_into_the_private_settings_and_never_the_command_line() {
        use crate::endpoints::Secret;
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let identity = dir.path().join("identity.json");
        let launch = |endpoint: &Endpoint| {
            let spec = ClaudeRuntime
                .launch(&LaunchContext {
                    agent: &agent,
                    settings: &AgentSettings::default(),
                    crew_bin: Path::new("/opt/my tools/crew"),
                    agent_dir: dir.path(),
                    identity_path: &identity,
                    briefing: "",
                    resume: false,
                    endpoint,
                    long_context: false,
                    private_home: None,
                    memory_dir: None,
                })
                .unwrap();
            let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
            (spec, serde_json::from_str::<Value>(&text).unwrap(), text)
        };

        // Nothing configured: the session is left to the user's own setup.
        let (_, settings, _) = launch(&Endpoint::default());
        assert!(settings.get("env").is_none() && settings.get("apiKeyHelper").is_none());

        // An API key is fetched from the daemon by a helper; it is in no
        // file, argument or environment variable.
        let mut endpoint = Endpoint {
            base_url: Some("https://gateway.example".into()),
            api_key: Some(Secret::new("sk-ant-crew-secret-key")),
            model: Some("gateway-model".into()),
            ..Endpoint::default()
        };
        endpoint
            .env
            .insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".into(), "small".into());
        let (spec, settings, text) = launch(&endpoint);
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"],
            "https://gateway.example"
        );
        assert_eq!(settings["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "small");
        // Credentials inherited from the operator's shell are blanked.
        assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "");
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "");
        let helper = settings["apiKeyHelper"].as_str().unwrap();
        assert!(
            helper.starts_with("'/opt/my tools/crew' credential --identity "),
            "{helper}"
        );
        assert!(!text.contains("sk-ant-crew-secret-key"));
        assert!(!format!("{spec:?}").contains("sk-ant-crew-secret-key"));
        // The endpoint's model replaces the one the team asked for.
        let joined = spec.args.join(" ");
        assert!(
            joined.contains("--model gateway-model") && !joined.contains("sonnet"),
            "{joined}"
        );
        assert!(
            settings["hooks"]["Stop"].is_array(),
            "hooks are still there"
        );

        // A bearer token has no helper mechanism: it goes into the settings.
        let endpoint = Endpoint {
            auth_token: Some(Secret::new("bearer-crew-token")),
            ..Endpoint::default()
        };
        let (spec, settings, _) = launch(&endpoint);
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "bearer-crew-token");
        assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "");
        assert!(settings.get("apiKeyHelper").is_none());
        assert!(settings["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert!(!format!("{spec:?}").contains("bearer-crew-token"));
    }

    #[test]
    fn hooks_map_to_signals() {
        // Context fill and compactions, as the crew mod and the hooks report them.
        let rt = ClaudeRuntime;
        assert_eq!(
            rt.on_hook(
                "usage",
                &json!({ "rate_limits": [], "context": { "tokens": 150000, "window": 200000 } })
            ),
            vec![Signal::Context {
                tokens: 150000,
                window: 200000
            }]
        );
        assert!(rt
            .on_hook(
                "usage",
                &json!({ "rate_limits": [], "context": { "window": 200000 } })
            )
            .is_empty());
        assert_eq!(
            rt.on_hook("post-compact", &json!({ "compact_summary": "..." })),
            vec![Signal::Compacted]
        );
        assert!(
            rt.on_hook("post-compact", &json!({ "agent_id": "sub-1" }))
                .is_empty(),
            "a subagent's own compaction"
        );
        assert_eq!(rt.on_hook("compacted", &json!({})), vec![Signal::Compacted]);
        assert_eq!(
            rt.on_hook("compact-skipped", &json!({ "reason": "a hook said no" })),
            vec![Signal::CompactSkipped("a hook said no".into())]
        );
        assert_eq!(rt.compact_command(None).as_deref(), Some("/compact"));
        assert_eq!(
            rt.compact_command(Some("the plan")).as_deref(),
            Some("/compact the plan")
        );
        assert!(HOOKS
            .iter()
            .any(|(event, name)| *event == "PostCompact" && *name == "post-compact"));
        // What the session runs on: the model at the start, the effort with
        // the hooks of a turn; a subagent's own say nothing about the agent.
        let started = rt.on_hook(
            "session-start",
            &json!({ "session_id": "s", "model": "claude-sonnet-5-5" }),
        );
        assert_eq!(
            started[0],
            Signal::Model {
                model: Some("claude-sonnet-5-5".into()),
                effort: None
            }
        );
        let tool = rt.on_hook(
            "pre-tool-use",
            &json!({ "tool_name": "Bash", "model": "x", "effort": { "level": "high" } }),
        );
        assert_eq!(
            tool[0],
            Signal::Model {
                model: None,
                effort: Some("high".into())
            }
        );
        assert!(matches!(tool[1], Signal::Activity(_)));
        assert_eq!(
            rt.on_hook("stop", &json!({ "effort": { "level": "max" } }))[0],
            Signal::Model {
                model: None,
                effort: Some("max".into())
            }
        );
        assert!(rt
            .on_hook(
                "pre-tool-use",
                &json!({ "agent_id": "sub", "effort": { "level": "low" } })
            )
            .is_empty());
        // The crew mod reports the model and effort of each request of the
        // main loop that differ from the one before: the session's, or the
        // one the engine fell back to.
        assert_eq!(
            rt.on_hook(
                "step",
                &json!({ "model": "claude-opus-5-5[1m]", "effort": "xhigh" })
            ),
            vec![Signal::Model {
                model: Some("claude-opus-5-5[1m]".into()),
                effort: Some("xhigh".into())
            }]
        );
        assert_eq!(
            rt.on_hook("step", &json!({ "model": "claude-opus-5-5", "effort": 70 })),
            vec![Signal::Model {
                model: Some("claude-opus-5-5".into()),
                effort: Some("70".into())
            }]
        );
        assert!(rt.on_hook("step", &json!({})).is_empty());
        let rt = ClaudeRuntime;
        let start = rt.on_hook(
            "session-start",
            &json!({"session_id": "s1", "transcript_path": "/t.jsonl"}),
        );
        assert_eq!(
            start,
            vec![
                Signal::Session {
                    id: Some("s1".into()),
                    transcript: Some("/t.jsonl".into())
                },
                Signal::Ready
            ]
        );
        assert_eq!(
            rt.on_hook("user-prompt-submit", &json!({})),
            vec![Signal::TurnStarted]
        );
        assert_eq!(
            rt.on_hook(
                "pre-tool-use",
                &json!({"tool_name": "Edit", "tool_input": {"file_path": "/w/src/ledger.rs"}})
            ),
            vec![Signal::Activity("Edit ledger.rs".into())]
        );
        assert_eq!(
            rt.on_hook(
                "pre-tool-use",
                &json!({"tool_name": "mcp__crew__agent_send", "tool_input": {}})
            ),
            vec![Signal::Activity("crew: agent_send".into())]
        );
        assert_eq!(
            rt.on_hook(
                "notification",
                &json!({"message": "Claude needs your permission to use Bash"})
            ),
            vec![Signal::Blocked(
                "Claude needs your permission to use Bash".into()
            )]
        );
        assert!(rt
            .on_hook(
                "notification",
                &json!({"message": "Claude is waiting for your input"})
            )
            .is_empty());
        assert!(matches!(
            rt.on_hook("stop", &json!({}))[..],
            [_, Signal::TurnEnded]
        ));
        // Subagent events never drive the agent's state, only its subagent count.
        assert!(rt.on_hook("stop", &json!({"agent_id": "sub-1"})).is_empty());
        assert_eq!(
            rt.on_hook("subagent-start", &json!({"agent_id": "sub-1"})),
            vec![Signal::Subagents(1)]
        );
        assert_eq!(
            rt.on_hook("subagent-stop", &json!({"agent_id": "sub-1"})),
            vec![Signal::Subagents(-1)]
        );
    }

    #[test]
    fn detects_the_trust_dialog() {
        let rt = ClaudeRuntime;
        let d = rt
            .detect_dialog("Do you trust the files in this folder?\n ❯ 1. Yes, proceed")
            .unwrap();
        assert_eq!(d.kind, "trust");
        assert_eq!(d.accept_keys, vec!["enter"]);
        // Newer builds preselect "No, exit": move down before confirming.
        let d = rt
            .detect_dialog(
                "Quick safety check: Is this a project you created or one you trust?\n ❯ No, exit\n   Yes, I trust this folder",
            )
            .unwrap();
        assert_eq!(d.accept_keys, vec!["down", "enter"]);
        assert!(rt.detect_dialog("> Try \"fix the bug\"").is_none());
    }

    /// The menu as a session showed it after a resume ran into the limit
    /// again (Claude Code 2.1.295).
    const LIMIT_MENU: &str = "\
  Now also features-p5-linux. Need same metrics for the money/percent precision
  and lower/open-bound phases; format source via jj.
────────────────────────────────────────────────────────────────────────────────
  What do you want to do?

  ❯ 1. Stop and wait for limit to reset
    2. Wait here, then continue automatically at 3:00 PM
    3. Switch to usage credits

  Enter to confirm · Esc to cancel
";

    #[test]
    fn the_usage_limit_menu_is_closed_with_esc_whatever_its_options() {
        let rt = ClaudeRuntime;
        let d = rt.detect_dialog(LIMIT_MENU).unwrap();
        assert_eq!((d.kind, d.accept_keys), ("notice", vec!["esc"]));
        // An account may be offered credits or an upgrade first: Esc still
        // picks none of them.
        let upsell_first = LIMIT_MENU
            .replace(
                "❯ 1. Stop and wait for limit to reset",
                "❯ 1. Upgrade your plan",
            )
            .replace(
                "3. Switch to usage credits",
                "3. Stop and wait for limit to reset",
            );
        assert_eq!(
            rt.detect_dialog(&upsell_first).unwrap().accept_keys,
            ["esc"]
        );
        let usage_billed = LIMIT_MENU
            .replace("Stop and wait for limit to reset", "Stop")
            .replace(
                "Switch to usage credits",
                "Add funds to continue with usage",
            );
        assert!(rt.detect_dialog(&usage_billed).is_some());

        // Quoted, as in a reply or a diff, with the input box below it; or
        // another menu that asks the same: not this one.
        let quoted = format!(
            "{LIMIT_MENU}\n● The menu above is Claude Code's.\n\n────────\n❯ \n────────\n  ⏵⏵ bypass permissions on\n"
        );
        assert!(rt.detect_dialog(&quoted).is_none());
        let other = "  What do you want to do?\n  ❯ 1. Resume the session\n    2. Start over\n  Enter to confirm · Esc to cancel\n";
        assert!(rt.detect_dialog(other).is_none());
    }

    #[test]
    fn usage_is_summed_per_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let lines = [
            json!({"type":"user","message":{"content":"hi"}}),
            json!({"type":"assistant","message":{"id":"a","usage":{"input_tokens":10,"output_tokens":1}}}),
            json!({"type":"assistant","message":{"id":"a","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100}}}),
            json!({"type":"assistant","message":{"id":"b","usage":{"input_tokens":3,"output_tokens":7,"cache_creation_input_tokens":20}}}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, text).unwrap();
        let usage = read_usage(&path).unwrap();
        assert_eq!(
            usage,
            Usage {
                input_tokens: 13,
                output_tokens: 12,
                cache_read_tokens: 100,
                cache_creation_tokens: 20,
                turns: 2
            }
        );
        assert!(read_usage(&dir.path().join("missing")).is_none());
    }
}
