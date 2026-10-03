//! A sandboxed agent's CLI keeps its files in a private home under the
//! agent's writable tree, seeded from the operator's settings, with the
//! operator's own home and credentials out of reach. Real bubblewrap and the
//! fake CLIs (`crew fake-cli`); skipped with a printed reason where the host
//! cannot run sandboxes, and a failure under `CREW_TEST_SANDBOX=require`.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::*;
use crew_core::config::{AgentSpec, SandboxConfig, SandboxMode};
use crew_core::RuntimeKind;
use serde_json::Value;

fn sandboxes_available() -> bool {
    let path = std::env::var_os("PATH");
    match crew::sandbox::probe::probe(Path::new(CREW), path.as_ref()) {
        Ok(_) => true,
        Err(e) => {
            if std::env::var("CREW_TEST_SANDBOX").as_deref() == Ok("require") {
                panic!("CREW_TEST_SANDBOX=require but sandboxes are unavailable: {e}");
            }
            eprintln!("skipped: sandboxes are unavailable: {e}");
            false
        }
    }
}

/// Wrapper scripts for `claude`, `codex` and `pi`, installed the way the real
/// CLIs are: the program in `real/`, a symlink to it in `bin/`, which is what
/// goes on PATH. The tree sits beside the crew binary: that is mounted into
/// the sandbox, and `/tmp` is not.
fn fake_clis() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir_in(Path::new(CREW).parent().unwrap()).unwrap();
    let base = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(base.join("real")).unwrap();
    std::fs::create_dir_all(base.join("bin")).unwrap();
    for name in ["claude", "codex", "pi"] {
        let path = base.join("real").join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\nexec '{CREW}' fake-cli {name} \"$@\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&path, base.join("bin").join(name)).unwrap();
    }
    let bin = base.join("bin");
    (dir, bin)
}

