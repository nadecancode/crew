//! End-to-end tests: a real daemon, real agent processes on PTYs, the real
//! bus, scheduler and git worktrees. The agents are scripted, so none of this
//! needs Claude Code or Codex (see `tests/real_cli.rs` for that).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;

use common::*;
use crew_core::config::{AgentSpec, OrchestratorConfig, TaskSpec};
use crew_core::{AgentStatus, Message, MessageType, RuntimeKind, TaskStatus};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Messaging
// ---------------------------------------------------------------------------

#[test]
fn direct_messages_go_peer_to_peer_and_the_recipient_reacts() {
    let s = Session::new();
    let team = s.team(
        "p2p",
        vec![
            agent(
                "asker",
                "asks",
                &[
                    "--on-task=send answerer question what is the schema?",
                    "--on-task=status waiting on answerer",
                    "--on-task=wait-message answerer",
                    "--on-task=done the answer was: {body}",
                ],
            ),
            agent("answerer", "answers", &["--on-message=reply three tables"]),
        ],
        vec![task("ask", "asker", &[])],
    );
    s.run(&team);
    let snapshot = s.wait_all_done();

    // The reply reached the asker's session and it acted on it.
    assert_eq!(
        find_task(&snapshot, "ask").result.as_deref(),
        Some("the answer was: three tables")
    );

    let messages = s.messages();
    let question = messages
        .iter()
        .find(|m| m.kind == MessageType::Question)
        .expect("the question");
    assert_eq!(
        (question.from.as_str(), question.to.as_str()),
        ("asker", "answerer")
    );
    assert!(question.is_p2p());
    let answer = messages
        .iter()
        .find(|m| m.kind == MessageType::Answer)
        .expect("the answer");
    assert_eq!(
        (answer.from.as_str(), answer.to.as_str()),
        ("answerer", "asker")
    );
    assert!(answer.is_p2p());

    // Nothing was relayed: the only orchestrator traffic is the task dispatch.
    let via_orchestrator: Vec<_> = messages
        .iter()
        .filter(|m| m.from == "orchestrator" || m.to == "orchestrator")
        .collect();
    assert_eq!(via_orchestrator.len(), 1, "{via_orchestrator:?}");
    assert_eq!(via_orchestrator[0].kind, MessageType::Task);

    // Both deliveries went into the recipients' sessions (logged once
    // each delivery is confirmed, which can come a moment later).
    s.wait_for("both deliveries to be logged", || {
        let delivered: Vec<_> = s
            .events()
            .into_iter()
            .filter(|e| e.event_type == "message.delivered")
            .filter_map(|e| e.agent_id)
            .collect();
        delivered.contains(&"answerer".to_string()) && delivered.contains(&"asker".to_string())
    });
}

#[test]
fn a_broadcast_reaches_every_other_agent_exactly_once() {
    let s = Session::new();
    let team = s.team(
        "broadcast",
        vec![
            agent(
                "lead",
                "",
                &[
                    "--on-task=broadcast freeze the API",
                    "--on-task=done announced",
                ],
            ),
            agent("b", "", &["--on-message=reply b heard: {body}"]),
            agent("c", "", &["--on-message=reply c heard: {body}"]),
        ],
        vec![task("announce", "lead", &[])],
    );
    s.run(&team);
    s.wait_all_done();
    s.wait_for("both replies", || {
        s.messages()
            .iter()
            .filter(|m| m.kind == MessageType::Answer)
            .count()
            == 2
    });
    let messages = s.messages();
    // One broadcast row, addressed to everyone.
    let broadcasts: Vec<_> = messages.iter().filter(|m| m.is_broadcast()).collect();
    assert_eq!(broadcasts.len(), 1);
    assert_eq!(broadcasts[0].from, "lead");
    assert!(!broadcasts[0].is_p2p());
    let mut repliers: Vec<&str> = messages
        .iter()
        .filter(|m| m.kind == MessageType::Answer)
        .map(|m| m.from.as_str())
        .collect();
    repliers.sort_unstable();
    // The sender did not receive its own broadcast.
    assert_eq!(repliers, ["b", "c"]);
    let snapshot = s.snapshot();
    assert_eq!(
        find_agent(&snapshot, "lead").inbox_total,
        3,
        "task + two replies"
    );
    assert_eq!(find_agent(&snapshot, "b").inbox_total, 1);
}

#[test]
fn a_resent_message_id_is_delivered_once_and_identities_cannot_be_forged() {
    let s = Session::new();
    let team = s.team(
        "dedup",
        vec![
            agent("a", "", &[]),
            agent("b", "", &["--on-message=reply ack"]),
        ],
        vec![],
    );
    s.run(&team);
    s.wait("agents to come up", |snap| {
        snap.agents
            .iter()
            .all(|a| a.status != AgentStatus::Spawning)
    });

    let args = json!({ "to": "b", "body": "run the migration", "id": "retry-1" }).to_string();
    let first = s.ctl("a", &["tool", "agent_send", &args]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(
        stdout(&first).contains("Delivered to b"),
        "{}",
        stdout(&first)
    );
    // A retry with the same id (say, after a timeout) must not deliver twice.
    let second = s.ctl("a", &["tool", "agent_send", &args]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(
        stdout(&second).contains("Already sent"),
        "{}",
        stdout(&second)
    );

    s.wait_for("b's reply", || {
        s.messages().iter().any(|m| m.kind == MessageType::Answer)
    });
    std::thread::sleep(std::time::Duration::from_millis(400));
    let messages = s.messages();
    assert_eq!(messages.iter().filter(|m| m.id == "retry-1").count(), 1);
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.kind == MessageType::Answer)
            .count(),
        1,
        "b reacted once"
    );

    // An agent cannot reuse another sender's message id...
    let spoof = s.ctl(
        "b",
        &[
            "tool",
            "agent_send",
            &json!({ "to": "a", "body": "x", "id": "retry-1" }).to_string(),
        ],
    );
    assert!(!spoof.status.success());
    // ...nor speak as another agent: b's token does not open a's identity.
    let b: Value =
        serde_json::from_str(&std::fs::read_to_string(s.identity_path("b")).unwrap()).unwrap();
    let forged = s.root().join("forged.json");
    std::fs::write(
        &forged,
        json!({ "socket": b["socket"], "agent_id": "a", "token": b["token"] }).to_string(),
    )
    .unwrap();
    let impostor = s
        .command()
        .env("CREW_IDENTITY", &forged)
        .args(["ctl", "send", "b", "I am a, honest"])
        .output()
        .unwrap();
    assert!(!impostor.status.success());
    assert!(
        stderr(&impostor).contains("unauthorized"),
        "{}",
        stderr(&impostor)
    );
    assert!(!s.messages().iter().any(|m| m.body.contains("honest")));

    // Oversized bodies are rejected.
    let huge = "x".repeat(70 * 1024);
    let too_big = s.ctl("a", &["send", "b", &huge]);
    assert!(!too_big.status.success());
    // Unknown recipients are an error, not a silent drop.
    let nobody = s.ctl("a", &["send", "nobody", "hello"]);
    assert!(!nobody.status.success());
    assert!(
        stderr(&nobody).contains("no agent 'nobody'"),
        "{}",
        stderr(&nobody)
    );
}

#[test]
fn messages_can_be_pushed_over_the_socket_instead_of_typed() {
    let s = Session::new();
    let team = s.team(
        "socket",
        vec![agent("a", "", &[]), agent("b", "", &[])],
        vec![],
    );
    s.run(&team);
    s.wait("agents to come up", |snap| {
        snap.agents
            .iter()
            .all(|a| a.status != AgentStatus::Spawning)
    });

    // `crew bridge` is what a runtime-side mod runs: it holds the agent's
    // delivery channel open and prints one JSON line per delivery.
    let mut bridge = s
        .command()
        .args(["bridge", "--identity"])
        .arg(s.identity_path("b"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(bridge.stdout.take().unwrap()).lines();
    s.wait("b's socket channel", |snap| {
        find_agent(snap, "b").delivery == "socket"
    });

    s.send("b", "hello over the socket");
    let line = lines.next().expect("a delivery line").unwrap();
    let delivery: Value = serde_json::from_str(&line).unwrap();
    assert!(delivery["text"]
        .as_str()
        .unwrap()
        .contains("hello over the socket"));
    let id = delivery["ids"][0].as_str().unwrap().to_string();

    // Nothing was typed into b's terminal.
    let screen = s.agent_cmd(&["output", "b"]);
    assert!(!screen.contains("new message"), "{screen}");
    // Until the runtime confirms, the message is not marked delivered.
    assert!(!s
        .events()
        .iter()
        .any(|e| e.event_type == "message.delivered" && e.agent_id.as_deref() == Some("b")));

    let confirmed = s
        .command()
        .args(["delivered", "--identity"])
        .arg(s.identity_path("b"))
        .arg(&id)
        .output()
        .unwrap();
    assert!(confirmed.status.success(), "{}", stderr(&confirmed));
    s.wait_for("the delivery to be recorded", || {
        s.events().iter().any(|e| {
            e.event_type == "message.delivered"
                && e.agent_id.as_deref() == Some("b")
                && e.message.contains("via socket")
        })
    });

    // When the channel closes, delivery falls back to the terminal.
    bridge.kill().unwrap();
    bridge.wait().unwrap();
    s.wait("fallback to the terminal", |snap| {
        find_agent(snap, "b").delivery == "terminal"
    });
    s.send("b", "and now the terminal");
    s.wait_for("terminal delivery", || {
        s.agent_cmd(&["output", "b"]).contains("new message")
    });
}

#[test]
fn the_mcp_bridge_exposes_the_tools_and_sends_as_its_agent() {
    let s = Session::new();
    let team = s.team(
        "mcp",
        vec![agent("a", "", &[]), agent("b", "", &[])],
        vec![],
    );
    s.run(&team);
    s.wait("agents to come up", |snap| {
        snap.agents
            .iter()
            .all(|a| a.status != AgentStatus::Spawning)
    });

    let mut mcp = s
        .command()
        .args(["mcp", "--identity"])
        .arg(s.identity_path("a"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = mcp.stdin.take().unwrap();
    let mut lines = BufReader::new(mcp.stdout.take().unwrap()).lines();
    let mut call = |request: Value| -> Value {
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
        serde_json::from_str(&lines.next().expect("a response").unwrap()).unwrap()
    };

    let init = call(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "crew");

    let tools = call(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "agents_list",
        "agent_send",
        "agent_inbox",
        "agent_status",
        "task_update",
        "task_complete",
        "agent_spawn",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    // A worker is not offered the orchestrator's tools.
    assert!(!names.contains(&"task_create"));
    assert!(!names.contains(&"agent_add"));

    let sent = call(
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"agent_send","arguments":{"to":"b","body":"via mcp","type":"question"}}}),
    );
    assert_eq!(sent["result"]["isError"], false, "{sent}");
    let listed = call(
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"agents_list","arguments":{}}}),
    );
    assert!(listed["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("b ["));
    // The daemon enforces the orchestrator-only tools even if a worker calls them.
    let denied = call(
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"task_create","arguments":{"id":"x","description":"y"}}}),
    );
    assert_eq!(denied["result"]["isError"], true);
    drop(stdin);
    mcp.wait().unwrap();

    let message = s
        .messages()
        .into_iter()
        .find(|m| m.body == "via mcp")
        .expect("the message sent through MCP");
    // The sender is whoever the token says, not something the client chose.
    assert_eq!(message.from, "a");
    assert_eq!(message.kind, MessageType::Question);
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

#[test]
fn a_task_starts_only_after_all_its_dependencies_are_done() {
    let s = Session::new();
    let team = s.team(
        "deps",
        vec![
            agent("a", "", &["--on-task=sleep 300", "--on-task=done a"]),
            agent("b", "", &["--on-task=sleep 1500", "--on-task=done b"]),
            agent("c", "", &["--on-task=done c"]),
        ],
        vec![
            task("ta", "a", &[]),
            task("tb", "b", &[]),
            task("tc", "c", &["ta", "tb"]),
        ],
    );
    s.run(&team);

    // A is done, B is still working: C must not have started.
    let midway = s.wait("ta to finish", |snap| {
        find_task(snap, "ta").status == TaskStatus::Done
    });
    if find_task(&midway, "tb").status != TaskStatus::Done {
        assert_eq!(find_task(&midway, "tc").status, TaskStatus::Queued);
        assert!(find_task(&midway, "tc").started_at.is_none());
    }

    let done = s.wait_all_done();
    let (ta, tb, tc) = (
        find_task(&done, "ta"),
        find_task(&done, "tb"),
        find_task(&done, "tc"),
    );
    let started = tc.started_at.unwrap();
    assert!(started >= ta.completed_at.unwrap());
    assert!(started >= tb.completed_at.unwrap());
    // C's card tells it what its dependencies produced; the dispatch only
    // names the card.
    let dispatch = s
        .messages()
        .into_iter()
        .find(|m| m.to == "c" && m.kind == MessageType::Task)
        .unwrap();
    assert!(
        dispatch.body.ends_with("task_get(\"tc\") reads its card."),
        "{}",
        dispatch.body
    );
    let card = stdout(&s.ctl("c", &["task", "tc"]));
    assert!(
        card.contains("- ta [done] by a: a\n") && card.contains("- tb [done] by b: b\n"),
        "{card}"
    );
    assert!(done.agents.iter().all(|a| a.status == AgentStatus::Done));
}

#[test]
fn a_task_blocked_before_it_started_cannot_be_completed() {
    let s = Session::new();
    let team = s.team(
        "gate",
        vec![
            agent("a", "", &["--on-task=fail the API is down"]),
            agent("b", "", &[]),
            agent(
                "c",
                "",
                &[
                    "--on-task=status blocked on a flaky test",
                    "--on-task=done anyway",
                ],
            ),
        ],
        vec![
            task("dep", "a", &[]),
            task("child", "b", &["dep"]),
            task("own", "c", &[]),
        ],
    );
    s.run(&team);
    s.wait("the dependent to be blocked", |snap| {
        find_task(snap, "child").status == TaskStatus::Blocked
    });
    // Its dependency failed: it never started, so it cannot be done.
    let complete = json!({ "task_id": "child", "result": "done without dep" }).to_string();
    let out = s.ctl("b", &["tool", "task_complete", &complete]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("has not been started"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        find_task(&s.snapshot(), "child").status,
        TaskStatus::Blocked
    );
    // A task its agent blocked while working on it can still be finished.
    s.wait("the self-blocked task to be done", |snap| {
        find_task(snap, "own").status == TaskStatus::Done
    });
}

#[test]
fn a_failed_dependency_blocks_its_dependents_and_is_reported_as_a_stall() {
    let s = Session::new();
    let team = s.team(
        "blocked",
        vec![
            agent("a", "", &["--on-task=fail the API is down"]),
            agent("b", "", &["--on-task=done never runs"]),
        ],
        vec![task("first", "a", &[]), task("second", "b", &["first"])],
    );
    s.run(&team);
    let snapshot = s.wait("the dependent to be blocked", |snap| {
        find_task(snap, "second").status == TaskStatus::Blocked
    });
    let first = find_task(&snapshot, "first");
    assert_eq!(first.status, TaskStatus::Failed);
    assert!(first.note.as_deref().unwrap().contains("the API is down"));
    assert_eq!(
        find_task(&snapshot, "second").note.as_deref(),
        Some("dependency 'first' failed")
    );
    s.wait_for("the stall to be reported", || {
        s.has_event("scheduler.stall")
    });
    assert!(snapshot.session.completed_at.is_none());

    // Retrying the failed task unblocks the graph (it fails again here, but
    // the dependent is re-evaluated rather than stuck forever).
    s.ok(&[
        "task",
        "retry",
        "first",
        "-C",
        &s.workspace.to_string_lossy(),
    ]);
    s.wait_for("the retry", || s.has_event("task.retry"));
}

#[test]
fn a_task_too_long_for_its_task_message_is_refused_at_creation() {
    let s = Session::new();
    // The task message names the card by id and title; what the card says is
    // read with task_get and is not in it.
    let huge = "x".repeat(3000);
    let mut team = s.team(
        "oversize",
        vec![agent("a", "", &[])],
        vec![task("huge", "a", &[])],
    );
    team.limits.max_message_bytes = 2048;
    team.tasks[0].title = Some(huge.clone());
    // In a team file: refused before anything is set up.
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let run = s.crew(&["run", "team.yaml", "--no-attach"]);
    assert!(!run.status.success(), "{}", stdout(&run));
    assert!(
        stderr(&run).contains("limits.max_message_bytes"),
        "{}",
        stderr(&run)
    );
    let nothing = s.snapshot();
    assert!(nothing.agents.is_empty() && nothing.tasks.is_empty());

    // Added to a running team: refused too.
    team.tasks.clear();
    s.run(&team);
    let ws = s.workspace.to_string_lossy().into_owned();
    let added = s.crew(&[
        "task",
        "add",
        "--id",
        "huge",
        "--title",
        &huge,
        "--description",
        "do huge",
        "--agent",
        "a",
        "-C",
        &ws,
    ]);
    assert!(!added.status.success(), "{}", stdout(&added));
    assert!(
        stderr(&added).contains("limits.max_message_bytes"),
        "{}",
        stderr(&added)
    );
    assert!(s.snapshot().tasks.is_empty());

    // A long card is not a long message: its description stays on the board.
    s.ok(&[
        "task",
        "add",
        "--id",
        "long",
        "--title",
        "a long card",
        "--description",
        &huge,
        "--agent",
        "a",
        "-C",
        &ws,
    ]);
    let told = s.wait("the long card to be handed out", |snap| {
        find_task(snap, "long").status == TaskStatus::Working
    });
    assert_eq!(find_task(&told, "long").description, huge);
}

#[test]
fn a_task_whose_task_message_is_refused_fails_instead_of_staying_working() {
    let s = Session::new();
    let team = s.team(
        "undeliverable",
        vec![
            agent(
                "a",
                "",
                &["--on-task=wait-message operator", "--on-task=done first"],
            ),
            agent("b", "", &["--on-task=done never told"]),
        ],
        vec![task("first", "a", &[]), task("second", "b", &["first"])],
    );
    s.run(&team);
    s.wait_for("a to be on first", || {
        s.agent_cmd(&["output", "a"])
            .contains("Task first is yours")
    });
    // The database refuses the dependent's task message when its turn comes.
    let db = rusqlite::Connection::open(s.workspace.join(".crew/crew.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_task_message BEFORE INSERT ON messages \
         WHEN NEW.kind = 'task' AND NEW.recipient = 'b' \
         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END;",
    )
    .unwrap();
    s.send("a", "carry on");
    let snapshot = s.wait("the dependent to fail", |snap| {
        find_task(snap, "second").status == TaskStatus::Failed
    });
    let note = find_task(&snapshot, "second").note.clone().unwrap();
    assert!(note.contains("could not be dispatched"), "{note}");
    assert!(note.contains("the disk is full"), "{note}");
    assert!(find_agent(&snapshot, "b").task.is_none());
}

#[test]
fn progress_and_self_reported_status_reach_the_dashboard_state() {
    let s = Session::new();
    let team = s.team(
        "status",
        vec![agent(
            "a",
            "worker",
            &[
                "--on-task=progress 51 half way there",
                "--on-task=status waiting on the operator",
                "--on-task=wait-message operator",
                "--on-task=status working resuming",
                "--on-task=done finished",
            ],
        )],
        vec![task("t", "a", &[])],
    );
    s.run(&team);
    let waiting = s.wait("the agent to report waiting", |snap| {
        find_agent(snap, "a").status == AgentStatus::Waiting
    });
    let a = find_agent(&waiting, "a");
    assert_eq!(a.progress, Some(51));
    assert_eq!(a.task.as_deref(), Some("task t"));
    assert_eq!(a.activity, "on the operator");
    assert!(a.pid.is_some());
    let t = find_task(&waiting, "t");
    assert_eq!(t.status, TaskStatus::Waiting);
    assert_eq!(t.progress, Some(51));

    s.send("a", "carry on");
    let done = s.wait_all_done();
    let a = find_agent(&done, "a");
    assert_eq!(a.status, AgentStatus::Done);
    assert_eq!(a.progress, Some(100));
    assert!(a.task.is_none());
    let types: Vec<String> = s.events().into_iter().map(|e| e.event_type).collect();
    for expected in [
        "task.dispatched",
        "task.progress",
        "agent.self_status",
        "task.done",
        "session.complete",
    ] {
        assert!(types.contains(&expected.to_string()), "missing {expected}");
    }
}

#[test]
fn a_reviewed_task_is_done_only_after_approval() {
    let s = Session::new();
    let team = {
        let mut team = s.team(
            "review",
            vec![
                agent("author", "", &["--on-task=done first draft"]),
                agent("critic", "", &["--on-review=review approve looks good"]),
            ],
            vec![task("write", "author", &[])],
        );
        team.tasks[0].reviewer = Some("critic".into());
        team
    };
    s.run(&team);
    let done = s.wait_all_done();
    assert_eq!(find_task(&done, "write").status, TaskStatus::Done);
    let messages = s.messages();
    // The request and the verdict went directly between author and reviewer.
    let request = messages
        .iter()
        .find(|m| m.kind == MessageType::ReviewRequest)
        .unwrap();
    assert_eq!(
        (request.from.as_str(), request.to.as_str()),
        ("author", "critic")
    );
    let verdict = messages
        .iter()
        .find(|m| m.kind == MessageType::ReviewResult)
        .unwrap();
    assert_eq!(
        (verdict.from.as_str(), verdict.to.as_str()),
        ("critic", "author")
    );
    let types: Vec<String> = s.events().into_iter().map(|e| e.event_type).collect();
    let review = types.iter().position(|t| t == "task.review").unwrap();
    let approved = types.iter().position(|t| t == "task.approved").unwrap();
    let finished = types.iter().position(|t| t == "task.done").unwrap();
    assert!(review < approved && approved < finished);
}

#[test]
fn a_rejected_task_waits_until_its_author_has_finished_the_next_one() {
    let s = Session::new();
    let team = {
        let mut team = s.team(
            "rework",
            vec![
                agent(
                    "author",
                    "",
                    &["--on-task=wait-message operator", "--on-task=done finished"],
                ),
                agent(
                    "critic",
                    "",
                    &[
                        "--on-review=wait-message operator",
                        r#"--on-review=tool review_submit {"task_id":"first","approve":false,"comments":"please fix"}"#,
                    ],
                ),
            ],
            vec![task("first", "author", &[]), task("second", "author", &[])],
        );
        team.tasks[0].reviewer = Some("critic".into());
        team
    };
    s.run(&team);
    let reached = |agent: &str, text: &str| s.agent_cmd(&["output", agent]).contains(text);
    s.wait_for("the author to be on first", || {
        reached("author", "Task first is yours")
    });
    s.send("author", "continue");
    s.wait_for("the author to be on second", || {
        reached("author", "Task second is yours")
    });
    s.wait_for("the critic to read the request", || {
        reached("critic", "review_request from author")
    });
    s.send("critic", "go");
    s.wait_for("the rejection", || s.has_event("task.changes_requested"));

    // The author is still on second: the rework waits its turn.
    let snapshot = s.snapshot();
    assert_eq!(find_task(&snapshot, "second").status, TaskStatus::Working);
    let first = find_task(&snapshot, "first");
    assert_eq!(first.status, TaskStatus::Ready, "{first:?}");
    assert_eq!(first.assigned_agent.as_deref(), Some("author"));
    assert_eq!(first.result.as_deref(), Some("finished"));

    // task_complete without an id closes the task the author is on, and
    // the rework is handed back afterwards.
    s.send("author", "continue");
    let after = s.wait("second done and first handed back", |snap| {
        find_task(snap, "second").status == TaskStatus::Done
            && find_task(snap, "first").status == TaskStatus::Working
    });
    assert_eq!(
        find_agent(&after, "author").task.as_deref(),
        Some("task first")
    );
}

#[test]
fn a_task_whose_reviewer_is_unavailable_stays_in_review_and_the_orchestrator_is_told() {
    let s = Session::new();
    let mut team = s.team(
        "no-reviewer",
        vec![
            agent(
                "author",
                "",
                &["--on-task=wait-message operator", "--on-task=done draft"],
            ),
            agent("critic", "", &[]),
        ],
        vec![
            task("write", "author", &[]),
            task("publish", "author", &["write"]),
        ],
    );
    team.tasks[0].reviewer = Some("critic".into());
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[]),
        ..OrchestratorConfig::default()
    };
    s.run(&team);
    s.wait_for("the author to be on write", || {
        s.agent_cmd(&["output", "author"])
            .contains("Task write is yours")
    });
    s.agent_cmd(&["kill", "critic"]);
    s.wait("the reviewer to stop", |snap| {
        find_agent(snap, "critic").status == AgentStatus::Stopped
    });
    s.send("author", "continue");
    let snapshot = s.wait("the author to finish", |snap| {
        find_task(snap, "write").status != TaskStatus::Working
    });

    // Nobody approved it, so it is not done and its dependent stays put.
    let write = find_task(&snapshot, "write");
    assert_eq!(write.status, TaskStatus::Review, "{write:?}");
    assert_eq!(find_task(&snapshot, "publish").status, TaskStatus::Queued);
    s.wait_for("the orchestrator to be told", || {
        s.messages().iter().any(|m| {
            m.to == "orchestrator"
                && m.body.contains("Task write")
                && m.body.contains("critic is stopped")
        })
    });
    assert!(s.has_event("task.review_blocked"));
    assert!(!s.has_event("task.done"));
}

#[test]
fn a_task_cannot_be_reviewed_by_its_own_agent() {
    let s = Session::new();
    let mut team = s.team(
        "self-review",
        vec![agent("a", "", &[])],
        vec![task("mine", "a", &[])],
    );
    team.tasks[0].reviewer = Some("a".into());
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let run = s.crew(&["run", "team.yaml", "--no-attach"]);
    assert!(!run.status.success(), "{}", stdout(&run));
    assert!(
        stderr(&run).contains("cannot be reviewed by its own agent"),
        "{}",
        stderr(&run)
    );

    team.tasks.clear();
    s.run(&team);
    let ws = s.workspace.to_string_lossy().into_owned();
    let added = s.crew(&[
        "task",
        "add",
        "--id",
        "mine",
        "--agent",
        "a",
        "--reviewer",
        "a",
        "-C",
        &ws,
    ]);
    assert!(!added.status.success(), "{}", stdout(&added));
    assert!(
        stderr(&added).contains("cannot be reviewed by its own agent"),
        "{}",
        stderr(&added)
    );
}

