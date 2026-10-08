//! Wire fixtures for the web dashboard (notes/web-design.md §tests, W12).
//!
//! Each test builds values with the real wire types and compares them, as
//! JSON, with a file under `tests/fixtures/web/`. A change to a wire type
//! shows up as a difference here; `CREW_UPDATE_FIXTURES=1 cargo test --test
//! web_wire` rewrites the files, and the frontend's tests then say whether
//! its decoders still read them. Timestamps are fixed, so the files are
//! stable.

use std::path::PathBuf;

use crew_core::rpc::{
    Call, CommitInfo, ConflictReport, DaemonRef, GitOutcome, Hello, HelloInfo, InboxResult,
    ModelsResult, OtherSession, OutputResult, Push, Request, Role, RpcError, SendResult,
    ServerFrame, TaskCard, TranscriptResult, WebInfo, WebPaired, WorkspaceEntry, WorkspaceSummary,
    Workspaces, CLOSE_WORKSPACE_ENDED, CLOSE_WORKSPACE_REMOVED, PROTOCOL_VERSION,
};
use crew_core::types::{
    AgentStatus, Event, EventLevel, InboxEntry, Message, MessageType, RuntimeKind, TaskStatus,
    TranscriptEntry, TranscriptKind, WebEventKind, WorkspaceState, AGENT_ERROR_EVENT,
    AGENT_REFUSED_EVENT, BROADCAST, OPERATOR_ID, ORCHESTRATOR_ID,
};
use crew_core::vcs::{VcsAction, VcsEvent, VcsOutcome, INTEGRATION_AGENT};
use crew_tui::anim::{Anims, LinkKind, MarkKind, Tone};
use crew_tui::fixture;
use serde::Serialize;
use serde_json::{json, Value};

/// The moment every fixture is built at.
const NOW: i64 = 1_760_000_000_000;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/web")
}

/// Compare `contents` with the fixture `name`, or write it under
/// `CREW_UPDATE_FIXTURES=1`.
fn check(name: &str, contents: String) {
    let path = dir().join(name);
    if std::env::var("CREW_UPDATE_FIXTURES").as_deref() == Ok("1") {
        std::fs::create_dir_all(dir()).unwrap();
        std::fs::write(&path, &contents).unwrap();
        return;
    }
    let found = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        found == contents,
        "{} differs from what the wire types serialize now; if the change is meant, run \
`CREW_UPDATE_FIXTURES=1 cargo test -p crew --test web_wire` and tell the frontend",
        path.display()
    );
}

fn pretty<T: Serialize>(value: &T) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    text
}

fn value<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap()
}

#[test]
fn snapshots() {
    check("snapshot.json", pretty(&fixture::snapshot(NOW)));
    check("subagents.json", pretty(&fixture::with_subagents(NOW)));
    check("team10.json", pretty(&fixture::team(10, NOW)));
    check("toolbox.json", pretty(&fixture::toolbox_agents(NOW)));
}

// ---- pushes ------------------------------------------------------------------

/// A full commit id from a short one.
fn full(short: &str) -> String {
    format!(
        "{short}{}",
        &"0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e"[..40 - short.len()]
    )
}

fn vcs(action: VcsAction, outcome: VcsOutcome, op: &str, agent: &str) -> VcsEvent {
    VcsEvent::new(action, outcome, op, "jj", agent)
}

