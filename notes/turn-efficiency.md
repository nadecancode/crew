# Turn efficiency

Operator request (w-e08e0c30759a45de, item 3): find and remove wasted turns
and tool calls in member sessions. The operator's example: a Codex member is
given `[crew] Message from scheduler …` with the whole body in its prompt,
then calls `crew.agent_inbox` twice and gets `No new messages` both times.

This note measures four patterns across this team's sessions. For each one
it traces the crew text or mechanism that causes it and gives a fix, an owner
and a regression test. The measurements select candidates by tool names and
message wrappers; they show where turns and calls are likely to be spent for
nothing, not proven waste, and the requests and costs they associate are
upper bounds, not savings. Three fixes are made here: the wording of the
`agent_inbox`, `agent_send` and `agent_broadcast` tools. One more, sending
landing news as status, is already applied by the orchestrator. The rest are
proposed cards, because they touch files that other cards own.

## Summary

Measured over the records up to 2026-10-08T09:59:18Z (`--until`, so the run
can be repeated with the same result): 49 Claude Code sessions (18,591
requests, 2,030 turns) and 45 Codex sessions (10,452 requests, 3,692 turns).
18 turns were still open at that cutoff and are counted as partial. There
were no pi sessions in the cohort.

Token counts are input tokens of the requests associated with each pattern,
nearly all of them cache reads; see Method for how requests are associated.
Dollar figures are list prices for the input side of Claude requests only,
since Codex models have no listed price. Every Claude request in this cohort
reported its cache-write TTL split, so these figures are exact rather than
ranges. The patterns overlap, so their rows must not be added together.

| Pattern | Candidates | Associated requests (upper bound) | Input tokens | Claude list cost (upper bound) | Fix | State |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| 1. `agent_inbox` that returns nothing, or only messages already delivered in full, within 3 responses of a delivery | 675 of 1,123 | 387 | 103.4 M | $1.75 | 1a wording; 1b mark read on delivery; 1c notice wording | 1a made here; 1b, 1c proposed |
| 1. All empty `agent_inbox` calls | 946 (945 Codex) | 344 | 94.6 M | none (Codex) | as above | |
| 1. `agent_inbox` returning only messages already delivered in full | 231 | 81 | 18.6 M, plus 859,372 result bytes | $2.61 | 1b | proposed |
| 2. Byte-identical repeated reads | 9 | 0 (all bundled) | 196,237 result bytes | — | none needed | |
| 2. `tasks_list` result size | 137 calls, 1.78 MB | — | p90 90,522 B on Codex, largest 118,535 B | — | 2a explicit compact board view | proposed |
| 3. Turns started by a notice or status message that made only status calls or crew reads | 530 of 1,380 | 872 | 239.8 M (8.6 M cache write) | $111.14 | 3a–3f | 3a applied; rest proposed |
| 3. Replies crew may forward to the sender as an answer or message | 492 | 492 | 159.4 M | $114.73 | 3a | applied for landing news |
| 4. Crew reads in turns started by a delivered message | 3,126 reads in 4,773 turns | 1,434 | 362.0 M | $30.16 | follows from 1 | |

Status-only turns of every origin come to 1,329 turns, 1,998 requests, 572.8 M
input tokens and $304.64 for the Claude share. Many of these are legitimate:
a member parks a card, or reports a result as status. The rows above count
only the turns whose cause is traced in this note.

## Method

`scripts/cache/turns.mjs` is a read-only sibling of runtime's
`scripts/cache/baseline.mjs`. It reuses `baseline.mjs`'s `collect()` for the
cohort, the agent ids and the per-request usage. It then re-reads each native
session file, up to the size `baseline.mjs` read, to get tool calls, tool
results and delivered inputs. It writes `summary.json`, `summary.md` and
`examples.json` and refuses to write into a session directory.
`scripts/cache/turns.test.mjs` pins its classifiers. The full report from
this run is in `notes/turn-efficiency/summary.md`.

How a candidate is associated with requests:

- **Alone.** A candidate call is "alone" when every tool call answered by
  the next model response is `agent_inbox`, `agent_status` or
  `task_update`. It is then associated with that whole next request, the
  one the native session records between that response and the one after
  it. This is an upper bound: the same request may also carry useful
  reasoning.
- **Bundled.** A call that ran beside other real work, for example inside a
  Codex `exec` script with `exec_command`, is associated only with its
  result bytes.
- **Turns.** A candidate turn is associated with every request in it.
- **Folded messages.** A message folded into a turn that is already running
  counts as a delivery but starts no turn.
