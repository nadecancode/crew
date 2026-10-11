//! A hand-written six-agent session used by the rendering tests and the
//! `preview` example. Nothing here talks to a daemon.

use crew_core::rpc::{ModelChoice, SessionInfo, Snapshot};
use crew_core::types::{
    Agent, AgentStatus, Artifact, Event, EventLevel, Message, MessageType, RuntimeKind, Task,
    TaskStatus, Usage, ORCHESTRATOR_ID,
};
use crew_core::vcs::{VcsAction, VcsEvent, VcsOutcome, VCS_SOURCE};
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
        toolbox: None,
    }
}

pub fn task(id: &str, agent: &str, status: TaskStatus, deps: &[&str], now: i64) -> Task {
    Task {
        epic: None,
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
        vcs_event(
            4,
            t0 + 500,
            "claude-3: cherry-pick of a91ec21 conflicts with claude-5 in src/ledger.rs; nothing was changed",
            VcsEvent {
                peer: Some("claude-5".into()),
                source: Some("a91ec21f0e4d3b2c1a09f8e7d6c5b4a392817060".into()),
                branch: Some("agents/claude-3".into()),
                files: vec!["src/ledger.rs".into()],
                ..vcs(VcsAction::CherryPick, VcsOutcome::Conflict, "v4", "claude-3")
            },
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
        epics: None,
        questions: Vec::new(),
        session: SessionInfo {
            workspace_run_id: None,
            project: Some("checkout-rewrite".to_string()),
            objective: "Rewrite the checkout service while preserving API compatibility."
                .to_string(),
            style: None,
            workflow: None,
            quotas: Vec::new(),
            workspace: "/work/demo".to_string(),
            state_dir: "/work/demo/.crew".to_string(),
            backend: "herdr".to_string(),
            isolation: "worktree".to_string(),
            sandbox: "off".to_string(),
            vcs: "jj".to_string(),
            vcs_note: String::new(),
            web: String::new(),
            socket_warning: String::new(),
            binary_warning: String::new(),
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

// ---- version control ---------------------------------------------------------

/// A VCS event as the daemon builds it: just what every event has.
pub fn vcs(action: VcsAction, outcome: VcsOutcome, op: &str, agent: &str) -> VcsEvent {
    VcsEvent {
        action,
        outcome,
        op: op.to_string(),
        backend: "jj".to_string(),
        agent: agent.to_string(),
        peer: None,
        commit: None,
        change: None,
        source: None,
        branch: None,
        files: Vec::new(),
        summary: None,
    }
}

/// The envelope the daemon puts around a VCS event: `vcs.<action>`, the log
/// level of its outcome, the payload in `data`.
pub fn vcs_event(seq: i64, ts: i64, message: &str, vcs: VcsEvent) -> Event {
    let level = match vcs.outcome {
        VcsOutcome::Started => EventLevel::Debug,
        VcsOutcome::Ok if vcs.action.is_read() => EventLevel::Debug,
        VcsOutcome::Ok => EventLevel::Info,
        VcsOutcome::Conflict | VcsOutcome::Error => EventLevel::Warn,
    };
    Event {
        seq,
        timestamp: ts,
        source: VCS_SOURCE.to_string(),
        event_type: vcs.event_type(),
        agent_id: (vcs.agent != "integration").then(|| vcs.agent.clone()),
        task_id: None,
        message: message.to_string(),
        level,
        data: serde_json::to_value(&vcs).unwrap_or(Value::Null),
    }
}

/// Every VCS animation in turn, as a script: `(milliseconds after the start,
/// message, event)`. Agents are those of [`snapshot`]. Played one event at a
/// time it shows each piece: a workspace being made, a commit, a status and a
/// diff, a cherry-pick, a merge that runs for a while, a conflict, and an
/// integration with its branches.
pub fn vcs_script() -> Vec<(i64, String, VcsEvent)> {
    let full = |c: &str| format!("{c}0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c");
    let mut script: Vec<(i64, String, VcsEvent)> = Vec::new();
    let mut add = |at: i64, text: &str, event: VcsEvent| script.push((at, text.to_string(), event));
    let v = |action, outcome, op: &str, agent: &str| vcs(action, outcome, op, agent);

    add(
        0,
        "creating the workspace of claude-3-s1…",
        VcsEvent {
            peer: Some("claude-3".into()),
            ..v(
                VcsAction::Workspace,
                VcsOutcome::Started,
                "v1",
                "claude-3-s1",
            )
        },
    );
    add(
        1500,
        "workspace for claude-3-s1 on agents/claude-3-s1 at 1a2b3c4d",
        VcsEvent {
            peer: Some("claude-3".into()),
            commit: Some(full("1a2b3c4d")),
            branch: Some("agents/claude-3-s1".into()),
            ..v(VcsAction::Workspace, VcsOutcome::Ok, "v1", "claude-3-s1")
        },
    );
    add(
        4500,
        "claude-1 committed 3f4e5d6c on agents/claude-1: Add billing totals",
        VcsEvent {
            commit: Some(full("3f4e5d6c")),
            change: Some("kzvsvsvnkzvsvsvn".into()),
            branch: Some("agents/claude-1".into()),
            summary: Some("Add billing totals".into()),
            ..v(VcsAction::Commit, VcsOutcome::Ok, "v2", "claude-1")
        },
    );
    add(
        7000,
        "claude-2: status",
        v(VcsAction::Status, VcsOutcome::Ok, "v3", "claude-2"),
    );
    add(
        7600,
        "claude-2: diff against agents/claude-4",
        VcsEvent {
            source: Some("agents/claude-4".into()),
            ..v(VcsAction::Diff, VcsOutcome::Ok, "v4", "claude-2")
        },
    );
    add(
        9000,
        "claude-4: cherry-pick of 5a6b7c8d…",
        VcsEvent {
            source: Some("5a6b7c8d".into()),
            ..v(VcsAction::CherryPick, VcsOutcome::Started, "v5", "claude-4")
        },
    );
    add(
        10_000,
        "claude-4 cherry-picked 5a6b7c8d from claude-5 as 7e8f9a0b",
        VcsEvent {
            peer: Some("claude-5".into()),
            commit: Some(full("7e8f9a0b")),
            source: Some(full("5a6b7c8d")),
            ..v(VcsAction::CherryPick, VcsOutcome::Ok, "v5", "claude-4")
        },
    );
    add(
        13_000,
        "claude-3: merging agents/claude-5…",
        VcsEvent {
            peer: Some("claude-5".into()),
            source: Some("agents/claude-5".into()),
            ..v(VcsAction::Merge, VcsOutcome::Started, "v6", "claude-3")
        },
    );
    add(
        17_000,
        "claude-3: merge of agents/claude-5 conflicts in src/ledger.rs, db/schema.sql; nothing was changed",
        VcsEvent {
            peer: Some("claude-5".into()),
            source: Some("agents/claude-5".into()),
            files: vec!["src/ledger.rs".into(), "db/schema.sql".into()],
            ..v(VcsAction::Merge, VcsOutcome::Conflict, "v6", "claude-3")
        },
    );
    add(
        20_000,
        "claude-6: status failed: the workspace is stale",
        VcsEvent {
            summary: Some("the workspace is stale".into()),
            ..v(VcsAction::Status, VcsOutcome::Error, "v7", "claude-6")
        },
    );
    add(
        22_000,
        "integrating into agents/integration…",
        v(
            VcsAction::Integrate,
            VcsOutcome::Started,
            "v8",
            "integration",
        ),
    );
    add(
        23_500,
        "integration merged agents/claude-5 (9c0d1e2f)",
        VcsEvent {
            peer: Some("claude-5".into()),
            commit: Some(full("9c0d1e2f")),
            ..v(VcsAction::Integrate, VcsOutcome::Ok, "v9", "integration")
        },
    );
    add(
        25_000,
        "integration: merge of agents/claude-2 conflicts in src/queue.rs; skipped",
        VcsEvent {
            peer: Some("claude-2".into()),
            files: vec!["src/queue.rs".into()],
            ..v(
                VcsAction::Integrate,
                VcsOutcome::Conflict,
                "v10",
                "integration",
            )
        },
    );
    add(
        26_500,
        "integration on agents/integration: 1 merged, 1 conflict",
        VcsEvent {
            branch: Some("agents/integration".into()),
            summary: Some("1 merged, 1 conflict".into()),
            ..v(
                VcsAction::Integrate,
                VcsOutcome::Conflict,
                "v8",
                "integration",
            )
        },
    );
    script
}

/// The two members of the toolbox example team (`notes/toolboxes.md`
/// §Team file, case T02) with their effective toolboxes as a dashboard
/// receives them: `qa` on Claude Code, and `release` on Codex with a change
/// waiting for a restart. No value of a reference is here, only its name.
pub fn toolbox_agents(now: i64) -> Vec<Agent> {
    use crew_core::types::{
        AgentToolbox, ToolboxItem, ToolboxItemCode as Code, ToolboxItemKind as Kind,
        ToolboxItemStatus, ToolboxSource, ToolboxSourceCode as SourceCode,
        ToolboxSourceOrigin as Origin, ToolboxSourceStatus,
    };
    let item =
        |kind, name: &str, origin: &str, required, code, text: &str, detail: Option<&str>| {
            ToolboxItem {
                kind,
                name: name.to_string(),
                origin: origin.to_string(),
                required,
                status: ToolboxItemStatus {
                    code,
                    text: text.to_string(),
                },
                detail: detail.map(str::to_string),
            }
        };
    let source = |origin,
                  kind: Option<&str>,
                  label: &str,
                  code,
                  text: &str,
                  names: &[&str],
                  detail: Option<&str>| {
        ToolboxSource {
            origin,
            kind: kind.map(str::to_string),
            label: label.to_string(),
            status: ToolboxSourceStatus {
                code,
                text: text.to_string(),
            },
            names: names.iter().map(|n| n.to_string()).collect(),
            detail: detail.map(str::to_string),
        }
    };
    let mut qa = agent("qa", "frontend QA", AgentStatus::Working, Some(40), now);
    qa.runtime = RuntimeKind::Claude;
    qa.toolbox = Some(AgentToolbox {
        fingerprint: "3f9a0c21e7b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4".into(),
        items: vec![
            item(
                Kind::Mcp,
                "context7",
                "bundle:base",
                false,
                Code::Active,
                "Active",
                Some("npx -y @upstash/context7-mcp"),
            ),
            item(
                Kind::Mcp,
                "playwright",
                "bundle:web",
                true,
                Code::Active,
                "Active",
                Some("npx @playwright/mcp --headless"),
            ),
            item(
                Kind::Skills,
                "frontend-qa",
                "bundle:web",
                false,
                Code::Active,
                "Active",
                None,
            ),
        ],
        sources: vec![
            source(
                Origin::Operator,
                None,
                "Your Claude Code configuration",
                SourceCode::NotUsed,
                "Not used",
                &[],
                None,
            ),
            source(
                Origin::Project,
                None,
                "Project",
                SourceCode::Used,
                "Used: skills, hooks, instructions",
                &[],
                None,
            ),
        ],
        ..AgentToolbox::default()
    });
    let mut release = agent("release", "release notes", AgentStatus::Idle, None, now);
    release.runtime = RuntimeKind::Codex;
    release.model = Some("gpt-6-astra".to_string());
    release.toolbox = Some(AgentToolbox {
        fingerprint: "b56e01d7c4a98f3e2d1c0b9a8f7e6d5c4b3a29180f7e6d5c4b3a2918f0e1d2c3".into(),
        pending_fingerprint: Some(
            "c0ffee12ab34cd56ef7890ab12cd34ef567890ab12cd34ef567890ab12cd34ef".into(),
        ),
        items: vec![
            item(
                Kind::Mcp,
                "docs",
                "bundle:github",
                false,
                Code::Active,
                "Active",
                Some("https://docs.example.com/mcp · Authorization: Bearer $DOCS_TOKEN"),
            ),
            item(
                Kind::Mcp,
                "github",
                "bundle:github",
                false,
                Code::Needs,
                "Needs $SHOP_GITHUB_TOKEN",
                Some("github-mcp-server stdio · GITHUB_TOKEN: $SHOP_GITHUB_TOKEN (not set) · GITHUB_TOOLSETS: repos,issues,pull_requests"),
            ),
            item(Kind::Skills, "changelog", "member", false, Code::Active, "Active", None),
            item(
                Kind::Plugins,
                "lint-kit",
                "member",
                false,
                Code::NotAvailable,
                "Not available on Codex",
                Some("Claude Code plugin format"),
            ),
        ],
        sources: vec![
            source(
                Origin::Operator,
                Some("mcp"),
                "Your Codex MCP servers",
                SourceCode::Disabled,
                "Disabled",
                &["ambient", "docs-local"],
                None,
            ),
            source(
                Origin::Operator,
                Some("subagents"),
                "Your subagents",
                SourceCode::CannotBeExcluded,
                "Cannot be excluded on Codex",
                &[],
                Some("without the sandbox"),
            ),
        ],
        ..AgentToolbox::default()
    });
    vec![qa, release]
}
