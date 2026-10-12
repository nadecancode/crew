//! A finished card is distilled into memories by the model `memory.extract`
//! names, through Claude Code's print mode: here the fake `claude`
//! (`crew fake-cli`), scripted to answer with two memories. Each test puts
//! its own fakes first on the PATH its daemon is started with; nothing
//! here touches the process's own.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::*;
use crew_core::config::TeamConfig;
use crew_core::{AgentStatus, MemoryKind, TaskStatus};
use serde_json::Value;

/// The shell line of a `claude` that answers print mode with `reply`, and
/// keeps its files in `config`.
fn print_shim(s: &Session, reply: &Path, config: &Path) -> String {
    format!(
        "CLAUDE_CONFIG_DIR='{}' CREW_FAKE_CLAUDE_PRINT_FILE='{}' exec {} fake-cli claude \"$@\"",
        config.display(),
        reply.display(),
        s.sandboxed_crew()
    )
}

/// `scripts` installed as commands in the test's own `bin`: the PATH that
/// finds them first.
fn install(s: &Session, scripts: &[(&str, String)]) -> String {
    let bin = s.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, script) in scripts {
        let path = bin.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// `crew run` of `team`, with the daemon (and so the model it spawns)
/// finding commands on `path`.
fn start(s: &Session, path: &str, team: &TeamConfig) {
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("PATH", path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .expect("running crew");
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn a_finished_card_is_distilled_into_memories_by_the_model() {
    let s = Session::new();
    let ws = s.workspace.to_string_lossy().into_owned();
    let scratch = tempfile::tempdir().unwrap();
    let reply = scratch.path().join("reply.json");
    // As Claude Code's print mode wraps an answer, with the model's prose
    // and fence around the JSON.
    std::fs::write(
        &reply,
        serde_json::json!({
            "type": "result",
            "result": "Here:\n```json\n{\"memories\": [\
{\"name\": \"docs-host-quill\", \"description\": \"Docs that leave the repo go to Quill\", \"kind\": \"decision\", \"body\": \"Chosen over Notion during card first: it has an API we can test against.\"}, \
{\"name\": \"Bad Name!\", \"description\": \"dropped\", \"body\": \"an invalid name is not kept\"}, \
{\"name\": \"leak\", \"description\": \"dropped too\", \"body\": \"token ghp_abcdefghijklmnopqrstuvwxyz012345\"}, \
{\"name\": \"ci-flaky-under-load\", \"description\": \"The e2e suite is load-sensitive; rerun failures alone\", \"kind\": \"pitfall\", \"body\": \"Three tests timed out under load and passed alone.\"}\
]}\n```"
        })
        .to_string(),
    )
    .unwrap();
    let config = scratch.path().join("claude-config");
    let path = install(
        &s,
        &[(
            "claude",
            format!("#!/bin/sh\n{}\n", print_shim(&s, &reply, &config)),
        )],
    );

    let mut team = s.team(
        "distill",
        vec![agent("a", "builds", &["--on-task=done built {task}"])],
        vec![task("first", "a", &[])],
    );
    // The default, which the test harness turns off.
    team.memory.extract = crew_core::config::MemoryConfig::default().extract;
    start(&s, &path, &team);
    let done = s.wait_all_done();
    assert_eq!(find_task(&done, "first").status, TaskStatus::Done);
    s.wait_for("the card to be distilled", || {
        s.events()
            .iter()
            .filter(|e| e.event_type == "memory.added" && e.message.contains("distilled"))
            .count()
            == 2
    });
    assert!(!s.has_event("memory.distill_failed"));

    // What the model was asked: the card, by its member, through print
    // mode on the model the team names, with nothing of the operator's.
    let seen: Value = serde_json::from_str(
        &std::fs::read_to_string(config.join("fake-claude-print.json")).unwrap(),
    )
    .unwrap();
    let args: Vec<&str> = seen["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(args[0], "-p");
    let prompt = args[1];
    assert!(
        prompt.contains("Card first: task first")
            && prompt.contains("Done by: a")
            && prompt.contains("Result: built first"),
        "{prompt}"
    );
    let at = args.iter().position(|a| *a == "--model").unwrap();
    assert_eq!(args[at + 1], "haiku");
    assert!(args.contains(&"--no-session-persistence") && args.contains(&"--strict-mcp-config"));
    assert_eq!(seen["env"]["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1");
    assert!(seen["env"].get("CREW_IDENTITY").is_none());

    // The two good memories, crew's, pointing at the card; the bad name
    // and the token dropped.
    let memories: Vec<crew_core::Memory> =
        serde_json::from_str(&s.ok(&["memory", "list", "--json", "-C", &ws])).unwrap();
    let mut names: Vec<&str> = memories.iter().map(|m| m.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["ci-flaky-under-load", "docs-host-quill"]);
    let quill = memories
        .iter()
        .find(|m| m.name == "docs-host-quill")
        .unwrap();
    assert_eq!(quill.by, "crew");
    assert_eq!(quill.kind, MemoryKind::Decision);
    assert_eq!(quill.source.as_deref(), Some("first"));
    let pitfall = memories
        .iter()
        .find(|m| m.name == "ci-flaky-under-load")
        .unwrap();
    assert_eq!(pitfall.kind, MemoryKind::Pitfall);
    // Claude Code's index lists them.
    let index = std::fs::read_to_string(s.workspace.join(".crew/memory/MEMORY.md")).unwrap();
    assert!(
        index.contains("- [docs-host-quill](docs-host-quill.md) — Docs that leave the repo go to Quill (decision, crew, "),
        "{index}"
    );
    assert_eq!(find_agent(&done, "a").status, AgentStatus::Done);
}

/// A member moved to another CLI mid-card leaves its transcript behind on
/// the CLI it came from: crew distils its work so far before the move, so
/// its fresh session on the other CLI — and every later one — has it.
#[test]
fn a_member_moved_to_another_cli_has_its_work_so_far_distilled_first() {
    let s = Session::new();
    let root = s.root().to_path_buf();
    let ws = s.workspace.to_string_lossy().into_owned();
    let scratch = tempfile::tempdir().unwrap();
    let reply = scratch.path().join("reply.json");
    std::fs::write(
        &reply,
        serde_json::json!({
            "type": "result",
            "result": "{\"memories\": [{\"name\": \"quill-empty-title\", \"description\": \"Quill's API rejects an empty title; send a placeholder\", \"kind\": \"pitfall\", \"body\": \"A POST with an empty title is a 422. Send untitled and rename afterwards.\"}]}"
        })
        .to_string(),
    )
    .unwrap();
    let config = scratch.path().join("claude-config");
    // `claude` is the member's session and, in print mode, the model that
    // distils; `codex` is where the member is moved to.
    let claude = format!(
        "#!/bin/sh\ncase \"$1\" in -p) {} ;; esac\n{}",
        print_shim(&s, &reply, &config),
        FAKE_RUNTIME.strip_prefix("#!/bin/sh\n").unwrap()
    );
    let path = install(
        &s,
        &[
            ("claude", claude),
            ("codex", runtime_script("codex", FAKE_RUNTIME)),
        ],
    );

    let mut team = s.team("handoff", vec![], vec![]);
    team.memory.extract = crew_core::config::MemoryConfig::default().extract;
    team.agents.push(crew_core::config::AgentSpec {
        id: "a".to_string(),
        runtime: Some(crew_core::RuntimeKind::Claude),
        model: Some("fable".to_string()),
        task: Some("design the thing".to_string()),
        ..crew_core::config::AgentSpec::default()
    });
    start(&s, &path, &team);
    s.wait("a to come up", |snap| {
        snap.agents.len() == 1 && snap.agents[0].status != AgentStatus::Spawning
    });
    // What it has said so far, in the session file its hooks report.
    let transcript = root.join("a.jsonl");
    let said = "Quill's API rejects an empty title: I send \"untitled\" and rename afterwards.";
    let reply = serde_json::json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": said }] } });
    std::fs::write(&transcript, format!("{reply}\n")).unwrap();
    let session = find_agent(&s.snapshot(), "a").session_id.clone().unwrap();
    let payload =
        serde_json::json!({ "session_id": session, "transcript_path": transcript }).to_string();
    let out = s.crew(&[
        "hook",
        "session-start",
        "--identity",
        &s.identity_path("a").to_string_lossy(),
        &payload,
    ]);
    assert!(out.status.success(), "{}", stderr(&out));

    let out = s
        .command()
        .env("PATH", &path)
        .args(["agent", "model", "a", "--runtime", "codex", "-C", &ws])
        .output()
        .expect("running crew");
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait_for("the work so far to be distilled", || {
        s.events()
            .iter()
            .any(|e| e.event_type == "memory.added" && e.message.contains("distilled"))
    });
    assert!(!s.has_event("memory.distill_failed"), "{:?}", s.events());
    let added = s
        .events()
        .into_iter()
        .find(|e| e.event_type == "memory.added")
        .unwrap();
    assert!(
        added.message.starts_with(
            "crew distilled quill-empty-title (pitfall) from a's work so far on card a-task, \
before its move to Codex on its default model: "
        ),
        "{}",
        added.message
    );

    // What the model was asked: the move, the card it was on, and the
    // transcript it leaves behind.
    let seen: Value = serde_json::from_str(
        &std::fs::read_to_string(config.join("fake-claude-print.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(seen["args"][0], "-p", "{seen}");
    let prompt = seen["args"][1].as_str().unwrap();
    for part in [
        "being moved from Claude Code on Fable 5.1 to Codex on its default model",
        "Card a-task: design the thing (in progress, handed over with the move)",
        "Worked on by: a",
        said,
    ] {
        assert!(prompt.contains(part), "missing {part:?} in\n{prompt}");
    }
    assert!(!prompt.contains("Result:"), "{prompt}");

    // Kept as crew's, with the card as its source, where the fresh session
    // on Codex reads it.
    let memories: Vec<crew_core::Memory> =
        serde_json::from_str(&s.ok(&["memory", "list", "--json", "-C", &ws])).unwrap();
    assert_eq!(memories.len(), 1, "{memories:?}");
    assert_eq!(
        (
            memories[0].name.as_str(),
            memories[0].by.as_str(),
            memories[0].kind,
            memories[0].source.as_deref()
        ),
        (
            "quill-empty-title",
            "crew",
            MemoryKind::Pitfall,
            Some("a-task")
        )
    );
    let snap = s.wait("a to be back, on codex", |snap| {
        let a = find_agent(snap, "a");
        a.runtime == crew_core::RuntimeKind::Codex && a.status != AgentStatus::Spawning
    });
    assert!(!find_task(&snap, "a-task").status.is_terminal());
}

#[test]
fn a_team_with_extraction_off_asks_no_model() {
    let s = Session::new();
    let ws = s.workspace.to_string_lossy().into_owned();
    let scratch = tempfile::tempdir().unwrap();
    let reply = scratch.path().join("reply.json");
    std::fs::write(
        &reply,
        r#"{"memories":[{"name":"x","description":"y","body":"z"}]}"#,
    )
    .unwrap();
    let config = scratch.path().join("claude-config");
    let path = install(
        &s,
        &[(
            "claude",
            format!("#!/bin/sh\n{}\n", print_shim(&s, &reply, &config)),
        )],
    );
    let mut team = s.team(
        "no-distill",
        vec![agent("a", "builds", &["--on-task=done built {task}"])],
        vec![task("first", "a", &[])],
    );
    team.memory.extract = "off".into();
    start(&s, &path, &team);
    s.wait_all_done();
    // Long enough for a distillation to have happened, had there been one.
    s.wait_for("the member to rest", || {
        find_agent(&s.snapshot(), "a").status == AgentStatus::Done
    });
    // The daemon's own `claude -p /usage` may have run; no distillation did.
    if let Ok(seen) = std::fs::read_to_string(config.join("fake-claude-print.json")) {
        let seen: Value = serde_json::from_str(&seen).unwrap();
        assert_eq!(seen["args"][1], "/usage", "{seen}");
    }
    assert!(!s.has_event("memory.added") && !s.has_event("memory.distill_failed"));
    assert!(s
        .ok(&["memory", "list", "-C", &ws])
        .contains("nothing is remembered yet"));
}