#[test]
fn a_task_whose_reviewer_stopped_is_reported_as_a_stall() {
    let s = Session::new();
    let team = {
        let mut team = s.team(
            "lost-reviewer",
            vec![
                agent("author", "", &["--on-task=done first draft"]),
                // Takes the review request and never answers it.
                agent("critic", "", &[]),
            ],
            vec![task("write", "author", &[])],
        );
        team.tasks[0].reviewer = Some("critic".into());
        team
    };
    s.run(&team);
    s.wait("the task to await review", |snap| {
        find_task(snap, "write").status == TaskStatus::Review
    });
    s.agent_cmd(&["kill", "critic"]);
    s.wait_for("the stall to be reported", || {
        s.events().iter().any(|e| {
            e.event_type == "scheduler.stall"
                && e.message.contains("'write' awaits review by critic")
        })
    });
    assert_eq!(find_task(&s.snapshot(), "write").status, TaskStatus::Review);
}

#[test]
fn a_reviewer_with_an_open_review_is_restarted_after_a_daemon_restart() {
    let s = Session::new();
    let team = {
        let mut team = s.team(
            "review-recovery",
            vec![
                agent("author", "", &["--on-task=done first draft"]),
                // Reads the review request; the verdict is still to come.
                agent("critic", "", &[]),
            ],
            vec![task("write", "author", &[])],
        );
        team.tasks[0].reviewer = Some("critic".into());
        team
    };
    s.run(&team);
    let before = s.wait("the task to await review", |snap| {
        find_task(snap, "write").status == TaskStatus::Review
    });
    s.wait_for("the reviewer to read the request", || {
        s.agent_cmd(&["output", "critic"])
            .contains("review_request from author")
    });

    let old_pid = before.session.daemon_pid;
    let killed = std::process::Command::new("kill")
        .args(["-9", &old_pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    s.wait_for("the daemon to die", || {
        !crew_core::paths::pid_alive(old_pid)
    });
    s.ok(&["start", "-C", &s.workspace.to_string_lossy()]);
    s.wait_for("the recovery", || s.has_event("daemon.recovered"));

    // The review is still owed, so its reviewer comes back for it.
    let after = s.snapshot();
    let critic = find_agent(&after, "critic");
    assert_ne!(critic.status, AgentStatus::Stopped, "{critic:?}");
    assert_eq!(find_task(&after, "write").status, TaskStatus::Review);
    s.wait("the reviewer to be running again", |snap| {
        find_agent(snap, "critic").pid.is_some()
    });
}

// ---------------------------------------------------------------------------
// Agent lifecycle and failures
// ---------------------------------------------------------------------------

#[test]
fn agents_can_be_paused_killed_and_restarted() {
    let s = Session::new();
    let team = s.team(
        "lifecycle",
        vec![agent(
            "a",
            "",
            &[
                "--on-task=progress 10 started",
                "--on-task=wait-message operator",
                "--on-task=done finished after the restart",
            ],
        )],
        vec![task("t", "a", &[])],
    );
    s.run(&team);
    let running = s.wait("the task to start", |snap| {
        find_agent(snap, "a").progress == Some(10)
    });
    let first_pid = find_agent(&running, "a").pid.unwrap();

    s.agent_cmd(&["pause", "a"]);
    assert!(find_agent(&s.snapshot(), "a").paused);
    s.agent_cmd(&["resume", "a"]);
    assert!(!find_agent(&s.snapshot(), "a").paused);

    s.agent_cmd(&["kill", "a"]);
    let killed = s.wait("the agent to stop", |snap| {
        find_agent(snap, "a").status == AgentStatus::Stopped
    });
    assert!(find_agent(&killed, "a").pid.is_none());
    // Its task is parked, not lost and not failed.
    assert_eq!(find_task(&killed, "t").status, TaskStatus::Blocked);
    assert!(!crew_core::paths::pid_alive(first_pid));

    s.agent_cmd(&["restart", "a"]);
    let restarted = s.wait("the task to be handed out again", |snap| {
        let a = find_agent(snap, "a");
        a.pid.is_some()
            && a.pid != Some(first_pid)
            && find_task(snap, "t").status == TaskStatus::Working
    });
    assert_ne!(find_agent(&restarted, "a").status, AgentStatus::Stopped);
    s.wait_for("the restarted agent to reach its wait", || {
        s.agent_cmd(&["output", "a"])
            .matches("Progress recorded")
            .count()
            >= 1
            && find_agent(&s.snapshot(), "a").progress == Some(10)
    });
    s.send("a", "go on");
    let done = s.wait_all_done();
    assert_eq!(
        find_task(&done, "t").result.as_deref(),
        Some("finished after the restart")
    );
}

#[test]
fn a_crashed_agent_is_restarted_and_a_repeat_offender_fails_its_task() {
    let s = Session::new();
    let team = s.team(
        "crash",
        vec![
            // Crashes on its first attempt, succeeds after the restart.
            agent(
                "flaky",
                "",
                &[
                    "--on-task=crash-once crashed.marker",
                    "--on-task=done recovered",
                ],
            ),
            // Crashes every time: one restart, then it is given up on.
            agent("doomed", "", &["--on-task=exit 7"]),
            agent("waiter", "", &["--on-task=done never"]),
        ],
        vec![
            task("flaky-task", "flaky", &[]),
            task("doomed-task", "doomed", &[]),
            task("after-doomed", "waiter", &["doomed-task"]),
        ],
    );
    s.run(&team);
    let snapshot = s.wait("both outcomes", |snap| {
        find_task(snap, "flaky-task").status == TaskStatus::Done
            && find_agent(snap, "doomed").status == AgentStatus::Failed
            && find_task(snap, "after-doomed").status == TaskStatus::Blocked
    });

    let flaky = find_agent(&snapshot, "flaky");
    assert_eq!(flaky.restarts, 1);
    assert_eq!(flaky.status, AgentStatus::Done);
    assert_eq!(
        find_task(&snapshot, "flaky-task").result.as_deref(),
        Some("recovered")
    );

    let doomed = find_agent(&snapshot, "doomed");
    assert_eq!(doomed.restarts, 1, "restart budget is one");
    assert_eq!(doomed.exit_code, Some(7));
    assert!(doomed
        .error
        .as_deref()
        .unwrap()
        .contains("exited with code 7"));
    assert!(doomed.pid.is_none());
    let failed = find_task(&snapshot, "doomed-task");
    assert_eq!(failed.status, TaskStatus::Failed);
    assert!(failed
        .note
        .as_deref()
        .unwrap()
        .contains("agent doomed failed"));

    let events = s.events();
    let crashes = events
        .iter()
        .filter(|e| e.event_type == "agent.crashed")
        .count();
    assert_eq!(crashes, 3, "flaky once, doomed twice");
    assert!(events.iter().any(|e| e.event_type == "agent.restarting"));
    // The status change says why the agent failed.
    assert!(events.iter().any(|e| e.event_type == "agent.status"
        && e.message.starts_with("doomed: ")
        && e.message.contains("→ failed (")
        && e.message.contains("exited with code 7")));
}

#[test]
fn the_daemon_recovers_its_state_after_being_killed() {
    let s = Session::new();
    let team = s.team(
        "recovery",
        vec![
            agent("quick", "", &["--on-task=done quick work"]),
            agent(
                "slow",
                "",
                &[
                    "--on-task=progress 30 before the crash",
                    "--on-task=wait-message operator",
                    "--on-task=done survived the daemon restart",
                ],
            ),
        ],
        vec![
            task("quick-task", "quick", &[]),
            task("slow-task", "slow", &[]),
        ],
    );
    s.run(&team);
    let before = s.wait("the first task and the slow agent's progress", |snap| {
        find_task(snap, "quick-task").status == TaskStatus::Done
            && find_agent(snap, "slow").progress == Some(30)
    });
    let old_pid = before.session.daemon_pid;
    let events_before = s.events().len();
    let messages_before = s.messages();

    // Kill the daemon the hard way: no shutdown, no cleanup.
    let killed = std::process::Command::new("kill")
        .args(["-9", &old_pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    s.wait_for("the daemon to die", || {
        !crew_core::paths::pid_alive(old_pid)
    });

    // A new daemon picks the session up from the database.
    s.ok(&["start", "-C", &s.workspace.to_string_lossy()]);
    let after = s.wait("the recovered session", |snap| {
        snap.session.project.as_deref() == Some("recovery")
            && find_task(snap, "slow-task").status == TaskStatus::Working
            && find_agent(snap, "slow").pid.is_some()
            && find_agent(snap, "slow").progress == Some(30)
    });
    assert_ne!(after.session.daemon_pid, old_pid);
    // Finished work stayed finished.
    let quick = find_task(&after, "quick-task");
    assert_eq!(quick.status, TaskStatus::Done);
    assert_eq!(quick.result.as_deref(), Some("quick work"));
    // Message history and the event log survived.
    let messages_after = s.messages();
    for m in &messages_before {
        assert!(
            messages_after.iter().any(|n| n.id == m.id),
            "lost message {}",
            m.id
        );
    }
    let events = s.events();
    assert!(events.len() > events_before);
    assert!(events.iter().any(|e| e.event_type == "daemon.recovered"));
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == "daemon.started")
            .count(),
        2
    );
    assert!(events
        .iter()
        .any(|e| e.event_type == "task.done" && e.task_id.as_deref() == Some("quick-task")));

    // The team can finish the job after the restart.
    s.wait_for("the restarted agent to be waiting again", || {
        s.agent_cmd(&["output", "slow"])
            .contains("Progress recorded")
    });
    s.send("slow", "continue");
    let done = s.wait_all_done();
    assert_eq!(
        find_task(&done, "slow-task").result.as_deref(),
        Some("survived the daemon restart")
    );
}

#[test]
fn a_task_transition_the_database_refuses_is_an_error_and_not_applied() {
    let s = Session::new();
    let team = s.team(
        "durable",
        vec![agent("author", "", &["--on-task=wait-message operator"])],
        vec![task("write", "author", &[])],
    );
    s.run(&team);
    s.wait_for("the author to be on write", || {
        s.agent_cmd(&["output", "author"])
            .contains("Task write is yours")
    });
    let stored_status = || -> String {
        let db = rusqlite::Connection::open(s.workspace.join(".crew/crew.db")).unwrap();
        let data: String = db
            .query_row("SELECT data FROM tasks WHERE id = 'write'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let task: Value = serde_json::from_str(&data).unwrap();
        task["status"].as_str().unwrap().to_string()
    };
    let db = rusqlite::Connection::open(s.workspace.join(".crew/crew.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_task_writes BEFORE UPDATE ON tasks \
         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END;",
    )
    .unwrap();

    let complete = json!({ "result": "finished" }).to_string();
    let refused = s.ctl("author", &["tool", "task_complete", &complete]);
    assert!(!refused.status.success(), "{}", stdout(&refused));
    assert!(
        stderr(&refused).contains("the disk is full"),
        "{}",
        stderr(&refused)
    );
    // What the daemon shows is what it stored.
    assert_eq!(stored_status(), "working");
    assert_eq!(
        find_task(&s.snapshot(), "write").status,
        TaskStatus::Working
    );

    db.execute_batch("DROP TRIGGER refuse_task_writes;")
        .unwrap();
    let done = s.ctl("author", &["tool", "task_complete", &complete]);
    assert!(done.status.success(), "{}", stderr(&done));
    assert_eq!(stored_status(), "done");
    assert_eq!(find_task(&s.snapshot(), "write").status, TaskStatus::Done);
}

#[test]
fn a_second_daemon_for_the_same_workspace_is_refused_and_a_team_cannot_be_loaded_twice() {
    let s = Session::new();
    let team = s.team("single", vec![agent("a", "", &[])], vec![]);
    s.run(&team);
    let ws = s.workspace.to_string_lossy().into_owned();
    let second = s.crew(&["daemon", "--workspace", &ws]);
    assert!(!second.status.success());
    assert!(
        stderr(&second).contains("already running"),
        "{}",
        stderr(&second)
    );
    let again = s.crew(&["run", "team.yaml", "--no-attach"]);
    assert!(!again.status.success());
    assert!(
        stderr(&again).contains("already running"),
        "{}",
        stderr(&again)
    );
    // The session is discoverable by project name from anywhere.
    let sessions = s.ok(&["sessions"]);
    assert!(sessions.contains("single"), "{sessions}");
    let status = s.ok(&["status", "--project", "single"]);
    assert!(status.contains("project    single"));
}

#[test]
fn two_teams_loaded_at_once_do_not_merge() {
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    let ws = s.workspace.to_string_lossy().into_owned();
    s.ok(&["start", "-C", &ws]);
    let runs: Vec<_> = (0..2)
        .map(|n| {
            let mut team = s.team(
                &format!("load-{n}"),
                vec![agent(&format!("worker-{n}"), "", &[])],
                vec![],
            );
            // No repository to create: the two loads meet in the daemon,
            // not in `git init`.
            team.workspace.init = false;
            let file = s.root().join(format!("team-{n}.yaml"));
            std::fs::write(&file, team.to_yaml().unwrap()).unwrap();
            s.command()
                .args(["run", &file.to_string_lossy(), "--no-attach"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outputs: Vec<_> = runs
        .into_iter()
        .map(|run| run.wait_with_output().unwrap())
        .collect();

    let won: Vec<usize> = (0..2).filter(|n| outputs[*n].status.success()).collect();
    assert_eq!(won.len(), 1, "exactly one load succeeds: {outputs:?}");
    let (winner, loser) = (won[0], 1 - won[0]);
    assert!(
        stderr(&outputs[loser]).contains("already running"),
        "{}",
        stderr(&outputs[loser])
    );
    let snapshot = s.snapshot();
    let project = format!("load-{winner}");
    assert_eq!(snapshot.session.project.as_deref(), Some(project.as_str()));
    let members: Vec<&str> = snapshot.agents.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(members, [format!("worker-{winner}")]);
}

#[test]
fn two_agents_added_at_once_under_one_id_are_one_agent() {
    let s = Session::new();
    let mut team = s.team("dup", vec![agent("a", "", &[])], vec![]);
    team.defaults.command = scripted(&[]);
    // Shared: in worktrees, git itself refuses the second worktree.
    team.workspace.isolation = crew_core::config::Isolation::Shared;
    team.workspace.init = false;
    std::fs::create_dir_all(&s.workspace).unwrap();
    s.run(&team);
    // Two operator connections, ready before either asks, so the two adds
    // reach the daemon together.
    let info: Value = serde_json::from_str(
        &std::fs::read_to_string(s.workspace.join(".crew/daemon.json")).unwrap(),
    )
    .unwrap();
    let socket = std::path::PathBuf::from(info["socket"].as_str().unwrap());
    let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
    let adds: Vec<_> = (0..2)
        .map(|_| {
            let (socket, ready) = (socket.clone(), ready.clone());
            std::thread::spawn(move || {
                let (mut client, _) = crew_core::client::Client::operator(&socket, "test").unwrap();
                ready.wait();
                client
                    .call::<crew_core::Agent>(crew_core::rpc::Call::AgentAdd {
                        spec: AgentSpec {
                            id: "dup".into(),
                            ..AgentSpec::default()
                        },
                    })
                    .map_err(|e| e.to_string())
            })
        })
        .collect();
    let results: Vec<_> = adds.into_iter().map(|add| add.join().unwrap()).collect();

    let refused: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(refused.len(), 1, "exactly one add succeeds: {results:?}");
    assert!(
        refused[0].contains("agent 'dup' already exists"),
        "{}",
        refused[0]
    );
    let snapshot = s.snapshot();
    assert_eq!(snapshot.agents.iter().filter(|a| a.id == "dup").count(), 1);
    let added = s
        .events()
        .into_iter()
        .filter(|e| e.event_type == "agent.added")
        .count();
    assert_eq!(added, 1);
}

#[test]
fn a_daemon_started_while_another_is_still_starting_is_refused() {
    // A daemon binds its socket before it registers in daemon.json, so two
    // starts close together can both miss each other's registration. Put a
    // running daemon back in that window and start a second one.
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    let ws = s.workspace.to_string_lossy().into_owned();
    s.ok(&["start", "-C", &ws]);
    let info = s.workspace.join(".crew").join("daemon.json");
    let registration = std::fs::read_to_string(&info).unwrap();
    let first: Value = serde_json::from_str(&registration).unwrap();
    let socket = first["socket"].as_str().unwrap().to_string();
    std::fs::remove_file(&info).unwrap();

    let mut second = s
        .command()
        .args(["daemon", "--workspace", &ws])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + TIMEOUT;
    let exited = loop {
        if let Some(status) = second.try_wait().unwrap() {
            break Some(status);
        }
        // The second daemon registering itself means it came up.
        if info.exists() || std::time::Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    };
    let Some(status) = exited else {
        let _ = second.kill();
        let _ = second.wait();
        // The first daemon lost its socket to the second: stop it by pid.
        let _ = std::process::Command::new("kill")
            .arg(first["pid"].to_string())
            .status();
        panic!("a second daemon came up for a workspace that already had one");
    };
    let mut err = String::new();
    std::io::Read::read_to_string(&mut second.stderr.take().unwrap(), &mut err).unwrap();
    assert!(!status.success());
    assert!(err.contains("already running"), "{err}");
    // The refused daemon left the first one's socket alone.
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());

    // Registered again, so the session's teardown stops it.
    std::fs::write(&info, registration).unwrap();
}

// ---------------------------------------------------------------------------
// Workspace isolation
// ---------------------------------------------------------------------------

#[test]
fn agents_work_in_their_own_worktrees_and_exchange_commits() {
    let s = Session::new();
    let team = s.team(
        "worktrees",
        vec![
            agent(
                "db",
                "",
                &[
                    "--on-task=write schema.sql CREATE TABLE t (id INTEGER);",
                    "--on-task=commit Add schema",
                    "--on-task=publish ledger The schema is ready.",
                    "--on-task=done schema committed",
                ],
            ),
            agent(
                "ledger",
                "",
                &[
                    "--on-task=wait-message db",
                    "--on-task=cherry-pick",
                    "--on-task=write ledger.rs pub struct Ledger;",
                    "--on-task=commit Add ledger",
                    "--on-task=done ledger on top of the schema",
                ],
            ),
            agent(
                "rival",
                "",
                &[
                    "--on-task=write schema.sql CREATE TABLE other (x TEXT);",
                    "--on-task=commit Rival schema",
                    "--on-task=done rival schema",
                ],
            ),
        ],
        vec![
            task("schema", "db", &[]),
            task("ledger", "ledger", &[]),
            task("rival", "rival", &[]),
        ],
    );
    s.run(&team);
    let done = s.wait_all_done();
    assert_eq!(done.session.isolation, "worktree");

    let worktree = |id: &str| s.workspace.join(".crew/worktrees").join(id);
    for id in ["db", "ledger", "rival"] {
        let a = find_agent(&done, id);
        assert_eq!(a.branch.as_deref(), Some(format!("agents/{id}").as_str()));
        assert_eq!(std::path::Path::new(&a.workdir), worktree(id));
    }
    // The main checkout was never touched by an agent.
    assert!(!s.workspace.join("schema.sql").exists());
    assert!(!s.workspace.join("ledger.rs").exists());
    // ledger received db's schema through the commit db published...
    assert_eq!(
        std::fs::read_to_string(worktree("ledger").join("schema.sql"))
            .unwrap()
            .trim(),
        "CREATE TABLE t (id INTEGER);"
    );
    // ...and nobody overwrote anybody: rival's conflicting file stayed its own.
    assert!(
        std::fs::read_to_string(worktree("rival").join("schema.sql"))
            .unwrap()
            .contains("other")
    );
    assert!(!worktree("db").join("ledger.rs").exists());

    // Commits are recorded as task artifacts and announced directly.
    let schema = find_task(&done, "schema");
    assert_eq!(schema.artifacts.len(), 1);
    assert_eq!(schema.artifacts[0].kind, "commit");
    let published = s
        .messages()
        .into_iter()
        .find(|m| m.kind == MessageType::Artifact)
        .unwrap();
    assert_eq!(
        (published.from.as_str(), published.to.as_str()),
        ("db", "ledger")
    );
    assert!(published.body.contains(&schema.artifacts[0].value));

    // Integration merges what merges and reports what conflicts.
    let out = s.ok(&["integrate", "-C", &s.workspace.to_string_lossy()]);
    let report: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(report["branch"], "agents/integration");
    let merged: Vec<&str> = report["merged"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        merged.contains(&"db") && merged.contains(&"ledger"),
        "{report}"
    );
    assert_eq!(report["conflicts"][0]["agent"], "rival");
    assert_eq!(report["conflicts"][0]["files"][0], "schema.sql");
    assert!(s.has_event("git.conflict"));
}

// ---------------------------------------------------------------------------
// Dynamic teams
// ---------------------------------------------------------------------------

#[test]
fn an_agent_can_launch_a_subagent_that_reports_back_and_is_retired() {
    let s = Session::new();
    let team = s.team(
        "subagents",
        vec![agent(
            "lead",
            "implementation",
            &[
                "--on-task=write lib.rs pub fn lib() {}",
                "--on-task=commit Add lib",
                "--on-task=spawn write the tests for lib.rs",
                "--on-task=wait-message lead-s1",
                "--on-task=cherry-pick",
                "--on-task=done with help: {body}",
                // The same script runs in the subagent, with these steps.
                "--on-subtask=write tests.rs fn test_lib() {}",
                "--on-subtask=commit Add tests",
                "--on-subtask=done tests written",
            ],
        )],
        vec![task("build", "lead", &[])],
    );
    s.run(&team);
    let done = s.wait_all_done();

    let sub = find_agent(&done, "lead-s1");
    assert_eq!(sub.parent.as_deref(), Some("lead"));
    assert_eq!(sub.branch.as_deref(), Some("agents/lead-s1"));
    // The subagent branched from its parent, so it saw the parent's commit.
    assert!(std::path::Path::new(&sub.workdir).join("lib.rs").exists());
    let sub_task = find_task(&done, "lead-s1-task");
    assert_eq!(sub_task.status, TaskStatus::Done);
    assert_eq!(sub_task.assigned_agent.as_deref(), Some("lead-s1"));

    // It reported straight to its parent, with its commit.
    let report = s
        .messages()
        .into_iter()
        .find(|m| m.from == "lead-s1" && m.to == "lead")
        .expect("the report to the parent");
    assert!(report.is_p2p());
    assert!(report.body.contains("tests written"), "{}", report.body);
    assert!(find_task(&done, "build")
        .result
        .as_deref()
        .unwrap()
        .contains("Subagent lead-s1 finished"));
    // The parent took the subagent's commit.
    assert!(std::path::Path::new(&find_agent(&done, "lead").workdir)
        .join("tests.rs")
        .exists());

    // Once done, the subagent's process is retired; its record stays.
    let retired = s.wait("the subagent to be retired", |snap| {
        let a = find_agent(snap, "lead-s1");
        a.pid.is_none() && a.status == AgentStatus::Done
    });
    assert_eq!(find_agent(&retired, "lead").status, AgentStatus::Done);
    assert!(s.has_event("agent.subagent"));
    s.wait_for("the retirement to be logged", || {
        s.has_event("agent.retired")
    });

    // Subagents are not standing team members.
    let roster =
        crew_core::config::TeamConfig::load(&s.workspace.join(".crew/roster.yaml")).unwrap();
    let members: Vec<&str> = roster.agents.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(members, ["lead"]);
}

#[test]
fn an_agent_whose_task_cannot_be_created_is_not_created_either() {
    let s = Session::new();
    // 26 characters: the longest id a generated subagent id must fit after.
    let lead = "lead-with-a-very-long-name";
    let mut team = s.team("orphans", vec![agent(lead, "", &[])], vec![]);
    team.defaults.command = scripted(&[]);
    s.run(&team);
    s.wait("the lead to come up", |snap| {
        find_agent(snap, lead).status != AgentStatus::Spawning
    });
    let ws = s.workspace.to_string_lossy().into_owned();
    let ids = |snap: &crew_core::rpc::Snapshot| -> Vec<String> {
        snap.agents.iter().map(|a| a.id.clone()).collect()
    };

    // A valid agent id whose task id ("<id>-task") would be too long.
    let long = "a".repeat(28);
    let spawn = json!({ "task": "a quick job", "id": long }).to_string();
    let out = s.ctl(lead, &["tool", "agent_spawn", &spawn]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert_eq!(ids(&s.snapshot()), [lead]);
    assert!(stderr(&out).contains("at most 27"), "{}", stderr(&out));

    // The same through agent_add with a task.
    let added = s.crew(&[
        "agent", "add", "--name", &long, "--task", "a job", "-C", &ws,
    ]);
    assert!(!added.status.success(), "{}", stdout(&added));
    assert_eq!(ids(&s.snapshot()), [lead]);
    assert!(stderr(&added).contains("at most 27"), "{}", stderr(&added));

    // A generated id leaves room for the task id.
    let spawn = json!({ "task": "a quick job" }).to_string();
    let out = s.ctl(lead, &["tool", "agent_spawn", &spawn]);
    assert!(out.status.success(), "{}", stderr(&out));
    let snapshot = s.snapshot();
    let sub = snapshot
        .agents
        .iter()
        .find(|a| a.parent.as_deref() == Some(lead))
        .expect("the subagent");
    assert!(
        snapshot
            .tasks
            .iter()
            .any(|t| t.assigned_agent.as_deref() == Some(sub.id.as_str())),
        "{snapshot:?}"
    );
}

#[test]
fn new_work_does_not_go_to_a_retired_subagent() {
    let s = Session::new();
    let team = s.team(
        "retired",
        vec![agent(
            "lead",
            "",
            &[
                "--on-task=spawn a quick job",
                "--on-task=wait-message operator",
                "--on-task=done lead finished",
                "--on-subtask=done quick job done",
            ],
        )],
        vec![task("build", "lead", &[])],
    );
    s.run(&team);
    s.wait("the subagent to be retired", |snap| {
        snap.agents
            .iter()
            .any(|a| a.id == "lead-s1" && a.status == AgentStatus::Done && a.pid.is_none())
            && find_task(snap, "build").status == TaskStatus::Working
    });
    s.wait_for("the retirement to be logged", || {
        s.has_event("agent.retired")
    });

    // The only other worker is busy: new unassigned work must wait for it,
    // not go to the subagent whose process is gone.
    let ws = s.workspace.to_string_lossy().into_owned();
    s.ok(&[
        "task",
        "add",
        "--id",
        "extra",
        "--description",
        "more work",
        "-C",
        &ws,
    ]);
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let extra = find_task(&s.snapshot(), "extra").clone();
    assert_eq!(extra.status, TaskStatus::Ready, "{extra:?}");
    assert_eq!(extra.assigned_agent, None);

    // Once the lead is free, it takes the work.
    s.send("lead", "continue");
    s.wait("the extra task to go to the lead", |snap| {
        find_task(snap, "extra").assigned_agent.as_deref() == Some("lead")
    });
}

#[test]
fn the_orchestrator_designs_the_team_and_the_roster_is_saved_and_reused() {
    let s = Session::new();
    // No agents are declared: the (scripted) orchestrator decides who is needed.
    let mut team = s.team("dynamic", vec![], vec![]);
    team.defaults.command = scripted(&["--on-task=done built by {me}"]);
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[
            r#"--on-start=tool agent_add {"id":"builder","role":"builds things"}"#,
            r#"--on-start=tool agent_add {"id":"tester","role":"tests things"}"#,
            r#"--on-start=tool task_create {"id":"build","description":"build it","agent":"builder"}"#,
            r#"--on-start=tool task_create {"id":"test","description":"test it","agent":"tester","depends_on":["build"]}"#,
        ]),
        ..OrchestratorConfig::default()
    };
    s.run(&team);
    let done = s.wait_all_done();
    let mut workers: Vec<&str> = done
        .agents
        .iter()
        .filter(|a| !a.orchestrator)
        .map(|a| a.id.as_str())
        .collect();
    workers.sort_unstable();
    assert_eq!(workers, ["builder", "tester"]);
    assert_eq!(find_agent(&done, "builder").role, "builds things");
    assert_eq!(
        find_task(&done, "test").result.as_deref(),
        Some("built by tester")
    );
    // The orchestrator is told when tasks finish, but relays nothing.
    assert!(s
        .messages()
        .iter()
        .any(|m| m.from == "scheduler" && m.to == "orchestrator"));

    // The roster it settled on is on disk as a runnable team file.
    let roster_path = s.workspace.join(".crew/roster.yaml");
    let roster = crew_core::config::TeamConfig::load(&roster_path).unwrap();
    assert_eq!(roster.project, "dynamic");
    let ids: Vec<&str> = roster.agents.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["builder", "tester"]);
    assert!(roster.tasks.is_empty());

    // The roster can be modified while the team runs, and stays in sync.
    s.agent_cmd(&["remove", "tester"]);
    let after_removal = s.wait("tester to leave", |snap| {
        snap.agents.iter().all(|a| a.id != "tester")
    });
    assert!(after_removal.agents.iter().any(|a| a.id == "builder"));
    let roster = crew_core::config::TeamConfig::load(&roster_path).unwrap();
    assert_eq!(roster.agents.len(), 1);
    s.agent_cmd(&["add", "--name", "docs", "--role", "writes docs"]);
    s.wait("docs to join", |snap| {
        snap.agents.iter().any(|a| a.id == "docs")
    });
    let roster = crew_core::config::TeamConfig::load(&roster_path).unwrap();
    let ids: Vec<&str> = roster.agents.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["builder", "docs"]);

    // Next run: the same objective-only file starts from the saved team, so
    // the orchestrator does not have to design it again.
    s.stop();
    team.orchestrator.command = scripted(&[
        r#"--on-start=tool task_create {"id":"again","description":"second session","agent":"docs"}"#,
    ]);
    let out = s.run(&team);
    assert!(out.contains("using the saved roster (2 member(s)"), "{out}");
    let second = s.wait_all_done();
    let mut workers: Vec<&str> = second
        .agents
        .iter()
        .filter(|a| !a.orchestrator)
        .map(|a| a.id.as_str())
        .collect();
    workers.sort_unstable();
    assert_eq!(workers, ["builder", "docs"]);
    assert_eq!(
        find_task(&second, "again").result.as_deref(),
        Some("built by docs")
    );

    // --replan throws the saved roster away.
    s.stop();
    team.orchestrator.command = scripted(&[]);
    s.run_with(&team, &["--replan"]);
    let replanned = s.wait("the orchestrator alone", |snap| {
        snap.agents
            .iter()
            .any(|a| a.orchestrator && a.status != AgentStatus::Spawning)
    });
    assert_eq!(replanned.agents.len(), 1);
}

#[test]
fn only_the_orchestrator_may_change_the_team_or_the_plan() {
    let s = Session::new();
    let team = s.team(
        "authz",
        vec![agent("a", "", &[]), agent("b", "", &[])],
        vec![],
    );
    s.run(&team);
    s.wait("agents to come up", |snap| {
        snap.agents
            .iter()
            .all(|a| a.status != AgentStatus::Spawning)
    });
    for (tool, args) in [
        ("agent_add", json!({ "id": "c", "role": "x" })),
        ("agent_remove", json!({ "id": "b" })),
        ("agent_set_model", json!({ "id": "b", "model": "sonnet" })),
        ("task_create", json!({ "id": "t", "description": "x" })),
        ("session_complete", json!({ "summary": "x" })),
    ] {
        let out = s.ctl("a", &["tool", tool, &args.to_string()]);
        assert!(
            !out.status.success(),
            "{tool} should be refused for a worker"
        );
        assert!(
            stderr(&out).contains("only the orchestrator"),
            "{tool}: {}",
            stderr(&out)
        );
    }
    // Agents cannot act on each other's git or inbox either.
    let out = s.ctl(
        "a",
        &["tool", "git_status", &json!({ "agent": "b" }).to_string()],
    );
    assert!(!out.status.success());
    assert_eq!(s.snapshot().agents.len(), 2);
    let _: Vec<AgentSpec> = Vec::new();
    let _: Vec<TaskSpec> = Vec::new();
}

#[test]
fn plain_crew_starts_a_team_reopens_it_and_remembers_it() {
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    let ws = s.workspace.to_string_lossy().into_owned();

    // A mistyped command is not taken for an objective, and without a
    // terminal there is nobody to ask for one. Neither starts anything.
    let out = s.crew(&["atach", "-C", &ws]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("did you mean `crew attach`"),
        "{}",
        stderr(&out)
    );
    let out = s.crew(&["-C", &ws]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("no objective"), "{}", stderr(&out));
    let out = s.crew(&["build a thing", "status"]);
    assert!(!out.status.success());
    assert!(!s.workspace.join(".crew").exists());

    // A crew.yaml in the directory is picked up; the objective on the command
    // line wins over the file's.
    let mut team = s.team("plain", vec![], vec![]);
    team.defaults.command = scripted(&["--on-task=done built by {me}"]);
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[
            r#"--on-start=tool agent_add {"id":"builder","role":"builds things"}"#,
            r#"--on-start=tool task_create {"id":"build","description":"build it","agent":"builder"}"#,
        ]),
        ..OrchestratorConfig::default()
    };
    let file = s.workspace.join("crew.yaml");
    std::fs::write(&file, team.to_yaml().unwrap()).unwrap();
    let out = s.ok(&["build the first thing", "--no-attach", "-C", &ws]);
    assert!(out.contains("starting team file crew.yaml"), "{out}");
    let first = s.wait_all_done();
    assert_eq!(first.session.objective, "build the first thing");
    assert_eq!(
        find_task(&first, "build").result.as_deref(),
        Some("built by builder")
    );

    // While the team is up, plain `crew` goes back to it instead of starting
    // another, and a new objective is handed to the orchestrator.
    let out = s.ok(&["-C", &ws]);
    assert!(out.contains("team 'plain' is running here"), "{out}");
    let out = s.ok(&["now build a second thing", "-C", &ws]);
    assert!(out.contains("sent the objective to orchestrator"), "{out}");
    s.wait_for("the objective to reach the orchestrator", || {
        s.messages()
            .iter()
            .any(|m| m.to == "orchestrator" && m.body == "now build a second thing")
    });
    let same = s.snapshot();
    assert_eq!(same.session.team_started_at, first.session.team_started_at);

    // After a stop, the directory remembers its team: no team file needed.
    s.stop();
    std::fs::remove_file(&file).unwrap();
    let roster_path = s.workspace.join(".crew/roster.yaml");
    let mut roster = crew_core::config::TeamConfig::load(&roster_path).unwrap();
    roster.orchestrator.command = scripted(&[
        r#"--on-start=tool task_create {"id":"third","description":"third","agent":"builder"}"#,
    ]);
    std::fs::write(&roster_path, roster.to_yaml().unwrap()).unwrap();
    let out = s.ok(&["a third thing to build", "--no-attach", "-C", &ws]);
    assert!(
        out.contains("the team of the last session (1 member(s))"),
        "{out}"
    );
    let third = s.wait_all_done();
    assert_eq!(third.session.objective, "a third thing to build");
    assert_eq!(
        find_task(&third, "third").result.as_deref(),
        Some("built by builder")
    );
    assert_ne!(third.session.team_started_at, first.session.team_started_at);

    // With no objective given, the last one is continued; --replan drops the
    // members and leaves the staffing to the orchestrator again.
    s.stop();
    let mut roster = crew_core::config::TeamConfig::load(&roster_path).unwrap();
    assert_eq!(roster.objective, "a third thing to build");
    roster.orchestrator.command = scripted(&[]);
    std::fs::write(&roster_path, roster.to_yaml().unwrap()).unwrap();
    let out = s.ok(&["--replan", "--no-attach", "-C", &ws]);
    assert!(out.contains("objective  a third thing to build"), "{out}");
    let replanned = s.wait("the orchestrator alone", |snap| {
        snap.agents
            .iter()
            .any(|a| a.orchestrator && a.status != AgentStatus::Spawning)
    });
    assert_eq!(replanned.agents.len(), 1);
}

/// A stand-in for the `claude` / `codex` binaries: records how it was
/// started next to the agent's identity file, then idles like a session
/// waiting at its prompt.
const FAKE_RUNTIME: &str = r#"#!/bin/sh
dir="${CREW_IDENTITY%/*}"
: > "$dir/fake-args"
for arg in "$@"; do printf '%s\n' "$arg" >> "$dir/fake-args"; done
env > "$dir/fake-env"
while read -r _; do :; done
"#;

#[test]
fn team_members_get_their_endpoints_and_keys_from_the_layered_config() {
    use std::os::unix::fs::PermissionsExt;
    const GLOBAL_KEY: &str = "sk-ant-e2e-global-key-0000000001";
    const USER_KEY: &str = "sk-ant-e2e-user-env-key-00000002";
    const PROJECT_KEY: &str = "sk-ant-e2e-project-key-000000003";
    const CODEX_KEY: &str = "sk-e2e-codex-user-key-0000000004";

    let s = Session::new();
    let root = s.root().to_path_buf();
    let write = |path: &Path, text: &str, mode: u32| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    for name in ["claude", "codex"] {
        write(&root.join("bin").join(name), FAKE_RUNTIME, 0o755);
    }
    // global → user → project, each overriding part of the one before.
    let global = root.join("etc/config.toml");
    write(
        &global,
        &format!(
            "[claude]\nbase_url = \"https://global.example\"\napi_key = \"{GLOBAL_KEY}\"\n\
             model = \"global-model\"\n[claude.env]\nFROM_GLOBAL = \"yes\"\n"
        ),
        0o644,
    );
    write(
        &root.join("xdg/crew/config.toml"),
        "[claude]\nbase_url = \"https://user.example\"\napi_key_env = \"E2E_TEAM_KEY\"\n\
         [codex]\nbase_url = \"https://codex.example/v1\"\napi_key_file = \"codex.key\"\n",
        0o600,
    );
    write(
        &root.join("xdg/crew/codex.key"),
        &format!("{CODEX_KEY}\n"),
        0o600,
    );
    let project = s.workspace.join(".crew/config.toml");
    write(
        &project,
        &format!(
            "[claude]\napi_key = \"{PROJECT_KEY}\"\n\
             [agents.claude-2]\nbase_url = \"http://localhost:4000\"\napi_key = \"\"\n"
        ),
        0o600,
    );
    let ws = s.workspace.to_string_lossy().into_owned();
    let crew = |args: &[&str]| {
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        s.command()
            .env("PATH", path)
            .env("CREW_GLOBAL_CONFIG", &global)
            .env("XDG_CONFIG_HOME", root.join("xdg"))
            .env("E2E_TEAM_KEY", USER_KEY)
            .args(args)
            .output()
            .expect("running crew")
    };

    let mut team = s.team("endpoints", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    for (id, runtime) in [
        ("claude-1", RuntimeKind::Claude),
        ("claude-2", RuntimeKind::Claude),
        ("codex-1", RuntimeKind::Codex),
    ] {
        team.agents.push(AgentSpec {
            id: id.to_string(),
            runtime: Some(runtime),
            model: Some("team-model".to_string()),
            ..AgentSpec::default()
        });
    }
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = crew(&["run", "team.yaml", "--no-attach"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // The global file holds a key and is world-readable: said so up front.
    assert!(stderr(&out).contains("chmod 600"), "{}", stderr(&out));

    let agent_dir = |id: &str| s.workspace.join(".crew/agents").join(id);
    let read = |id: &str, file: &str| {
        std::fs::read_to_string(agent_dir(id).join(file)).unwrap_or_default()
    };
    s.wait_for("the sessions to start", || {
        ["claude-1", "claude-2", "codex-1"]
            .iter()
            .all(|id| read(id, "fake-env").contains("CREW_IDENTITY"))
    });

    // claude-1: endpoint from the user file (over global), model and extra
    // environment from global, and a key helper instead of a key.
    let settings: serde_json::Value =
        serde_json::from_str(&read("claude-1", "settings.json")).unwrap();
    assert_eq!(
        settings["env"]["ANTHROPIC_BASE_URL"],
        "https://user.example"
    );
    assert_eq!(settings["env"]["FROM_GLOBAL"], "yes");
    assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "");
    assert!(settings["apiKeyHelper"]
        .as_str()
        .unwrap()
        .contains(" credential --identity "));
    let args = read("claude-1", "fake-args");
    assert!(args.contains("--model\nglobal-model\n"), "{args}");
    assert!(!args.contains("team-model"));
    // The helper returns the key of the nearest layer (project), and each
    // agent only ever gets its own.
    let identity = |id: &str| s.identity_path(id).to_string_lossy().into_owned();
    let key = crew(&["credential", "--identity", &identity("claude-1")]);
    assert_eq!(stdout(&key).trim(), PROJECT_KEY, "{}", stderr(&key));
    let codex_key = crew(&["credential", "--identity", &identity("codex-1")]);
    assert_eq!(stdout(&codex_key).trim(), CODEX_KEY);

    // claude-2: its own section wins over [claude] and takes the key back.
    let settings: serde_json::Value =
        serde_json::from_str(&read("claude-2", "settings.json")).unwrap();
    assert_eq!(
        settings["env"]["ANTHROPIC_BASE_URL"],
        "http://localhost:4000"
    );
    assert!(settings.get("apiKeyHelper").is_none());
    assert!(settings["env"].get("ANTHROPIC_API_KEY").is_none());
    let none = crew(&["credential", "--identity", &identity("claude-2")]);
    assert!(!none.status.success());
    assert!(
        stderr(&none).contains("sets no api key"),
        "{}",
        stderr(&none)
    );

    // codex-1: a provider of its own, the key in its environment only.
    let args = read("codex-1", "fake-args");
    assert!(args.contains("model_provider=\"crew\""), "{args}");
    assert!(
        args.contains("model_providers.crew.base_url=\"https://codex.example/v1\""),
        "{args}"
    );
    assert!(read("codex-1", "fake-env").contains(&format!("CREW_CODEX_API_KEY={CODEX_KEY}")));

    // No key is on a command line, in a Claude session's environment, or in
    // anything crew persists (database, roster, event log, daemon log).
    let keys = [GLOBAL_KEY, USER_KEY, PROJECT_KEY, CODEX_KEY];
    let leaked = |text: &[u8]| {
        keys.iter()
            .any(|k| text.windows(k.len()).any(|w| w == k.as_bytes()))
    };
    for id in ["claude-1", "claude-2", "codex-1"] {
        assert!(
            !leaked(read(id, "fake-args").as_bytes()),
            "{id} command line"
        );
        assert!(
            !leaked(read(id, "settings.json").as_bytes()),
            "{id} settings"
        );
    }
    // (The daemon itself was started with E2E_TEAM_KEY in its environment and
    // hands that environment on; the keys crew resolved are what matters.)
    assert!(!read("claude-1", "fake-env").contains(PROJECT_KEY));
    assert!(!read("claude-1", "fake-env").contains(GLOBAL_KEY));
    for entry in std::fs::read_dir(s.workspace.join(".crew"))
        .unwrap()
        .flatten()
    {
        let path = entry.path();
        if path.is_file() && path != project {
            assert!(
                !leaked(&std::fs::read(&path).unwrap()),
                "{} holds a key",
                path.display()
            );
        }
    }
    let events = s.events();
    let endpoint = events
        .iter()
        .find(|e| e.event_type == "agent.endpoint" && e.agent_id.as_deref() == Some("claude-1"))
        .expect("an endpoint event");
    assert!(
        endpoint
            .message
            .contains("endpoint https://user.example (user config)"),
        "{}",
        endpoint.message
    );
    assert!(
        endpoint
            .message
            .contains("api key from the config file (project config)"),
        "{}",
        endpoint.message
    );
    assert_eq!(
        find_agent(&s.snapshot(), "claude-1").model.as_deref(),
        Some("global-model")
    );

    // `crew config` shows where each value comes from, keys masked.
    let report = crew(&["config", "-C", &ws]);
    assert!(report.status.success(), "{}", stderr(&report));
    let report = stdout(&report);
    assert!(report.contains("https://user.example  (user)"), "{report}");
    assert!(report.contains("sk-a…0003  (project)"), "{report}");
    assert!(report.contains("[agents.claude-2]"), "{report}");
    assert!(!leaked(report.as_bytes()), "{report}");

    // A config file crew cannot trust stops the launch instead of letting
    // the agent run on some other endpoint or login.
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o666)).unwrap();
    let report = crew(&["config", "-C", &ws]);
    assert!(!report.status.success());
    assert!(stdout(&report).contains("REJECTED"), "{}", stdout(&report));
    s.agent_cmd(&["restart", "claude-1"]);
    let failed = s.wait("claude-1 to fail", |snap| {
        find_agent(snap, "claude-1").status == AgentStatus::Failed
    });
    let error = find_agent(&failed, "claude-1")
        .error
        .clone()
        .unwrap_or_default();
    assert!(error.contains("writable by other users"), "{error}");
}

#[test]
fn work_styles_are_launched_by_name_and_each_remembers_its_team() {
    use crew_core::config::{AgentDefaults, Style};
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    let folder = s.workspace.join(".crew/styles");
    std::fs::create_dir_all(&folder).unwrap();
    let ws = s.workspace.to_string_lossy().into_owned();
    let write_style = |name: &str, style: &Style| {
        std::fs::write(
            folder.join(format!("{name}.yaml")),
            style.to_yaml().unwrap(),
        )
        .unwrap();
    };
    let shell_defaults = AgentDefaults {
        runtime: RuntimeKind::Shell,
        command: scripted(&["--on-task=done by {me}"]),
        ..AgentDefaults::default()
    };
    let orchestrated = |steps: &[&str]| OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(steps),
        ..OrchestratorConfig::default()
    };
    // "notes": the orchestrator staffs the team. "lab": a fixed roster.
    let mut notes = Style {
        description: "Take notes".into(),
        instructions: "Write everything to notes.md.".into(),
        defaults: shell_defaults.clone(),
        orchestrator: orchestrated(&[
            r#"--on-start=tool agent_add {"id":"scout","role":"reads"}"#,
            r#"--on-start=tool task_create {"id":"read","description":"read it","agent":"scout"}"#,
        ]),
        ..Style::default()
    };
    write_style("notes", &notes);
    write_style(
        "lab",
        &Style {
            description: "Run experiments".into(),
            instructions: "Record every run.".into(),
            defaults: shell_defaults.clone(),
            orchestrator: OrchestratorConfig {
                agent: false,
                ..OrchestratorConfig::default()
            },
            agents: vec![agent("runner", "runs", &["--on-task=done by {me}"])],
            tasks: vec![task("run", "runner", &[])],
            ..Style::default()
        },
    );
    std::fs::write(folder.join("broken.yaml"), "agents: 3\n").unwrap();

    let list = s.ok(&["style", "list", "-C", &ws]);
    for expected in [
        "default",
        "research",
        "experiment",
        "cleanup",
        "built-in",
        "Take notes",
        "Run experiments",
    ] {
        assert!(list.contains(expected), "{expected}: {list}");
    }
    let unknown = s.crew(&["do a thing now", "--style", "nope", "-C", &ws]);
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains(
            "no work style 'nope' (available: default, research, experiment, cleanup, lab, notes)"
        ),
        "{}",
        stderr(&unknown)
    );

    // Launch one style…
    let out = s.crew(&[
        "survey the queue designs",
        "--style",
        "notes",
        "--no-attach",
        "-C",
        &ws,
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("starting a new team (notes style)"),
        "{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("broken.yaml"),
        "a broken style file is reported: {}",
        stderr(&out)
    );
    let first = s.wait_all_done();
    assert_eq!(first.session.style.as_deref(), Some("notes"));
    assert_eq!(first.session.objective, "survey the queue designs");
    assert_eq!(
        find_task(&first, "read").result.as_deref(),
        Some("by scout")
    );
    assert!(s.ok(&["status", "-C", &ws]).contains("style      notes"));
    // …it cannot be swapped under a running team…
    let busy = s.crew(&["measure the queue", "--style", "lab", "-C", &ws]);
    assert!(!busy.status.success());
    assert!(
        stderr(&busy).contains("is running here"),
        "{}",
        stderr(&busy)
    );

    // …but after a stop the same workspace runs another style, with its own team.
    s.stop();
    let out = s.ok(&[
        "measure the queue",
        "--style",
        "lab",
        "--no-attach",
        "-C",
        &ws,
    ]);
    assert!(out.contains("starting a new team (lab style)"), "{out}");
    let second = s.wait_all_done();
    assert_eq!(second.session.style.as_deref(), Some("lab"));
    let ids: Vec<&str> = second.agents.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["runner"]);
    s.stop();

    // Each style remembers the team it settled on and what it was working on.
    let load = |style: &str| {
        crew_core::config::TeamConfig::load(
            &s.workspace.join(format!(".crew/rosters/{style}.yaml")),
        )
        .unwrap()
    };
    assert_eq!(load("notes").agents[0].id, "scout");
    assert_eq!(load("notes").objective, "survey the queue designs");
    assert_eq!(load("lab").agents[0].id, "runner");
    assert_eq!(load("lab").instructions, "Record every run.");

    // Back to the first style: its team returns, its objective is continued,
    // and an edit to the style file applies (the roster holds only the members).
    notes.instructions = "Write everything to notes.md. Date each entry.".into();
    notes.orchestrator = orchestrated(&[
        r#"--on-start=tool task_create {"id":"again","description":"read more","agent":"scout"}"#,
    ]);
    write_style("notes", &notes);
    let out = s.ok(&["--style", "notes", "--no-attach", "-C", &ws]);
    assert!(
        out.contains("the notes team of the last session (1 member(s))"),
        "{out}"
    );
    assert!(out.contains("objective  survey the queue designs"), "{out}");
    let third = s.wait_all_done();
    assert_eq!(
        find_task(&third, "again").result.as_deref(),
        Some("by scout")
    );
    assert!(load("notes").instructions.ends_with("Date each entry."));
    s.stop();
    // With nobody to ask and no --style, the style used last is used again;
    // --replan drops its members.
    let out = s.ok(&["one more pass please", "--replan", "--no-attach", "-C", &ws]);
    assert!(out.contains("starting a new team (notes style)"), "{out}");
    s.wait("the orchestrator", |snap| {
        snap.agents.iter().any(|a| a.orchestrator)
    });
    assert_eq!(s.snapshot().session.style.as_deref(), Some("notes"));
    s.stop();

    // The style's instructions are part of a member's briefing.
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_RUNTIME).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    write_style(
        "brief",
        &Style {
            instructions: "Cite a source for every claim.".into(),
            orchestrator: OrchestratorConfig {
                agent: false,
                ..OrchestratorConfig::default()
            },
            agents: vec![AgentSpec {
                id: "reader".into(),
                role: "reads closely".into(),
                ..AgentSpec::default()
            }],
            ..Style::default()
        },
    );
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = s
        .command()
        .env("PATH", path)
        .args([
            "read things closely",
            "--style",
            "brief",
            "--no-attach",
            "-C",
            &ws,
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let briefing = s.workspace.join(".crew/agents/reader/briefing.md");
    s.wait_for("the briefing", || briefing.exists());
    let briefing = std::fs::read_to_string(briefing).unwrap();
    assert!(
        briefing.contains("## How this team works (brief)\nCite a source for every claim.\n"),
        "{briefing}"
    );
    assert!(
        briefing.contains("Team objective: read things closely"),
        "{briefing}"
    );
}

#[test]
fn the_model_catalog_reaches_the_orchestrator_and_a_model_decides_the_runtime() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    for name in ["claude", "codex"] {
        std::fs::write(root.join("bin").join(name), FAKE_RUNTIME).unwrap();
        std::fs::set_permissions(
            root.join("bin").join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    // The user's own additions to the catalog.
    std::fs::create_dir_all(root.join("config/crew")).unwrap();
    std::fs::write(
        root.join("config/crew/models.toml"),
        "[models.house-model]\nruntime = \"codex\"\ntier = \"fast\"\nsummary = \"Ours.\"\ngood_for = [\"drafts\"]\n\
         [[picks]]\nwork = \"drafts\"\nmodels = [\"house-model\"]\n",
    )
    .unwrap();
    let ws = s.workspace.to_string_lossy().into_owned();
    let crew = |args: &[&str]| {
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        s.command()
            .env("PATH", path)
            .args(args)
            .output()
            .expect("running crew")
    };

    let listing = crew(&["models"]);
    let listing = stdout(&listing);
    for expected in [
        "fable",
        "gpt-6-astra",
        "plan and design review",
        "house-model",
        "[user]",
        "drafts",
    ] {
        assert!(listing.contains(expected), "{expected}: {listing}");
    }
    let shown = stdout(&crew(&["models", "show", "claude-fable-5-1"]));
    assert!(
        shown.contains("fable  (Claude Fable 5.1)") && shown.contains("sources"),
        "{shown}"
    );

    let mut team = s.team("models", vec![], vec![]);
    team.defaults.runtime = RuntimeKind::Claude;
    team.defaults.model = Some("sonnet".to_string());
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.orchestrator = OrchestratorConfig::default();
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = crew(&["run", "team.yaml", "--no-attach"]);
    assert!(out.status.success(), "{}", stderr(&out));

    // The orchestrator is briefed on what each model is for.
    let briefing = s.workspace.join(".crew/agents/orchestrator/briefing.md");
    s.wait_for("the orchestrator's briefing", || briefing.exists());
    let briefing = std::fs::read_to_string(briefing).unwrap();
    assert!(briefing.contains("## Models"), "{briefing}");
    assert!(
        briefing.contains("- gpt-6-astra (Codex, frontier"),
        "{briefing}"
    );
    assert!(
        briefing.contains("- plan and design review: gpt-6-astra, then fable"),
        "{briefing}"
    );
    assert!(
        briefing.contains("- house-model (Codex, fast): Ours. Good for: drafts."),
        "{briefing}"
    );
    assert!(
        briefing.contains("checked by the other model family"),
        "{briefing}"
    );

    // Naming a model is enough to put a member on the right CLI…
    let args = |id: &str| {
        std::fs::read_to_string(s.workspace.join(".crew/agents").join(id).join("fake-args"))
            .unwrap_or_default()
    };
    let add = |extra: &[&str]| {
        let mut full = vec!["agent", "add"];
        full.extend_from_slice(extra);
        full.extend_from_slice(&["-C", &ws]);
        let out = crew(&full);
        assert!(out.status.success(), "{}", stderr(&out));
    };
    add(&[
        "--name",
        "reviewer",
        "--role",
        "reviews plans",
        "--model",
        "gpt-6-astra",
    ]);
    // …even against a runtime that cannot run it, or by its full id…
    add(&[
        "--name",
        "builder",
        "--role",
        "builds",
        "--model",
        "claude-opus-5-5",
        "--runtime",
        "codex",
    ]);
    // …and a Codex member without a model does not inherit the Claude default.
    add(&["--name", "helper", "--role", "helps", "--runtime", "codex"]);
    add(&["--name", "writer", "--role", "writes"]);
    s.wait_for("the members to start", || {
        ["reviewer", "builder", "helper", "writer"]
            .iter()
            .all(|id| !args(id).is_empty())
    });
    let snapshot = s.snapshot();
    let member = |id: &str| {
        let a = find_agent(&snapshot, id);
        (a.runtime, a.model.clone())
    };
    assert_eq!(
        member("reviewer"),
        (RuntimeKind::Codex, Some("gpt-6-astra".to_string()))
    );
    assert_eq!(
        member("builder"),
        (RuntimeKind::Claude, Some("opus".to_string()))
    );
    assert_eq!(member("helper"), (RuntimeKind::Codex, None));
    // Each is known by its model's proper name, from the catalog; one on
    // the runtime's default has none until its session says what it runs on.
    let name = |id: &str| find_agent(&snapshot, id).model_name.clone();
    assert_eq!(name("reviewer").as_deref(), Some("GPT 6 Astra"));
    assert_eq!(name("builder").as_deref(), Some("Opus 5.5"));
    assert_eq!(name("writer").as_deref(), Some("Sonnet 5.5"));
    assert_eq!(name("helper"), None);
    assert_eq!(
        member("writer"),
        (RuntimeKind::Claude, Some("sonnet".to_string()))
    );
    assert!(
        args("reviewer").contains("--model\ngpt-6-astra\n"),
        "{}",
        args("reviewer")
    );
    assert!(
        args("reviewer").contains("mcp_servers.crew.command"),
        "started by the Codex adapter"
    );
    // A Claude model with a 1M-token context variant is started in it…
    assert!(
        args("builder").contains("--model\nopus[1m]\n") && args("builder").contains("--mcp-config")
    );
    assert!(
        args("writer").contains("--model\nsonnet[1m]\n"),
        "{}",
        args("writer")
    );
    // …and a Codex model with the window is asked for it, as configuration.
    assert!(
        args("reviewer").contains("-c\nmodel_context_window=1000000\n"),
        "{}",
        args("reviewer")
    );
    assert!(
        args("reviewer").contains("-c\nmodel_auto_compact_token_limit=900000\n"),
        "{}",
        args("reviewer")
    );
    assert!(
        !args("helper").contains("model_context_window"),
        "no model named: {}",
        args("helper")
    );
    // One without such a variant is started as it is.
    add(&[
        "--name",
        "runner",
        "--role",
        "runs errands",
        "--model",
        "haiku",
    ]);
    s.wait_for("the runner to start", || !args("runner").is_empty());
    assert!(
        args("runner").contains("--model\nhaiku\n"),
        "{}",
        args("runner")
    );
    // An account without the 1M window refuses every request of such a
    // session. The agent is started again on the standard window, for good.
    let refusal = json!({
        "error": "invalid_request",
        "error_details": "API Error: 400 The long context beta is not yet available for this subscription.",
    })
    .to_string();
    let identity = s.identity_path("builder").to_string_lossy().into_owned();
    let out = s.crew(&["hook", "stop-failure", "--identity", &identity, &refusal]);
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait_for("builder to restart on the standard window", || {
        args("builder").contains("--model\nopus\n")
    });
    assert!(s.has_event("agent.long_context_refused"));
    s.agent_cmd(&["restart", "builder"]);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        args("builder").contains("--model\nopus\n"),
        "not asked for again: {}",
        args("builder")
    );
    // Any other failed turn of another agent changes nothing about its model.
    let identity = s.identity_path("writer").to_string_lossy().into_owned();
    let out = s.crew(&[
        "hook",
        "stop-failure",
        "--identity",
        &identity,
        r#"{"error":"overloaded","error_details":"Overloaded"}"#,
    ]);
    assert!(out.status.success());
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert!(args("writer").contains("--model\nsonnet[1m]\n"));
    assert!(!args("helper").contains("--model"), "{}", args("helper"));
    assert!(s.events().iter().any(|e| e.event_type == "agent.model"
        && e.message == "builder: opus runs on Claude Code, not Codex"));
    // The choice is part of the saved roster.
    let roster =
        crew_core::config::TeamConfig::load(&s.workspace.join(".crew/roster.yaml")).unwrap();
    let reviewer = roster.agents.iter().find(|a| a.id == "reviewer").unwrap();
    assert_eq!(
        (reviewer.runtime, reviewer.model.as_deref()),
        (Some(RuntimeKind::Codex), Some("gpt-6-astra"))
    );
}

/// A Codex stand-in: while a file `limited` sits next to its identity it
/// answers every prompt (and its start) with Codex's usage-limit message;
/// otherwise it answers normally. Prompts it receives are recorded.
const FAKE_LIMITED_CODEX: &str = r#"#!/bin/sh
dir="${CREW_IDENTITY%/*}"
say() {
  if [ -e "$dir/limited" ]; then
    printf '\n■ You’ve hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at 11:58 PM.\n\n› Ask Codex to do anything\n'
  else
    printf '\n• on it\n\n› Ask Codex to do anything\n'
  fi
}
say
while read -r line; do
  printf '%s\n' "$line" >> "$dir/fake-input"
  say
done
"#;

#[test]
fn a_usage_limit_parks_the_agent_until_it_is_resumed() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    let ws = s.workspace.to_string_lossy().into_owned();
    let write_bin = |name: &str, text: &str| {
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join(name), text).unwrap();
        std::fs::set_permissions(
            root.join("bin").join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    };
    write_bin("codex", FAKE_LIMITED_CODEX);
    write_bin("claude", FAKE_RUNTIME);
    let agent_dir = |id: &str| s.workspace.join(".crew/agents").join(id);
    std::fs::create_dir_all(agent_dir("codex-1")).unwrap();
    std::fs::write(agent_dir("codex-1").join("limited"), "").unwrap();
    // The session file Codex would keep, with the account's usage in it.
    let day = chrono::Local::now().format("%Y/%m/%d").to_string();
    let sessions = root.join("codex-home/sessions").join(day);
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("rollout-2026-01-01T00-00-00-thread-e2e.jsonl"),
        r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":56.0,"window_minutes":10080,"resets_at":1999999999},"secondary":null}}}"#,
    )
    .unwrap();
    // Claude Code's own usage cache: the account's every window, among
    // them the one that only it knows, a model's own weekly limit.
    let fetched_at = crew_core::util::now_ms() - 90_000;
    std::fs::create_dir_all(root.join("claude-home")).unwrap();
    std::fs::write(
        root.join("claude-home/.claude.json"),
        json!({ "numStartups": 3, "cachedUsageUtilization": { "fetchedAtMs": fetched_at, "utilization": { "limits": [
            { "kind": "session", "percent": 33, "resets_at": "2030-01-01T00:00:00+00:00" },
            { "kind": "weekly_all", "percent": 65, "resets_at": "2030-01-03T00:00:00+00:00" },
            { "kind": "weekly_scoped", "percent": 72, "resets_at": "2030-01-03T00:00:00+00:00",
              "scope": { "model": { "id": null, "display_name": "Fable" }, "surface": null } },
        ] } } })
        .to_string(),
    )
    .unwrap();
    let crew = |args: &[&str]| {
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        s.command()
            .env("PATH", path)
            .args(args)
            .output()
            .expect("running crew")
    };

    let mut team = s.team("limits", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    // Far enough out that nothing resumes by itself during the test.
    team.limits.limit_retry_secs = 3600;
    for (id, runtime) in [
        ("codex-1", RuntimeKind::Codex),
        ("claude-1", RuntimeKind::Claude),
    ] {
        team.agents.push(AgentSpec {
            id: id.to_string(),
            runtime: Some(runtime),
            task: Some(format!("work for {id}")),
            ..AgentSpec::default()
        });
    }
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = crew(&["run", "team.yaml", "--no-attach"]);
    assert!(out.status.success(), "{}", stderr(&out));

    // Codex has no hook for it: the message on its screen parks the agent.
    let parked = s.wait("codex-1 to be parked at its usage limit", |snap| {
        find_agent(snap, "codex-1").limit.is_some()
    });
    let codex = find_agent(&parked, "codex-1");
    let limit = codex.limit.clone().unwrap();
    assert_eq!(
        (codex.status, codex.block_kind.as_deref()),
        (AgentStatus::Blocked, Some("usage_limit"))
    );
    assert_eq!(limit.kind, "usage");
    assert!(
        limit.message.contains("hit your usage limit"),
        "{}",
        limit.message
    );
    let resets = limit.resets_at.expect("the reset time on the screen");
    // Resumed by itself a minute after the stated reset.
    assert_eq!(limit.retry_at, Some(resets + 60_000));
    assert!(
        codex.activity.starts_with("usage limit reached · resets "),
        "{}",
        codex.activity
    );
    // It keeps its task, and the task is not failed or handed on.
    assert_eq!(
        find_task(&parked, "codex-1-task").status,
        TaskStatus::Working
    );
    assert!(s
        .events()
        .iter()
        .any(|e| e.event_type == "agent.limited" && e.agent_id.as_deref() == Some("codex-1")));
    let status = s.ok(&["status", "-C", &ws]);
    assert!(
        status.contains("limits     1 paused at a usage limit (codex-1)"),
        "{status}"
    );
    assert!(status.contains("crew resume --restart"), "{status}");
    let frame = s.ok(&["status", "--frame", "170x40", "-C", &ws]);
    assert!(
        frame.contains("codex  ⊘ usage limit reached · 1 paused (codex-1) · resets "),
        "{frame}"
    );
    assert!(frame.contains("u continue now"), "{frame}");

    // Nothing is typed at a parked agent: a message for it waits on the bus.
    let input =
        || std::fs::read_to_string(agent_dir("codex-1").join("fake-input")).unwrap_or_default();
    s.send("codex-1", "are you there");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(!input().contains("are you there"), "{}", input());

    // The limit lifts (or the account was switched): `crew resume`.
    std::fs::remove_file(agent_dir("codex-1").join("limited")).unwrap();
    let out = s.ok(&["resume", "-C", &ws]);
    assert!(out.contains("told to continue: codex-1"), "{out}");
    s.wait_for("the resume prompt", || {
        input().contains("usage limit that stopped you has lifted")
    });
    let resumed = s.snapshot();
    assert!(find_agent(&resumed, "codex-1").limit.is_none());
    assert_eq!(find_agent(&resumed, "codex-1").status, AgentStatus::Working);
    assert!(s
        .events()
        .iter()
        .any(|e| e.event_type == "agent.limit_lifted"
            && e.message.contains("resumed by the operator")));
    // Once its turn is over, what waited for it is delivered, and the
    // account's usage (from Codex's own session file) is known.
    let identity = |id: &str| s.identity_path(id).to_string_lossy().into_owned();
    let hook = |agent: &str, event: &str, payload: &str| {
        let out = s.crew(&["hook", event, "--identity", &identity(agent), payload]);
        assert!(out.status.success(), "{event}: {}", stderr(&out));
    };
    hook(
        "codex-1",
        "codex-notify",
        r#"{"type":"agent-turn-complete","thread-id":"thread-e2e"}"#,
    );
    s.wait_for("the held message", || input().contains("are you there"));
    let with_quota = s.wait("the Codex account's usage", |snap| {
        snap.session
            .quotas
            .iter()
            .any(|q| q.runtime == RuntimeKind::Codex)
    });
    let quota = with_quota
        .session
        .quotas
        .iter()
        .find(|q| q.runtime == RuntimeKind::Codex)
        .unwrap();
    assert_eq!((quota.window.as_str(), quota.percent_used), ("week", 56.0));
    assert_eq!(
        s.ok(&["resume", "-C", &ws]).trim(),
        "no agent is paused at a usage limit"
    );

    // Claude Code reports a failed turn through its StopFailure hook.
    hook(
        "claude-1",
        "stop-failure",
        r#"{"hook_event_name":"StopFailure","error":"rate_limit","error_details":"You've hit your session limit · resets 11:59pm"}"#,
    );
    let parked = s.wait("claude-1 to be parked", |snap| {
        find_agent(snap, "claude-1").limit.is_some()
    });
    let limit = find_agent(&parked, "claude-1").limit.clone().unwrap();
    assert!(
        limit.resets_at.is_some() && limit.message.contains("session limit"),
        "{limit:?}"
    );
    // The crew mod reports the account's windows as the engine measures them.
    hook(
        "claude-1",
        "usage",
        r#"{"rate_limits":[{"kind":"five_hour","percentUsed":100,"resetsAt":"2030-01-01T00:00:00Z"},{"kind":"seven_day","percentUsed":41.3}]}"#,
    );
    // Before any session reported, the cache's figures stood in; what the
    // session reports since replaces them. The model's own weekly window
    // is the cache's alone, and stays.
    let fable = s.wait("the model's own weekly window", |snap| {
        snap.session.quotas.iter().any(|q| q.window == "week/Fable")
    });
    let claude: Vec<(&str, f32, i64)> = fable
        .session
        .quotas
        .iter()
        .filter(|q| q.runtime == RuntimeKind::Claude)
        .map(|q| (q.window.as_str(), q.percent_used, q.updated_at))
        .collect();
    assert_eq!(claude.len(), 3, "{claude:?}");
    assert_eq!((claude[0].0, claude[0].1), ("5h", 100.0), "{claude:?}");
    assert_eq!((claude[1].0, claude[1].1), ("week", 41.3), "{claude:?}");
    assert_eq!(claude[2], ("week/Fable", 72.0, fetched_at), "{claude:?}");
    assert!(
        claude[0].2 > fetched_at && claude[1].2 > fetched_at,
        "{claude:?}"
    );
    let status = s.ok(&["status", "-C", &ws]);
    assert!(
        status.contains(
            "usage      claude  session    ████████████████████████  100% used · resets "
        ),
        "{status}"
    );
    assert!(
        status.contains("week       ██████████░░░░░░░░░░░░░░   41% used"),
        "{status}"
    );
    assert!(
        status.contains("Fable week █████████████████░░░░░░░   72% used · resets "),
        "{status}"
    );
    assert!(!status.contains("as of"), "{status}");
    assert!(
        status.contains("codex   week       █████████████░░░░░░░░░░░   56% used"),
        "{status}"
    );
    let frame = s.ok(&["status", "--frame", "170x40", "-C", &ws]);
    assert!(
        frame.contains("claude ⊘ usage limit reached · 1 paused (claude-1)"),
        "{frame}"
    );
    // Beside the paused notice the bar is at its narrowest; alone it is wide.
    assert!(
        frame.contains("█ 100%  ↻ ") && frame.contains("  56%  ↻ "),
        "{frame}"
    );
    // The same window is in the same column for both runtimes.
    let week_at = |name: &str| {
        let line = frame
            .lines()
            .find(|l| l.starts_with(name) && l.contains("%  ↻"))
            .unwrap_or_default();
        line.find("week ").map(|at| line[..at].chars().count())
    };
    assert!(week_at(" ✱ claude ").is_some(), "{frame}");
    assert_eq!(week_at(" ✱ claude "), week_at(" ❂ codex "), "{frame}");
    // The model's own window is a column of its own, after the week.
    let claude_line = frame
        .lines()
        .find(|l| l.starts_with(" ✱ claude ") && l.contains("%  ↻"))
        .unwrap();
    assert!(
        claude_line.contains("Fable week ") && claude_line.contains("  72%  ↻ "),
        "{frame}"
    );
    assert!(
        claude_line.find("Fable week ") > claude_line.find("week "),
        "{frame}"
    );
    // The figures belong to the accounts and are reported only now and
    // then, so a daemon that is restarted still has them.
    let pid = s.snapshot().session.daemon_pid;
    s.ok(&["stop", "--keep-agents", "-C", &ws]);
    let out = crew(&["start", "-C", &ws]);
    assert!(out.status.success(), "{}", stderr(&out));
    let restarted = s.snapshot();
    assert_ne!(restarted.session.daemon_pid, pid);
    let kept: Vec<(RuntimeKind, &str, f32)> = restarted
        .session
        .quotas
        .iter()
        .map(|q| (q.runtime, q.window.as_str(), q.percent_used))
        .collect();
    assert!(
        kept.contains(&(RuntimeKind::Claude, "5h", 100.0)),
        "{kept:?}"
    );
    assert!(
        kept.contains(&(RuntimeKind::Claude, "week", 41.3)),
        "{kept:?}"
    );
    assert!(
        kept.contains(&(RuntimeKind::Codex, "week", 56.0)),
        "{kept:?}"
    );
    assert!(
        kept.contains(&(RuntimeKind::Claude, "week/Fable", 72.0)),
        "{kept:?}"
    );
    s.wait("claude-1 to be back", |snap| {
        find_agent(snap, "claude-1").status != AgentStatus::Spawning
    });

    // A session that goes on by itself (the runtime retried, or someone
    // typed into it) is no longer parked.
    hook(
        "claude-1",
        "pre-tool-use",
        r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#,
    );
    let free = s.wait("claude-1 to go on", |snap| {
        find_agent(snap, "claude-1").limit.is_none()
    });
    assert_eq!(find_agent(&free, "claude-1").status, AgentStatus::Working);

    // A login that stopped working does not lift by waiting: no automatic
    // resume, and `crew resume --restart` starts the session afresh.
    hook(
        "claude-1",
        "stop-failure",
        r#"{"error":"authentication_failed","error_details":"Not logged in"}"#,
    );
    let parked = s.wait("claude-1 to wait for a login", |snap| {
        find_agent(snap, "claude-1").limit.is_some()
    });
    let limit = find_agent(&parked, "claude-1").limit.clone().unwrap();
    assert_eq!((limit.kind.as_str(), limit.retry_at), ("auth", None));
    assert!(find_agent(&parked, "claude-1")
        .activity
        .starts_with("sign-in needed"));
    let out = s.ok(&["resume", "--restart", "-C", &ws]);
    assert!(
        out.contains("restarted, resuming their sessions: claude-1"),
        "{out}"
    );
    s.wait("claude-1 to be restarted", |snap| {
        find_agent(snap, "claude-1").limit.is_none()
    });
    assert!(s
        .events()
        .iter()
        .any(|e| e.event_type == "agent.restart" && e.agent_id.as_deref() == Some("claude-1")));

    // Any other API error just ends the turn; the agent is not parked.
    hook(
        "claude-1",
        "stop-failure",
        r#"{"error":"overloaded","error_details":"Overloaded"}"#,
    );
    s.wait_for("the failed turn to be logged", || {
        s.has_event("agent.turn_failed")
    });
    assert!(find_agent(&s.snapshot(), "claude-1").limit.is_none());
}

