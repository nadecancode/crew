//! Harness for the integration tests.
//!
//! Each test gets its own workspace, daemon, socket directory and session
//! registry (via `XDG_RUNTIME_DIR` / `XDG_STATE_HOME`), and drives everything
//! through the real `crew` binary: `crew run` starts a detached daemon, the
//! agents are `crew scripted-agent` processes on real PTYs, and assertions
//! read `crew status --json`, `crew messages --json` and `crew logs --json`.
//! Nothing here needs Claude Code or Codex.

#![allow(dead_code)]

#[path = "../../../crew-core/src/test_support.rs"]
pub mod test_support;

#[path = "../../src/git_test_support.rs"]
pub mod git_test_support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crew_core::config::{
    AgentDefaults, AgentSpec, BackendChoice, OrchestratorConfig, SandboxConfig, SandboxMode,
    TaskSpec, TeamConfig, VcsChoice, WorkspaceConfig,
};
use crew_core::rpc::Snapshot;
use crew_core::{Agent, Event, Message, RuntimeKind, Task};

pub const CREW: &str = env!("CARGO_BIN_EXE_crew");
pub const TIMEOUT: Duration = Duration::from_secs(40);

/// The argv of a scripted agent; `steps` are `--on-task=…`-style flags.
pub fn scripted(steps: &[&str]) -> Vec<String> {
    let mut argv = vec!["{crew}".to_string(), "scripted-agent".to_string()];
    argv.extend(steps.iter().map(|s| s.to_string()));
    argv
}

pub fn agent(id: &str, role: &str, steps: &[&str]) -> AgentSpec {
    AgentSpec {
        id: id.to_string(),
        role: role.to_string(),
        command: scripted(steps),
        ..AgentSpec::default()
    }
}

pub fn task(id: &str, agent: &str, depends_on: &[&str]) -> TaskSpec {
    TaskSpec {
        id: id.to_string(),
        title: Some(format!("task {id}")),
        description: format!("do {id}"),
        agent: Some(agent.to_string()),
        depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
        ..TaskSpec::default()
    }
}

/// The version control every test team is built with: `CREW_TEST_VCS`
/// (`jj` or `git`), else `auto` (jj when it is on PATH, git otherwise).
pub fn test_vcs() -> VcsChoice {
    match std::env::var("CREW_TEST_VCS").as_deref() {
        Ok("jj") => VcsChoice::Jj,
        Ok("git") => VcsChoice::Git,
        Ok("") | Err(_) => VcsChoice::Auto,
        Ok(other) => panic!("CREW_TEST_VCS is jj or git, not {other}"),
    }
}

pub struct Session {
    tmp: tempfile::TempDir,
    pub workspace: PathBuf,
}

impl Session {
    pub fn new() -> Self {
        // A short path: Unix socket paths are limited to ~108 bytes.
        let tmp = tempfile::Builder::new()
            .prefix("crew-t")
            .tempdir_in("/tmp")
            .expect("creating a temp dir");
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(tmp.path().join("home")).expect("creating isolated operator home");
        Self { tmp, workspace }
    }

    pub fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// A team of scripted agents with no orchestrator agent.
    pub fn team(&self, project: &str, agents: Vec<AgentSpec>, tasks: Vec<TaskSpec>) -> TeamConfig {
        TeamConfig {
            project: project.to_string(),
            objective: "integration test".to_string(),
            style: None,
            instructions: String::new(),
            workspace: WorkspaceConfig {
                path: self.workspace.to_string_lossy().into_owned(),
                init: true,
                // The scripted agents run unsandboxed whatever the default becomes;
                // tests/sandbox.rs is where sandboxes are tested.
                sandbox: SandboxConfig {
                    mode: SandboxMode::Off,
                    ..SandboxConfig::default()
                },
                vcs: test_vcs(),
                ..WorkspaceConfig::default()
            },
            backend: BackendChoice::Pty,
            herdr: Default::default(),
            toolboxes: Default::default(),
            defaults: AgentDefaults {
                runtime: RuntimeKind::Shell,
                ..AgentDefaults::default()
            },
            orchestrator: OrchestratorConfig {
                agent: false,
                ..OrchestratorConfig::default()
            },
            agents,
            tasks,
            limits: Default::default(),
        }
    }

    pub fn command(&self) -> Command {
        self.command_of(Path::new(CREW))
    }