/// One VCS event for every row of jj-design §events: each `started` with its
/// terminal event, conflicts, errors and both reads, as the daemon fills
/// them.
fn vcs_events() -> Vec<(String, VcsEvent)> {
    let mut out: Vec<(String, VcsEvent)> = Vec::new();
    let mut add = |message: &str, event: VcsEvent| out.push((message.to_string(), event));
    let branch = |id: &str| Some(format!("agents/{id}"));

    // A member's workspace: a subagent, so with its parent as the peer.
    let ws = |outcome| VcsEvent {
        peer: Some("claude-3".into()),
        branch: branch("claude-3-s1"),
        ..vcs(VcsAction::Workspace, outcome, "v1", "claude-3-s1")
    };
    add(
        "creating the workspace of claude-3-s1…",
        ws(VcsOutcome::Started),
    );
    add(
        "workspace for claude-3-s1 on agents/claude-3-s1 at 1a2b3c4d",
        VcsEvent {
            commit: Some(full("1a2b3c4d")),
            ..ws(VcsOutcome::Ok)
        },
    );
    add(
        "creating the workspace of claude-7…",
        VcsEvent {
            branch: branch("claude-7"),
            ..vcs(VcsAction::Workspace, VcsOutcome::Started, "v2", "claude-7")
        },
    );
    add(
        "claude-7: creating the workspace failed: /work/demo/.crew/worktrees/claude-7 already exists",
        VcsEvent {
            branch: branch("claude-7"),
            summary: Some("/work/demo/.crew/worktrees/claude-7 already exists".into()),
            ..vcs(VcsAction::Workspace, VcsOutcome::Error, "v2", "claude-7")
        },
    );
    // Commits.
    add(
        "claude-1 committed 3f4e5d6c on agents/claude-1: Add billing totals",
        VcsEvent {
            commit: Some(full("3f4e5d6c")),
            change: Some("kzvsvsvnkzvsvsvnkzvsvsvnkzvsvsvn".into()),
            branch: branch("claude-1"),
            summary: Some("Add billing totals".into()),
            ..vcs(VcsAction::Commit, VcsOutcome::Ok, "v3", "claude-1")
        },
    );
    add(
        "claude-4: commit failed: nothing to commit: the working tree has no changes",
        VcsEvent {
            branch: branch("claude-4"),
            summary: Some("nothing to commit: the working tree has no changes".into()),
            ..vcs(VcsAction::Commit, VcsOutcome::Error, "v4", "claude-4")
        },
    );
    // Reads.
    add(
        "claude-2: status",
        vcs(VcsAction::Status, VcsOutcome::Ok, "v5", "claude-2"),
    );
    add(
        "claude-2: diff against agents/claude-4",
        VcsEvent {
            source: Some("agents/claude-4".into()),
            ..vcs(VcsAction::Diff, VcsOutcome::Ok, "v6", "claude-2")
        },
    );
    add(
        "claude-6: status failed: the working copy is stale",
        VcsEvent {
            summary: Some("the working copy is stale".into()),
            ..vcs(VcsAction::Status, VcsOutcome::Error, "v7", "claude-6")
        },
    );
    add(
        "claude-6: diff failed: unknown revision 'nope'",
        VcsEvent {
            source: Some("nope".into()),
            summary: Some("unknown revision 'nope'".into()),
            ..vcs(VcsAction::Diff, VcsOutcome::Error, "v8", "claude-6")
        },
    );
    // Cherry-picks: ok, conflict, error.
    let pick = |op: &str, outcome| VcsEvent {
        branch: branch("claude-4"),
        ..vcs(VcsAction::CherryPick, outcome, op, "claude-4")
    };
    add(
        "claude-4: cherry-pick of 5a6b7c8d…",
        VcsEvent {
            source: Some("5a6b7c8d".into()),
            ..pick("v9", VcsOutcome::Started)
        },
    );
    add(
        "claude-4 cherry-picked 5a6b7c8d from claude-5 as 7e8f9a0b",
        VcsEvent {
            peer: Some("claude-5".into()),
            source: Some(full("5a6b7c8d")),
            commit: Some(full("7e8f9a0b")),
            change: Some("pqrstuvwpqrstuvwpqrstuvwpqrstuvw".into()),
            summary: Some("Add the queue schema".into()),
            ..pick("v9", VcsOutcome::Ok)
        },
    );
    add(
        "claude-4: cherry-pick of d082aaff…",
        VcsEvent {
            source: Some("d082aaff".into()),
            ..pick("v10", VcsOutcome::Started)
        },
    );
    add(
        "claude-4: cherry-pick of d082aaff conflicts with claude-6 in schema.sql; nothing was changed",
        VcsEvent {
            peer: Some("claude-6".into()),
            source: Some(full("d082aaff")),
            files: vec!["schema.sql".into()],
            ..pick("v10", VcsOutcome::Conflict)
        },
    );
    add(
        "claude-4: cherry-pick of deadbeef…",
        VcsEvent {
            source: Some("deadbeef".into()),
            ..pick("v11", VcsOutcome::Started)
        },
    );
    add(
        "claude-4: cherry-pick failed: unknown commit 'deadbeef'",
        VcsEvent {
            source: Some("deadbeef".into()),
            summary: Some("unknown commit 'deadbeef'".into()),
            ..pick("v11", VcsOutcome::Error)
        },
    );
    // Merges: ok, conflict, error (a timeout that kept its result).
    let merge = |op: &str, outcome, peer: &str| VcsEvent {
        peer: Some(peer.into()),
        source: branch(peer),
        branch: branch("claude-3"),
        ..vcs(VcsAction::Merge, outcome, op, "claude-3")
    };
    add(
        "claude-3: merging agents/claude-5…",
        merge("v12", VcsOutcome::Started, "claude-5"),
    );
    add(
        "claude-3 merged agents/claude-5 (5e6f7a8b)",
        VcsEvent {
            commit: Some(full("5e6f7a8b")),
            change: Some("kzvsvsvnpqrstuvwkzvsvsvnpqrstuvw".into()),
            summary: Some("Merge agents/claude-5".into()),
            ..merge("v12", VcsOutcome::Ok, "claude-5")
        },
    );
    add(
        "claude-3: merging agents/claude-6…",
        merge("v13", VcsOutcome::Started, "claude-6"),
    );
    add(
        "claude-3: merge of agents/claude-6 conflicts in src/ledger.rs, db/schema.sql; nothing was changed",
        VcsEvent {
            files: vec!["src/ledger.rs".into(), "db/schema.sql".into()],
            ..merge("v13", VcsOutcome::Conflict, "claude-6")
        },
    );
    add(
        "claude-3: merging agents/claude-2…",
        merge("v14", VcsOutcome::Started, "claude-2"),
    );
    add(
        "claude-3: merge failed: jj merge timed out (jj did not finish within 300s and was killed) after it completed: 6b7c8d9e is on agents/claude-3; the result was kept, check the history before retrying",
        VcsEvent {
            commit: Some(full("6b7c8d9e")),
            summary: Some("jj merge timed out (jj did not finish within 300s and was killed) after it completed: 6b7c8d9e is on agents/claude-3; the result was kept, check the history before retrying".into()),
            ..merge("v14", VcsOutcome::Error, "claude-2")
        },
    );
    // An integration run: its start, one event per branch, the tally.
    let run = |outcome| VcsEvent {
        branch: Some("agents/integration".into()),
        ..vcs(VcsAction::Integrate, outcome, "v15", INTEGRATION_AGENT)
    };
    let step = |op: &str, outcome, peer: &str| VcsEvent {
        peer: Some(peer.into()),
        source: branch(peer),
        branch: Some("agents/integration".into()),
        ..vcs(VcsAction::Integrate, outcome, op, INTEGRATION_AGENT)
    };
    add(
        "integrating into agents/integration…",
        run(VcsOutcome::Started),
    );
    add(
        "integration merged agents/claude-5 (9c0d1e2f)",
        VcsEvent {
            commit: Some(full("9c0d1e2f")),
            change: Some("wvutsrqpwvutsrqpwvutsrqpwvutsrqp".into()),
            summary: Some("Merge agents/claude-5".into()),
            ..step("v16", VcsOutcome::Ok, "claude-5")
        },
    );
    add(
        "integration: merge of agents/claude-6 conflicts in schema.sql; skipped",
        VcsEvent {
            files: vec!["schema.sql".into()],
            ..step("v17", VcsOutcome::Conflict, "claude-6")
        },
    );
    add(
        "integration on agents/integration: 1 merged, 1 conflict",
        VcsEvent {
            commit: Some(full("9c0d1e2f")),
            summary: Some("1 merged, 1 conflict".into()),
            ..run(VcsOutcome::Conflict)
        },
    );
    add(
        "integrating into agents/integration…",
        VcsEvent {
            op: "v18".into(),
            ..run(VcsOutcome::Started)
        },
    );
    add(
        "integration on agents/integration: 2 merged, 0 conflicts",
        VcsEvent {
            op: "v18".into(),
            commit: Some(full("ab12cd34")),
            summary: Some("2 merged, 0 conflicts".into()),
            ..run(VcsOutcome::Ok)
        },
    );
    add(
        "integrating into agents/integration…",
        VcsEvent {
            op: "v19".into(),
            ..run(VcsOutcome::Started)
        },
    );
    add(
        "integration: integrate failed: interrupted",
        VcsEvent {
            op: "v19".into(),
            summary: Some("interrupted".into()),
            ..run(VcsOutcome::Error)
        },
    );
    out
}

