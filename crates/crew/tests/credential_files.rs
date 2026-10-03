//! Tokens the operator keeps in files reach their own agent's environment and
//! no other agent, and appear nowhere crew reports. The agents are the fake
//! CLIs (`crew fake-cli`); the team is not sandboxed, which does not matter
//! here: the sandbox passes exactly the names in the launch environment, and
//! tests/private_home.rs covers the sandboxed launch.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::*;
use crew_core::config::AgentSpec;
use crew_core::RuntimeKind;
use serde_json::Value;

fn secret_file(dir: &Path, name: &str, token: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("{token}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn agent_of(id: &str, runtime: RuntimeKind) -> AgentSpec {
    AgentSpec {
        id: id.to_string(),
        role: "implementation".to_string(),
        runtime: Some(runtime),
        ..AgentSpec::default()
    }
}

#[test]
fn each_token_reaches_its_own_agent_and_no_other_and_is_reported_nowhere() {
    let s = Session::new();
    // Wrapper scripts for the CLIs, first on PATH.
    let bin = tempfile::tempdir().unwrap();
    for name in ["claude", "codex", "pi"] {
        let path = bin.path().join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\nexec '{CREW}' fake-cli {name} \"$@\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin.path().to_path_buf()];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());

    let secrets = tempfile::tempdir().unwrap();
    let claude = secret_file(secrets.path(), "claude", "tok-claude-1111");
    let codex = secret_file(secrets.path(), "codex", "tok-codex-2222");
    let pi = secret_file(secrets.path(), "pi", "tok-pi-3333");
    let pi_home = s.root().join("pi-home");
    let config = format!(
        "[agents.c1.env_files]\nCLAUDE_CODE_OAUTH_TOKEN = \"{}\"\n\
[agents.x1.env_files]\nOPENAI_API_KEY = \"{}\"\n\
[agents.p1.env_files]\nANTHROPIC_API_KEY = \"{}\"\n\
[agents.p1.env]\nPI_CODING_AGENT_DIR = \"{}\"\n",
        claude.display(),
        codex.display(),
        pi.display(),
        pi_home.display()
    );
    let global = s.root().join("etc/config.toml");
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    std::fs::write(&global, config).unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();

    let team = s.team(
        "tokens",
        vec![
            agent_of("c1", RuntimeKind::Claude),
            agent_of("c2", RuntimeKind::Claude),
            agent_of("x1", RuntimeKind::Codex),
            agent_of("p1", RuntimeKind::Pi),
        ],
        vec![],
    );
    s.run(&team);

    let claude_home = s.root().join("claude-home");
    let codex_home = s.root().join("codex-home");
    // c1 and c2 are both Claude agents on the same home; each records itself
    // by its --name argument.
    s.wait_for("the fakes to start", || {
        claude_home.join("fake-claude-c1.json").is_file()
            && claude_home.join("fake-claude-c2.json").is_file()
            && codex_home.join("fake-codex.json").is_file()
            && pi_home.join("fake-pi.json").is_file()
    });
    let records = [
        json(&claude_home.join("fake-claude-c1.json")),
        json(&codex_home.join("fake-codex.json")),
        json(&pi_home.join("fake-pi.json")),
    ];
    let env = |record: &Value, name: &str| record["env"][name].as_str().map(str::to_string);
    assert_eq!(
        env(&records[0], "CLAUDE_CODE_OAUTH_TOKEN").as_deref(),
        Some("tok-claude-1111")
    );
    // The other Claude agent, on the same config, has none.
    let c2 = json(&claude_home.join("fake-claude-c2.json"));
    assert_eq!(env(&c2, "CLAUDE_CODE_OAUTH_TOKEN"), None);
    assert_eq!(
        env(&records[1], "OPENAI_API_KEY").as_deref(),
        Some("tok-codex-2222")
    );
    assert_eq!(
        env(&records[2], "ANTHROPIC_API_KEY").as_deref(),
        Some("tok-pi-3333")
    );

    // No agent's environment holds a token that is not its own.
    let own = ["tok-claude-1111", "tok-codex-2222", "tok-pi-3333"];
    for (i, record) in records.iter().chain([&c2]).enumerate() {
        let text = record["env"].to_string();
        for (j, token) in own.iter().enumerate() {
            // c2 (index 3) owns none of them.
            if i == 3 || i != j {
                assert!(!text.contains(token), "agent {i} holds token {j}");
            }
        }
    }

    // Nothing crew reports carries a token: status, events, the roster, the
    // config report, and the files the daemon wrote for the agents.
    let mut reported = String::new();
    reported.push_str(&s.ok(&["status", "--json", "-C", &s.workspace.to_string_lossy()]));
    reported.push_str(&s.ok(&["logs", "-n", "500", "-C", &s.workspace.to_string_lossy()]));
    reported.push_str(&s.ok(&["config", "-C", &s.workspace.to_string_lossy()]));
    for entry in walk(&s.workspace.join(".crew")) {
        if entry.extension().is_some_and(|e| e == "db") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&entry) {
            reported.push_str(&text);
        }
    }
    for token in own {
        assert!(
            !reported.contains(token),
            "{token} appears in what crew reports or writes"
        );
    }
    let db = std::fs::read(s.workspace.join(".crew/crew.db")).unwrap();
    for token in own {
        assert!(
            !db.windows(token.len()).any(|w| w == token.as_bytes()),
            "{token} is in the database"
        );
    }
    s.stop();
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}