// ---------------------------------------------------------------------------
// Questions for the operator
// ---------------------------------------------------------------------------

#[test]
fn a_question_reaches_the_operator_through_the_orchestrator_and_the_answer_comes_back() {
    let s = Session::new();
    let ws = s.workspace.to_string_lossy().into_owned();
    let mut team = s.team(
        "ask",
        vec![agent("a", "builds", &[]), agent("b", "tests", &[])],
        vec![],
    );
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[]),
        ..OrchestratorConfig::default()
    };
    s.run(&team);
    s.wait("agents to come up", |snap| {
        snap.agents.len() == 3
            && snap
                .agents
                .iter()
                .all(|a| a.status != AgentStatus::Spawning)
    });
    assert!(s
        .ok(&["questions", "-C", &ws])
        .contains("no question is waiting for you"));

    // A member does not go to the human directly: the orchestrator is asked first.
    let out = s.ctl(
        "a",
        &["send", "operator", "which colour?", "--type", "question"],
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("put this question to orchestrator first"),
        "{}",
        stderr(&out)
    );
    let out = s.ctl(
        "a",
        &[
            "tool",
            "operator_ask",
            &json!({ "question": "which colour?" }).to_string(),
        ],
    );
    assert!(!out.status.success());
    assert!(s.snapshot().questions.is_empty());

    // What the orchestrator cannot settle it puts to the operator, saying
    // whose question it is and which answers it sees.
    let ask = |args: Value| s.ctl("orchestrator", &["tool", "operator_ask", &args.to_string()]);
    let out = ask(json!({
        "question": "Keep the legacy column order, or switch to the new schema?",
        "options": ["Keep it", " Switch\nto the new one "],
        "asked_by": "a",
    }));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("The question is with the operator"),
        "{}",
        stdout(&out)
    );
    let snapshot = s.snapshot();
    assert_eq!(snapshot.questions.len(), 1);
    let question = snapshot.questions[0].clone();
    assert_eq!(question.from, "orchestrator");
    assert_eq!(question.asked_by(), Some("a"));
    assert_eq!(question.options(), ["Keep it", "Switch to the new one"]);
    assert!(s.has_event("question.asked"));

    // Malformed questions are refused and leave nothing open.
    let refused = [
        (
            json!({ "question": "x?", "asked_by": "ghost" }),
            "no agent 'ghost'",
        ),
        (
            json!({ "question": "x?", "options": ["1", "2", "3", "4", "5", "6", "7"] }),
            "at most 6 options",
        ),
        (
            json!({ "question": "x?", "options": ["y".repeat(300)] }),
            "an option is a short answer",
        ),
    ];
    for (args, why) in refused {
        let out = ask(args);
        assert!(!out.status.success());
        assert!(stderr(&out).contains(why), "{why}: {}", stderr(&out));
    }
    assert_eq!(s.snapshot().questions.len(), 1);

    // The operator sees it in full, wherever they look.
    assert!(s
        .ok(&["status", "-C", &ws])
        .contains("questions  1 waiting for your answer"));
    let listed = s.ok(&["questions", "-C", &ws]);
    assert!(
        listed.contains(&format!("{}  from orchestrator, for a", question.id)),
        "{listed}"
    );
    assert!(
        listed.contains("Keep the legacy column order, or switch to the new schema?"),
        "{listed}"
    );
    assert!(
        listed.contains("1. Keep it") && listed.contains("2. Switch to the new one"),
        "{listed}"
    );

    // A slash command is not an answer, and neither is nothing.
    let out = s.crew(&["answer", "/exit", "-C", &ws]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("looks like a command, not an answer"),
        "{}",
        stderr(&out)
    );
    let out = s.crew(&["answer", "  ", "-C", &ws]);
    assert!(!out.status.success());
    assert_eq!(s.snapshot().questions.len(), 1);

    // A number picks a suggested answer. It reaches the orchestrator, which
    // asked, and the member who wanted to know.
    let out = s.ok(&["answer", "2", "-C", &ws]);
    assert!(
        out.contains("answered orchestrator: Switch to the new one"),
        "{out}"
    );
    assert!(s.snapshot().questions.is_empty());
    assert!(s.has_event("question.answered"));
    let messages = s.messages();
    let answers: Vec<&Message> = messages
        .iter()
        .filter(|m| m.from == "operator" && m.kind == MessageType::Answer)
        .collect();
    let to_orchestrator = answers
        .iter()
        .find(|m| m.to == "orchestrator")
        .expect("the answer");
    assert_eq!(
        to_orchestrator.reply_to.as_deref(),
        Some(question.id.as_str())
    );
    assert!(
        to_orchestrator
            .body
            .contains("Re: \"Keep the legacy column order"),
        "{}",
        to_orchestrator.body
    );
    assert!(
        to_orchestrator.body.ends_with("Switch to the new one"),
        "{}",
        to_orchestrator.body
    );
    let to_member = answers
        .iter()
        .find(|m| m.to == "a")
        .expect("the member's copy");
    assert!(
        to_member
            .body
            .contains("a question orchestrator raised for you"),
        "{}",
        to_member.body
    );
    assert!(
        to_member.body.ends_with("Switch to the new one"),
        "{}",
        to_member.body
    );
    assert_eq!(answers.len(), 2);

    // An answered question cannot be answered again.
    let out = s.crew(&["answer", &question.id, "again", "-C", &ws]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no open question"),
        "{}",
        stderr(&out)
    );

    // With several open, the answer names its question.
    assert!(ask(json!({ "question": "First?" })).status.success());
    assert!(ask(json!({ "question": "Second?" })).status.success());
    let open = s.snapshot().questions;
    assert_eq!(open.len(), 2);
    let out = s.crew(&["answer", "yes", "-C", &ws]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("2 questions are open"),
        "{}",
        stderr(&out)
    );
    let out = s.ok(&["answer", &open[1].id, "in my own words", "-C", &ws]);
    assert!(
        out.contains("answered orchestrator: in my own words"),
        "{out}"
    );
    let left = s.snapshot().questions;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].body, "First?");
    assert!(s
        .ok(&["answer", "yes", "-C", &ws])
        .contains("answered orchestrator"));

    // A final report says what it leaves for the operator to decide. Each
    // decision is asked of them, so the team is seen to wait instead of
    // looking finished.
    let complete = |args: Value| {
        s.ctl(
            "orchestrator",
            &["tool", "session_complete", &args.to_string()],
        )
    };
    let report = json!({
        "summary": "Built and reviewed. Two things are yours to decide.",
        "decisions": [
            "Which TRL version to pin? I recommend 1.2.0.",
            " Contact the authors about concurrent work? ",
            "",
            "Which TRL version to pin? I recommend 1.2.0.",
        ],
    });
    let out = complete(report.clone());
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("2 decision(s) are with the operator"),
        "{}",
        stdout(&out)
    );
    let snapshot = s.snapshot();
    assert_eq!(
        snapshot
            .questions
            .iter()
            .map(|q| (q.from.as_str(), q.body.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                "orchestrator",
                "Which TRL version to pin? I recommend 1.2.0."
            ),
            ("orchestrator", "Contact the authors about concurrent work?"),
        ]
    );
    assert_eq!(
        snapshot.session.summary.as_deref(),
        Some("Built and reviewed. Two things are yours to decide.")
    );
    let frame = s.ok(&["status", "--frame", "120x40", "-C", &ws]);
    assert!(frame.contains("? 2 questions for you"), "{frame}");
    assert!(frame.contains("│ ? 2 answers needed"), "{frame}");

    // The report sent again asks nothing twice, not even what has been
    // answered in the meantime.
    let out = s.ok(&["answer", &snapshot.questions[0].id, "1.2.0", "-C", &ws]);
    assert!(out.contains("answered orchestrator: 1.2.0"), "{out}");
    let out = complete(report);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("decision(s)"), "{}", stdout(&out));
    assert_eq!(s.snapshot().questions.len(), 1);
    assert!(s
        .ok(&["status", "--frame", "120x40", "-C", &ws])
        .contains("│ ? your answer needed"));

    // Too many decisions are refused before anything of the report is kept.
    let many: Vec<String> = (0..13).map(|i| format!("Decision {i}?")).collect();
    let out = complete(json!({ "summary": "another report", "decisions": many }));
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("at most 12 decisions"),
        "{}",
        stderr(&out)
    );
    let snapshot = s.snapshot();
    assert_eq!(snapshot.questions.len(), 1);
    assert!(snapshot
        .session
        .summary
        .as_deref()
        .unwrap()
        .starts_with("Built and reviewed."));

    // A shell agent keeps no session file, so it has no transcript.
    let out = s.crew(&["agent", "transcript", "a", "-C", &ws]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("a has no session transcript"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn without_an_orchestrator_members_ask_the_operator_themselves() {
    let s = Session::new();
    let ws = s.workspace.to_string_lossy().into_owned();
    let team = s.team(
        "flat",
        vec![agent("a", "", &[]), agent("b", "", &[])],
        vec![],
    );
    s.run(&team);
    s.wait("agents to come up", |snap| {
        snap.agents
            .iter()
            .all(|a| a.status != AgentStatus::Spawning)
    });
    let out = s.ctl(
        "a",
        &["send", "operator", "Tabs or spaces?", "--type", "question"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    // An ordinary message to the operator is not a question to answer.
    assert!(s
        .ctl("b", &["send", "operator", "fyi: done with the parser"])
        .status
        .success());
    let open = s.snapshot().questions;
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open[0].from.as_str(), open[0].body.as_str()),
        ("a", "Tabs or spaces?")
    );

    assert!(s
        .ok(&["answer", "spaces", "-C", &ws])
        .contains("answered a: spaces"));
    assert!(s.snapshot().questions.is_empty());
    let messages = s.messages();
    let answers: Vec<&Message> = messages
        .iter()
        .filter(|m| m.from == "operator" && m.kind == MessageType::Answer)
        .collect();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].to, "a");
}