fn event(seq: i64, message: &str, kind: &str, agent: Option<&str>, level: EventLevel) -> Event {
    let mut event = fixture::event(seq, level, "daemon", kind, agent, message, NOW + seq * 100);
    event.data = Value::Null;
    event
}

fn pushes() -> Vec<Value> {
    let snap = fixture::snapshot(NOW);
    let mut frames: Vec<Value> = Vec::new();
    let mut push = |p: Push| frames.push(value(ServerFrame::push(p)));
    let mut seq = 1000;
    // Every kind the page handles.
    push(Push::Agent(Box::new(snap.agents[0].clone())));
    push(Push::Task(Box::new(snap.tasks[0].clone())));
    push(Push::Session(Box::new(snap.session.clone())));
    push(Push::Message(fixture::message(
        101,
        "claude-1",
        "claude-2",
        MessageType::Message,
        "The billing totals are in; take them from 3f4e5d6c.",
        NOW + 1000,
    )));
    push(Push::Message(fixture::message(
        102,
        ORCHESTRATOR_ID,
        BROADCAST,
        MessageType::Status,
        "Everyone: rebase on agents/integration before the next commit.",
        NOW + 1100,
    )));
    // An operator question and its answer.
    let question = fixture::question(NOW + 1200);
    push(Push::Message(question.clone()));
    let mut answer = fixture::message(
        103,
        OPERATOR_ID,
        ORCHESTRATOR_ID,
        MessageType::Answer,
        "Convert in the adapter",
        NOW + 1300,
    );
    answer.reply_to = Some(question.id.clone());
    push(Push::Message(answer));
    push(Push::AgentRemoved {
        id: "claude-6".into(),
    });
    for (kind, level, text) in [
        (
            AGENT_ERROR_EVENT,
            EventLevel::Error,
            "claude-2: the provider answered 529 overloaded; the turn ended",
        ),
        (
            AGENT_REFUSED_EVENT,
            EventLevel::Warn,
            "claude-2: the model refused the turn",
        ),
        ("session.vcs", EventLevel::Info, "version control: jj 0.44.0 (state in .crew/jj; agent branches are exported to git as agents/<id>)"),
    ] {
        seq += 1;
        push(Push::Event(event(seq, text, kind, Some("claude-2").filter(|_| kind != "session.vcs"), level)));
    }
    for (message, vcs) in vcs_events() {
        seq += 1;
        push(Push::Event(fixture::vcs_event(
            seq,
            NOW + seq * 100,
            &message,
            vcs,
        )));
    }
    push(Push::Resync);
    // A terminal socket's frames.
    push(Push::Pty {
        data: "G1sxbWNyZXcbWzBtICQgY2FyZ28gdGVzdA0K".into(),
    });
    push(Push::PtyClosed);
    push(Push::Shutdown);
    // A kind this build does not know: the page ignores it.
    frames.push(json!({"push": {"kind": "future_kind", "data": {"anything": [1, 2, 3]}}}));
    frames
}

#[test]
fn pushes_jsonl() {
    let mut text = String::new();
    for frame in pushes() {
        text.push_str(&serde_json::to_string(&frame).unwrap());
        text.push('\n');
    }
    check("pushes.jsonl", text);
}

// ---- results and errors --------------------------------------------------------