    /// `command()`, running `program`, a crew binary of the test's own
    /// (`own_crew`).
    pub fn command_of(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        git_test_support::clear_repository_environment(&mut cmd);
        cmd.current_dir(self.root())
            .env("HOME", self.root().join("home"))
            .env("XDG_RUNTIME_DIR", self.root().join("run"))
            .env("XDG_STATE_HOME", self.root().join("state"))
            // Nor on this machine's crew config, model catalog or Codex
            // model list.
            .env("XDG_CONFIG_HOME", self.root().join("config"))
            .env("CREW_GLOBAL_CONFIG", self.root().join("etc/config.toml"))
            .env("CODEX_HOME", self.root().join("codex-home"))
            .env("CLAUDE_CONFIG_DIR", self.root().join("claude-home"))
            .env("PI_CODING_AGENT_DIR", self.root().join("pi-home"))
            // The tests must not depend on (or disturb) the terminal
            // multiplexer or agent session they happen to be run from.
            .env_remove("HERDR_ENV")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("CREW_IDENTITY")
            .env_remove("CREW_SOCKET")
            .stdin(Stdio::null());
        cmd
    }

    pub fn crew(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("running crew")
    }

    /// Run crew and return stdout, panicking with stderr on failure.
    pub fn ok(&self, args: &[&str]) -> String {
        let output = self.crew(args);
        assert!(
            output.status.success(),
            "crew {args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn ws(&self) -> String {
        self.workspace.to_string_lossy().into_owned()
    }

    /// Write the team file and `crew run` it.
    pub fn run(&self, team: &TeamConfig) -> String {
        self.run_with(team, &[])
    }

    /// `run`, with the team's daemon started from `program` (`own_crew`).
    pub fn run_from(&self, program: &Path, team: &TeamConfig) -> String {
        let file = self.root().join("team.yaml");
        std::fs::write(&file, team.to_yaml().expect("encoding the team"))
            .expect("writing team.yaml");
        let output = self
            .command_of(program)
            .args(["run", "team.yaml", "--no-attach"])
            .output()
            .expect("running crew");
        assert!(
            output.status.success(),
            "crew run failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// A crew binary of the test's own, `<root>/<name>`, to start a daemon
    /// from when the test replaces or removes the file it was started from.
    pub fn own_crew(&self, name: &str) -> PathBuf {
        let path = self.root().join(name);
        std::fs::copy(CREW, &path).expect("copying crew");
        path
    }

    /// What is in `<state>/bin`, where the daemon keeps its copy of crew.
    pub fn crew_copies(&self) -> Vec<PathBuf> {
        let dir = self.workspace.join(".crew/bin");
        let mut copies: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|entries| entries.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default();
        copies.sort();
        copies
    }

    /// Shell words for the only crew binary this session's sandboxes mount:
    /// the daemon's copy in `<state>/bin`, looked up when they run.
    pub fn sandboxed_crew(&self) -> String {
        format!(
            "\"$(ls -d '{}'/crew-* | head -n 1)\"",
            self.workspace.join(".crew/bin").display()
        )
    }

    /// The daemon's copy of crew, when it keeps exactly one.
    pub fn crew_copy(&self) -> PathBuf {
        let copies = self.crew_copies();
        assert_eq!(copies.len(), 1, "{copies:?}");
        copies[0].clone()
    }

    pub fn run_with(&self, team: &TeamConfig, extra: &[&str]) -> String {
        let file = self.root().join("team.yaml");
        std::fs::write(&file, team.to_yaml().expect("encoding the team"))
            .expect("writing team.yaml");
        let mut args = vec!["run", "team.yaml", "--no-attach"];
        args.extend_from_slice(extra);
        self.ok(&args)
    }

    pub fn snapshot(&self) -> Snapshot {
        let out = self.ok(&["status", "--json", "-C", &self.ws()]);
        serde_json::from_str(&out).expect("status --json")
    }

    pub fn messages(&self) -> Vec<Message> {
        let out = self.ok(&["messages", "--json", "-n", "500", "-C", &self.ws()]);
        serde_json::from_str(&out).expect("messages --json")
    }

    pub fn events(&self) -> Vec<Event> {
        let out = self.ok(&["logs", "--json", "-n", "2000", "-C", &self.ws()]);
        out.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("an event per line"))
            .collect()
    }

    pub fn has_event(&self, event_type: &str) -> bool {
        self.events().iter().any(|e| e.event_type == event_type)
    }

    /// Poll until `done` holds, or fail with the current state.
    pub fn wait<F: Fn(&Snapshot) -> bool>(&self, what: &str, done: F) -> Snapshot {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let snapshot = self.snapshot();
            if done(&snapshot) {
                return snapshot;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what}\n--- status\n{}\n--- events\n{}",
                    self.ok(&["status", "-C", &self.ws()]),
                    self.ok(&["logs", "-n", "60", "-C", &self.ws()]),
                );
            }
            std::thread::sleep(Duration::from_millis(60));
        }
    }

    /// Poll an arbitrary condition (for things outside the snapshot).
    pub fn wait_for<F: Fn() -> bool>(&self, what: &str, done: F) {
        let deadline = Instant::now() + TIMEOUT;
        while !done() {
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what}\n--- events\n{}",
                    self.ok(&["logs", "-n", "60", "-C", &self.ws()]),
                );
            }
            std::thread::sleep(Duration::from_millis(60));
        }
    }

    pub fn wait_all_done(&self) -> Snapshot {
        self.wait("every task to be done", |s| {
            s.session.completed_at.is_some()
        })
    }

    pub fn identity_path(&self, agent: &str) -> PathBuf {
        self.workspace
            .join(".crew")
            .join("agents")
            .join(agent)
            .join("identity.json")
    }

    /// Run an agent-side command (`crew ctl …`) as `agent`.
    pub fn ctl(&self, agent: &str, args: &[&str]) -> Output {
        let mut full = vec!["ctl"];
        full.extend_from_slice(args);
        self.command()
            .env("CREW_IDENTITY", self.identity_path(agent))
            .args(full)
            .output()
            .expect("running crew ctl")
    }

    pub fn agent_cmd(&self, args: &[&str]) -> String {
        let ws = self.ws();
        let mut full = vec!["agent"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["-C", &ws]);
        self.ok(&full)
    }

    pub fn send(&self, to: &str, body: &str) {
        self.ok(&["send", to, body, "-C", &self.ws()]);
    }

    pub fn stop(&self) {
        let out = self.crew(&["stop", "-C", &self.ws()]);
        // Nothing to stop is fine (a test may have stopped it already);
        // anything else is worth seeing when a later step fails.
        let err = stderr(&out);
        if !out.status.success() && !err.contains("no crew daemon") {
            eprintln!("crew stop: {}", err.trim());
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
        // File snapshots and now MCP catalog snapshots are immutable. Remove
        // this session's store through its directory descriptor after stop,
        // so TempDir can clean up without following any links left by agents.
        if let Ok(root) = std::fs::File::open(self.root()) {
            if let Ok(parent) =
                crew_core::paths::list_dir_beneath(&root, Path::new("ws/.crew/toolbox"))
            {
                let _ = crew::agents::toolbox::store::remove(&parent, Path::new("store"));
            }
        }
        // A dashboard server the test started stops with it.
        if self.root().join("run/crew/web.sock").exists() {
            let _ = self.crew(&["web", "--stop"]);
        }
    }
}