// ---------------------------------------------------------------------------
// Context windows and compaction
// ---------------------------------------------------------------------------

#[test]
fn the_orchestrator_sees_full_context_windows_and_queues_compactions() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    let ws = s.workspace.to_string_lossy().into_owned();
    // Stand-ins for both CLIs that record what is typed at them.
    for name in ["codex", "claude"] {
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join(name), FAKE_LIMITED_CODEX).unwrap();
        std::fs::set_permissions(
            root.join("bin").join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let agent_dir = |id: &str| s.workspace.join(".crew/agents").join(id);
    let input =
        |id: &str| std::fs::read_to_string(agent_dir(id).join("fake-input")).unwrap_or_default();
    // The session file Codex keeps: its context is 77% full.
    let day = chrono::Local::now().format("%Y/%m/%d").to_string();
    let sessions = root.join("codex-home/sessions").join(day);
    std::fs::create_dir_all(&sessions).unwrap();
    let rollout = sessions.join("rollout-2026-01-01T00-00-00-thread-ctx.jsonl");
    let token_count = |tokens: u64| {
        json!({ "type": "event_msg", "payload": { "type": "token_count", "info": {
            "last_token_usage": { "total_tokens": tokens }, "model_context_window": 258_400,
        }}})
        .to_string()
    };
    // It also records what each turn runs on.
    let turn =
        json!({ "type": "turn_context", "payload": { "model": "gpt-6-luna", "effort": "max" } });
    std::fs::write(&rollout, format!("{turn}\n{}\n", token_count(200_000))).unwrap();

    let mut team = s.team("context", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[]),
        ..OrchestratorConfig::default()
    };
    for (id, runtime, task) in [
        ("codex-1", RuntimeKind::Codex, "review the parser"),
        ("claude-1", RuntimeKind::Claude, "fix the parser"),
    ] {
        team.agents.push(AgentSpec {
            id: id.to_string(),
            runtime: Some(runtime),
            task: Some(task.to_string()),
            ..AgentSpec::default()
        });
    }
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = s
        .command()
        .env("PATH", path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("agents to come up", |snap| {
        snap.agents.len() == 3
            && snap
                .agents
                .iter()
                .all(|a| a.status != AgentStatus::Spawning)
    });

    let identity = |id: &str| s.identity_path(id).to_string_lossy().into_owned();
    let hook = |agent: &str, event: &str, payload: &str| {
        let out = s.crew(&["hook", event, "--identity", &identity(agent), payload]);
        assert!(out.status.success(), "{event}: {}", stderr(&out));
    };
    let told = |to: &str, text: &str| {
        s.messages()
            .iter()
            .any(|m| m.from == "scheduler" && m.to == to && m.body.contains(text))
    };
    let compact =
        |agent: &str, args: Value| s.ctl(agent, &["tool", "agent_compact", &args.to_string()]);

    // Claude Code reports how full its window is through the crew mod. Past
    // the threshold the orchestrator is told, once.
    hook(
        "claude-1",
        "session-start",
        r#"{"session_id":"sess-ctx","transcript_path":"/nonexistent","model":"claude-sonnet-5-5"}"#,
    );
    // What the session runs on: the model from its start, the effort from
    // the hooks of a turn. Both end up on the agent's box.
    hook(
        "claude-1",
        "pre-tool-use",
        r#"{"tool_name":"Bash","tool_input":{"command":"ls"},"effort":{"level":"high"}}"#,
    );
    let snap = s.wait("claude-1's model and effort", |snap| {
        find_agent(snap, "claude-1").effort.is_some()
    });
    let claude = find_agent(&snap, "claude-1");
    assert_eq!(
        (claude.running_model.as_deref(), claude.effort.as_deref()),
        (Some("claude-sonnet-5-5"), Some("high"))
    );
    assert_eq!(claude.model_name.as_deref(), Some("Sonnet 5.5"));
    assert!(s
        .ok(&["status", "--frame", "120x40", "-C", &ws])
        .contains(" Sonnet 5.5 · high "));
    hook(
        "claude-1",
        "usage",
        r#"{"rate_limits":[],"context":{"tokens":90000,"window":200000}}"#,
    );
    let snap = s.wait("claude-1's context", |snap| {
        find_agent(snap, "claude-1").context.is_some()
    });
    assert_eq!(
        find_agent(&snap, "claude-1")
            .context
            .as_ref()
            .unwrap()
            .percent,
        45
    );
    assert!(!told("orchestrator", "claude-1's context window"));
    hook(
        "claude-1",
        "usage",
        r#"{"rate_limits":[],"context":{"tokens":160000,"window":200000}}"#,
    );
    s.wait_for("the orchestrator to be warned", || {
        told(
            "orchestrator",
            "claude-1's context window is 80% full (160k of 200k tokens)",
        )
    });
    hook(
        "claude-1",
        "usage",
        r#"{"rate_limits":[],"context":{"tokens":164000,"window":200000}}"#,
    );
    s.wait("the newer reading", |snap| {
        find_agent(snap, "claude-1")
            .context
            .as_ref()
            .is_some_and(|c| c.percent == 82)
    });
    let warnings = s
        .messages()
        .iter()
        .filter(|m| m.to == "orchestrator" && m.body.contains("claude-1's context window is"))
        .count();
    assert_eq!(warnings, 1, "one warning per filling");
    assert!(s.has_event("agent.context_full"));
    assert!(
        s.ok(&["status", "-C", &ws])
            .contains("claude-1 [working 0% · ctx 82%]"),
        "{}",
        s.ok(&["status", "-C", &ws])
    );

    // The runtime compacted on its own (the window was full): the agent is
    // pointed back at its task and the orchestrator hears of it.
    hook(
        "claude-1",
        "post-compact",
        r#"{"hook_event_name":"PostCompact","compact_summary":"..."}"#,
    );
    let snap = s.wait("the compaction to be noticed", |snap| {
        find_agent(snap, "claude-1").compactions == 1
    });
    assert!(
        find_agent(&snap, "claude-1").context.is_none(),
        "the old figure is gone"
    );
    s.wait_for("the agent to be told where it stands", || {
        told("claude-1", "Your conversation was just compacted") && told("claude-1", "Your open task is claude-1-task (\"fix the parser\"): task_get(\"claude-1-task\") reads its card.")
    });
    s.wait_for("the orchestrator to hear of it", || {
        told(
            "orchestrator",
            "claude-1's context was compacted by its runtime",
        )
    });

    // Who may compact whom: an agent itself, the orchestrator anyone, and
    // only sessions that have a context.
    let out = compact("codex-1", json!({ "id": "claude-1" }));
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("only the orchestrator compacts a teammate's"),
        "{}",
        stderr(&out)
    );
    let out = compact("orchestrator", json!({}));
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no context to compact"),
        "{}",
        stderr(&out)
    );
    let out = compact("orchestrator", json!({ "id": "ghost" }));
    assert!(
        stderr(&out).contains("no agent 'ghost'"),
        "{}",
        stderr(&out)
    );

    // Codex: the figure comes from its session file, and the operator
    // queues the compaction. The agent is at rest, so it is asked at once.
    hook(
        "codex-1",
        "codex-notify",
        r#"{"type":"agent-turn-complete","thread-id":"thread-ctx"}"#,
    );
    let snap = s.wait("codex-1's context", |snap| {
        find_agent(snap, "codex-1").context.is_some()
    });
    assert_eq!(
        find_agent(&snap, "codex-1")
            .context
            .as_ref()
            .unwrap()
            .percent,
        77
    );
    let codex = find_agent(&snap, "codex-1");
    assert_eq!(
        (codex.running_model.as_deref(), codex.effort.as_deref()),
        (Some("gpt-6-luna"), Some("max"))
    );
    assert_eq!(codex.model_name.as_deref(), Some("GPT 6 Luna"));
    assert!(s
        .ok(&["status", "--frame", "120x40", "-C", &ws])
        .contains(" GPT 6 Luna · max "));
    s.wait_for("the warning about codex-1", || {
        told("orchestrator", "codex-1's context window is 77% full")
    });
    let out = s.ok(&["agent", "compact", "codex-1", "-C", &ws]);
    assert!(
        out.contains("queued a compaction of codex-1's context"),
        "{out}"
    );
    s.wait_for("the compact command at codex-1", || {
        input("codex-1").lines().any(|l| l == "/compact")
    });
    assert!(find_agent(&s.snapshot(), "codex-1").compact_pending);
    // Its window shrinking is how the compaction shows. It is told where
    // the work stands; its standing instructions are in its system prompt,
    // which the compaction keeps, so they are not sent again.
    let mut log = std::fs::read_to_string(&rollout).unwrap();
    log.push_str(&format!("{}\n", token_count(17_000)));
    std::fs::write(&rollout, log).unwrap();
    let snap = s.wait("codex-1's compaction", |snap| {
        find_agent(snap, "codex-1").compactions == 1
    });
    let codex = find_agent(&snap, "codex-1");
    assert!(!codex.compact_pending);
    s.wait_for("where the work stands", || {
        input("codex-1").contains("Your conversation was just compacted")
    });
    assert!(
        !input("codex-1").contains("standing instructions"),
        "{}",
        input("codex-1")
    );
    assert!(
        input("codex-1").contains("Your open task is codex-1-task (\"review the parser\")"),
        "{}",
        input("codex-1")
    );
    assert_eq!(
        input("codex-1")
            .lines()
            .filter(|l| *l == "/compact")
            .count(),
        1
    );
    s.wait("the new figure", |snap| {
        find_agent(snap, "codex-1")
            .context
            .as_ref()
            .is_some_and(|c| c.percent == 6)
    });

    // The orchestrator queues one for claude-1, saying what to keep. The
    // agent is in the middle of a turn: nothing is typed until it ends.
    hook("claude-1", "user-prompt-submit", "{}");
    s.wait("claude-1 to be working", |snap| {
        find_agent(snap, "claude-1").status == AgentStatus::Working
    });
    let out = compact(
        "orchestrator",
        json!({ "id": "claude-1", "keep": "the parser task\nand the failing test" }),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("A compaction of claude-1's context is queued; it runs when its current turn ends, keeping: the parser task and the failing test"),
        "{}",
        stdout(&out)
    );
    assert!(s.ok(&["status", "-C", &ws]).contains("compaction queued"));
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(
        !input("claude-1").contains("/compact"),
        "{}",
        input("claude-1")
    );
    hook("claude-1", "stop", "{}");
    s.wait_for("the compact command at claude-1", || {
        input("claude-1")
            .lines()
            .any(|l| l == "/compact the parser task and the failing test")
    });
    hook("claude-1", "post-compact", "{}");
    let snap = s.wait("claude-1's second compaction", |snap| {
        find_agent(snap, "claude-1").compactions == 2
    });
    assert!(!find_agent(&snap, "claude-1").compact_pending);
    s.wait_for("what was to be kept", || {
        told(
            "claude-1",
            "What was to be kept in mind: the parser task and the failing test",
        )
    });
    let events = s.events();
    assert!(events.iter().any(
        |e| e.event_type == "agent.compact_queued" && e.message.contains("orchestrator queued")
    ));
    assert!(events.iter().any(|e| e.event_type == "agent.compacted"
        && e.message.contains("claude-1")
        && e.message.contains("as queued")));
    assert!(events
        .iter()
        .any(|e| e.event_type == "agent.compacted" && e.message.contains("by its runtime")));
    // A compaction asked for is not also reported to the orchestrator as a surprise.
    let surprises = s
        .messages()
        .iter()
        .filter(|m| m.to == "orchestrator" && m.body.contains("compacted by its runtime"))
        .count();
    assert_eq!(surprises, 1);
}

