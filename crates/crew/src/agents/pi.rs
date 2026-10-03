//! pi adapter: the `pi` coding agent (pi.dev, `@earendil-works/pi-coding-agent`,
//! 1.0 and on).
//!
//! pi has no hooks, but an extension API that covers what crew needs: crew
//! writes a small extension next to the agent's identity file and loads it
//! with `--extension`. It registers crew's MCP server with the session (so
//! the crew tools are declared to the model like built-in ones), pushes
//! team messages in over the daemon socket (`crew bridge`), reports the
//! session's life to the daemon (`crew hook`), and compacts the context
//! when asked. The standing instructions go in with
//! `--append-system-prompt`; the session is kept under the agent's own
//! directory under an id crew chooses (`--session-dir`, `--session-id`), so
//! a restart opens exactly that session; the first prompt is pi's trailing
//! argument. pi runs tools without permission prompts, so there is no
//! permission mode to set; project-local pi configuration in the workspace
//! is trusted when the team trusts it (`auto_trust`) and ignored otherwise,
//! so no trust dialog ever waits for a hand.

use std::path::Path;

use anyhow::Result;
use crew_core::paths::{write_private_file, IDENTITY_ENV};
use crew_core::util::{one_line, truncate_chars};
use crew_core::RuntimeKind;
use serde_json::{json, Value};

use super::{AgentRuntime, LaunchContext, LaunchSpec, Signal};
use crate::limits;

const EXTENSION: &str = include_str!("../../assets/crew-pi.ts");
const EXTENSION_FILE: &str = "crew-pi.ts";
/// Where the agent's sessions are kept: a directory per agent, so `--continue`
/// resumes this agent's session and nobody else's.
const SESSIONS_DIR: &str = "pi-sessions";

pub struct PiRuntime;

fn str_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A short label for a tool call, e.g. `$ cargo test` or `edit main.rs`.
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
    let label = if tool == "bash" {
        match input.get("command").and_then(Value::as_str) {
            Some(cmd) => format!("$ {}", one_line(cmd)),
            None => "bash".into(),
        }
    } else if let Some(name) = file("path").or_else(|| file("file_path")) {
        format!("{tool} {name}")
    } else if let Some(pattern) = input.get("pattern").and_then(Value::as_str) {
        format!("{tool} {pattern}")
    } else if crate::mcp::tools::definitions()
        .iter()
        .any(|t| t.name == tool)
    {
        format!("crew: {tool}")
    } else {
        tool.to_string()
    };
    truncate_chars(&label, 60)
}

/// Names the pi program to run, for a pi that is not the first `pi` on
/// `PATH` (another agent's `pi` is there) or not on it at all.
pub const PI_ENV: &str = "CREW_PI";

/// Where pi lands when installed for one user, under `$HOME`: `bun add -g`,
/// `npm install -g --prefix ~/.local`, an npm prefix of `~/.npm-global`.
const USER_BIN_DIRS: &[&str] = &[".bun/bin", ".local/bin", ".npm-global/bin"];

/// How pi is installed, for the messages that ask for it.
pub const INSTALL_HINT: &str = "npm install -g @earendil-works/pi-coding-agent";

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Every `pi` on `path` (a `PATH` value), then in the user install
/// directories under `home`, each once.
fn candidates(
    path: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = path
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        dirs.extend(USER_BIN_DIRS.iter().map(|dir| Path::new(&home).join(dir)));
    }
    let mut found: Vec<std::path::PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for candidate in dirs.into_iter().map(|dir| dir.join("pi")) {
        if executable(&candidate)
            && seen.insert(std::fs::canonicalize(&candidate).unwrap_or(candidate.clone()))
        {
            found.push(candidate);
        }
    }
    found
}

