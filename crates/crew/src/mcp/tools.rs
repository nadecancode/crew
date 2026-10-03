//! The agent-facing tool set.
//!
//! Each tool is a thin mapping from JSON arguments to one protocol call, plus
//! a compact text rendering of the result. The MCP server exposes these as
//! MCP tools and `crew ctl` exposes the same functions as subcommands, so the
//! two front ends cannot drift apart.

use crew_core::client::{Client, ClientError};
use crew_core::config::TaskSpec;
use crew_core::rpc::{Call, CommitInfo, GitOutcome, InboxResult, SendParams, SendResult, TaskCard};
use crew_core::util::{fmt_clock, one_line, truncate_chars};
use crew_core::{Agent, AgentStatus, Artifact, Message, MessageType, Task, BROADCAST, OPERATOR_ID};
use serde_json::{json, Value};

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
    /// Only offered to the orchestrator.
    pub orchestrator_only: bool,
    pub read_only: bool,
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

const MESSAGE_TYPES: &[&str] = &[
    "message",
    "question",
    "answer",
    "status",
    "artifact",
    "review_request",
    "review_result",
    "error",
];

pub fn definitions() -> Vec<ToolDef> {
    let tool = |name, description, schema, read_only| ToolDef {
        name,
        description,
        schema,
        orchestrator_only: false,
        read_only,
    };
    let orchestrator = |name, description, schema| ToolDef {
        name,
        description,
        schema,
        orchestrator_only: true,
        read_only: false,
    };
    vec![
        tool(
            "agents_list",
            "List every agent on the team with its role, status, progress and current task.",
            object(json!({}), &[]),
            true,
        ),
        tool(
            "agent_send",
            "Send a message directly to one teammate. It is delivered straight into that agent's \
session; the orchestrator is not involved. Use type \"question\" when you need an answer, \
\"answer\" to reply, \"artifact\" to hand over a commit id or file.",
            object(
                json!({
                    "to": { "type": "string", "description": "Recipient agent id, e.g. claude-5; several separated by commas (claude-5,db), each getting its own copy" },
                    "body": { "type": "string", "description": "The message text" },
                    "type": { "type": "string", "enum": MESSAGE_TYPES, "default": "message" },
                    "reply_to": { "type": "string", "description": "Id of the message this answers" },
                }),
                &["to", "body"],
            ),
            false,
        ),
        tool(
            "agent_broadcast",
            "Send one message to every other agent on the team.",
            object(
                json!({
                    "body": { "type": "string" },
                    "type": { "type": "string", "enum": MESSAGE_TYPES, "default": "message" },
                }),
                &["body"],
            ),
            false,
        ),
        tool(
            "agent_compact",
            "Queue a compaction of a context window: at the end of the session's current turn its \
conversation so far is replaced by a summary, which frees the window. Without an id it is your own \
conversation; the orchestrator may name a teammate. Use it at a natural break when a window is \
getting full (agents_list shows \"ctx NN%\"), and say in keep what must survive. Afterwards the \
agent is pointed back to its task and carries on.",
            object(
                json!({
                    "id": { "type": "string", "description": "The teammate to compact (orchestrator only); leave out for yourself" },
                    "keep": { "type": "string", "description": "What the summary must keep: the task, decisions made, files in play, open questions" },
                }),
                &[],
            ),
            false,
        ),
        tool(
            "agent_inbox",
            "Read your messages in full. By default returns unread messages and marks them read; \
with an id, that one message (one you sent or received), e.g. one a card or a teammate points to.",
            object(
                json!({
                    "id": { "type": "string", "description": "A message id (m-…)" },
                    "all": { "type": "boolean", "description": "Include messages already read", "default": false },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 },
                }),
                &[],
            ),
            false,
        ),
        tool(
            "agent_status",
            "Set your own status: \"working\", \"waiting\" (on a teammate), \"blocked\" (cannot \
proceed) or \"idle\". Add a note saying on whom or what.",
            object(
                json!({
                    "status": { "type": "string", "enum": ["working", "waiting", "blocked", "idle"] },
                    "note": { "type": "string" },
                }),
                &["status"],
            ),
            false,
        ),
        tool(
            "tasks_list",
            "Show the board: every task with status, assignee, dependencies and artifacts.",
            object(json!({}), &[]),
            true,
        ),
        tool(
            "task_get",
            "Read a task's card: what is to be done, the results and commits of the tasks it builds \
on, its reviewer, and where it stands. A task handed to you, or one a teammate refers to, is read \
here by its id.",
            object(
                json!({ "id": { "type": "string", "description": "The task id; defaults to your task in progress" } }),
                &[],
            ),
            true,
        ),
        tool(
            "task_update",
            "Report progress on your current task (0-100) with a short note.",
            object(
                json!({
                    "percent": { "type": "integer", "minimum": 0, "maximum": 100 },
                    "message": { "type": "string" },
                }),
                &["percent"],
            ),
            false,
        ),
        tool(
            "task_complete",
            "Mark your current task as done. Call once, after committing your work. Give a short \
result summary; commits made with git_commit are attached automatically.",
            object(
                json!({
                    "result": { "type": "string", "description": "What was done, in a sentence or two" },
                    "artifacts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string", "description": "commit, file, note, ..." },
                                "value": { "type": "string" },
                                "description": { "type": "string" },
                            },
                            "required": ["kind", "value"],
                        },
                    },
                    "task_id": { "type": "string", "description": "Defaults to your current task" },
                }),
                &["result"],
            ),
            false,
        ),
        tool(
            "decline",
            "Say that you will not do what you were just asked, because it goes against your \
guidelines or usage policies. crew hands that work to another model when your turn ends (or \
tells the orchestrator), so stop there. Not for work you cannot do yet, that belongs to a \
teammate, or that needs an answer first: say those with agent_send or agent_status.",
            object(json!({ "reason": { "type": "string", "description": "Why, in a sentence" } }), &["reason"]),
            false,
        ),
        tool(
            "task_fail",
            "Give up on your current task because it cannot be done. Explain why.",
            object(
                json!({
                    "reason": { "type": "string" },
                    "task_id": { "type": "string" },
                }),
                &["reason"],
            ),
            false,
        ),
        tool(
            "review_request",
            "Ask a teammate to review something (a commit id, a file, a design).",
            object(
                json!({
                    "agent_id": { "type": "string" },
                    "artifact": { "type": "string", "description": "What to review, e.g. a commit id" },
                    "note": { "type": "string" },
                    "task_id": { "type": "string" },
                }),
                &["agent_id", "artifact"],
            ),
            false,
        ),
        tool(
            "review_submit",
            "Give your verdict on a task you were asked to review.",
            object(
                json!({
                    "task_id": { "type": "string" },
                    "approve": { "type": "boolean" },
                    "comments": { "type": "string" },
                }),
                &["task_id", "approve"],
            ),
            false,
        ),
        tool(
            "git_commit",
            "Commit all changes in your worktree on your branch. Returns the commit id, which you \
can send to teammates.",
            object(json!({ "message": { "type": "string" } }), &["message"]),
            false,
        ),
        tool(
            "git_diff",
            "Show your uncommitted changes, or the difference against a revision.",
            object(
                json!({ "against": { "type": "string", "description": "Commit id or branch (optional)" } }),
                &[],
            ),
            true,
        ),
        tool(
            "git_status",
            "Show your branch, last commit and working-tree status.",
            object(json!({}), &[]),
            true,
        ),
        tool(
            "git_cherry_pick",
            "Apply a teammate's commit onto your branch. On conflict nothing is changed and the \
conflicting files are reported.",
            object(json!({ "commit": { "type": "string" } }), &["commit"]),
            false,
        ),
        tool(
            "git_merge",
            "Merge a teammate's whole branch into yours. On conflict nothing is changed and the \
conflicting files are reported.",
            object(json!({ "from_agent": { "type": "string" } }), &["from_agent"]),
            false,
        ),
        tool(
            "agent_spawn",
            "Launch a subagent of your own: a real teammate with its own session and worktree \
(branched from yours) that works on one self-contained task and reports back to you directly \
when done. Use it to parallelise your task, not to hand off all of it.",
            object(
                json!({
                    "task": { "type": "string", "description": "Everything the subagent needs to know" },
                    "role": { "type": "string", "description": "Short role label, e.g. \"tests\"" },
                    "id": { "type": "string", "description": "Optional id; generated when omitted" },
                    "runtime": { "type": "string", "enum": ["claude", "codex", "pi"] },
                    "model": { "type": "string" },
                }),
                &["task"],
            ),
            false,
        ),
        orchestrator(
            "agent_add",
            "Add a member to the team: a real agent with its own session, worktree and branch. \
Give it a short id and a role; optionally a first task (otherwise use task_create). The members \
added before your first turn with a plan ends are the founding team, saved for future runs; those \
added after are temporary: not saved, and taken off the team once their tasks are done.",
            object(
                json!({
                    "id": { "type": "string", "description": "Short id: lowercase letters, digits, - and _ (e.g. backend, claude-2)" },
                    "role": { "type": "string", "description": "What this member is responsible for" },
                    "model": { "type": "string", "description": "The model this member runs on, chosen for its role from the Models section of your briefing (e.g. opus, gpt-6-astra). It also decides which CLI runs the member. Default: the team default" },
                    "runtime": { "type": "string", "enum": ["claude", "codex", "pi"], "description": "Only needed for a model that is not in the catalog (pi: always, with the model as pi names it)" },
                    "task": { "type": "string", "description": "Optional first task for the new member" },
                }),
                &["id", "role"],
            ),
        ),
        orchestrator(
            "agent_remove",
            "Take a member off the team. Its process stops and its unfinished tasks return to \
the pool to be reassigned.",
            object(json!({ "id": { "type": "string" } }), &["id"]),
        ),
        orchestrator(
            "agent_set_model",
            "Put a member on another model (one its CLI runs: a Claude model for a Claude Code \
member, a GPT model for a Codex member). Its session is restarted on that model, resumed where \
it was, and the note is the first thing it reads: use it for instructions revised for the new \
model. For a member whose session fell back to another model, this is how to choose the model \
yourself; to move the work to the other model family, agent_add a member instead. Your own \
model, and moving anyone to another CLI, are the operator's to change.",
            object(
                json!({
                    "id": { "type": "string" },
                    "model": { "type": "string", "description": "A model from the Models section of your briefing, e.g. opus, sonnet, gpt-6.1-sol; for a pi member, as pi names it" },
                    "note": { "type": "string", "description": "What the member should know on its new model: revised instructions, what to redo or keep" },
                }),
                &["id", "model"],
            ),
        ),
        orchestrator(
            "usage_status",
            "The accounts' usage windows now, which models are out until their window resets, and \
the closest models to take instead. Check it before choosing a model for a new member.",
            object(json!({}), &[]),
        ),
        orchestrator(
            "task_create",
            "Put a task on the board. The scheduler hands it to its agent, by id, once its \
dependencies are done; the agent reads the card with task_get.",
            object(
                json!({
                    "id": { "type": "string", "description": "Short id: lowercase letters, digits, - and _" },
                    "title": { "type": "string" },
                    "description": { "type": "string", "description": "What is to be done and how to tell it is done; point to docs, tasks, messages and commits by path or id instead of restating them" },
                    "agent": { "type": "string", "description": "Assignee (omit to let the scheduler pick)" },
                    "depends_on": { "type": "array", "items": { "type": "string" } },
                    "reviewer": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1 },
                }),
                &["id", "description"],
            ),
        ),
        orchestrator(
            "task_edit",
            "Revise a task's card: its title or description. Whoever is at work on it (or reviewing \
it) is told to read it again; a failed task can be revised before task_retry.",
            object(
                json!({
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "description": { "type": "string", "description": "The whole new description" },
                }),
                &["id"],
            ),
        ),
        orchestrator(
            "task_assign",
            "Assign or reassign a task to an agent.",
            object(
                json!({ "id": { "type": "string" }, "agent": { "type": "string" } }),
                &["id", "agent"],
            ),
        ),
        orchestrator(
            "task_retry",
            "Put a failed or blocked task back in the queue.",
            object(json!({ "id": { "type": "string" } }), &["id"]),
        ),
        orchestrator(
            "integrate",
            "Merge every agent branch into the integration branch and report conflicts.",
            object(json!({}), &[]),
        ),
        orchestrator(
            "operator_ask",
            "Put a question to the operator, the human who started this team. For what only a \
human can decide: a preference, a missing requirement, a risky or irreversible choice. The \
operator sees exactly this text and nothing else of the conversation, so make it complete: \
what is being decided, why it matters, and what happens with each choice. The call returns at \
once; the answer arrives later as a [crew] message from operator (type answer).",
            object(
                json!({
                    "question": { "type": "string", "description": "The question, with the context needed to decide it" },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "maxItems": 6,
                        "description": "Two to four short answers the operator can pick from; they can always answer in their own words",
                    },
                    "asked_by": { "type": "string", "description": "Id of the teammate whose question you are passing on; they get the answer too" },
                }),
                &["question"],
            ),
        ),
        orchestrator(
            "session_complete",
            "Record the final report once every task is done. Whatever is left for the operator \
to decide goes in `decisions`: each is put to them as a question and the dashboard shows that \
the team is waiting for their answer. A decision that is only mentioned in the summary is not \
asked.",
            object(
                json!({
                    "summary": { "type": "string" },
                    "decisions": {
                        "type": "array",
                        "items": { "type": "string" },
                        "maxItems": 12,
                        "description": "What only the operator can decide now, one decision per entry, each written to stand alone: what is being decided, what you recommend and why. Leave out when nothing needs them.",
                    },
                }),
                &["summary"],
            ),
        ),
    ]
}