- **Delivered in full.** A message counts as already delivered only when its
  whole body reached the session: the id in a `[crew] Message from …`
  header with a body, or an entry of a `[crew] N new messages:` batch,
  without crew's `… (truncated` suffix. An id that a notice only mentions
  is a reference, not a delivery.
- **Status-only.** "Status-only" means the turn's only tool calls were
  `agent_status`, `task_update` or `agent_send` with type `status`. A turn with
  no tool calls also counts when its reply is 300 characters or shorter.
- **Read-only.** "Read-only" adds crew reads (`agent_inbox`, `tasks_list`,
  `task_get`, `agents_list`) to the status calls.
- **Cost.** Input-side list price per request. A write with no TTL split is
  priced as a range between the 5-minute and 1-hour write prices; a request
  without usage is "usage unknown", never $0; a model without a price is
  "no price".
- **Window.** `--since` and `--until` select the calls and turns measured
  and the requests associated with them. Earlier records still count as
  context: a message delivered before the window makes a read inside it a
  duplicate. A turn is seen only up to `--until`, so records written later
  do not change a result; a turn with no end marker by then is counted as
  partial.
- **Tokens.** Each token column sums the requests that report that counter.
  When only some do, the cell adds "(K of N)"; when none do, it says "not
  reported". A missing counter is never added as 0.

These selections come from tool names and message types only. A candidate
turn can still have done useful work (an analysis, a review, a move to
waiting, a continuation), and a status message can still have carried
something the team needed. Codex usage comes from cumulative `token_count`
deltas, so the calls inside one `exec` share one request.

## 1. `agent_inbox` after the message was already delivered

### Evidence

Codex members call `agent_inbox` from their own `exec` scripts, often as the
last line: `tools.mcp__crew__agent_inbox({limit: 8})`.

**Empty results.**

- Total: 946 calls returned `No new messages.`; 945 of them were Codex calls.
- Alone: 344 calls were alone, associated with 344 requests and 94.6 M input
  tokens, nearly all cache reads.
- Bundled: 599 cost 9,584 result bytes; 3 had no later request by the cutoff.

**After an in-band delivery.** There were 1,123 inbox calls within three
model responses of a delivery that carried every message's full body. 675
of them are candidates:

- 486 returned nothing: 317 alone, 168 bundled, 1 with no later request.
- 189 returned only messages already delivered in full: 71 alone, 118
  bundled.

They are associated with 387 requests and 103.4 M input tokens; the Claude
share lists at $1.75.

**Duplicate-only results across the session.** 231 calls returned only
messages already delivered in full. Together they put 859,372 bytes of
repeated message text into context, and that text is read again by every
later request in those sessions. The 82 alone calls are associated with 81
requests and 18.6 M input tokens ($2.61 for the Claude share).

Example: `~/.codex/sessions/2026/10/05/rollout-2026-10-05T12-26-02-01a10d19-e2c7-7430-bc06-6f4ac616dbd6.jsonl:5773`
(qa). An orchestrator answer arrives in-band. Lines 5781 and 5791 each end an
`exec` script with `agent_inbox({limit:8})` and get `No new messages.`

Same pattern in this session (designer, Claude Code). After a compaction, one
`agent_inbox()` returned 50 messages as unread, all of which had been
delivered in-band hours earlier: about 8,000 tokens of repeated text.

**Inbox reads that did find new messages.** These are not candidates. Of
1,552 Codex `agent_inbox` calls that returned messages, 1,364 returned at
least one id whose full body had not been delivered in-band. By type, the new ids were mostly
answers, held teammate status, artifacts and messages. So a read made while
doing other work does pick some messages up before delivery would. Fix 1a
targets only calls made to wait for a message.

### Cause

Delivery puts the message in the session but does not mark it read:

- `delivered()` (`crates/crew/src/daemon/delivery.rs:374`) only calls
  `bus.mark_delivered`, which sets `deliveries.delivered_at`
  (`persistence/sqlite.rs:398-410`).
- "Unread" means `acked_at IS NULL` (`persistence/sqlite.rs:361`). The only
  member path that sets `acked_at` is an `agent_inbox` call
  (`daemon/handlers.rs:415-424`). `Call::Ack` exists, but no tool, plugin or
  hook calls it.
- So the first `agent_inbox()` after an in-band message returns that message
  again in full and only then marks it read. Every later call returns
  `No new messages.` (`mcp/tools.rs:799` before this change).

Runtime confirmed (m-6a3b949a1857) that `cache-delivery` keeps this behaviour.
It moves `delivered_at` for queued Claude delivery to the observed native
turn, and leaves acknowledgement and `receive` filtering unchanged.

Several texts send members to `agent_inbox` when nothing is waiting for them:

| Text | Where | Effect |
| --- | --- | --- |
| "call agent_inbox to check for messages you may have missed" | `daemon/supervisor/launch.rs:29-30` (restart), `daemon/supervisor/recovery.rs:192-193` (daemon restart) | One `agent_inbox` per restart. A resumed session already holds what was delivered, and what was not delivered is delivered next. |
| "Read again what you need … tasks_list for the plan, … agent_inbox(all: true) for earlier messages" | `daemon/context.rs:415-419` (compaction notice) | Up to 50 full messages (the default `limit`) and the whole board are read again after a compaction. |
| "… (truncated — call agent_inbox for the full text)" | `agents/prompt.rs:719`, `daemon/delivery.rs:78` | Needed, but it names no id, so it fetches every unread message. |
| Tool description: "By default returns unread messages and marks them read" | `mcp/tools.rs:111-113` | Nothing says that messages arrive on their own, so a model checks. |
| Empty result: "No new messages." | `mcp/tools.rs:799` | Nothing tells the model to stop, and it often checks again. |

Some turns are started by injected text that has no bus message behind it:
the idle reminder, the restart prompts and the Claude bridge's "Team messages
came as your last turn ended". An `agent_inbox` call in those turns finds
nothing new.

### Fix

**1a. Wording (made in this card).** In `crates/crew/src/mcp/tools.rs`:

- The `agent_inbox` description gains one sentence: "New messages are
  delivered to you without asking (lines starting with "[crew]"), so do not
  call this to wait for one."
- An empty result now reads: "No new messages. One that arrives later is
  delivered to you: if you are waiting for it, end your turn, or park your
  card with agent_status("waiting", …), instead of checking again."

Both strings are constants compiled into the crew binary. The tool list a
session sees therefore stays byte-identical for the whole session. It changes
only when a session starts on a new crew build, and a new build already
changes the tool list (`notes/prompt-cache.md`, "What crew sends, by
runtime"). The same output reaches shell members through `crew ctl inbox`;
the wording holds for them too, since they are woken by a line naming the
command.

**1b. Mark a message read once its full text is in the session.** Proposed
card for runtime, after `cache-delivery` (D1) lands. Runtime confirmed
(m-eb6097657782) that D1 keeps today's read semantics, so 1b is a separate
change on top of it.

In `delivered()`, acknowledge an id only when there is proof that its full,
unchanged body entered the session. The point where delivery is recorded is
not enough on its own:

- queued Claude delivery: D1's observed native turn marker proves the message
  was routed and consumed, but not that its body arrived unchanged, since a
  native hook can rewrite the prompt. Acknowledge only when the observed
  prompt carries the full text of that id, for example by matching a digest
  of the delivered block.
- socket routes: `crew delivered` runs; the same full-body proof applies.
- Codex: the queue replies with the text crew sent, which `clip_for_argv`
  may have cut.
- terminal: the prompt is submitted after flattening, and herdr panes may
  reflow it.

Without that proof the message stays unread, as today. Two cases always keep
a message unread:

- Shell members receive only the wake-up line (`agents/shell.rs:61-66`), so
  their messages stay unread until `crew ctl inbox`.
- A body cut by `clip` (`agents/prompt.rs:714-721`) or `clip_for_argv`
  (`daemon/delivery.rs:72-80`) stays unread. Its suffix then names the id:
  `… (truncated: agent_inbox(id: "m-…") reads the full text)`.

Two effects to carry along in the card:

- The dashboards' unread badge (`web/src/graph/AgentNode.tsx:93`,
  `web/src/views/Inspector.tsx:149`) then counts the messages that have not
  reached the session yet, which is what an operator wants to see.
- Parked-card wakeups (`record_task_wakeups`, `daemon/overlap.rs:130`) run
  today only on the inbox path. The card records the wakeup before it
  acknowledges, in the same order as the inbox path
  (`daemon/handlers.rs:415-424`), so a parked card still learns of messages
  delivered in-band.

**1c. Restart and compaction notices.** Proposed card; owners are listed
under "Owners and cards".

- The restart and daemon-restart notices drop "and call agent_inbox to check
  for messages you may have missed". In its place: "Messages that arrived
  while it was down are delivered to you next."
- The compaction notice for a member keeps `task_get` and
  `agent_inbox(id: …)`, and drops `tasks_list` and `agent_inbox(all: true)`.
  It reads: "Read again only what you need: task_get for your card,
  agent_inbox(id: …) for a message the summary names, git_status and git_diff
  for work in progress." The orchestrator's notice keeps `tasks_list`.

### Regression tests

