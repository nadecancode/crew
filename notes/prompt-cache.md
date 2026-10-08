# Prompt caching in crew

Operator objective: raise the prompt cache hit rate of crew's agentic
workloads. This note collects the work on it. `cache-baseline` keeps its
numbers in `notes/prompt-cache/`. `cache-design` builds on this audit.

## Audit

Task `cache-prompt-audit`. This is a read-only audit; nothing was changed.
Source is `main` at `c8052373`. `crates/crew` is identical at `e3ba08ba`.

Every line reference is to a file in that tree. The CLIs are the local
installs:

- Claude Code 2.1.291
- Codex CLI 0.159.0
- pi from the `gsd-pi` 2.70.1 bundle, which contains `pi-ai` and
  `pi-coding-agent`

### How each CLI caches

**Claude Code.** The cached prefix is ordered tools, then system, then
messages. A change at any point invalidates everything after it.

- **TTL.** Settings key `promptCacheTtl`, or `CLAUDE_CODE_PROMPT_CACHE_TTL`,
  which takes precedence. Values are `"5m"` or `"1h"`.
  - When unset, the binary's settings description says: 1 hour on a Claude
    subscription within its usage limits, and 5 minutes on an API key,
    Bedrock, Vertex or Foundry.
  - Subagents use `subagentPromptCacheTtl` or
    `CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL`. Their default is 5 minutes unless
    `ENABLE_PROMPT_CACHING_1H=1` is set.
  - Caching can be switched off with `DISABLE_PROMPT_CACHING` and its
    per-model variants.
  - crew sets none of these. Every member write in the sample below is 1 h.
- **Prefix sharing across sessions.** The first request of every member
  session reads about 23.4k tokens from cache. That is Claude Code's own
  system prompt and tools, shared across sessions.
  - A miss that keeps the session's own system prompt reads about 28.8k
    instead.
  - So crew's per-session system part is about 5.4k tokens: the briefing, the
    member tone and the environment.
- **MCP tools are deferred** behind ToolSearch. Their names arrive as
  `deferred_tools_record` and `deferred_tools_delta` attachments, and a schema
  enters the conversation as a tool result once loaded. MCP server
  instructions arrive as an `mcp_instructions_delta` attachment. So a change
  to crew's tool set or instructions appends to messages; it does not break
  the tools or system prefix.
- **Thinking blocks are bound to the prefix.** When the client finds its
  message history changed, it records a `thinking_drop` attachment with
  `clientChange.kinds: messagesHistoryChanged` and `firstChangedMessageIndex`.
  It then drops every later thinking block (`reason: prefix_mismatch`), and
  the request writes everything from that index on to cache again.
- **The statusline JSON has a `prompt_cache` object.** It gives `warm`,
  `ttl`, `expires_at`, `hit_ratio`, `misses`, `miss_recache_tokens` and
  `last_miss_cause`.
  - Causes come from a closed set: `system_prompt_changed`, `tools_changed`,
    `model_changed`, `messages_rewritten`, `ttl_expired_5m`,
    `ttl_expired_1h`, `likely_server_side`, `unknown`.
  - It is a client-side classifier available per session. crew writes each
    member's `settings.json` (`agents/claude.rs:178`), so a `statusLine`
    command could log it.

**Codex.** It uses the Responses API, which caches automatically from 1024
tokens.

- crew's standing instructions go into `developer_instructions`
  (`agents/codex.rs:103-125`). Codex places them in a developer message at the
  head of the conversation, so they are part of the prefix.
- Codex keys requests by thread with `prompt_cache_key`. This comes from the
  upstream source, not the local binary: only the field names
  `prompt_cache_retention` and `prompt_cache_options` appear in it, and only in
  bundled docs.
- No retention setting was found in 0.159's configuration. The bundled
  OpenAI docs mention `prompt_cache_options.ttl` (`"30m"`) for the GPT-6
  family.
- Changing the request-level reasoning effort mid-thread changes the prefix,
  per the same docs.
- Not verified: whether `codex resume` with different `developer_instructions`
  replaces the recorded ones or adds new ones. To be measured.

**pi.** It uses `pi-ai`.

