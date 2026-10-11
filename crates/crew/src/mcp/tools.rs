//! The agent-facing tool set.
//!
//! Each tool is a thin mapping from JSON arguments to one protocol call, plus
//! a compact text rendering of the result. The MCP server exposes these as
//! MCP tools and `crew ctl` exposes the same functions as subcommands, so the
//! two front ends cannot drift apart.

use crew_core::client::{Client, ClientError};
use crew_core::config::TaskSpec;
use crew_core::rpc::{Call, CommitInfo, GitOutcome, InboxResult, SendParams, SendResult, TaskCard};
use crew_core::types::{AgentToolbox, ToolboxItem, ToolboxItemCode};
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
            "agent_peek",
            "Where a teammate stands, without waking it: its status, what it is doing now and \
for how long it has been quiet, and its open cards with their notes, results and commits. It \
costs the teammate nothing, where a message asking it costs a turn over its whole \
conversation; ask it only for what this does not show.",
            object(
                json!({ "id": { "type": "string", "description": "The teammate's id" } }),
                &["id"],
            ),
            true,
        ),
        tool(
            "agent_send",
            "Send a message directly to one teammate. It is delivered straight into that agent's \
session; the orchestrator is not involved. Use type \"question\" when you need an answer, \
\"answer\" to reply, \"artifact\" to hand over a commit id or file, \"status\" for news that needs \
nothing back (it does not wake a resting teammate unless that teammate is waiting).",
            object(
                json!({
                    "to": { "type": "string", "description": "Recipient agent id, e.g. claude-5; several separated by commas (claude-5,db), each getting its own copy" },
                    "body": { "type": "string", "description": "The message text" },
                    "type": { "type": "string", "enum": MESSAGE_TYPES, "default": "message" },
                    "reply_to": { "type": "string", "description": "Id of the message this answers" },
                    "wait": { "type": "boolean", "description": "You wait for their answer: your status is set to waiting on them (your card parked) in the same call, with no agent_status after it" },
                }),
                &["to", "body"],
            ),
            false,
        ),
        tool(
            "agent_broadcast",
            "Send one message to every other agent on the team. News that needs nothing back goes as \
type \"status\": it does not wake a resting teammate unless that teammate is waiting.",
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
            "agent_clear",
            "Start a context afresh: when the session's current turn ends it is replaced by a \
new session of the same runtime and model with nothing of the conversation kept; the briefing is \
in place as at any launch, an open card is handed to it again, and the note is the first thing it \
reads. For when what came before does not matter (a card finished, the next work unrelated to \
it): cheaper and cleaner than a compaction, which keeps a summary that every later request \
carries. Without an id it is your own session; the orchestrator may name a member, never itself.",
            object(
                json!({
                    "id": { "type": "string", "description": "The member to clear (orchestrator only); leave out for yourself" },
                    "note": { "type": "string", "description": "What the fresh session should know first: why, and what to pick up" },
                }),
                &[],
            ),
            false,
        ),
        tool(
            "agent_inbox",
            "Read your messages in full. By default returns unread messages and marks them read; \
with an id, that one message (one you sent or received), e.g. one a card or a teammate points to. \
New messages are delivered to you without asking (lines starting with \"[crew]\"), so do not \
call this to wait for one.",
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
            "Set your status: working, waiting (park this card), blocked or idle. Add a note \
saying on whom or what. task_id names a parked card, or an assigned Ready card to start with \
working. Starting requires no executing card and all dependencies done. Defaults to the \
executing card, otherwise the latest started waiting card.",
            object(
                json!({
                    "status": { "type": "string", "enum": ["working", "waiting", "blocked", "idle"] },
                    "note": { "type": "string" },
                    "task_id": { "type": "string" },
                }),
                &["status"],
            ),
            false,
        ),
        tool(
            "tasks_list",
            "Show the board: every task that is not done in full, with status, assignee, \
dependencies and artifacts; then each done task on one line (id, assignee, title, latest commit).",
            object(
                json!({
                    "done": { "type": "boolean", "description": "Show done tasks in full too", "default": false },
                }),
                &[],
            ),
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
            "Commit all changes in your workspace on your branch (agents/<your id>, which plain git \
sees too). Returns the commit id, which you can send to teammates.",
            object(json!({ "message": { "type": "string" } }), &["message"]),
            false,
        ),
        tool(
            "git_diff",
            "Show your uncommitted changes, or the difference against a revision.",
            object(
                json!({ "against": { "type": "string", "description": "Commit id, jj change id or branch (optional)" } }),
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
            object(
                json!({ "commit": { "type": "string", "description": "Its commit id (or jj change id)" } }),
                &["commit"],
            ),
            false,
        ),
        tool(
            "git_merge",
            "Merge a teammate's whole branch into yours, or, with from_agent \"integration\", the \
integration branch (agents/integration: where a workflow team lands every completed card). On \
conflict nothing is changed and the conflicting files are reported.",
            object(
                json!({ "from_agent": { "type": "string", "description": "A teammate's id, or integration" } }),
                &["from_agent"],
            ),
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
                    "toolbox": { "type": "array", "items": { "type": "string" }, "description": "Bundles by name, for a subagent that needs less than your own toolbox; default: a copy of yours. It cannot get anything you do not have" },
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
                    "toolbox": { "type": "array", "items": { "type": "string" }, "description": "Bundles from the team's toolboxes this member gets on top of the team default, by name (e.g. [\"web\"]). The team file defines what each bundle holds" },
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
dependencies are done; the agent reads the card with task_get. Create independent cards \
immediately so free members can work while other tasks run or await review.",
            object(
                json!({
                    "id": { "type": "string", "description": "Short id: lowercase letters, digits, - and _" },
                    "title": { "type": "string" },
                    "description": { "type": "string", "description": "What is to be done and how to tell it is done; point to docs, tasks, messages and commits by path or id instead of restating them" },
                    "agent": { "type": "string", "description": "Assignee (omit to let the scheduler pick)" },
                    "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Completion gates: only tasks whose finished, approved output is needed before any part of this card can start. Split independent preparation into a separate card; no dependency is needed merely to order phases or prevent two tasks on the same agent." },
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
            "workflow_run",
            "Lay this team's workflow on the board for one unit of work (a phase): one card per \
step, named <step>-<phase>, assigned to the step's role and chained as the workflow says, each \
carrying the brief. Only a team whose style declares a workflow has one; your briefing lists its \
steps. Returns the cards.",
            object(
                json!({
                    "phase": { "type": "string", "description": "Short id of the unit, as in 03-auth: lowercase letters, digits, - and _" },
                    "title": { "type": "string", "description": "The unit's title, part of every card's title" },
                    "brief": { "type": "string", "description": "What every step needs to know: the goal, the scope, what came before, where to look" },
                }),
                &["phase", "title"],
            ),
        ),
        tool(
            "workflow_slices",
            "Declare a unit's slices once its plan says what they are: the parts of the unit \
that can be done side by side (a slice with `depends` waits for those slices first; slices whose \
`touches` overlap take turns). Each slice step of the workflow gets one card per slice (named \
<step>-<phase>-<slice>, spread over the step's members in turn), and the steps after them are \
laid out to wait for every slice. All or none. Declared again before any slice has started \
(a review changed the split), the cards are replaced; once a slice has begun they stay. The \
orchestrator may declare any unit's; a member the one its open card belongs to.",
            object(
                json!({
                    "phase": { "type": "string", "description": "The unit, as given to workflow_run" },
                    "slices": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Short id: lowercase letters, digits, - and _ (e.g. api)" },
                                "title": { "type": "string" },
                                "brief": { "type": "string", "description": "What this slice covers: its tasks from the plan, the files it owns, what it must not touch" },
                                "depends": { "type": "array", "items": { "type": "string" }, "description": "Slices of this unit it waits for; left out, it runs beside the others" },
                                "touches": { "type": "array", "items": { "type": "string" }, "description": "The files or directories this slice writes (workspace-relative). Two slices that touch the same path take turns (the later-declared one waits for the earlier); a slice that names none runs alone, after every earlier slice and before every later one. What a slice writes outside its touches is reported when its card is done" },
                            },
                            "required": ["id", "title"],
                            "additionalProperties": false,
                        },
                    },
                }),
                &["phase", "slices"],
            ),
            false,
        ),
        tool(
            "workflow_phases",
            "Queue units of work for the workflow, in order (the phases of a roadmap). Under the \
workflow's auto mode crew runs each when the one before lets it; otherwise the orchestrator runs \
them with workflow_run. The orchestrator may queue; a member whose open card is the roadmap's may \
too. Adds to what is queued.",
            object(
                json!({
                    "phases": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Short id, as in 03-auth" },
                                "title": { "type": "string" },
                                "brief": { "type": "string", "description": "What every step of the unit needs to know" },
                            },
                            "required": ["id", "title"],
                            "additionalProperties": false,
                        },
                    },
                }),
                &["phases"],
            ),
            false,
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