- **1a, in this card:**
  - `mcp::tools::tests::empty_inbox_says_to_stop_checking`;
  - `mcp::tools::tests::inbox_description_says_messages_arrive_unasked`.
- **1b:** a daemon test with a fake socket member.
  - Deliver one short message and one of 4,001 characters.
  - `inbox(unread_only)` returns only the long one, and the short one has
    `acked_at` set.
  - A delivery whose observed prompt differs from the sent block (a hook
    rewrote it) stays unread.
  - A Codex delivery cut by `clip_for_argv` stays unread, and its suffix
    names the id.
  - A shell member's message stays unread.
  - A parked card records the wakeup before the acknowledgement.
- **1c:** string tests on the restart, recovery and compaction notices.
  - No `agent_inbox` without `id:`.
  - No `tasks_list` in a member's compaction notice.
- **Measurement:** `scripts/cache/turns.test.mjs` classifies both the old
  and the new empty result as empty, so a rerun after the wording lands
  still counts empty calls. Rerun `turns.mjs --since <landing time>` and
  compare candidates per session hour with a run of the same length before
  it. That shows whether the candidates went down; it is not a saving until
  a paired before-and-after run on comparable work shows one.

## 2. Repeated board and card reads

### Evidence

Only 9 reads in the whole cohort returned exactly the same bytes as the same
call made earlier, with no change of the caller's own state in between: 6
`task_get`, 2 `tasks_list` and 1 `git_status`. All 9 were bundled with other
work, so no request is associated with them, only 196,237 result bytes. Members do
not re-read unchanged state often.

The cost is in the size of `tasks_list`:

- Calls: 137, returning 1.78 MB in all.
- Size: median 5,238 B, p90 25,171 B. On Codex the p90 is 90,522 B; 10
  calls exceeded 50 KB and the largest was 118,535 B.
- The current board: on 2026-10-08 it holds 122 cards, 109 of them done. One
  `tasks_list` from this session returned 112,377 B, which Claude Code would
  not keep inline.
- Done cards make up most of that size, because each one lists its result,
  commits and files.

### Cause

`render_tasks` (`mcp/tools.rs:572`) prints every card with its result line,
commits and files, whatever its status. Several texts point members to
`tasks_list`:

- the compaction notice (`daemon/context.rs:415`);
- the orchestrator briefing ("find out why (tasks_list, agents_list)",
  `agents/prompt.rs:407-409`);
- the orchestrator's notices ("Inspect with tasks_list / agents_list",
  `daemon/orchestrate.rs:255`).

`notes/prompt-cache.md` D4 rules out silently truncating `tasks_list` and asks
for an explicit read-view contract instead.

### Fix

**2a. Explicit compact board.** Proposed card. By default `tasks_list` prints
every card that is not done in full, as today, and then one line per done
card: id, assignee and title. The list ends with a fixed sentence: "109 done
cards shown by title only: task_get(id) reads one, tasks_list(done: true)
shows them in full."

The new `done` parameter is a tool-shape change. It lands once, in a new crew
build, so the tool list stays stable within a session. Members keep 1c, which
removes `tasks_list` from their compaction notice.

### Regression tests

- **2a:** a `render_tasks` test with three open and three done cards. Open
  cards print in full. Done cards print one line each, followed by the count
  sentence. `done: true` prints all six in full.
- **Size:** a test that a done card with a long result still renders in one
  line.

## 3. Turns that need no action

### Evidence

Turns started by a crew notice or a status message: 1,380. Of these, 530
made only status calls or crew reads (candidates). They are associated with
872 requests, 239.8 M input tokens and 8.6 M cache-write tokens; the Claude
share lists at $111.14.

| Started by | Turns | Candidates (status or read only) | Requests in all such turns | Claude list cost of all such turns |
| --- | ---: | ---: | ---: | ---: |
| Teammate or crew status message | 480 | 200 | 1,156 | $72.05 |
| "Your last turn ended without a tool call" | 162 | 89 | 240 | $1.70 |
| Orchestrator "main fast-forwarded to …" (type `message`) | 159 | 74 | 568 | $86.01 |
| "The usage limit that stopped you has lifted" | 142 | 0 (113 failed with no model response) | 916 | Codex only |
| Idle reminder "Reminder: task … is still open" | 72 | 14 | 919 | $73.40 |
| Claude bridge "Team messages came as your last turn ended" | 60 | 17 | 473 | $104.84 |
| "You are online as … Nothing is assigned to you yet" | 57 | 57 | 57 | $7.10 |
| "Agent … is unresponsive" (to the orchestrator) | 57 | 37 | 106 | $6.06 |
| "Task … was completed by …" | 51 | 13 | 105 | $7.11 |
| "Your conversation was just compacted" | 43 | 9 | 1,163 | $72.61 |

