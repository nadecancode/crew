//! A hand-written six-agent session used by the rendering tests and the
//! `preview` example. Nothing here talks to a daemon.

use crew_core::rpc::{ModelChoice, SessionInfo, Snapshot};
use crew_core::types::{
    Agent, AgentStatus, Artifact, Event, EventLevel, Message, MessageType, RuntimeKind, Task,
    TaskStatus, Usage, ORCHESTRATOR_ID,
};
use serde_json::Value;

pub fn agent(id: &str, role: &str, status: AgentStatus, progress: Option<u8>, now: i64) -> Agent {
    Agent {
        id: id.to_string(),
        name: id.to_string(),
        role: role.to_string(),
        task: Some(format!("implement {role}")),
        status,
        activity: match status {
            AgentStatus::Working => "Edit src/lib.rs".to_string(),
            _ => String::new(),
        },
        runtime: if id == "claude-2" {
            RuntimeKind::Codex
        } else {
            RuntimeKind::Claude
        },
        model: Some("sonnet".to_string()),
        workdir: format!("/work/demo/.crew/worktrees/{id}"),
        branch: Some(format!("agents/{id}")),
        backend: "herdr".to_string(),
        backend_ref: Some("w2:p4".to_string()),
        pid: Some(4242),
        session_id: Some("0d73aa0a-bdaf-4916-8421-d1917fc3bf2a".to_string()),
        orchestrator: id == ORCHESTRATOR_ID,
        parent: None,
        subagents: 0,
        temporary: false,
        delivery: "socket".to_string(),
        started_at: Some(now - 754_000),
        last_activity_at: now - 2_000,
        progress,
        paused: false,
        block_kind: None,
        limit: None,
        context: None,
        compact_pending: false,
        compactions: 0,
        running_model: Some("claude-sonnet-5-5".to_string()),
        model_name: Some("Sonnet 5.5".to_string()),
        effort: Some("high".to_string()),
        fallback: None,
        exit_code: None,
        restarts: 0,
        inbox_unread: 0,
        inbox_total: 3,
        outbox_total: 2,
        usage: Some(Usage {
            input_tokens: 18_230,
            output_tokens: 4_112,
            cache_read_tokens: 220_448,
            cache_creation_tokens: 31_004,
            turns: 9,
        }),
        error: None,
    }
}

pub fn task(id: &str, agent: &str, status: TaskStatus, deps: &[&str], now: i64) -> Task {
    Task {
        id: id.to_string(),
        title: format!("implement {id}"),
        description: format!("Build the {id} module and commit it."),
        assigned_agent: Some(agent.to_string()),
        status,
        dependencies: deps.iter().map(|d| d.to_string()).collect(),
        created_at: now - 760_000,
        started_at: Some(now - 700_000),
        completed_at: (status == TaskStatus::Done).then_some(now - 60_000),
        artifacts: if status == TaskStatus::Done {
            vec![Artifact {
                kind: "commit".to_string(),
                value: "a91ec21".to_string(),
                description: format!("{id} implementation"),
                agent: Some(agent.to_string()),
                created_at: now - 60_000,
            }]
        } else {
            Vec::new()
        },
        reviewer: None,
        result: (status == TaskStatus::Done).then(|| format!("{id} done")),
        progress: match status {
            TaskStatus::Done => Some(100),
            TaskStatus::Working => Some(51),
            _ => None,
        },
        note: None,
        timeout_secs: None,
    }
}

pub fn message(n: usize, from: &str, to: &str, kind: MessageType, body: &str, ts: i64) -> Message {
    Message {
        id: format!("m-{n:04}"),
        from: from.to_string(),
        to: to.to_string(),
        kind,
        body: body.to_string(),
        timestamp: ts,
        correlation_id: None,
        reply_to: None,
        metadata: Value::Null,
    }
}

pub fn event(
    seq: i64,
    level: EventLevel,
    source: &str,
    kind: &str,
    agent: Option<&str>,
    text: &str,
    ts: i64,
) -> Event {
    Event {
        seq,
        timestamp: ts,
        source: source.to_string(),
        event_type: kind.to_string(),
        agent_id: agent.map(str::to_string),
        task_id: None,
        message: text.to_string(),
        level,
        data: Value::Null,
    }
}