#[test]
fn a_claude_session_is_read_from_its_transcript_when_nothing_else_reports() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    let ws = s.workspace.to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_RUNTIME).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let mut team = s.team("quiet", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.agents.push(AgentSpec {
        id: "quiet".to_string(),
        runtime: Some(RuntimeKind::Claude),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = s
        .command()
        .env("PATH", path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    // No hook fires and no mod reports (an idle session after a daemon
    // restart, say): nothing is known about what the agent runs on.
    let snap = s.wait("the agent", |snap| {
        snap.agents
            .iter()
            .any(|a| a.id == "quiet" && a.session_id.is_some())
    });
    let quiet = find_agent(&snap, "quiet");
    assert_eq!(
        (
            quiet.running_model.as_deref(),
            quiet.effort.as_deref(),
            &quiet.context
        ),
        (None, None, &None)
    );

    // Claude Code keeps a transcript of the session, which says.
    let project = root.join("claude-home/projects/-work-quiet");
    std::fs::create_dir_all(&project).unwrap();
    let reply = json!({
        "type": "assistant", "isSidechain": false, "effort": "xhigh",
        "message": { "model": "claude-fable-5-1", "usage": {
            "input_tokens": 4, "cache_read_input_tokens": 120_000, "cache_creation_input_tokens": 996, "output_tokens": 200,
        }},
    });
    std::fs::write(
        project.join(format!("{}.jsonl", quiet.session_id.as_deref().unwrap())),
        format!("{reply}\n"),
    )
    .unwrap();
    let snap = s.wait("the transcript to be read", |snap| {
        find_agent(snap, "quiet").effort.is_some()
    });
    let quiet = find_agent(&snap, "quiet");
    assert_eq!(
        (
            quiet.running_model.as_deref(),
            quiet.model_name.as_deref(),
            quiet.effort.as_deref()
        ),
        (Some("claude-fable-5-1"), Some("Fable 5.1"), Some("xhigh"))
    );
    // It says how many tokens are in the window, not how big the window is.
    let snap = s.wait("the context", |snap| {
        find_agent(snap, "quiet").context.is_some()
    });
    let context = find_agent(&snap, "quiet").context.clone().unwrap();
    assert_eq!(
        (context.tokens, context.window, context.percent),
        (121_000, 0, 0)
    );
    assert!(
        s.ok(&["status", "-C", &ws]).contains("ctx 121k tokens"),
        "{}",
        s.ok(&["status", "-C", &ws])
    );
    let frame = s.ok(&["status", "--frame", "120x40", "-C", &ws]);
    assert!(
        frame.contains("121k tokens") && frame.contains(" Fable 5.1 · xhigh "),
        "{frame}"
    );
    // More tokens than the standard window holds: it can only be the 1M one.
    let transcript = project.join(format!("{}.jsonl", quiet.session_id.as_deref().unwrap()));
    let mut text = std::fs::read_to_string(&transcript).unwrap();
    text.push_str(&reply.to_string().replace("120000", "300000"));
    text.push('\n');
    std::fs::write(&transcript, text).unwrap();
    let snap = s.wait("the larger reading", |snap| {
        find_agent(snap, "quiet")
            .context
            .as_ref()
            .is_some_and(|c| c.tokens == 301_000)
    });
    let context = find_agent(&snap, "quiet").context.clone().unwrap();
    assert_eq!((context.window, context.percent), (1_000_000, 30));

    // The same file is the agent's transcript: what it was told, what it
    // wrote and the tools it called, with nothing read off its terminal.
    let mut text = std::fs::read_to_string(&transcript).unwrap();
    for line in [
        json!({ "type": "user", "timestamp": "2026-10-02T19:46:31.613Z", "message": { "role": "user",
            "content": "The crew-bridge plugin sent a message:\n[crew] Message from operator (type task, id m-1):\n\nCheck the build." } }),
        json!({ "type": "assistant", "message": { "role": "assistant", "content": [
            { "type": "thinking", "thinking": "let me see" },
            { "type": "text", "text": "Running the build." },
            { "type": "tool_use", "name": "Bash", "input": { "command": "cargo build --workspace" } },
        ] } }),
        json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "content": "Finished" }] } }),
        json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": "It builds." }] } }),
    ] {
        text.push_str(&line.to_string());
        text.push('\n');
    }
    std::fs::write(&transcript, text).unwrap();
    let printed = s.ok(&["agent", "transcript", "quiet", "-C", &ws]);
    let rest = printed
        .strip_prefix("── prompt · ")
        .unwrap_or_else(|| panic!("{printed}"));
    assert_eq!(
        rest.split_once('\n').unwrap().1,
        "[crew] Message from operator (type task, id m-1):\n\nCheck the build.\n\n\
Running the build.\n\n▸ Bash  cargo build --workspace\n\nIt builds.\n",
        "{printed}"
    );
    assert_eq!(
        s.ok(&["agent", "transcript", "quiet", "-n", "1", "-C", &ws]),
        "[earlier entries not shown]\nIt builds.\n"
    );

    // Once the runtime itself reports the window, its word stands.
    let identity = s.identity_path("quiet").to_string_lossy().into_owned();
    let out = s.crew(&[
        "hook",
        "usage",
        "--identity",
        &identity,
        r#"{"rate_limits":[],"context":{"tokens":310000,"window":1000000}}"#,
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let snap = s.wait("the reported window", |snap| {
        find_agent(snap, "quiet")
            .context
            .as_ref()
            .is_some_and(|c| c.window == 1_000_000)
    });
    assert_eq!(
        find_agent(&snap, "quiet").context.as_ref().unwrap().percent,
        31
    );
}

// ---------------------------------------------------------------------------
// A session moved to another model
// ---------------------------------------------------------------------------

#[test]
fn a_session_moved_to_another_model_is_noticed_and_the_member_can_be_put_on_one() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    let ws = s.workspace.to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_RUNTIME).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let crew = |args: &[&str]| {
        s.command()
            .env("PATH", &path)
            .args(args)
            .output()
            .expect("running crew")
    };
    let agent_dir = |id: &str| s.workspace.join(".crew/agents").join(id);
    let read = |id: &str, file: &str| {
        std::fs::read_to_string(agent_dir(id).join(file)).unwrap_or_default()
    };

    let mut team = s.team("models", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[]),
        ..OrchestratorConfig::default()
    };
    // One member on a chosen model, one on the runtime's default.
    team.agents.push(AgentSpec {
        id: "design".to_string(),
        runtime: Some(RuntimeKind::Claude),
        model: Some("fable".to_string()),
        task: Some("design the thing".to_string()),
        ..AgentSpec::default()
    });
    team.agents.push(AgentSpec {
        id: "writer".to_string(),
        runtime: Some(RuntimeKind::Claude),
        task: Some("write it up".to_string()),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = crew(&["run", "team.yaml", "--no-attach"]);
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("agents to come up", |snap| {
        snap.agents.len() == 3
            && snap
                .agents
                .iter()
                .all(|a| a.status != AgentStatus::Spawning)
    });
    let identity = |id: &str| s.identity_path(id).to_string_lossy().into_owned();
    let hook = |agent: &str, event: &str, payload: &str| {
        let out = s.crew(&["hook", event, "--identity", &identity(agent), payload]);
        assert!(out.status.success(), "{event}: {}", stderr(&out));
    };
    let told = |text: &str| {
        s.messages()
            .iter()
            .any(|m| m.from == "scheduler" && m.to == "orchestrator" && m.body.contains(text))
    };
    // By default the engine may substitute a model (crew is told); the
    // member runs on its model with the 1M window.
    assert!(
        !read("design", "fake-env").contains("CLAUDE_CODE_NO_MODEL_FALLBACK"),
        "{}",
        read("design", "fake-env")
    );
    assert!(
        read("design", "fake-args").contains("--model\nfable[1m]\n"),
        "{}",
        read("design", "fake-args")
    );

    // The sessions announce themselves (so a restart can resume them).
    for id in ["design", "writer"] {
        let session = find_agent(&s.snapshot(), id).session_id.clone().unwrap();
        hook(
            id,
            "session-start",
            &json!({ "session_id": session, "transcript_path": "/nonexistent" }).to_string(),
        );
    }
    // The crew mod reports the model of each request. On its own model,
    // nothing is amiss.
    hook(
        "design",
        "step",
        r#"{"model":"claude-fable-5-1[1m]","effort":"xhigh"}"#,
    );
    let snap = s.wait("design's model", |snap| {
        find_agent(snap, "design").running_model.is_some()
    });
    assert!(find_agent(&snap, "design").fallback.is_none());
    assert_eq!(
        find_agent(&snap, "design").model_name.as_deref(),
        Some("Fable 5.1")
    );

    // Claude Code's transcript holds its word on the switch it is about to
    // report: the model's weekly window is used up.
    let session = find_agent(&snap, "design").session_id.clone().unwrap();
    let project = root.join("claude-home/projects/-work-design");
    std::fs::create_dir_all(&project).unwrap();
    let consent = json!({ "type": "system", "subtype": "model_consent_fallback", "level": "warning",
        "content": "Switched to Opus 5.5 for this session · Fable 5.1 requires usage credits · /model to change",
        "choice": "switch_default", "originalModel": "claude-fable-5-1[1m]", "fallbackModel": "claude-opus-5-5[1m]" });
    let reply = json!({ "type": "assistant", "isSidechain": false, "effort": "xhigh",
        "message": { "model": "claude-opus-5-5", "usage": { "input_tokens": 4, "cache_read_input_tokens": 1000 } } });
    std::fs::write(
        project.join(format!("{session}.jsonl")),
        format!("{consent}\n{reply}\n"),
    )
    .unwrap();
    // The next request runs on another model: the member is marked, the
    // log says so, and the orchestrator hears what happened and what it
    // can do, with the accounts' usage.
    hook(
        "design",
        "usage",
        r#"{"rate_limits":[{"kind":"seven_day","percentUsed":74},{"kind":"five_hour","percentUsed":1}]}"#,
    );
    hook(
        "design",
        "step",
        r#"{"model":"claude-opus-5-5[1m]","effort":"xhigh"}"#,
    );
    let snap = s.wait("the fallback to be noticed", |snap| {
        find_agent(snap, "design").fallback.is_some()
    });
    let design = find_agent(&snap, "design");
    assert_eq!(design.fallback.as_ref().unwrap().from, "Fable 5.1");
    assert_eq!(design.model_name.as_deref(), Some("Opus 5.5"));
    s.wait("the runtime's note on it", |snap| {
        find_agent(snap, "design")
            .fallback
            .as_ref()
            .is_some_and(|f| !f.note.is_empty())
    });
    assert_eq!(
        find_agent(&s.snapshot(), "design").fallback.as_ref().unwrap().note,
        "Switched to Opus 5.5 for this session · Fable 5.1 requires usage credits · /model to change"
    );
    assert!(
        s.events()
            .iter()
            .any(|e| e.event_type == "agent.model_fallback"
                && e.message == "design runs on Opus 5.5 instead of Fable 5.1"
                && e.level == crew_core::types::EventLevel::Warn),
        "{:?}",
        s.events()
    );
    s.wait_for("the orchestrator to be told", || {
        told("Agent design (Claude Code) now runs on Opus 5.5 instead of Fable 5.1")
    });
    let brief = s
        .messages()
        .into_iter()
        .find(|m| m.to == "orchestrator" && m.body.contains("now runs on Opus 5.5"))
        .unwrap()
        .body;
    assert!(brief.contains("(Switched to Opus 5.5 for this session · Fable 5.1 requires usage credits · /model to change)"), "{brief}");
    assert!(
        brief.contains("agent_set_model(id=\"design\", model=..., note=...)"),
        "{brief}"
    );
    assert!(
        brief.contains("agent_add a member on a model that is not affected"),
        "{brief}"
    );
    assert!(brief.contains("Usage now: week 74%"), "{brief}");
    // It shows everywhere the member does.
    let status = s.ok(&["status", "-C", &ws]);
    assert!(
        status.contains(
            "fallback   design runs on Opus 5.5 instead of Fable 5.1 — Switched to Opus 5.5"
        ),
        "{status}"
    );
    assert!(
        status.contains("crew agent model design <model> to choose"),
        "{status}"
    );
    let frame = s.ok(&["status", "--frame", "140x40", "-C", &ws]);
    assert!(frame.contains(" ↓ Opus 5.5 · xhigh "), "{frame}");
    let list = s.ctl("orchestrator", &["tool", "agents_list", "{}"]);
    assert!(
        String::from_utf8_lossy(&list.stdout)
            .contains("design [working 0% · on Opus 5.5 instead of Fable 5.1]"),
        "{}",
        String::from_utf8_lossy(&list.stdout)
    );

    // The orchestrator decides on a model: the member's session is restarted
    // on it, resumed, with the note as the first thing it reads; the roster
    // keeps the choice.
    let out = s.ctl(
        "orchestrator",
        &[
            "tool",
            "agent_set_model",
            &json!({ "id": "design", "model": "sonnet",
            "note": "Keep the design to one page; skip the appendix." })
            .to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("design is being restarted on Sonnet 5.5"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    s.wait_for("design to restart on sonnet", || {
        read("design", "fake-args").contains("--model\nsonnet[1m]\n")
    });
    let args = read("design", "fake-args");
    assert!(args.contains("--resume\n"), "{args}");
    assert!(args.contains("[crew] Your session was restarted on Sonnet 5.5 by orchestrator. Keep the design to one page; skip the appendix. Continue your current task"), "{args}");
    let snap = s.wait("design to be back", |snap| {
        find_agent(snap, "design").status != AgentStatus::Spawning
    });
    let design = find_agent(&snap, "design");
    assert_eq!(
        (
            design.model.as_deref(),
            design.model_name.as_deref(),
            &design.fallback
        ),
        (Some("sonnet"), Some("Sonnet 5.5"), &None)
    );
    assert!(
        s.events().iter().any(|e| e.event_type == "agent.model_set"
            && e.message
                .starts_with("design moves from fable to sonnet (orchestrator): Keep the design")),
        "{:?}",
        s.events()
    );
    let roster =
        crew_core::config::TeamConfig::load(&s.workspace.join(".crew/roster.yaml")).unwrap();
    assert_eq!(
        roster
            .agents
            .iter()
            .find(|a| a.id == "design")
            .unwrap()
            .model
            .as_deref(),
        Some("sonnet")
    );
    // On the chosen model there is nothing to report.
    hook(
        "design",
        "step",
        r#"{"model":"claude-sonnet-5-5[1m]","effort":"high"}"#,
    );
    let snap = s.wait("design's new model", |snap| {
        find_agent(snap, "design").effort.as_deref() == Some("high")
    });
    assert!(find_agent(&snap, "design").fallback.is_none());
    // A model of the other CLI is not a model change.
    let out = s.ctl(
        "orchestrator",
        &[
            "tool",
            "agent_set_model",
            &json!({ "id": "design", "model": "gpt-6-luna" }).to_string(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("gpt-6-luna runs on Codex, not Claude Code"),
        "{}",
        stderr(&out)
    );

    // A member on the runtime's default is held to the model it started
    // on; back on it, the mark goes and the orchestrator hears.
    hook(
        "writer",
        "step",
        r#"{"model":"claude-opus-5-5","effort":"high"}"#,
    );
    s.wait("writer's model", |snap| {
        find_agent(snap, "writer").running_model.is_some()
    });
    hook(
        "writer",
        "step",
        r#"{"model":"claude-sonnet-5-5","effort":"high"}"#,
    );
    let snap = s.wait("writer's fallback", |snap| {
        find_agent(snap, "writer").fallback.is_some()
    });
    assert_eq!(
        find_agent(&snap, "writer").fallback.as_ref().unwrap().from,
        "Opus 5.5"
    );
    hook(
        "writer",
        "step",
        r#"{"model":"claude-opus-5-5","effort":"high"}"#,
    );
    s.wait("writer back on its model", |snap| {
        find_agent(snap, "writer").fallback.is_none()
    });
    s.wait_for("the orchestrator to hear it is back", || {
        told("Agent writer is back on Opus 5.5.")
    });
    assert!(s.has_event("agent.model_restored"));

    // The operator has the same choice from the command line.
    let out = s.ok(&[
        "agent",
        "model",
        "writer",
        "haiku",
        "--note",
        "Short sentences.",
        "-C",
        &ws,
    ]);
    assert_eq!(out.trim(), "restarting writer on Haiku 4.5");
    s.wait_for("writer to restart on haiku", || {
        read("writer", "fake-args").contains("--model\nhaiku\n")
    });
    assert!(
        read("writer", "fake-args")
            .contains("restarted on Haiku 4.5 by operator. Short sentences."),
        "{}",
        read("writer", "fake-args")
    );

    // A team may forbid the engine substituting a model at all: the turn
    // fails at the limit instead, and the agent is parked (see the usage
    // limit test).
    s.ok(&["stop", "-C", &ws]);
    team.defaults.model_fallback = false;
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = crew(&["run", "team.yaml", "--no-attach"]);
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait_for("design to start without substitution", || {
        read("design", "fake-env").contains("CLAUDE_CODE_NO_MODEL_FALLBACK=1")
    });
}

// ---------------------------------------------------------------------------
// pi members
// ---------------------------------------------------------------------------

#[test]
fn a_pi_member_is_started_with_crews_extension_and_reports_through_it() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    let ws = s.workspace.to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/pi"), FAKE_RUNTIME).unwrap();
    std::fs::set_permissions(root.join("bin/pi"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let agent_dir = |id: &str| s.workspace.join(".crew/agents").join(id);
    let read = |id: &str, file: &str| {
        std::fs::read_to_string(agent_dir(id).join(file)).unwrap_or_default()
    };

    let mut team = s.team("pi", vec![], vec![]);
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[]),
        ..OrchestratorConfig::default()
    };
    team.agents.push(AgentSpec {
        id: "pi-1".to_string(),
        runtime: Some(RuntimeKind::Pi),
        model: Some("anthropic/claude-opus-4-6:high".to_string()),
        task: Some("try things".to_string()),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("PATH", &path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("the pi member to start", |snap| {
        !read("pi-1", "fake-args").is_empty() && snap.agents.len() == 2
    });
    let snap = s.snapshot();
    let pi = find_agent(&snap, "pi-1");
    assert_eq!(pi.runtime, RuntimeKind::Pi);
    // pi is started with crew's extension, the standing instructions, a
    // session of crew's choosing in the agent's own directory, no trust
    // dialog, the model as pi names it, and the first prompt last.
    let args = read("pi-1", "fake-args");
    let extension = agent_dir("pi-1").join("crew-pi.ts");
    assert!(
        args.contains(&format!("--extension\n{}\n", extension.display())),
        "{args}"
    );
    assert!(
        args.contains(&format!(
            "--append-system-prompt\n{}\n",
            agent_dir("pi-1").join("briefing.md").display()
        )),
        "{args}"
    );
    assert!(
        args.contains(&format!(
            "--session-dir\n{}\n",
            agent_dir("pi-1").join("pi-sessions").display()
        )),
        "{args}"
    );
    let session = pi.session_id.clone().expect("a session id of crew's");
    assert!(
        args.contains(&format!("--session-id\n{session}\n")),
        "{args}"
    );
    assert!(
        args.contains("--no-approve\n--model\nanthropic/claude-opus-4-6:high\n--\n"),
        "{args}"
    );
    assert!(
        args.ends_with("Task pi-1-task is yours: \"try things\".\n"),
        "{args}"
    );
    assert!(
        read("pi-1", "fake-env").contains("CREW_IDENTITY="),
        "{}",
        read("pi-1", "fake-env")
    );
    let source = std::fs::read_to_string(&extension).unwrap();
    assert!(
        source.contains("registerMcpServer(\"crew\"") && !source.contains("__IDENTITY__"),
        "{source}"
    );
    let briefing = std::fs::read_to_string(agent_dir("pi-1").join("briefing.md")).unwrap();
    assert!(briefing.contains("You are \"pi-1\""), "{briefing}");
    // The orchestrator is told it may staff pi members.
    s.wait_for("the orchestrator's briefing", || {
        agent_dir("orchestrator").join("briefing.md").exists()
    });
    let orchestrator =
        std::fs::read_to_string(agent_dir("orchestrator").join("briefing.md")).unwrap();
    assert!(
        orchestrator.contains("A member may also run on pi (`runtime: \"pi\"`)"),
        "{orchestrator}"
    );

    // The extension reports the session's life: what it runs on, each
    // turn and tool call, how full the context is, and a failed turn.
    let identity = s.identity_path("pi-1").to_string_lossy().into_owned();
    let hook = |event: &str, payload: &str| {
        let out = s.crew(&["hook", event, "--identity", &identity, payload]);
        assert!(out.status.success(), "{event}: {}", stderr(&out));
    };
    let transcript = agent_dir("pi-1")
        .join("pi-sessions")
        .join(format!("2026-10-02T19-46-31-000Z_{session}.jsonl"));
    hook(
        "session-start",
        &json!({ "session_id": session, "transcript_path": transcript, "model": "anthropic/claude-opus-4-6" }).to_string(),
    );
    hook(
        "model",
        r#"{"model":"anthropic/claude-opus-4-6","effort":"high"}"#,
    );
    let snap = s.wait("the model to be known", |snap| {
        find_agent(snap, "pi-1").effort.is_some()
    });
    let pi = find_agent(&snap, "pi-1");
    assert_eq!(
        (
            pi.running_model.as_deref(),
            pi.model_name.as_deref(),
            pi.effort.as_deref()
        ),
        (
            Some("anthropic/claude-opus-4-6"),
            Some("Opus 4.6"),
            Some("high")
        )
    );
    assert!(
        pi.fallback.is_none(),
        "the model it was given, under pi's name"
    );
    hook("turn-start", "{}");
    hook(
        "tool-start",
        r#"{"tool_name":"bash","tool_input":{"command":"cargo test"}}"#,
    );
    let snap = s.wait("the activity", |snap| {
        find_agent(snap, "pi-1").activity == "$ cargo test"
    });
    assert_eq!(find_agent(&snap, "pi-1").status, AgentStatus::Working);
    hook("usage", r#"{"context":{"tokens":50000,"window":200000}}"#);
    let snap = s.wait("the context", |snap| {
        find_agent(snap, "pi-1").context.is_some()
    });
    assert_eq!(
        find_agent(&snap, "pi-1").context.as_ref().unwrap().percent,
        25
    );
    let frame = s.ok(&["status", "--frame", "120x40", "-C", &ws]);
    assert!(
        frame.contains("π pi-1") && frame.contains(" Opus 4.6 · high "),
        "{frame}"
    );

    // The inspector's transcript comes from pi's session file.
    let message = |value: Value| json!({ "type": "message", "timestamp": "2026-10-02T19:46:31.613Z", "message": value });
    let lines = [
        json!({ "type": "session", "version": 3, "id": session, "cwd": "/w" }),
        message(
            json!({ "role": "user", "content": [{ "type": "text", "text": "[crew] try things" }] }),
        ),
        message(json!({ "role": "assistant", "content": [
            { "type": "text", "text": "On it." },
            { "type": "toolCall", "name": "mcp__crew__task_complete", "args": "{\"result\":\"done\"}" },
        ] })),
    ];
    std::fs::write(
        &transcript,
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
    let printed = s.ok(&["agent", "transcript", "pi-1", "-C", &ws]);
    assert!(
        printed.contains(
            "[crew] try things\n\nOn it.\n\n▸ crew.task_complete  {\"result\":\"done\"}\n"
        ),
        "{printed}"
    );

    // A turn that failed at the account's limit parks the member.
    hook(
        "turn-end",
        r#"{"error":"error","error_details":"429 rate_limit_error: You've hit your usage limit · resets 3:19 PM"}"#,
    );
    let parked = s.wait("the member to be parked", |snap| {
        find_agent(snap, "pi-1").limit.is_some()
    });
    let limit = find_agent(&parked, "pi-1").limit.clone().unwrap();
    assert_eq!(limit.kind, "usage");
    assert!(limit.resets_at.is_some(), "{limit:?}");
}

/// A stand-in for `claude` that, asked for `/usage` the way crew asks, writes
/// a usage cache as Claude Code does (the model's own week used up), and
/// otherwise idles like a session.
const FAKE_USAGE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "-p" ] && [ "$2" = "/usage" ]; then
  printf '%s\n' "$@" > "$CLAUDE_CONFIG_DIR/usage-args"
  now=$(date +%s%3N)
  printf '{"cachedUsageUtilization":{"fetchedAtMs":%s,"utilization":{"limits":[{"kind":"weekly_all","percent":82,"resets_at":"2030-01-03T00:00:00+00:00"},{"kind":"weekly_scoped","percent":100,"resets_at":"2030-01-03T00:00:00+00:00","scope":{"model":{"display_name":"Fable"}}}]}}}' "$now" > "$CLAUDE_CONFIG_DIR/.claude.json"
  exit 0
fi
while read -r _; do :; done
"#;

#[test]
fn claude_code_is_asked_for_the_accounts_usage_so_a_models_own_week_stays_current() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("claude-home")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_USAGE_CLAUDE).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut team = s.team("usage", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.agents.push(AgentSpec {
        id: "writer".to_string(),
        runtime: Some(RuntimeKind::Claude),
        model: Some("fable".to_string()),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("PATH", &path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    // With a Claude member on the team, Claude Code is asked straight away,
    // and what it wrote is read at once: the model's week is spent.
    let snap = s.wait("the model's own week", |snap| {
        snap.session
            .quotas
            .iter()
            .any(|q| q.window == "week/Fable" && q.percent_used >= 100.0)
    });
    let week = snap
        .session
        .quotas
        .iter()
        .find(|q| q.window == "week")
        .unwrap();
    assert_eq!(week.percent_used, 82.0);
    // It is asked as a local command: no model call, no session left behind,
    // none of the user's MCP servers started.
    let args = std::fs::read_to_string(root.join("claude-home/usage-args")).unwrap();
    assert_eq!(args, "-p\n/usage\n--no-session-persistence\n--strict-mcp-config\n--mcp-config\n{\"mcpServers\":{}}\n");
}

// ---------------------------------------------------------------------------
// Moving an agent to another CLI
// ---------------------------------------------------------------------------

#[test]
fn only_the_operator_moves_the_orchestrator_to_another_cli_and_its_new_session_is_handed_the_work()
{
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    let ws = s.workspace.to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    for name in ["claude", "codex"] {
        std::fs::write(root.join("bin").join(name), FAKE_RUNTIME).unwrap();
        std::fs::set_permissions(
            root.join("bin").join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let crew = |args: &[&str]| {
        s.command()
            .env("PATH", &path)
            .args(args)
            .output()
            .expect("running crew")
    };
    let read = |id: &str, file: &str| {
        std::fs::read_to_string(s.workspace.join(".crew/agents").join(id).join(file))
            .unwrap_or_default()
    };

    let mut team = s.team("switch", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Claude),
        model: Some("opus".to_string()),
        ..OrchestratorConfig::default()
    };
    team.agents.push(AgentSpec {
        id: "design".to_string(),
        runtime: Some(RuntimeKind::Claude),
        model: Some("fable".to_string()),
        task: Some("design the thing".to_string()),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = crew(&["run", "team.yaml", "--no-attach"]);
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("agents to come up", |snap| {
        snap.agents.len() == 2
            && snap
                .agents
                .iter()
                .all(|a| a.status != AgentStatus::Spawning)
    });
    // Each session announces itself, with a transcript of what it said.
    for (id, said) in [
        (
            "orchestrator",
            "The plan is loaded; design is drafting the spec.",
        ),
        ("design", "Half of the spec is in spec.md."),
    ] {
        let transcript = root.join(format!("{id}.jsonl"));
        let reply = json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": said }] } });
        std::fs::write(&transcript, format!("{reply}\n")).unwrap();
        let session = find_agent(&s.snapshot(), id).session_id.clone().unwrap();
        let payload = json!({ "session_id": session, "transcript_path": transcript }).to_string();
        let out = s.crew(&[
            "hook",
            "session-start",
            "--identity",
            &s.identity_path(id).to_string_lossy(),
            &payload,
        ]);
        assert!(out.status.success(), "{}", stderr(&out));
    }

    // The orchestrator cannot put itself on another model, nor move a
    // member to another CLI: that is the operator's call.
    let own = s.ctl(
        "orchestrator",
        &[
            "tool",
            "agent_set_model",
            &json!({ "id": "orchestrator", "model": "sonnet" }).to_string(),
        ],
    );
    assert!(!own.status.success());
    assert!(
        stderr(&own).contains("only the operator can change the orchestrator's model or CLI"),
        "{}",
        stderr(&own)
    );
    let across = s.ctl(
        "orchestrator",
        &[
            "tool",
            "agent_set_model",
            &json!({ "id": "design", "model": "gpt-6-astra" }).to_string(),
        ],
    );
    assert!(!across.status.success());
    assert!(
        stderr(&across).contains("gpt-6-astra runs on Codex, not Claude Code; only the operator can move a member to another CLI"),
        "{}",
        stderr(&across)
    );
    assert_eq!(
        find_agent(&s.snapshot(), "orchestrator").runtime,
        RuntimeKind::Claude
    );

    // The dashboard's picker lists what can be started here, by CLI.
    let identity: Value = serde_json::from_str(&read("orchestrator", "identity.json")).unwrap();
    let socket = std::path::PathBuf::from(identity["socket"].as_str().unwrap());
    let (mut operator, _) = crew_core::client::Client::operator(&socket, "test").unwrap();
    let listed: crew_core::rpc::ModelsResult =
        operator.call(crew_core::rpc::Call::ModelsList).unwrap();
    let runtimes: Vec<RuntimeKind> = listed.models.iter().map(|m| m.runtime).collect();
    assert_eq!(runtimes.first(), Some(&RuntimeKind::Claude), "{listed:?}");
    let rank = |r: &RuntimeKind| {
        [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Pi]
            .iter()
            .position(|k| k == r)
    };
    assert!(
        runtimes.windows(2).all(|w| rank(&w[0]) <= rank(&w[1])),
        "grouped by CLI: {runtimes:?}"
    );
    let fable = listed.models.iter().find(|m| m.model == "fable").unwrap();
    assert_eq!(
        (fable.name.as_str(), fable.unavailable.as_deref()),
        ("Fable 5.1", None)
    );
    assert!(
        listed
            .models
            .iter()
            .any(|m| m.runtime == RuntimeKind::Codex && m.model == "gpt-6-astra"),
        "{listed:?}"
    );
    assert!(
        listed
            .models
            .iter()
            .any(|m| m.runtime == RuntimeKind::Pi && m.model.is_empty()),
        "{listed:?}"
    );

    // The operator moves the orchestrator to Codex: a fresh session there,
    // told what it takes over.
    let out = crew(&[
        "agent",
        "model",
        "orchestrator",
        "gpt-6-astra",
        "--note",
        "Keep the plan as it is.",
        "-C",
        &ws,
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "moving orchestrator to Codex on GPT 6 Astra: a fresh session there, handed the work"
    );
    s.wait_for("the orchestrator to start on codex", || {
        read("orchestrator", "fake-args").contains("--model\ngpt-6-astra\n")
    });
    let args = read("orchestrator", "fake-args");
    assert!(
        args.contains("mcp_servers.crew"),
        "started as Codex: {args}"
    );
    assert!(!args.contains("\nresume\n"), "{args}");
    for said in [
        "[crew] You take over as this team's orchestrator. The role ran on Claude Code on Opus 5.5 until now; operator moved it to Codex on GPT 6 Astra.",
        "From operator: Keep the plan as it is.",
        "Where the work stands:\n- ",
        "→ design [",
        "The team:\n- design (Claude Code, Fable 5.1): ",
        "The last thing your predecessor wrote:\n> The plan is loaded; design is drafting the spec.",
        "agent_inbox holds the messages that came in",
    ] {
        assert!(args.contains(said), "missing {said:?} in\n{args}");
    }
    let snap = s.wait("the orchestrator to be back", |snap| {
        find_agent(snap, "orchestrator").status != AgentStatus::Spawning
    });
    let orchestrator = find_agent(&snap, "orchestrator");
    assert_eq!(
        (
            orchestrator.runtime,
            orchestrator.model.as_deref(),
            orchestrator.model_name.as_deref(),
            orchestrator.session_id.as_deref()
        ),
        (
            RuntimeKind::Codex,
            Some("gpt-6-astra"),
            Some("GPT 6 Astra"),
            None
        )
    );
    assert!(s.events().iter().any(|e| e.event_type == "agent.runtime_set"
        && e.message.starts_with("orchestrator moves from Claude Code on Opus 5.5 to Codex on GPT 6 Astra (operator)")), "{:?}", s.events());
    // The team keeps it: the roster and a restarted daemon run it on Codex.
    let roster =
        crew_core::config::TeamConfig::load(&s.workspace.join(".crew/roster.yaml")).unwrap();
    assert_eq!(
        (
            roster.orchestrator.runtime,
            roster.orchestrator.model.as_deref()
        ),
        (Some(RuntimeKind::Codex), Some("gpt-6-astra"))
    );

    // A model of its new CLI does not move it again; the operator can
    // also move a member, and its new session finds its task and work.
    let out = crew(&["agent", "model", "design", "--runtime", "codex", "-C", &ws]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "moving design to Codex on its default model: a fresh session there, handed the work"
    );
    s.wait_for("design to start on codex", || {
        read("design", "fake-args").contains("mcp_servers.crew")
    });
    let args = read("design", "fake-args");
    assert!(!args.contains("--model\n"), "the CLI's own default: {args}");
    // Who it is and its role are in the system prompt, not in a prompt.
    let developer = args
        .lines()
        .find(|a| a.starts_with("developer_instructions="))
        .expect("developer instructions");
    assert!(
        developer.contains(r#"You are \"design\", one agent in a team"#),
        "{developer}"
    );
    assert_eq!(
        args.matches("one agent in a team").count(),
        1,
        "the first prompt does not repeat it: {args}"
    );
    for said in [
        "[crew] You take over the work of design. It ran on Claude Code on Fable 5.1 until now; operator moved it to Codex on its default model.",
        "Your working directory (",
        "read git status and git diff before you go on",
        "Your task is design-task (\"design the thing\"); it is handed to you again, and task_get(\"design-task\") reads its card.",
        "> Half of the spec is in spec.md.",
    ] {
        assert!(args.contains(said), "missing {said:?} in\n{args}");
    }
    // And back to Claude Code on a model named for it.
    let out = crew(&["agent", "model", "orchestrator", "sonnet", "-C", &ws]);
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait_for("the orchestrator back on claude", || {
        read("orchestrator", "fake-args").contains("--model\nsonnet")
    });
    let args = read("orchestrator", "fake-args");
    assert!(
        args.contains("--session-id\n") && !args.contains("--resume\n"),
        "{args}"
    );
    assert!(
        args.contains("The role ran on Codex on GPT 6 Astra until now"),
        "{args}"
    );
}

// ---------------------------------------------------------------------------
// A work style's model fallbacks
// ---------------------------------------------------------------------------

/// Claude Code that answers `-p /usage` with the limits in
/// `$CLAUDE_CONFIG_DIR/limits.json` and otherwise records how it was
/// started, like FAKE_RUNTIME.
const FAKE_CLAUDE_WITH_LIMITS: &str = r#"#!/bin/sh
if [ "$1" = "-p" ] && [ "$2" = "/usage" ]; then
  now=$(date +%s%3N)
  printf '{"cachedUsageUtilization":{"fetchedAtMs":%s,"utilization":{"limits":%s}}}' "$now" "$(cat "$CLAUDE_CONFIG_DIR/limits.json")" > "$CLAUDE_CONFIG_DIR/.claude.json"
  exit 0
fi
dir="${CREW_IDENTITY%/*}"
: > "$dir/fake-args"
for arg in "$@"; do printf '%s\n' "$arg" >> "$dir/fake-args"; done
while read -r _; do :; done
"#;

#[test]
fn a_work_style_puts_members_on_its_fallback_while_their_models_window_is_used_up_and_back_after() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    let ws = s.workspace.to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("claude-home")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_CLAUDE_WITH_LIMITS).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let crew = |args: &[&str]| {
        s.command()
            .env("PATH", &path)
            .args(args)
            .output()
            .expect("running crew")
    };
    let read = |id: &str| {
        std::fs::read_to_string(s.workspace.join(".crew/agents").join(id).join("fake-args"))
            .unwrap_or_default()
    };
    let identity = |id: &str| s.identity_path(id).to_string_lossy().into_owned();
    let hook = |id: &str, event: &str, payload: &str| {
        let out = s.crew(&["hook", event, "--identity", &identity(id), payload]);
        assert!(out.status.success(), "{event}: {}", stderr(&out));
    };
    let told = |text: &str| {
        s.messages()
            .iter()
            .any(|m| m.to == "orchestrator" && m.body.contains(text))
    };

    // Fable's own week is used up, resetting in a little while.
    let resets = chrono::Utc::now() + chrono::Duration::seconds(20);
    let limits = json!([
        { "kind": "weekly_all", "percent": 60, "resets_at": "2030-01-03T00:00:00+00:00" },
        { "kind": "weekly_scoped", "percent": 100, "resets_at": resets.to_rfc3339(), "scope": { "model": { "display_name": "Fable" } } },
    ]);
    std::fs::write(root.join("claude-home/limits.json"), limits.to_string()).unwrap();

    let mut team = s.team("fallbacks", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    // Sonnet rather than Opus, so it is crew's choice and not Claude Code's.
    team.defaults
        .model_fallbacks
        .insert("fable".to_string(), vec!["sonnet".to_string()]);
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[]),
        ..OrchestratorConfig::default()
    };
    team.agents.push(AgentSpec {
        id: "design".to_string(),
        runtime: Some(RuntimeKind::Claude),
        model: Some("fable".to_string()),
        task: Some("design the thing".to_string()),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = crew(&["run", "team.yaml", "--no-attach"]);
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("Fable's week to be known", |snap| {
        snap.session
            .quotas
            .iter()
            .any(|q| q.window == "week/Fable" && q.percent_used >= 100.0)
    });
    s.wait("design to come up", |snap| {
        find_agent(snap, "design").status != AgentStatus::Spawning
    });
    let session = find_agent(&s.snapshot(), "design")
        .session_id
        .clone()
        .unwrap();
    hook(
        "design",
        "session-start",
        &json!({ "session_id": session, "transcript_path": "/nonexistent" }).to_string(),
    );

    // Claude Code moves the session to Opus by itself; the work style says
    // Sonnet, and crew puts it there, resumed, without asking anyone.
    hook(
        "design",
        "step",
        r#"{"model":"claude-opus-5-5[1m]","effort":"xhigh"}"#,
    );
    s.wait_for("design to restart on sonnet", || {
        read("design").contains("--model\nsonnet[1m]\n")
    });
    let args = read("design");
    assert!(args.contains("--resume\n"), "{args}");
    assert!(
        args.contains(
            "[crew] Your session was restarted on Sonnet 5.5: Fable week is used up (it resets "
        ),
        "{args}"
    );
    assert!(
        args.contains("the work style puts it on Sonnet 5.5 until then."),
        "{args}"
    );
    let snap = s.wait("design to be back up", |snap| {
        find_agent(snap, "design").status != AgentStatus::Spawning
    });
    let design = find_agent(&snap, "design");
    assert_eq!(
        (design.model.as_deref(), design.model_name.as_deref()),
        (Some("sonnet"), Some("Sonnet 5.5"))
    );
    let marker = design.fallback.clone().expect("marked as off its model");
    assert!(marker.policy && marker.from == "Fable 5.1", "{marker:?}");
    s.wait_for("the orchestrator to hear, with nothing to decide", || {
        told("Agent design moves from Fable 5.1 to Sonnet 5.5 for now, as the work style says: Fable week is used up")
    });
    assert!(told(
        "It goes back to Fable 5.1 by itself once that resets. Nothing needs deciding."
    ));
    assert!(!told("Decide:"), "the orchestrator is not asked to choose");
    assert!(
        s.events()
            .iter()
            .any(|e| e.event_type == "agent.model_policy"),
        "{:?}",
        s.events()
    );
    let status = s.ok(&["status", "-C", &ws]);
    assert!(
        status.contains(
            "fallback   design runs on Sonnet 5.5 instead of Fable 5.1 — Fable week is used up"
        ),
        "{status}"
    );
    assert!(status.contains("it goes back by itself"), "{status}");
    // The member is still Fable's: the roster says so.
    let roster =
        crew_core::config::TeamConfig::load(&s.workspace.join(".crew/roster.yaml")).unwrap();
    assert_eq!(
        roster
            .agents
            .iter()
            .find(|a| a.id == "design")
            .unwrap()
            .model
            .as_deref(),
        Some("fable")
    );

    // A member the orchestrator adds on Fable meanwhile starts on Sonnet
    // straight away, and the orchestrator hears why.
    let out = s.ctl(
        "orchestrator",
        &[
            "tool",
            "agent_add",
            &json!({ "id": "writer", "role": "writer", "model": "fable" }).to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("writer joined the team as writer (claude, sonnet). It starts there, not on Fable 5.1: Fable week is used up"), "{said}");
    assert!(
        said.contains("it moves to Fable 5.1 once that resets."),
        "{said}"
    );
    s.wait_for("writer to start", || read("writer").contains("--model\n"));
    assert!(
        read("writer").contains("--model\nsonnet[1m]\n"),
        "{}",
        read("writer")
    );
    let snap = s.wait("writer to be up", |snap| {
        find_agent(snap, "writer").status != AgentStatus::Spawning
    });
    assert!(find_agent(&snap, "writer")
        .fallback
        .as_ref()
        .is_some_and(|f| f.policy));

    // Once the window has reset — the reset time passed and a reading came
    // since — each goes back to Fable between turns, its session resumed.
    let wait = (resets - chrono::Utc::now()).num_milliseconds().max(0) as u64 + 500;
    std::thread::sleep(std::time::Duration::from_millis(wait));
    for id in ["design", "writer"] {
        let session = find_agent(&s.snapshot(), id).session_id.clone().unwrap();
        hook(
            id,
            "session-start",
            &json!({ "session_id": session, "transcript_path": "/nonexistent" }).to_string(),
        );
        hook(
            id,
            "usage",
            r#"{"rate_limits":[{"kind":"five_hour","percentUsed":10}]}"#,
        );
        hook(id, "stop", "{}");
    }
    for id in ["design", "writer"] {
        s.wait_for("the member back on fable", || {
            read(id).contains("--model\nfable[1m]\n")
        });
        assert!(read(id).contains("[crew] Fable week has reset: your session was restarted on Fable 5.1, your own model again."), "{}", read(id));
    }
    let snap = s.wait("both back up", |snap| {
        ["design", "writer"]
            .iter()
            .all(|id| find_agent(snap, id).status != AgentStatus::Spawning)
    });
    for id in ["design", "writer"] {
        let agent = find_agent(&snap, id);
        assert_eq!(
            (agent.model.as_deref(), agent.fallback.as_ref()),
            (Some("fable"), None),
            "{id}"
        );
    }
    s.wait_for("the orchestrator to hear they are back", || {
        told("Agent design is back on Fable 5.1: Fable week has reset.")
    });
}

#[test]
fn the_orchestrator_staffs_the_team_around_the_models_whose_window_is_used_up() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("claude-home")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_CLAUDE_WITH_LIMITS).unwrap();
    std::fs::write(root.join("bin/codex"), FAKE_RUNTIME).unwrap();
    for name in ["claude", "codex"] {
        std::fs::set_permissions(
            root.join("bin").join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let read = |id: &str| {
        std::fs::read_to_string(s.workspace.join(".crew/agents").join(id).join("fake-args"))
            .unwrap_or_default()
    };
    let limits = json!([
        { "kind": "session", "percent": 31, "resets_at": "2030-01-01T02:00:00+00:00" },
        { "kind": "weekly_all", "percent": 82, "resets_at": "2030-01-03T00:00:00+00:00" },
        { "kind": "weekly_scoped", "percent": 100, "resets_at": "2030-01-03T00:00:00+00:00", "scope": { "model": { "display_name": "Fable" } } },
    ]);
    std::fs::write(root.join("claude-home/limits.json"), limits.to_string()).unwrap();

    // No members yet and no fallbacks in the style: the orchestrator decides.
    let mut team = s.team("staffing", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Claude),
        model: Some("opus".to_string()),
        ..OrchestratorConfig::default()
    };
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("PATH", &path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));

    // Its first prompt says what each model has left, before it adds anyone:
    // Fable is out, and Astra, as strong, is the first to take instead.
    s.wait_for("the orchestrator's kickoff", || {
        read("orchestrator").contains("no members yet")
    });
    let kickoff = read("orchestrator");
    // After the options' end, so no option can take it for a value of its own.
    assert!(
        kickoff.contains("\n--allowedTools\nmcp__crew\n")
            && kickoff.contains("\n--\n[crew] You are online"),
        "{kickoff}"
    );
    for said in [
        "Usage now: Claude Code session 31% · week 82% · Fable week 100% (resets ",
        "Out until their window resets — staff none of these now; for a role that needs one, take the first model after it:",
        "- fable (Fable week used up, resets ",
        "): gpt-6-astra, then opus, then ",
        "Choose models with this in mind (usage_status says it again at any time).",
    ] {
        assert!(kickoff.contains(said), "missing {said:?} in\n{kickoff}");
    }
    let briefing =
        std::fs::read_to_string(s.workspace.join(".crew/agents/orchestrator/briefing.md")).unwrap();
    assert!(
        briefing.contains("the same tier of the other family first (for fable, gpt-6-astra)"),
        "{briefing}"
    );

    // It can ask again at any time.
    let out = s.ctl("orchestrator", &["tool", "usage_status", "{}"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let status = String::from_utf8_lossy(&out.stdout);
    assert!(
        status.contains("- fable (Fable week used up, resets ")
            && status.contains("gpt-6-astra, then opus"),
        "{status}"
    );

    // A member on Fable is refused, with what to take instead; on Astra it starts.
    let out = s.ctl(
        "orchestrator",
        &[
            "tool",
            "agent_add",
            &json!({ "id": "planner", "role": "planner", "model": "fable" }).to_string(),
        ],
    );
    assert!(!out.status.success());
    let refused = stderr(&out);
    assert!(
        refused.contains("fable cannot be started now: Fable week is used up (it resets "),
        "{refused}"
    );
    assert!(
        refused.contains("take gpt-6-astra or opus or "),
        "{refused}"
    );
    assert!(
        s.snapshot().agents.iter().all(|a| a.id != "planner"),
        "nothing was added"
    );
    let out = s.ctl(
        "orchestrator",
        &[
            "tool",
            "agent_add",
            &json!({ "id": "planner", "role": "planner", "model": "gpt-6-astra" }).to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout)
        .contains("planner joined the team as planner (codex, gpt-6-astra)."));
    s.wait_for("the planner to start on codex", || {
        read("planner").contains("--model\ngpt-6-astra\n")
    });
}

/// Codex at work on a slow request: after a while it shows its "extra
/// thought" menu, and records the first key it is given, as a byte.
const FAKE_CODEX_THINKING: &str = r#"#!/bin/sh
dir="${CREW_IDENTITY%/*}"
printf '› Ask Codex to do anything\n'
sleep 6
printf '\n  Giving this request a little extra thought\n\n› 1. Dismiss and keep waiting\n  2. Learn more\n\n  No action is required. Codex will keep waiting, and this menu will close when the response is ready.\n'
stty -icanon -icrnl -echo min 1 time 0 2>/dev/null
key=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')
printf 'pressed:[%s]\n' "$key" > "$dir/keys"
printf '\033[2J\033[H• Working (40s)\n'
while read -r _; do :; done
"#;

#[test]
fn codexs_extra_thought_menu_is_dismissed_without_interrupting_the_turn() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/codex"), FAKE_CODEX_THINKING).unwrap();
    std::fs::set_permissions(
        root.join("bin/codex"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut team = s.team("thinking", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.agents.push(AgentSpec {
        id: "solver".to_string(),
        runtime: Some(RuntimeKind::Codex),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("PATH", &path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));

    // Mid-turn the menu comes up; crew presses Enter on "Dismiss and keep
    // waiting" (not Esc, which would interrupt the turn), and says so.
    let keys = s.workspace.join(".crew/agents/solver/keys");
    s.wait_for("the menu to be dismissed", || keys.exists());
    assert_eq!(
        std::fs::read_to_string(&keys).unwrap(),
        "pressed:[0d]\n",
        "Enter, and nothing before it"
    );
    s.wait_for("the log to say so", || {
        s.events().iter().any(|e| {
            e.event_type == "agent.notice_dismissed"
                && e.message == "solver: dismissed — Codex says it is giving the request a little extra thought"
        })
    });
    let solver = find_agent(&s.snapshot(), "solver").clone();
    assert!(
        solver.block_kind.is_none() && solver.status != AgentStatus::Blocked,
        "{solver:?}"
    );
}

/// Claude Code that records how it was started and every line typed into it.
const FAKE_CLAUDE_TYPED: &str = r#"#!/bin/sh
dir="${CREW_IDENTITY%/*}"
: > "$dir/fake-args"
for arg in "$@"; do printf '%s\n' "$arg" >> "$dir/fake-args"; done
while IFS= read -r line; do printf '%s\n' "$line" >> "$dir/typed"; done
"#;

#[test]
fn a_teammates_status_report_waits_for_the_orchestrators_next_turn() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_CLAUDE_TYPED).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut team = s.team(
        "fyi",
        vec![agent(
            "worker",
            "works",
            &[
                "--on-task=sleep 1500",
                "--on-task=send orchestrator status halfway there",
                "--on-task=sleep 3000",
                "--on-task=done finished",
            ],
        )],
        vec![task("job", "worker", &[])],
    );
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Claude),
        ..OrchestratorConfig::default()
    };
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("PATH", &path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let typed = || {
        std::fs::read_to_string(s.workspace.join(".crew/agents/orchestrator/typed"))
            .unwrap_or_default()
    };
    // The orchestrator's first turn ends: it takes input from here on.
    s.wait("the orchestrator to come up", |snap| {
        find_agent(snap, "orchestrator").status != AgentStatus::Spawning
    });
    let identity = s
        .identity_path("orchestrator")
        .to_string_lossy()
        .into_owned();
    let out = s.crew(&["hook", "stop", "--identity", &identity, "{}"]);
    assert!(out.status.success(), "{}", stderr(&out));

    // A teammate's status report is news, not a call to act: alone, it does
    // not start a turn.
    s.wait_for("the worker's report", || {
        s.messages()
            .iter()
            .any(|m| m.from == "worker" && m.to == "orchestrator" && m.kind == MessageType::Status)
    });
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(
        !typed().contains("halfway there"),
        "a status report alone woke the orchestrator:\n{}",
        typed()
    );

    // crew's notice that the task finished does, and the report goes with it.
    s.wait_all_done();
    s.wait_for("the notice and the report together", || {
        typed().contains("halfway there")
    });
    // One prompt, one turn: the report, then crew's notices.
    let typed = typed();
    let line = typed.lines().find(|l| l.contains("halfway there")).unwrap();
    assert!(
        line.contains("new messages:  (1) from worker · type status"),
        "{typed}"
    );
    assert!(
        line.contains("(2) from scheduler · type status")
            && line.contains("was completed by worker: finished"),
        "{typed}"
    );
}

#[test]
fn members_added_after_the_team_is_founded_are_temporary_and_leave_once_their_work_is_done() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_CLAUDE_TYPED).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut team = s.team("founding", vec![], vec![]);
    team.defaults.command = scripted(&[
        "--on-task=write {me}.txt by {me}",
        "--on-task=commit work of {me}",
        "--on-task=done finished by {me}",
    ]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Claude),
        ..OrchestratorConfig::default()
    };
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("PATH", &path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("the orchestrator to come up", |snap| {
        find_agent(snap, "orchestrator").status != AgentStatus::Spawning
    });
    let typed = || {
        std::fs::read_to_string(s.workspace.join(".crew/agents/orchestrator/typed"))
            .unwrap_or_default()
    };
    let identity = s
        .identity_path("orchestrator")
        .to_string_lossy()
        .into_owned();
    let turn_ends = || {
        let out = s.crew(&["hook", "stop", "--identity", &identity, "{}"]);
        assert!(out.status.success(), "{}", stderr(&out));
    };
    let tool = |name: &str, args: Value| {
        let out = s.ctl("orchestrator", &["tool", name, &args.to_string()]);
        assert!(out.status.success(), "{name}: {}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // The kick-off turn: the orchestrator staffs the team and plans. Whoever
    // is on the team when that turn ends is the founding team.
    let added = tool("agent_add", json!({ "id": "founder", "role": "builds" }));
    assert!(!added.contains("temporary"), "{added}");
    tool(
        "task_create",
        json!({ "id": "build", "description": "build it", "agent": "founder" }),
    );
    assert!(!s.has_event("team.founded"));
    turn_ends();
    s.wait_for("the founding team to be settled", || {
        s.has_event("team.founded")
    });
    let founded = s
        .events()
        .into_iter()
        .find(|e| e.event_type == "team.founded")
        .unwrap();
    assert!(
        founded
            .message
            .contains("founding team is settled: founder;"),
        "{}",
        founded.message
    );

    // From here on, a member the orchestrator adds is temporary: it is not
    // saved with the roster.
    let added = tool(
        "agent_add",
        json!({ "id": "helper", "role": "lends a hand", "task": "write the docs" }),
    );
    assert!(added.contains("It is temporary"), "{added}");
    let snap = s.snapshot();
    assert!(find_agent(&snap, "helper").temporary && !find_agent(&snap, "founder").temporary);
    let listed = tool("agents_list", json!({}));
    assert!(
        listed
            .lines()
            .any(|l| l.starts_with("helper ") && l.contains("(temporary)")),
        "{listed}"
    );
    let roster_path = s.workspace.join(".crew/roster.yaml");
    let saved = || -> Vec<String> {
        let roster = crew_core::config::TeamConfig::load(&roster_path).unwrap();
        roster.agents.into_iter().map(|a| a.id).collect()
    };
    assert_eq!(saved(), ["founder"]);

    // Its work done, the orchestrator hears that it is about to leave...
    s.wait_all_done();
    s.wait_for("the notice that helper leaves", || {
        typed().contains("helper is temporary and has nothing else to do")
    });
    s.wait_for("the notice that everything is done", || {
        typed().contains("tasks are done")
    });
    // ...and it stays while that turn runs, so it can be given more work.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        s.snapshot().agents.iter().any(|a| a.id == "helper"),
        "helper left before the orchestrator's turn ended"
    );

    // Once the turn is over, it leaves; the founding member stays.
    turn_ends();
    let after = s.wait("helper to leave", |snap| {
        snap.agents.iter().all(|a| a.id != "helper")
    });
    assert!(after.agents.iter().any(|a| a.id == "founder"));
    let left = s
        .events()
        .into_iter()
        .find(|e| e.event_type == "agent.removed")
        .unwrap();
    assert!(
        left.message
            .contains("helper left the team: a temporary member, its work done"),
        "{}",
        left.message
    );
    assert_eq!(saved(), ["founder"]);

    // Its work is still part of the result.
    let out = s.ok(&["integrate", "-C", &s.workspace.to_string_lossy()]);
    let report: Value = serde_json::from_str(&out).unwrap();
    let merged: Vec<&str> = report["merged"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        merged.contains(&"founder") && merged.contains(&"helper"),
        "{report}"
    );
}

#[test]
fn a_failed_task_of_a_member_who_left_goes_back_to_the_pool_when_retried() {
    let s = Session::new();
    let team = s.team(
        "leftover",
        vec![
            agent("gone", "", &["--on-task=fail cannot do it"]),
            agent("stays", "", &["--on-task=done done by {me}"]),
        ],
        vec![task("job", "gone", &[])],
    );
    s.run(&team);
    s.wait("the task to fail", |snap| {
        find_task(snap, "job").status == TaskStatus::Failed
    });
    s.agent_cmd(&["remove", "gone"]);
    s.wait("gone to leave", |snap| {
        snap.agents.iter().all(|a| a.id != "gone")
    });
    s.ok(&["task", "retry", "job", "-C", &s.workspace.to_string_lossy()]);
    let done = s.wait_all_done();
    assert_eq!(
        find_task(&done, "job").assigned_agent.as_deref(),
        Some("stays")
    );
    assert_eq!(
        find_task(&done, "job").result.as_deref(),
        Some("done by stays")
    );
}

#[test]
fn a_turn_codexs_model_refuses_is_noticed_and_whoever_waits_on_it_is_told() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/codex"), FAKE_LIMITED_CODEX).unwrap();
    std::fs::set_permissions(
        root.join("bin/codex"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    // The session file Codex keeps, with one turn that went fine.
    let day = chrono::Local::now().format("%Y/%m/%d").to_string();
    let sessions = root.join("codex-home/sessions").join(day);
    std::fs::create_dir_all(&sessions).unwrap();
    let rollout = sessions.join("rollout-2026-01-01T00-00-00-thread-cyber.jsonl");
    let event = |payload: Value| json!({ "type": "event_msg", "payload": payload }).to_string();
    let secs = || chrono::Utc::now().timestamp();
    let first = [
        json!({ "type": "turn_context", "payload": { "model": "gpt-6-astra", "effort": "max" } })
            .to_string(),
        event(json!({ "type": "task_started", "turn_id": "t-1" })),
        event(
            json!({ "type": "task_complete", "turn_id": "t-1", "last_agent_message": "ready", "completed_at": secs() }),
        ),
    ];
    std::fs::write(&rollout, first.join("\n") + "\n").unwrap();

    let mut team = s.team(
        "refusal",
        vec![agent("lead", "leads the design", &[])],
        vec![],
    );
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[]),
        ..OrchestratorConfig::default()
    };
    team.agents.push(AgentSpec {
        id: "rev".to_string(),
        role: "reviews the design".to_string(),
        runtime: Some(RuntimeKind::Codex),
        task: Some("review the design".to_string()),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = s
        .command()
        .env("PATH", path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("agents to come up", |snap| {
        snap.agents.len() == 3
            && snap
                .agents
                .iter()
                .all(|a| a.status != AgentStatus::Spawning)
    });
    let identity = s.identity_path("rev").to_string_lossy().into_owned();
    let out = s.crew(&[
        "hook",
        "codex-notify",
        "--identity",
        &identity,
        r#"{"type":"agent-turn-complete","thread-id":"thread-cyber"}"#,
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("rev's session file to be read", |snap| {
        find_agent(snap, "rev").running_model.as_deref() == Some("gpt-6-astra")
    });
    assert_ne!(
        find_agent(&s.snapshot(), "rev").status,
        AgentStatus::Blocked,
        "a turn that went fine is nothing to report"
    );

    // lead asks rev for a review; Codex refuses the turn. Its notify hook is
    // not called for that, so only the session file says the turn is over.
    let out = s.ctl(
        "lead",
        &[
            "tool",
            "agent_send",
            &json!({ "to": "rev", "type": "review_request", "body": "Please review design.md" })
                .to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("rev to take the request", |snap| {
        find_agent(snap, "rev").status == AgentStatus::Working
    });
    let request = s
        .messages()
        .into_iter()
        .find(|m| m.from == "lead" && m.to == "rev")
        .unwrap();
    let given = format!(
        "[crew] Message from lead (type review_request, id {}):\n\nPlease review design.md",
        request.id
    );
    let refused = [
        event(json!({ "type": "task_started", "turn_id": "t-2" })),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": given }] } }).to_string(),
        event(json!({ "type": "task_complete", "turn_id": "t-2", "last_agent_message": null, "completed_at": secs(), "error": {
            "message": "This content was flagged for possible cybersecurity risk.", "codex_error_info": "cyber_policy" } })),
    ];
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&rollout)
        .unwrap();
    file.write_all((refused.join("\n") + "\n").as_bytes())
        .unwrap();

    // rev is set aside, so nothing prompts it into the same refusal again...
    let snap = s.wait("rev to be blocked", |snap| {
        find_agent(snap, "rev").block_kind.as_deref() == Some("refused")
    });
    let rev = find_agent(&snap, "rev");
    assert_eq!(rev.status, AgentStatus::Blocked);
    assert!(
        rev.activity
            .contains("GPT 6 Astra refused the turn (cyber_policy)"),
        "{}",
        rev.activity
    );
    let task = snap
        .tasks
        .iter()
        .find(|t| t.assigned_agent.as_deref() == Some("rev"))
        .unwrap();
    assert_eq!(task.status, TaskStatus::Blocked);
    assert!(s.has_event("agent.turn_failed") && s.has_event("agent.refused"));
    // ...the orchestrator hears why and what to do instead...
    let told = |to: &str| {
        s.messages()
            .into_iter()
            .filter(|m| m.from == "scheduler" && m.to == to)
            .map(|m| m.body)
            .collect::<Vec<_>>()
            .join("\n")
    };
    s.wait_for("the orchestrator to be told", || {
        told("orchestrator").contains("rev's model")
    });
    let orchestrator = told("orchestrator");
    assert!(orchestrator.contains("rev's model (GPT 6 Astra) refused its turn: it was flagged for possible cybersecurity risk"), "{orchestrator}");
    assert!(
        orchestrator.contains(&format!(
            "It was given {} from lead (they have been told)",
            request.id
        )),
        "{orchestrator}"
    );
    assert!(
        orchestrator.contains("do not just retry it") && orchestrator.contains("operator_ask"),
        "{orchestrator}"
    );
    // ...and lead does not wait for an answer that will not come.
    s.wait_for("lead to be told", || {
        told("lead").contains("rev cannot act on your message")
    });
    let lead = s
        .messages()
        .into_iter()
        .find(|m| m.to == "lead" && m.body.contains("rev cannot act"))
        .unwrap();
    assert_eq!(lead.reply_to.as_deref(), Some(request.id.as_str()));
}

/// pi.dev's `pi`, as far as crew asks it things: it lists two models, and
/// a session records how it was started, then idles at its prompt.
const FAKE_PI: &str = r#"#!/bin/sh
case "$1" in
  --help) echo "pi - a coding agent"; exit 0 ;;
  --list-models) printf 'provider    model                        context\nzai         glm-5.3                      200K\nopenrouter  deepseek/deepseek-v4-flash   1M\n'; exit 0 ;;
esac
dir="${CREW_IDENTITY%/*}"
: > "$dir/fake-args"
for arg in "$@"; do printf '%s\n' "$arg" >> "$dir/fake-args"; done
while read -r _; do :; done
"#;

#[test]
fn a_turn_codex_refuses_goes_to_the_work_styles_pi_model_and_the_member_comes_back_after() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    for (name, script) in [("codex", FAKE_LIMITED_CODEX), ("pi", FAKE_PI)] {
        std::fs::write(root.join("bin").join(name), script).unwrap();
        std::fs::set_permissions(
            root.join("bin").join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let day = chrono::Local::now().format("%Y/%m/%d").to_string();
    let sessions = root.join("codex-home/sessions").join(day);
    std::fs::create_dir_all(&sessions).unwrap();
    let rollout = sessions.join("rollout-2026-01-01T00-00-00-thread-handover.jsonl");
    let event = |payload: Value| json!({ "type": "event_msg", "payload": payload }).to_string();
    let secs = || chrono::Utc::now().timestamp();
    let first = [
        json!({ "type": "turn_context", "payload": { "model": "gpt-6-astra", "effort": "max" } })
            .to_string(),
        event(json!({ "type": "task_started", "turn_id": "t-1" })),
        event(
            json!({ "type": "task_complete", "turn_id": "t-1", "last_agent_message": "ready", "completed_at": secs() }),
        ),
    ];
    std::fs::write(&rollout, first.join("\n") + "\n").unwrap();

    let mut team = s.team(
        "handover",
        vec![agent("lead", "leads the design", &[])],
        vec![],
    );
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    // The first is not among the models pi lists; the second is, under zai.
    team.defaults.refusal_fallbacks = vec!["deepseek-v4-pro".to_string(), "glm-5.3".to_string()];
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[]),
        ..OrchestratorConfig::default()
    };
    team.agents.push(AgentSpec {
        id: "rev".to_string(),
        role: "reviews the design".to_string(),
        runtime: Some(RuntimeKind::Codex),
        model: Some("gpt-6-astra".to_string()),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = s
        .command()
        .env("PATH", path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("agents to come up", |snap| {
        snap.agents.len() == 3
            && snap
                .agents
                .iter()
                .all(|a| a.status != AgentStatus::Spawning)
    });
    let identity = s.identity_path("rev").to_string_lossy().into_owned();
    let hook = |event: &str, payload: &str| {
        let out = s.crew(&["hook", event, "--identity", &identity, payload]);
        assert!(out.status.success(), "{event}: {}", stderr(&out));
    };
    hook(
        "codex-notify",
        r#"{"type":"agent-turn-complete","thread-id":"thread-handover"}"#,
    );
    s.wait("rev's session file to be read", |snap| {
        find_agent(snap, "rev").running_model.as_deref() == Some("gpt-6-astra")
    });

    // lead asks for a review, and Codex refuses the turn.
    let out = s.ctl(
        "lead",
        &[
            "tool",
            "agent_send",
            &json!({ "to": "rev", "type": "review_request", "body": "Please review design.md" })
                .to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("rev to take the request", |snap| {
        find_agent(snap, "rev").status == AgentStatus::Working
    });
    let request = s
        .messages()
        .into_iter()
        .find(|m| m.from == "lead" && m.to == "rev")
        .unwrap();
    let given = format!(
        "[crew] Message from lead (type review_request, id {}):\n\nPlease review design.md",
        request.id
    );
    let refused = [
        event(json!({ "type": "task_started", "turn_id": "t-2" })),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": given }] } }).to_string(),
        event(json!({ "type": "task_complete", "turn_id": "t-2", "last_agent_message": null, "completed_at": secs(), "error": {
            "message": "This content was flagged for possible cybersecurity risk.", "codex_error_info": "cyber_policy" } })),
    ];
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&rollout)
        .unwrap();
    file.write_all((refused.join("\n") + "\n").as_bytes())
        .unwrap();

    // The work style's pi model takes that part over: a fresh pi session,
    // handed what was refused.
    let snap = s.wait("rev to move to pi", |snap| {
        let rev = find_agent(snap, "rev");
        rev.runtime == RuntimeKind::Pi && rev.fallback.is_some()
    });
    let rev = find_agent(&snap, "rev");
    assert_eq!(
        (rev.model.as_deref(), rev.model_name.as_deref()),
        (Some("zai/glm-5.3"), Some("GLM 5.3"))
    );
    assert_ne!(rev.status, AgentStatus::Blocked);
    let marker = rev.fallback.clone().expect("on the fallback for now");
    assert!(marker.policy && marker.from == "GPT 6 Astra", "{marker:?}");
    assert!(
        marker.note.contains(
            "refused a turn (cyber_policy); it goes back to GPT 6 Astra once that work is done"
        ),
        "{marker:?}"
    );
    let args = || {
        std::fs::read_to_string(s.workspace.join(".crew/agents/rev/fake-args")).unwrap_or_default()
    };
    s.wait_for("the pi session to start", || {
        args().contains("--model\nzai/glm-5.3\n")
    });
    let handed = args();
    assert!(
        handed.contains("[crew] You take over the work of rev."),
        "{handed}"
    );
    assert!(
        handed.contains(
            "GPT 6 Astra refused the last turn (it was flagged for possible cybersecurity risk)"
        ),
        "{handed}"
    );
    assert!(handed.contains(&given), "{handed}");
    // The orchestrator hears of it, with nothing to decide; lead keeps
    // waiting for its review, which is coming.
    let told = |to: &str| {
        s.messages()
            .into_iter()
            .filter(|m| m.from == "scheduler" && m.to == to)
            .map(|m| m.body)
            .collect::<Vec<_>>()
            .join("\n")
    };
    s.wait_for("the orchestrator to be told", || {
        told("orchestrator").contains("rev moves to GLM 5.3")
    });
    assert!(
        told("orchestrator").contains("Nothing needs deciding"),
        "{}",
        told("orchestrator")
    );
    assert!(!told("lead").contains("cannot act"), "{}", told("lead"));
    // The roster keeps rev's own model.
    let roster =
        crew_core::config::TeamConfig::load(&s.workspace.join(".crew/roster.yaml")).unwrap();
    let saved = roster.agents.iter().find(|a| a.id == "rev").unwrap();
    assert_eq!(
        (saved.runtime, saved.model.as_deref()),
        (Some(RuntimeKind::Codex), Some("gpt-6-astra"))
    );

    // That part done (its turn over, nothing else to do), rev goes back.
    hook("turn-start", "{}");
    hook("turn-end", "{}");
    let snap = s.wait("rev to go back to Codex", |snap| {
        find_agent(snap, "rev").runtime == RuntimeKind::Codex
    });
    let rev = find_agent(&snap, "rev");
    assert_eq!(rev.model.as_deref(), Some("gpt-6-astra"));
    assert!(rev.fallback.is_none());
    assert!(s.has_event("agent.model_restored"));
    s.wait_for("the orchestrator to hear it", || {
        told("orchestrator").contains("rev is back on GPT 6 Astra: the work it refused is done")
    });
}

#[test]
fn a_turn_that_ends_without_a_tool_call_is_asked_again_and_one_that_declines_is_dealt_with() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_CLAUDE_TYPED).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let mut team = s.team("unanswered", vec![agent("lead", "leads", &[])], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.orchestrator = OrchestratorConfig {
        agent: true,
        runtime: Some(RuntimeKind::Shell),
        command: scripted(&[]),
        ..OrchestratorConfig::default()
    };
    for id in ["words", "tools", "decliner"] {
        team.agents.push(AgentSpec {
            id: id.to_string(),
            runtime: Some(RuntimeKind::Claude),
            ..AgentSpec::default()
        });
    }
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = s
        .command()
        .env("PATH", path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let snap = s.wait("agents to come up", |snap| {
        snap.agents.len() == 5
            && snap
                .agents
                .iter()
                .all(|a| a.status != AgentStatus::Spawning)
    });
    let hook = |id: &str, event: &str, payload: &str| {
        let identity = s.identity_path(id).to_string_lossy().into_owned();
        let out = s.crew(&["hook", event, "--identity", &identity, payload]);
        assert!(out.status.success(), "{event}: {}", stderr(&out));
    };
    let transcript = |id: &str| root.join(format!("{id}.jsonl"));
    let record = |id: &str, lines: &[Value]| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(transcript(id))
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    };
    let user =
        |text: &str| json!({ "type": "user", "message": { "role": "user", "content": text } });
    let said = |text: &str| json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] } });
    let called = |tool: &str, input: Value| json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "tool_use", "name": tool, "input": input }] } });
    let told = |to: &str| {
        s.messages()
            .into_iter()
            .filter(|m| m.from == "scheduler" && m.to == to)
            .map(|m| m.body)
            .collect::<Vec<_>>()
            .join("\n")
    };
    // lead asks each member a question; each session says where its transcript is.
    let mut asked = std::collections::HashMap::new();
    for id in ["words", "tools", "decliner"] {
        let session = find_agent(&snap, id).session_id.clone().unwrap();
        hook(id, "session-start", &json!({ "session_id": session, "transcript_path": transcript(id), "model": "claude-sonnet-5-5" }).to_string());
        let out = s.ctl(
            "lead",
            &[
                "tool",
                "agent_send",
                &json!({ "to": id, "type": "question", "body": "Which file holds the parser?" })
                    .to_string(),
            ],
        );
        assert!(out.status.success(), "{}", stderr(&out));
        let typed = s.workspace.join(".crew/agents").join(id).join("typed");
        s.wait_for("the question to be typed", || {
            std::fs::read_to_string(&typed)
                .unwrap_or_default()
                .contains("Which file holds the parser?")
        });
        let question = s
            .messages()
            .into_iter()
            .find(|m| m.from == "lead" && m.to == id)
            .unwrap();
        asked.insert(
            id,
            format!(
                "[crew] Message from lead (type question, id {}):\n\nWhich file holds the parser?",
                question.id
            ),
        );
    }

    // Answered with a tool: nothing to say.
    record(
        "tools",
        &[
            user(&asked["tools"]),
            called(
                "mcp__crew__agent_send",
                json!({ "to": "lead", "body": "src/parser.rs" }),
            ),
            said("Sent."),
        ],
    );
    hook("tools", "stop", "{}");
    // Answered in words alone: nobody saw it, so crew asks again.
    record(
        "words",
        &[user(&asked["words"]), said("It is src/parser.rs.")],
    );
    hook("words", "stop", "{}");
    s.wait_for("words to be asked again", || {
        told("words").contains("Your last turn ended without a tool call")
    });
    let again = told("words");
    assert!(
        again.contains("question from lead")
            && again.contains("decline(reason)")
            && again.contains("agent_status(\"idle\""),
        "{again}"
    );
    assert!(s.has_event("agent.unanswered"));
    assert_ne!(
        find_agent(&s.snapshot(), "words").status,
        AgentStatus::Blocked
    );
    // The decline tool: what the turn asked is dealt with as refused.
    let out = s.ctl(
        "decliner",
        &[
            "tool",
            "decline",
            &json!({ "reason": "it goes against my guidelines" }).to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    record(
        "decliner",
        &[
            user(&asked["decliner"]),
            called(
                "mcp__crew__decline",
                json!({ "reason": "it goes against my guidelines" }),
            ),
        ],
    );
    hook("decliner", "stop", "{}");
    let snap = s.wait("decliner to be set aside", |snap| {
        find_agent(snap, "decliner").block_kind.as_deref() == Some("refused")
    });
    assert!(
        find_agent(&snap, "decliner")
            .activity
            .contains("refused the turn (declined)"),
        "{:?}",
        find_agent(&snap, "decliner")
    );
    s.wait_for("lead to hear of it", || {
        told("lead").contains("decliner cannot act on your message")
    });
    s.wait_for("the orchestrator to hear of it", || told("orchestrator").contains("decliner's model (Sonnet 5.5) refused its turn: it declined to do it. It answered: \"it goes against my guidelines\""));

    // Asked again, words answers in words alone once more: taken as declined.
    let typed = s.workspace.join(".crew/agents/words/typed");
    s.wait_for("the question again to be typed", || {
        std::fs::read_to_string(&typed)
            .unwrap_or_default()
            .contains("Your last turn ended without a tool call")
    });
    let follow_up = s
        .messages()
        .into_iter()
        .find(|m| m.to == "words" && m.body.contains("Your last turn ended"))
        .unwrap();
    record(
        "words",
        &[
            user(&format!(
                "[crew] Message from scheduler (type status, id {}):\n\n{}",
                follow_up.id, follow_up.body
            )),
            said("Understood."),
        ],
    );
    hook("words", "stop", "{}");
    let snap = s.wait("words to be set aside", |snap| {
        find_agent(snap, "words").block_kind.as_deref() == Some("refused")
    });
    assert_eq!(find_agent(&snap, "words").status, AgentStatus::Blocked);
    s.wait_for("lead to hear of words", || {
        told("lead").contains("words cannot act on your message")
    });
    // The one that answered with a tool was left alone.
    let tools = find_agent(&s.snapshot(), "tools").clone();
    assert!(
        tools.block_kind.is_none() && !told("tools").contains("without a tool call"),
        "{tools:?}"
    );
}

#[test]
fn a_resting_member_gets_a_burst_as_one_prompt_and_news_only_with_it() {
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_CLAUDE_TYPED).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let mut team = s.team("gather", vec![agent("lead", "leads", &[])], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    team.agents.push(AgentSpec {
        id: "m".to_string(),
        runtime: Some(RuntimeKind::Claude),
        ..AgentSpec::default()
    });
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = s
        .command()
        .env("PATH", path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait("m to come up", |snap| {
        find_agent(snap, "m").status != AgentStatus::Spawning
    });
    // Its first turn ends: m is at rest.
    let identity = s.identity_path("m").to_string_lossy().into_owned();
    let out = s.crew(&["hook", "stop", "--identity", &identity, "{}"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let typed =
        || std::fs::read_to_string(s.workspace.join(".crew/agents/m/typed")).unwrap_or_default();
    let send = |kind: &str, body: &str| {
        let out = s.ctl(
            "lead",
            &[
                "tool",
                "agent_send",
                &json!({ "to": "m", "type": kind, "body": body }).to_string(),
            ],
        );
        assert!(out.status.success(), "{}", stderr(&out));
    };

    // News alone does not start a turn.
    send("status", "schema merged");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert!(
        !typed().contains("schema merged"),
        "news alone woke m:\n{}",
        typed()
    );
    // Two requests in quick succession: one prompt, the news with them.
    send("question", "Which port does the API use?");
    send("message", "Rebase on main.");
    s.wait_for("the burst to be typed", || {
        typed().contains("Rebase on main.")
    });
    let typed = typed();
    let lines: Vec<&str> = typed
        .lines()
        .filter(|l| {
            l.contains("schema merged") || l.contains("Which port") || l.contains("Rebase on main")
        })
        .collect();
    assert_eq!(lines.len(), 1, "{typed}");
    assert!(
        lines[0].contains("[crew] 3 new messages:")
            && lines[0].contains("schema merged")
            && lines[0].contains("Which port")
            && lines[0].contains("Rebase on main"),
        "{typed}"
    );
    // How to answer is in the briefing, not in every message.
    assert!(!typed.contains("directly with agent_send"), "{typed}");
}

#[test]
fn a_reply_is_carried_back_to_whoever_asked_and_an_at_line_starts_a_conversation() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let s = Session::new();
    let root = s.root().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/claude"), FAKE_CLAUDE_TYPED).unwrap();
    std::fs::set_permissions(
        root.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let mut team = s.team("carry", vec![], vec![]);
    team.defaults.delivery = crew_core::config::DeliveryChoice::Terminal;
    for id in ["a", "b"] {
        team.agents.push(AgentSpec {
            id: id.to_string(),
            runtime: Some(RuntimeKind::Claude),
            ..AgentSpec::default()
        });
    }
    std::fs::write(root.join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = s
        .command()
        .env("PATH", path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let snap = s.wait("both up", |snap| {
        snap.agents.len() == 2
            && snap
                .agents
                .iter()
                .all(|a| a.status != AgentStatus::Spawning)
    });
    let hook = |id: &str, event: &str, payload: &str| {
        let identity = s.identity_path(id).to_string_lossy().into_owned();
        let out = s.crew(&["hook", event, "--identity", &identity, payload]);
        assert!(out.status.success(), "{event}: {}", stderr(&out));
    };
    let transcript = |id: &str| root.join(format!("{id}.jsonl"));
    let record = |id: &str, lines: &[Value]| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(transcript(id))
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    };
    let user =
        |text: &str| json!({ "type": "user", "message": { "role": "user", "content": text } });
    let said = |text: &str| json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] } });
    let called = |tool: &str| json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "tool_use", "name": tool, "input": {} }] } });
    let typed = |id: &str| {
        std::fs::read_to_string(s.workspace.join(".crew/agents").join(id).join("typed"))
            .unwrap_or_default()
    };
    let between = |from: &str, to: &str| {
        s.messages()
            .into_iter()
            .filter(|m| m.from == from && m.to == to)
            .collect::<Vec<_>>()
    };
    for id in ["a", "b"] {
        let session = find_agent(&snap, id).session_id.clone().unwrap();
        hook(
            id,
            "session-start",
            &json!({ "session_id": session, "transcript_path": transcript(id) }).to_string(),
        );
        hook(id, "stop", "{}"); // its first turn is over: at rest
    }

    // a asks; b looks something up and ends its turn with the answer.
    let out = s.ctl(
        "a",
        &[
            "tool",
            "agent_send",
            &json!({ "to": "b", "type": "question", "body": "Which port does the API use?" })
                .to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait_for("the question to reach b", || {
        typed("b").contains("Which port does the API use?")
    });
    let question = between("a", "b").pop().unwrap();
    record(
        "b",
        &[
            user(&format!(
                "[crew] Message from a (type question, id {}):\n\nWhich port does the API use?",
                question.id
            )),
            called("Read"),
            said("Port 8080, set in `config.toml:12`."),
        ],
    );
    hook("b", "stop", "{}");
    // It reaches a as b's answer to that question, with no tool call.
    s.wait_for("the answer to reach a", || {
        typed("a").contains("Port 8080, set in `config.toml:12`.")
    });
    let answer = between("b", "a").pop().unwrap();
    assert_eq!(
        (answer.kind, answer.reply_to.as_deref()),
        (MessageType::Answer, Some(question.id.as_str()))
    );
    assert_eq!(answer.metadata["via"], "reply");

    // a's thanks answers an answer: it goes nowhere. Its @b line starts a
    // new conversation.
    record(
        "a",
        &[
            user(&format!(
                "[crew] Message from b (type answer, id {}):\n\n{}",
                answer.id, answer.body
            )),
            said("Thanks.\n@b: rebase on main, then rerun the tests."),
        ],
    );
    hook("a", "stop", "{}");
    s.wait_for("the @line to reach b", || {
        typed("b").contains("rebase on main, then rerun the tests.")
    });
    let asked = between("a", "b");
    assert_eq!(asked.len(), 2, "{asked:?}");
    let request = asked.last().unwrap();
    assert_eq!(
        (request.kind, request.body.as_str()),
        (
            MessageType::Message,
            "rebase on main, then rerun the tests."
        )
    );
    assert!(
        !asked.iter().any(|m| m.body.contains("Thanks")),
        "{asked:?}"
    );

    // b answers it with a tool call of its own: nothing is sent twice.
    let out = s.ctl(
        "b",
        &[
            "tool",
            "agent_send",
            &json!({ "to": "a", "type": "answer", "body": "rebased; tests green" }).to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    record(
        "b",
        &[
            user(&format!(
                "[crew] Message from a (type message, id {}):\n\n{}",
                request.id, request.body
            )),
            called("mcp__crew__agent_send"),
            said("Done."),
        ],
    );
    hook("b", "stop", "{}");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let answers: Vec<String> = between("b", "a").into_iter().map(|m| m.body).collect();
    assert_eq!(
        answers,
        [
            "Port 8080, set in `config.toml:12`.",
            "rebased; tests green"
        ]
    );
}

#[test]
fn one_message_can_go_to_several_teammates_each_with_its_own_copy() {
    let s = Session::new();
    let team = s.team(
        "several",
        vec![
            agent("a", "", &[]),
            agent("b", "", &[]),
            agent("c", "", &[]),
        ],
        vec![],
    );
    s.run(&team);
    s.wait("agents to come up", |snap| {
        snap.agents
            .iter()
            .all(|a| a.status != AgentStatus::Spawning)
    });
    let out = s.ctl(
        "a",
        &[
            "tool",
            "agent_send",
            &json!({ "to": "b, c, b", "type": "question", "body": "freeze main?" }).to_string(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        said.contains("Delivered to b (id") && said.contains("Delivered to c (id"),
        "{said}"
    );
    let sent: Vec<Message> = s.messages().into_iter().filter(|m| m.from == "a").collect();
    let mut to: Vec<&str> = sent.iter().map(|m| m.to.as_str()).collect();
    to.sort_unstable();
    assert_eq!(to, ["b", "c"], "one copy each, the repeat dropped");
    assert_ne!(sent[0].id, sent[1].id);
    assert!(sent
        .iter()
        .all(|m| m.kind == MessageType::Question && m.body == "freeze main?"));
    // A name that is not on the team: nothing is sent, to anyone.
    let out = s.ctl(
        "a",
        &[
            "tool",
            "agent_send",
            &json!({ "to": "b, ghost", "body": "x" }).to_string(),
        ],
    );
    assert!(
        !out.status.success() && stderr(&out).contains("no agent ghost; nothing was sent"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        s.messages().into_iter().filter(|m| m.from == "a").count(),
        2
    );
}

#[test]
fn a_task_is_handed_over_by_its_id_and_read_off_its_card() {
    let s = Session::new();
    let ws = s.workspace.to_string_lossy().into_owned();
    let team = s.team(
        "board",
        vec![
            agent(
                "db",
                "",
                &[
                    "--on-task=write schema.sql CREATE TABLE t (id INTEGER);",
                    "--on-task=commit Add schema",
                    "--on-task=done schema committed",
                ],
            ),
            agent(
                "ledger",
                "",
                &[
                    "--on-task=wait-message operator",
                    "--on-task=done ledger built",
                ],
            ),
        ],
        vec![
            task("schema", "db", &[]),
            task("ledger", "ledger", &["schema"]),
        ],
    );
    s.run(&team);
    s.wait("ledger to be at work", |snap| {
        find_task(snap, "ledger").status == TaskStatus::Working
    });
    // The dispatch names the card; what the card says stays on the board.
    let dispatch = s
        .messages()
        .into_iter()
        .find(|m| m.to == "ledger" && m.kind == MessageType::Task)
        .expect("ledger's task message");
    assert_eq!(
        dispatch.body,
        "Task ledger is yours: \"task ledger\". task_get(\"ledger\") reads its card."
    );
    // The card: the work, and what it builds on with its result and commit.
    let out = s.ctl("ledger", &["task"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let card = stdout(&out);
    let commit = find_task(&s.snapshot(), "schema")
        .artifacts
        .iter()
        .find(|a| a.kind == "commit")
        .map(|a| a.value.clone());
    let commit = commit.expect("schema's commit on its card");
    assert!(
        card.starts_with("ledger [working] ledger — task ledger\n"),
        "{card}"
    );
    assert!(card.contains("\ndo ledger\n"), "{card}");
    assert!(
        card.contains("- schema [done] by db: schema committed\n")
            && card.contains(&format!("commit {commit}")),
        "{card}"
    );
    // Any card is read by its id.
    assert!(stdout(&s.ctl("ledger", &["task", "schema"])).contains("\ndo schema\n"));

    // A revised card: whoever works on it is pointed back to it.
    s.ok(&[
        "task",
        "edit",
        "ledger",
        "--description",
        "do ledger, with an audit column",
        "-C",
        &ws,
    ]);
    s.wait("ledger to be told", |_| {
        s.messages().iter().any(|m| m.to == "ledger" && m.from == "scheduler" && m.body == "Card ledger revised by operator (description): task_get(\"ledger\") reads it.")
    });
    assert!(stdout(&s.ctl("ledger", &["task"])).contains("\ndo ledger, with an audit column\n"));
    assert!(s.has_event("task.edited"));
    let edited = s.crew(&["task", "edit", "schema", "--title", "again", "-C", &ws]);
    assert!(
        !edited.status.success() && stderr(&edited).contains("task schema is done"),
        "{}",
        stderr(&edited)
    );

    // A message is pointed to by its id, for whoever took part in it.
    let read = |who: &str| {
        s.ctl(
            who,
            &[
                "tool",
                "agent_inbox",
                &json!({ "id": dispatch.id }).to_string(),
            ],
        )
    };
    let out = read("ledger");
    assert!(
        out.status.success() && stdout(&out).contains("Task ledger is yours"),
        "{}",
        stderr(&out)
    );
    let out = read("db");
    assert!(
        !out.status.success() && stderr(&out).contains("sent or received by you"),
        "{}",
        stderr(&out)
    );

    s.send("ledger", "go ahead");
    let done = s.wait_all_done();
    assert_eq!(
        find_task(&done, "ledger").result.as_deref(),
        Some("ledger built")
    );
}
