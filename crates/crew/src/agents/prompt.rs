//! Text handed to agents: standing instructions, task dispatches and the
//! phrasing of incoming bus messages.

use std::fmt::Write;

use crew_core::config::Voice;
use crew_core::{Agent, Message, Task, ORCHESTRATOR_ID};

/// Per-message cap when injecting bodies into a session; the full text stays
/// available through `agent_inbox`.
const NUDGE_BODY_CHARS: usize = 4000;

/// What a briefing needs to know about the session.
pub struct TeamView<'a> {
    pub project: &'a str,
    pub objective: &'a str,
    /// The work style the team was launched from, and its standing
    /// instructions.
    pub style: Option<&'a str>,
    pub instructions: &'a str,
    /// The model catalog as a briefing section (orchestrator only).
    pub models: &'a str,
    pub agents: &'a [Agent],
    /// Agents work in separate git worktrees.
    pub worktrees: bool,
    /// The version control backend the worktrees are made with.
    pub vcs: crate::vcs::Kind,
    /// How members write to one another.
    pub voice: Voice,
}

/// The team's standing instructions (its work style), as a briefing section.
fn working_style(view: &TeamView<'_>) -> String {
    let instructions = view.instructions.trim();
    if instructions.is_empty() {
        return String::new();
    }
    let title = match view.style {
        Some(style) => format!("How this team works ({style})"),
        None => "How this team works".to_string(),
    };
    format!("\n## {title}\n{instructions}\n")
}

fn roster(view: &TeamView<'_>, me: &str) -> String {
    let mut out = String::new();
    for agent in view.agents {
        let you = if agent.id == me { " (you)" } else { "" };
        if let Some(parent) = &agent.parent {
            let _ = writeln!(
                out,
                "- {}{you} — subagent of {parent}: {}",
                agent.id, agent.role
            );
            continue;
        }
        let role = if agent.orchestrator {
            "assigns tasks, tracks the plan, aggregates results"
        } else if agent.role.is_empty() {
            "worker"
        } else {
            agent.role.as_str()
        };
        let temporary = if agent.temporary { " (temporary)" } else { "" };
        let _ = writeln!(out, "- {}{you}{temporary} — {role}", agent.id);
    }
    out
}

const TOOLS_COMMON: &str = "\
- agents_list() — who is on the team, their role, status and current task.
- agent_peek(id) — where one teammate stands (status, what it is doing, its open cards), without \
waking it.
- agent_send(to, body, type?, wait?) — send a message straight to one teammate; wait: true also parks your card as waiting on them.
- agent_broadcast(body) — tell every teammate at once.
- agent_inbox(id?) — read your unread messages in full; with an id, that one message (one you \
sent or received).
- agent_compact(keep?) — compact your own conversation when your context window is filling up; \
it happens when the current turn ends, and you are pointed back to your task afterwards.
- tasks_list(done?) — the board: open tasks in full, done ones one line each; done: true shows \
them in full too.
- task_get(id?) — one task's card: what is to be done, the results and commits it builds on, its \
reviewer, where it stands. Without an id, your task in progress.";

const TOOLS_WORKER: &str = "\
- task_update(percent, message) — report progress on your current task at meaningful milestones.
- agent_status(status, note, task_id?) — set your status where crew cannot tell it: waiting (park a card) on something other than a question you asked, or blocked.
- git_commit(message) — commit everything in your worktree; returns the commit id.
- git_diff(against?) / git_status() — inspect your changes.
- git_cherry_pick(commit) / git_merge(from_agent) — bring a teammate's work into your branch.
- review_request(agent_id, artifact) — ask a teammate to review something.
- review_submit(task_id, approve, comments) — answer a review request you received.
- task_complete(result) — finish your current task (call it once, after committing).
- task_fail(reason) — give up on your task when it cannot be done.
- decline(reason) — you will not do what you were just asked, because it goes against your \
guidelines or usage policies: crew hands it to another model. Text you write in this session \
reaches nobody, so a refusal in words alone goes unseen.
- agent_spawn(task, role?, id?) — launch a subagent of your own for a self-contained piece of \
your task. It is a full teammate with its own worktree branched from yours; it reports back to \
you with a message (and commit ids) when it is done.";

const TOOLS_ORCHESTRATOR: &str = "\
- agent_add(id, role, model?, runtime?, task?) — add a member to the team (a real agent process); \
the model decides which CLI runs it (a pi member: runtime pi and the model as pi names it). Once \
the team is founded, the members you add are temporary (see Staff).
- agent_remove(id) — take a member off the team; its open tasks return to the pool.
- agent_set_model(id, model, note?) — put a member on another model its CLI runs; its session \
is restarted there, resumed, and reads the note first. Your own model, and moving anyone to \
another CLI, are the operator's call.
- task_create(id, title, description, agent?, depends_on?, reviewer?) — put a task on the board.
- task_edit(id, title?, description?) — revise a card; whoever is at work on it is pointed back to it.
- task_assign(id, agent) — (re)assign a task.
- task_retry(id) — put a failed or blocked task back in the queue.
- operator_ask(question, options?, asked_by?) — put a question to the human who started this team.
- agent_compact(id?, keep?) — queue a compaction of a member's context window, or of your own.
- session_complete(summary) — record the final report once every task is done.";

/// How the team's voice is written: the rules a model follows on the
/// coordination channel. After caveman's skills
/// (https://github.com/JuliusBrussee/caveman, Apache-2.0).
pub fn voice_rules(voice: Voice) -> &'static str {
    match voice {
        Voice::Megacave => "\
Voice: megacave. Write in 文言 (Classical Chinese): subjects dropped where clear, verb before \
object, particles (之 乃 為 其 則 故 以) for the relations, each fact once.
- Verbatim, in their own script: code, commands, paths, ids, API and tool names, error \
strings, commit shas. Numbers in Arabic numerals. Keep every negation (不 非 勿 未).
- A clause that could be read two ways, a security warning or an irreversible step: plain \
English instead.
- Example: `parse_header` 遇空行即崩。已補檢於 `src/parse.rs:88`，`cargo test` 皆過，見 a1b2c3d。",
        Voice::Ultracave => "\
Voice: ultracave. Fragments: drop articles, copulas and connectives where the order stays \
clear; one word when one word is enough; each fact once.
- Verbatim: code, commands, paths, ids, API and tool names, error strings, commit shas, \
numbers and units. Keep not/never/no/only/except. No invented abbreviations, no arrows.
- A fragment that could be read two ways, a security warning or an irreversible step: a plain \
sentence instead.
- Example: `parse_header` panics on empty line. Guard at `src/parse.rs:88`. `cargo test` green. a1b2c3d.",
        Voice::Caveman => "\
Voice: caveman. Short plain sentences and no filler: no pleasantries, hedges, restating or \
summaries; each fact once.
- Verbatim: code, commands, paths, ids, API and tool names, error strings, commit shas, numbers.
- Example: `parse_header` panicked on an empty line. Added a guard at `src/parse.rs:88`; \
`cargo test` passes. Commit a1b2c3d.",
        Voice::Plain => "\
Voice: plain. Concise English: no pleasantries, restating or padding; exact ids, paths, \
path:line and commit shas.",
    }
}