**Replies crew may forward.** crew forwards a member's end-of-turn text in
two cases only (`daemon/turns.rs:305-372`): to the sender of a `message` or
`question` addressed directly to that member by a teammate (not the
scheduler, not a broadcast), as an answer; and to each teammate named on a
line starting `@<id>:`, as a message. A reply to a status message, an answer
or a crew notice goes nowhere. A forwarded answer or message starts a turn in
a resting recipient, because only status news waits for company.

492 replies fall in those two cases: 451 after a `message` or `question`
opener and 41 with an explicit `@<id>:` line. 85 of the 451 followed an
orchestrator "main fast-forwarded" message. Native session files do not show
whether such a message was sent directly or as a broadcast, so 451 is an
upper bound on forwarded replies. Another 533 replies were not forwardable.

Turns started by teammate status are mostly to waiting members, by design:

- by sender: qa 107 turns (47 candidates), orchestrator 77 (24), scheduler
  60 (23), graph 45 (21), runtime 28 (18);
- a teammate's status starts a turn only in a member that is waiting
  (`daemon/delivery.rs:60-64`).

Idle reminders mostly lead to work: 58 of 72 did more than status calls or
reads. In 19 of the 36 Claude reminders, though, the member had launched a
background Bash, Agent or Monitor task that had not reported back yet (12
of them within the previous two hours). This session had several such
reminders while its own measurement subagent was running, and one more after
an unrelated question was delivered while its card was parked: delivery
resumes a parked card (`resume_waiting_from_delivery`), and the reminder
followed once the answer turn ended.

### Cause and fix

**3a. Landing news sent as type `message`.**

- *Cause.* The orchestrator announced each fast-forward of main to members
  as a `message`, which starts a turn in every resting recipient: 159 turns,
  74 of them candidates. Those candidate turns include 4.19 M cache-write
  tokens; writes alone do not show why the cache was written. 85 replies
  followed such a message; crew forwards them to the orchestrator as answers
  only if the message was addressed directly.
- *Fix, orchestrator behaviour (applied 2026-10-08, m-3b7c9bfbdd38).*
  Landing news goes out as an `agent_broadcast` of type `status`. A status
  from a teammate is held for a resting member until its next message, and
  still wakes a member that is waiting. Rebase or replay instructions go as a
  `message` only to the members who must act.
- *Fix, tool wording (made in this card).* The descriptions of `agent_send`
  and `agent_broadcast` now say that `status` is for news that needs nothing
  back and does not wake a resting teammate unless that teammate is waiting.
  That claim is pinned by
  `delivery::tests::news_waits_for_company_unless_it_is_what_a_member_waits_for`.
  It says nothing about a member in the middle of a turn: pi, for one, queues
  a status that arrives mid-turn as a follow-up that runs after the turn.
- *Fix, briefing (proposed for `cache-standing-prompts`, `agents/prompt.rs`,
  orchestrator step 3, Supervise).* One fixed sentence: "Tell the team news
  that needs nothing back, such as main moving, with agent_broadcast type
  status; send a message only to the members who must act on it, saying what
  to do."
  `agent_broadcast` already takes `type` (`mcp/tools.rs:90`, any of
  `MESSAGE_TYPES`, default `message`), so the sentence needs no tool
  change. It is a fixed workflow correction, separate from D3's prompt
  placement experiment. Any D3 placement comparison carries the same
  wording on both sides, so fewer wake-ups are not credited to the prefix
  layout.

**3b. The no-tool-call follow-up.**

- *Cause.* The worker briefing says "End a turn with what whoever asked
  needs, or nothing" (`agents/prompt.rs:155-161`). The follow-up in
  `daemon/turns.rs:269-276` then tells a member whose turn ended without a
  tool call that "nothing came of it", and asks for `agent_status("idle",
  …)`. 89 of its 162 turns were candidates.
- *Fix.* `cache-obligation-wakes` (D4, landed in 66eadf2f) already suppresses
  the follow-up for settled obligations and identified replies. Most of this
  sample predates it. Rerun `turns.mjs` over a window that starts when the
  running daemon was confirmed to be a build containing D4, not at the merge
  time (2026-10-08T06:29Z), and report the native sessions and builds the
  window covers. Change nothing more until that rerun shows a need. Genuine
  `message` and `question` obligations stay; no wording heuristic decides
  that a message needed nothing (`notes/prompt-cache.md` D4). Owner: content.

**3c. The Claude bridge's wake-up prompt.**