fn results() -> Value {
    let snap = fixture::snapshot(NOW);
    let agent = snap.agents[0].clone();
    let message = fixture::message(
        104,
        OPERATOR_ID,
        "claude-1",
        MessageType::Message,
        "Please add the rounding rule to the totals.",
        NOW + 2000,
    );
    let sent = SendResult {
        message: message.clone(),
        duplicate: false,
        recipients: vec!["claude-1".into()],
    };
    let commit = CommitInfo {
        sha: full("3f4e5d6c"),
        short: "3f4e5d6c".into(),
        branch: "agents/claude-1".into(),
        summary: "Add billing totals".into(),
        files_changed: 2,
        change_id: Some("kzvsvsvnkzvsvsvnkzvsvsvnkzvsvsvn".into()),
    };
    let diff = GitOutcome {
        ok: true,
        commit: None,
        conflict: None,
        text: "M src/billing.rs\n\ndiff --git a/src/billing.rs b/src/billing.rs\n--- a/src/billing.rs\n+++ b/src/billing.rs\n@@ -1 +1,2 @@\n pub fn total() {}\n+pub fn round() {}".into(),
    };
    let merged = GitOutcome {
        ok: true,
        commit: Some(commit.clone()),
        conflict: None,
        text: "merged agents/claude-5 as 3f4e5d6c".into(),
    };
    let conflicted = GitOutcome {
        ok: false,
        commit: None,
        conflict: Some(ConflictReport {
            operation: "merge".into(),
            files: vec!["schema.sql".into()],
            detail: "merge of agents/claude-6 conflicts in 1 file(s)".into(),
        }),
        text: "merge aborted because of conflicts; the worktree is unchanged".into(),
    };
    let tasks = snap.tasks.clone();
    let card = TaskCard {
        task: tasks[0].clone(),
        dependencies: tasks.iter().skip(1).take(1).cloned().collect(),
    };
    let transcript = TranscriptResult {
        entries: vec![
            TranscriptEntry {
                kind: TranscriptKind::Prompt,
                at: Some(NOW - 60_000),
                text: "[crew] Task billing is yours.".into(),
            },
            TranscriptEntry {
                kind: TranscriptKind::Tool,
                at: Some(NOW - 50_000),
                text: "Edit src/billing.rs".into(),
            },
            TranscriptEntry {
                kind: TranscriptKind::Reply,
                at: Some(NOW - 40_000),
                text: "Totals done, committed 3f4e5d6c.".into(),
            },
            TranscriptEntry {
                kind: TranscriptKind::Note,
                at: None,
                text: "compacted".into(),
            },
        ],
        available: true,
        truncated: false,
    };
    let inbox = InboxResult {
        messages: vec![InboxEntry {
            message: message.clone(),
            delivered_at: Some(NOW + 2100),
            acked_at: None,
        }],
    };
    let hello = HelloInfo {
        role: Role::Operator,
        agent_id: None,
        project: Some("checkout-rewrite".into()),
        version: "0.1.0".into(),
        protocol: PROTOCOL_VERSION,
        pid: 4242,
        web_proof: Some("pr00f5ecretpr00f5ecretpr00f5ecretpr00f5ecret".into()),
    };
    let paired = WebPaired {
        token: "t0kent0kent0kent0kent0kent0kent0kent0kent0k".into(),
        verifier: "9d5b3f0b4e8c4a6f2d1e0c9b8a7f6e5d4c3b2a1f0e9d8c7b6a5f4e3d2c1b0a9f".into(),
    };
    let info = web_info();
    let output = OutputResult {
        text: "\u{1b}[1mcrew\u{1b}[0m $ cargo test\ntest result: ok. 12 passed".into(),
        alive: true,
    };
    let mut paused = agent.clone();
    paused.paused = true;
    let mut killed = agent.clone();
    killed.status = AgentStatus::Stopped;
    let mut moved = agent.clone();
    moved.model = Some("opus".into());
    moved.model_name = Some("Opus 5.5".into());
    moved.runtime = RuntimeKind::Claude;
    json!({
        "web_pair": [value(&paired)],
        "hello": [value(&hello)],
        "subscribe": [{"subscribed": true}],
        "snapshot": [value(&snap)],
        "send": [value(&sent), value(SendResult { duplicate: true, ..sent.clone() })],
        "answer": [value(&sent)],
        "agent_pause": [value(&paused)],
        "agent_resume": [value(&agent)],
        "agent_restart": [value(&agent)],
        "agent_restart_fresh": [value(&agent)],
        "agent_kill": [value(&killed), Value::Null],
        "agent_compact": [value(&agent)],
        "models_list": [value(ModelsResult { models: fixture::models() })],
        "agent_set_model": [value(&moved)],
        "resume": [{"resumed": ["claude-1", "claude-2"]}],
        "shutdown": [{"stopping": true}],
        "agent_focus": [{"ok": true}],
        "agent_input": [{"ok": true}],
        "agent_output": [value(&output)],
        "agent_transcript": [value(&transcript)],
        "inbox": [value(&inbox)],
        "events": [value(&snap.events)],
        "history": [value(&snap.messages)],
        "message_get": [value(&message)],
        "task_get": [value(&card)],
        "tasks_list": [value(&tasks)],
        "git_diff": [value(&diff)],
        "git_merge": [value(&merged), value(&conflicted)],
        "web_prefs": [{"prefs": {}}, {"prefs": {"positions": {"claude-1": [120.5, -40.0]}, "keys": true}}],
        "web_open": [value(&info)],
        "web_close": [{"stopped": true}, {"stopped": false}, {"removed": true}],
        "workspaces": [value(listing())],
        "summary": [value(summary(12))],
    })
}

// ---- several workspaces (notes/web-multi.md §multi-tests) -------------------------

const ORIGIN: &str = "http://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417/";
const PAYMENTS: &str = "946d4617aa702870";
const SEARCH: &str = "0b54c1d2e3f4a596";

fn web_info() -> WebInfo {
    WebInfo {
        url: ORIGIN.into(),
        port: 23417,
        pair_url: format!("{ORIGIN}#pair=3f9ac0dec0dec0dec0dec0dec0dec0dec0dec0dee1&w={PAYMENTS}"),
        expires_in_secs: 120,
        started: true,
        workspace: Some(PAYMENTS.into()),
        workspace_url: Some(format!("{ORIGIN}#/w/{PAYMENTS}/")),
        joined: vec![PAYMENTS.into()],
    }
}

fn summary(rev: u64) -> WorkspaceSummary {
    WorkspaceSummary {
        rev,
        project: Some("payments".into()),
        agents: 6,
        working: 3,
        attention: 1,
        questions: 2,
        tasks: 6,
        tasks_done: 4,
    }
}

/// A listing with an entry in every state, and a session not joined.
fn listing() -> Workspaces {
    let entry = |id: &str, generation: u64, project: &str, state: WorkspaceState| WorkspaceEntry {
        id: id.into(),
        generation,
        daemon: (state == WorkspaceState::Running).then_some(DaemonRef {
            pid: 4242,
            started_at: NOW - 3_600_000,
        }),
        project: Some(project.into()),
        workspace: format!("/home/op/work/{project}"),
        state,
        detail: match state {
            WorkspaceState::Incompatible => {
                "started by another version of crew (protocol 5): restart the team".into()
            }
            WorkspaceState::Unreachable => "its daemon did not answer within 2 s".into(),
            _ => String::new(),
        },
        summary: match state {
            WorkspaceState::Running | WorkspaceState::Stopped => Some(summary(12)),
            _ => None,
        },
        joined_at: NOW - 60_000 + generation as i64,
    };
    Workspaces {
        rev: 41,
        joined: vec![
            entry(PAYMENTS, 1, "payments", WorkspaceState::Running),
            entry("4c2e8a1b9d0f3e57", 2, "billing", WorkspaceState::Stopped),
            entry("7a1d2c3b4e5f6071", 3, "infra", WorkspaceState::Incompatible),
            entry("e0f1a2b3c4d5e6f7", 5, "search", WorkspaceState::Unreachable),
        ],
        others: vec![OtherSession {
            id: SEARCH.into(),
            project: Some("docs".into()),
            workspace: "/home/op/work/docs".into(),
        }],
    }
}

fn request(id: u64, call: Call) -> Value {
    value(Request { id, call })
}