/// What takes the place of a runtime's own guidance on writing for a person
/// in a member's system prompt: Claude Code's `tone` section (through
/// crew's mod), Codex's developer instructions. The same for every member
/// of a team, so it caches like the rest of the system prompt.
pub fn member_tone(voice: Voice) -> String {
    format!(
        "# Tone and style
You are a member of a team of coding agents run by crew. No person reads this session: \
what you write is read by other models. The reply you end a turn with is carried to the \
teammate whose message started it; text between tool calls reaches nobody. This replaces any \
guidance meant for a person reading your output (tone, personality, progress commentary, \
final-answer summaries, markdown formatting, file links).
- Work silently between tool calls. End a turn with what whoever asked needs, or nothing. No \
markdown, headings, lists, tables or emojis, and no summary of what you did.
- Your replies, and what goes into crew's tools (task_update, task_complete, review_submit), \
are in the team's voice below.
- What lasts for people is written as the project writes it, whatever the voice: code, \
comments, commit messages, docs, reports and other files; so is anything to the operator.

{}",
        voice_rules(voice)
    )
}

/// How a member writes: everything on the coordination channel is read by
/// another model.
fn writing_to_agents(voice: Voice) -> String {
    format!(
        "
## Writing
Your messages, task_update notes, task_complete results, review comments and decline reasons \
are read by other models: write them in the team's voice.
{}
- Message only when the reader must act or know; one message per need; nothing that needs no \
reply. Type it: status for news that needs nothing back, question for an answer, answer to \
a question (to whoever asked), message for something to do.
- Only the reply you end a turn with is carried over (to whoever asked, and to whoever you \
name with @); text between tool calls reaches nobody. A reply to an answer goes nowhere: to go \
on, ask again.
- Not in the voice: what lasts for people (code, comments, commit messages, docs, reports and \
other files you write), written as the project writes them; and anything to the human operator \
(agent_send to \"operator\"), in plain sentences a person can read cold.
",
        voice_rules(voice)
    )
}

/// How the orchestrator writes: to members in the team's voice, to the
/// operator for a person.
fn writing_as_orchestrator(voice: Voice) -> String {
    format!(
        "
## Writing
Two audiences.
- Members are models. Write to them in the team's voice: agent_send, agent_broadcast, \
agent_set_model notes and task_create descriptions, and the reply you end a turn with when a \
member's question or message started it, which is carried back to them. Task titles stay in \
plain English: the operator reads them on the dashboard.
- A card says what is to be done and how to tell it is done, and points to what exists (a doc's \
path and §, a task id, a message id, a commit) instead of restating it: the member reads those \
itself. Never paste a member's own words (its proposal, its findings) back to it: name the \
message they are in.
{}
- The operator is a person reading cold. operator_ask questions and the session_complete \
summary and decisions are plain, self-contained English sentences with the context needed to \
decide; members report to you in the voice, so put what they found in plain words there. The \
operator also reads your replies in this session when they look at your terminal: keep those \
short and plain.
",
        voice_rules(voice)
    )
}

/// Standing instructions for a worker agent.
pub fn worker_briefing(view: &TeamView<'_>, me: &Agent) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "You are \"{}\", one agent in a team of coding agents coordinated by crew.",
        me.id
    );
    if !me.role.is_empty() {
        let _ = writeln!(out, "Your role: {}.", me.role);
    }
    if let Some(parent) = &me.parent {
        let _ = writeln!(
            out,
            "You are a subagent launched by {parent}: do the one task you are given and report \
back to {parent}. You cannot launch subagents yourself."
        );
    }
    let _ = writeln!(out, "Project: {}", view.project);
    if !view.objective.trim().is_empty() {
        let _ = writeln!(out, "Team objective: {}", view.objective.trim());
    }
    let _ = write!(out, "\n## Team\n{}", roster(view, &me.id));
    out.push_str(&working_style(view));
    let _ = write!(
        out,
        "\n## Coordination\n\
You have a `crew` MCP server. Its tools are the only way to reach your teammates:\n\
{TOOLS_COMMON}\n{TOOLS_WORKER}\n\n\
Your work is on a board of task cards. A [crew] task message names your card by id; \
task_get reads it (the work, the results it builds on, its reviewer), and reads any other card \
by its id. Refer to work by id in the same way (a task, a message, a commit) instead of \
restating it.\n\n\
Talk to teammates directly, as you would to a person; crew carries the words over:\n\
- The reply you end a turn with goes back to the teammate whose question or message started \
it, as your answer. No tool needed.\n\
- To start a conversation, begin a line of that reply with `@<id>:` (`@operator:` for the human \
operator; `@db @lead:` for several, `@all:` for the whole team); everything up to the next such \
line goes to each of them, and their answers come back.\n\
- agent_send is for the rest: a typed message (an artifact, a review request), a broadcast, \
or something to say mid-turn.\n\
If your work affects another agent, or you need something from one, talk to it yourself: \
the orchestrator is not a relay.\n\n\
Every response you make sends your whole conversation to the model again, so each tool call \
that waits for the one before costs that much. Make the calls that do not depend on one \
another together, in one response (a message and a status; a commit and the next read), and \
do not follow a call that settles things (a message sent, a verdict, a status) with a closing \
line that says it again.\n\n\
Messages from the team arrive in this conversation as lines starting with \"[crew]\". \
They come from your teammates or the crew scheduler, not from a human. Act on them.\n"
    );
    out.push_str(&writing_to_agents(view.voice));
    out.push_str(&worker_questions(view, me));
    if view.worktrees && view.vcs == crate::vcs::Kind::Jj {
        let branch = me.branch.as_deref().unwrap_or("your own branch");
        let _ = write!(
            out,
            "\n## Workspace\n\
Your working directory is your own jj workspace, published as branch `{branch}`. Only change \
files inside it. Teammates work in their own workspaces and cannot see your uncommitted files, \
and you cannot see theirs. To share work: git_commit, then send the commit id to whoever \
needs it (agent_send with type \"artifact\"); they apply it with git_cherry_pick. \
Inspect your work with the git_status and git_diff tools, or `jj status`, `jj diff` and \
`jj log`. Plain `git` does not work in this directory. Do not run history-changing jj commands \
(rebase, squash, abandon, edit, and the like), and never on a teammate's commits: use the crew \
git tools so the team has a record.\n"
        );
    } else if view.worktrees {
        let branch = me.branch.as_deref().unwrap_or("your own branch");
        let _ = write!(
            out,
            "\n## Workspace\n\
Your working directory is your own git worktree on branch `{branch}`. Only change files \
inside it. Teammates work in their own worktrees and cannot see your uncommitted files, \
and you cannot see theirs. To share work: git_commit, then send the commit id to whoever \
needs it (agent_send with type \"artifact\"); they apply it with git_cherry_pick. \
Use the crew git tools instead of running `git commit`, `git merge`, `git cherry-pick` \
or `git push` yourself, so the team has a record.\n"
        );
    } else {
        let _ = write!(
            out,
            "\n## Workspace\n\
All agents share one working directory, so crew cannot attribute a commit to one agent: do not commit. \
git_commit, git_merge and git_cherry_pick are refused here. Only touch the files your task is \
about, and tell a teammate (agent_send) before changing anything they own.\n"
        );
    }
    let done = if view.worktrees {
        "git_commit your work, then call task_complete with a short summary of the result"
    } else {
        "call task_complete with a short summary of the result that lists the changed files \
(nothing is committed in a shared checkout)"
    };
    let _ = write!(
        out,
        "\n## Finishing\n\
When your task is done: {done}. crew tells the orchestrator when your task finishes, fails or stalls, \
so do not send it progress or status reports: task_update records progress where it can \
see it. Before parking source changes, checkpoint them with git_commit where that tool is \
available, or record which pending edits belong to this card and keep their ownership clear \
during the next card. git_commit commits the whole worktree, so a later card's commit must \
not silently include parked unfinished work. Your status follows your cards when a turn ends: \
a card in review, or no open card, needs no report. If you must wait for a teammate, ask it (a \
question, or review_request) and end this turn: while that is unanswered crew parks the card as \
waiting on it, with no agent_status. For any other wait, agent_status(\"waiting\", \"<who and \
what>\"). That card stays parked while another independent Ready \
card may arrive. Id-less task calls address your executing card, otherwise your most recently \
started Waiting card (equal starts keep board order). Name a parked card by id when acting \
on it during another task. Incoming messages are retained and crew points you back to parked \
work after the executing card finishes; report waiting again if its answer is still missing. \
To resume a parked card explicitly, finish or park the executing card first, then call \
agent_status(\"working\", task_id: \"<parked id>\"). If it needs no more work, \
task_complete(task_id: \"<parked id>\") finishes it directly.\n"
    );
    out
}

/// How a worker gets a question answered: nobody reads its terminal.
fn worker_questions(view: &TeamView<'_>, me: &Agent) -> String {
    let orchestrator = view
        .agents
        .iter()
        .find(|a| a.orchestrator)
        .map(|a| a.id.as_str());
    let whom = match (&me.parent, orchestrator) {
        (Some(parent), _) => {
            format!("put it to {parent}: agent_send(to: \"{parent}\", type: \"question\")")
        }
        (None, Some(id)) => format!(
            "put it to {id}: agent_send(to: \"{id}\", type: \"question\"). It answers what it can \
and brings in the human operator for what only a human can decide; if its answer does not \
settle the matter, say so and ask it to take the question to the operator"
        ),
        (None, None) => "put it to the human operator: agent_send(to: \"operator\", type: \
\"question\"). They see only that message, so make it complete"
            .to_string(),
    };
    format!(
        "\n## Questions\n\
Nobody watches your terminal: a question written there, or asked through a built-in \
ask-the-user dialog, is never seen and leaves you stuck. Settle what you can yourself or \
with the teammate who owns the area. When something is not yours to decide (a requirement \
that is missing, a preference, a risky or irreversible choice), {whom}. Say what you need to \
know, why, and the options you see. Then carry on with what does not depend on the answer, \
stating the assumption you made, or end the turn if nothing does: crew parks your card while \
the question is open. The answer arrives as a [crew] message.\n"
    )
}

/// Standing instructions for the orchestrator agent.
pub fn orchestrator_briefing(view: &TeamView<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "You are the orchestrator of a team of coding agents coordinated by crew."
    );
    let _ = writeln!(out, "Project: {}", view.project);
    if !view.objective.trim().is_empty() {
        let _ = writeln!(out, "Objective: {}", view.objective.trim());
    }
    let _ = write!(out, "\n## Team\n{}", roster(view, ORCHESTRATOR_ID));
    out.push_str(&working_style(view));
    out.push_str(view.models);
    let _ = write!(
        out,
        "\n## How the team runs\n\
Work is a board of task cards, ordered by their dependencies. A card is written once, with \
task_create; the crew scheduler hands it to its agent by id as soon as its dependencies are \
done, and the agent reads it with task_get. So you never relay task assignments or repeat \
what a card says: refer to work by id (a task, a message, a commit), and change a task with \
task_edit rather than a message. Workers message each other directly — you are not in the \
loop for their conversations and must not act as a relay.\n\n\
You have a `crew` MCP server with these tools:\n\
{TOOLS_COMMON}\n{TOOLS_ORCHESTRATOR}\n\n\
Every response you make sends your whole conversation to the model again, so each tool call \
that waits for the one before costs that much. Make the calls that do not depend on one \
another together, in one response (the cards of a plan, a card and its assignment), and do not \
follow a call that settles things with a closing line that says it again. To learn where a \
member stands, agent_peek(id): a message asking it wakes it for a turn over its own whole \
conversation. Message a member when you need something from it, not to check on it.\n\n\
## Your job\n\
1. Staff. You decide who is on the team. If it has no members yet, work out which roles \
the objective needs and add them with agent_add (2 to 10 members, each with a distinct \
responsibility; pick each member's model for its role). Whoever is on the team when your first \
turn with a plan ends is the founding team: it is saved and reused on the next run, and you \
reshape it with agent_remove when a role is no longer needed. A member you add after that is \
temporary, for extra hands or a passing need: it is not saved, and crew takes it off the team \
once its tasks are done and you have been told so. To keep one, give it more work in that turn.\n\
2. Plan. If the task graph is empty, decompose the objective into tasks with task_create: \
one clear deliverable each, assigned to the member whose role fits, with depends_on set \
only when the card cannot start without another task's completed, approved output. Create \
independent cards immediately so idle roles can work while others implement or await review. \
Design, implementation and test preparation can overlap: split an initial interface or \
design contract from later polish, and test preparation from final verification, with \
dependencies on the specific deliverables each needs. Members share intermediate artifacts \
directly; keep required approval gates on work that uses them. A dependency waits for the \
whole task, including its review, so do not use it merely for phase order or to wait for an \
unrelated worker. The scheduler already gives each member one task at a time and frees the \
author while a reviewer works; no extra dependency is needed for that.\n\
3. Supervise. Status lines starting with \"[crew]\" tell you when tasks finish, fail or \
stall. When something is stuck, find out why (tasks_list, agents_list) and fix it: \
message the agent, reassign the task, or retry it. When a role is idle, inspect its cards: \
Queued waits for dependencies; Ready waits for an available owner. A Working or started \
Blocked card occupies that owner. A Waiting card is parked: its owner can take independent \
Ready work once the current turn ends, then resume parked work when its messages arrive. \
Split useful independent preparation from \
a broad deliverable, or give it to another free member, while preserving its required \
inputs and reviews. Do not bypass dependencies or abandon a waiting task just to keep a \
member busy. A member whose session was moved to \
another model by its runtime (its model's weekly window used up, or overloaded) keeps \
working, on a model nobody chose for its role; you are told, with the accounts' usage. Decide \
whether that model will do for the work; if not, agent_set_model it onto one you choose with \
revised instructions, or add a member on an unaffected model and move the task.\n\
4. Answer. Members bring you the questions they cannot settle (type question). Answer \
from the objective, the code and the team's work whenever you can, and say what you \
assumed. What only a human can decide (a preference, a requirement nobody stated, a risky \
or irreversible choice, an objective you cannot make sense of) goes to the operator with \
operator_ask: one self-contained question with the context to decide it, concrete options \
when there are any, and asked_by set when you pass a member's question on. Do not ask what \
you can decide yourself, and keep the rest of the team moving while you wait. Nobody \
watches your terminal: never ask there or through a built-in ask-the-user dialog.\n\
5. Mind the context windows. agents_list shows how full each window is (\"ctx NN%\"), and \
you are told when one is getting full, your own included. A full window is compacted by the \
runtime at whatever moment that happens, with no say in what is kept. Better: at a natural \
break (between tasks, after a commit, while an agent waits) queue it yourself with \
agent_compact(id, keep), naming what the summary must keep. For your own window keep the plan, \
who is doing what, decisions made and questions still open. Do not compact an agent in the \
middle of delicate work, or one whose window has plenty of room.\n\
6. Conclude. When every task is done you are told so; call session_complete with a \
concise final report: what was built, by whom, and the commits or artifacts. Whatever is left \
for the operator to decide goes in its `decisions`, one per entry: each is then asked of them and \
the dashboard shows that the team waits for their answer. A decision that is only mentioned in \
the report or in your terminal is not seen as a request.\n\n\
Do not do the workers' tasks yourself and do not edit files.\n"
    );
    out.push_str(&writing_as_orchestrator(view.voice));
    out
}

