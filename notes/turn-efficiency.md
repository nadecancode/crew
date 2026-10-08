# Turn efficiency

Operator request (w-e08e0c30759a45de, item 3): find and remove wasted turns
and tool calls in member sessions. The operator's example: a Codex member is
given `[crew] Message from scheduler …` with the whole body in its prompt,
then calls `crew.agent_inbox` twice and gets `No new messages` both times.

This note measures four patterns across this team's sessions. For each one
it traces the crew text or mechanism that causes it and gives a fix, an owner
and a regression test. Three fixes are made here: the wording of the
`agent_inbox`, `agent_send` and `agent_broadcast` tools. One more, sending
landing news as status, is already applied by the orchestrator. The rest are
proposed cards, because they touch files that other cards own.

## Summary

Measured 2026-10-08T07:13Z over 49 Claude Code sessions (17,604 requests,
1,876 turns) and 45 Codex sessions (9,621 requests, 3,486 turns). There were
no pi sessions in the cohort.

Token counts are input tokens of the requests charged to each pattern, nearly
all of them cache reads; see Method for how requests are charged. Dollar
figures are list prices for the input side of Claude requests only, since
Codex models have no listed price. The patterns overlap, so their rows must
not be added together.

| Pattern | Count | Requests charged | Input tokens | Claude list cost | Fix | State |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| 1. `agent_inbox` that returns nothing, or only messages already delivered, within 3 responses of a delivery | 623 of 1,049 | 358 | 99.5 M | $1.76 | 1a wording; 1b mark read on delivery; 1c notice wording | 1a made here; 1b, 1c proposed |
| 1. All empty `agent_inbox` calls | 868 (867 Codex) | 315 | 90.1 M | none (Codex) | as above | |
| 1. `agent_inbox` returning only messages already delivered | 226 | 78 | 18.8 M, plus 780,759 result bytes | $2.37 | 1b | proposed |
| 2. Byte-identical repeated reads | 7 | 0 (bundled) | 188,740 result bytes | — | none needed | |
| 2. `tasks_list` result size | 133 calls, 1.65 MB | — | p90 90,522 B on Codex; 112,377 B on the current board | — | 2a explicit compact board view | proposed |
| 3. Turns started by a notice or status message that did only status calls or crew reads | 486 of 1,296 | 789 | 212.0 M (7.9 M cache write) | $103.62 | 3a–3f | 3a applied; rest proposed |
| 3. Text-only replies to an orchestrator message or status message | 186 | 186 | 60.6 M (4.5 M cache write) | $57.55 | 3a covers the 85 replies to orchestrator messages | applied |
| 4. Crew reads in turns started by a delivered message | 2,892 reads in 4,424 turns | 1,320 | 330.9 M | $27.45 | follows from 1 | |

Status-only turns of every origin come to 1,178 turns, 1,736 requests, 484.3 M
input tokens and $284.04 for the Claude share. Many of these are legitimate:
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

How a call or turn is charged:

- **Alone.** A wasted call is "alone" when every tool call answered by the
  next model response is `agent_inbox`, `agent_status` or `task_update`. It
  is then charged that whole next request, since the request exists only
  because of the call.
- **Bundled.** A call that ran beside other real work, for example inside a
  Codex `exec` script with `exec_command`, is charged only its result bytes.
- **Turns.** A wasted turn is charged every request in it.
- **Folded messages.** A message folded into a turn that is already running
  counts as a delivery but starts no turn.
- **Status-only.** "Status-only" means the turn's only tool calls were
  `agent_status`, `task_update` or `agent_send` with type `status`. A turn with
  no tool calls also counts when its reply is 300 characters or shorter.
- **Read-only.** "Read-only" adds crew reads (`agent_inbox`, `tasks_list`,
  `task_get`, `agents_list`) to the status calls.

Both labels come from tool names and message types only, so a status message
can still have carried something the team needed. Codex usage comes from
cumulative `token_count` deltas, so the calls inside one `exec` share one
request.

## 1. `agent_inbox` after the message was already delivered

### Evidence

Codex members call `agent_inbox` from their own `exec` scripts, often as the
last line: `tools.mcp__crew__agent_inbox({limit: 8})`.

**Empty results.**

- Total: 868 calls returned `No new messages.`; 867 of them were Codex calls.
- Alone: 315 calls were alone. They cost 315 requests and 90.1 M input
  tokens, nearly all cache reads.
- Bundled: the other 550 cost 8,800 result bytes.

**After an in-band delivery.** There were 1,049 inbox calls within three
model responses of a delivery that already carried the message bodies. 623
of them were wasted:

- 437 returned nothing: 290 alone, 147 bundled.
- 184 returned only messages that had been delivered: 69 alone, 115 bundled.

**Duplicate-only results across the session.** 226 calls returned only
messages already delivered in-band. Together they put 780,759 bytes of
repeated message text into context, and that text is read again by every
later request in those sessions.

Example: `~/.codex/sessions/2026/10/05/rollout-2026-10-05T12-26-02-01a10d19-e2c7-7430-bc06-6f4ac616dbd6.jsonl:5773`
(qa). An orchestrator answer arrives in-band. Lines 5781 and 5791 each end an
`exec` script with `agent_inbox({limit:8})` and get `No new messages.`

