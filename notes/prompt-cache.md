# Prompt caching in crew

Operator objective: raise the prompt cache hit rate of crew's agentic
workloads. This note collects the work on it. `cache-baseline` keeps its
numbers in `notes/prompt-cache/`. `cache-design` builds on this audit.

## Audit

Task `cache-prompt-audit`. This is a read-only audit: nothing in crew was
changed.

### Sources

**crew.** Every `crates/crew` line reference is to `main` at `c8052373`.

`main` at `e3ba08ba` differs from it in seven `crates/crew` files:

- `cli/ops/native.rs`
- `daemon/binary.rs`
- `git/beneath.rs`
- `os.rs`
- `sandbox/landlock.rs`
- `sandbox/run.rs`
- `tests/native_gateway.rs`

None of the prompt, delivery or lifecycle paths cited here is among them.

**CLIs.** Their mechanics were read from the local installs. None is a
source checkout.

| CLI | Path read | How |
| --- | --- | --- |
| Claude Code 2.1.291 | `~/.local/share/claude/versions/2.1.291` | Strings in the binary: settings descriptions, the statusline schema, environment names |
| Codex CLI 0.159.0 | `/nix/store/pmh4yzyd0g8rz5l1jv2lnsb2gz33ljhi-codex-0.159.0/libexec/codex` | Strings in the binary only. No Codex source or upstream revision was available here. |
| pi | not installed here | crew supports `@earendil-works/pi-coding-agent` 1.0 and later (`agents/pi.rs:1-2,92`). The only pi here is GSD's, `/nix/store/v0rin235k416809iv79f4iayi7907n8l-gsd-pi-2.70.1`, which crew refuses (`agents/pi.rs:130`, `check_binary`). |

The sessions measured below ran Claude Code 2.1.287 to 2.1.290.

Each claim below is marked **measured** (the sample below), **code** (read in
crew or a CLI) or **unmeasured** (stated by a CLI's documentation or
inferred, not observed here).

### How each CLI caches

#### Claude Code

The prefix is tools, then system, then messages. A change invalidates
everything after it.

- **TTL (code, measured).**
  - Set with the settings key `promptCacheTtl` or the environment variable
    `CLAUDE_CODE_PROMPT_CACHE_TTL`, which wins. Values are `"5m"` or `"1h"`.
  - When unset, the binary's settings description gives: 1 hour on a Claude
    subscription within its usage limits; 5 minutes on an API key, Bedrock,
    Vertex or Foundry.
  - Subagents: `subagentPromptCacheTtl` or
    `CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL`. Their default is 5 minutes
    unless `ENABLE_PROMPT_CACHING_1H=1`.
  - `DISABLE_PROMPT_CACHING` and its per-model variants turn caching off.
  - crew sets none of these.
  - Measured: every cache write in the sample is a 1 h write
    (`cache_creation.ephemeral_1h_input_tokens`).
- **Prefix shared across sessions (measured).** The first request of 27 of
  the 29 sessions starting in the sample read 23,368 to 23,881 tokens from
  cache. That is Claude Code's own system prompt and tools, cached across
  sessions.
  - A request that also read the session's own system prompt read 27,697 to
    31,351.
  - So crew's per-session system part (briefing, member tone, environment) is
    about 5k to 7k tokens.
- **Deferred MCP tools (code, measured, conditional).** When tool search is
  on, MCP tools are deferred. Their names arrive as `deferred_tools_record`
  and `deferred_tools_delta` attachments. A schema enters the conversation as
  a tool result once loaded, and server instructions arrive as an
  `mcp_instructions_delta` attachment. A changed tool set then only appends.
  - Tool search depends on:
    - `ENABLE_TOOL_SEARCH`: `true`, `false`, `auto` or `auto:N`;
    - the endpoint: a base URL that is not a first-party Anthropic host turns
      it off unless that variable is set;
    - a server's `alwaysLoad` option, and per tool the `_meta`
      `anthropic/alwaysLoad`. These put the definitions in the tools prefix.
  - Documentation: <https://code.claude.com/docs/en/mcp#configure-tool-search>.
  - Measured: all 37 worktree sessions in the sample have deferred-tool
    records.
  - A member on a gateway endpoint, or with `ENABLE_TOOL_SEARCH=false` in
    its endpoint `env`, would carry crew's tools in its prefix.