- *Cause.* Claude's bridge submits "Team messages came as your last turn
  ended (above). Act on them." (`assets/crew-bridge.register.ts:18`) when a
  message was appended to a turn that ended before the model saw it. 60
  turns, 17 of them candidates ($18.87 associated).
- *Fix.* `cache-delivery` (D1) moves ordinary Claude delivery to turn
  boundaries, which removes the case. `cache-acceptance` checks that this
  opening no longer occurs for ordinary traffic.

**3d. Retries against an account that is out of credits.**

- *Cause.* `limits.rs` retries a `Credits` limit every `limit_retry_secs`
  (1,800 s by default), as for a usage window (`daemon/limits.rs:104-111`).
  Each retry sends "The usage limit that stopped you has lifted". In this
  sample, 113 of 142 such turns failed with no model response. A further
  1,278 Codex turns failed with "Your workspace is out of credits". These
  cost no tokens, but each one is a wake-up, a status change and a failed
  turn.
- *Fix (proposed, runtime, low priority).* Back off exponentially from
  `limit_retry_secs` up to 4 h for `LimitKind::Credits` only, and reset the
  backoff on a successful turn.

**3e. Idle reminders while the member's own background work runs.**

- *Cause.* `supervisor/health.rs:110-125` counts a session as idle once its
  turn ends. A Claude session waiting for its own background subagent or
  Bash task is in that state.
- *Fix (proposed, runtime with the Claude bridge, low priority).* Report
  background tasks the bridge sees as running as activity, so that
  `idle_since` does not start. That needs a real, bounded activity signal
  from the runtime (a task that never reports back must not suppress
  reminders forever). The 90 s, three-reminder policy stays until such a
  signal exists and evidence supports the change. Only 23 reminder turns in
  the sample ended with no tool call or only `agent_status` (15 associated
  requests, $1.06).

**3f. Launch prompt for a member with nothing assigned.**

- *Cause.* "You are online as … Nothing is assigned to you yet. Reply with
  one short line" accounts for 57 turns, all of them status-only, $7.10.
- *Fix.* None proposed. This turn creates the session and writes its cached
  prefix, which the member's first real turn then reads.

### Regression tests

- **3a:** `mcp::tools::tests::send_descriptions_say_status_news_does_not_wake_resting_teammates`
  (in this card), together with the existing delivery test. The briefing
  sentence goes in `cache-standing-prompts`' byte snapshot. Measurement:
  in a run with `--since 2026-10-08T07:20:00Z`, "main fast-forwarded" no
  longer appears as a turn-starting `message` opening.
- **3b:** rerun of `turns.mjs` with `--since` set to the time the running
  daemon was confirmed on a D4 build, with session and build coverage in the
  report.
- **3c:** `cache-acceptance`: the bridge's wake-up opening count is 0 for
  ordinary traffic.
- **3d:** limits test: successive `Credits` retries are spaced 30 min, 1 h,
  2 h, 4 h, 4 h; a usage-window limit keeps its fixed interval.
- **3e:** bridge test: with a background task reported running, the health
  check sends no reminder.

## 4. Tool round trips per delivered message

There were 4,773 turns started by a delivered message. The median turn makes
1 tool call; the mean is 6.06 and the p90 is 9.

| Runtime | Turns | Tool calls | Crew reads | Turns whose first call is a crew read | Requests per turn |
| --- | ---: | ---: | ---: | ---: | ---: |
| Claude Code | 1,354 | 12,543 | 442 | 147 | 9.16 |
| Codex | 3,419 | 16,392 | 2,684 | 715 | 2.65 |

The crew reads made in these turns were 2,191 `agent_inbox`, 764 `task_get`,
88 `tasks_list` and 83 `agents_list`. The requests that consumed them, each
shared among the calls it answered, total 1,434, with 362.0 M input tokens;
the Claude share lists at $30.16.

Most of the extra round trips come from pattern 1: `agent_inbox` calls are
70% of all crew reads after a delivery, almost all of them from Codex members. Of the rest:

- The `task_get` after a task dispatch is needed, since the dispatch names
  the card rather than including it (`agents/prompt.rs:706`). That is 132
  dispatch turns in this sample. Putting the card in the dispatch would save
  one request each, but card text can be long and changes as dependencies
  finish. Not proposed.
- Bursts are already gathered: 286 turns opened with "[crew] N new
  messages", using the existing 1.5 s quiet window and 5 s maximum
  (`daemon/delivery.rs:46-50`). Not proposed.

## After

### 2026-10-08: deployment gate is still pending

The live team has **not run the D4/D1 build yet**. The follow-up-fix verdict
therefore remains pending. No further `daemon/turns.rs` change is justified
by this rerun.