/// A tool's result: text for the model, plus the raw value for `--json`.
pub struct ToolOutput {
    pub text: String,
    pub raw: Value,
}

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing required argument '{key}'"))
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn message_type(args: &Value) -> Result<MessageType, String> {
    match opt_str(args, "type") {
        Some(t) => t.parse(),
        None => Ok(MessageType::Message),
    }
}

/// Why a tool call failed: the daemon refused it, or the connection to
/// the daemon failed (the call may or may not have run).
#[derive(Debug)]
pub enum CallError {
    Tool(String),
    Daemon(ClientError),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Tool(message) => f.write_str(message),
            CallError::Daemon(error) => write!(f, "{error}"),
        }
    }
}

impl From<String> for CallError {
    fn from(message: String) -> Self {
        CallError::Tool(message)
    }
}

impl From<&str> for CallError {
    fn from(message: &str) -> Self {
        CallError::Tool(message.to_string())
    }
}

fn rpc<T: serde::de::DeserializeOwned>(
    client: &mut Client,
    call: Call,
) -> Result<(T, Value), CallError> {
    let raw = client.call_value(call).map_err(|e| match e {
        ClientError::Rpc(e) => CallError::Tool(e.message),
        other => CallError::Daemon(other),
    })?;
    let typed =
        serde_json::from_value(raw.clone()).map_err(|e| format!("unexpected response: {e}"))?;
    Ok((typed, raw))
}