/// Whether the program at `path` is pi. Other agents install a `pi` of their
/// own (GSD's `gsd-pi` does) with a command line of their own, which would
/// take crew's arguments for prompts and subcommands; they say who they are
/// in their help. A program that does not answer in time is given the
/// benefit of the doubt: its launch then fails or succeeds on its own.
pub fn check_binary(path: &Path) -> Result<(), String> {
    let Some(help) = output_within(path, &["--help"], std::time::Duration::from_secs(10)) else {
        return Ok(());
    };
    let first = help
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    if first.starts_with("GSD") || help.contains("Get Shit Done") {
        return Err(format!(
            "{} is GSD's `pi`, not pi (https://pi.dev)",
            path.display()
        ));
    }
    Ok(())
}

/// What `program args` prints, if it finishes within `limit`. Its output is
/// read as it comes, so a long listing cannot stall it on a full pipe.
fn output_within(program: &Path, args: &[&str], limit: std::time::Duration) -> Option<String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    // The probe leads its own process group, so a timeout takes its children
    // (which may hold the pipe open) down with it.
    let mut child = Command::new(program)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    // One deadline covers the exit and the draining of stdout.
    let deadline = std::time::Instant::now() + limit;
    let kill_group = |child: &mut std::process::Child| {
        // SAFETY: plain syscall; a negative pid addresses the probe's group.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
        let _ = child.kill();
        let _ = child.wait();
    };
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                kill_group(&mut child);
                return None;
            }
        }
    }
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    let text = rx.recv_timeout(left).ok();
    if text.is_none() {
        // Something still holds the pipe; it is not wanted any more.
        kill_group(&mut child);
    }
    text
}

/// The models pi can reach with the providers it has credentials for, as
/// `provider/model` (what `--model` takes), from `pi --list-models`. The
/// answer is kept for a minute: pi takes a moment to start.
pub fn list_models() -> Result<Vec<String>, String> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    type Listed = Option<(Instant, Result<Vec<String>, String>)>;
    static LISTED: Mutex<Listed> = Mutex::new(None);
    if let Some((at, listed)) = LISTED.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if at.elapsed() < Duration::from_secs(60) {
            return listed.clone();
        }
    }
    let listed = program().and_then(|program| {
        let listing = output_within(&program, &["--list-models"], Duration::from_secs(15))
            .ok_or("pi did not list its models in time")?;
        Ok(parse_model_list(&listing))
    });
    *LISTED.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), listed.clone()));
    listed
}

/// `provider  model  context …` rows, under a heading row.
fn parse_model_list(listing: &str) -> Vec<String> {
    let mut models = Vec::new();
    for line in listing.lines() {
        let mut cells = line.split_whitespace();
        let (Some(provider), Some(model)) = (cells.next(), cells.next()) else {
            continue;
        };
        if provider.eq_ignore_ascii_case("provider") && model.eq_ignore_ascii_case("model") {
            continue;
        }
        let name = format!("{provider}/{model}");
        let usable = name.len() <= 120 && !name.chars().any(char::is_control);
        if usable && !models.contains(&name) {
            models.push(name);
        }
    }
    models
}

/// `check_binary`, once per installed program.
fn check_cached(path: &Path) -> Result<(), String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CHECKED: OnceLock<Mutex<HashMap<std::path::PathBuf, Result<(), String>>>> =
        OnceLock::new();
    let key = std::fs::canonicalize(path).unwrap_or(path.to_path_buf());
    let checked = CHECKED.get_or_init(Default::default);
    if let Some(known) = checked.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return known.clone();
    }
    let result = check_binary(path);
    checked
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, result.clone());
    result
}

/// The pi program crew runs: `$CREW_PI` when set, else the first `pi` on
/// `PATH` or in a user install directory that is pi and not another agent's
/// program of the same name — or why there is none.
pub fn program() -> Result<std::path::PathBuf, String> {
    pick(
        std::env::var_os(PI_ENV)
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from),
        candidates(std::env::var_os("PATH"), std::env::var_os("HOME")),
        check_cached,
    )
}