Checks at 16:12:31Z and 16:30:21Z found the same daemon, PID 2658115, started
at **2026-10-05T17:26:01.609Z**. Its process start ticks and boot id match
`daemon.json`. The executable actually mapped by `/proc/2658115/exe` is the
deleted Oct 5 release, 15,632,944 bytes, modified at 17:24:53.185Z, SHA-256
`d40f215508b87e3d5cbf7a9e86336d5c63b93ee3059f3eb7dbc7eba70fdbb310`.
The image predates the Oct 8 D4/D1 changes; it also lacks D1's
`native-delivery:` marker. Both reads had stable inode/size/mtime. A source
merge or a newer binary elsewhere on disk is not evidence that this process
is running it. The exact source commit of the old image is unknown.

The [rerun evidence](turn-efficiency/after-2026-10-08.json) retains the build
checks, read-only database horizons, collector hashes, window totals and
native source ids/paths/hashes/initial-byte bounds. No prompts, tool-result
bodies or credentials are included. Native CLI build versions were not
collected; current PATH versions must not stand in for historical versions.

### Reproduced cut and legacy continuation

The original `--until 2026-10-08T09:59:18Z` cut reproduces the reported
18,591 Claude and 10,452 Codex usage observations, 2,030 and 3,692 turns,
162 no-tool-call follow-up turn openings (89 status/read-only candidates),
946 empty inbox calls, 1,329 status-only turns and 530 notice/status
candidates. Discovery now sees two additional Codex files with no activity
before that cut: 47 discovered files, but the same 45 active files. Claude
remains 49. The collector's cohort `sessions` field counts discovered files,
including ones outside the chosen window; it is not an exposure denominator.

Two equal six-hour windows describe what the **same old daemon** did next:

- **A:** `(2026-10-08T03:59:18Z, 2026-10-08T09:59:18Z]`.
- **B:** `(2026-10-08T09:59:18Z, 2026-10-08T15:59:18Z]`.

The CLI uses inclusive bounds, so each `--since` is one millisecond after
the open left endpoint. Neither window is a post-fix sample.

| Measure | A: before the cut | B: legacy continuation |
| --- | ---: | ---: |
| Active native files, Claude / Codex | 5 / 6 | 5 / 8 |
| Usage observations, Claude / Codex | 3,007 / 2,010 | 645 / 649 |
| Turns, Claude / Codex | 372 / 494 | 152 / 306 |
| Partial turns at cutoff, Claude / Codex | 2 / 4 | 1 / 5 |
| Turns without a model response, Claude / Codex | 5 / 48 | 0 / 195 |
| No-tool-call follow-up turn openings | 24 | 9 |
| Of those, status/read-only candidates | 19 | 2 |
| Associated requests for all those follow-up turns, upper bound | 43 | 12 |
| Empty inbox calls | 148 | 43 |
| Status-only turns, Claude / Codex | 100 / 219 | 61 / 20 |
| Notice/status turns that were status/read-only candidates | 93 | 21 |

All no-tool-call openings and empty inbox calls in these two windows are
Codex observations. Codex's empty-inbox share barely changes: 148 of 381
inbox calls (38.85%) in A, and 43 of 117 (36.75%) in B. Claude made 17 inbox
calls in A and none in B. The lower absolute counts accompany much less
observed work and a large increase in Codex turns without a model response;
they do not establish a reduction caused by D4, D1, the wording changes or
any other landed code. Candidate status turns can still be legitimate work.

The published manifest identifies every session active in A or B; the full
96-file provenance manifest stays with the local outputs. Both windows use the
same five Claude sessions (orchestrator, designer, reviewer, scheduler, ui).
B retains the six Codex sessions in A and adds Codex ui and scheduler. Models
observed in these files are `claude-opus-5-5`, `claude-fable-5-1`,
`gpt-6-astra` and `gpt-6.1-sol`; there is no pi sample. No admitted source
changed during the provenance read and no malformed source lines were
reported. Discovery excluded 17 files. A separate provenance pass reused
`readEvents`/`analyzeSession` on bounded prefixes and asserted equality with
all three CLI reports for requests, turns, partial turns, calls and the three
target patterns.

Read-only daemon event counts are a separate measure: A has 34
`agent.unanswered`, 26 `agent.reminded` and 27 `agent.idle_turn` events; B has
8, 3 and 8. No `daemon.started` event lies in either window. Generation
events, delivered inputs and turn openings are not one-to-one: for example,
A has 29 matching native inputs, five folded into an existing turn, leaving
24 openings. These counts are not proofs that a particular obligation was
settled or that a follow-up was unnecessary.