fn status_cell(agent: &Agent) -> String {
    let mut s = agent.status.to_string();
    if let Some(p) = agent.progress {
        if agent.status != AgentStatus::Done {
            s.push_str(&format!(" {p}%"));
        }
    }
    if agent.paused {
        s.push_str(" (paused)");
    }
    // How full the context window is, and whether a compaction is queued.
    match &agent.context {
        Some(context) if context.window > 0 => s.push_str(&format!(" · ctx {}%", context.percent)),
        // The window's size is not known: the tokens in it are.
        Some(context) => s.push_str(&format!(" · ctx {}k tokens", context.tokens.div_ceil(1000))),
        None => {}
    }
    if agent.compact_pending {
        s.push_str(" · compaction queued");
    }
    // The session was moved to another model than the member was given.
    if let Some(fallback) = &agent.fallback {
        let running = agent
            .model_label()
            .unwrap_or_else(|| "another model".to_string());
        s.push_str(&format!(" · on {running} instead of {}", fallback.from));
    }
    s
}

pub fn render_agents(agents: &[Agent]) -> String {
    let mut out = String::new();
    for a in agents {
        let role = if a.role.is_empty() {
            "-"
        } else {
            a.role.as_str()
        };
        let temporary = if a.temporary { " (temporary)" } else { "" };
        out.push_str(&format!("{} [{}] {role}{temporary}", a.id, status_cell(a)));
        if let Some(task) = &a.task {
            out.push_str(&format!(" — task: {task}"));
        }
        match &a.error {
            Some(error) if a.status == AgentStatus::Failed => {
                out.push_str(&format!(" — {}", truncate_chars(&one_line(error), 240)));
            }
            _ if !a.activity.is_empty() => {
                out.push_str(&format!(
                    " — {}",
                    truncate_chars(&one_line(&a.activity), 80)
                ));
            }
            _ => {}
        }
        out.push('\n');
    }
    if out.is_empty() {
        out.push_str("(no agents)\n");
    }
    out
}