Same pattern in this session (designer, Claude Code). After a compaction, one
`agent_inbox()` returned 50 messages as unread, all of which had been
delivered in-band hours earlier: about 8,000 tokens of repeated text.

**Inbox reads that did find new messages.** These are not all waste. Of
1,474 Codex `agent_inbox` calls that returned messages, 1,289 returned at
least one id not yet delivered in-band. By type, the new ids were mostly
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
- **Measurement:** rerun `turns.mjs --since <landing time>`. Expected: alone
  empty `agent_inbox` calls near 0, and `inbox_duplicate_only` near 0 once 1b
  lands.

## 2. Repeated board and card reads

### Evidence

Only 7 reads in the whole cohort returned exactly the same bytes as the same
call made earlier, with no change of the caller's own state in between: 4
`task_get`, 2 `tasks_list` and 1 `git_status`. All 7 were bundled with other
work, so no request is charged to them, only 188,740 result bytes. Members do
not re-read unchanged state often.

The cost is in the size of `tasks_list`:

- Calls: 133, returning 1.65 MB in all.
- Size: median 5,267 B. On Codex the p90 is 90,522 B, and 9 calls exceeded
  50 KB.
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

Turns started by a crew notice or a status message: 1,296. Of these, 486
ended with only status calls or crew reads, at a cost of 789 requests, 212.0
M input tokens and 7.9 M cache-write tokens. The Claude share lists at
$103.62.

| Started by | Turns | Status or read only | Requests in all such turns | Claude list cost of all such turns |
| --- | ---: | ---: | ---: | ---: |
| Teammate or crew status message | 437 | 173 | 1,016 | $53.48 |
| "Your last turn ended without a tool call" | 154 | 81 | 223 | $1.70 |
| Orchestrator "main fast-forwarded to …" (type `message`) | 153 | 73 | 445 | $86.01 |
| "The usage limit that stopped you has lifted" | 142 | 0 (113 failed with no model response) | 916 | Codex only |
| Idle reminder "Reminder: task … is still open" | 61 | 9 | 865 | $68.45 |
| "You are online as … Nothing is assigned to you yet" | 57 | 57 | 57 | $7.10 |
| "Agent … is unresponsive" (to the orchestrator) | 57 | 37 | 106 | $6.06 |
| Claude bridge "Team messages came as your last turn ended" | 55 | 17 | 338 | $91.65 |
| "Task … was completed by …" | 49 | 12 | 101 | $6.76 |
| "Your conversation was just compacted" | 37 | 7 | 994 | $61.64 |

Text-only replies: 186 turns opened by an orchestrator message or a status
message ended with text and no tool call. They cost 186 requests, 60.6 M input
tokens and 4.5 M cache-write tokens; the Claude share is $57.55. Among them:

- 52 were replies to "main fast-forwarded" ($25.85);
- 33 were replies to other orchestrator messages.

crew forwards a text reply to a `message` or `question` as an answer
(`daemon/turns.rs:331-362`). Each of these replies therefore started a turn in
the orchestrator as well.

Turns started by teammate status are mostly to waiting members, by design:

- by sender: qa 107 turns (47 wasted), orchestrator 71 (19), graph 40 (18),
  runtime 24 (14);
- a teammate's status starts a turn only in a member that is waiting
  (`daemon/delivery.rs:60-64`).

Idle reminders mostly lead to work: 52 of 61 did more than status calls. In
13 of the 29 Claude reminders, though, the member had launched a background
Bash, Agent or Monitor task that had not reported back yet. This session had
three such reminders while its own measurement subagent was running.

### Cause and fix

**3a. Landing news sent as type `message`.**

- *Cause.* The orchestrator announced each fast-forward of main to every
  member as a `message`. Every resting member took a turn: 153 turns, 73 of
  them status or read only. Claude members woken this way had a cold cache:
  4.19 M cache-write tokens in the wasted turns alone. 52 members then
  replied with text, which crew forwarded to the orchestrator as an answer.
- *Fix, orchestrator behaviour (applied 2026-10-08, m-3b7c9bfbdd38).*
  Landing news goes out as an `agent_broadcast` of type `status`. A status
  from a teammate is held for a resting member until its next message, and
  still wakes a member that is waiting. Rebase or replay instructions go as a
  `message` only to the members who must act.
- *Fix, tool wording (made in this card).* The descriptions of `agent_send`
  and `agent_broadcast` now say that `status` is for news that needs nothing
  back and starts no turn of its own unless the teammate is waiting. That
  claim is pinned by
  `delivery::tests::news_waits_for_company_unless_it_is_what_a_member_waits_for`.
  One exception: pi queues a status that arrives mid-turn as a follow-up,
  which runs after the turn. No pi member was in this cohort.
- *Fix, briefing (proposed for `cache-standing-prompts`, `agents/prompt.rs`,
  orchestrator step 3, Supervise).* One fixed sentence: "Tell the team news
  that needs nothing back, such as main moving, with agent_broadcast type
  status; send a message only to the members who must act on it, saying what
  to do."