No cost, cache-hit or savings verdict is made. The unchanged collector keeps
its audit price book, where Codex observations are unpriced; that is a
limitation of this book, not a claim that the provider has no list price.

### Resuming the actual after measurement

After the operator rebuilds and restarts crew, capture the live PID, start
time and executable hash again. The deployment boundary is that verified PID
start for an image whose hash matches the recorded fixed build. Verify its
source revision contains D4 and D1 and that both D1 image markers,
`native-delivery:` and `native-delivery-started`, are present; the markers
alone do not establish D4's source provenance. Record member native sessions
and delivery modes too; D1's queued
Claude route must be distinguished from legacy terminal/Codex/pi paths.
Start the after window at that verified deployment boundary, not the merge
time. The after window must be at least as long as the pre-deployment window
and contain comparable responding-turn counts per runtime before any rates
are compared. Otherwise collect more exposure or leave that comparison
inconclusive. Retain an equal-duration pre-deployment window where possible,
with runtime/model and responding-turn coverage stated. Separate old prompts
queued before the restart from prompts generated by the new daemon.

Join any remaining candidate follow-up to message ids and typed task/reply
history before proposing another fix. Genuine unanswered questions,
messages, executing work and requested rework must still be reminded; the
target is redundant prompts, not a zero count of all follow-ups. Until that
exposure exists, `turn-efficiency-rerun` stays pending deployment.

Reproduction commands (Node 24.19.0; collector tests passed 18/18):

```sh
node --test scripts/cache/turns.test.mjs
node scripts/cache/turns.mjs --out target/turn-efficiency-rerun/original-cut \
  --until 2026-10-08T09:59:18Z
node scripts/cache/turns.mjs --out target/turn-efficiency-rerun/pre-cut-6h \
  --since 2026-10-08T03:59:18.001Z --until 2026-10-08T09:59:18Z
node scripts/cache/turns.mjs --out target/turn-efficiency-rerun/continuation-6h \
  --since 2026-10-08T09:59:18.001Z --until 2026-10-08T15:59:18Z
```

The outputs and provenance sidecars remain under that target directory in
content's worktree. The commands use the same workspace discovery as the
original run; when run elsewhere, pass
`--workspace /home/nade/workspaces/personal/crew` explicitly.

## Owners and cards

| Fix | Files | Owner | State |
| --- | --- | --- | --- |
| 1a `agent_inbox` wording; 3a `agent_send`/`agent_broadcast` wording | `crates/crew/src/mcp/tools.rs` | designer, this card | made, tested |
| 3a landing news as status broadcast | orchestrator behaviour | orchestrator | applied 2026-10-08 |
| 3a briefing sentence | `agents/prompt.rs` | native-architect, inside `cache-standing-prompts` | proposed |
| 1b mark read on delivery, id in the truncation suffix | `daemon/delivery.rs`, `daemon/handlers.rs`, `daemon/overlap.rs`, `agents/prompt.rs` `clip` | runtime, new card after `cache-delivery`; the one-line `clip` change agreed with native-architect | proposed |
| 1c restart and compaction notices | `daemon/supervisor/launch.rs`, `daemon/supervisor/recovery.rs`, `daemon/context.rs` | runtime, after D1's final source; `launch.rs` serialised with `cache-standing-prompts` | proposed |
| 2a compact board view | `crates/crew/src/mcp/tools.rs` `render_tasks`, `Call::Tasks` | orchestrator to assign; the tool-shape change lands once per build | proposed |
| 3b re-measure the no-tool-call follow-up after D4 | `daemon/turns.rs` only if the rerun shows a need | content | legacy continuation measured; post-fix verdict pending deployment (After) |
| 3c bridge wake-up prompt | `assets/crew-bridge.register.ts` | runtime, `cache-delivery`; verified by `cache-acceptance` | in progress |
| 3d backoff for `Credits` | `daemon/limits.rs` | runtime, its own card | proposed, low priority |
| 3e background work counts as activity | `assets/crew-bridge.register.ts`, `supervisor/health.rs` | runtime with content, its own card, once a bounded activity signal exists | proposed, low priority |

## Reproducing

```sh
node --test scripts/cache/turns.test.mjs
# the figures in this note:
node scripts/cache/turns.mjs --out target/turn-efficiency --until 2026-10-08T09:59:18Z
# after a fix lands:
node scripts/cache/turns.mjs --out target/turn-efficiency-after --since 2026-10-08T07:20:00Z
```

Options: `--workspace PATH`, `--window N` (default 3 model responses),
`--short-reply N` (default 300 characters), `--since`/`--until` ISO times,
`--session-file runtime=PATH`, `--all-runs`, `--prices TSV`. A run takes
about 10 s and reads native session files only.