/// The program named, or else the first candidate `check` takes for pi.
fn pick(
    named: Option<std::path::PathBuf>,
    candidates: Vec<std::path::PathBuf>,
    check: impl Fn(&Path) -> Result<(), String>,
) -> Result<std::path::PathBuf, String> {
    if let Some(named) = named {
        if !executable(&named) {
            return Err(format!("{PI_ENV}={} is not a program", named.display()));
        }
        return check(&named)
            .map(|()| named)
            .map_err(|why| format!("{PI_ENV}: {why}"));
    }
    let mut refused = Vec::new();
    for candidate in candidates {
        match check(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(why) => refused.push(why),
        }
    }
    Err(match refused.first() {
        None => format!("pi is not installed: `{INSTALL_HINT}` (or bun add -g) installs it"),
        Some(why) => format!(
            "{why}. Install pi with `{INSTALL_HINT}` (or bun add -g), or set {PI_ENV} to its path"
        ),
    })
}

/// Whether pi can be started here.
pub fn check_installed() -> Result<(), String> {
    program().map(|_| ())
}

/// Write the crew extension for an agent and return its path.
fn write_extension(
    agent_dir: &Path,
    crew_bin: &Path,
    identity: &Path,
) -> Result<std::path::PathBuf> {
    let path = agent_dir.join(EXTENSION_FILE);
    // The two paths are embedded as JSON string literals, which are valid
    // TypeScript string literals.
    let source = EXTENSION
        .replace(
            "__CREW_BIN__",
            &json!(crew_bin.to_string_lossy()).to_string(),
        )
        .replace(
            "__IDENTITY__",
            &json!(identity.to_string_lossy()).to_string(),
        );
    write_private_file(&path, source.as_bytes())?;
    Ok(path)
}