- **Anthropic.** The provider puts `cache_control` on the system block and the
  last user message (`providers/anthropic-shared.js:295-366`).
  - The TTL is 5 minutes unless `PI_CACHE_RETENTION=long`, which gives 1 h,
    and only for `api.anthropic.com` (`:36-53`).
- **OpenAI Responses.** `prompt_cache_key` is the session id.
  - `PI_CACHE_RETENTION=long` asks for 24 h retention on `api.openai.com`
    (`providers/openai-responses.js:25-33,82-88`).
- **The system prompt ends with the current date and time, to the second, and
  the working directory** (`pi-coding-agent/dist/core/system-prompt.js:55-56,
  179-180`).
  - It is rebuilt at every session start and whenever the active tool set
    changes (`agent-session.js:545,1498`).
  - So every pi launch, a resume included, misses its whole prefix whatever
    crew sends.
  - crew registers its tools once, at extension load
    (`assets/crew-pi.ts:186-217`). gsd-pi 2.70.1 has no `registerMcpServer`,
    so the `crew ctl tools` path is the one taken, and it does not rebuild the
    prompt mid-session.

### Measured sample

This is an interim sample, to be replaced by `cache-baseline`. It reads the
Claude Code session files of this workspace's crew members and orchestrators
(`~/.claude/projects/-home-nade-workspaces-personal-crew*`), 2026-10-02 to
2026-10-07, read-only.

It takes every main-thread assistant response once (by message id, not
sidechains) with its `usage`. Codex rollouts and pi sessions are not in it;
`runtime` measures them in `cache-baseline`.

| Measure | Value |
| --- | --- |
| Sessions | 45 |
| Requests | 15,474 |
| Uncached input | 33,914 tokens |
| Cache read | 5,377,091,737 tokens |
| Cache write (all 1 h TTL) | 114,612,219 tokens |
| Hit rate by tokens (read / all input) | 97.9% |
| Writes as share of input-side cost (write 2x, read 0.1x base input) | 29.9% |

The hit rate by tokens looks high, but writes are 2.1% of input tokens and
about 30% of input-side cost. The metric worth moving is write volume.

Classification of each request that wrote at least 10k tokens and more than it
read:

| Class | Events | Tokens written | Share of all writes |
| --- | --- | --- | --- |
| History rewrite, idle gap under 5 min (reads about 28.8k: system intact, messages rewritten) | 151 | 54,025,202 | 47.1% |
| History rewrite, idle gap 5 to 60 min (same 28.8k signature, so not a TTL expiry under 1 h) | 21 | 7,147,416 | 6.2% |
| Idle gap over 1 h (TTL expired; reads only the shared 23.4k) | 57 | 24,393,821 | 21.3% |
| Model change | 2 | 846,025 | 0.7% |
| Session start | 26 | 651,089 | 0.6% |
| After a compaction boundary | 4 | 135,831 | 0.1% |
| Ordinary growth (the rest) | | 27,412,835 | 23.9% |

The history rewrites follow folded deliveries (rank 1 below):

- 16 sessions record `thinking_drop`.
- Of their 297 turns that contained a folded delivery, 157 (53%) were followed
  by a history change at the next turn's first request.
- Of their 939 turns without one, 1 (0.1%) was.
- Turns with Claude Code's silent-turn reminder but no fold were followed by
  none.

Lifecycle frequencies from crew's event log (`.crew/crew.db`, read-only,
2026-10-02 14:08 to 2026-10-07 22:53):

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
| Usage-limit parks / lifts | 150 / 149 |
| Daemon recoveries (all adopted, none restarted) | 18 |

Of the usage-limit parks, 51 are `runtime` on "out of credits", which is
resumed every 30 min.

### Ranked cache-breaking points

The ranking goes by measured tokens. Where no number is separable yet, it goes
by what one event costs.