/// `tasks_list`'s `done` argument: whether done cards are shown in full.
/// Anything but true, false or nothing is refused rather than read as false.
fn done_in_full(args: &Value) -> Result<bool, String> {
    match args.get("done") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(done)) => Ok(*done),
        Some(_) => Err("done must be true or false".into()),
    }
}

/// The `toolbox` argument: bundle names, as a list or one comma-separated
/// string. Anything else is refused rather than dropped, so an attempt to
/// define items never passes as "no choice".
fn bundle_names(args: &Value) -> Result<Option<Vec<String>>, String> {
    const ONLY_NAMES: &str =
        "toolbox takes bundle names from the team's toolboxes, e.g. [\"web\"]; \
the team file defines what each bundle holds";
    let names: Vec<&str> = match args.get("toolbox") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(one)) => one.split(',').collect(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().ok_or(ONLY_NAMES))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(ONLY_NAMES.into()),
    };
    Ok(Some(
        names
            .into_iter()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    ))
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

/// Whether an item counts as a problem in a summary: not Active, and not
/// merely planned or not applicable.
fn not_active(item: &ToolboxItem) -> bool {
    !matches!(
        item.status.code,
        ToolboxItemCode::Active | ToolboxItemCode::Planned | ToolboxItemCode::NotApplicable
    )
}

