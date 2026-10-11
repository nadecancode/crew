//! A finished card is distilled into memories by the model `memory.extract`
//! names, through Claude Code's print mode: here the fake `claude`
//! (`crew fake-cli`), scripted to answer with two memories. Its own process,
//! since it puts the fake on PATH.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::*;
use crew_core::{AgentStatus, MemoryKind, TaskStatus};
use serde_json::Value;

/// A `claude` on PATH that answers print mode with `reply`, and keeps its
/// files in `config`.
fn fake_claude(s: &Session, reply: &Path, config: &Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir_in(Path::new(CREW).parent().unwrap()).unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("claude");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nCLAUDE_CONFIG_DIR='{}' CREW_FAKE_CLAUDE_PRINT_FILE='{}' exec {} fake-cli claude \"$@\"\n",
            config.display(),
            reply.display(),
            s.sandboxed_crew()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
    dir
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
    let _fake = fake_claude(&s, &reply, &config);

    let mut team = s.team(
        "distill",
        vec![agent("a", "builds", &["--on-task=done built {task}"])],
        vec![task("first", "a", &[])],
    );
    // The default, which the test harness turns off.
    team.memory.extract = crew_core::config::MemoryConfig::default().extract;
    s.run(&team);
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
    let _fake = fake_claude(&s, &reply, &config);
    let mut team = s.team(
        "no-distill",
        vec![agent("a", "builds", &["--on-task=done built {task}"])],
        vec![task("first", "a", &[])],
    );
    team.memory.extract = "off".into();
    s.run(&team);
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