pub fn render_tasks(tasks: &[Task]) -> String {
    let mut out = String::new();
    for t in tasks {
        out.push_str(&format!(
            "{} [{}] {} — {}",
            t.id,
            t.status,
            t.assigned_agent.as_deref().unwrap_or("unassigned"),
            t.title
        ));
        if !t.dependencies.is_empty() {
            out.push_str(&format!(" (after {})", t.dependencies.join(", ")));
        }
        if let Some(reviewer) = &t.reviewer {
            out.push_str(&format!(" (reviewer {reviewer})"));
        }
        if let Some(note) = &t.note {
            out.push_str(&format!(" — {}", truncate_chars(&one_line(note), 100)));
        }
        out.push('\n');
        if let Some(result) = &t.result {
            out.push_str(&format!(
                "    result: {}\n",
                truncate_chars(&one_line(result), 200)
            ));
        }
        for a in &t.artifacts {
            out.push_str(&format!("    {} {}", a.kind, a.value));
            if !a.description.is_empty() {
                out.push_str(&format!(" — {}", a.description));
            }
            out.push('\n');
        }
    }
    if out.is_empty() {
        out.push_str("(no tasks)\n");
    }
    out
}

/// A task's card in full, for whoever works on it or refers to it.
pub fn render_card(card: &TaskCard) -> String {
    let t = &card.task;
    let mut out = format!(
        "{} [{}] {} — {}\n",
        t.id,
        t.status,
        t.assigned_agent.as_deref().unwrap_or("unassigned"),
        t.title
    );
    if let Some(reviewer) = &t.reviewer {
        out.push_str(&format!("reviewer: {reviewer}\n"));
    }
    if let Some(note) = &t.note {
        out.push_str(&format!("note: {}\n", one_line(note)));
    }
    if !t.description.is_empty() && t.description != t.title {
        out.push_str(&format!("\n{}\n", t.description.trim_end()));
    }
    if !card.dependencies.is_empty() {
        out.push_str("\nBuilds on:\n");
        for dep in &card.dependencies {
            let by = dep.assigned_agent.as_deref().unwrap_or("unassigned");
            out.push_str(&format!("- {} [{}] by {by}", dep.id, dep.status));
            if let Some(result) = &dep.result {
                out.push_str(&format!(": {}", result.trim()));
            }
            out.push('\n');
            for a in &dep.artifacts {
                out.push_str(&format!("  {} {}\n", a.kind, a.value));
            }
        }
    }
    if let Some(result) = &t.result {
        out.push_str(&format!("\nResult: {}\n", result.trim()));
    }
    for a in &t.artifacts {
        out.push_str(&format!("  {} {}", a.kind, a.value));
        if !a.description.is_empty() {
            out.push_str(&format!(" — {}", a.description));
        }
        out.push('\n');
    }
    out
}