impl AgentRuntime for PiRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Pi
    }

    fn launch(&self, ctx: &LaunchContext<'_>) -> Result<LaunchSpec> {
        let briefing_path = ctx.agent_dir.join("briefing.md");
        write_private_file(&briefing_path, ctx.briefing.as_bytes())?;
        let extension = write_extension(ctx.agent_dir, ctx.crew_bin, ctx.identity_path)?;
        let sessions = ctx.agent_dir.join(SESSIONS_DIR);
        crew_core::paths::ensure_private_dir(&sessions)?;

        let mut args: Vec<String> = vec![
            "--extension".into(),
            extension.to_string_lossy().into_owned(),
            // A file path: pi reads it.
            "--append-system-prompt".into(),
            briefing_path.to_string_lossy().into_owned(),
            "--session-dir".into(),
            sessions.to_string_lossy().into_owned(),
        ];
        // Created the first time, opened on every restart.
        if let Some(id) = &ctx.agent.session_id {
            args.push("--session-id".into());
            args.push(id.clone());
        } else if ctx.resume {
            args.push("--continue".into());
        }
        // Project-local pi files (`.pi/`) in the workspace: trusted or ignored,
        // never asked about.
        args.push(
            if ctx.settings.auto_trust {
                "--approve"
            } else {
                "--no-approve"
            }
            .into(),
        );
        // A model as pi names it, with its provider in front when the name
        // alone would be ambiguous: `anthropic/claude-opus-4-6` (pi and GSD
        // both take that form in one argument).
        if let Some(model) = ctx.endpoint.model.as_ref().or(ctx.agent.model.as_ref()) {
            args.push("--model".into());
            args.push(model.clone());
        }
        args.extend(ctx.settings.extra_args.iter().cloned());
        // Whatever follows is the first prompt, even one starting with `-`.
        args.push("--".into());

        let mut env = vec![(
            IDENTITY_ENV.to_string(),
            ctx.identity_path.to_string_lossy().into_owned(),
        )];
        env.extend(ctx.endpoint.env.iter().map(|(k, v)| (k.clone(), v.clone())));

        Ok(LaunchSpec {
            agent_id: ctx.agent.id.clone(),
            kind: RuntimeKind::Pi,
            program: "pi".into(),
            args,
            env,
            cwd: ctx.agent.workdir.clone().into(),
            prompt: None,
            prompt_as_arg: true,
            sidecar: None,
        })
    }

    fn on_hook(&self, event: &str, payload: &Value) -> Vec<Signal> {
        let running = || Signal::Model {
            model: str_field(payload, "model"),
            effort: str_field(payload, "effort"),
        };
        match event {
            // The extension speaks before pi's input is on screen: readiness
            // is judged from the terminal settling, as for Codex.
            "session-start" => vec![
                Signal::Session {
                    id: str_field(payload, "session_id"),
                    transcript: str_field(payload, "transcript_path"),
                },
                running(),
            ],
            "model" => match running() {
                Signal::Model {
                    model: None,
                    effort: None,
                } => Vec::new(),
                signal => vec![signal],
            },
            "turn-start" => vec![Signal::TurnStarted],
            "turn-end" => {
                let mut signals = vec![Signal::TurnEnded];
                if payload.get("error").is_some() {
                    signals.push(
                        match limits::pi_turn_error(payload, crew_core::util::now_ms()) {
                            limits::TurnFailure::Limited(info) => Signal::Limited(info),
                            limits::TurnFailure::Other(what) => Signal::TurnFailed(what),
                        },
                    );
                }
                signals
            }
            "tool-start" => vec![Signal::Activity(describe_tool(payload))],
            "usage" => {
                let figure = |key: &str| {
                    payload
                        .pointer(&format!("/context/{key}"))
                        .and_then(Value::as_u64)
                };
                match (figure("tokens"), figure("window")) {
                    (Some(tokens), Some(window)) => vec![Signal::Context { tokens, window }],
                    _ => Vec::new(),
                }
            }
            "compacted" => vec![Signal::Compacted],
            "compact-skipped" => {
                let why =
                    str_field(payload, "reason").unwrap_or_else(|| "no reason given".to_string());
                vec![Signal::CompactSkipped(why)]
            }
            "exiting" => vec![Signal::Exiting],
            _ => Vec::new(),
        }
    }

    // Typed at the prompt when the extension's channel is not there.
    fn compact_command(&self, _keep: Option<&str>) -> Option<String> {
        Some("/compact".to_string())
    }

    fn detect_limit(&self, screen: &str, now_ms: i64) -> Option<limits::LimitInfo> {
        limits::pi_screen(screen, now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentSettings;
    use crate::endpoints::Endpoint;
    use crew_core::{Agent, AgentStatus};

    fn agent(dir: &Path) -> Agent {
        Agent {
            id: "pi-1".into(),
            name: "pi-1".into(),
            role: "builder".into(),
            task: None,
            status: AgentStatus::Spawning,
            activity: String::new(),
            runtime: RuntimeKind::Pi,
            model: Some("anthropic/claude-opus-4-6".into()),
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
        }
    }

    #[test]
    fn launch_loads_the_extension_and_keeps_the_session_in_the_agents_own_directory() {
        let dir = tempfile::tempdir().unwrap();
        let agent = agent(dir.path());
        let settings = AgentSettings {
            extra_args: vec!["--thinking".into(), "high".into()],
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
        };
        let spec = PiRuntime.launch(&ctx).unwrap();
        assert_eq!(spec.program, "pi");
        assert!(spec.prompt_as_arg && spec.sidecar.is_none());
        let extension = dir.path().join("crew-pi.ts");
        let briefing = dir.path().join("briefing.md");
        let sessions = dir.path().join("pi-sessions");
        assert_eq!(
            spec.args,
            [
                "--extension",
                extension.to_str().unwrap(),
                "--append-system-prompt",
                briefing.to_str().unwrap(),
                "--session-dir",
                sessions.to_str().unwrap(),
                "--session-id",
                "11111111-2222-3333-4444-555555555555",
                "--no-approve",
                "--model",
                "anthropic/claude-opus-4-6",
                "--thinking",
                "high",
                "--",
            ]
        );
        assert!(sessions.is_dir());
        assert_eq!(std::fs::read_to_string(&briefing).unwrap(), "be helpful");
        // The extension has the paths it needs and nothing from pi itself.
        let source = std::fs::read_to_string(&extension).unwrap();
        assert!(
            source.contains(r#"const CREW_BIN: string = "/opt/my tools/crew""#),
            "{source}"
        );
        assert!(source.contains(&format!(
            "const IDENTITY: string = {}",
            json!(identity.to_str().unwrap())
        )));
        assert!(!source.contains("__CREW_BIN__") && !source.contains("pi-coding-agent"));
        assert!(source.contains("registerMcpServer") && source.contains("sendUserMessage"));
        assert_eq!(
            spec.env[0],
            (
                "CREW_IDENTITY".to_string(),
                identity.to_string_lossy().into_owned()
            )
        );

        // A restart opens the same session; without an id of crew's, pi's
        // newest for the agent; a team that trusts the workspace says so.
        let mut plain = agent.clone();
        plain.model = Some("claude-sonnet-4-6".into());
        plain.session_id = None;
        let resumed = PiRuntime
            .launch(&LaunchContext {
                agent: &plain,
                resume: true,
                settings: &AgentSettings {
                    auto_trust: true,
                    ..settings.clone()
                },
                ..ctx
            })
            .unwrap();
        let joined = resumed.args.join(" ");
        assert!(
            joined.contains("--session-dir") && joined.contains(" --continue --approve "),
            "{joined}"
        );
        assert!(!joined.contains("--session-id"), "{joined}");
        assert!(
            joined.ends_with("--model claude-sonnet-4-6 --thinking high --"),
            "{joined}"
        );
    }

    #[test]
    fn another_agents_pi_is_told_apart_from_pi() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, help: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{help}'\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let pi = script(
            "pi",
            "pi - AI coding assistant with read, bash, edit, write tools",
        );
        assert_eq!(check_binary(&pi), Ok(()));
        let gsd = script("gsd", "GSD v2.70.1 — Get Shit Done");
        let refused = check_binary(&gsd).unwrap_err();
        assert!(
            refused.contains("is GSD's `pi`, not pi") && refused.contains("https://pi.dev"),
            "{refused}"
        );
        // A program that cannot be asked is left to its launch.
        assert_eq!(check_binary(&dir.path().join("missing")), Ok(()));
    }

    #[test]
    fn a_probe_whose_child_holds_stdout_still_returns_within_the_limit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pi");
        // The program exits at once; its background child keeps stdout open.
        std::fs::write(&path, "#!/bin/sh\necho hi\nsleep 30 &\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let _ = output_within(&path, &["--help"], std::time::Duration::from_secs(1));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn hooks_map_to_signals() {
        let rt = PiRuntime;
        assert_eq!(
            rt.on_hook(
                "session-start",
                &json!({ "session_id": "s-1", "transcript_path": "/s/s-1.jsonl", "model": "anthropic/claude-opus-4-6" })
            ),
            vec![
                Signal::Session {
                    id: Some("s-1".into()),
                    transcript: Some("/s/s-1.jsonl".into())
                },
                Signal::Model {
                    model: Some("anthropic/claude-opus-4-6".into()),
                    effort: None
                },
            ]
        );
        assert_eq!(
            rt.on_hook(
                "model",
                &json!({ "model": "anthropic/claude-sonnet-4-6", "effort": "high" })
            ),
            vec![Signal::Model {
                model: Some("anthropic/claude-sonnet-4-6".into()),
                effort: Some("high".into())
            }]
        );
        assert!(rt.on_hook("model", &json!({})).is_empty());
        assert_eq!(
            rt.on_hook("turn-start", &json!({})),
            vec![Signal::TurnStarted]
        );
        assert_eq!(rt.on_hook("turn-end", &json!({})), vec![Signal::TurnEnded]);
        // A turn that ended in the account's usage limit parks the agent; any
        // other error just ends the turn.
        let limited = rt.on_hook(
            "turn-end",
            &json!({ "error": "error", "error_details": "429 rate_limit_error: You've hit your usage limit · resets 3:19 PM" }),
        );
        assert_eq!(limited[0], Signal::TurnEnded);
        assert!(
            matches!(&limited[1], Signal::Limited(info) if info.kind == limits::LimitKind::Usage && info.resets_at.is_some()),
            "{limited:?}"
        );
        let failed = rt.on_hook(
            "turn-end",
            &json!({ "error": "error", "error_details": "500 overloaded" }),
        );
        assert!(
            matches!(&failed[1], Signal::TurnFailed(what) if what.contains("overloaded")),
            "{failed:?}"
        );
        assert_eq!(
            rt.on_hook(
                "tool-start",
                &json!({ "tool_name": "bash", "tool_input": { "command": "cargo test" } })
            ),
            vec![Signal::Activity("$ cargo test".into())]
        );
        assert_eq!(
            rt.on_hook(
                "tool-start",
                &json!({ "tool_name": "edit", "tool_input": { "path": "/w/src/main.rs" } })
            ),
            vec![Signal::Activity("edit main.rs".into())]
        );
        assert_eq!(
            rt.on_hook(
                "tool-start",
                &json!({ "tool_name": "agent_send", "tool_input": { "to": "a" } })
            ),
            vec![Signal::Activity("crew: agent_send".into())]
        );
        assert_eq!(
            rt.on_hook(
                "usage",
                &json!({ "context": { "tokens": 40000, "window": 200000 } })
            ),
            vec![Signal::Context {
                tokens: 40000,
                window: 200000
            }]
        );
        assert_eq!(rt.on_hook("compacted", &json!({})), vec![Signal::Compacted]);
        assert_eq!(rt.on_hook("exiting", &json!({})), vec![Signal::Exiting]);
        assert_eq!(
            rt.compact_command(Some("the plan")).as_deref(),
            Some("/compact")
        );
    }
}

