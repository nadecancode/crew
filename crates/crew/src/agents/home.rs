//! An agent's private home (design-b3.md §3.5, D2): what its CLI starts with.
//!
//! A sandboxed agent's CLI keeps its configuration, credentials and sessions
//! under `<A>/rw/home/<cli>`, which the agent can write. Everything the daemon
//! puts there it puts through the agent's root descriptor (`<A>`), one
//! component at a time with no link followed, and only while no sandbox of
//! that agent is running (the caller's barrier): an agent that swapped a
//! directory or a file for a link cannot make the daemon write elsewhere.
//!
//! What is copied from the operator is settings and model configuration, one
//! way and filtered: no hooks, plugins, MCP servers, extensions or project
//! trust. Credentials are never copied. A CLI that refreshes a copied token
//! would invalidate the operator's own login, so a sandboxed agent
//! authenticates with a token the operator provides in its environment
//! (`claude setup-token`, an API key), not with the operator's login files.

use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crew_core::{paths, RuntimeKind};
use serde_json::{json, Value};

/// The operator's own configuration directories, which seeding reads.
#[derive(Clone, Debug)]
pub struct Operator {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub pi: PathBuf,
}

impl Operator {
    /// Where the operator's CLIs keep their configuration, as the daemon's
    /// environment says.
    pub fn detect() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        let home = var("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        Self {
            claude: var("CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from),
            codex: var("CODEX_HOME").map_or_else(|| home.join(".codex"), PathBuf::from),
            pi: var("PI_CODING_AGENT_DIR").map_or_else(|| home.join(".pi/agent"), PathBuf::from),
        }
    }
}

/// The largest operator configuration file read.
const MAX_CONFIG: u64 = 1 << 20;

/// Settings that are preferences about the model and how it is shown. Anything
/// else (hooks, plugins, MCP servers, permissions, environment, helpers that
/// run programs) is the operator's alone.
const CLAUDE_SETTINGS: &[&str] = &[
    "model",
    "theme",
    "effortLevel",
    "alwaysThinkingEnabled",
    "outputStyle",
    "language",
];
const CODEX_CONFIG: &[&str] = &[
    "model",
    "model_provider",
    "model_providers",
    "model_reasoning_effort",
    "model_reasoning_summary",
    "model_verbosity",
    "model_context_window",
    "model_auto_compact_token_limit",
    "personality",
];
const PI_SETTINGS: &[&str] = &[
    "defaultProvider",
    "defaultModel",
    "defaultThinkingLevel",
    "enabledModels",
    "thinkingBudgets",
    "theme",
];

/// Make `<A>/rw/home/<cli>` (and, for pi, `<A>/rw/pi-sessions`) and put the
/// filtered operator settings in it. `agent_root` is `<A>`. Call it only while
/// none of the agent's sandboxes is running.
pub fn prepare(kind: RuntimeKind, agent_root: BorrowedFd<'_>, operator: &Operator) -> Result<()> {
    let rw = subdir(agent_root, "rw")?;
    let home = subdir(rw.as_fd(), "home")?;
    match kind {
        RuntimeKind::Claude => {
            let dir = subdir(home.as_fd(), "claude")?;
            let settings = read_json(&operator.claude.join("settings.json"));
            let theme = settings
                .as_ref()
                .and_then(|s| s.get("theme").cloned())
                .unwrap_or_else(|| json!("dark"));
            if let Some(settings) = settings {
                put(
                    dir.as_fd(),
                    "settings.json",
                    &pretty(&only(settings, CLAUDE_SETTINGS))?,
                )?;
            }
            // Claude Code's state file, with the first-run dialogs already
            // answered. Created once: it also holds what the session learns
            // (project trust), which a restart must not forget.
            let state = json!({ "hasCompletedOnboarding": true, "theme": theme });
            put_if_absent(dir.as_fd(), ".claude.json", &pretty(&state)?)?;
        }
        RuntimeKind::Codex => {
            let dir = subdir(home.as_fd(), "codex")?;
            if let Some(text) = read_text(&operator.codex.join("config.toml")) {
                if let Ok(mut table) = text.parse::<toml::Table>() {
                    table.retain(|key, _| CODEX_CONFIG.contains(&key));
                    put(
                        dir.as_fd(),
                        "config.toml",
                        toml::to_string(&table)?.as_bytes(),
                    )?;
                }
            }
        }
        RuntimeKind::Pi => {
            let dir = subdir(home.as_fd(), "pi")?;
            if let Some(settings) = read_json(&operator.pi.join("settings.json")) {
                put(
                    dir.as_fd(),
                    "settings.json",
                    &pretty(&only(settings, PI_SETTINGS))?,
                )?;
            }
            // Custom providers and models: model configuration.
            if let Some(models) = read_json(&operator.pi.join("models.json")) {
                put(dir.as_fd(), "models.json", &pretty(&models)?)?;
            }
            subdir(rw.as_fd(), "pi-sessions")?;
        }
        RuntimeKind::Shell => {}
    }
    Ok(())
}