fn hello(workspace: Option<&str>) -> Call {
    Call::Hello(Hello {
        role: Role::Operator,
        agent_id: None,
        token: Some("t0kent0kent0kent0kent0kent0kent0kent0kent0k".into()),
        client: "web".into(),
        protocol: PROTOCOL_VERSION,
        web: false,
        workspace: workspace.map(str::to_string),
    })
}

/// Everything a page and the dashboard server say to each other that
/// protocol 5 did not have, with the control socket's `web_open` and the
/// daemon's refusals of the calls it no longer serves.
fn multi() -> Value {
    let proof = "pr00f5ecretpr00f5ecretpr00f5ecretpr00f5ecret";
    let index_reply = HelloInfo {
        role: Role::Operator,
        agent_id: None,
        project: None,
        version: "0.1.0".into(),
        protocol: PROTOCOL_VERSION,
        pid: 31337,
        web_proof: Some(proof.into()),
    };
    let workspace_reply = HelloInfo {
        project: Some("payments".into()),
        pid: 4242,
        ..index_reply.clone()
    };
    let err = |id: u64, error: RpcError| value(ServerFrame::err(id, error));
    json!({
        "hello": {
            "index": {
                "request": request(1, hello(None)),
                "reply": value(ServerFrame::ok(1, value(&index_reply))),
            },
            "workspace": {
                "request": request(1, hello(Some(PAYMENTS))),
                "reply": value(ServerFrame::ok(1, value(&workspace_reply))),
                // What the server sends the daemon in its place.
                "to_daemon": request(0, Call::Hello(Hello {
                    token: Some("daem0nt0kendaem0nt0kendaem0nt0kendaem0nt0k".into()),
                    web: true,
                    workspace: None,
                    ..match hello(None) { Call::Hello(h) => h, _ => unreachable!() }
                })),
            },
            "workspace_refused": {
                "not_found": err(1, RpcError::not_found(format!(
                    "workspace {SEARCH} is not in this dashboard: run `crew web` in it"
                ))),
                "unavailable": err(1, RpcError::unavailable(
                    "billing is not running: start it (`crew run` in /home/op/work/billing)"
                )),
                "protocol": err(1, RpcError::new(
                    "protocol",
                    "infra was started by another version of crew (protocol 5): restart the team",
                )),
                "unauthorized": err(1, RpcError::unauthorized(
                    "this browser is signed out: run `crew web` and open the address it prints",
                )),
            },
        },
        "workspaces": {
            "request": request(2, Call::Workspaces),
            "reply": value(ServerFrame::ok(2, value(listing()))),
            "push": value(ServerFrame::push(Push::Workspaces(Workspaces {
                rev: 42,
                ..listing()
            }))),
        },
        "summary": {
            "request": request(3, Call::Summary { watch: true }),
            "reply": value(ServerFrame::ok(3, value(summary(12)))),
            "push": value(ServerFrame::push(Push::Summary(summary(13)))),
        },
        "web_attach": {
            "request": request(4, Call::WebAttach { url: ORIGIN.into() }),
            "reply": value(ServerFrame::ok(4, json!({"attached": true}))),
        },
        "web_event": {
            "request": request(5, Call::WebEvent { kind: WebEventKind::Paired, count: 3 }),
            "reply": value(ServerFrame::ok(5, json!({"ok": true}))),
        },
        "web_open": {
            "request": request(6, Call::WebOpen {
                port: None,
                workspace: Some("/home/op/work/payments/.crew".into()),
                all: false,
                restore: None,
            }),
            "reply": value(ServerFrame::ok(6, value(web_info()))),
        },
        "web_close": {
            "request": request(7, Call::WebClose { workspace: Some(SEARCH.into()) }),
            "reply": value(ServerFrame::ok(7, json!({"removed": true}))),
        },
        "daemon_refusals": {
            "web_open": err(8, RpcError::invalid(
                "web_open is served by the dashboard server: run `crew web`"
            )),
            "web_close": err(9, RpcError::invalid(
                "web_close is served by the dashboard server: run `crew web --stop`"
            )),
            "web_pair": err(10, RpcError::invalid(
                "web_pair is served by the dashboard server: open the address `crew web` prints"
            )),
        },
        "close": {
            "workspace_ended": {"code": CLOSE_WORKSPACE_ENDED, "reason": "the workspace's connection ended"},
            "workspace_removed": {"code": CLOSE_WORKSPACE_REMOVED, "reason": "taken out of this dashboard"},
            "line_break": {"code": 1008, "reason": "one request per message: no line breaks"},
        },
    })
}

#[test]
fn multi_json() {
    check("multi.json", pretty(&multi()));
}

#[test]
fn results_json() {
    check("results.json", pretty(&results()));
}

#[test]
fn errors_json() {
    let examples = [
        RpcError::unauthorized("say hello first"),
        RpcError::new("protocol", "client speaks protocol 5, daemon speaks 6"),
        RpcError::forbidden("web sessions cannot call web_open"),
        RpcError::not_found("no agent 'claude-9'"),
        RpcError::invalid("malformed request: missing field `id`"),
        RpcError::conflict("message id m-0104 is already used for another message"),
        RpcError::too_large("message body exceeds 65536 bytes"),
        RpcError::unavailable("the web dashboard is stopping"),
        RpcError::internal("persisting task billing failed"),
        RpcError::new("git", "nothing to commit: the working tree has no changes"),
    ];
    let frames: Vec<Value> = examples
        .into_iter()
        .enumerate()
        .map(|(i, error)| value(ServerFrame::err(i as u64 + 1, error)))
        .collect();
    check("errors.json", pretty(&frames));
}