fn render_message(m: &Message) -> String {
    format!(
        "--- {} from {} to {} · type {} · id {}\n{}\n",
        fmt_clock(m.timestamp),
        m.from,
        m.to,
        m.kind,
        m.id,
        m.body.trim_end()
    )
}

fn render_commit(commit: &CommitInfo) -> String {
    format!(
        "Committed {} on {} ({} file(s)): {}\nFull id: {}",
        commit.short, commit.branch, commit.files_changed, commit.summary, commit.sha
    )
}

fn render_outcome(outcome: &GitOutcome, done: &str) -> String {
    if let Some(conflict) = &outcome.conflict {
        return format!(
            "CONFLICT: the {} was aborted and your worktree is unchanged.\nConflicting files:\n{}\n\n{}\n\
Coordinate with the author (agent_send) before trying again.",
            conflict.operation,
            conflict
                .files
                .iter()
                .map(|f| format!("  {f}"))
                .collect::<Vec<_>>()
                .join("\n"),
            conflict.detail
        );
    }
    match &outcome.commit {
        Some(commit) => format!("{done}: now at {} — {}", commit.short, commit.summary),
        None => outcome.text.clone(),
    }
}

/// Run one tool. `Err` is a message for the caller (shown as a tool error).
pub fn call(client: &mut Client, name: &str, args: &Value) -> Result<ToolOutput, CallError> {
    let out = |text: String, raw: Value| Ok(ToolOutput { text, raw });
    match name {
        "agents_list" => {
            let (agents, raw): (Vec<Agent>, _) = rpc(client, Call::AgentsList)?;
            out(render_agents(&agents), raw)
        }
        "agent_send" | "agent_broadcast" => {
            let recipients: Vec<String> = if name == "agent_broadcast" {
                vec![BROADCAST.to_string()]
            } else {
                // Several recipients: a copy each, so each answers on its own.
                let mut list: Vec<String> = Vec::new();
                for to in arg_str(args, "to")?
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    if !list.iter().any(|seen| seen == to) {
                        list.push(to.to_string());
                    }
                }
                if list.is_empty() {
                    return Err("'to' names nobody".into());
                }
                list
            };
            let several = recipients.len() > 1;
            if several {
                // All or none: a copy sent before a bad name would be sent
                // again when the call is retried.
                let (agents, _): (Vec<Agent>, _) = rpc(client, Call::AgentsList)?;
                let unknown: Vec<&str> = recipients
                    .iter()
                    .map(String::as_str)
                    .filter(|to| {
                        *to != crew_core::OPERATOR_ID && !agents.iter().any(|a| a.id == *to)
                    })
                    .collect();
                if !unknown.is_empty() {
                    return Err(format!("no agent {}; nothing was sent", unknown.join(", ")).into());
                }
            }
            let mut lines = Vec::new();
            let mut raws = Vec::new();
            for to in recipients {
                let (sent, raw): (SendResult, _) = rpc(
                    client,
                    Call::Send(SendParams {
                        to,
                        kind: message_type(args)?,
                        body: arg_str(args, "body")?,
                        correlation_id: opt_str(args, "correlation_id"),
                        reply_to: opt_str(args, "reply_to"),
                        metadata: args.get("metadata").cloned().unwrap_or(Value::Null),
                        // A chosen id names one message; copies get their own.
                        id: if several { None } else { opt_str(args, "id") },
                    }),
                )?;
                lines.push(if sent.duplicate {
                    format!(
                        "Already sent (id {}); not delivered twice.",
                        sent.message.id
                    )
                } else {
                    format!(
                        "Delivered to {} (id {}).",
                        sent.recipients.join(", "),
                        sent.message.id
                    )
                });
                raws.push(raw);
            }
            let raw = if raws.len() == 1 {
                raws.remove(0)
            } else {
                Value::Array(raws)
            };
            out(lines.join("\n"), raw)
        }
        "agent_inbox" => {
            if let Some(id) = opt_str(args, "id") {
                let (message, raw): (Message, _) = rpc(client, Call::MessageGet { id })?;
                return out(render_message(&message), raw);
            }
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n as usize);
            let (inbox, raw): (InboxResult, _) = rpc(
                client,
                Call::Inbox {
                    agent: None,
                    unread_only: !all,
                    ack: true,
                    limit,
                },
            )?;
            if inbox.messages.is_empty() {
                return out("No new messages.".into(), raw);
            }
            let mut text = format!("{} message(s):\n", inbox.messages.len());
            for entry in &inbox.messages {
                let m = &entry.message;
                text.push_str(&format!(
                    "\n--- {} from {} · type {} · id {}\n{}\n",
                    fmt_clock(m.timestamp),
                    m.from,
                    m.kind,
                    m.id,
                    m.body.trim_end()
                ));
            }
            out(text, raw)
        }
        "agent_compact" => {
            let keep = opt_str(args, "keep");
            let (agent, raw): (Agent, _) = rpc(
                client,
                Call::AgentCompact {
                    id: opt_str(args, "id"),
                    keep: keep.clone(),
                },
            )?;
            let mut text = format!(
                "A compaction of {}'s context is queued; it runs when its current turn ends",
                agent.id
            );
            match keep {
                Some(keep) => text.push_str(&format!(
                    ", keeping: {}",
                    truncate_chars(&one_line(&keep), 200)
                )),
                None => text.push('.'),
            }
            out(text, raw)
        }
        "agent_status" => {
            let status: AgentStatus = arg_str(args, "status")?.parse()?;
            let (agent, raw): (Agent, _) = rpc(
                client,
                Call::StatusUpdate {
                    status,
                    note: opt_str(args, "note"),
                },
            )?;
            out(format!("Status set to {}.", agent.status), raw)
        }
        "tasks_list" => {
            let (tasks, raw): (Vec<Task>, _) = rpc(client, Call::TasksList)?;
            out(render_tasks(&tasks), raw)
        }
        "task_get" => {
            let (card, raw): (TaskCard, _) = rpc(
                client,
                Call::TaskGet {
                    id: opt_str(args, "id"),
                },
            )?;
            out(render_card(&card), raw)
        }
        "task_update" => {
            let percent = args
                .get("percent")
                .and_then(Value::as_u64)
                .ok_or("missing required argument 'percent'")?;
            if percent > 100 {
                return Err("percent must be between 0 and 100".into());
            }
            let (_, raw): (Value, _) = rpc(
                client,
                Call::Progress {
                    percent: percent as u8,
                    message: opt_str(args, "message"),
                    task_id: opt_str(args, "task_id"),
                },
            )?;
            out(format!("Progress recorded: {percent}%."), raw)
        }
        "task_complete" => {
            let artifacts: Vec<Artifact> = match args.get("artifacts") {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|a| {
                        Some(Artifact {
                            kind: a.get("kind")?.as_str()?.to_string(),
                            value: a.get("value")?.as_str()?.to_string(),
                            description: a
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            agent: None,
                            created_at: 0,
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let (task, raw): (Task, _) = rpc(
                client,
                Call::TaskComplete {
                    task_id: opt_str(args, "task_id"),
                    result: opt_str(args, "result").unwrap_or_default(),
                    artifacts,
                },
            )?;
            let text = match task.status {
                crew_core::TaskStatus::Review => format!(
                    "Task {} submitted; {} was asked to review it. You will be told the verdict.",
                    task.id,
                    task.reviewer.as_deref().unwrap_or("a reviewer")
                ),
                _ => format!("Task {} is {}.", task.id, task.status),
            };
            out(text, raw)
        }
        "decline" => {
            let (_, raw): (Value, _) = rpc(
                client,
                Call::Decline {
                    reason: arg_str(args, "reason")?,
                },
            )?;
            out(
                "Noted: when this turn ends, crew hands the work to another model. Stop here."
                    .to_string(),
                raw,
            )
        }
        "task_fail" => {
            let (task, raw): (Task, _) = rpc(
                client,
                Call::TaskFail {
                    task_id: opt_str(args, "task_id"),
                    reason: arg_str(args, "reason")?,
                },
            )?;
            out(format!("Task {} marked failed.", task.id), raw)
        }
        "review_request" => {
            let (sent, raw): (SendResult, _) = rpc(
                client,
                Call::ReviewRequest {
                    agent_id: arg_str(args, "agent_id")?,
                    artifact: arg_str(args, "artifact")?,
                    task_id: opt_str(args, "task_id"),
                    note: opt_str(args, "note"),
                },
            )?;
            out(
                format!("Review requested from {}.", sent.recipients.join(", ")),
                raw,
            )
        }
        "review_submit" => {
            let approve = args
                .get("approve")
                .and_then(Value::as_bool)
                .ok_or("missing required argument 'approve'")?;
            let (task, raw): (Task, _) = rpc(
                client,
                Call::ReviewSubmit {
                    task_id: arg_str(args, "task_id")?,
                    approve,
                    comments: opt_str(args, "comments").unwrap_or_default(),
                },
            )?;
            out(
                format!("Review recorded: task {} is now {}.", task.id, task.status),
                raw,
            )
        }
        "git_commit" => {
            let (commit, raw): (CommitInfo, _) = rpc(
                client,
                Call::GitCommit {
                    message: arg_str(args, "message")?,
                    agent: opt_str(args, "agent"),
                },
            )?;
            out(render_commit(&commit), raw)
        }
        "git_diff" => {
            let (outcome, raw): (GitOutcome, _) = rpc(
                client,
                Call::GitDiff {
                    against: opt_str(args, "against"),
                    agent: opt_str(args, "agent"),
                },
            )?;
            let text = if outcome.text.trim().is_empty() {
                "No changes.".to_string()
            } else {
                outcome.text.clone()
            };
            out(text, raw)
        }
        "git_status" => {
            let (outcome, raw): (GitOutcome, _) = rpc(
                client,
                Call::GitStatus {
                    agent: opt_str(args, "agent"),
                },
            )?;
            out(outcome.text.clone(), raw)
        }
        "git_cherry_pick" => {
            let (outcome, raw): (GitOutcome, _) = rpc(
                client,
                Call::GitCherryPick {
                    commit: arg_str(args, "commit")?,
                    agent: opt_str(args, "agent"),
                },
            )?;
            out(render_outcome(&outcome, "Cherry-picked"), raw)
        }
        "git_merge" => {
            let (outcome, raw): (GitOutcome, _) = rpc(
                client,
                Call::GitMerge {
                    from_agent: arg_str(args, "from_agent")?,
                    agent: opt_str(args, "agent"),
                },
            )?;
            out(render_outcome(&outcome, "Merged"), raw)
        }
        "agent_spawn" => {
            let runtime = match opt_str(args, "runtime") {
                Some(r) => Some(r.parse()?),
                None => None,
            };
            let (agent, raw): (Agent, _) = rpc(
                client,
                Call::AgentSpawn {
                    id: opt_str(args, "id"),
                    role: opt_str(args, "role").unwrap_or_default(),
                    task: arg_str(args, "task")?,
                    runtime,
                    model: opt_str(args, "model"),
                },
            )?;
            out(
                format!(
                    "Subagent {} is starting on its task. It will message you when it is done; \
reach it with agent_send(to=\"{}\").",
                    agent.id, agent.id
                ),
                raw,
            )
        }
        "agent_add" => {
            let runtime = match opt_str(args, "runtime") {
                Some(r) => Some(r.parse()?),
                None => None,
            };
            let (agent, raw): (Agent, _) = rpc(
                client,
                Call::AgentAdd {
                    spec: crew_core::config::AgentSpec {
                        id: arg_str(args, "id")?,
                        role: opt_str(args, "role").unwrap_or_default(),
                        runtime,
                        model: opt_str(args, "model"),
                        task: opt_str(args, "task"),
                        ..Default::default()
                    },
                },
            )?;
            // Started on the work style's fallback: say why, and for how long.
            let instead = agent
                .fallback
                .as_ref()
                .filter(|f| f.policy)
                .map(|f| {
                    format!(
                        " It starts there, not on {}: {}; it moves to {} once that resets.",
                        f.from, f.note, f.from
                    )
                })
                .unwrap_or_default();
            let temporary = if agent.temporary {
                " It is temporary: not saved with the roster, it leaves the team once its tasks are \
done and you have been told so."
            } else {
                ""
            };
            out(
                format!(
                    "{} joined the team as {} ({}{}).{instead}{temporary}",
                    agent.id,
                    agent.role,
                    agent.runtime,
                    agent
                        .model
                        .as_deref()
                        .map(|m| format!(", {m}"))
                        .unwrap_or_default()
                ),
                raw,
            )
        }
        "agent_remove" => {
            let id = arg_str(args, "id")?;
            let (_, raw): (Value, _) = rpc(client, Call::AgentRemove { id: id.clone() })?;
            out(format!("{id} left the team."), raw)
        }
        "usage_status" => {
            let (value, raw): (Value, _) = rpc(client, Call::UsageStatus)?;
            out(value["text"].as_str().unwrap_or_default().to_string(), raw)
        }
        "agent_set_model" => {
            let id = arg_str(args, "id")?;
            let (agent, raw): (Agent, _) = rpc(
                client,
                Call::AgentSetModel {
                    id: id.clone(),
                    model: arg_str(args, "model")?,
                    runtime: None,
                    note: opt_str(args, "note"),
                },
            )?;
            out(
                format!(
                    "{id} is being restarted on {}; it resumes its session there and reads your note first.",
                    agent.model_name.clone().or(agent.model).unwrap_or_default()
                ),
                raw,
            )
        }
        "task_create" => {
            let depends_on = match args.get("depends_on") {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                _ => Vec::new(),
            };
            let (task, raw): (Task, _) = rpc(
                client,
                Call::TaskCreate {
                    spec: TaskSpec {
                        id: arg_str(args, "id")?,
                        title: opt_str(args, "title"),
                        description: opt_str(args, "description").unwrap_or_default(),
                        agent: opt_str(args, "agent"),
                        depends_on,
                        reviewer: opt_str(args, "reviewer"),
                        timeout_secs: args.get("timeout_secs").and_then(Value::as_u64),
                    },
                },
            )?;
            out(format!("Task {} created ({}).", task.id, task.status), raw)
        }
        "task_edit" => {
            let (task, raw): (Task, _) = rpc(
                client,
                Call::TaskEdit {
                    id: arg_str(args, "id")?,
                    title: opt_str(args, "title"),
                    description: opt_str(args, "description"),
                },
            )?;
            let told = match task.status {
                crew_core::TaskStatus::Working
                | crew_core::TaskStatus::Waiting
                | crew_core::TaskStatus::Blocked => task
                    .assigned_agent
                    .as_deref()
                    .map(|a| format!("; {a} was told to read it again")),
                crew_core::TaskStatus::Review => task
                    .reviewer
                    .as_deref()
                    .map(|r| format!("; {r}, reviewing it, was told to read it again")),
                _ => None,
            };
            out(
                format!("Card {} revised{}.", task.id, told.unwrap_or_default()),
                raw,
            )
        }
        "task_assign" => {
            let (task, raw): (Task, _) = rpc(
                client,
                Call::TaskAssign {
                    id: arg_str(args, "id")?,
                    agent: arg_str(args, "agent")?,
                },
            )?;
            out(
                format!(
                    "Task {} assigned to {}.",
                    task.id,
                    task.assigned_agent.as_deref().unwrap_or("?")
                ),
                raw,
            )
        }
        "task_retry" => {
            let (task, raw): (Task, _) = rpc(
                client,
                Call::TaskRetry {
                    id: arg_str(args, "id")?,
                },
            )?;
            out(format!("Task {} queued again.", task.id), raw)
        }
        "integrate" => {
            let (result, raw): (Value, _) = rpc(client, Call::Integrate)?;
            let merged = result["merged"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let mut text = format!(
                "Integration branch {}: merged [{}].",
                result["branch"].as_str().unwrap_or("?"),
                merged
            );
            if let Some(conflicts) = result["conflicts"].as_array().filter(|c| !c.is_empty()) {
                text.push_str("\nConflicts (branches left unmerged):");
                for c in conflicts {
                    text.push_str(&format!(
                        "\n  {}: {}",
                        c["agent"].as_str().unwrap_or("?"),
                        c["files"]
                    ));
                }
            }
            out(text, raw)
        }
        "operator_ask" => {
            let mut metadata = serde_json::Map::new();
            if let Some(options) = args.get("options").filter(|o| o.is_array()) {
                metadata.insert("options".into(), options.clone());
            }
            if let Some(member) = opt_str(args, "asked_by") {
                metadata.insert("asked_by".into(), json!(member));
            }
            let (sent, raw): (SendResult, _) = rpc(
                client,
                Call::Send(SendParams {
                    to: OPERATOR_ID.to_string(),
                    kind: MessageType::Question,
                    body: arg_str(args, "question")?,
                    correlation_id: None,
                    reply_to: None,
                    metadata: Value::Object(metadata),
                    id: opt_str(args, "id"),
                }),
            )?;
            out(
                format!(
                    "The question is with the operator (id {}). Their answer arrives as a [crew] \
message from operator; do not ask again. Until then keep everything that does not depend on it \
moving.",
                    sent.message.id
                ),
                raw,
            )
        }
        "session_complete" => {
            let decisions: Vec<String> = args
                .get("decisions")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let (done, raw): (Value, _) = rpc(
                client,
                Call::SessionComplete {
                    summary: arg_str(args, "summary")?,
                    decisions,
                },
            )?;
            let text = match done.get("decisions").and_then(Value::as_u64).unwrap_or(0) {
                0 => "Final report recorded.".to_string(),
                n => format!(
                    "Final report recorded. {n} decision(s) are with the operator; each answer \
arrives as a [crew] message from operator. Act on it when it comes: add tasks if work follows \
from it, and report again once that is done."
                ),
            };
            out(text, raw)
        }
        other => Err(format!("unknown tool '{other}'").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schemas_are_well_formed() {
        let defs = definitions();
        let mut names = std::collections::HashSet::new();
        for def in &defs {
            assert!(names.insert(def.name), "duplicate tool {}", def.name);
            assert_eq!(def.schema["type"], "object", "{}", def.name);
            let props = def.schema["properties"].as_object().unwrap();
            for required in def.schema["required"].as_array().unwrap() {
                assert!(
                    props.contains_key(required.as_str().unwrap()),
                    "{}: required '{}' is not a property",
                    def.name,
                    required
                );
            }
            assert!(!def.description.is_empty());
        }
        for expected in [
            "agents_list",
            "agent_send",
            "agent_inbox",
            "agent_status",
            "task_update",
            "task_complete",
        ] {
            assert!(names.contains(expected), "missing tool {expected}");
        }
        assert!(defs
            .iter()
            .any(|d| d.orchestrator_only && d.name == "task_create"));
        // Members ask the orchestrator; only it brings in the operator.
        assert!(defs
            .iter()
            .any(|d| d.orchestrator_only && d.name == "operator_ask"));
    }

    #[test]
    fn argument_helpers() {
        let args = json!({ "to": "a", "empty": "", "type": "question" });
        assert_eq!(arg_str(&args, "to").unwrap(), "a");
        assert!(arg_str(&args, "empty").is_err());
        assert!(arg_str(&args, "missing").is_err());
        assert_eq!(message_type(&args).unwrap(), MessageType::Question);
        assert_eq!(message_type(&json!({})).unwrap(), MessageType::Message);
        assert!(message_type(&json!({ "type": "nope" })).is_err());
    }
}