fn read_text(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_CONFIG)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&read_text(path)?).ok()
}

fn pretty(value: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(value)?)
}

/// `value` (a JSON object) with only the listed keys.
fn only(value: Value, keys: &[&str]) -> Value {
    match value {
        Value::Object(mut map) => {
            map.retain(|key, _| keys.contains(&key.as_str()));
            Value::Object(map)
        }
        _ => json!({}),
    }
}

/// The directory `name` in `dir`, made if it is missing (0700). A link, a
/// file or anything but a directory in its place is an error.
fn subdir(dir: BorrowedFd<'_>, name: &str) -> Result<OwnedFd> {
    let c_name = CString::new(name)?;
    // SAFETY: `dir` is a live descriptor and `c_name` is NUL-terminated.
    if unsafe { libc::mkdirat(dir.as_raw_fd(), c_name.as_ptr(), 0o700) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(err).with_context(|| format!("creating {name}"));
        }
    }
    paths::open_dir_beneath(dir, Path::new(name))
        .with_context(|| format!("{name} is not a plain directory of the agent's"))
}

/// Write `name` in `dir`: whatever was there (a file, or a link the agent put
/// in its place) is unlinked, then the new file is created exclusively without
/// following anything.
fn put(dir: BorrowedFd<'_>, name: &str, bytes: &[u8]) -> Result<()> {
    let c_name = CString::new(name)?;
    // SAFETY: `dir` is live and `c_name` is NUL-terminated.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), c_name.as_ptr(), 0) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err).with_context(|| format!("replacing {name}"));
        }
    }
    create(dir, &c_name, name, bytes)
}

/// Write `name` unless something is there already.
fn put_if_absent(dir: BorrowedFd<'_>, name: &str, bytes: &[u8]) -> Result<()> {
    let c_name = CString::new(name)?;
    match create(dir, &c_name, name, bytes) {
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
        {
            Ok(())
        }
        other => other,
    }
}