/// A question the orchestrator put to the operator, passing on one of
/// claude-2's, with answers to pick from.
pub fn question(now: i64) -> Message {
    let mut question = message(
        9001,
        "orchestrator",
        crew_core::types::OPERATOR_ID,
        MessageType::Question,
        "The legacy export writes amounts as integer cents; the new ledger API wants decimal \
strings. Converting in the adapter keeps every downstream report working but hides the \
mismatch; changing the export is cleaner and breaks the two finance dashboards until they \
are updated. Which do you want?",
        now - 185_000,
    );
    question.metadata = serde_json::json!({
        "options": ["Convert in the adapter", "Change the export and update the dashboards"],
        "asked_by": "claude-2",
    });
    question
}

/// What the daemon lists for the model picker: a few models of each CLI,
/// one that this machine cannot start.
pub fn models() -> Vec<ModelChoice> {
    let model =
        |runtime, model: &str, name: &str, tier: &str, price: &str, unavailable: Option<&str>| {
            ModelChoice {
                runtime,
                model: model.to_string(),
                name: name.to_string(),
                tier: tier.to_string(),
                price: price.to_string(),
                summary: format!("{name}, as its vendor describes it."),
                unavailable: unavailable.map(str::to_string),
            }
        };
    vec![
        model(
            RuntimeKind::Claude,
            "fable",
            "Fable 5.1",
            "frontier",
            "$10 / $50 per MTok",
            None,
        ),
        model(
            RuntimeKind::Claude,
            "opus",
            "Opus 5.5",
            "strong",
            "$4 / $20 per MTok",
            None,
        ),
        model(
            RuntimeKind::Claude,
            "sonnet",
            "Sonnet 5.5",
            "balanced",
            "$2 / $10 per MTok",
            None,
        ),
        model(
            RuntimeKind::Codex,
            "gpt-6-astra",
            "GPT 6 Astra",
            "frontier",
            "$5 / $40 per MTok",
            None,
        ),
        model(
            RuntimeKind::Codex,
            "gpt-6-luna",
            "GPT 6 Luna",
            "fast",
            "",
            Some("not in the model list of this Codex account"),
        ),
        model(RuntimeKind::Pi, "", "pi's default", "", "", None),
        model(
            RuntimeKind::Pi,
            "anthropic/claude-opus-4-6",
            "anthropic/claude-opus-4-6",
            "",
            "",
            None,
        ),
    ]
}