fn wait_for_file(s: &Session, path: &Path) {
    s.wait_for(&format!("{}", path.display()), || path.is_file());
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn runtime_agent(id: &str, runtime: RuntimeKind) -> AgentSpec {
    AgentSpec {
        id: id.to_string(),
        role: "implementation".to_string(),
        runtime: Some(runtime),
        ..AgentSpec::default()
    }
}

#[test]
fn sandboxed_clis_keep_their_files_in_private_homes_seeded_without_the_operators_secrets() {
    if !sandboxes_available() {
        return;
    }
    let s = Session::new();
    let (_fakes, bin) = fake_clis();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());

    // The operator's own homes, as Session::command points the daemon at them.
    let operator_claude = s.root().join("claude-home");
    let operator_codex = s.root().join("codex-home");
    let write = |p: PathBuf, text: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write(
        operator_claude.join("settings.json"),
        r#"{"model":"opus","hooks":{"Stop":[{"command":"touch /tmp/evil"}]}}"#,
    );
    write(
        operator_claude.join(".credentials.json"),
        r#"{"token":"operator-login"}"#,
    );
    write(
        operator_codex.join("config.toml"),
        "model = \"gpt-5\"\nnotify = [\"/bin/evil\"]\n",
    );
    write(
        operator_codex.join("auth.json"),
        r#"{"OPENAI_API_KEY":"operator-key"}"#,
    );

    // What only the daemon's side of the agent directory holds.
    let planted = s.workspace.join(".crew/agents/h/planted.jsonl");
    std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
    std::fs::write(
        &planted,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"SECRET-FROM-THE-DAEMONS-AREA"}]}}"#,
    )
    .unwrap();

    let mut team = s.team(
        "private-homes",
        vec![
            runtime_agent("c", RuntimeKind::Claude),
            AgentSpec {
                extra_args: vec![],
                ..runtime_agent("x", RuntimeKind::Codex)
            },
            runtime_agent("p", RuntimeKind::Pi),
            // A hostile one: it tells the daemon its transcript is a file in the daemon's
            // own part of the agent directory.
            AgentSpec {
                extra_args: vec![
                    "--fake-report-transcript".into(),
                    s.workspace
                        .join(".crew/agents/h/planted.jsonl")
                        .to_string_lossy()
                        .into_owned(),
                ],
                ..runtime_agent("h", RuntimeKind::Claude)
            },
        ],
        vec![],
    );
    team.workspace.sandbox = SandboxConfig {
        mode: SandboxMode::Required,
        ..SandboxConfig::default()
    };
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let started = s.crew(&["run", "team.yaml", "--no-attach"]);
    if !started.status.success() {
        let error = stderr(&started);
        // The staged rollout (crew-core/src/config.rs) still refuses `required`
        // until the daemon wiring is complete; then this test runs for real.
        if error.contains("required is not available yet") {
            assert!(
                std::env::var("CREW_TEST_SANDBOX").as_deref() != Ok("require"),
                "CREW_TEST_SANDBOX=require but the config still gates required: {error}"
            );
            eprintln!("skipped: the config still gates sandbox mode required");
            return;
        }
        panic!("crew run failed: {error}");
    }

    let agents = s.workspace.join(".crew/agents");
    let home = |id: &str| agents.join(id).join("rw/home");

    // Claude Code: its configuration directory is the private one.
    let seen = home("c").join("claude/fake-claude.json");
    wait_for_file(&s, &seen);
    let seen = json(&seen);
    assert_eq!(
        seen["env"]["CLAUDE_CONFIG_DIR"],
        home("c").join("claude").to_str().unwrap()
    );
    assert_eq!(seen["env"]["HOME"], home("c").to_str().unwrap());
    let settings = json(&home("c").join("claude/settings.json"));
    assert_eq!(settings["model"], "opus");
    assert!(settings.get("hooks").is_none(), "{settings}");
    assert!(home("c").join("claude/projects/-fake").is_dir());
    // The daemon reads the transcript from there, through the agent's root.
    s.wait_for("the transcript through the daemon", || {
        s.agent_cmd(&["transcript", "c"])
            .contains("hello from the fake")
    });
    assert!(!home("c").join("claude/.credentials.json").exists());

    // Codex: the app-server and the TUI share the private home.
    wait_for_file(&s, &home("x").join("codex/fake-codex.json"));
    let server = json(&home("x").join("codex/fake-codex-server.json"));
    assert_eq!(
        server["env"]["CODEX_HOME"],
        home("x").join("codex").to_str().unwrap()
    );
    let config = std::fs::read_to_string(home("x").join("codex/config.toml")).unwrap();
    assert!(
        config.contains("gpt-5") && !config.contains("notify"),
        "{config}"
    );
    assert!(!home("x").join("codex/auth.json").exists());

    // pi: its directory and sessions are under the agent's writable tree.
    let pi = home("p").join("pi/fake-pi.json");
    wait_for_file(&s, &pi);
    assert_eq!(
        json(&pi)["env"]["PI_CODING_AGENT_DIR"],
        home("p").join("pi").to_str().unwrap()
    );
    assert!(agents.join("p/rw/pi-sessions").is_dir());

    // A hostile agent cannot make the daemon open a file outside its writable
    // tree by naming it: the daemon falls back to the real transcript.
    wait_for_file(&s, &home("h").join("claude/fake-claude.json"));
    s.wait_for("the hostile agent's transcript", || {
        s.agent_cmd(&["transcript", "h"])
            .contains("hello from the fake")
    });
    assert!(
        !s.agent_cmd(&["transcript", "h"])
            .contains("SECRET-FROM-THE-DAEMONS-AREA"),
        "the daemon opened a path the agent named"
    );

    // The operator's homes were not used: no fake CLI wrote there.
    for operator in [&operator_claude, &operator_codex] {
        assert!(
            !operator.join("projects").exists() && !operator.join("fake-claude.json").exists(),
            "{} was written",
            operator.display()
        );
    }
    assert!(!operator_codex.join("sessions").exists());
    s.stop();
}