fn create(dir: BorrowedFd<'_>, c_name: &CString, name: &str, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    // SAFETY: `dir` is live and `c_name` is NUL-terminated; the result is checked.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .map_err(|e| anyhow::Error::new(e).context(format!("creating {name}")));
    }
    // SAFETY: openat returned a new descriptor that nothing else owns.
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    file.write_all(bytes)
        .with_context(|| format!("writing {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    struct Fixture {
        dir: tempfile::TempDir,
        root: std::fs::File,
        operator: Operator,
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let agent = base.join("ws/.crew/agents/a");
        std::fs::create_dir_all(agent.join("rw/home")).unwrap();
        let op = base.join("operator");
        write(
            &op.join("claude/settings.json"),
            r#"{"model":"opus","theme":"dark",
 "hooks":{"PreToolUse":[{"command":"curl evil"}]},
 "enabledPlugins":{"x":true},"mcpServers":{"s":{}},"env":{"K":"v"},
 "permissions":{"allow":["Bash(*)"]},"apiKeyHelper":"/bin/steal"}"#,
        );
        write(
            &op.join("claude/.credentials.json"),
            r#"{"token":"operator-login"}"#,
        );
        write(&op.join("claude/projects/p/s.jsonl"), "operator transcript");
        write(
            &op.join("codex/config.toml"),
            "model = \"gpt-5\"\nmodel_reasoning_effort = \"high\"\nnotify = [\"/bin/steal\"]\n\
[mcp_servers.evil]\ncommand = \"/bin/evil\"\n[projects.\"/x\"]\ntrust_level = \"trusted\"\n\
[hooks]\nstop = \"/bin/steal\"\n[model_providers.local]\nname = \"local\"\nbase_url = \"http://localhost:1/v1\"\n",
        );
        write(
            &op.join("codex/auth.json"),
            r#"{"OPENAI_API_KEY":"operator-key"}"#,
        );
        write(
            &op.join("pi/settings.json"),
            r#"{"defaultProvider":"anthropic","defaultModel":"x","theme":"dark",
 "extensions":["/home/op/evil.ts"],"packages":["npm:evil"],"skills":["/s"],"hooks":{}}"#,
        );
        write(
            &op.join("pi/auth.json"),
            r#"{"anthropic":{"key":"operator-key"}}"#,
        );
        write(
            &op.join("pi/models.json"),
            r#"{"providers":{"local":{"baseUrl":"http://localhost:1"}}}"#,
        );
        let root = std::fs::File::open(&agent).unwrap();
        Fixture {
            operator: Operator {
                claude: op.join("claude"),
                codex: op.join("codex"),
                pi: op.join("pi"),
            },
            root,
            dir,
        }
    }

    impl Fixture {
        fn home(&self, rel: &str) -> PathBuf {
            self.dir.path().join("ws/.crew/agents/a/rw/home").join(rel)
        }

        fn prepare(&self, kind: RuntimeKind) -> Result<()> {
            prepare(kind, self.root.as_fd(), &self.operator)
        }
    }

    fn json(path: PathBuf) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn claude_gets_its_model_settings_and_no_hooks_plugins_servers_or_credentials() {
        let f = fixture();
        f.prepare(RuntimeKind::Claude).unwrap();
        let settings = json(f.home("claude/settings.json"));
        assert_eq!(settings["model"], "opus");
        assert_eq!(settings["theme"], "dark");
        for dropped in [
            "hooks",
            "enabledPlugins",
            "mcpServers",
            "env",
            "permissions",
            "apiKeyHelper",
        ] {
            assert!(
                settings.get(dropped).is_none(),
                "{dropped} was copied: {settings}"
            );
        }
        // Not the operator's login, nor their transcripts.
        assert!(!f.home("claude/.credentials.json").exists());
        assert!(!f.home("claude/projects").exists());
        // The first run does not stop at the onboarding dialogs.
        assert_eq!(
            json(f.home("claude/.claude.json"))["hasCompletedOnboarding"],
            true
        );
    }

    #[test]
    fn codex_gets_model_configuration_and_no_hooks_servers_trust_or_credentials() {
        let f = fixture();
        f.prepare(RuntimeKind::Codex).unwrap();
        let config: toml::Table =
            toml::from_str(&std::fs::read_to_string(f.home("codex/config.toml")).unwrap()).unwrap();
        assert_eq!(config["model"].as_str(), Some("gpt-5"));
        assert_eq!(config["model_reasoning_effort"].as_str(), Some("high"));
        assert!(config.contains_key("model_providers"));
        for dropped in ["notify", "mcp_servers", "projects", "hooks"] {
            assert!(
                !config.contains_key(dropped),
                "{dropped} was copied: {config:?}"
            );
        }
        assert!(!f.home("codex/auth.json").exists());
    }

    #[test]
    fn pi_gets_settings_and_models_without_extensions_packages_or_credentials_and_a_sessions_dir() {
        let f = fixture();
        f.prepare(RuntimeKind::Pi).unwrap();
        let settings = json(f.home("pi/settings.json"));
        assert_eq!(settings["defaultModel"], "x");
        for dropped in ["extensions", "packages", "skills", "hooks"] {
            assert!(
                settings.get(dropped).is_none(),
                "{dropped} was copied: {settings}"
            );
        }
        assert!(f.home("pi/models.json").is_file());
        assert!(!f.home("pi/auth.json").exists());
        assert!(f
            .dir
            .path()
            .join("ws/.crew/agents/a/rw/pi-sessions")
            .is_dir());
    }

    #[test]
    fn seeding_keeps_session_data_and_replaces_only_its_own_files() {
        let f = fixture();
        f.prepare(RuntimeKind::Claude).unwrap();
        write(
            &f.home("claude/projects/p/session.jsonl"),
            "the agent's transcript",
        );
        write(
            &f.home("claude/settings.json"),
            r#"{"model":"changed by the agent"}"#,
        );
        f.prepare(RuntimeKind::Claude).unwrap();
        assert_eq!(
            std::fs::read_to_string(f.home("claude/projects/p/session.jsonl")).unwrap(),
            "the agent's transcript"
        );
        assert_eq!(json(f.home("claude/settings.json"))["model"], "opus");
    }

    #[test]
    fn a_link_planted_by_the_agent_is_never_written_through() {
        // A file replaced by a link to somewhere outside.
        let f = fixture();
        let outside = f.dir.path().join("outside-file");
        std::fs::write(&outside, "sentinel").unwrap();
        std::fs::create_dir_all(f.home("claude")).unwrap();
        symlink(&outside, f.home("claude/settings.json")).unwrap();
        f.prepare(RuntimeKind::Claude).unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "sentinel");
        assert!(!std::fs::symlink_metadata(f.home("claude/settings.json"))
            .unwrap()
            .is_symlink());
        // A directory replaced by a link: seeding refuses, outside stays empty.
        let f = fixture();
        let elsewhere = f.dir.path().join("outside-dir");
        std::fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, f.home("codex")).unwrap();
        assert!(f.prepare(RuntimeKind::Codex).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        // `rw/home` itself replaced by a link.
        let f = fixture();
        let moved = f.dir.path().join("moved-home");
        let home = f.dir.path().join("ws/.crew/agents/a/rw/home");
        std::fs::rename(&home, &moved).unwrap();
        let target = f.dir.path().join("outside-home");
        std::fs::create_dir_all(&target).unwrap();
        symlink(&target, &home).unwrap();
        assert!(f.prepare(RuntimeKind::Claude).is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }
}