1. **A delivery folded into a running turn is rewritten at the next turn.**
   - **Code:** `assets/crew-bridge.register.ts:76-98`. `deliver` calls
     `$.session.append` while a turn runs; Claude Code records those rows with
     `isMeta: true`. The `WAKE` prompt at `:154-166` starts a turn when the
     appended rows went unread. The daemon pushes over the socket while a turn
     is active (`daemon/delivery.rs`, `Route::Socket`).
   - **Position:** messages, from the first folded row on.
   - **What varies:** every delivery that lands mid-turn. 24% of turns in the
     sample had one.
   - **Invalidates:** at the first request of the next turn, Claude Code
     reports `messagesHistoryChanged` and drops all later thinking blocks.
     Everything after the fold is written again.
   - **Measured:** 172 events and 61.2M tokens written, 53.3% of all writes
     (about 16% of input-side cost). That is 356k tokens per event on
     average.
   - **Avoidable:** yes. Candidates for `cache-design`:
     - submit every delivery as a queued prompt for the turn boundary (the
       mod's `prompt.submit` path at `:97`, which never showed the effect),
       accepting the added latency;
     - or find an in-turn form Claude Code keeps byte-stable.
     - Either needs a controlled A/B run in `cache-bench`.
   - **Other runtimes:** Codex queues through its app-server
     (`agents/codex.rs:293`) and pi uses `sendUserMessage(..., { deliverAs:
     "followUp" })` (`assets/crew-pi.ts:145`). Neither is measured yet.

2. **Idle beyond the TTL.**
   - **Code:** crew sets no TTL anywhere (`agents/claude.rs:178-188`,
     `agents/pi.rs:398-420`). Anything that starts a turn after the gap
     re-writes the whole session:
     - a delivery (`daemon/delivery.rs`);
     - a reminder (`daemon/supervisor/health.rs:104-128,176`);
     - an orchestrator notice (`notify_orchestrator`, `daemon/handlers.rs:810-819`,
       `daemon/context.rs:122-180`);
     - `RESUME_PROMPT` after a usage limit (`daemon/limits.rs:311`).
   - **Position:** all of it. Only Claude Code's shared 23.4k survives.
   - **What varies:** per member, with how long it waits (review, dependencies,
     overnight).
   - **Measured:** 57 events and 24.4M tokens, 21.3% of writes, 428k per event.
   - **Avoidable:** partly.
     - Claude members on a subscription already get 1 h. A member on an API
       key, Bedrock or Vertex gets 5 min unless `promptCacheTtl` is set; the
       endpoint config can already pass `CLAUDE_CODE_PROMPT_CACHE_TTL` per
       agent through `apply_endpoint` (`agents/claude.rs:128-146`).
     - pi members get 5 min unless `PI_CACHE_RETENTION=long`.
     - Compacting before a long park shrinks what is written on waking.
     - A keep-alive turn costs one read (0.1x the context) and adds a little to
       the history. At a 1 h TTL, about 19 of them cost as much as one re-write
       (2x), so they pay only for a member expected to wake within hours.
     - Batching wake-ups for the same member helps too.

3. **The briefing is rebuilt at every launch, a resume included.**
   - **Code:**
     - `daemon/supervisor/launch.rs:175-204` builds `TeamView` from the live
       `model.agents` and, for the orchestrator, the model catalog with
       `Availability::detect()` and the work style's fallbacks.
     - `agents/prompt.rs:45-68` (roster), `:222` (`worker_briefing`), `:364`
       (`orchestrator_briefing`).
     - Delivered by `--append-system-prompt-file` (`agents/claude.rs:212`),
       `developer_instructions` (`agents/codex.rs:103-125`) and
       `--append-system-prompt` (`agents/pi.rs:363-366`).
   - **Position:** system for Claude Code and pi; the head developer message for
     Codex.
   - **What varies:**
     - per member, in the first line (id, role) and the workspace branch;
     - per team change: the roster (`agent_add`, `agent_remove`,
       `agent_spawn`, temporary members leaving, role edits), the objective,
       the style instructions and the orchestrator's id;
     - for the orchestrator only, also CLI availability.
     - It never changes inside a running session. There are no timestamps or
       ids in it.
   - **Invalidates:** on a resume, the system prompt and every message (the
     23.4k-only read signature), even inside the TTL.
   - **Frequency:** 65 roster changes in 5 days.
   - **Evidence:** `scheduler`'s `briefing.md` is from 2026-10-05 22:14. The
     roster now differs from it by two lines (`linux` left, `cache-reviewer`
     joined), so its next resume would write its whole context again.
   - **Measured:** not separable in the sample, because resumes there came
     after gaps over 1 h. One event costs the whole context: 400k to 650k
     tokens for the long-lived members above.
   - **Avoidable:** yes. Keep the briefing byte-identical across a session's
     launches:
     - reuse the stored `briefing.md` on resume;
     - or take the volatile parts (roster, objective) out of it into
       `agents_list` or a delivered message.

4. **Relaunches.** These are the paths that resume a session:
   - a crash restart, up to `max_restarts` (default 1; `daemon/supervisor/
     exits.rs`);
   - daemon recovery stopping and resuming a process of another build
     (`daemon/supervisor/recovery.rs:158-199`). This workspace's 18
     recoveries all adopted the running process instead;
   - a usage limit with `--restart` or a dead process
     (`daemon/limits.rs:302`). A live session is prompted in place instead
     (`:311`), which keeps its cache;
   - a refused 1M window, which restarts on the standard window
     (`daemon/context.rs:57-95`).

   With a stable briefing and inside the TTL, a Claude Code resume should read
   its prefix. Still unmeasured: whether Claude Code adds anything that is not
   appended at the end on resume (environment, date, skills listing). Ask
   `cache-bench` to restart a member inside the TTL with an unchanged briefing.

5. **Model and CLI changes.** The cache is per model, so each one writes the
   whole context.
   - **Code:**
     - `daemon/fallback.rs:291` (`agent_set_model`) and `:434`
       (`move_to_runtime`, a fresh session plus handoff,
       `agents/prompt.rs:551`);
     - `daemon/policy.rs`: the work-style fallback, and the move back after
       the reset or `RETRY_HOME_MS` (1 h);
     - `daemon/turns.rs`: the refusal fallback;
     - `agents/claude.rs:186` (`switchModelsOnFlag`) and `:258-262`
       (`CLAUDE_CODE_NO_MODEL_FALLBACK` is only set when `model_fallback` is
       off, so Claude Code may switch on its own).
   - **Measured:** 2 events and 0.85M tokens in the Claude sessions. The
     20 moves to Codex this week each started a fresh session, which is
     inherent.
   - **Timing:** a move back home, or any optional move, can wait until the
     member has been idle past its TTL or is at a compaction point. Then it
     costs no more than waking would.

6. **Compaction.**
   - **Code:**
     - the orchestrator is warned at `context_warn_percent`, default 75
       (`daemon/context.rs:122-180`);
     - queued at `:184-302` and run by the mod at turn end, or at once when
       idle (`assets/crew-bridge.register.ts:62-81,154-160`);
     - reorientation at `daemon/context.rs:380-428`, keeping at most 1000
       characters.
   - **Position:** messages. The summary replaces the history; system and
     tools stay.
   - **Measured:** the rebuild is cheap, 4 events and 0.14M tokens (30k to
     47k each).
   - **Hidden cost:** the compaction request itself reads the whole context.
     That is 0.1x when warm, but a member compacted after idling past its TTL
     writes its whole context first.
   - **Timing:** compact at turn end, as the mod already does. Never compact a
     cold idle member unless it is about to wake. Since every later request
     reads the whole context, an earlier compaction can save more than the
     miss costs.

7. **Session start.**
   - **Measured:** 26 events and 0.65M tokens.
   - Claude Code's own 23.4k comes from the shared cache. crew's part (about
     5.4k) is written per start.
   - The per-member first line (`agents/prompt.rs:222-232`) prevents sharing
     the rest of the 9.5 KB briefing across members. At most about 5k tokens
     per launch could be saved, so this is low priority.

8. **Turn count times context size.** This is cost, not hit rate. Every
   injection that starts a turn reads the whole context once at 0.1x:
   - deliveries;
   - reminders, after 90 s idle with a `Working` card, at most 3
     (`daemon/supervisor/health.rs:104-128`);
   - `WAKE`;
   - orchestrator notices;
   - the compaction reorientation (`daemon/context.rs:364-368`);
   - `RESUME_PROMPT` every 30 min while a limit persists
     (`daemon/limits.rs:311`);
   - the first prompt (`agents/prompt.rs:448,465`;
     `daemon/supervisor/launch.rs:350-384`).

   Mitigations already exist: a 1.5 to 5 s gather (`daemon/delivery.rs:46-50`)
   and status news that waits for company (`:52-63`). Tool output also stays
   in the context until a compaction: `tasks_list` on this board returns
   90 KB, about 22k tokens, read again by every later request.

9. **Claude Code's silent-turn reminders.** There are over 1,000 in the
   sample ("The user hasn't heard from you in a while ..."), controlled by
   `CLAUDE_CODE_SILENT_TURN_REMINDER*`. They work against crew's member tone,
   "Work silently between tool calls" (`agents/prompt.rs:152-169`). They only
   append and caused no history change on their own. The cost is a little text
   per long turn; low priority.

10. **Tool definitions.**
    - crew's tools are static per build and role: `&'static str`
      descriptions, sorted schema keys (`mcp/tools.rs:44`), `listChanged:
      false` and a fixed `instructions` string (`mcp/mod.rs:131-134`).
    - Claude Code defers them, so a change only appends a delta. Codex and pi
      send them with each request, so there they are prefix.
    - Edge: if the daemon cannot be reached at `tools/list`, the
      orchestrator is given the worker list (`mcp/mod.rs:45-64`). On Codex or
      pi its next launch would then see a different tool list.
    - `inherit_mcp` adds the operator's servers to Claude
      (`agents/claude.rs:207`); crew does not control their stability.
    - A crew upgrade that edits a description breaks only the next launch.
    - No measured cost.

### What crew sends, by runtime

| Item | Runtimes | Position | Varies with | Breaks on change |
| --- | --- | --- | --- | --- |
| crew tool list and schemas (`mcp/tools.rs:44`, filtered at `mcp/mod.rs:98`; pi via `crew ctl tools`, `cli/agent_side.rs:77-88`) | all | Claude: messages (deferred); Codex, pi: tools | role, crew build | Codex, pi: everything; Claude: nothing (appends a delta) |
| MCP `instructions` (`mcp/mod.rs:133`) | Claude, Codex | Claude: attachment; Codex: context | crew build | as above |
| Briefing (`agents/prompt.rs:222`, `:364`) | all | system (Claude, pi); developer message (Codex) | member, roster, objective, style, branch; orchestrator: catalog | system and messages, at the next launch |
| Member tone (`agents/prompt.rs:152`) | Claude (replaces the `tone` section, `assets/crew-bridge.register.ts:186-199`), Codex (in `developer_instructions`) | system | voice only | no (constant) |
| `model_verbosity = "low"` for members (`agents/codex.rs:117`) | Codex | request parameter | constant | no |
| Operator's own `developer_instructions` (`agents/codex.rs:36-43`) | Codex | developer message | the operator's config | messages, at the next launch |
| `settings.json` (`agents/claude.rs:178-188`) | Claude | none directly; no TTL set | endpoint | no |
| Hooks (`cli/agent_side.rs:107-137`) | Claude, Codex `notify`, pi extension | nothing: they never print | | no |
| First prompt: standby, kickoff with live usage, handoff, `RESUME_NOTICE`, pending messages (`daemon/supervisor/launch.rs:350-384`) | all | messages (appended) | per launch | no |
| Delivered messages, `[crew] Message from ...` (`agents/prompt.rs:724`, 4000 chars each) | all | messages (appended); Claude fold mid-turn | per message | Claude fold: rank 1 |
| `WAKE` (`assets/crew-bridge.register.ts:18`) | Claude | messages | | no (one more turn) |
| Reminder (`daemon/supervisor/health.rs:176`) | all | messages | | no (one more turn) |
| `RESUME_PROMPT`, `RESTART_NOTICE` (`daemon/limits.rs:24-30`); recovery notice (`daemon/supervisor/recovery.rs:192`) | all | messages | | no; the restart path is rank 3 and 4 |
| Compaction keep text and reorientation (`daemon/context.rs:380-428`) | Claude, Codex | messages after the summary | per compaction | (the compaction itself is rank 6) |
| `agent_set_model` note, handoff (`daemon/fallback.rs`, `agents/prompt.rs:551`) | all | first message of the new session | per move | the move: rank 5 |
| Task dispatch (`agents/prompt.rs:701`) | all | messages | per card | no |
| Tool results (`mcp/tools.rs` renderers) | all | messages | per call | no; they grow the context |
| `--model` with `[1m]` (`agents/claude.rs:241-249`), Codex `model_context_window` / `model_auto_compact_token_limit` (`agents/codex.rs:126-131`) | Claude, Codex | model | 1M refusal | everything (rank 5) |

### Session lifecycle

A launch is one of three kinds:

- **New:** a chosen session id, `--session-id`.
- **Resume:** `--resume <id>`, `codex resume <thread>`, or pi `--session-id`.
  A launch counts as a resume only once the session id was confirmed
  (`daemon/supervisor/launch.rs:205`).
- **Fresh:** a new id and a handoff prompt, used for a move to another CLI and
  for an operator "fresh" restart.

| Trigger | Kind | Code | Avoidable, or when to do it |
| --- | --- | --- | --- |
| Team start or member added | New | `daemon/supervisor/creation.rs` | Inherent. About 5.4k tokens of crew system per start. |
| Crash | Resume (up to `max_restarts`, default 1) | `daemon/supervisor/exits.rs` | Inherent. Costs no write if the briefing is stable and the TTL holds (unmeasured). |
| Daemon restart, same build | none (adopted) | `daemon/supervisor/recovery.rs:48-110` | Already avoided. |
| Daemon restart, other build | Resume | `daemon/supervisor/recovery.rs:158-199` | Restart the daemon while members are idle and warm; keep the briefing stable. |
| Usage limit lifted, process alive | prompt in place | `daemon/limits.rs:311` | Keeps the cache if lifted within the TTL. A 30 min retry against an unchanged limit (`runtime`, 51 times) is a turn each time. |
| Usage limit, `--restart` or process gone | Resume | `daemon/limits.rs:302` | As for a crash. |
| 1M window refused | Resume on another model variant | `daemon/context.rs:57-95` | Inherent, once per member. |
| Work-style fallback and the move back home | Resume on another model | `daemon/policy.rs` | Move back only once the member is idle past its TTL. |
| `agent_set_model` | Resume on another model | `daemon/fallback.rs:291` | The orchestrator's choice. Best at a compaction point. |
| Move to another CLI | Fresh plus handoff | `daemon/fallback.rs:434` | Inherent. The handoff carries no history. |
| Compaction | in place | `daemon/context.rs:184-302`, `assets/crew-bridge.register.ts:62-81` | At turn end, while warm. |

### Open for the next cards

- `cache-baseline` (runtime):
  - Codex `cached_input_tokens` per event class, including a resume with
    changed `developer_instructions`.
  - Confirm the fold signature with its own tool. The `thinking_drop` record
    and the `isMeta` rows are a clean join key.
- `cache-bench` (qa): controlled A/B runs on the same workload:
  - fold on and off;
  - a restart inside the TTL with an unchanged briefing, and with a roster
    line changed;
  - compaction while warm and while cold.
- `cache-design`:
  - a delivery that never rewrites history (rank 1);
  - a briefing stable across launches (rank 3);
  - TTL per member and keep-alive policy (rank 2);
  - timing of optional model moves (rank 5).
- `toolbox-spec`: per-member MCP servers and skills add tool definitions.
  - On Codex and pi those are prefix: they must be byte-stable and
    deterministically ordered, and set only at launch.
  - On Claude Code they are deferred, so a change appends.

### Method

The figures can be reproduced read-only. In each session file, take the
`assistant` lines that are not sidechains and have `.message.usage`, once per
`.message.id`. From each, read `cache_creation_input_tokens`,
`cache_read_input_tokens`, `input_tokens` and
`cache_creation.ephemeral_{1h,5m}_input_tokens`.

A big write is a request that writes at least 10k tokens and more than it
reads. Classify each by these tests, in order:

1. a `system/compact_boundary` before it;
2. a model different from the previous request;
3. the gap since the previous request: over 3600 s, or 300 to 3600 s;
4. otherwise, a history rewrite.

For the fold correlation:

- A turn ends at a `system/turn_duration` line.
- A fold is a `user` line whose array content starts with `[crew]`; Claude
  Code also marks these `isMeta`.
- A history change is an `attachment` of type `thinking_drop`.
