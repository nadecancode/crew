//! Member toolbox lifecycle through the real daemon and PTYs. Native CLI
//! discovery behavior is covered by notes/toolbox-probes; the stand-ins here
//! record the exact process environment and launch configuration.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::*;
use crew_core::config::{AgentSpec, SandboxMode, TeamConfig};
use crew_core::{AgentToolbox, RuntimeKind, ToolboxItemCode};
use serde_json::{json, Value};

struct Fixture {
    session: Session,
    _clis: tempfile::TempDir,
    path: std::ffi::OsString,
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn seed_codex_project(session: &Session, config: &str) {
    write(&session.workspace.join(".codex/config.toml"), config);
    for args in [
        vec!["init", "-q"],
        vec!["add", ".codex"],
        vec!["commit", "-qm", "Seed native project configuration"],
    ] {
        let output = git_test_support::git(&session.workspace)
            .env("HOME", session.root().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }
}

/// Parse every -c value from an actual captured member launch, including
/// app-server-only permission overrides. `mcp list` loads native configuration
/// without opening a model session or making an inference request.
fn native_codex_parses_all_overrides(record: &Value, process: &str) {
    let Ok(binary) = std::env::var("CREW_TEST_CODEX_BINARY") else {
        return;
    };
    let args: Vec<_> = record["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let overrides: Vec<_> = args
        .windows(2)
        .filter(|pair| pair[0] == "-c" || pair[0] == "--config")
        .flat_map(|pair| pair.iter().copied())
        .collect();
    assert!(!overrides.is_empty());
    let mut command = std::process::Command::new(binary);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    for name in ["HOME", "CODEX_HOME"] {
        if let Some(value) = record["env"][name].as_str() {
            command.env(name, value);
        }
    }
    let output = command
        .current_dir(record["cwd"].as_str().unwrap())
        .args(&overrides)
        .args(["mcp", "list", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Codex rejected {process} -c values: {}",
        stderr(&output)
    );
    assert!(serde_json::from_slice::<Value>(&output.stdout)
        .unwrap()
        .is_array());
    eprintln!(
        "native Codex accepted all {} {process} overrides",
        overrides.len() / 2
    );
}

impl Fixture {
    fn new() -> Self {
        let session = Session::new();
        let clis = tempfile::tempdir_in(Path::new(CREW).parent().unwrap()).unwrap();
        let bin = clis.path().canonicalize().unwrap();
        for name in ["claude", "codex", "pi"] {
            let file = bin.join(name);
            // An optional native inventory gate: forward only `mcp list`
            // to the installed Codex. Session and model calls stay in the
            // stand-in, so this cannot make a provider request.
            let inventory = if name == "codex" {
                std::env::var("CREW_TEST_CODEX_BINARY")
                    .ok()
                    .map(|program| {
                        format!(
                            "for arg do\n  if [ \"$arg\" = mcp ]; then exec {} \"$@\"; fi\ndone\n",
                            crew::agents::shell_quote(
                                Path::new(&program)
                                    .canonicalize()
                                    .unwrap()
                                    .to_str()
                                    .unwrap()
                            )
                        )
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let discovery = if name == "pi" {
                "if [ \"$1\" = --help ] || [ \"$1\" = --list-models ]; then exit 0; fi\n"
            } else {
                ""
            };
            write(
                &file,
                &format!(
                    "#!/bin/sh\n{inventory}{discovery}exec {} fake-cli {name} \"$@\"\n",
                    session.sandboxed_crew()
                ),
            );
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        Self {
            session,
            _clis: clis,
            path: std::env::join_paths(paths).unwrap(),
        }
    }

    fn team(&self, runtime: RuntimeKind, toolbox: Value) -> TeamConfig {
        let mut team = self.session.team(
            "toolbox-launch",
            vec![AgentSpec {
                id: "x".into(),
                role: "fixture".into(),
                runtime: Some(runtime),
                toolbox: serde_json::from_value(toolbox).unwrap(),
                ..AgentSpec::default()
            }],
            vec![],
        );
        team.defaults.max_restarts = 0;
        team
    }

    fn run(&self, team: &TeamConfig) {
        write(
            &self.session.root().join("team.yaml"),
            &team.to_yaml().unwrap(),
        );
        std::fs::create_dir_all(self.session.root().join("home")).unwrap();
        // Unlike the stand-in, native Codex requires an explicitly selected
        // CODEX_HOME to exist before even `mcp list --json` can run.
        std::fs::create_dir_all(self.session.root().join("codex-home")).unwrap();
        let output = self
            .session
            .command()
            .env("PATH", &self.path)
            .env("CREW_PI", self._clis.path().join("pi"))
            .env("HOME", self.session.root().join("home"))
            .env("TOOLBOX_ALIAS_SOURCE", "alias-secret-sentinel")
            .args(["run", "team.yaml", "--no-attach"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }

    fn toolbox(&self) -> AgentToolbox {
        let text = self.session.ok(&[
            "toolbox",
            "x",
            "--json",
            "-C",
            self.session.workspace.to_str().unwrap(),
        ]);
        let value: Value = serde_json::from_str(&text).unwrap();
        serde_json::from_value(value[0]["toolbox"].clone()).unwrap()
    }

    fn record(&self, path: &Path) -> Value {
        self.session
            .wait_for("fake CLI record", || read_json(path).is_some());
        read_json(path).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.session.stop();
        // Immutable store and generated plugin directories must be writable
        // for the test harness to remove its own temporary workspace.
        fn directories(path: &Path) {
            if !std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                return;
            }
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    directories(&entry.path());
                }
            }
        }
        directories(&self.session.workspace.join(".crew/toolbox"));
        directories(&self.session.workspace.join(".crew/agents"));
    }
}

fn sandbox_available() -> bool {
    match crew::sandbox::probe::probe(Path::new(CREW), std::env::var_os("PATH").as_ref()) {
        Ok(_) => true,
        Err(error) => {
            assert_ne!(
                std::env::var("CREW_TEST_SANDBOX").as_deref(),
                Ok("require"),
                "{error}"
            );
            eprintln!("skipped: sandboxes unavailable: {error}");
            false
        }
    }
}

#[test]
fn compact_cli_is_opt_in_pinned_and_recovers_through_the_member_binding() {
    use sha2::{Digest, Sha256};
    for runtime in [RuntimeKind::Claude, RuntimeKind::Codex] {
        for sandboxed in [false, true] {
            if sandboxed && !sandbox_available() {
                continue;
            }
            let fixture = Fixture::new();
            let s = &fixture.session;
            let mut team = fixture.team(runtime, json!({"include":["compact-cli"]}));
            if sandboxed {
                team.workspace.sandbox.mode = SandboxMode::Required;
            }
            if sandboxed && runtime == RuntimeKind::Claude {
                // Execute the selected script inside the real member sandbox,
                // before the recorded CLI reports ready. No extra mounts/rights.
                write(
                    &fixture._clis.path().join("claude"),
                    &format!(
                        r#"#!/bin/sh
set -eu
if [ -n "${{CREW_COMPACT_CLI_HELPER:-}}" ]; then
  compact_script="$(dirname "$CREW_IDENTITY")/crew-toolbox/skills/compact-cli/scripts/output"
  printf 'running 1 test\ntest fixture ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n' > compact-capture.log
  "$compact_script" view compact-capture.log --exit-code 0 > compact-view.json
  compact_ref=$(sed -n 's/.*"reference":"\([^"]*\)".*/\1/p' compact-view.json)
  "$compact_script" raw "$compact_ref" > compact-recovered.log 2> compact-range.json
  cmp compact-capture.log compact-recovered.log
fi
exec {} fake-cli claude "$@"
"#,
                        s.sandboxed_crew()
                    ),
                );
            }
            fixture.run(&team);
            let home = if sandboxed {
                s.workspace
                    .join(".crew/agents/x/rw/home")
                    .join(runtime.as_str())
            } else {
                s.root().join(format!("{}-home", runtime.as_str()))
            };
            let record_name = if runtime == RuntimeKind::Claude {
                "fake-claude-x.json"
            } else {
                "fake-codex-server.json"
            };
            let record = fixture.record(&home.join(record_name));
            if sandboxed && runtime == RuntimeKind::Claude {
                let workdir = Path::new(record["cwd"].as_str().unwrap());
                assert_eq!(
                    read_json(&workdir.join("compact-view.json")).unwrap()["kind"],
                    "summary"
                );
                assert_eq!(
                    std::fs::read(workdir.join("compact-capture.log")).unwrap(),
                    std::fs::read(workdir.join("compact-recovered.log")).unwrap()
                );
            }
            let pin = s.crew_copy();
            assert_eq!(record["env"]["CREW_COMPACT_CLI_HELPER"], json!(pin));
            let skill = if runtime == RuntimeKind::Claude {
                s.workspace
                    .join(".crew/agents/x/crew-toolbox/skills/compact-cli")
            } else {
                let roots = fixture.record(&home.join("fake-codex-skill-roots.json"));
                Path::new(roots["extraRoots"][0].as_str().unwrap()).to_path_buf()
            };
            let manifest = read_json(&skill.join("helper.json")).unwrap();
            let digest = format!("{:x}", Sha256::digest(std::fs::read(&pin).unwrap()));
            assert_eq!(manifest["helper_sha256"], digest);
            let before = fixture.toolbox();
            assert_eq!(before.items.len(), 1);
            assert_eq!(before.items[0].status.code, ToolboxItemCode::Active);
            assert_eq!(before.items[0].origin, "bundle:compact-cli");
            let input = s.root().join("captured.txt");
            let source =
                include_bytes!("../../crew-core/tests/fixtures/output-view/cargo-success.txt");
            std::fs::write(&input, source).unwrap();
            let invoke = |args: &[&str]| {
                std::process::Command::new(skill.join("scripts/output"))
                    .env("CREW_COMPACT_CLI_HELPER", &pin)
                    .env("CREW_IDENTITY", s.identity_path("x"))
                    .args(args)
                    .output()
                    .unwrap()
            };
            let viewed = invoke(&["view", input.to_str().unwrap(), "--exit-code", "0"]);
            assert!(viewed.status.success(), "{}", stderr(&viewed));
            let view: Value = serde_json::from_slice(&viewed.stdout).unwrap();
            assert_eq!(view["kind"], "summary");
            let raw = invoke(&["raw", view["source"]["reference"].as_str().unwrap()]);
            assert!(raw.status.success(), "{}", stderr(&raw));
            assert_eq!(raw.stdout, source);
            // The executable check precedes identity and source access.
            let refused = std::process::Command::new(&pin)
                .env_remove("CREW_IDENTITY")
                .args([
                    "output",
                    "--expected-helper",
                    &"0".repeat(64),
                    "view",
                    "/missing",
                    "--exit-code",
                    "0",
                ])
                .output()
                .unwrap();
            assert!(!refused.status.success());
            assert!(stderr(&refused).contains("helper changed"));
            assert!(refused.stdout.is_empty());
            // Exclusion stays pending; a live skill is not mutated by reload.
            team.agents[0].toolbox = serde_json::from_value(
                json!({"include":["compact-cli"],"exclude":["skills.compact-cli"]}),
            )
            .unwrap();
            write(&s.root().join("team.yaml"), &team.to_yaml().unwrap());
            s.ok(&[
                "toolbox",
                "reload",
                "team.yaml",
                "-C",
                s.workspace.to_str().unwrap(),
            ]);
            assert_eq!(fixture.toolbox().fingerprint, before.fingerprint);
            assert!(skill.join("SKILL.md").exists());
            s.agent_cmd(&["restart", "x"]);
            s.wait("excluded compact skill", |snapshot| {
                snapshot.agents.iter().any(|agent| {
                    agent.id == "x"
                        && agent
                            .toolbox
                            .as_ref()
                            .is_some_and(|toolbox| toolbox.items.is_empty())
                })
            });
        }
    }
}

#[test]
fn guarded_codex_reports_compact_cli_unavailable_without_changing_permissions() {
    let fixture = Fixture::new();
    let s = &fixture.session;
    let mut team = fixture.team(RuntimeKind::Codex, json!({"include":["compact-cli"]}));
    team.defaults.permissions = crew_core::config::PermissionPolicy::Guarded;
    fixture.run(&team);
    let record = fixture.record(&s.root().join("codex-home/fake-codex-server.json"));
    assert!(record["args"]
        .as_array()
        .unwrap()
        .contains(&json!("sandbox_mode=\"workspace-write\"")));
    assert_eq!(record["env"]["CREW_COMPACT_CLI_HELPER"], "");
    let view = fixture.toolbox();
    assert_eq!(view.items[0].status.code, ToolboxItemCode::NotAvailable);
    assert!(view.items[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("native sandbox"));
    assert!(!s
        .root()
        .join("codex-home/fake-codex-skill-roots.json")
        .exists());
}

#[test]
fn claude_pins_files_and_credentials_and_reload_waits_for_restart() {
    let fixture = Fixture::new();
    let s = &fixture.session;
    let skill = s.root().join("selected-skill");
    write(
        &skill.join("SKILL.md"),
        "---\nname: selected\ndescription: Selected skill.\n---\nFIRST_BODY\n",
    );
    let secret = s.root().join("token");
    write(&secret, "file-secret-sentinel\n");
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    write(&s.workspace.join(".crew/config.toml"), &format!("[agents.x.toolbox_env]\nTOOLBOX_FILE = {{file={}}}\nTOOLBOX_ALIAS = {{env=\"TOOLBOX_ALIAS_SOURCE\"}}\n", json!(secret)));
    let server = |reference: &str| json!({"command":[CREW,"fake-mcp","--leak","DEST"],"env":{"DEST":{"env":reference}},"required":true});
    let mut team = fixture.team(
        RuntimeKind::Claude,
        json!({
            "skills":{"selected":{"path":skill,"required":true}},
            "mcp":{"file":server("TOOLBOX_FILE"),"alias":server("TOOLBOX_ALIAS")}
        }),
    );
    fixture.run(&team);
    let record = fixture.record(&s.root().join("claude-home/fake-claude-x.json"));
    for name in [
        "TOOLBOX_FILE",
        "TOOLBOX_ALIAS",
        "TOOLBOX_ALIAS_SOURCE",
        "DEST",
    ] {
        assert!(
            record["env"].get(name).is_none(),
            "{name} leaked to the session"
        );
    }
    let args = record["args"].as_array().unwrap();
    assert!(args.contains(&json!("--strict-mcp-config")));
    assert!(args
        .windows(2)
        .any(|args| args == [json!("--setting-sources"), json!("project")]));
    let before = fixture.toolbox();
    assert!(before
        .items
        .iter()
        .all(|item| item.status.code == ToolboxItemCode::Active));
    let agent = s.workspace.join(".crew/agents/x");
    let plugin_skill = agent.join("crew-toolbox/skills/selected/SKILL.md");
    let mcp = std::fs::read(agent.join("mcp.json")).unwrap();
    for (name, reference) in [("file", "TOOLBOX_FILE"), ("alias", "TOOLBOX_ALIAS")] {
        let snapshot =
            std::fs::read_to_string(agent.join(format!("toolbox/mcp/{name}.json"))).unwrap();
        assert!(
            snapshot.contains(&format!("[redacted ${reference}]")),
            "credential did not reach its server"
        );
        assert!(!snapshot.contains("secret-sentinel"));
    }
    write(
        &skill.join("SKILL.md"),
        "---\nname: selected\ndescription: Selected skill.\n---\nSECOND_BODY\n",
    );
    team.objective = "ignored objective edit".into();
    write(&s.root().join("team.yaml"), &team.to_yaml().unwrap());
    let reloaded = s.ok(&[
        "toolbox",
        "reload",
        "team.yaml",
        "-C",
        s.workspace.to_str().unwrap(),
    ]);
    assert!(reloaded.contains("objective"), "{reloaded}");
    let pending = fixture.toolbox();
    assert_eq!(pending.fingerprint, before.fingerprint);
    assert!(pending.pending_fingerprint.is_some());
    assert!(std::fs::read_to_string(&plugin_skill)
        .unwrap()
        .contains("FIRST_BODY"));
    assert!(s.has_event("toolbox.pending"));
    s.agent_cmd(&["restart", "x"]);
    s.wait("new toolbox to be applied", |snapshot| {
        snapshot.agents.iter().any(|agent| {
            agent.id == "x"
                && agent.toolbox.as_ref().is_some_and(|toolbox| {
                    toolbox.fingerprint != before.fingerprint
                        && toolbox.pending_fingerprint.is_none()
                })
        })
    });
    fixture.record(&s.root().join("claude-home/fake-claude-x.json"));
    assert!(std::fs::read_to_string(&plugin_skill)
        .unwrap()
        .contains("SECOND_BODY"));
    assert_eq!(mcp, std::fs::read(agent.join("mcp.json")).unwrap());
    s.wait_for("toolbox changed event", || s.has_event("toolbox.changed"));
    let exposed = format!(
        "{}{}{}",
        serde_json::to_string(&s.events()).unwrap(),
        serde_json::to_string(&fixture.toolbox()).unwrap(),
        std::fs::read_to_string(agent.join("local.json")).unwrap_or_default()
    );
    assert!(!exposed.contains("secret-sentinel"));
}

/// Exercise the operator CLI and the real daemon's launch/preflight path.
/// Runtime stand-ins confirm session identity; QA's localhost native probes
/// separately check the definitions that reach each runtime's model requests.
fn resumed_restart_refreshes_mcp_catalog(runtime: RuntimeKind) {
    let fixture = Fixture::new();
    let s = &fixture.session;
    let state = s.root().join("mcp-listings");
    let mut team = fixture.team(
        runtime,
        json!({"mcp":{"probe":{
            "command":[CREW,"fake-mcp","--tools","probe","--state",state,
                       "--change-description-after","1"],
            "required":true
        }}}),
    );
    team.defaults.max_restarts = 2;
    fixture.run(&team);
    let running = s.wait("member with a pinned MCP catalog", |snapshot| {
        let agent = find_agent(snapshot, "x");
        agent.pid.is_some()
            && agent.status != crew_core::AgentStatus::Spawning
            && agent
                .toolbox
                .as_ref()
                .is_some_and(|toolbox| toolbox.definition_digest.is_some())
    });
    // The Codex stand-in does not make a turn; supply the same session
    // confirmation its native notify hook sends after a completed turn.
    if runtime == RuntimeKind::Codex {
        let output = s
            .command()
            .args(["hook", "codex-notify", "--identity"])
            .arg(s.identity_path("x"))
            .arg(r#"{"type":"agent-turn-complete","thread-id":"toolbox-test-thread"}"#)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let database = rusqlite::Connection::open_with_flags(
        s.workspace.join(".crew/crew.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    s.wait_for("confirmed member session", || {
        database
            .query_row("SELECT value FROM meta WHERE key = 'agent:x'", [], |row| {
                row.get::<_, String>(0)
            })
            .ok()
            .and_then(|value| serde_json::from_str::<Value>(&value).ok())
            .is_some_and(|local| local["session_confirmed"] == true)
    });
    let initial = find_agent(&s.snapshot(), "x").clone();
    let before = initial.toolbox.unwrap();
    assert!(initial.session_id.is_some());
    let pin = s.workspace.join(".crew/agents/x/toolbox/mcp/probe.json");
    let old_pin = std::fs::read(&pin).unwrap();
    assert!(!String::from_utf8_lossy(&old_pin).contains("Changed."));
    assert!(!s.has_event("toolbox.changed"));

    // A crash starts another preflight, which now sees the changed upstream
    // description. Recovery must still serve the old catalog and session.
    let old_pid = find_agent(&running, "x").pid.unwrap();
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &old_pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let recovered = s.wait("crash recovery to preserve the catalog", |snapshot| {
        let agent = find_agent(snapshot, "x");
        agent.restarts == 1
            && agent.pid.is_some_and(|pid| pid != old_pid)
            && agent.status != crew_core::AgentStatus::Spawning
    });
    let recovered = find_agent(&recovered, "x");
    assert_eq!(recovered.session_id, initial.session_id);
    assert_eq!(std::fs::read(&pin).unwrap(), old_pin);
    assert_eq!(
        recovered.toolbox.as_ref().unwrap().definition_digest,
        before.definition_digest
    );
    s.wait_for("different upstream listing to be logged", || {
        s.has_event("toolbox.listing")
    });
    assert!(!s.has_event("toolbox.changed"));

    // Unlike an automatic recovery, the explicit operator restart refreshes
    // the definitions while resuming the confirmed conversation.
    let (record, resume_arg) = match runtime {
        RuntimeKind::Claude => ("claude-home/fake-claude-x.json", "--resume"),
        RuntimeKind::Codex => ("codex-home/fake-codex.json", "resume"),
        RuntimeKind::Pi => ("pi-home/fake-pi.json", "--session-id"),
        RuntimeKind::Shell => unreachable!(),
    };
    let record = s.root().join(record);
    assert!(!fixture.record(&record)["args"]
        .as_array()
        .unwrap()
        .contains(&json!("--system-prompt-snapshot")));
    std::fs::remove_file(&record).unwrap();
    s.ok(&[
        "resume",
        "--restart",
        "x",
        "-C",
        s.workspace.to_str().unwrap(),
    ]);
    let refreshed = s.wait("explicit resumed restart to finish", |snapshot| {
        let agent = find_agent(snapshot, "x");
        agent.pid.is_some()
            && agent.pid != recovered.pid
            && agent.status != crew_core::AgentStatus::Spawning
    });
    let refreshed = find_agent(&refreshed, "x");
    assert_eq!(refreshed.session_id, initial.session_id);
    let launch = fixture.record(&record);
    let args = launch["args"].as_array().unwrap();
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == resume_arg && pair[1].as_str() == initial.session_id.as_deref()),
        "{runtime} did not resume the confirmed session: {args:?}"
    );
    if runtime == RuntimeKind::Claude {
        assert!(args
            .windows(2)
            .any(|pair| pair == [json!("--system-prompt-snapshot"), json!("off")]));
    }
    let after = refreshed.toolbox.as_ref().unwrap();
    assert_eq!(after.fingerprint, before.fingerprint);
    assert_ne!(after.definition_digest, before.definition_digest);
    assert!(std::fs::read_to_string(&pin).unwrap().contains("Changed."));
    s.wait_for("catalog refresh to publish toolbox.changed", || {
        s.events().iter().any(|event| {
            event.event_type == "toolbox.changed"
                && event.agent_id.as_deref() == Some("x")
                && event.data["definition_digest"].as_str() == after.definition_digest.as_deref()
        })
    });
    assert_eq!(
        s.events()
            .iter()
            .filter(|event| event.event_type == "toolbox.changed"
                && event.agent_id.as_deref() == Some("x"))
            .count(),
        1
    );
    if runtime == RuntimeKind::Claude {
        // A one-shot flag would let the native runtime restore its original
        // schema snapshot on this automatic recovery. Retain the SID marker.
        // Recovery also must not depend on another capability probe succeeding.
        write(&fixture._clis.path().join("claude"), &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo unknown; exit 0; fi\nexec {} fake-cli claude \"$@\"\n",
            s.sandboxed_crew()
        ));
        std::fs::remove_file(&record).unwrap();
        assert!(std::process::Command::new("kill")
            .args(["-KILL", &refreshed.pid.unwrap().to_string()])
            .status()
            .unwrap()
            .success());
        let recovered_again = s.wait("refreshed Claude conversation to recover", |snapshot| {
            let agent = find_agent(snapshot, "x");
            agent.restarts == 2
                && agent.pid.is_some()
                && agent.pid != refreshed.pid
                && agent.status != crew_core::AgentStatus::Spawning
        });
        let recovered_again = find_agent(&recovered_again, "x");
        assert_eq!(recovered_again.session_id, initial.session_id);
        assert_eq!(
            recovered_again.toolbox.as_ref().unwrap().definition_digest,
            after.definition_digest
        );
        let launch = fixture.record(&record);
        assert!(launch["args"]
            .as_array()
            .unwrap()
            .windows(2)
            .any(|pair| { pair == [json!("--system-prompt-snapshot"), json!("off")] }));
        let local: String = database
            .query_row("SELECT value FROM meta WHERE key = 'agent:x'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&local).unwrap()["claude_rebuild_prompt_session"]
                .as_str(),
            initial.session_id.as_deref()
        );
        std::fs::remove_file(&record).unwrap();
        s.agent_cmd(&["restart", "--fresh", "x"]);
        s.wait("fresh Claude session", |snapshot| {
            let agent = find_agent(snapshot, "x");
            agent.session_id.is_some()
                && agent.session_id != initial.session_id
                && agent.pid.is_some()
                && agent.status != crew_core::AgentStatus::Spawning
        });
        assert!(!fixture.record(&record)["args"]
            .as_array()
            .unwrap()
            .contains(&json!("--system-prompt-snapshot")));
        let local: String = database
            .query_row("SELECT value FROM meta WHERE key = 'agent:x'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            serde_json::from_str::<Value>(&local).unwrap()["claude_rebuild_prompt_session"]
                .is_null()
        );
    }
}

#[test]
fn claude_resumed_restart_refreshes_mcp_catalog() {
    resumed_restart_refreshes_mcp_catalog(RuntimeKind::Claude);
}

#[test]
fn codex_resumed_restart_refreshes_mcp_catalog() {
    resumed_restart_refreshes_mcp_catalog(RuntimeKind::Codex);
}

#[test]
fn pi_resumed_restart_refreshes_mcp_catalog() {
    resumed_restart_refreshes_mcp_catalog(RuntimeKind::Pi);
}

#[test]
fn an_old_claude_cannot_claim_a_refreshed_resumed_toolbox() {
    let fixture = Fixture::new();
    let s = &fixture.session;
    let team = fixture.team(
        RuntimeKind::Claude,
        json!({"mcp":{"probe":{
            "command":[CREW,"fake-mcp","--tools","probe","--state",s.root().join("listings"),
                       "--change-description-after","1"],
            "required":true
        }}}),
    );
    let cli = fixture._clis.path().join("claude");
    write(&cli, &format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '2.1.256 (Claude Code fixture)'; exit 0; fi\nexec {} fake-cli claude \"$@\"\n",
        s.sandboxed_crew()
    ));
    fixture.run(&team);
    let record = s.root().join("claude-home/fake-claude-x.json");
    fixture.record(&record);
    s.wait("old Claude session to be ready", |snapshot| {
        let agent = find_agent(snapshot, "x");
        agent.pid.is_some() && agent.status != crew_core::AgentStatus::Spawning
    });
    let database = rusqlite::Connection::open_with_flags(
        s.workspace.join(".crew/crew.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    s.wait_for("old Claude session to be confirmed", || {
        let local: String = database
            .query_row("SELECT value FROM meta WHERE key = 'agent:x'", [], |row| {
                row.get(0)
            })
            .unwrap();
        serde_json::from_str::<Value>(&local).unwrap()["session_confirmed"] == true
    });
    std::fs::remove_file(&record).unwrap();
    s.ok(&[
        "resume",
        "--restart",
        "x",
        "-C",
        s.workspace.to_str().unwrap(),
    ]);
    s.wait("unavailable resumed refresh to be reported", |snapshot| {
        let agent = find_agent(snapshot, "x");
        agent.status == crew_core::AgentStatus::Failed
            && agent
                .error
                .as_deref()
                .is_some_and(|error| error.contains("2.1.257"))
    });
    assert!(
        !record.exists(),
        "the unsupported native session was started"
    );
    let view = fixture.toolbox();
    assert_eq!(view.items[0].status.code, ToolboxItemCode::NotAvailable);
    assert!(view.items[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("--fresh x"));
    // The refused refresh did not replace the old pin or latch an unsupported
    // flag. An ordinary resumed restart still works with the old CLI.
    s.agent_cmd(&["restart", "x"]);
    let recovered = fixture.record(&record);
    assert!(!recovered["args"]
        .as_array()
        .unwrap()
        .contains(&json!("--system-prompt-snapshot")));
    assert!(
        !std::fs::read_to_string(s.workspace.join(".crew/agents/x/toolbox/mcp/probe.json"))
            .unwrap()
            .contains("Changed.")
    );
    std::fs::remove_file(&record).unwrap();
    // The documented escape starts a new native snapshot, so an old CLI
    // can still start fresh without being passed the unsupported flag.
    s.agent_cmd(&["restart", "--fresh", "x"]);
    let launch = fixture.record(&record);
    assert!(!launch["args"]
        .as_array()
        .unwrap()
        .contains(&json!("--system-prompt-snapshot")));
}

#[test]
fn codex_closes_ambient_servers_on_both_processes_and_sets_roots_before_tui() {
    for sandboxed in [false, true] {
        if sandboxed && !sandbox_available() {
            continue;
        }
        let fixture = Fixture::new();
        let s = &fixture.session;
        let skill = s.root().join("skill");
        write(
            &skill.join("SKILL.md"),
            "---\nname: selected\ndescription: Selected.\n---\nBODY\n",
        );
        let native_home = s.root().join("codex-home");
        for path in [
            s.root().join("home/.agents/skills/operator/SKILL.md"),
            native_home.join("skills/home/SKILL.md"),
        ] {
            write(&path, "---\nname: operator_fixture\ndescription: Excluded operator skill.\n---\nPRIVATE_FIXTURE\n");
        }
        let ambient = ["ambient", "dot.name", "two words", "quote\"name"];
        let operator_config = ambient
            .iter()
            .map(|name| format!("[mcp_servers.{}]\ncommand='/bin/false'\n", json!(name)))
            .collect::<String>();
        write(&native_home.join("config.toml"), &operator_config);
        write(
            &s.workspace.join(".crew/config.toml"),
            "[agents.x.toolbox_env]\nTOOLBOX_ALIAS = {env=\"TOOLBOX_ALIAS_SOURCE\"}\n",
        );
        let mut team = fixture.team(
            RuntimeKind::Codex,
            json!({
                "skills":{"selected":{"path":skill,"required":true}},
                "mcp":{"selected":{"command":["/bin/sh","-c",format!("exec {} fake-mcp --leak DEST",s.sandboxed_crew())],"env":{"DEST":{"env":"TOOLBOX_ALIAS"}},"required":true}}
            }),
        );
        if sandboxed {
            team.workspace.sandbox.mode = SandboxMode::Required;
            for relative in [".agents/skills/stale/SKILL.md", "codex/agents/stale.toml"] {
                write(
                    &s.workspace.join(".crew/agents/x/rw/home").join(relative),
                    "STALE_MEMBER_FILE\n",
                );
            }
        }
        fixture.run(&team);
        let home = if sandboxed {
            s.workspace.join(".crew/agents/x/rw/home/codex")
        } else {
            native_home.clone()
        };
        let tui = fixture.record(&home.join("fake-codex.json"));
        if sandboxed {
            let private = s.workspace.join(".crew/agents/x/rw/home");
            assert!(!private.join(".agents/skills/stale/SKILL.md").exists());
            assert!(!private.join("codex/agents/stale.toml").exists());
        }
        let server = fixture.record(&home.join("fake-codex-server.json"));
        let roots = read_json(&home.join("fake-codex-skill-roots.json"))
            .expect("roots acknowledged before TUI launch");
        let files = read_json(&home.join("fake-codex-skill-files.json")).unwrap();
        assert_eq!(files, json!([{"readable":true,"read_only":true}]));
        assert_eq!(roots["extraRoots"].as_array().unwrap().len(), 1);
        let root = roots["extraRoots"][0].as_str().unwrap();
        assert!(root.contains("/.crew/toolbox/store/"));
        assert!(std::fs::read_to_string(Path::new(root).join("SKILL.md"))
            .unwrap()
            .contains("BODY"));
        for (process, record) in [("TUI", tui), ("app-server", server)] {
            native_codex_parses_all_overrides(&record, process);
            for name in ["TOOLBOX_ALIAS", "TOOLBOX_ALIAS_SOURCE", "DEST"] {
                assert!(
                    record["env"].get(name).is_none(),
                    "{name} reached the member process"
                );
            }
            let args = record["args"].as_array().unwrap();
            assert!(args.contains(&json!("mcp_servers.crew.enabled=true")));
            if !sandboxed {
                assert!(
                    args.iter()
                        .filter_map(Value::as_str)
                        .any(|arg| arg.starts_with("skills.config=[")),
                    "fixture must exercise the array-of-tables override"
                );
                let patch = args
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|arg| arg.starts_with("mcp_servers={"))
                    .unwrap();
                let parsed: toml::Value = patch.parse().unwrap();
                for name in ambient {
                    assert_eq!(
                        parsed["mcp_servers"][name]["enabled"].as_bool(),
                        Some(false),
                        "{name} not excluded"
                    );
                }
            }
        }
        assert_eq!(
            std::fs::read_to_string(native_home.join("config.toml")).unwrap(),
            operator_config
        );
        if !sandboxed {
            let view = fixture.toolbox();
            for name in ambient {
                assert!(
                    view.sources.iter().any(|source| source.status.code
                        == crew_core::ToolboxSourceCode::Disabled
                        && source.names.iter().any(|entry| entry == name)),
                    "{name} not reported disabled"
                );
            }
        }
        assert!(fixture
            .toolbox()
            .items
            .iter()
            .all(|item| item.status.code == ToolboxItemCode::Active));
    }
}

#[test]
fn missing_references_are_visible_and_required_items_refuse_runtime_changes() {
    let fixture = Fixture::new();
    let s = &fixture.session;
    let team = fixture.team(RuntimeKind::Claude, json!({
        "mcp":{"missing":{"command":["/bin/false"],"env":{"TOKEN":{"env":"TOOLBOX_UNSET_LAUNCH_TEST"}}}},
        "hooks":{"required":{"event":"PreToolUse","command":["/bin/true"],"required":true}}
    }));
    fixture.run(&team);
    fixture.record(&s.root().join("claude-home/fake-claude-x.json"));
    let before = fixture.toolbox();
    assert!(before
        .items
        .iter()
        .any(|item| item.name == "missing" && item.status.code == ToolboxItemCode::Needs));
    let switch = s.crew(&[
        "agent",
        "model",
        "x",
        "--runtime",
        "codex",
        "-C",
        s.workspace.to_str().unwrap(),
    ]);
    assert!(!switch.status.success());
    assert!(
        stderr(&switch).contains("hooks.required"),
        "{}",
        stderr(&switch)
    );
    assert_eq!(fixture.toolbox().fingerprint, before.fingerprint);
    assert_eq!(
        s.snapshot()
            .agents
            .iter()
            .find(|agent| agent.id == "x")
            .unwrap()
            .runtime,
        RuntimeKind::Claude
    );
}

#[test]
fn strict_shell_members_do_not_read_or_require_toolbox_files() {
    let fixture = Fixture::new();
    let mut team = fixture.team(
        RuntimeKind::Shell,
        json!({"strict":true,"skills":{"absent":{"path":"/absent/skill","required":true}}}),
    );
    team.agents[0].command = scripted(&[]);
    fixture.run(&team);
    fixture.session.wait("shell toolbox", |snapshot| {
        snapshot
            .agents
            .iter()
            .any(|agent| agent.id == "x" && agent.toolbox.is_some())
    });
    assert_eq!(
        fixture.toolbox().items[0].status.code,
        ToolboxItemCode::NotApplicable
    );
}

#[test]
fn required_missing_references_and_unexcluded_sources_stop_before_cli_launch() {
    for strict_source in [false, true] {
        let fixture = Fixture::new();
        let s = &fixture.session;
        let (runtime, toolbox) = if strict_source {
            write(&s.root().join("codex-home/AGENTS.md"), "PRIVATE_RULES\n");
            (RuntimeKind::Codex, json!({"strict":true}))
        } else {
            (
                RuntimeKind::Claude,
                json!({"mcp":{"missing":{"command":["/bin/false"],"env":{"TOKEN":{"env":"TOOLBOX_UNSET_REQUIRED_LAUNCH_TEST"}},"required":true}}}),
            )
        };
        fixture.run(&fixture.team(runtime, toolbox));
        s.wait("toolbox refusal", |snapshot| {
            snapshot.agents.iter().any(|agent| {
                agent.id == "x"
                    && agent
                        .error
                        .as_ref()
                        .is_some_and(|error| error.contains("toolbox prevents launch"))
            })
        });
        assert!(!s.root().join("claude-home/fake-claude-x.json").exists());
        assert!(!s.root().join("codex-home/fake-codex-server.json").exists());
        let view = fixture.toolbox();
        if strict_source {
            assert!(
                view.sources
                    .iter()
                    .any(|source| source.status.code
                        == crew_core::ToolboxSourceCode::CannotBeExcluded)
            );
        } else {
            assert_eq!(view.items[0].status.code, ToolboxItemCode::Needs);
        }
    }
}

#[test]
fn an_unbound_mcp_program_is_blocked_without_extending_the_sandbox() {
    if !sandbox_available() {
        return;
    }
    let fixture = Fixture::new();
    let s = &fixture.session;
    let program = s.root().join("outside/server");
    write(&program, "#!/bin/sh\nexit 41\n");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut team = fixture.team(
        RuntimeKind::Claude,
        json!({"mcp":{"outside":{"command":[program]}}}),
    );
    team.workspace.sandbox.mode = SandboxMode::Required;
    fixture.run(&team);
    fixture.record(
        &s.workspace
            .join(".crew/agents/x/rw/home/claude/fake-claude-x.json"),
    );
    let toolbox = fixture.toolbox();
    assert_eq!(toolbox.items[0].status.code, ToolboxItemCode::Blocked);
    let config = read_json(&s.workspace.join(".crew/agents/x/mcp.json")).unwrap();
    assert!(config["mcpServers"].get("outside").is_none());
}

#[test]
fn codex_name_collisions_disable_both_definitions_or_refuse_a_required_item() {
    for required in [false, true] {
        let fixture = Fixture::new();
        let s = &fixture.session;
        write(
            &s.root().join("codex-home/config.toml"),
            "[mcp_servers.ambient]\nurl='http://127.0.0.1:1/mcp'\n",
        );
        let team = fixture.team(
            RuntimeKind::Codex,
            json!({
                "inherit":{"operator":["mcp"]},
                "mcp":{"ambient":{"command":[CREW,"fake-mcp"],"required":required}}
            }),
        );
        fixture.run(&team);
        if required {
            s.wait("collision refusal", |snapshot| {
                snapshot.agents.iter().any(|agent| {
                    agent.id == "x"
                        && agent
                            .error
                            .as_ref()
                            .is_some_and(|error| error.contains("Rename this item"))
                })
            });
            assert!(!s.root().join("codex-home/fake-codex-server.json").exists());
        } else {
            let record = fixture.record(&s.root().join("codex-home/fake-codex.json"));
            assert!(!record["args"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .any(|arg| arg.starts_with("mcp_servers.ambient.command=")));
        }
        let toolbox = fixture.toolbox();
        assert_eq!(toolbox.items[0].status.code, ToolboxItemCode::Shadowed);
        assert!(toolbox.sources.iter().any(|source| source.status.code
            == crew_core::ToolboxSourceCode::Disabled
            && source.names.contains(&"ambient".into())));
        assert!(!toolbox.sources.iter().any(|source| source.status.code
            == crew_core::ToolboxSourceCode::Used
            && source.names.contains(&"ambient".into())));
    }
}

#[test]
fn codex_project_sources_stay_visible_and_strict_refuses_before_launch() {
    use crew_core::{ToolboxSourceCode, ToolboxSourceOrigin};
    for (kind, config) in [
        ("mcp", "[mcp_servers.project]\ncommand='/bin/false'\n"),
        (
            "subagents",
            "[agents.project]\ndescription='Project role'\nconfig_file='/fixture/role.toml'\n",
        ),
    ] {
        for strict in [false, true] {
            let fixture = Fixture::new();
            let s = &fixture.session;
            seed_codex_project(s, config);
            fixture.run(&fixture.team(
                RuntimeKind::Codex,
                json!({"strict":strict,"inherit":{"project":"none"}}),
            ));
            if strict {
                s.wait("source exclusion refusal", |snapshot| {
                    snapshot.agents.iter().any(|a| {
                        a.id == "x"
                            && a.error
                                .as_ref()
                                .is_some_and(|e| e.contains("toolbox prevents launch"))
                    })
                });
                assert!(!s.root().join("codex-home/fake-codex-server.json").exists());
            } else {
                fixture.record(&s.root().join("codex-home/fake-codex.json"));
            }
            let view = fixture.toolbox();
            assert!(view
                .sources
                .iter()
                .any(|row| row.origin == ToolboxSourceOrigin::Project
                    && row.kind.as_deref() == Some(kind)
                    && row.status.code == ToolboxSourceCode::CannotBeExcluded));
            assert!(!view
                .sources
                .iter()
                .any(|row| row.origin == ToolboxSourceOrigin::Project
                    && row.kind.as_deref() == Some(kind)
                    && row.status.code == ToolboxSourceCode::Disabled));
        }
    }
}

#[test]
fn codex_untrusted_project_names_cannot_merge_into_managed_servers() {
    for name in ["crew", "selected"] {
        let fixture = Fixture::new();
        let s = &fixture.session;
        // Both the stand-in inventory and real Codex's untrusted-project
        // inventory omit this table. It must still reserve its name.
        seed_codex_project(
            s,
            &format!("[mcp_servers.{name}]\nurl='http://127.0.0.1:1/mcp'\n"),
        );
        fixture.run(&fixture.team(
            RuntimeKind::Codex,
            json!({
                "mcp":{"selected":{"command":[CREW,"fake-mcp"]}}
            }),
        ));
        if name == "crew" {
            s.wait("reserved crew name refusal", |snapshot| {
                snapshot.agents.iter().any(|a| {
                    a.id == "x"
                        && a.error
                            .as_ref()
                            .is_some_and(|e| e.contains("MCP server 'crew'"))
                })
            });
            assert!(!s.root().join("codex-home/fake-codex-server.json").exists());
        } else {
            let record = fixture.record(&s.root().join("codex-home/fake-codex.json"));
            assert!(!record["args"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .any(|arg| arg.starts_with("mcp_servers.selected.")));
            assert_eq!(
                fixture.toolbox().items[0].status.code,
                ToolboxItemCode::Shadowed
            );
        }
    }
}

#[test]
fn checking_another_file_uses_its_sandbox_and_delivery_without_changing_the_member() {
    let fixture = Fixture::new();
    let s = &fixture.session;
    let mut live = fixture.team(RuntimeKind::Shell, json!({}));
    live.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    live.agents[0].command = scripted(&[]);
    fixture.run(&live);
    s.wait("live shell", |snapshot| {
        snapshot
            .agents
            .iter()
            .any(|agent| agent.id == "x" && agent.toolbox.is_some())
    });
    let role = s.root().join("role.md");
    write(
        &role,
        "---\nname: role\ndescription: Role fixture.\n---\nInstructions.\n",
    );
    let mut candidate = fixture.team(
        RuntimeKind::Codex,
        json!({"subagents":{"role":{"path":role,"required":true}}}),
    );
    candidate.workspace.sandbox.mode = SandboxMode::Required;
    write(
        &s.root().join("candidate.yaml"),
        &candidate.to_yaml().unwrap(),
    );
    let before = fixture.toolbox();
    let checked: Value = serde_json::from_str(&s.ok(&[
        "toolbox",
        "check",
        "candidate.yaml",
        "--json",
        "-C",
        s.workspace.to_str().unwrap(),
    ]))
    .unwrap();
    assert_eq!(
        checked[0]["toolbox"]["items"][0]["status"]["code"],
        "planned"
    );
    assert_eq!(fixture.toolbox().fingerprint, before.fingerprint);
    assert!(!s
        .workspace
        .join(".crew/agents/x/rw/home/codex/agents/role.toml")
        .exists());
}