/// The session from the reference screenshot: six agents around an
/// orchestrator, one still working, with a mix of orchestrator and
/// peer-to-peer traffic on the bus.
pub fn snapshot(now: i64) -> Snapshot {
    let mut agents = vec![
        agent("claude-1", "billing", AgentStatus::Working, Some(51), now),
        agent("claude-2", "queue", AgentStatus::Done, Some(100), now),
        agent("claude-3", "ledger", AgentStatus::Done, Some(100), now),
        agent("claude-4", "notify", AgentStatus::Done, Some(100), now),
        agent("claude-5", "db", AgentStatus::Done, Some(100), now),
        agent("claude-6", "search", AgentStatus::Done, Some(100), now),
        agent(
            ORCHESTRATOR_ID,
            "orchestrator",
            AgentStatus::Waiting,
            None,
            now,
        ),
    ];
    agents[6].task = None;
    agents[6].branch = None;

    let mut tasks = vec![
        task("db", "claude-5", TaskStatus::Done, &[], now),
        task("queue", "claude-2", TaskStatus::Done, &[], now),
        task("ledger", "claude-3", TaskStatus::Done, &["db"], now),
        task("search", "claude-6", TaskStatus::Done, &["db"], now),
        task("notify", "claude-4", TaskStatus::Done, &["queue"], now),
        task(
            "billing",
            "claude-1",
            TaskStatus::Working,
            &["ledger", "queue"],
            now,
        ),
    ];
    tasks[2].reviewer = Some("claude-2".to_string());

    let t0 = now - 20_000;
    let messages = vec![
        message(
            1,
            "claude-3",
            "claude-5",
            MessageType::Question,
            "your trait bound broke Transaction::commit(), can you relax it?",
            t0 + 6,
        ),
        message(
            2,
            ORCHESTRATOR_ID,
            "claude-4",
            MessageType::Task,
            "pin tokio 1.41 across the workspace",
            t0 + 177,
        ),
        message(
            3,
            ORCHESTRATOR_ID,
            "claude-3",
            MessageType::Task,
            "pin tokio 1.38 across crates until the audit lands",
            t0 + 209,
        ),
        message(
            4,
            ORCHESTRATOR_ID,
            "claude-5",
            MessageType::ReviewRequest,
            "review claude-2's PR #5",
            t0 + 247,
        ),
        message(
            5,
            ORCHESTRATOR_ID,
            "*",
            MessageType::Status,
            "bump edition 2021 → 2024",
            t0 + 301,
        ),
        message(
            6,
            "claude-3",
            "claude-1",
            MessageType::Message,
            "your macro shadows mine, renamed to ledger_entry!",
            t0 + 325,
        ),
        message(
            7,
            "claude-5",
            "claude-3",
            MessageType::Artifact,
            "approved · direct channel works. Commit a91ec21",
            t0 + 412,
        ),
    ];

    let events = vec![
        event(
            1,
            EventLevel::Info,
            "daemon",
            "team.loaded",
            None,
            "team checkout-rewrite loaded (6 agents)",
            now - 754_000,
        ),
        event(
            2,
            EventLevel::Info,
            "scheduler",
            "task.dispatched",
            Some("claude-5"),
            "db → claude-5",
            now - 700_000,
        ),
        event(
            3,
            EventLevel::Info,
            "bus",
            "message.sent",
            Some("claude-3"),
            "claude-3 → claude-5 (question)",
            t0 + 6,
        ),
        event(
            4,
            EventLevel::Warn,
            "git",
            "git.conflict",
            Some("claude-3"),
            "cherry-pick a91ec21 conflicted in src/ledger.rs; aborted",
            t0 + 500,
        ),
        event(
            5,
            EventLevel::Error,
            "daemon",
            "agent.crashed",
            Some("claude-1"),
            "process exited with code 1; restarting (1/1)",
            t0 + 900,
        ),
        event(
            6,
            EventLevel::Info,
            "daemon",
            "agent.status",
            Some("claude-1"),
            "claude-1 is working",
            t0 + 1500,
        ),
    ];

    Snapshot {
        questions: Vec::new(),
        session: SessionInfo {
            project: Some("checkout-rewrite".to_string()),
            objective: "Rewrite the checkout service while preserving API compatibility."
                .to_string(),
            style: None,
            quotas: Vec::new(),
            workspace: "/work/demo".to_string(),
            state_dir: "/work/demo/.crew".to_string(),
            backend: "herdr".to_string(),
            isolation: "worktree".to_string(),
            sandbox: "off".to_string(),
            socket_warning: String::new(),
            daemon_started_at: now - 760_000,
            team_started_at: Some(now - 754_000),
            completed_at: None,
            summary: None,
            herdr_workspace: Some("w2".to_string()),
            daemon_pid: 4000,
            version: "0.1.0".to_string(),
        },
        agents,
        tasks,
        messages,
        events,
        server_time: now,
    }
}

/// The reference session with subagents in play: `claude-3` has launched
/// `claude-3-s1` (a real team member with its own process), and `claude-1`
/// has two in-session subagents running inside it.
pub fn with_subagents(now: i64) -> Snapshot {
    let mut snap = snapshot(now);
    snap.agents.push(subagent(
        "claude-3-s1",
        "claude-3",
        "tests",
        AgentStatus::Working,
        Some(40),
        now,
    ));
    for a in &mut snap.agents {
        if a.id == "claude-1" {
            a.subagents = 2;
        }
    }
    snap.tasks.push(task(
        "ledger-tests",
        "claude-3-s1",
        TaskStatus::Working,
        &[],
        now,
    ));
    snap.messages.push(message(
        8,
        "claude-3-s1",
        "claude-3",
        MessageType::Artifact,
        "ledger tests pass. Commit: b7e21aa",
        now - 4_000,
    ));
    snap
}

/// A subagent of `parent`, as the daemon reports one.
pub fn subagent(
    id: &str,
    parent: &str,
    role: &str,
    status: AgentStatus,
    progress: Option<u8>,
    now: i64,
) -> Agent {
    let mut a = agent(id, role, status, progress, now);
    a.parent = Some(parent.to_string());
    a.started_at = Some(now - 90_000);
    a
}

/// `n` workers plus an orchestrator, for layout stress tests.
pub fn team(n: usize, now: i64) -> Snapshot {
    let mut snap = snapshot(now);
    let orchestrator = snap.agents.pop().expect("fixture has an orchestrator");
    snap.agents = (1..=n)
        .map(|i| {
            agent(
                &format!("claude-{i}"),
                &format!("role-{i}"),
                AgentStatus::Working,
                Some((i * 9 % 100) as u8),
                now,
            )
        })
        .collect();
    snap.agents.push(orchestrator);
    snap
}