#[test]
fn enums_json() {
    let push_kinds = [
        "event",
        "message",
        "agent",
        "agent_removed",
        "task",
        "session",
        "resync",
        "deliver",
        "compact",
        "pty",
        "pty_closed",
        "shutdown",
        "summary",
        "workspaces",
    ];
    // Each kind in the list is one the daemon serializes.
    for kind in push_kinds {
        let frame = pushes()
            .into_iter()
            .chain([
                value(ServerFrame::push(Push::Deliver {
                    ids: vec![],
                    text: String::new(),
                })),
                value(ServerFrame::push(Push::Compact { instructions: None })),
                value(ServerFrame::push(Push::Summary(summary(1)))),
                value(ServerFrame::push(Push::Workspaces(listing()))),
            ])
            .find(|f| f["push"]["kind"] == kind);
        assert!(frame.is_some(), "no push of kind {kind}");
    }
    let enums = json!({
        "protocol": PROTOCOL_VERSION,
        "agent_status": value(AgentStatus::ALL),
        "task_status": value(TaskStatus::ALL),
        "message_type": value(MessageType::ALL),
        "runtime_kind": value(RuntimeKind::ALL),
        "event_level": value(EventLevel::ALL),
        "transcript_kind": value(TranscriptKind::ALL),
        "role": value([Role::Operator, Role::Agent]),
        "push": push_kinds,
        "vcs_action": value(VcsAction::ALL),
        "vcs_outcome": value([VcsOutcome::Started, VcsOutcome::Ok, VcsOutcome::Conflict, VcsOutcome::Error]),
        "workspace_state": value(WorkspaceState::ALL),
        "web_event_kind": value(WebEventKind::ALL),
    });
    check("enums.json", pretty(&enums));
}

#[test]
fn commands_json() {
    let commands: Vec<Value> = crew_tui::slash::DASHBOARD
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "aliases": c.aliases,
                "args": c.args,
                "about": c.about,
            })
        })
        .collect();
    check("commands.json", pretty(&commands));
}

// ---- animation cases ---------------------------------------------------------------

/// What a case feeds the animation state at a time.
enum Input {
    Message(Message),
    Vcs(VcsEvent),
    /// Events of a snapshot, with how long ago each happened.
    Snapshot(Vec<(VcsEvent, i64)>),
    Remove(String),
}

struct Case {
    name: &'static str,
    workers: Vec<String>,
    steps: Vec<(i64, Input)>,
    checks: Vec<i64>,
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn tone(t: Tone) -> &'static str {
    match t {
        Tone::Ok => "ok",
        Tone::Conflict => "conflict",
        Tone::Error => "error",
    }
}

/// What `anims` holds at `now`, after `gc(now)`.
fn observe(anims: &mut Anims, workers: &[String], now: i64) -> Value {
    anims.gc(now);
    let links: Vec<Value> = anims
        .links
        .iter()
        .map(|l| {
            let (kind, t) = match l.kind {
                LinkKind::P2p => ("p2p", None),
                LinkKind::Orchestrator => ("orchestrator", None),
                LinkKind::Broadcast => ("broadcast", None),
                LinkKind::Vcs(t) => ("vcs", Some(tone(t))),
            };
            json!({
                "from": l.from, "to": l.to, "kind": kind, "tone": t,
                "held": l.held.is_some(), "pulse": l.pulse(now).map(round3), "live": l.is_live(now),
            })
        })
        .collect();
    let marks: Vec<Value> = anims
        .marks
        .iter()
        .map(|m| {
            let kind = match &m.kind {
                MarkKind::Running { .. } => "running",
                MarkKind::Done => "done",
                MarkKind::Glance => "glance",
            };
            json!({
                "agent": m.agent, "kind": kind, "tone": tone(m.tone), "label": m.label,
                "started": m.started, "live": m.is_live(now), "sweep": m.sweep(now).map(round3),
            })
        })
        .collect();
    let mut boxes: Vec<String> = workers.to_vec();
    boxes.push(ORCHESTRATOR_ID.to_string());
    let shown: serde_json::Map<String, Value> = boxes
        .iter()
        .filter_map(|id| {
            anims
                .mark_for(id, now)
                .map(|m| (id.clone(), json!(m.label)))
        })
        .collect();
    json!({
        "at": now,
        "links": links,
        "marks": marks,
        "shown": shown,
        "animating": anims.animating(now),
        "running": anims.running(),
        "next_expiry": anims.next_expiry(now),
    })
}

fn run(case: &Case) -> Value {
    let mut anims = Anims::default();
    // Steps before checks at the same time; steps in their order.
    let mut times: Vec<(i64, Option<&Input>)> = case
        .steps
        .iter()
        .map(|(at, input)| (*at, Some(input)))
        .collect();
    times.extend(case.checks.iter().map(|at| (*at, None)));
    times.sort_by_key(|(at, input)| (*at, input.is_none()));
    let mut checks = Vec::new();
    for (at, input) in times {
        match input {
            Some(Input::Message(m)) => anims.on_message(m, &case.workers, at),
            Some(Input::Vcs(e)) => anims.on_vcs(e, at),
            Some(Input::Snapshot(events)) => {
                anims.resume(events.iter().map(|(e, age)| (e, *age)), at)
            }
            Some(Input::Remove(id)) => anims.remove_agent(id),
            None => checks.push(observe(&mut anims, &case.workers, at)),
        }
    }
    let steps: Vec<Value> = case
        .steps
        .iter()
        .map(|(at, input)| match input {
            Input::Message(m) => json!({"at": at, "message": value(m)}),
            Input::Vcs(e) => json!({"at": at, "vcs": value(e)}),
            Input::Snapshot(events) => json!({"at": at, "snapshot": events
                .iter()
                .map(|(e, age)| json!({"vcs": value(e), "age": age}))
                .collect::<Vec<_>>()}),
            Input::Remove(id) => json!({"at": at, "remove": id}),
        })
        .collect();
    json!({"name": case.name, "workers": case.workers, "steps": steps, "checks": checks})
}

fn msg(n: usize, from: &str, to: &str) -> Input {
    let kind = if to == OPERATOR_ID {
        MessageType::Question
    } else {
        MessageType::Message
    };
    Input::Message(fixture::message(n, from, to, kind, "x", 0))
}

fn ev(action: VcsAction, outcome: VcsOutcome, op: &str, agent: &str) -> VcsEvent {
    vcs(action, outcome, op, agent)
}

fn with(mut e: VcsEvent, f: impl FnOnce(&mut VcsEvent)) -> VcsEvent {
    f(&mut e);
    e
}

