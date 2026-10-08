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

The cost column is what the class's writes cost. Reading the same tokens from
cache would cost 1.25% to 2.5% of that (read rate over 1 h write rate). So the
most that removing a class could save is its write cost minus that read cost,
about 2% below its write cost (`costs.tsv`, last column). That is an upper
bound, not an avoidable-loss estimate.

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

## Design

Task `cache-design`; proposed implementation contract, not a claim that the
changes or their savings have shipped. Product source inspected at `df4575cf`;
the prompt and delivery paths still match the audit's `c8052373`. The measured
input is `cache-baseline` at `d14b7e50`, [baseline.md](prompt-cache/baseline.md)
and its sanitized timeline, now landed on main `b5bdcc86`. Toolbox requirements are
[Spec](toolboxes.md#spec), including the approved capability probes. The
implementation acceptance workload is `cache-bench-lifecycle`, not the short
invoice workload alone.

### Evidence and priorities

Optimize the input that must be processed again and the cost of completing the
work, while preserving delivery and task correctness. A high hit rate alone
can reward unnecessary requests or a needlessly large context.

The final baseline covers 24,129 native observations. Claude read
5,506,578,462 of 5,623,886,457 input tokens (97.9141%), with 117,271,674 explicit
writes. Codex read 1,799,755,520 of 1,839,395,631 (97.8449%); all 8,057 Codex
observations contain a native write counter, and all those counters are zero.
There is no measured supported-pi workload. Do not carry forward the audit's
interim statement that Codex has no write field, or add its earlier cohort to
this one.

| Priority | Change or decision | Measured exposure, not recoverable tokens | Runtime scope |
| --- | --- | --- | --- |
| 1 | D1: queue ordinary Claude deliveries at turn boundaries; preserve immediate operator steering | 183 strict history-change diagnostics, 44,113,859 writes (37.6% of Claude writes). In diagnostic-emitting sessions, 183/340 requests after folded turns versus 0/1,135 controls declared a history change. Another 19,776,027 writes are fold-associated without that diagnostic and remain unproved. | Claude first. Codex and pi already use queued delivery paths; test for regressions, do not ascribe Claude's savings to them. |
| 2 | D2: measure effective retention; use an existing supported TTL override only where beneficial; no automatic keep-alive | Gaps over an hour precede 26,349,326 non-read input tokens, including 23,253,702 writes, across runtimes. Ordinary new input and retention/routing uncertainty are included. | Claude has explicit one-hour write buckets in this baseline. Codex retention is unobserved per request. Supported pi remains unmeasured. |
| 3 | D3: stable standing instructions and resume artifacts; live team data at the tail | Restart overlaps contain 2,462,046 non-read tokens and 1,018,900 writes, not a causal estimate. Only two observations have primary resume association. No observed baseline separates the briefing's cost. | All three adapters; validate each runtime's actual treatment of resume arguments. |
| 4 | D4: reduce redundant wake-ups without suppressing obligations | Reminder and nudge primary classes contain 3,368,136 and 3,869,786 non-read tokens respectively. Neither number measures the cost of redundant reminders alone. Every extra request also rereads cached history. | Shared daemon; measure request count and completion cost separately by runtime. |
| 5 | D5: keep necessary lifecycle changes at safe boundaries | Compaction association: 1,426,278 non-read tokens, 420,175 writes. Crew model/runtime-change association: 1,616,017 non-read tokens. These classes do not justify a new automatic compaction or model-switch policy. | Shared daemon plus each runtime's native compaction. |
| 6 | D6: pin definitions and prove final tool order; put common prompt text first | No measured tool-change loss attributable to crew. Startup primary class: 1,629,184 non-read tokens across runtimes. Audit's separate Claude sample estimates 5k–7k tokens of crew/session prefix per start. | Toolbox work is required for predictable scoping regardless of cache savings. Cross-member prefix reuse is secondary. |

These primary classes partition the collector's observations; overlapping
restart/delivery events do not. `avoidable_miss_tokens` remains null. The
older audit's 56.3% cache-write share is an API list-price calculation for its
own Claude cohort, not this team's invoice. Ranking beyond the first two rows
is based on mechanism and implementation risk, because controlled per-event
loss is not yet established.

QA's completed short control (`5275807e`, runner `a1cd0cf5`) measured 66
requests, 94.1114% weighted hits, and $0.425565–$0.437488 API-equivalent cost.
Its per-member hit rates were 89.6120% / 95.0204% / 95.1831% for orchestrator /
Claude worker / Codex worker. It has only a 20.4535-second idle gap. It remains
a control, not the acceptance baseline for lifecycle changes.

The lifecycle pilot is also not the final series: its successful resume steps
read almost all their input despite a changed briefing file, before another
step invalidated the run. The corrected Haiku fold probe observes a folded row
and subsequent thinking blocks without a `thinking_drop`. Thus neither a file
delta nor an injected row proves a provider miss. Review the final three-run
series before attributing a benefit to either D1 or D3. At this design's
2026-10-08 checkpoint, QA's series uses runner `0d7f506d` and is still running;
its final numeric report is not an input claimed here.

### D1. Preserve message history during ordinary delivery

Choose turn-boundary delivery for ordinary teammate traffic to Claude Code.
Keep the daemon's durable bus as the queue while `turn_active` is true; do not
acknowledge a message merely because it entered a plugin's in-memory buffer.
At turn completion, gather pending messages with the existing 1.5-second quiet
window and five-second maximum, then submit one user prompt through the
runtime's supported queue. Old message and thinking blocks must remain
unchanged. A race in which a turn starts after the daemon's check uses the
runtime queue, never a fallback to `session.append` for ordinary traffic.

This changes the automatic delivery latency of teammate questions, answers,
reviews and status. They remain visible immediately through `agent_inbox`.
A worker awaiting an answer must end its turn or park its card, as the standing
instructions already require; the daemon must not rely on a polling turn to
flush the queue. No additional delay is introduced once a member can consume a
prompt beyond the existing gather window. Record time queued, runtime-accepted
and first observed model consumption separately; a transport acknowledgement
is not evidence of consumption.

Operator-origin messages keep the existing immediate steering path, selected
by authenticated sender identity, not by parsing the message body. Stop,
pause, permission and compaction control paths retain their own semantics.
Split a mixed pending batch by delivery policy: an operator message can
overtake ordinary queued traffic, but must not carry that traffic into an
immediate append. Preserve message order within each policy and retain every
id. A body claiming to be from the operator does not change its policy.
If immediate steering still needs a history-mutating native operation, label
that delivery as such in diagnostic metadata and exclude it from the ordinary
queued-delivery performance claim. Do not delay an operator correction to
improve the cache metric.

The implementation must preserve the existing at-least-once contract and its
20-second in-flight fallback. A message held in the durable queue is not
in-flight; the fallback timer starts only when a runtime submission is
attempted. Confirm only runtime acceptance of the identified message batch,
not an unawaited `prompt.submit` call. Verify the native API's acceptance
boundary before implementing the acknowledgement: a promise which resolves
only after the model finishes is not a submission receipt and cannot hold the
20-second transport timer open. If no accepted-queue receipt exists, use an
identified native turn-start acknowledgement and keep the durable copy until
then; test the busy-turn race without converting waiting to terminal fallback.
A failed submission leaves its ids pending. Deduplicate an already accepted
batch on same-session retry; a crash
between native acceptance and acknowledgement may still replay under the
existing at-least-once contract, and must be reported as such. Never claim
exactly-once delivery from an in-memory deduplication set.

Keep the existing reply-routing meaning: the batch that opens the next turn
belongs to that turn's `turn_openers` and `turn_messages`. A later review or
answer must not be forwarded as an answer to the previous turn's sender. The
`WAKE` workaround is unnecessary for the ordinary queue; retain it only where
the immediate path can leave an unseen appended row.

**Regression contract D1:** a fake runtime holds a foreground tool call while
several identified messages arrive. Assert zero ordinary `session.append`
calls, no acknowledgement or terminal fallback while durably held, one ordered
batch at the next boundary, correct reply recipients, and unchanged serialized
prior messages. Race turn start/end, failed submission, reconnect, duplicate
acknowledgement and process death. Test immediate operator steering separately.
Include mixed-policy batches and a forged operator label in an ordinary body.
Run the same correctness workload through Codex's app-server and pi follow-up
paths. The live fold scenario must still deliver and act on the same messages;
removing the work from the benchmark is not an optimization.

### D2. Retention and idle policy

Keep provider and CLI defaults unless a supported, measured override is
explicitly configured. For Claude, the existing endpoint environment can carry
`CLAUDE_CODE_PROMPT_CACHE_TTL=1h`; it takes precedence over `promptCacheTtl` in
the audited CLI. This is useful to test for a five-minute writer that often
resumes between five minutes and an hour. The production baseline already has
one-hour writes, so enabling it there cannot recover the over-hour cohort.
A longer TTL has a different write price: compare all input and output cost,
not hit rate alone. Validate the native TTL bucket rather than inferring it
from subscription type. See [Anthropic's cache lifetime and usage
contract](https://platform.claude.com/docs/en/build-with-claude/prompt-caching).

For GPT-5.6 and later, current [OpenAI caching
documentation](https://developers.openai.com/api/docs/guides/prompt-caching)
specifies `prompt_cache_options.ttl: "30m"` as a minimum lifetime, with
automatic routing; a shared `prompt_cache_key` is not needed for routing
optimization on these models. Earlier models use different controls. These
are API options, not evidence that Codex 0.159.0 accepts matching `-c` keys.
The inspected [Codex configuration
reference](https://learn.chatgpt.com/docs/config-file/config-reference) exposes
no `prompt_cache` setting. Crew will not invent a CLI setting, rewrite runtime
requests, or force one cache key across members. Preserve native thread IDs on
resume. Measure the actual model, endpoint, region when known, and usage.

For supported pi 1.1.0, capability probes establish toolbox behavior, not
provider caching or TTL. Do not configure it from the rejected GSD binary's
`PI_CACHE_RETENTION` strings. A provider-specific source/native-request probe
and a real workload are prerequisites to a pi retention recommendation.

Do not introduce periodic keep-alive prompts, dummy tool calls, or early
compactions to manufacture hits. They consume paid requests, append context,
can wake useful work, and cannot guarantee a future cache entry. For a future
opt-in experiment, preregister the expected idle duration, native TTL, token
cost of each wake and output, and the probability of useful reuse. Its total
cost must beat the no-wake control; the audit's simplified write/read ratio is
not that experiment. Do not increase the frequency of usage-limit retries to
keep caches warm.

**Regression contract D2:** identical endpoint settings produce identical
launch arguments and nonsecret prompt bytes; credential rotation leaves them
unchanged. Explicit supported TTL values pass through unchanged, including
operator precedence. With no override, no new TTL/key argument or wake timer
appears. Model, endpoint and effective TTL changes are comparison boundaries,
not silently mixed samples.

#### D2 revision: keeping idle Claude members warm

Task `cache-retention-policy`. This is a measured proposal and a
preregistered experiment. Nothing changes by default until the three pairs
below pass. It replaces the blanket "no keep-alive" above for one case only:
Claude Code members at rest, opt-in, through crew's own text path.

**What a long gap costs.** QA's lifecycle run `lifecycle-final-01` (runner
series `ffbeb0be`, Haiku 4.5 and `gpt-6-luna`) measured the first request
after each idle step:

| Member | After ~5 min (native gap) | After > 1 h (native gap) | Written after > 1 h |
| --- | --- | --- | --- |
| Claude worker | 99.8% read (320 s) | 55.9% read: 23,940 of 42,805 (3,678 s) | 18,855 tokens at the 1 h price |
| Orchestrator | 99.7% read | 51.6% read: 23,715 of 45,955 | 22,231 tokens at the 1 h price |
| Codex worker | 97.9% read (515 s) | 26.9% read: 6,912 of 25,738 (3,684 s) | not reported (18,826 uncached) |

The part read after an hour (~23.4k tokens) is Claude Code's shared prefix,
which other sessions keep warm. Everything after it was written again.

In the production baseline (24,129 requests, 2026-10-02 15:14Z to 10-08
04:43Z), 62 Claude requests that opened a turn after more than an hour wrote
24.5M tokens again: 33 on Opus 5.5, 22 on Fable 5.1, 7 on Sonnet 5.5. Priced
as write minus read, that is $317 at list price. The whole window's Claude
traffic is $2,957 API-equivalent (Codex: $813). These gaps are long: median
1.6–2.9 h by model, 90th percentile up to 43 h. Contexts are 200k–700k
tokens.

**Retention each runtime supports.**

- Claude Code writes in 5-minute or 1-hour buckets. This team's writes are all
  1 h (subscription). `CLAUDE_CODE_PROMPT_CACHE_TTL=1h` gives an API-key
  member the same. Nothing longer exists. A read refreshes an entry for its
  TTL, so the only supported way to carry a session's cache across more than
  an hour of rest is a request within each hour.
- Codex 0.159 exposes no cache setting (D2 above). Measured retention lies
  between 515 s (97.9%) and 3,684 s (26.9%). The OpenAI API's retention
  options are not reachable through the CLI, and crew will not rewrite its
  requests.
- Supported pi: unmeasured. No policy.

**Cost of a keep-alive turn and the rewrite it avoids.** For a context of
`C` tokens, of which `C'` would be written again after expiry (all but the
shared prefix):

- keep-alive turn `k = C·r + Δin·w + Δout·o`, with read price `r`, 1 h write
  price `w`, output price `o`; `Δin` (crew's line, the CLI's attachments, the
  reply) is taken as 200–2,000 tokens and `Δout` as 20–400;
- avoided rewrite `S = (C' − t)·(w − r)`, where `t` (0–10,000) is the new
  message the next turn brings anyway.

For a 300k context: Opus 5.5 `k` = $0.062–0.084, `S` up to $2.34; Fable 5.1
`k` = $0.080–0.135, `S` up to $5.93; Sonnet 5.5 `k` = $0.031–0.042, `S` up to
$1.17. The ratio `(w − r)/r` bounds how many keep-alives one avoided rewrite
pays for: 39 on Opus 5.5 and Sonnet 5.5, 79 on Fable 5.1, 19 on Haiku 4.5
(prices in [retention/prices.tsv](prompt-cache/retention/prices.tsv), read
2026-10-08). For reference on Codex, `(input − cached)/cached` is 19 on
`gpt-6.1-sol` and 9 on `gpt-6-astra` and `gpt-6-luna`.

**Break-even rule.** Send the `n`-th keep-alive to an idle member only while
`n · k < p · S`, where `p` is the chance the member works again before the
policy stops. Since `k ≈ C·r` and `S ≈ C·(w − r)` for large contexts, this is
`n < p · (w − r)/r`. A member that never works again makes every keep-alive a
loss, so `p` falls with the length of the rest, and a cap stands in for it.
The cap is chosen from the team's own history, next.

**Retrospective simulation.**
[retention/simulate.mjs](prompt-cache/retention/simulate.mjs) replays every
idle stretch in the baseline timeline. A policy sends the first keep-alive 55
minutes after a member's last request, then every 55 minutes, up to a cap.
It only fills gaps between turns, never inside one. It skips gaps that end in
a limit lift, a pause, a restart, or with the member unresponsive.

For each policy the script counts:

- what the keep-alives cost, including those wasted on a session's last rest
  or on gaps the cache would have survived;
- which rewrites they would have avoided.

[retention/results.json](prompt-cache/retention/results.json) has every
policy. Ranges pair costly keep-alives and a large new tail (conservative)
with cheap ones and none (optimistic):

| Claude, cap | Keep-alives | Their cost | Gaps kept warm | Rewrites avoided | Net |
| --- | --- | --- | --- | --- | --- |
| 1 | 99 | $7.6–10.4 | 23 | $135.9–138.8 | +$125.5–131.2 |
| 4 | 290 | $20.9–28.4 | 39 | $212.3–217.1 | +$184.0–196.2 |
| 6 | 396 | $27.7–37.7 | 46 | $228.0–233.5 | +$190.3–205.7 |
| 8 | 490 | $33.8–46.1 | 46 | $228.0–233.5 | +$182.0–199.7 |
| 24 | 1,183 | $77.2–108.1 | 53 | $263.5–269.7 | +$155.5–192.5 |

A cap of 6 (warm for about 5.5 h after the last turn) is best. That is about
6.4–7.0% of the window's Claude cost. Most keep-alives (291 of 396) buy
nothing; the rewrites they avoid are large enough to carry them. A minimum
context of 50k or 150k tokens moves the net by $1.3 at most.

Codex loses at every cap, even assuming 30-minute retention:

- 30-minute retention: −$1.6 to −$2.8 at cap 1, −$5.2 to −$8.4 at cap 6;
- 10-minute retention: −$9.9 to −$12.5 at cap 1.

Codex members get no keep-alive.

This is a replay of what happened, not a controlled result. Keep-alive cost
and coverage are modelled, not observed. The timeline does not record a
member's status, so the replay counts every rest. The policy below is
narrower: only members Waiting on open work. The replay is therefore an
upper bound on both its keep-alives and its savings. The pairs below test
the model.

**Mechanism.** The keep-alive is crew's own line, through the same path as a
reminder (`Daemon::inject`), never a message:

> [crew] Cache keep-alive: nothing has changed and nothing is asked of you.
> Reply with just "ok", call no tools, and end your turn.

- **Who gets one.** A Claude Code member is eligible when all of these hold:
  - it is Waiting on open work: a member with a parked card and none
    executing, or the orchestrator while the team has any card open;
  - the team has not finished;
  - it is ready and no turn is running;
  - it is not paused and not at a usage limit;
  - no compaction is queued, and no waking delivery is pending;
  - its last reported context is at least the threshold.
- **Who does not.** Not a member with executing work: nudges cover it. Not
  one without open work (Done or Idle between cards), which may never be
  given another. Not one that is Blocked, retiring or temporary.
- **Usage limits.** It is skipped while any Claude Code usage window is 80%
  used or more, so keep-alives never spend the last of a window.
- **D1.** It is queued at a turn boundary like any own-text line. A turn
  that starts after a keep-alive was handed over, with nothing put into
  the session since, is the keep-alive's (`Daemon::keepalive_turn`, for
  both hook-reported and native-bridge turns). If a delivery joins that
  turn, `delivered` makes the member Working and the turn is ordinary work
  from then on. A message put into its session (D1 delivery) restarts the
  count of keep-alives for that rest. No change to `turn_ended` or `inject`.
- **D4.** It is not a message and its turn is not work:
  - nothing goes to an inbox, and it raises no obligation or follow-up;
  - it neither counts as nor resets a reminder or nudge;
  - the member stays Waiting while its turn runs and after: the turn's
    start does not make it Working, and its parked card (or the team's
    open cards, for the orchestrator) keeps it Waiting when the turn ends;
  - so the planner sees no progress. Were the member shown Working, a
    stalled team's stall would end with the turn and be reported to the
    orchestrator again 12 s later: a message and an orchestrator turn
    after every worker keep-alive (found by qa and reviewer on
    `ef969fb9`; regression `a_keep_alive_turn_does_not_report_a_stall_again`).
- **Visibility.** Each keep-alive is an `agent.keepalive` event, emitted
  before the line is handed to the session, so it precedes every request
  the keep-alive causes. Its data holds `n`, `max`, `rested_ms`,
  `dispatched_at_ms` and the exact `text` sent. A hand-over that fails adds
  `agent.keepalive_failed`, and that keep-alive does not count.
- **History.** Each keep-alive adds `Δin + Δout` to the session. The cap
  bounds it.

Settings, all in `limits`:

| Setting | Meaning | Default |
| --- | --- | --- |
| `keep_cache_warm_secs` | Seconds after a member's last turn before a keep-alive. Below the 1 h TTL; 3300 recommended. | 0 (off) |
| `keep_cache_warm_max` | Keep-alives per rest. | 6 |
| `keep_cache_warm_min_tokens` | Smallest context worth keeping warm. | 50,000 |

**Preregistered experiment: three long-gap pairs.** QA's lifecycle runner and
authorization; template and models as `ffbeb0be` (Haiku 4.5, `gpt-6-luna`),
sandbox off.

The lifecycle baseline stays as it is. In it the team finishes before the
long gap (the orchestrator is Done with a summary), so no member would be
eligible. Both arms therefore add the same D2 setup, and the original
lifecycle template is not changed:

- **Sentinel cards from the start.** Each worker gets a sentinel card when
  the team is seeded, depending on its warm card. Adding sentinels only
  after the team is done would be too late: creating a task clears
  `completed_at` but leaves `session.summary`, and a team with a summary
  gets no keep-alives.
- **Re-parked before each idle.** Before each idle step the runner has each
  worker park its sentinel explicitly (`agent_status("waiting", …)`), so it
  rests as Waiting on open work.
- **Never finished.** The sentinels stay open through the measurement, so
  the team never gets an all-done summary and the orchestrator rests as
  Waiting.
- **Stall notice drained.** Once every worker has parked, the team has
  stalled, and 12 s later crew tells the orchestrator once
  (`scheduler.stall`). That notice is part of crew's lifecycle in both arms
  and is not suppressed. The runner waits for it, and for the
  orchestrator's turn on it to end, before the measured idle starts. The
  same stall is not reported again while the team stays stalled, and a
  keep-alive turn does not end it. A second `scheduler.stall`, or any other
  message to a member, during the idle makes the pair invalid.
- **Orchestrator probe.** After the gap, both arms send the orchestrator
  the same explicit work probe. With sentinels open, no all-done notice
  arrives to make it act, so without the probe there might be no
  orchestrator request to measure.
- **D2 steps only.** The lifecycle's idle-delivery, fold and after-fold
  probes are left out of both arms: they test D1, whose own acceptance
  covers them. One frozen retention plan serves both arms.

QA's `cache-retention-bench` card fixes the exact steps. Same models,
catalog and build in both arms.

- **Control arm.** The existing long idle: a measured gap over 3,600 s since
  each member's last work request, then the probes.
- **Treatment arm.** The same, plus:
  - `keep_cache_warm_secs: 3000`, `keep_cache_warm_max: 6`,
    `keep_cache_warm_min_tokens: 0` (bench contexts are ~45k);
  - this sends one keep-alive to each Claude member at about 50 minutes;
  - a keep-alive turn is the member's first native turn after its
    `agent.keepalive` event (not followed by `agent.keepalive_failed`) whose
    prompt is exactly the event's `text` and whose boundary is complete;
    the runner's quiet check ignores its requests only if no
    `message.delivered` or `agent.reminded` event for that member falls
    between the keep-alive event and that turn's last request; anything
    else is pending or invalid, never skipped;
  - a keep-alive turn that a delivery joined is work: the pair is retained
    and marked invalid, never excluded;
  - the matching rule is provisional until a native turn marker exists;
    a mixed or ambiguous turn is retained and marked invalid;
  - both arms run one daemon build, models and catalog, with only the
    setting changed;
  - gaps are measured from the last other request;
  - Codex gets none in either arm; its read share is reported in both as a
    control.
- **Pairing.** Arms run side by side in the same window. Pairs are matched by
  ordinal, decided before the runs, as in §Acceptance.
- **Measure, per Claude member and pair:**
  - the priced upper cost of the keep-alive turn(s) plus the post-gap probe
    step, against the control's probe step;
  - the first post-gap request's read share;
  - requests, output tokens and elapsed time.
- **Expected.** From `lifecycle-final-01`:
  - control: first request about $0.04–0.05;
  - treatment: keep-alive about $0.005–0.010 plus a warm first request
    about $0.005–0.006.
- **Pass.** All of these:
  - in at least two of three pairs and at the median, the treatment total is
    below the control for each Claude member;
  - the treatment's first-request read share is at least 95%;
  - every fixture oracle passes;
  - the keep-alive causes no message, obligation notice, reminder or nudge;
  - the member's status before, during and after its keep-alive is the
    same (Waiting).
- **Fail.** If the pairs fail, close as a documented no-op and remove the
  setting.
- **If it passes.** The setting stays opt-in. Making it the default (3300, 6,
  50,000) needs production evidence: the `agent.keepalive` association in
  the baseline collector, compared with the over-one-hour class above.


### D3. Separate standing policy from live team context

Generate standing text in this order: common coordination/work rules, selected
voice and role-class rules (worker/orchestrator/subagent), team standing
instructions, then fixed member identity and workspace details. In Codex,
place crew's common member tone before crew's identity suffix; preserve the
operator-instruction inheritance rule from the toolbox spec. Do not replace
provider instructions, change a real working directory, omit identity, or pad
text merely to meet a cache threshold. Cross-member equality applies only up
to the first genuine difference; runtime-injected environment text may stop
reuse before crew's text does.

Remove the live roster, model availability catalog and current objective from
the standing prefix. Put a canonical team-context message immediately before
the initial task/standby message. Sort roster entries by id; include identity,
role and relationship, not progress, timestamps or current status. The task
board remains authoritative through `task_get`; the current roster remains
available through `agents_list`. A model catalog belongs to the orchestrator's
context message only, and is validated again at the action that uses it.

Persist the applied standing artifacts for the confirmed native session,
including the fully composed Codex developer instructions and Claude tone
replacement, not just `briefing.md`. A small versioned manifest records the
native session/runtime, prompt-format version, input-policy digests, artifact
hashes and last delivered team-context digest. Keep the manifest in the
member's private launch state. It contains no credential values and is never
injected into the prompt.

On crash restart, ordinary resume or daemon recovery of the same session with
unchanged standing policy, reuse the exact bytes. A roster/catalog/objective
change appends one context update with the next dispatch; it never replaces
an earlier context message. Put context and dispatch in the same prompt batch,
with context first, so a roster edit alone does not buy an extra model turn.
Persist that update through the bus with a stable
id so a crash cannot silently lose it or regenerate a new update on every
launch. Coalesce intermediate roster changes to the latest revision, preserving
an explicit removal notice where a previously named recipient disappeared.
A model-backed joining member must see the current context, and an existing
member must learn of the join without a standing-prefix rewrite.

This is not permission to keep obsolete instructions. Changes to explicit team
instructions, voice, role, workspace authority, crew prompt format, or the
configured toolbox apply through the existing lifecycle that adopts them and
are recorded as deliberate prompt/definition changes. Toolbox refresh and
removal follow T07/T08/T14/T18, including a fresh-session handoff where a native
resume cannot tolerate removed tools. Operator commands are never deferred for
cache retention. If no trustworthy matching manifest exists for a legacy
session, record `legacy_missing` and generate one current snapshot on resume;
do not claim preserved bytes or copy another member's snapshot. A corrupted
snapshot is rejected and explicitly recovered, not silently treated as a hit.

**Regression contract D3:** across fresh start, confirmed resume, crash and
recovery, compare full standing artifacts byte for byte. Reorder the roster
and simulate clock/CLI-availability changes; assert a stable prefix and only
necessary appended context. Mutate instructions/role/toolbox and assert an
intentional digest transition. Test legacy/missing/corrupt manifests, restart
between context enqueue and acknowledgement, and compaction reorientation.
Native probes must also show what the runtime actually sends: changed files
or CLI arguments alone do not establish replacement of resumed instructions.

### D4. Reduce wake-ups while keeping work moving

Keep the existing status-message gather policy and 90-second/three-attempt
idle reminder defaults in the first implementation. The audit found no idle
reminders on Review cards; do not fix that nonexistent condition by muting all
reminders. Preserve questions, assignments, review requests, changes requested
and failure/limit notices as actionable. Do not infer that a free-text
`message` is informational by its wording.

Use typed state to avoid duplicate obligations: a status-only batch, an answer
with no open obligation, or an acknowledged parked/review state must not earn
an `agent.unanswered` turn solely to demand an idle acknowledgement. A real
unanswered question/message still receives the existing bounded retry. Where a
member tries to report waiting while its only card is in Review, allow a
non-mutating acknowledgement of that state; do not move the review card back
to Working/Waiting, occupy the worker slot, or steal review ownership. A member
with another executing card keeps its own task identity.

Measure how many turns this removes before changing timer cadence. Retain
existing per-message truncation and the full `agent_inbox` retrieval path.
Prefer `task_get(id)` in standing instructions. Do not silently truncate
`tasks_list` results as a cache fix; a separate paginated/read-view contract is
needed if the large-board output is later optimized.

**Regression contract D4:** deterministic-clock tests cover an abandoned
Working task, Waiting, Review, a second executing card, a status-only turn,
one genuine unanswered question and a changes-requested transition. Required
wakes still occur once under their normal retry policy. Repeated no-change
state acknowledgements add neither new model prompts nor mutated old prompt
bytes. Report request-count savings separately from cache-hit changes.

### D5. Lifecycle timing

Keep the current turn-boundary compaction mechanism and native context
thresholds. An explicit compaction still runs as requested. It replaces
conversation history with a summary; it need not invalidate unchanged tools
and system text. The audit's post-compaction requests retained roughly 28k
cached prefix tokens, so acceptance must not assume a full-prefix miss.
Include the compaction request's own input/output cost and the first resumed
work, not just the smaller context afterwards.

No automatic compaction merely because a member waits for review or a cache
TTL approaches. If compaction is already needed for context capacity, prefer
the existing warm, safe task boundary rather than a new wake of a cold idle
member. Keep pending keep-text and task identity intact. Do not delay needed
recovery, a user-selected model switch, or a fallback needed to do work for a
hypothetical cache benefit. Optional model-return/compaction scheduling remains
deferred until a controlled series establishes net benefit; do not alter
`policy.rs` on the evidence in this note alone.

**Regression contract D5:** one queued compaction at the safe boundary; no
new compaction on parking/TTL expiry; reorientation preserves the selected and
parked cards; model/endpoint/effort changes are visible comparison boundaries.
Standing-artifact and tool-definition hashes stay stable when those inputs
did not change. Summary/history hashes are expected to change.

### D6. Toolboxes and the final native tool catalog

Keep the approved toolbox proxy/pinning contract for every scoped local or
remote MCP server. Deferred loading does not replace it. Leave Claude tool
search at the runtime/operator's setting: forcing it on custom endpoints or
changing `alwaysLoad` semantics can make tools unavailable. The feature's
conditional behavior is documented by [Claude Code's MCP
reference](https://code.claude.com/docs/en/mcp#configure-tool-search). Keep pi's
current direct exposure until a separately measured supported-runtime deferred
route preserves tool discovery and task completion. Smaller per-role toolboxes
are the first way to reduce irrelevant definitions on all endpoints.

Use the existing planned per-server interfaces, `crew toolbox mcp <name>`,
`mcp::proxy::pin` and `definition_digest`; no cross-runtime aggregate server is
required. Definitions and instructions stay in their pinned per-member
snapshots. The fingerprint describes configuration; the definition digest
identifies what the proxy actually serves. Preserve source/account differences
rather than forcing two members' schemas to match. Secret rotation alone is
not a cache epoch; a legitimate new schema after a refresh is.

For pi, add a verified readiness barrier in crew's extension: register crew
first, then configured servers in name order, completing each actual native
registration before the first model request. Awaiting a function which returns
before discovery is not a barrier. Reverse and randomize server response
delays in native probes and compare the final `getAllTools` catalog. If pi
exposes no usable completion/barrier, this route fails T03/T16: a follow-up
must provide one pi-facing aggregate of the same pinned snapshots, with an
explicit stable tool-name/routing map and preserved per-server errors. It
must not quietly claim deterministic order from sorted configuration.

Crew's own role-filtered catalog must come from launch identity, not a live
`agents_list` lookup with worker fallback. This is an implementation change in
the toolbox MCP slice, not in the read-only audit. The daemon remains the
authority for tool-call permissions; a role hint must not grant privileges.
Missing legacy role metadata requires validated resolution or an explicit
unavailable result, never a guessed successful catalog.

**Regression contract D6:** toolbox T03/T07/T11/T14/T16/T18/T20/T21 remain
required. Add randomized native connection-order tests, daemon-offline
orchestrator catalog equality, direct/always-loaded and deferred exposure,
late/upstream-schema-change cases, and exact account-dependent digest
inequality. Compare the actual ordered native catalog and loaded schema
bytes, not merely `mcp.json`. Claude's captured selected skill/subagent lines
are path-stable; that limited result says nothing about equality of the whole
request or provider cache reuse across working directories. Inherited servers
stay explicitly outside crew's cache guarantees.

### Acceptance targets and measurement protocol

These are preregistered implementation targets. No numerical after-result is
claimed by this design. QA owns the runner and evidence; runtime owns native
counter parsing. The design card can finish before the product patches and
their after-series, but the cache improvement cannot be called demonstrated
until these gates pass.

**Comparable series.** Run at least three successful lifecycle samples per
side, `c8052373` versus the integrated candidate, with one final runner/template
revision used on both. Repeat an earlier baseline if its template changed.
Keep sandbox `off`, authorized login route, CLI versions, requested and actual
models, effort, toolbox selection, relevant ambient catalog, gap policy and
fixture output identical. Record run order, timestamps, binary/runner/template
hashes and provider/region information available without secrets. Failed runs
are retained with their reason and never substituted into the successful
sample set. Changed CLI/account/tool conditions require a new comparison set.

Toolbox defaults are an intentional exception to an otherwise matched
environment: old `c8052373` can inherit operator skills that the new scoped
default excludes. Keep that integrated before/after result, but report the
changed catalog and do not attribute all its gain to delivery or resume. For
C3, either use an explicit compatibility configuration with equal native
catalogs, or make a three-pair ablation on the same integrated toolbox build
with only the cache change absent/present. Record both binary/config revisions;
never describe that additional control as unmodified `c8052373`. Scoped-default
cost and matched-prefix cost are separate results.

Include unchanged-roster resume, added/removed-roster resume, a model-backed
join (the inexpensive shell observer alone is insufficient), ordinary
mid-turn delivery plus its next turn, large-context compaction, and an actual
last-observed-request gap over 300 seconds. A one-hour case requires a measured
gap strictly over 3,600 seconds. One long-gap pair is coverage only; a claimed
one-hour benefit needs three pairs. Measure the native TTL bucket where it
exists: a five-minute gap is not expiry of a one-hour Claude entry or Codex's
30-minute minimum; even a longer gap does not prove eviction. Never fabricate
an expiry outcome when the provider retains the entry.

**Metrics.** For every runtime, role, model and step, publish the first request
and all requests through that step's completion, plus the preceding matched
control. Let `I` be total input, `R` cache reads, `W` explicit writes, and
`U = I - R`. Preserve missing `W` as null and observed zero as zero. Report
`sum(R)/sum(I)`, `sum(U)`, `sum(W)` where known, output tokens, request count,
priced cost bounds, elapsed time and message queue/consumption latency. Include
collector source ids, timestamps, direct diagnostic markers and coverage.
Temporal joins alone are labeled associations.

Keep three evidence layers separate: (1) generated standing artifacts and
appended context, (2) native persisted context such as Codex's
`session_meta.base_instructions`, developer messages and `dynamic_tools`, and
(3) ordered catalog/request rendering from an isolated synthetic probe using
fixture MCP servers and a fake endpoint, as in the toolbox capability probes.
The paid lifecycle workload requires no provider-request interception or
credential copying. Compare pinned MCP snapshots separately. Record byte
counts, hashes and longest common prefixes where available. Raw content stays
in private fixture evidence; committed reports contain sanitized numbers and
hashes. Native persisted context is not the complete provider wire payload.
Claude's unavailable full request prefix stays unknown. Byte tests, scoped
native evidence and paired usage may demonstrate a narrower result; neither
launch-file equality nor an absent diagnostic proves whole-request equality.
Never infer token positions from byte counts. Use
`baseline.mjs --session-file` for only the fixture's new sessions,
as documented in [the collector contract](../scripts/cache/README-baseline.md).

| Gate | Claude Code target | Codex target | Supported pi target |
| --- | --- | --- | --- |
| C1: delivery and prefix integrity | D1 ordinary traffic makes zero historical-message edits and reaches the correct next turn; immediate operator path preserved. D3 standing artifacts identical on unchanged-policy resume; roster updates appended once. | Existing queued path and reply recipients preserved; full composed developer bytes stable, native application of resume changes recorded. | Follow-up delivery and preserved identity verified natively; no inference from GSD. |
| C2: warm control | Three-pair median token hit rate no more than 0.5 percentage points below that runtime's lifecycle-before median. | Same 0.5-point non-regression margin, relative to the new lifecycle series, not a demand to exceed the historical 97.8449%. | No numerical threshold claimed without a before-series; require three pairs on one named provider/model first. |
| C3: avoidable event cost | For an affected-model fold/resume scenario with positive excess in at least two of three before-runs, target at least 50% reduction in median excess `U`, improvement in at least two pairs, and no ordinary queued-delivery history-change diagnostic in the after-series. | If resume has measurable excess, same reduction target. If it does not, require C1/C2 and report stability, not invented savings. | After a provider baseline, same event/control comparison; until then only catalog/correctness results are accepted. |
| C4: retention/compaction controls | Report unavoidable new summary/expired-context writes separately. No added keep-alive or compaction request; one requested compaction still completes and the next work is correct. A TTL experiment must lower total priced step cost, not only improve hits. | Same no-extra-wake and completion gates; do not classify a five-minute gap as expired by assumption. | No TTL setting or benefit claim without supported provider evidence. |
| C5: toolbox stability | Exact pinned definitions and native catalog/order checks pass T03/T16 with tool search enabled and disabled/alwaysLoad. | Exact native catalog/loaded-schema evidence and deterministic generated overrides. | Randomized connection delays produce an identical final native catalog before the first request, or the cache guarantee remains incomplete. |

For C3, define the event's excess as `U_event - U_control` over equally bounded
first-post-step requests, then publish the full step totals as a check. Pair
successful runs by preregistered ordinal (before 1 with after 1, and so on),
and require the control and event's immediately preceding input sizes to be
within 10% for the normalized event comparison. Outside that range, retain
the full totals but mark that comparison unmatched; do not select a better
pair after seeing the outcome. Freeze this rule with the runner before the
candidate series. Use a materiality floor of the larger of 4,096 tokens or 10% of the
pre-step input to avoid calling small normal tail growth a history rewrite.
If the before excess is below that floor, report no measured opportunity for
that model. Haiku's absence of fold diagnostics cannot validate the production
Opus claim: add three same-model fold pairs from a model that reproduces the
mechanism before claiming that benefit. This is a separate priced QA subcase,
subject to the same per-run budget; it is not permission for unbounded runs.

Across the complete comparable workload, median upper API-equivalent cost must
not rise more than 5%, and every fixed task/review/output oracle must pass.
Report all paired values and their range; three pairs are an engineering gate,
not a statistical significance claim. More cached reads bought by extra
requests do not pass. D4 must reduce targeted duplicate wake requests to zero
in the deterministic obligation fixtures, while required wakes remain intact.
Ordinary D1 latency may include the remaining running turn plus the existing
five-second gather cap and measured transport overhead; publish that tradeoff.
Idle delivery and immediate operator/control handling must not gain a new
wait. Any stuck queue, lost message or incorrect reply recipient fails the gate
regardless of token savings.

### Implementation cards and ownership

Orchestrator creates the cards. These are file ownership suggestions, not
permission for concurrent edits in another slice's files. Toolbox work already
underway keeps its current owners.

| Proposed card | Owner | Files and boundary | Acceptance |
| --- | --- | --- | --- |
| `cache-delivery` | runtime | `daemon/delivery.rs`, delivery/turn accounting hooks, `assets/crew-bridge.register.ts`, related daemon/native-bridge tests. One owner covers durable queue, native submit/ack and turn recipients together. | D1; C1/C3 and message-latency/correctness gates. |
| `cache-standing-prompts` | native-architect | `agents/prompt.rs`, new prompt snapshot module, `AgentLocal` manifest field and `supervisor/launch.rs` assembly; narrow adapter changes for full standing-artifact reuse. Starts after graph's `toolbox-launch` changes to these files land, or uses an explicitly agreed merge checkpoint. | D3; byte regressions, changed-roster native resume and model-backed join, C1/C2/C3. |
| `cache-obligation-wakes` | content | `daemon/turns.rs` obligation checks, `orchestrate.rs` non-mutating review-state acknowledgement, `supervisor/health.rs` only as required; tests. Serialize any shared turn-accounting edits with `cache-delivery`. | D4; no masked unfinished work or occupied review slots; request-count/cost evidence. |
| Existing `toolbox-launch` / `toolbox-mcp-cli` plus a pi-order follow-up if required | graph / scheduler | Existing per-server proxy/pin APIs, role in launch identity, native registration barrier in `assets/crew-pi.ts`. Do not duplicate snapshot ownership in the prompt module. | D6; toolbox T03/T07/T11/T14/T16/T18/T20/T21 and C5. |
| `cache-acceptance` | qa, with runtime for collector changes | `scripts/cache/`, lifecycle template and sanitized reports under `notes/prompt-cache/`; no product adapters. Include effective TTL configuration evidence and the affected-model fold subcase. | Three matched lifecycle runs per side, native and byte evidence, C1–C5, documented no-op/unknown outcomes. |

D2's existing endpoint override needs documentation and measurement, not a new
configuration family. D5 changes no automatic policy in this phase. A future
keep-alive, pi TTL/deferred-exposure, paginated tool-output or optional
model-return policy needs its own measured proposal. First land correctness
and deterministic bytes; then judge cache and completion cost on the stated
workload, without changing the workload to manufacture the result.