/// The first prompt of an agent that has nothing to do yet.
pub fn standby_prompt(agent: &Agent) -> String {
    if agent.orchestrator {
        "[crew] The team is starting. Reply with one short line; you will receive [crew] \
status lines as the work progresses."
            .to_string()
    } else {
        format!(
            "[crew] You are online as {}. Nothing is assigned to you yet. Reply with one \
short line to confirm you are ready, then wait: tasks and messages will arrive as [crew] lines.",
            agent.id
        )
    }
}

/// Kick-off for the orchestrator, listing the declared plan (possibly empty)
/// and, while it staffs the team, the accounts' usage (`usage`: what
/// `usage_report` says; empty when nothing is known).
pub fn orchestrator_kickoff(tasks: &[Task], members: usize, usage: &str) -> String {
    let usage = if usage.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\n{}\nChoose models with this in mind (usage_status says it again at any time).",
            usage.trim()
        )
    };
    if members == 0 {
        return format!(
            "[crew] You are online and the team has no members yet. Decide which \
members the objective needs and add each with agent_add (id, role, runtime), then decompose \
the objective into tasks with task_create. The scheduler starts each task as soon as its \
dependencies are done, and the roster you choose is saved for future runs.{usage}"
        );
    }
    if tasks.is_empty() {
        return format!(
            "[crew] The team is online and the task graph is empty. Check the roster \
(agents_list) and adjust it if the objective needs different members (agent_add / \
agent_remove), then decompose the objective into tasks with task_create; the scheduler \
starts each one as soon as its dependencies are done.{usage}"
        );
    }
    let mut out = String::from(
        "[crew] The team is online. The plan below is already loaded and the scheduler is \
dispatching it; you do not need to assign anything.\n\n",
    );
    for task in tasks {
        let who = task.assigned_agent.as_deref().unwrap_or("unassigned");
        let deps = if task.dependencies.is_empty() {
            String::new()
        } else {
            format!(" (after {})", task.dependencies.join(", "))
        };
        let _ = writeln!(out, "- {} → {who}: {}{deps}", task.id, task.title);
    }
    out.push_str(
        "\nReply with one short line. You will be told when tasks finish, fail or stall, \
and when everything is done.",
    );
    out.push_str(&usage);
    out
}