fn cases() -> Vec<Case> {
    use VcsAction::*;
    use VcsOutcome::*;
    let workers = |n: usize| -> Vec<String> { (1..=n).map(|i| format!("claude-{i}")).collect() };
    let pair = || vec!["db".to_string(), "ledger".to_string()];
    let peer = |e: VcsEvent, p: &str| with(e, |e| e.peer = Some(p.into()));
    let commit = |e: VcsEvent, c: &str| with(e, |e| e.commit = Some(full(c)));
    let files =
        |e: VcsEvent, f: &[&str]| with(e, |e| e.files = f.iter().map(|s| s.to_string()).collect());
    let merge_ok = commit(peer(ev(Merge, Ok, "v1", "ledger"), "db"), "5e6f7a8b");
    vec![
        Case {
            name: "message: peer to peer, pulses and expires",
            workers: workers(3),
            steps: vec![(0, msg(1, "claude-1", "claude-2"))],
            checks: vec![0, 450, 899, 900, 2999, 3000],
        },
        Case {
            name: "message: orchestrator and operator traffic uses the hub",
            workers: workers(3),
            steps: vec![
                (0, msg(1, ORCHESTRATOR_ID, "claude-1")),
                (100, msg(2, "claude-2", ORCHESTRATOR_ID)),
                (200, msg(3, OPERATOR_ID, "claude-3")),
                (300, msg(4, "claude-3", OPERATOR_ID)),
            ],
            checks: vec![300, 1200],
        },
        Case {
            name: "message: a broadcast fans out to every other worker",
            workers: workers(3),
            steps: vec![
                (0, msg(1, "claude-1", BROADCAST)),
                (100, msg(2, ORCHESTRATOR_ID, BROADCAST)),
            ],
            checks: vec![100, 500],
        },
        Case {
            name: "message: a repeat restarts the same link",
            workers: workers(3),
            steps: vec![
                (0, msg(1, "claude-1", "claude-2")),
                (2000, msg(2, "claude-1", "claude-2")),
            ],
            checks: vec![1999, 2000, 2450, 4999, 5000],
        },
        Case {
            name: "commit: marks its agent with the short id",
            workers: pair(),
            steps: vec![(
                0,
                Input::Vcs(commit(ev(Commit, Ok, "v1", "db"), "1a2b3c4d")),
            )],
            checks: vec![0, 350, 700, 2399, 2400],
        },
        Case {
            name: "cherry-pick: pulses from the peer and marks on arrival",
            workers: pair(),
            steps: vec![(
                0,
                Input::Vcs(commit(
                    peer(ev(CherryPick, Ok, "v1", "ledger"), "db"),
                    "7e8f9a0b",
                )),
            )],
            checks: vec![0, 450, 900, 1250, 3299, 3300],
        },
        Case {
            name: "merge: started, then ok",
            workers: pair(),
            steps: vec![
                (
                    0,
                    Input::Vcs(peer(ev(Merge, Started, "v1", "ledger"), "db")),
                ),
                (4000, Input::Vcs(merge_ok.clone())),
            ],
            checks: vec![100, 3999, 4000, 4450, 4900, 7299, 7300],
        },
        Case {
            name: "merge: a terminal event alone renders like one after a start",
            workers: pair(),
            steps: vec![(0, Input::Vcs(merge_ok))],
            checks: vec![0, 450, 900, 3299],
        },
        Case {
            name: "conflict: a distinct tone with its file count",
            workers: pair(),
            steps: vec![
                (
                    0,
                    Input::Vcs(files(
                        peer(ev(Merge, Conflict, "v1", "ledger"), "db"),
                        &["schema.sql"],
                    )),
                ),
                (
                    5000,
                    Input::Vcs(files(
                        peer(ev(CherryPick, Conflict, "v2", "db"), "ledger"),
                        &["a.rs", "b.rs"],
                    )),
                ),
            ],
            checks: vec![450, 900, 5900],
        },
        Case {
            name: "workspace: links parent to member and marks the member",
            workers: vec!["db".into(), "db-s1".into()],
            steps: vec![
                (
                    0,
                    Input::Vcs(peer(ev(Workspace, Started, "v1", "db-s1"), "db")),
                ),
                (
                    1500,
                    Input::Vcs(with(
                        commit(peer(ev(Workspace, Ok, "v1", "db-s1"), "db"), "1a2b3c4d"),
                        |e| e.branch = Some("agents/db-s1".into()),
                    )),
                ),
            ],
            checks: vec![100, 1500, 2400, 2500],
        },
        Case {
            name: "workspace: a member without a parent runs on the hub",
            workers: pair(),
            steps: vec![
                (0, Input::Vcs(ev(Workspace, Started, "v1", "ledger"))),
                (
                    1000,
                    Input::Vcs(with(ev(Workspace, Error, "v1", "ledger"), |e| {
                        e.summary = Some("/w/.crew/worktrees/ledger already exists".into())
                    })),
                ),
            ],
            checks: vec![100, 1000, 1200],
        },
        Case {
            name: "integrate: branches run to the hub and the tally marks it",
            workers: pair(),
            steps: vec![
                (
                    0,
                    Input::Vcs(ev(Integrate, Started, "v1", INTEGRATION_AGENT)),
                ),
                (
                    1000,
                    Input::Vcs(commit(
                        peer(ev(Integrate, Ok, "v2", INTEGRATION_AGENT), "db"),
                        "9c0d1e2f",
                    )),
                ),
                (
                    2000,
                    Input::Vcs(files(
                        peer(ev(Integrate, Conflict, "v3", INTEGRATION_AGENT), "ledger"),
                        &["schema.sql"],
                    )),
                ),
                (
                    3000,
                    Input::Vcs(with(
                        ev(Integrate, Conflict, "v1", INTEGRATION_AGENT),
                        |e| {
                            e.summary = Some("1 merged, 1 conflict".into());
                            e.branch = Some("agents/integration".into());
                        },
                    )),
                ),
            ],
            checks: vec![500, 1450, 2450, 3000, 3350, 5399, 5400],
        },
        Case {
            name: "reads: glance and never hide a write",
            workers: pair(),
            steps: vec![
                (0, Input::Vcs(ev(Status, Ok, "v1", "db"))),
                (
                    100,
                    Input::Vcs(with(ev(Diff, Ok, "v2", "db"), |e| {
                        e.source = Some("agents/ledger".into())
                    })),
                ),
                (
                    2000,
                    Input::Vcs(commit(ev(Commit, Ok, "v3", "db"), "1a2b3c4d")),
                ),
                (2100, Input::Vcs(ev(Status, Ok, "v4", "db"))),
                (5000, Input::Vcs(ev(Diff, Ok, "v5", "db"))),
            ],
            checks: vec![150, 999, 1000, 2100, 2200, 5000, 5150, 5900],
        },
        Case {
            name: "errors: reads glance in warn, other errors mark done",
            workers: pair(),
            steps: vec![
                (
                    0,
                    Input::Vcs(with(ev(Status, Error, "v1", "db"), |e| {
                        e.summary = Some("stale".into())
                    })),
                ),
                (
                    0,
                    Input::Vcs(with(ev(Commit, Error, "v2", "ledger"), |e| {
                        e.summary =
                            Some("nothing to commit: the working tree has no changes".into())
                    })),
                ),
                (
                    100,
                    Input::Vcs(with(ev(Integrate, Error, "v3", INTEGRATION_AGENT), |e| {
                        e.summary = Some("interrupted".into())
                    })),
                ),
            ],
            checks: vec![100, 400, 1000],
        },
        Case {
            name: "started: a mark for each action that has one",
            workers: pair(),
            steps: vec![
                (
                    0,
                    Input::Vcs(with(ev(CherryPick, Started, "v1", "db"), |e| {
                        e.source = Some("5a6b7c8d".into())
                    })),
                ),
                (
                    0,
                    Input::Vcs(peer(ev(Merge, Started, "v2", "ledger"), "db")),
                ),
                (
                    0,
                    Input::Vcs(ev(Integrate, Started, "v3", INTEGRATION_AGENT)),
                ),
                (
                    0,
                    Input::Vcs(peer(ev(Workspace, Started, "v4", "db-s1"), "db")),
                ),
            ],
            checks: vec![0, 60_000],
        },
        Case {
            name: "hub: integration and operator map to it, self links are skipped",
            workers: pair(),
            steps: vec![
                (
                    0,
                    Input::Vcs(commit(peer(ev(Merge, Ok, "v1", "db"), "db"), "1a2b3c4d")),
                ),
                (
                    0,
                    Input::Vcs(commit(
                        peer(ev(Integrate, Ok, "v2", INTEGRATION_AGENT), "ledger"),
                        "2b3c4d5e",
                    )),
                ),
            ],
            checks: vec![0, 450],
        },
        Case {
            name: "links: a vcs link replaces a live link between the same pair",
            workers: pair(),
            steps: vec![
                (0, msg(1, "db", "ledger")),
                (
                    500,
                    Input::Vcs(commit(
                        peer(ev(CherryPick, Ok, "v1", "ledger"), "db"),
                        "7e8f9a0b",
                    )),
                ),
            ],
            checks: vec![400, 500, 950],
        },
        Case {
            name: "running: without an end it is dropped at the cap",
            workers: pair(),
            steps: vec![(
                0,
                Input::Vcs(peer(ev(Merge, Started, "v1", "ledger"), "db")),
            )],
            checks: vec![599_999, 600_000],
        },
        Case {
            name: "resume: only what still runs is restored",
            workers: pair(),
            steps: vec![
                (
                    0,
                    Input::Vcs(commit(ev(Commit, Ok, "v0", "db"), "1a2b3c4d")),
                ),
                (
                    100,
                    Input::Snapshot(vec![
                        (peer(ev(Merge, Started, "v1", "ledger"), "db"), 5000),
                        (ev(Integrate, Started, "v2", INTEGRATION_AGENT), 4000),
                        (
                            with(ev(Integrate, Ok, "v2", INTEGRATION_AGENT), |e| {
                                e.summary = Some("1 merged, 0 conflicts".into())
                            }),
                            3000,
                        ),
                        (
                            with(ev(CherryPick, Started, "v3", "db"), |e| {
                                e.source = Some("abcdef1".into())
                            }),
                            700_000,
                        ),
                        (ev(Status, Ok, "v4", "db"), 1000),
                    ]),
                ),
            ],
            checks: vec![100, 594_999, 595_100],
        },
        Case {
            name: "remove: an agent that leaves takes its marks and links",
            workers: pair(),
            steps: vec![
                (
                    0,
                    Input::Vcs(peer(ev(Merge, Started, "v1", "ledger"), "db")),
                ),
                (
                    0,
                    Input::Vcs(commit(ev(Commit, Ok, "v2", "db"), "1a2b3c4d")),
                ),
                (0, msg(1, "db", ORCHESTRATOR_ID)),
                (100, Input::Remove("ledger".into())),
            ],
            checks: vec![50, 100],
        },
    ]
}