- **History changes (code, measured).** When the client drops thinking blocks
  it records a `thinking_drop` attachment. It carries
  `clientChange.kinds`, `clientChange.firstChangedMessageIndex` and
  `newlyDropped.reason`, with block and turn counts.
  - Kinds `messagesHistoryChanged` means the message history differs from
    what was sent before. The request then drops every thinking block after
    that index and writes the conversation from there to cache again.
  - Other kinds seen are `none` (reason `other` or `prefix_mismatch`), and a
    list that adds `systemPromptChanged` and `toolSchemasChanged`. Only kinds
    exactly `messagesHistoryChanged` count as a history change here.
- **Per-session cache report (code, unmeasured).** The statusline JSON carries
  a `prompt_cache` object:
  - `warm`, `ttl`, `expires_at`, `hit_ratio`, `misses`,
    `miss_recache_tokens`;
  - `last_miss_cause` and `miss_causes`, from a closed set:
    `system_prompt_changed`, `tools_changed`, `model_changed`,
    `messages_rewritten`, `ttl_expired_5m`, `ttl_expired_1h`,
    `likely_server_side`, `unknown`.

  It is a client-side heuristic. crew writes each member's `settings.json`
  (`agents/claude.rs:178`), so a `statusLine` command could record it.

#### Codex

It uses the Responses API, which caches automatically from 1024 tokens.

- crew's standing instructions go into `developer_instructions`
  (`agents/codex.rs:103-125`). Codex puts them in a developer message at the
  head of the conversation, so they are part of the prefix.
- Unmeasured, not found in the local binary:
  - which cache key Codex sends;
  - its retention.
- The binary bundles OpenAI docs that name `prompt_cache_options.ttl` (`"30m"`)
  for the GPT-6 family.
- `runtime` reports that the current official docs give 30 minutes as the
  minimum for GPT-5.6 and later (m-95a95daed241).
- The same bundled docs say that changing the request-level reasoning effort
  mid-thread changes the prefix.
- Unmeasured: whether `codex resume` with different `developer_instructions`
  replaces the recorded ones or adds new ones.
- **Measured by `runtime`, interim, one session (m-2d12f2182983).** Codex
  session `01a10d19…`:
  - 572 requests;
  - 98.0% of input tokens read from cache;
  - no native write field;
  - two requests after gaps over 1 h, which read almost nothing.

  Its full collector is in `cache-baseline`.

#### pi

**Unknown for crew's supported pi.**

- crew supports `@earendil-works/pi-coding-agent` 1.0 and later. It is not
  installed here, so its caching was not read or measured.
- What crew itself sends to pi is code read in crew:
  - the briefing by `--append-system-prompt` (`agents/pi.rs:363-366`);
  - the tools through `registerMcpServer` when pi has it, else `crew ctl
    tools` at extension load (`assets/crew-pi.ts:186-217`);
  - deliveries as follow-ups (`:145`);
  - compaction through `ctx.compact` (`:111-128`).
- No pi member is on the team now.

The only pi here is GSD's, `gsd-pi` 2.70.1, which crew refuses. Read as an
exploratory observation only, not crew's runtime, its bundled `pi-ai` and
`pi-coding-agent` show the following. Nothing in `cache-design` should rest on
them until the supported pi is checked.

- **Anthropic.** `cache_control` goes on the system block or blocks and on the
  last user message (`pi-ai/dist/providers/anthropic-shared.js:295-366`).
  - With an OAuth token, the system prompt is two blocks, each with a
    breakpoint (`:344-359`).
  - The TTL is 5 minutes unless `PI_CACHE_RETENTION=long`, which gives 1 h on
    `api.anthropic.com` only (`:36-53`).
- **OpenAI.** `prompt_cache_key` is the session id.
  - `PI_CACHE_RETENTION=long` asks for 24 h retention on `api.openai.com`
    (`pi-ai/dist/providers/openai-responses.js:25-33,82-88`).
- **The system prompt ends with the current date and time, to the second, and
  the working directory** (`pi-coding-agent/dist/core/system-prompt.js:55-56,
  179-180`).
  - It is rebuilt at each session start and when the tool set changes
    (`agent-session.js:545,1498`).
  - Everything from that suffix on would therefore change at each launch.
  - How much of the prefix before it still hits depends on the provider and
    the breakpoints, and is unmeasured.

### Measured sample

This is an interim sample of Claude Code sessions only; `cache-baseline`
replaces it and adds Codex and pi. The data was read read-only.

**Reproduce.** `notes/prompt-cache-audit/run.sh` reproduces the token counts,
then `cost.sh` the costs. They write:

- `cohort.tsv`: the sessions read;
- `sessions.tsv`: one row per session;
- `signatures.tsv`: the class, cache read, cache write, gap and model of every
  big write;
- `models.tsv`: tokens per session and model;
- `totals.tsv`;
- `costs.tsv`.

The files are kept as they were for this note. No message text leaves the
scripts.

**Cohort.**

- Every session file under
  `~/.claude/projects/-home-nade-workspaces-personal-crew--crew-worktrees-*/`.
- Every file in `~/.claude/projects/-home-nade-workspaces-personal-crew/`
  whose first plain-text user prompt starts with `[crew]` (the orchestrator
  and members launched in the workspace itself).
- Records stamped after 2026-10-08T04:00:00Z (`CUTOFF=1791432000`) are
  ignored.

**Requests.** A request is one main-thread assistant response, counted once
per `message.id`. Sidechain and `<synthetic>` lines are left out.

**Cost.** Costs are input-side list prices: each request is priced at its own
model's rate from Anthropic's prompt-caching table
(<https://platform.claude.com/docs/en/build-with-claude/prompt-caching#pricing>,
read 2026-10-08, kept in `notes/prompt-cache-audit/prices.tsv`).

| Model | Requests | Base input | 1 h write | Read |
| --- | --- | --- | --- | --- |
| Opus 5.5 | 10,136 | $4 / MTok | 2x base | 0.05x base |
| Fable 5.1 | 4,114 | $10 / MTok | 2x base | 0.025x base |
| Sonnet 5.5 | 1,339 | $2 / MTok | 2x base | 0.05x base |

Output tokens are not counted. These are API list prices; how a Claude
subscription is charged is not modelled. So the shares below describe API
pricing, not what this team paid.

| Measure | Value |
| --- | --- |
| Sessions | 49 |
| Requests | 15,589 |
| Uncached input | 34,178 tokens |
| Cache read | 5,393,044,647 tokens |
| Cache write (all 1 h) | 115,056,954 tokens |
| Hit rate by tokens (read / all input) | 97.9% |
| Input-side list cost: writes / reads / uncached | $1,439.87 / $1,116.27 / $0.19 |
| Cache writes as share of input-side list cost | 56.3% |

Writes are 2.1% of input tokens but 56.3% of input-side list cost. The number
worth moving is write volume, not the token hit rate.

**Big writes.** A big write is a request that wrote at least 10k tokens and
more than it read. Each is classed by the first test it passes; the cache
read ranges are the evidence for each class.

| Class | Test | Cache read (min / median / max) | Events | Tokens written | Share of writes | List cost (share of input side) |
| --- | --- | --- | --- | --- | --- | --- |
| First request | first in the session file | 0 / 23,372 / 23,881 | 29 | 726,680 | 0.6% | $7.28 (0.3%) |
| After compaction | a `compact_boundary` since the previous request | 0 / 28,434 / 29,835 | 4 | 135,831 | 0.1% | $2.37 (0.1%) |
| Model change | another model than the previous request | 0 / 0 / 0 | 2 | 846,025 | 0.7% | $16.92 (0.7%) |
| TTL expired (inferred from gap and signature) | over 3600 s since the previous request and read below 26,000 | 0 / 23,368 / 23,881 | 58 | 24,474,046 | 21.3% | $322.36 (12.6%) |
| History change, declared by the client | a `thinking_drop` for this request id with `clientChange.kinds` exactly `messagesHistoryChanged` | 27,719 / 28,786 / 30,696 | 121 | 41,044,930 | 35.7% | $398.08 (15.6%) |
| Declared system prompt or tool change | a `thinking_drop` whose kinds name `systemPromptChanged` or `toolSchemasChanged` | | 0 | 0 | 0% | |
| History change, undeclared (inferred from signature) | read at least 26,000 (system read, messages written again), no history `thinking_drop` | 27,697 / 30,368 / 31,351 | 48 | 18,635,030 | 16.2% | $364.83 (14.3%) |
| Unexplained | the rest: within the TTL, system prompt not read | 0 / 23,368 / 23,484 | 4 | 1,546,944 | 1.3% | $15.79 (0.6%) |
| Not classed | writes below the threshold (ordinary growth and small rewrites) | | | 27,647,468 | 24.0% | |

The cost column is what the class's writes cost. Had the same tokens been read
from cache instead, each would cost about 2% less (`costs.tsv`, last column).
That bounds what removing a class could save; it is not an avoidable-loss
estimate.

Notes on the classes:

- The 26,000 bound sits between the shared Claude Code prefix (23,368 to
  23,881) and the lowest read that included a session's own system prompt
  (27,697).