#[cfg(test)]
mod model_list_tests {
    use super::parse_model_list;

    #[test]
    fn pis_model_listing_is_read_as_provider_slash_model() {
        let listing = "provider   model              context  max-out  thinking  images\n\
anthropic  claude-opus-4-6    200K     32K      yes       yes\n\
openai     gpt-6-astra        1M       128K     yes       yes\n\
\n\
anthropic  claude-opus-4-6    200K     32K      yes       yes\n";
        assert_eq!(
            parse_model_list(listing),
            vec!["anthropic/claude-opus-4-6", "openai/gpt-6-astra"]
        );
        assert!(parse_model_list("").is_empty());
        assert!(parse_model_list("provider  model  context\n").is_empty());
    }
}

#[cfg(test)]
mod program_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn pi_is_found_past_another_agents_pi_on_path_and_in_user_install_dirs() {
        let root = tempfile::tempdir().unwrap();
        let script = |dir: &str, help: &str| {
            let dir = root.path().join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("pi");
            std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{help}'\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let gsd = script("profile/bin", "GSD v2.70.1 — Get Shit Done");
        let real = script(
            "home/.bun/bin",
            "pi - AI coding assistant with read, bash, edit, write tools",
        );
        let path = Some(
            std::env::join_paths([root.path().join("profile/bin"), root.path().join("empty")])
                .unwrap(),
        );
        let home = Some(root.path().join("home").into_os_string());

        // GSD's `pi` comes first on PATH; pi itself is in ~/.bun/bin.
        let found = candidates(path.clone(), home.clone());
        assert_eq!(found, vec![gsd.clone(), real.clone()]);
        assert_eq!(pick(None, found, check_binary), Ok(real.clone()));
        // Only GSD's: the reason says what to do.
        let why = pick(None, candidates(path.clone(), None), check_binary).unwrap_err();
        assert!(
            why.contains("is GSD's `pi`") && why.contains(INSTALL_HINT) && why.contains(PI_ENV),
            "{why}"
        );
        assert!(pick(None, Vec::new(), check_binary)
            .unwrap_err()
            .starts_with("pi is not installed"));
        // CREW_PI names it outright, and is held to being pi.
        assert_eq!(pick(Some(real.clone()), Vec::new(), check_binary), Ok(real));
        assert!(pick(Some(gsd), Vec::new(), check_binary)
            .unwrap_err()
            .starts_with("CREW_PI: "));
        let missing = pick(Some(root.path().join("nope")), Vec::new(), check_binary).unwrap_err();
        assert!(missing.contains("is not a program"), "{missing}");
    }
}