#[test]
fn anim_json() {
    let cases: Vec<Value> = cases().iter().map(run).collect();
    check(
        "anim.json",
        pretty(&json!({
            "constants": {
                "PULSE_MS": crew_tui::anim::PULSE_MS,
                "LINK_MS": crew_tui::anim::LINK_MS,
                "MARK_SWEEP_MS": crew_tui::anim::MARK_SWEEP_MS,
                "MARK_MS": crew_tui::anim::MARK_MS,
                "GLANCE_SWEEP_MS": crew_tui::anim::GLANCE_SWEEP_MS,
                "GLANCE_MS": crew_tui::anim::GLANCE_MS,
                "RUNNING_MAX_MS": crew_tui::anim::RUNNING_MAX_MS,
            },
            "cases": cases,
        })),
    );
}

#[test]
fn every_fixture_file_is_generated() {
    let expected = [
        "anim.json",
        "commands.json",
        "enums.json",
        "errors.json",
        "multi.json",
        "pushes.jsonl",
        "results.json",
        "snapshot.json",
        "subagents.json",
        "team10.json",
        "toolbox.json",
    ];
    if std::env::var("CREW_UPDATE_FIXTURES").as_deref() == Ok("1") {
        return;
    }
    let mut found: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != "README.md")
        .collect();
    found.sort();
    assert_eq!(found, expected);
}