/// A member's toolbox in a list line: `toolbox 3f9a0c21e7b4 (1 not available)`.
fn toolbox_cell(toolbox: &AgentToolbox) -> String {
    let mut cell = format!("toolbox {}", toolbox.short_fingerprint());
    let problems = toolbox.items.iter().filter(|i| not_active(i)).count();
    if problems > 0 {
        cell.push_str(&format!(" ({problems} not available)"));
    }
    if toolbox.pending_fingerprint.is_some() {
        cell.push_str(" (restart to apply a change)");
    }
    cell
}

/// What a member's toolbox holds, by kind, and what will not load:
/// `toolbox 3f9a0c21e7b4: mcp context7, playwright; skills frontend-qa;
/// not available on Codex: plugins.lint-kit`.
fn toolbox_summary(toolbox: &AgentToolbox) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut kinds: Vec<(String, Vec<&str>)> = Vec::new();
    for item in toolbox.items.iter().filter(|i| !not_active(i)) {
        let kind = serde_json::to_value(item.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        match kinds.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, names)) => names.push(&item.name),
            None => kinds.push((kind, vec![&item.name])),
        }
    }
    for (kind, names) in kinds {
        parts.push(format!("{kind} {}", names.join(", ")));
    }
    let mut problems: Vec<(String, Vec<String>)> = Vec::new();
    for item in toolbox.items.iter().filter(|i| not_active(i)) {
        let kind = serde_json::to_value(item.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let label = format!("{kind}.{}", item.name);
        let status = item.status.text.to_lowercase();
        match problems.iter_mut().find(|(s, _)| *s == status) {
            Some((_, items)) => items.push(label),
            None => problems.push((status, vec![label])),
        }
    }
    for (status, items) in problems {
        parts.push(format!("{status}: {}", items.join(", ")));
    }
    if parts.is_empty() {
        format!("toolbox {}: no items", toolbox.short_fingerprint())
    } else {
        format!(
            "toolbox {}: {}",
            toolbox.short_fingerprint(),
            parts.join("; ")
        )
    }
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
        if let Some(toolbox) = &a.toolbox {
            out.push_str(&format!(" · {}", toolbox_cell(toolbox)));
        }
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

/// One agent as `agent_peek` shows it: its line in `agents_list`, how long
/// it has been quiet, and every card it holds that is not finished, in full.
pub fn render_peek(agent: &Agent, tasks: &[Task], now: i64) -> String {
    let mut out = render_agents(std::slice::from_ref(agent));
    if agent.last_activity_at > 0 {
        out.push_str(&format!(
            "last active {} ago\n",
            crew_core::util::fmt_elapsed((now - agent.last_activity_at).max(0))
        ));
    }
    let cards: Vec<&Task> = tasks
        .iter()
        .filter(|t| {
            t.assigned_agent.as_deref() == Some(agent.id.as_str()) && !t.status.is_terminal()
        })
        .collect();
    if cards.is_empty() {
        out.push_str("no open cards\n");
    }
    for card in cards {
        render_task(&mut out, card);
    }
    out
}

/// Every card in full: status, assignee, dependencies, reviewer, note,
/// result and artifacts.
pub fn render_tasks(tasks: &[Task]) -> String {
    let mut out = String::new();
    for t in tasks {
        render_task(&mut out, t);
    }
    if out.is_empty() {
        out.push_str("(no tasks)\n");
    }
    out
}

/// What `tasks_list` shows. Every card that is not done is printed in full,
/// as [`render_tasks`] prints it; then each done card on one line: id,
/// assignee, title and its latest commit. A done card is shortened, never
/// left out, and `done_in_full` prints the whole board in full. Done cards
/// are most of a long session's board, and each one's result and commits
/// made it tens of kilobytes (notes/turn-efficiency.md, 2a).
pub fn render_board(tasks: &[Task], done_in_full: bool) -> String {
    let (done, open): (Vec<&Task>, Vec<&Task>) = tasks
        .iter()
        .partition(|t| t.status == crew_core::TaskStatus::Done);
    if done_in_full || done.is_empty() {
        return render_tasks(tasks);
    }
    let mut out = String::new();
    for t in open {
        render_task(&mut out, t);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&format!(
        "{} done task(s), one line each; task_get(id) reads one, tasks_list(done: true) shows \
them in full:\n",
        done.len()
    ));
    for t in done {
        out.push_str(&format!(
            "{} {} — {}",
            t.id,
            t.assigned_agent.as_deref().unwrap_or("unassigned"),
            truncate_chars(&one_line(&t.title), 120)
        ));
        let commits: Vec<&str> = t
            .artifacts
            .iter()
            .filter(|a| a.kind == "commit")
            .map(|a| a.value.as_str())
            .collect();
        match commits.as_slice() {
            [] => {}
            [one] => out.push_str(&format!(" · commit {}", short_commit(one))),
            [.., last] => out.push_str(&format!(
                " · {} commits, last {}",
                commits.len(),
                short_commit(last)
            )),
        }
        out.push('\n');
    }
    out
}

/// A commit id as a done card's line shows it: its first 8 characters.
fn short_commit(value: &str) -> String {
    let id = value.split_whitespace().next().unwrap_or_default();
    if id.chars().all(|c| c.is_ascii_hexdigit()) {
        id.chars().take(8).collect()
    } else {
        truncate_chars(&one_line(value), 40)
    }
}

/// One card in full, as `render_tasks` prints it.
fn render_task(out: &mut String, t: &Task) {
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

/// What `agent_inbox` shows. An empty inbox says so in a way that ends the
/// lookup: crew delivers a message that arrives later into the session.
fn render_inbox(inbox: &InboxResult) -> String {
    if inbox.messages.is_empty() {
        return NO_NEW_MESSAGES.to_string();
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
    text
}

const NO_NEW_MESSAGES: &str =
    "No new messages. One that arrives later is delivered to you: if you \
are waiting for it, end your turn, or park your card with agent_status(\"waiting\", …), instead \
of checking again.";

fn render_commit(commit: &CommitInfo) -> String {
    let mut text = format!(
        "Committed {} on {} ({} file(s)): {}\nFull id: {}",
        commit.short, commit.branch, commit.files_changed, commit.summary, commit.sha
    );
    if let Some(change) = &commit.change_id {
        text.push_str(&format!("\nChange id: {change}"));
    }
    text
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
        "agent_peek" => {
            let id = arg_str(args, "id")?;
            let (agents, raw): (Vec<Agent>, _) = rpc(client, Call::AgentsList)?;
            let agent = agents
                .into_iter()
                .find(|a| a.id == id)
                .ok_or_else(|| format!("no agent '{id}'"))?;
            let (tasks, _): (Vec<Task>, _) = rpc(client, Call::TasksList)?;
            out(render_peek(&agent, &tasks, crew_core::util::now_ms()), raw)
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
            let waiting_on = recipients.join(", ");
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
            // Parked in the same call: the message is out either way, so a
            // card that cannot be parked is said, not an error (the send
            // would be repeated).
            if name == "agent_send" && args.get("wait").and_then(Value::as_bool) == Some(true) {
                let parked: Result<(Agent, Value), CallError> = rpc(
                    client,
                    Call::StatusUpdate {
                        status: AgentStatus::Waiting,
                        note: Some(format!("waiting on {waiting_on}")),
                        task_id: None,
                    },
                );
                lines.push(match parked {
                    Ok(_) => format!("You are waiting on {waiting_on} now."),
                    Err(e) => format!("Your status was not set to waiting: {e}"),
                });
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
            out(render_inbox(&inbox), raw)
        }
        "agent_clear" => {
            let (agent, raw): (Agent, _) = rpc(
                client,
                Call::AgentClear {
                    id: opt_str(args, "id"),
                    note: opt_str(args, "note"),
                },
            )?;
            out(
                format!(
                    "A fresh start of {}'s session is queued: when its current turn ends, a new \
session takes over with nothing of the conversation kept.",
                    agent.id
                ),
                raw,
            )
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
                    task_id: opt_str(args, "task_id"),
                },
            )?;
            out(format!("Status set to {}.", agent.status), raw)
        }
        "tasks_list" => {
            let done = done_in_full(args)?;
            let (tasks, raw): (Vec<Task>, _) = rpc(client, Call::TasksList)?;
            out(render_board(&tasks, done), raw)
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
                    toolbox: bundle_names(args)?,
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
                    spec: Box::new(crew_core::config::AgentSpec {
                        id: arg_str(args, "id")?,
                        role: opt_str(args, "role").unwrap_or_default(),
                        runtime,
                        model: opt_str(args, "model"),
                        task: opt_str(args, "task"),
                        toolbox: crew_core::toolbox::Toolbox {
                            include: bundle_names(args)?.unwrap_or_default(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
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
            // What it was given beyond the team default, mapped onto its CLI.
            let toolbox = rpc::<Vec<crew_core::rpc::MemberToolbox>>(
                client,
                Call::Toolbox {
                    agent: Some(agent.id.clone()),
                    runtime: None,
                },
            )
            .ok()
            .and_then(|(members, _)| members.into_iter().next())
            .filter(|m| !m.toolbox.items.is_empty())
            .map(|m| format!(" Its {}.", toolbox_summary(&m.toolbox)))
            .unwrap_or_default();
            out(
                format!(
                    "{} joined the team as {} ({}{}).{toolbox}{instead}{temporary}",
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
        "workflow_run" => {
            let (tasks, raw): (Vec<Task>, _) = rpc(
                client,
                Call::WorkflowRun {
                    phase: arg_str(args, "phase")?,
                    title: arg_str(args, "title")?,
                    brief: opt_str(args, "brief").unwrap_or_default(),
                },
            )?;
            let chain: Vec<String> = tasks
                .iter()
                .map(|t| {
                    format!(
                        "{} [{}] {}",
                        t.id,
                        t.status,
                        t.assigned_agent.as_deref().unwrap_or("-")
                    )
                })
                .collect();
            out(
                format!(
                    "{} cards laid out; the first starts when its owner is free:\n{}",
                    tasks.len(),
                    chain.join("\n")
                ),
                raw,
            )
        }
        "workflow_slices" => {
            let slices: Vec<crew_core::guardrails::SliceSpec> =
                serde_json::from_value(args.get("slices").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("slices: {e}"))?;
            let (tasks, raw): (Vec<Task>, _) = rpc(
                client,
                Call::WorkflowSlices {
                    phase: arg_str(args, "phase")?,
                    slices,
                },
            )?;
            let chain: Vec<String> = tasks
                .iter()
                .map(|t| {
                    format!(
                        "{} [{}] {}",
                        t.id,
                        t.status,
                        t.assigned_agent.as_deref().unwrap_or("-")
                    )
                })
                .collect();
            out(
                format!(
                    "{} cards laid out; the slices start as their owners are free:\n{}",
                    tasks.len(),
                    chain.join("\n")
                ),
                raw,
            )
        }
        "workflow_phases" => {
            let phases: Vec<crew_core::guardrails::PhaseSpec> =
                serde_json::from_value(args.get("phases").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("phases: {e}"))?;
            let (queued, raw): (Vec<String>, _) = rpc(client, Call::WorkflowPhases { phases })?;
            out(
                format!(
                    "{} phase(s) queued, in order: {}",
                    queued.len(),
                    queued.join(" → ")
                ),
                raw,
            )
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

    fn board_card(id: &str, status: &str, result: Option<&str>, commits: usize) -> Task {
        let artifacts: Vec<Value> = (0..commits)
            .map(|n| {
                json!({ "kind": "commit", "value": format!("{n:x}{}", "a".repeat(39)),
                        "description": "Commit message ".repeat(8) })
            })
            .chain([json!({ "kind": "file", "value": "notes/plan.md" })])
            .collect();
        serde_json::from_value(json!({
            "id": id, "title": format!("Card {id}"), "status": status,
            "assigned_agent": "builder", "dependencies": ["base"], "reviewer": "checker",
            "note": "on schema", "result": result, "artifacts": artifacts,
            "created_at": 0,
        }))
        .unwrap()
    }

    /// The full board is what it was before the compact view: every card
    /// with its dependencies, reviewer, note, result and artifacts.
    #[test]
    fn the_full_board_is_unchanged() {
        let tasks = [
            board_card("open", "working", None, 0),
            board_card("shipped", "done", Some("Landed\nwith tests."), 1),
        ];
        let commit = format!("0{}", "a".repeat(39));
        let message = "Commit message ".repeat(8);
        let expected = format!(
            "open [working] builder — Card open (after base) (reviewer checker) — on schema
    file notes/plan.md
shipped [done] builder — Card shipped (after base) (reviewer checker) — on schema
    result: Landed with tests.
    commit {commit} — {message}
    file notes/plan.md
"
        );
        assert_eq!(render_tasks(&tasks), expected);
        assert_eq!(render_board(&tasks, true), expected);
        assert_eq!(render_tasks(&[]), "(no tasks)\n");
        assert_eq!(render_board(&[], false), "(no tasks)\n");
    }

    #[test]
    fn done_cards_are_one_line_each_unless_asked_for_in_full() {
        let tasks = [
            board_card("first", "done", Some("Merged."), 3),
            board_card("open-a", "working", None, 1),
            board_card("second", "done", None, 0),
            board_card("open-b", "waiting", None, 0),
            board_card("failed", "failed", Some("Could not build."), 0),
            board_card("third", "done", Some("Merged too."), 1),
            board_card("open-c", "review", None, 0),
        ];
        let compact = render_board(&tasks, false);
        let open: Vec<Task> = tasks
            .iter()
            .filter(|t| t.status != crew_core::TaskStatus::Done)
            .cloned()
            .collect();
        // Every card that is not done (failed included), in full and in
        // board order, then each done card on one line.
        let expected = format!(
            "{}\n3 done task(s), one line each; task_get(id) reads one, tasks_list(done: true) \
shows them in full:
first builder — Card first · 3 commits, last 2aaaaaaa
second builder — Card second
third builder — Card third · commit 0aaaaaaa
",
            render_tasks(&open)
        );
        assert_eq!(compact, expected);
        // Without done cards there is nothing to shorten.
        assert_eq!(render_board(&open, false), render_tasks(&open));
        // Only done cards: no open section.
        let done = [board_card("only", "done", Some("Merged."), 1)];
        assert_eq!(
            render_board(&done, false),
            "1 done task(s), one line each; task_get(id) reads one, tasks_list(done: true) \
shows them in full:\nonly builder — Card only · commit 0aaaaaaa\n"
        );
    }

    /// On a 100-card board, 90 of them done with long results and many
    /// commits, the done cards cost one short line each.
    #[test]
    fn a_long_board_stays_small() {
        let result = "A long result that goes on. ".repeat(80);
        let mut tasks: Vec<Task> = (0..90)
            .map(|n| board_card(&format!("done-{n:02}"), "done", Some(&result), 12))
            .collect();
        tasks.extend((0..10).map(|n| board_card(&format!("open-{n:02}"), "working", None, 1)));
        let full = render_board(&tasks, true);
        let compact = render_board(&tasks, false);
        let open = render_tasks(&tasks[90..]);
        assert!(compact.starts_with(&open));
        let done_lines: Vec<&str> = compact[open.len()..].lines().skip(2).collect();
        assert_eq!(done_lines.len(), 90);
        for (n, line) in done_lines.iter().enumerate() {
            assert!(
                line.starts_with(&format!("done-{n:02} builder — ")),
                "{line}"
            );
            assert!(line.len() <= 80, "{line}");
        }
        assert!(
            compact.len() < open.len() + 90 * 80 + 200,
            "{}",
            compact.len()
        );
        assert!(
            full.len() > 20 * compact.len(),
            "{} vs {}",
            full.len(),
            compact.len()
        );
        // A long title is cut, still on one line.
        let mut long = board_card("long", "done", None, 0);
        long.title = "word ".repeat(100);
        let line = render_board(&[long], false);
        assert_eq!(line.lines().count(), 2);
        assert!(line.lines().nth(1).unwrap().chars().count() <= 140);
    }

    #[test]
    fn tasks_list_takes_only_a_true_or_false_done() {
        let defs = definitions();
        let tasks = defs.iter().find(|d| d.name == "tasks_list").unwrap();
        assert_eq!(tasks.schema["properties"]["done"]["type"], "boolean");
        assert!(tasks.schema["required"].as_array().unwrap().is_empty());
        assert_eq!(done_in_full(&json!({})), Ok(false));
        assert_eq!(done_in_full(&json!({ "done": null })), Ok(false));
        assert_eq!(done_in_full(&json!({ "done": false })), Ok(false));
        assert_eq!(done_in_full(&json!({ "done": true })), Ok(true));
        for wrong in [json!("true"), json!(1), json!(["all"])] {
            assert_eq!(
                done_in_full(&json!({ "done": wrong })),
                Err("done must be true or false".to_string())
            );
        }
    }

    #[test]
    fn empty_inbox_says_to_stop_checking() {
        let empty: InboxResult = serde_json::from_value(json!({ "messages": [] })).unwrap();
        let text = render_inbox(&empty);
        assert!(text.starts_with("No new messages."), "{text}");
        assert!(text.contains("instead of checking again"), "{text}");

        let one: InboxResult = serde_json::from_value(json!({ "messages": [{
            "id": "m-1", "from": "runtime", "to": "designer", "type": "answer",
            "body": "D1 keeps acknowledgement as it is.\n", "timestamp": 0,
        }] }))
        .unwrap();
        let text = render_inbox(&one);
        assert!(text.starts_with("1 message(s):\n"), "{text}");
        assert!(
            text.contains(
                "from runtime · type answer · id m-1\nD1 keeps acknowledgement as it is.\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn inbox_description_says_messages_arrive_unasked() {
        let defs = definitions();
        let inbox = defs.iter().find(|d| d.name == "agent_inbox").unwrap();
        assert!(
            inbox
                .description
                .contains("do not call this to wait for one"),
            "{}",
            inbox.description
        );
    }

    #[test]
    fn send_descriptions_say_status_news_does_not_wake_resting_teammates() {
        // Backed by delivery::tests::news_waits_for_company_unless_it_is_what_a_member_waits_for.
        let defs = definitions();
        for name in ["agent_send", "agent_broadcast"] {
            let def = defs.iter().find(|d| d.name == name).unwrap();
            assert!(
                def.description
                    .contains("does not wake a resting teammate unless that teammate is waiting"),
                "{name}: {}",
                def.description
            );
        }
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

    /// The orchestrator chooses bundles by name and cannot define items
    /// (notes/toolboxes.md, Orchestrator tools): the schemas take strings
    /// only, and anything else is refused, never dropped.
    #[test]
    fn toolbox_arguments_are_bundle_names_only() {
        let defs = definitions();
        for tool in ["agent_add", "agent_spawn"] {
            let def = defs.iter().find(|d| d.name == tool).unwrap();
            let toolbox = &def.schema["properties"]["toolbox"];
            assert_eq!(toolbox["type"], "array", "{tool}");
            assert_eq!(toolbox["items"], json!({ "type": "string" }), "{tool}");
        }
        assert_eq!(bundle_names(&json!({})), Ok(None));
        assert_eq!(bundle_names(&json!({ "toolbox": null })), Ok(None));
        assert_eq!(
            bundle_names(&json!({ "toolbox": [" web ", "", "docs"] })),
            Ok(Some(vec!["web".to_string(), "docs".to_string()]))
        );
        assert_eq!(
            bundle_names(&json!({ "toolbox": "web, docs" })),
            Ok(Some(vec!["web".to_string(), "docs".to_string()]))
        );
        assert_eq!(bundle_names(&json!({ "toolbox": [] })), Ok(Some(vec![])));
        for defining in [
            json!({ "toolbox": { "mcp": { "x": { "command": ["x"] } } } }),
            json!({ "toolbox": ["web", { "mcp": { "x": { "command": ["x"] } } }] }),
            json!({ "toolbox": 1 }),
        ] {
            let refused = bundle_names(&defining).unwrap_err();
            assert!(refused.contains("bundle names"), "{defining}: {refused}");
        }
    }
}