- The 177 `thinking_drop` records in the sample split as:
  - 175 with kinds `messagesHistoryChanged`;
  - none naming a system prompt or tool change;
  - 2 with kinds `none` and reason `other`.

  The `none` records are not counted as history changes. One sits on a request
  that wrote 329,177 and read 23,484 (`root/66f738cc`,
  `req_011CfexLpbtwpwM9aK4x62Cf`), which is therefore Unexplained.
- Only 22 of the 49 sessions record a history change at all, so some
  undeclared rewrites may have the same cause. The declared and inferred
  classes are kept apart for that reason.
- The TTL and undeclared classes are inferred from timing and cache-read
  signatures. Only the declared class rests on a diagnostic the client wrote.

**Fold association, with controls.** In the 22 sessions that record a
declared history change:

| Turn before the history change | Turns | Followed by a declared history change |
| --- | --- | --- |
| Contained a folded delivery | 326 | 175 (53.7%) |
| Contained none | 1,106 | 0 |
| Contained none, but had a silent-turn reminder (control) | 99 | 0 |

How the turns are drawn:

- A folded delivery is an `isMeta` user row whose array content starts with
  `[crew]`: crew-bridge's `session.append`.
- Turns end at `system/turn_duration`.
- All 175 declared changes fall on the first request of a turn. That is the
  first main-thread request after the previous `turn_duration`, or the
  session's first. None falls later in a turn.

This is an association, not proof of cause. `cache-bench` will compare the
same workload before and after a delivery change (m-2176088183a2).

**Lifecycle frequencies** from crew's event log (`.crew/crew.db`, read-only,
2026-10-02 14:08 to 2026-10-07 22:53 local time):

| Event | Count |
| --- | --- |
| Messages delivered | 6,704 |
| Session launches | 116 |
| Restarts | 32 |
| Moves to another CLI (fresh session) | 20 |
| `agent_set_model` | 9 |
| Members added / removed | 40 / 25 |
| Compactions (51 queued, 3 by the runtime) | 54 |
| Reminders | 75 |
| "Asked again" notices | 185 |
| Usage-limit parks / lifts | 150 / 149 |
| Daemon recoveries (all adopted, none restarted) | 18 |

Of the usage-limit parks, 51 are `runtime` on "out of credits", resumed every
30 min.

### Ranked cache-breaking points

The ranking goes by measured write volume. Where no number is separable, it
goes by what one event costs. Write volumes are the tokens associated with a
class. They are not avoidable loss: even a fixed event writes some tokens.