/// The work style's model fallbacks, as the orchestrator's briefing gives
/// them; empty when it names none.
pub fn fallbacks_section(fallbacks: &std::collections::BTreeMap<String, Vec<String>>) -> String {
    if fallbacks.is_empty() {
        return String::new();
    }
    let rules: Vec<String> = fallbacks
        .iter()
        .map(|(model, instead)| format!("{model} → {}", instead.join(", then ")))
        .collect();
    format!(
        "\nWhen a model cannot be used for a while (its usage window used up), crew moves the \
members on it by itself, as this team's work style says, and back once the window resets: {}. \
You are told when it happens; there is nothing to decide, and the roster keeps each member's own \
model.\n",
        rules.join("; ")
    )
}

/// What a session that takes over from one on another CLI is told.
pub struct Handoff<'a> {
    /// The agent, as it was before the move.
    pub agent: &'a Agent,
    /// What it ran on and runs on now: `Claude Code on Fable 5.1`.
    pub from: &'a str,
    pub to: &'a str,
    /// Who moved it, and what they want it to know.
    pub who: &'a str,
    pub note: Option<&'a str>,
    pub tasks: &'a [Task],
    pub agents: &'a [Agent],
    /// The old session's last replies, oldest first.
    pub last_words: &'a [String],
    /// The version control backend of the workspace.
    pub vcs: crate::vcs::Kind,
}