/// Whether process `pid` runs the file now at `path` (the same inode),
/// whatever path it was started by.
pub fn runs(pid: u32, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(running), Ok(file)) = (
        std::fs::metadata(format!("/proc/{pid}/exe")),
        std::fs::metadata(path),
    ) else {
        return false;
    };
    (running.dev(), running.ino()) == (file.dev(), file.ino())
}

/// Variable `name` of process `pid`'s environment.
pub fn env_of(pid: u32, name: &str) -> Option<String> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let prefix = format!("{name}=");
    environ
        .split(|b| *b == 0)
        .find_map(|entry| entry.strip_prefix(prefix.as_bytes()))
        .map(|value| String::from_utf8_lossy(value).into_owned())
}

pub fn find_agent<'a>(snapshot: &'a Snapshot, id: &str) -> &'a Agent {
    snapshot
        .agents
        .iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| {
            panic!(
                "no agent {id} in {:?}",
                snapshot.agents.iter().map(|a| &a.id).collect::<Vec<_>>()
            )
        })
}

pub fn find_task<'a>(snapshot: &'a Snapshot, id: &str) -> &'a Task {
    snapshot
        .tasks
        .iter()
        .find(|t| t.id == id)
        .unwrap_or_else(|| panic!("no task {id}"))
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