1. **A delivery folded into a running turn, followed by a history change at
   the next turn.**
   - **Code:**
     - `assets/crew-bridge.register.ts:76-98`: `deliver` calls
       `$.session.append` while a turn runs. Claude Code records those rows
       with `isMeta: true`.
     - `WAKE` (`:154-166`) starts a turn when appended rows went unread.
     - The daemon pushes over the socket while a turn is active
       (`daemon/delivery.rs`, `Route::Socket`).
   - **Position:** messages, from the changed index on.
   - **What varies:** every delivery that lands mid-turn. 376 of 1,632 turns
     in the sample had one.
   - **Invalidates (measured):** the next turn's first request declares
     `messagesHistoryChanged` and drops later thinking blocks. Everything
     after the changed index is written again.
   - **Write volume:** 121 declared events and 41.0M tokens (35.7% of
     writes), plus possibly part of the 48 undeclared ones (18.6M, 16.2%).
     About 339k tokens per declared event.
   - **Candidates for `cache-design`:**
     - submit each delivery as a queued prompt for the turn boundary (the
       mod's `prompt.submit` path at `:97`, which shows no association),
       accepting the latency;
     - or an in-turn form Claude Code keeps byte-stable.
   - **Other runtimes (unmeasured):** Codex queues through its app-server
     (`agents/codex.rs:293`); pi uses `sendUserMessage(..., { deliverAs:
     "followUp" })` (`assets/crew-pi.ts:145`).

2. **Idle beyond the TTL.**
   - **Code:** crew sets no TTL (`agents/claude.rs:178-188`,
     `agents/pi.rs:398-420`). Anything that starts a turn after the gap
     writes the session again:
     - a delivery (`daemon/delivery.rs`);
     - a reminder (`daemon/supervisor/health.rs:104-128,176`);
     - an "asked again" notice (`daemon/turns.rs:263-299`);
     - an orchestrator notice (`notify_orchestrator`,
       `daemon/handlers.rs:810-819`; `daemon/context.rs:122-180`);
     - `RESUME_PROMPT` after a usage limit (`daemon/limits.rs:311`).
   - **Position:** everything after Claude Code's shared prefix.
   - **What varies:** per member, with how long it waits (review,
     dependencies, overnight).
   - **Write volume (measured):** 58 events and 24.5M tokens (21.3% of
     writes), 422k per event.
   - **Levers:**
     - Claude members on a subscription already get 1 h. One on an API key,
       Bedrock or Vertex gets 5 min unless `promptCacheTtl` is set. The
       endpoint config can already pass `CLAUDE_CODE_PROMPT_CACHE_TTL` per
       agent through `apply_endpoint` (`agents/claude.rs:128-146`).
     - The TTL of the supported pi is unknown (see §How each CLI caches).
     - Compacting before a long park shrinks what is written on waking.
     - A keep-alive turn reads the context once and adds a little to the
       history. One avoided re-write pays for about 39 such turns on Opus 5.5
       or Sonnet 5.5, and about 79 on Fable 5.1: that is (1 h write − read) /
       read at list price, ignoring each turn's output (`costs.tsv`). At one
       turn per TTL hour, that covers a member expected to wake within a day
       or more.
     - Batching wake-ups for the same member.

3. **The briefing is rebuilt at every launch, a resume included.**
   - **Code:**
     - `daemon/supervisor/launch.rs:175-204` builds `TeamView` from the live
       `model.agents`. For the orchestrator it adds the model catalog with
       `Availability::detect()` and the work style's fallbacks.
     - `agents/prompt.rs:45-68` (roster), `:222` (`worker_briefing`), `:364`
       (`orchestrator_briefing`).
     - Delivered by `--append-system-prompt-file` (`agents/claude.rs:212`),
       `developer_instructions` (`agents/codex.rs:103-125`) and
       `--append-system-prompt` (`agents/pi.rs:363-366`).
   - **Position:** system for Claude Code and pi; the head developer message
     for Codex.
   - **What varies:**
     - per member: its id and role in the first line, its branch;
     - per team change: the roster (`agent_add`, `agent_remove`,
       `agent_spawn`, temporary members leaving, role edits), the objective,
       the style instructions and the orchestrator's id;
     - for the orchestrator, also CLI availability.
     - It never changes inside a running session. It holds no timestamps,
       session ids or launch ids; agent ids and branch names are in it.
   - **Invalidates:** on a resume, the system prompt and every message, even
     inside the TTL. Code-based for Claude Code; the 23.4k-only read is what a
     miss would look like there.
   - **Frequency:** 65 roster changes in 5 days.
   - **Evidence:** `scheduler`'s `briefing.md` is from 2026-10-05 22:14. The
     roster now differs from it by two lines (`linux` left, `cache-reviewer`
     joined).
   - **Write volume:** not separable in the sample, because resumes there came
     after gaps over 1 h. One event writes the whole context: 400k to 650k
     tokens for the long-lived members here.
   - **Lever:** keep the briefing byte-identical across a session's launches:
     - reuse the stored `briefing.md` on resume;
     - or move the volatile parts (roster, objective) into `agents_list` or a
       delivered message.

4. **Relaunches.** These paths resume a session:
   - a crash restart, up to `max_restarts` (default 1; `daemon/supervisor/
     exits.rs:127-190`);
   - daemon recovery stopping and resuming a process of another build
     (`daemon/supervisor/recovery.rs:158-199`). The 18 recoveries here all
     adopted the running process instead;
   - a usage limit with `--restart` or a dead process
     (`daemon/limits.rs:302`). A live session is prompted in place instead
     (`:311`), which keeps its cache;
   - a refused 1M window, which restarts on the standard one
     (`daemon/context.rs:57-95`).

   With a stable briefing and inside the TTL, a resume should read its prefix.
   Unmeasured: whether Claude Code adds anything at resume that is not
   appended at the end.

5. **Model and CLI changes.** The cache is per model, so each one writes the
   whole context.
   - **Code:**
     - `daemon/fallback.rs:291` (`agent_set_model`) and `:434`
       (`move_to_runtime`: a fresh session plus handoff,
       `agents/prompt.rs:551`);
     - `daemon/policy.rs`: the work-style fallback, and the move back after
       the reset or `RETRY_HOME_MS` (1 h);
     - `daemon/turns.rs:733`: the refusal fallback;
     - `agents/claude.rs:186` (`switchModelsOnFlag`) and `:258-262`
       (`CLAUDE_CODE_NO_MODEL_FALLBACK` is set only when `model_fallback` is
       off, so Claude Code may switch on its own).
   - **Write volume (measured):** 2 events and 0.85M tokens in the Claude
     sessions. The 20 moves to Codex this week each began a fresh session.
   - **Timing:** a move back home, or any optional move, can wait until the
     member has idled past its TTL or reaches a compaction point.

6. **Compaction.**
   - **Code, Claude Code:** crew-bridge runs a queued compaction at turn end,
     or at once when idle (`assets/crew-bridge.register.ts:62-81,154-160`).
   - **Code, pi:** crew's extension runs it from the same socket order with
     `ctx.compact` and the keep text as `customInstructions`
     (`assets/crew-pi.ts:111-128`).
   - **Code, Codex (and the others without a socket):** `/compact` is typed
     while the member rests (`agents/codex.rs:333`, `agents/pi.rs:499`;
     `daemon/context.rs:250-302`). Codex's command takes no keep text.
   - **Code, both:**
     - the orchestrator is warned at `context_warn_percent`, default 75
       (`daemon/context.rs:122-180`);
     - after any compaction the member gets a reorientation keeping at most
       1000 characters (`daemon/context.rs:364-368,380-428`).
   - **Position:** messages. The summary replaces the history; system and
     tools stay.
   - **Write volume (measured):** 4 events and 0.14M tokens. The rebuild is
     small.
   - **Hidden cost:** the compaction request reads the whole context, at the read rate
     when warm. A member compacted after idling past its TTL writes its whole
     context first.
   - **Timing:** compact at turn end, as crew-bridge already does. Never
     compact a cold idle member unless it is about to wake.

7. **Session start.**
   - **Write volume (measured):** 29 events and 0.73M tokens.
   - Claude Code's own prefix comes from the shared cache; crew's part, 5k to
     7k tokens, is written per start.
   - The per-member first line (`agents/prompt.rs:222-232`) keeps the rest of
     the 9.5 KB briefing from being shared across members. That saves a few
     thousand tokens per launch at most, so this is low priority.

8. **Turns crew starts.** This is cost, not hit rate: each turn reads the
   whole context once at the read rate (0.025x to 0.05x base input for the models here).
   - **Deliveries.** A 1.5 to 5 s gather (`daemon/delivery.rs:46-50`), and
     status news waiting for company (`:52-63`), already merge some.
   - **Reminder** (`daemon/supervisor/health.rs:104-128`).
     - Fires only when the member is idle and ready, and its active card
       (`tasks::active_task`) is `Working`.
     - The first comes after 90 s idle, then one per 90 s, at most 3, then
       the member is reported unresponsive.
     - In the event log, all 75 reminders came while the card's latest state
       event was a dispatch (74) or `changes_requested` (1). None came while
       the card was in Review.
   - **"Asked again"** (`daemon/turns.rs:263-299`, `agent.unanswered`).
     - After a turn that called no tool and forwarded no reply, following a
       task, question, review request or message (`asked_of`, `:451-469`),
       crew sends one notice. If the next turn again does nothing, it is taken
       as idle (`agent.idle_turn`).
     - It is not gated by the card's state. `graph` got one at 22:57:53 with
       `toolbox-audit` in Review, after a designer message
       (m-76515cb7968a).
     - It fired 185 times, more than reminders.
   - **A refused waiting report.** `agent_status("waiting")` for a card that
     is not `Working`, `Waiting` or started `Blocked` is refused ("no started
     work", `daemon/orchestrate.rs:798-807`). A member whose card is in Review
     has no accepted way to say it waits, which leads to more turns.
   - **The rest:**
     - `WAKE`;
     - orchestrator notices;
     - the compaction reorientation;
     - `RESUME_PROMPT`, every 30 min while a limit lasts;
     - the first prompt (`agents/prompt.rs:448,465`;
       `daemon/supervisor/launch.rs:350-384`).
   - **Tool output** stays in the context until a compaction. `tasks_list` on
     this board returns 90 KB (about 22k tokens), read again by every later
     request.

9. **Claude Code's silent-turn reminders.** "The user hasn't heard from you in
   a while ..." is controlled by `CLAUDE_CODE_SILENT_TURN_REMINDER*`.
   - It works against crew's member tone, "Work silently between tool calls"
     (`agents/prompt.rs:152-169`).
   - Measured: it only appends. 136 turns without a fold had one; none was
     followed by a history change.
   - Low priority.

10. **Tool definitions.**
    - **Code:** crew's tools are static per build and role. Descriptions are
      `&'static str` with sorted schema keys (`mcp/tools.rs:44`).
      `listChanged` is `false` and `instructions` a fixed string
      (`mcp/mod.rs:131-134`).
    - On Claude Code with tool search they are deferred, so a change only
      appends. Otherwise, and on Codex, they are prefix (pi presumably too, unverified).
    - **Edge:** if the daemon cannot be reached at `tools/list`, the
      orchestrator is given the worker list (`mcp/mod.rs:45-64`). Its next
      launch would then see another list wherever the tools are prefix.
    - `inherit_mcp` adds the operator's servers to Claude
      (`agents/claude.rs:207`); crew does not control their stability.
    - **Write volume:** none measured.

### What crew sends, by runtime

| Item | Runtimes | Position | Varies with | Breaks on change |
| --- | --- | --- | --- | --- |
| crew tool list and schemas (`mcp/tools.rs:44`, filtered at `mcp/mod.rs:98`; pi via `registerMcpServer` or `crew ctl tools`, `cli/agent_side.rs:77-88`) | all | Claude with tool search: messages (deferred). Otherwise, and on Codex: tools; pi: presumably tools (unverified) | role, crew build | where prefix: everything; deferred: nothing (appends a delta) |
| MCP `instructions` (`mcp/mod.rs:133`) | Claude, Codex | Claude: attachment; Codex: context | crew build | as above |
| Briefing (`agents/prompt.rs:222`, `:364`) | all | system (Claude, pi); developer message (Codex) | member, roster, objective, style, branch; orchestrator: catalog | system and messages, at the next launch |
| Member tone (`agents/prompt.rs:152`) | Claude (replaces the `tone` section, `assets/crew-bridge.register.ts:186-199`), Codex (in `developer_instructions`); not pi | system | voice only | no (constant) |
| `model_verbosity = "low"` for members (`agents/codex.rs:117`) | Codex | request parameter | constant | no |
| Operator's own `developer_instructions` (`agents/codex.rs:36-43`) | Codex | developer message | the operator's config | messages, at the next launch |
| `settings.json` (`agents/claude.rs:178-188`) | Claude | none directly; no TTL set | endpoint | no |
| Hooks (`cli/agent_side.rs:107-137`) | Claude hooks, Codex `notify`, pi extension | nothing: they never print | | no |
| First prompt: standby, kickoff with live usage, handoff, `RESUME_NOTICE`, pending messages (`daemon/supervisor/launch.rs:350-384`) | all | messages (appended) | per launch | no |
| Delivered messages, `[crew] Message from ...` (`agents/prompt.rs:724`, 4000 characters each) | all | messages (appended); Claude: folded mid-turn | per message | Claude fold: rank 1 |
| `WAKE` (`assets/crew-bridge.register.ts:18`) | Claude | messages | | no (one more turn) |
| Reminder (`daemon/supervisor/health.rs:176`) | all | messages | | no (one more turn) |
| "Asked again" notice (`daemon/turns.rs:270-276`) | all | messages | | no (one more turn) |
| `RESUME_PROMPT`, `RESTART_NOTICE` (`daemon/limits.rs:24-30`); recovery notice (`daemon/supervisor/recovery.rs:192`) | all | messages | | no; the restart path is ranks 3 and 4 |
| Compaction keep text and reorientation (`daemon/context.rs:380-428`) | Claude and pi (keep text as instructions), Codex (reorientation only) | messages after the summary | per compaction | the compaction itself is rank 6 |
| `agent_set_model` note, handoff (`daemon/fallback.rs`, `agents/prompt.rs:551`) | all | first message of the new session | per move | the move is rank 5 |
| Task dispatch (`agents/prompt.rs:701`) | all | messages | per card | no |
| Tool results (renderers in `mcp/tools.rs`) | all | messages | per call | no; they grow the context |
| `--model` with `[1m]` (`agents/claude.rs:241-249`); Codex `model_context_window` and `model_auto_compact_token_limit` (`agents/codex.rs:126-131`) | Claude, Codex | model | a refused 1M window | everything (rank 5) |

### Session lifecycle

A launch is one of three kinds:

- **New:** a chosen session id, passed as `--session-id`.
- **Resume:** `--resume <id>`, `codex resume <thread>`, or pi `--session-id`.
  A launch counts as a resume only once the session id was confirmed
  (`daemon/supervisor/launch.rs:205`).
- **Fresh:** a new id and a handoff prompt. Used for a move to another CLI and
  for an operator's "fresh" restart.

| Trigger | Kind | Code | Avoidable, or when to do it |
| --- | --- | --- | --- |
| Team start or member added | New | `daemon/supervisor/creation.rs` | Inherent. About 5k to 7k tokens of crew system per start. |
| Crash | Resume, up to `max_restarts` (default 1) | `daemon/supervisor/exits.rs:127-190` | Inherent. No write if the briefing is stable and the TTL holds (unmeasured). |
| Daemon restart, same build | none (adopted) | `daemon/supervisor/recovery.rs:48-110` | Already avoided. |
| Daemon restart, other build | Resume | `daemon/supervisor/recovery.rs:158-199` | Restart the daemon while members are idle and warm; keep the briefing stable. |
| Usage limit lifted, process alive | prompt in place | `daemon/limits.rs:311` | Keeps the cache if lifted within the TTL. A 30 min retry against an unchanged limit (`runtime`, 51 times) is a turn each time. |
| Usage limit, `--restart` or process gone | Resume | `daemon/limits.rs:302` | As for a crash. |
| 1M window refused | Resume on another model variant | `daemon/context.rs:57-95` | Inherent, once per member. |
| Work-style fallback and the move back home | Resume on another model | `daemon/policy.rs` | Move back only once the member is idle past its TTL. |
| `agent_set_model` | Resume on another model | `daemon/fallback.rs:291` | The orchestrator's choice; best at a compaction point. |
| Move to another CLI | Fresh plus handoff | `daemon/fallback.rs:434` | Inherent. The handoff carries no history. |
| Compaction | in place | `daemon/context.rs:184-302`; `assets/crew-bridge.register.ts:62-81`; `assets/crew-pi.ts:111-128`; `/compact` typed for Codex | At turn end, while warm. |

### Open for the next cards

- `cache-baseline` (runtime):
  - Codex `cached_input_tokens` per event class, including a resume with
    changed `developer_instructions`.
  - Its own reading of the fold association. The keys are in §Method.
- `cache-bench` (qa): the same workload before and after a delivery change.
  Later:
  - a restart inside the TTL with an unchanged briefing, and with one roster
    line changed;
  - compaction while warm and while cold.
- `cache-design`:
  - a delivery that never changes history (rank 1);
  - a briefing stable across launches (rank 3);
  - TTL per member, and the keep-alive policy (rank 2);
  - the timing of optional model moves (rank 5);
  - fewer turns started by crew (rank 8): a waiting state for a card in
    Review, and an "asked again" that skips news.
- `toolbox-spec`: per-member MCP servers and skills add tool definitions.
  Wherever these are prefix (Codex, pi, Claude without tool search or with
  `alwaysLoad`), they must be byte-stable, deterministically ordered and set
  only at launch.

### Method

The scripts are in `notes/prompt-cache-audit/`: `run.sh` builds the cohort,
and `session.sh` classifies one session. The class tests are in the comment at
the top of `session.sh`.

Keys read from each Claude Code session file:

- **Requests:** `type == "assistant"`, not `isSidechain`, with
  `.message.usage`. Counted once per `.message.id`, because one response is
  written as one line per content block, each carrying the same usage. From
  each, `.requestId` and the usage fields `input_tokens`,
  `cache_creation_input_tokens`, `cache_read_input_tokens` and
  `cache_creation.ephemeral_{1h,5m}_input_tokens`.
- **History changes:** `type == "attachment"` with `.attachment.type ==
  "thinking_drop"` and `.attachment.clientChange.kinds` exactly
  `"messagesHistoryChanged"`.
  - Joined to the request by `.attachment.requestId`. It also carries
    `.attachment.newlyDropped.reason`.
  - A drop naming `systemPromptChanged` or `toolSchemasChanged` is classed
    apart.
  - Kinds `none` is not counted. Every drop is still tallied by kind in
    `sessions.tsv`.
- **Folds:** `type == "user"` with `isMeta == true`, array content, and
  `.message.content[0].text` starting with `[crew]`. A delivery submitted
  while the member rests, and `WAKE`, are plain string content and not
  `isMeta`.
- **Turn ends:** `type == "system"`, `subtype == "turn_duration"`.
- **Compactions:** `subtype == "compact_boundary"`.
- **Silent-turn reminders:** `.attachment.type == "silent_turn_reminder"`.

Records are taken in file order, because attachment timestamps can sit a
millisecond before the line that precedes them. A resumed session appends to
the same file.