/// The first prompt of a session that takes over from one on another CLI:
/// a new session has none of the old one's conversation, so it is told
/// where the work stands and what its predecessor said last.
pub fn handoff(h: &Handoff<'_>) -> String {
    let me = h.agent;
    // Paragraphs, joined by a blank line.
    let mut parts: Vec<String> = vec![if me.orchestrator {
        format!(
            "[crew] You take over as this team's orchestrator. The role ran on {} until now; {} \
moved it to {}. This is a fresh session: you have the team and the plan, not your \
predecessor's conversation.",
            h.from, h.who, h.to
        )
    } else {
        format!(
            "[crew] You take over the work of {}. It ran on {} until now; {} moved it to {}. This \
is a fresh session: you do not have the previous session's conversation.",
            me.id, h.from, h.who, h.to
        )
    }];
    if let Some(note) = h.note {
        parts.push(format!("From {}: {note}", h.who));
    }
    if me.orchestrator {
        let mut plan = String::from("Where the work stands:");
        if h.tasks.is_empty() {
            plan.push_str("\n- the task graph is empty");
        }
        for task in h.tasks {
            let who = task.assigned_agent.as_deref().unwrap_or("unassigned");
            let _ = write!(
                plan,
                "\n- {} → {who} [{}]: {}",
                task.id,
                task.status.as_str(),
                task.title
            );
        }
        parts.push(plan);
        let mut team = String::from("The team:");
        let members: Vec<&Agent> = h.agents.iter().filter(|a| !a.orchestrator).collect();
        if members.is_empty() {
            team.push_str("\n- no members yet");
        }
        for member in members {
            let model = member.model_name.as_deref().or(member.model.as_deref());
            let cli = crate::models::runtime_label(member.runtime);
            let runs = match model {
                Some(model) => format!("{cli}, {model}"),
                None => cli.to_string(),
            };
            let doing = if member.activity.is_empty() {
                String::new()
            } else {
                format!(
                    " — {}",
                    crew_core::util::truncate_chars(
                        &crew_core::util::one_line(&member.activity),
                        80
                    )
                )
            };
            let _ = write!(
                team,
                "\n- {} ({runs}): {}{doing}",
                member.id,
                member.status.as_str()
            );
        }
        parts.push(team);
    } else {
        let branch = me
            .branch
            .as_deref()
            .map(|b| format!(", branch {b}"))
            .unwrap_or_default();
        let mut work = format!(
            "Your working directory ({}{branch}) holds the work so far, uncommitted changes \
included: read {inspect} before you go on, and do not redo what is done.",
            me.workdir,
            inspect = if h.vcs == crate::vcs::Kind::Jj {
                "jj status and jj diff (or the git_status and git_diff tools)"
            } else {
                "git status and git diff"
            }
        );
        let mine = h.tasks.iter().find(|t| {
            t.assigned_agent.as_deref() == Some(me.id.as_str()) && !t.status.is_terminal()
        });
        if let Some(task) = mine {
            let _ = write!(
                work,
                " Your task is {} (\"{}\"); it is handed to you again, and task_get(\"{}\") reads its card.",
                task.id, task.title, task.id
            );
        }
        parts.push(work);
    }
    if !h.last_words.is_empty() {
        let quoted: Vec<String> = h
            .last_words
            .iter()
            .map(|words| {
                words
                    .lines()
                    .map(|line| format!("> {line}").trim_end().to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect();
        let whose = if me.orchestrator {
            "your predecessor"
        } else {
            "the previous session"
        };
        parts.push(format!(
            "The last thing {whose} wrote:\n{}",
            quoted.join("\n>\n")
        ));
    }
    parts.push(
        if me.orchestrator {
            "Carry on from here: agent_inbox holds the messages that came in, agents_list and \
tasks_list the details. The scheduler keeps dispatching the plan."
        } else {
            "Carry on from here; agent_inbox holds the messages that came in."
        }
        .to_string(),
    );
    parts.join("\n\n")
}

/// The last replies of a transcript, oldest first, each cut short: what a
/// session that takes over is told its predecessor said last.
pub fn last_words(entries: &[crew_core::types::TranscriptEntry]) -> Vec<String> {
    const REPLIES: usize = 3;
    const REPLY_CHARS: usize = 600;
    let mut words: Vec<String> = entries
        .iter()
        .rev()
        .filter(|e| e.kind == crew_core::types::TranscriptKind::Reply)
        .map(|e| e.text.trim())
        .filter(|t| !t.is_empty())
        .take(REPLIES)
        .map(|t| crew_core::util::truncate_chars(t, REPLY_CHARS))
        .collect();
    words.reverse();
    words
}

/// The body of the `task` message that hands a task to its agent: which
/// card on the board is theirs, by id. What the card says (the work, the
/// results it builds on, its reviewer) is read with task_get, not sent.
pub fn task_dispatch(task: &Task) -> String {
    let more = (!task.description.is_empty() && task.description != task.title)
        || !task.dependencies.is_empty();
    if more {
        format!(
            "Task {} is yours: \"{}\". task_get(\"{}\") reads its card.",
            task.id, task.title, task.id
        )
    } else {
        format!("Task {} is yours: \"{}\".", task.id, task.title)
    }
}

fn clip(body: &str, id: &str) -> String {
    if body.chars().count() <= NUDGE_BODY_CHARS {
        return body.to_string();
    }
    let mut out: String = body.chars().take(NUDGE_BODY_CHARS).collect();
    let _ = write!(
        out,
        "\n… (truncated: agent_inbox(id: {id:?}) reads the full text)"
    );
    out
}

pub(crate) fn nudge_is_complete(messages: &[Message]) -> bool {
    messages
        .iter()
        .all(|message| message.body.chars().count() <= NUDGE_BODY_CHARS)
}

pub(crate) struct NudgeSpan {
    pub id: String,
    pub start: usize,
    pub end: usize,
}

/// Byte spans come from rendering, never by parsing headers out of a body.
pub(crate) fn format_nudge_with_spans(messages: &[Message]) -> (String, Vec<NudgeSpan>) {
    let mut spans = Vec::new();
    let mut text = match messages {
        [] => String::new(),
        [m] => {
            let text = format!(
                "[crew] Message from {} (type {}, id {}):\n\n{}",
                m.from,
                m.kind,
                m.id,
                clip(&m.body, &m.id)
            );
            spans.push(NudgeSpan {
                id: m.id.clone(),
                start: 0,
                end: text.len(),
            });
            text
        }
        many => {
            let mut out = format!("[crew] {} new messages:\n", many.len());
            for (i, m) in many.iter().enumerate() {
                let start = out.len();
                let _ = write!(
                    out,
                    "\n({}) from {} · type {} · id {}\n{}",
                    i + 1,
                    m.from,
                    m.kind,
                    m.id,
                    clip(&m.body, &m.id)
                );
                spans.push(NudgeSpan {
                    id: m.id.clone(),
                    start,
                    end: out.len(),
                });
                out.push('\n');
            }
            out
        }
    };
    if !messages.is_empty() {
        text.push_str("\n\n[crew] Messages delivered here in full may be marked read; truncated messages stay unread; fetch only the id named above.");
    }
    (text, spans)
}

/// Phrase bus messages as a prompt for an interactive agent session.
pub fn format_nudge(messages: &[Message]) -> String {
    format_nudge_with_spans(messages).0
}

/// Who sent the bus messages in a prompt `format_nudge` wrote, by message
/// id: `(sender, id)`, in order. The way back from what a session was given
/// to whom it concerns.
pub fn nudge_senders(prompt: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut add = |from: &str, id: &str| {
        let id = id.trim_end_matches([':', ')']);
        if crew_core::util::is_valid_id(from)
            && !id.is_empty()
            && !id.contains(char::is_whitespace)
            && !found.iter().any(|(_, seen)| seen == id)
        {
            found.push((from.to_string(), id.to_string()));
        }
    };
    // `[crew] Message from lead (type review_request, id m-1):`
    for (at, mark) in prompt.match_indices("[crew] Message from ") {
        let rest = &prompt[at + mark.len()..];
        if let Some((from, rest)) = rest.split_once(" (type ") {
            if let Some((id, _)) = rest
                .split_once(", id ")
                .and_then(|(_, rest)| rest.split_once(')'))
            {
                add(from, id);
            }
        }
    }
    // `(2) from lead · type review_request · id m-1`
    for (at, mark) in prompt.match_indices(") from ") {
        let rest = &prompt[at + mark.len()..];
        if let Some((from, rest)) = rest.split_once(" · type ") {
            if let Some((_, rest)) = rest.split_once(" · id ") {
                add(from, rest.split_whitespace().next().unwrap_or_default());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::{AgentStatus, MessageType, RuntimeKind, TaskStatus};
    use serde_json::Value;

    fn agent(id: &str, role: &str, orchestrator: bool) -> Agent {
        Agent {
            id: id.into(),
            name: id.into(),
            role: role.into(),
            task: None,
            status: AgentStatus::Idle,
            activity: String::new(),
            runtime: RuntimeKind::Claude,
            model: None,
            workdir: "/w".into(),
            branch: Some(format!("agents/{id}")),
            backend: "pty".into(),
            backend_ref: None,
            pid: None,
            session_id: None,
            orchestrator,
            parent: None,
            subagents: 0,
            temporary: false,
            delivery: String::new(),
            started_at: None,
            last_activity_at: 0,
            progress: None,
            paused: false,
            block_kind: None,
            limit: None,
            context: None,
            compact_pending: false,
            compactions: 0,
            running_model: None,
            model_name: None,
            effort: None,
            fallback: None,
            exit_code: None,
            restarts: 0,
            inbox_unread: 0,
            inbox_total: 0,
            outbox_total: 0,
            usage: None,
            error: None,
            toolbox: None,
        }
    }

    fn message(from: &str, kind: MessageType, body: &str) -> Message {
        Message {
            id: "m-1".into(),
            from: from.into(),
            to: "me".into(),
            kind,
            body: body.into(),
            timestamp: 0,
            correlation_id: None,
            reply_to: None,
            metadata: Value::Null,
        }
    }

    #[test]
    fn a_jj_workspace_is_described_as_one_and_git_is_not_promised() {
        let agents = vec![
            agent("orchestrator", "", true),
            agent("claude-3", "ledger", false),
        ];
        let briefing = |vcs| {
            worker_briefing(
                &TeamView {
                    project: "demo",
                    objective: "Build it",
                    style: None,
                    instructions: "",
                    models: "",
                    agents: &agents,
                    worktrees: true,
                    vcs,
                    voice: Voice::default(),
                },
                &agents[1],
            )
        };
        let jj = briefing(crate::vcs::Kind::Jj);
        assert!(
            jj.contains("your own jj workspace, published as branch"),
            "{jj}"
        );
        assert!(jj.contains("Plain `git` does not work"), "{jj}");
        assert!(jj.contains("history-changing jj commands"), "{jj}");
        let git = briefing(crate::vcs::Kind::Git);
        assert!(git.contains("your own git worktree"), "{git}");
        assert!(!git.contains("jj"), "{git}");
    }

    #[test]
    fn members_write_to_each_other_in_the_teams_voice_and_to_people_plainly() {
        let agents = vec![
            agent("orchestrator", "", true),
            agent("claude-3", "ledger", false),
        ];
        let view = |voice| TeamView {
            project: "demo",
            objective: "Build it",
            style: None,
            instructions: "",
            models: "",
            agents: &agents,
            worktrees: true,
            vcs: crate::vcs::Kind::Git,
            voice,
        };
        let member = worker_briefing(&view(Voice::default()), &agents[1]);
        assert!(
            member.contains("## Writing") && member.contains("Voice: ultracave. Fragments"),
            "{member}"
        );
        assert!(
            member.contains(
                "Not in the voice: what lasts for people (code, comments, commit messages"
            ),
            "{member}"
        );
        let classical = worker_briefing(&view(Voice::Megacave), &agents[1]);
        assert!(
            classical.contains("Voice: megacave. Write in 文言")
                && !classical.contains("Voice: ultracave"),
            "{classical}"
        );
        let lead = orchestrator_briefing(&view(Voice::default()));
        assert!(
            lead.contains("Members are models. Write to them in the team's voice"),
            "{lead}"
        );
        assert!(
            lead.contains("plain, self-contained English sentences"),
            "{lead}"
        );
        assert!(lead.contains("Voice: ultracave."), "{lead}");
        // In the system prompt, in place of the runtime's guidance for a person.
        let tone = member_tone(Voice::Caveman);
        assert!(
            tone.starts_with("# Tone and style\nYou are a member of a team")
                && tone.ends_with(voice_rules(Voice::Caveman))
        );
    }

    #[test]
    fn worker_briefing_names_the_team_and_the_direct_channel() {
        let agents = vec![
            agent("orchestrator", "", true),
            agent("claude-3", "ledger", false),
            agent("claude-5", "db", false),
        ];
        let view = TeamView {
            project: "demo",
            objective: "Build it",
            style: None,
            instructions: "",
            models: "",
            agents: &agents,
            worktrees: true,
            vcs: crate::vcs::Kind::Git,
            voice: Voice::default(),
        };
        let text = worker_briefing(&view, &agents[1]);
        assert!(!text.contains("How this team works"));
        // A work style's instructions reach every member, the orchestrator included.
        let styled = TeamView {
            style: Some("research"),
            instructions: "Cite a source for every claim.\n",
            ..view
        };
        for text in [
            worker_briefing(&styled, &agents[1]),
            orchestrator_briefing(&styled),
        ] {
            assert!(
                text.contains(
                    "## How this team works (research)\nCite a source for every claim.\n"
                ),
                "{text}"
            );
        }
        assert!(text.contains("You are \"claude-3\""));
        assert!(text.contains("- claude-3 (you) — ledger"));
        assert!(text.contains("- claude-5 — db"));
        assert!(text.contains("the orchestrator is not a relay"));
        // Replies are carried over; a conversation starts with an @line.
        assert!(
            text.contains("goes back to the teammate whose question or message started it")
                && text.contains("begin a line of that reply with `@<id>:`"),
            "{text}"
        );
        // What is not the agent's to decide goes to the orchestrator, which
        // brings in the human; nobody reads the agent's own terminal.
        assert!(
            text.contains("## Questions\nNobody watches your terminal"),
            "{text}"
        );
        assert!(
            text.contains("- agent_compact(keep?) — compact your own conversation"),
            "{text}"
        );
        assert!(
            text.contains("agent_send(to: \"orchestrator\", type: \"question\")"),
            "{text}"
        );
        assert!(!text.contains("operator_ask"), "{text}");
        // A subagent asks the agent that launched it; a team without an
        // orchestrator asks the operator directly.
        let mut sub = agent("claude-3-1", "helper", false);
        sub.parent = Some("claude-3".into());
        let text_sub = worker_briefing(&view, &sub);
        assert!(
            text_sub
                .contains("put it to claude-3: agent_send(to: \"claude-3\", type: \"question\")"),
            "{text_sub}"
        );
        let flat = worker_briefing(
            &TeamView {
                agents: &agents[1..],
                ..view
            },
            &agents[1],
        );
        assert!(
            flat.contains("agent_send(to: \"operator\", type: \"question\")"),
            "{flat}"
        );
        assert!(text.contains("agents/claude-3"));
        let shared = worker_briefing(
            &TeamView {
                worktrees: false,
                vcs: crate::vcs::Kind::Git,
                voice: Voice::default(),
                ..view
            },
            &agents[1],
        );
        assert!(shared.contains("share one working directory"));
        // Nobody can commit in a shared checkout: the briefing says so and
        // finishing means task_complete with the changed files.
        assert!(shared.contains("do not commit"), "{shared}");
        assert!(!shared.contains("git_commit your work"), "{shared}");
        assert!(shared.contains("changed files"), "{shared}");
        assert!(text.contains("git_commit your work"));
    }

    #[test]
    fn orchestrator_briefing_forbids_relaying() {
        let agents = vec![agent("orchestrator", "", true), agent("a", "x", false)];
        let text = orchestrator_briefing(&TeamView {
            project: "demo",
            objective: "",
            style: None,
            instructions: "",
            models: "\n## Models\n- fable (Claude Code): strong.\n",
            agents: &agents,
            worktrees: true,
            vcs: crate::vcs::Kind::Git,
            voice: Voice::default(),
        });
        assert!(text.contains("must not act as a relay"));
        // The model catalog sits between the roster and the working rules.
        let models = text.find("## Models").expect("the catalog section");
        assert!(
            text.find("## Team").unwrap() < models
                && models < text.find("## How the team runs").unwrap()
        );
        assert!(text.contains("agent_add"));
        assert!(orchestrator_kickoff(&[], 0, "").contains("no members yet"));
        assert!(orchestrator_kickoff(&[], 3, "").contains("task graph is empty"));
        // While it staffs the team, the orchestrator hears what each model has left.
        let usage = "Usage now: Claude Code Fable week 100%.\nOut until their window resets — …\n- fable (Fable week used up): gpt-6-astra, then opus";
        let kickoff = orchestrator_kickoff(&[], 0, usage);
        assert!(kickoff.ends_with(&format!("for future runs.\n\n{usage}\nChoose models with this in mind (usage_status says it again at any time).")), "{kickoff}");
        assert!(!orchestrator_kickoff(&[], 0, "  ").contains("Usage"));
        assert!(text.contains("session_complete"));
        assert!(text.contains("- orchestrator (you)"));
        // Members' questions it cannot settle go to the human, through crew.
        assert!(
            text.contains("- operator_ask(question, options?, asked_by?)"),
            "{text}"
        );
        // It watches the context windows and compacts at a good moment.
        assert!(text.contains("- agent_compact(id?, keep?)"), "{text}");
        assert!(
            text.contains("5. Mind the context windows.") && text.contains("6. Conclude."),
            "{text}"
        );
        assert!(
            text.contains("4. Answer. Members bring you the questions they cannot settle"),
            "{text}"
        );
        assert!(
            text.contains("never ask there or through a built-in ask-the-user dialog"),
            "{text}"
        );
    }

    #[test]
    fn nudges() {
        assert_eq!(format_nudge(&[]), "");
        let one = format_nudge(&[message("claude-3", MessageType::Question, "column type?")]);
        assert!(one.starts_with("[crew] Message from claude-3 (type question, id m-1):"));
        assert!(one.contains("column type?"));
        // How to answer is in the briefing, not in every message.
        assert!(!one.contains("Answer claude-3 directly"));
        let two = format_nudge(&[
            message("a", MessageType::Message, "one"),
            message("b", MessageType::Artifact, "two"),
        ]);
        assert!(two.starts_with("[crew] 2 new messages:"));
        assert!(two.contains("(2) from b · type artifact"));
        let long = format_nudge(&[message("a", MessageType::Message, &"x".repeat(9000))]);
        assert!(long.contains("truncated"));
        assert!(long.len() < 5000);
    }

    #[test]
    fn the_senders_of_a_nudge_are_read_back_from_its_text() {
        let one = format_nudge(&[message(
            "lead",
            MessageType::ReviewRequest,
            "Please review: a (b) from c · type d",
        )]);
        assert_eq!(
            nudge_senders(&one),
            [("lead".to_string(), "m-1".to_string())]
        );
        let mut second = message("scheduler", MessageType::Status, "task done");
        second.id = "m-2".into();
        let two = format_nudge(&[
            message("lead", MessageType::Message, "one (x) from me"),
            second,
        ]);
        let expected = [
            ("lead".to_string(), "m-1".to_string()),
            ("scheduler".to_string(), "m-2".to_string()),
        ];
        assert_eq!(nudge_senders(&two), expected);
        // Typed into a terminal, the lines are joined; the senders are the same.
        assert_eq!(nudge_senders(&two.replace('\n', "  ")), expected);
        assert!(nudge_senders("fix the parser").is_empty());
    }

    #[test]
    fn a_dispatch_names_the_card_and_leaves_its_contents_on_the_board() {
        let mut dep = Task {
            epic: None,
            id: "db".into(),
            title: "schema".into(),
            description: String::new(),
            assigned_agent: Some("claude-5".into()),
            status: TaskStatus::Done,
            dependencies: vec![],
            created_at: 0,
            started_at: None,
            completed_at: None,
            artifacts: vec![crew_core::Artifact {
                kind: "commit".into(),
                value: "a91ec21".into(),
                description: String::new(),
                agent: Some("claude-5".into()),
                created_at: 0,
            }],
            reviewer: None,
            result: Some("added ledger table".into()),
            progress: None,
            note: None,
            timeout_secs: None,
        };
        let mut task = dep.clone();
        task.id = "ledger".into();
        task.title = "implement ledger".into();
        task.description = "Write ledger.rs".into();
        task.reviewer = Some("claude-2".into());
        dep.title = "schema".into();
        assert_eq!(
            task_dispatch(&task),
            "Task ledger is yours: \"implement ledger\". task_get(\"ledger\") reads its card."
        );
        // A card that says no more than its title is all in the dispatch.
        task.description = task.title.clone();
        assert_eq!(
            task_dispatch(&task),
            "Task ledger is yours: \"implement ledger\"."
        );
        // What it builds on is on the card, not in the dispatch.
        task.dependencies = vec![dep.id.clone()];
        assert!(task_dispatch(&task).ends_with("task_get(\"ledger\") reads its card."));
        // How to finish is in the briefing, which knows whether the checkout is
        // shared (nothing is committed there): the dispatch never says to commit.
        assert!(!task_dispatch(&task).contains("git_commit"));
    }

    #[test]
    fn a_session_on_another_cli_is_told_where_the_work_stands_and_what_was_said_last() {
        use crew_core::types::{TranscriptEntry, TranscriptKind};
        let task = |id: &str, who: &str, status: TaskStatus| Task {
            epic: None,
            id: id.into(),
            title: format!("{id} the thing"),
            description: String::new(),
            assigned_agent: Some(who.into()),
            status,
            dependencies: vec![],
            created_at: 0,
            started_at: None,
            completed_at: None,
            artifacts: vec![],
            reviewer: None,
            result: None,
            progress: None,
            note: None,
            timeout_secs: None,
        };
        let tasks = vec![
            task("design", "design", TaskStatus::Working),
            task("review", "skeptic", TaskStatus::Queued),
        ];
        let mut design = agent("design", "designer", false);
        design.status = AgentStatus::Working;
        design.activity = "Edit spec.md".into();
        design.model_name = Some("Fable 5.1".into());
        design.workdir = "/w/.crew/worktrees/design".into();
        design.branch = Some("agents/design".into());
        let orchestrator = agent(ORCHESTRATOR_ID, "orchestrator", true);
        let agents = vec![orchestrator.clone(), design.clone()];
        let entry = |kind, text: &str| TranscriptEntry {
            kind,
            at: None,
            text: text.into(),
        };
        let transcript = vec![
            entry(TranscriptKind::Reply, "one"),
            entry(TranscriptKind::Prompt, "do it"),
            entry(TranscriptKind::Reply, "two\nlines"),
            entry(TranscriptKind::Tool, "Bash ls"),
            entry(TranscriptKind::Reply, "three"),
            entry(TranscriptKind::Reply, "four"),
        ];
        let last = last_words(&transcript);
        assert_eq!(
            last,
            ["two\nlines", "three", "four"],
            "the last three replies, oldest first"
        );

        let text = handoff(&Handoff {
            agent: &orchestrator,
            from: "Claude Code on Fable 5.1",
            to: "Codex on GPT 6 Astra",
            who: "operator",
            note: Some("Keep the plan."),
            tasks: &tasks,
            agents: &agents,
            last_words: &last,
            vcs: crate::vcs::Kind::Git,
        });
        assert_eq!(
            text,
            "[crew] You take over as this team's orchestrator. The role ran on Claude Code on Fable 5.1 \
until now; operator moved it to Codex on GPT 6 Astra. This is a fresh session: you have the team and \
the plan, not your predecessor's conversation.\n\nFrom operator: Keep the plan.\n\nWhere the work \
stands:\n- design → design [working]: design the thing\n- review → skeptic [queued]: review the \
thing\n\nThe team:\n- design (Claude Code, Fable 5.1): working — Edit spec.md\n\nThe last thing your \
predecessor wrote:\n> two\n> lines\n>\n> three\n>\n> four\n\nCarry on from here: agent_inbox holds \
the messages that came in, agents_list and tasks_list the details. The scheduler keeps dispatching \
the plan."
        );

        let text = handoff(&Handoff {
            agent: &design,
            from: "Claude Code on Fable 5.1",
            to: "Codex on its default model",
            who: "operator",
            note: None,
            tasks: &tasks,
            agents: &agents,
            last_words: &[],
            vcs: crate::vcs::Kind::Git,
        });
        assert_eq!(
            text,
            "[crew] You take over the work of design. It ran on Claude Code on Fable 5.1 until now; \
operator moved it to Codex on its default model. This is a fresh session: you do not have the \
previous session's conversation.\n\nYour working directory (/w/.crew/worktrees/design, branch \
agents/design) holds the work so far, uncommitted changes included: read git status and git diff \
before you go on, and do not redo what is done. Your task is design (\"design the thing\"); it is \
handed to you again, and task_get(\"design\") reads its card.\n\nCarry on from here; agent_inbox holds the \
messages that came in."
        );
    }
}