**3b. The no-tool-call follow-up.**

- *Cause.* The worker briefing says "End a turn with what whoever asked
  needs, or nothing" (`agents/prompt.rs:155-161`). The follow-up in
  `daemon/turns.rs:269-276` then tells a member whose turn ended without a
  tool call that "nothing came of it", and asks for `agent_status("idle",
  …)`. 81 of its 154 turns did only that.
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
  message was appended to a turn that ended before the model saw it. 55
  turns, 17 of them wasted, $18.87.
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
  signal exists and evidence supports the change. Only 21 reminder turns in
  the sample were wasted ($0.83).

**3f. Launch prompt for a member with nothing assigned.**

- *Cause.* "You are online as … Nothing is assigned to you yet. Reply with
  one short line" accounts for 57 turns, all of them status-only, $7.10.
- *Fix.* None proposed. This turn creates the session and writes its cached
  prefix, which the member's first real turn then reads.

### Regression tests

- **3a:** `mcp::tools::tests::send_descriptions_say_status_news_starts_no_turn`
  (in this card), together with the existing delivery test. The briefing
  sentence goes in `cache-standing-prompts`' byte snapshot. Measurement:
  after 2026-10-08T07:20Z, "main fast-forwarded" no longer appears as a
  turn-starting `message` opening.
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

There were 4,424 turns started by a delivered message. The median turn makes
1 tool call; the mean is 6.05 and the p90 is 9.

| Runtime | Turns | Tool calls | Crew reads | Turns whose first call is a crew read | Requests per turn |
| --- | ---: | ---: | ---: | ---: | ---: |
| Claude Code | 1,207 | 11,619 | 406 | 143 | 9.5 |
| Codex | 3,217 | 15,142 | 2,486 | 656 | 2.56 |

The crew reads made in these turns were 2,029 `agent_inbox`, 696 `task_get`,
85 `tasks_list` and 82 `agents_list`. The requests that consumed them total
1,320, with 330.9 M input tokens; the Claude share lists at $27.45.

Most of the extra round trips come from pattern 1: `agent_inbox` calls are
70% of all crew reads after a delivery, almost all of them from Codex members. Of the rest:

- The `task_get` after a task dispatch is needed, since the dispatch names
  the card rather than including it (`agents/prompt.rs:706`). That is 128
  dispatch turns in this sample. Putting the card in the dispatch would save
  one request each, but card text can be long and changes as dependencies
  finish. Not proposed.
- Bursts are already gathered: 253 turns opened with "[crew] N new
  messages", using the existing 1.5 s quiet window and 5 s maximum
  (`daemon/delivery.rs:46-50`). Not proposed.

## Owners and cards

| Fix | Files | Owner | State |
| --- | --- | --- | --- |
| 1a `agent_inbox` wording; 3a `agent_send`/`agent_broadcast` wording | `crates/crew/src/mcp/tools.rs` | designer, this card | made, tested |
| 3a landing news as status broadcast | orchestrator behaviour | orchestrator | applied 2026-10-08 |
| 3a briefing sentence | `agents/prompt.rs` | native-architect, inside `cache-standing-prompts` | proposed |
| 1b mark read on delivery, id in the truncation suffix | `daemon/delivery.rs`, `daemon/handlers.rs`, `daemon/overlap.rs`, `agents/prompt.rs` `clip` | runtime, new card after `cache-delivery`; the one-line `clip` change agreed with native-architect | proposed |
| 1c restart and compaction notices | `daemon/supervisor/launch.rs`, `daemon/supervisor/recovery.rs`, `daemon/context.rs` | runtime, after D1's final source; `launch.rs` serialised with `cache-standing-prompts` | proposed |
| 2a compact board view | `crates/crew/src/mcp/tools.rs` `render_tasks`, `Call::Tasks` | orchestrator to assign; the tool-shape change lands once per build | proposed |
| 3b re-measure the no-tool-call follow-up after D4 | `daemon/turns.rs` only if the rerun shows a need | content | proposed |
| 3c bridge wake-up prompt | `assets/crew-bridge.register.ts` | runtime, `cache-delivery`; verified by `cache-acceptance` | in progress |
| 3d backoff for `Credits` | `daemon/limits.rs` | runtime, its own card | proposed, low priority |
| 3e background work counts as activity | `assets/crew-bridge.register.ts`, `supervisor/health.rs` | runtime with content, its own card, once a bounded activity signal exists | proposed, low priority |

## Reproducing

```sh
node --test scripts/cache/turns.test.mjs
node scripts/cache/turns.mjs --out target/turn-efficiency
# after a fix lands:
node scripts/cache/turns.mjs --out target/turn-efficiency-after --since 2026-10-08T07:20:00Z
```

Options: `--workspace PATH`, `--window N` (default 3 model responses),
`--short-reply N` (default 300 characters), `--since`/`--until` ISO times,
`--session-file runtime=PATH`, `--all-runs`, `--prices TSV`. A run takes
about 10 s and reads native session files only.
