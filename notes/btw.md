# Asides (`/btw`): ask the crew a side question without touching their sessions

Card `btw-mechanism`, 2026-10-08. Design and probes only; no product source
changed. Probed with Claude Code 2.1.291, Codex 0.159.0 and the supported pi
1.1.0 (`@earendil-works/pi-coding-agent`, installed under `target/` by
`notes/toolbox-probes/install-pi.mjs`). The probes are in
[btw-probes/](btw-probes/README.md).

Inputs:

- The operator's ask (w-2d3dcc6b066fa029): "some sort of /btw mechanism: talk
  to entire crew without impacting context window / workflow state".
- native-architect (relayed in m-041a71ea77e6): the `agent_send` path
  appends to a member's context and changes turn and workflow accounting, so
  `/btw` must not reuse it. The answer comes from a separate conversation over
  a read-only snapshot, with usage of its own, and it always costs tokens.
- reviewer (same relay): it must fit the D1 delivery contract
  (`notes/prompt-cache.md` §Design D1) and never be a wake-up source. It is
  out of band: an answer in the dashboard, never injected mid-turn, and
  clearly distinct from operator steering.
- designer, `notes/dashboard-features.md` §Asides and §Requirements on the
  design cards: answers from copies; availability per member with reasons;
  asynchronous answers with their own status and usage; Cancel; body-less
  `aside.asked` and `aside.answered` events; retention of the last 50 asides,
  64 KiB per answer.
- native-architect `epics-model` (m-5fdf8fdc82df): an aside creates no epic
  or task. Only an explicit promotion to real work can do that.

## What an aside is

The operator asks one side question of some or all members. Each chosen
member's answer is produced by **its runtime's own CLI, forked from a copy of
the member's current session**. The answering model therefore sees the
member's whole context: its standing instructions, tools, conversation and
the work in progress.

Nothing reaches the live member:

- nothing is appended to its conversation, and nothing is delivered, queued
  or acknowledged;
- no turn is started, woken or held, and its turn state does not change;
- no task, message, bus, board or roster state changes, and no hook reports
  for the member;
- its session files and the CLI's records of them are unchanged.

The answer appears in the dashboard (web Asides tab, TUI panel, `crew btw`).
It never goes to the member. **Send to claude-1 as a message…** in the
dashboard is the only way an aside's content reaches a live session, and it
goes through the ordinary composer.

An aside is not steering. Steering is an operator message that the member acts
on (D1's immediate path). An aside is a read-only question about the member,
answered by a copy of it. Nothing about the live member's work changes.

## Findings

All probes are offline. Each CLI talks to a localhost stub
([stub.mjs](btw-probes/stub.mjs)) that records request bodies and answers in
the provider's own streaming format, so every CLI completes real turns and
writes real session files without a provider call. Homes, repositories and
credentials are throwaway fixtures.

### Every runtime can fork a session without touching it

| Runtime | Fork command | Session files after the aside | Probe |
| --- | --- | --- | --- |
| Claude Code | `claude --print --resume <id> --fork-session --no-session-persistence` | Member's transcript byte-identical. No file added or changed in the config directory. | [claude.mjs](btw-probes/claude.mjs) |
| Codex | `codex exec … fork --ephemeral <thread>` | Member's rollout byte-identical. The parent thread's row and the thread list are unchanged. Codex's own log and state databases are written, as by any Codex run. | [codex.mjs](btw-probes/codex.mjs) |
| pi | `pi --print --fork <member session file> --session-dir <aside dir>` | Member's session directory and pi's agent directory unchanged. `--fork` cannot be combined with `--no-session`, so the fork is written to the aside's own directory, which crew deletes afterwards. | [pi.mjs](btw-probes/pi.mjs) |

Without persistence, a Claude Code fork wrote nothing at all. With
persistence (`--fork-session` alone), it added one new transcript beside the
member's. Codex without `--ephemeral` likewise adds a rollout and a thread
row. Both are therefore run without persistence.

A fork of a **copy** also works, so the aside can run in a home of its own:

- Claude Code: the transcript copied into another config directory under the
  same project path gives a byte-identical request to the in-place fork.
- Codex: a rollout copied into another `CODEX_HOME` repeats the whole live
  history. That home's first run adds a skills developer item after the
  history, so only the tail differs. The member's home is unchanged.
- pi: `--fork` takes a path, so it reads the member's file in place
  read-only, or a copy.

### Mid-turn: the live turn finishes normally; the aside sees its prompt

A fork taken while the member's turn was in flight (its model request held by
the stub) did not disturb it. The live turn then completed, and its session
gained its own reply and nothing from the aside (Claude Code and Codex).

The fork includes the in-flight user message, then a marker the CLI adds to
the fork only:

- Claude Code synthesizes the assistant reply "No response requested.".
- Codex adds a `<turn_aborted>` user item ("The user interrupted the previous
  turn on purpose…").

The aside's preamble (§Mechanism) tells the model that it is answering a side
question and must not continue the work.

### The aside's request repeats the member's prefix, if the aside keeps the member's tools

What matters for cost is whether the aside's first model request repeats the
member's cached prefix: tools, system prompt, then messages. The probes
compare the aside's request with the member's last one, and for Claude Code
also with the member's next one.

- **Claude Code.** With the member's own flags, the fork sends the member's
  tools and system prompt byte-identically. It also repeats every message
  that the member's own next request repeats: compared with the member's next
  turn (`live-3`), the first 6 of 8 messages are identical. The two that
  differ are the new user prompt and the per-request token note, which Claude
  Code rewrites on every request.
  - Permission mode does not enter the request: `dontAsk` and
    `--dangerously-skip-permissions` give identical bodies.
  - `--allowedTools`, the hooks in flag settings, `disableAllHooks`, and an
    MCP server with the same name and tool definitions but a different
    command give identical bodies.
  - Plan mode adds a request parameter (`safeguards`) and a message.
  - `--disallowedTools` and `--tools` remove tool definitions. With tool
    search off (a gateway endpoint), dropping the crew MCP server removes its
    tools. Any of these changes the tools prefix, and nothing after it can be
    read from cache.
- **Codex.** The fork sends the same `instructions`, tools and the whole live
  input. Its `prompt_cache_key` is the **parent thread's id**, so it is
  routed to the member's cache.
  - `--sandbox read-only` adds a permissions developer item and an
    environment item after the history, which is harmless.
  - It also turns `external_web_access` off in the `web_search` tool
    definition, which changes the tools prefix. `-c web_search="live"`
    restores the member's definition. The aside must reproduce whatever
    search setting the member runs with.
- **pi.** The fork sends the member's tools, system prompt and messages
  unchanged. pi lists the active tools inside its system prompt, so
  `--tools` or `--no-tools` change the system prompt. Restriction has to
  happen at call time instead (next section).

No provider call was made, so provider caching itself was not observed. The
request bodies show that an aside within the cache lifetime sends the prefix
the member has already written. Anthropic caches by prefix content, and
Codex routes the fork with the parent's cache key. Acceptance case B10
measures it with a cheap model once credentials are available to the
acceptance run. The probes' throwaway homes have none, and crew never copies
an operator's login.

### Refusing every acting tool without changing the tool list

The aside keeps the member's tool definitions and refuses the calls instead.
The stub asked for a write, a shell command, a crew MCP call and a read:

| Runtime | Setting | Write | Shell | crew MCP call | Read |
| --- | --- | --- | --- | --- | --- |
| Claude Code | `--permission-mode dontAsk`, no `--allowedTools` | refused | refused | refused | allowed |
| Claude Code | default mode with `--permission-prompts none` | **ran** | **ran** | refused | allowed |
| Codex | `--sandbox read-only`, `approval_policy="never"` | refused (read-only file system) | read-only commands only | refused ("requires approval, but approval policy is never") | allowed |
| pi | an aside extension blocking `tool_call` for all but `read`, `grep`, `find`, `ls` | refused | refused | refused (same handler) | allowed |

([claude-deny.mjs](btw-probes/claude-deny.mjs),
[codex-deny.mjs](btw-probes/codex-deny.mjs), `fork-guard` in
[pi.mjs](btw-probes/pi.mjs).)

The default permission mode in print mode let Write and Bash run. Asides
therefore always use `dontAsk` on Claude Code, never the default mode and
never bypass. The crew MCP server is also replaced (next section), so a call
that got through still could not reach the daemon.

### Hooks and reporting must stay off

Run with the member's flags, the fork:

- fired the member's `SessionStart`, `UserPromptSubmit` and `Stop` hooks;
- started the member's own MCP server.

For a crew member, those report to the daemon under the member's identity:
turn state, usage, model steps. The aside must therefore not carry the
member's identity, hooks, bridge mod, pi extension or crew MCP server.

- Claude Code: `disableAllHooks: true` in the aside's flag settings stopped
  project hooks without changing the request
  ([claude-hooks.mjs](btw-probes/claude-hooks.mjs)).
- Claude Code and Codex: an MCP server named `crew` that serves the same tool
  definitions keeps the request identical. It refuses every call.
- pi: the aside loads its own extension in place of `crew-pi.ts`. It
  registers the same crew tool definitions, refuses every call, and blocks
  acting built-in tools.

## Mechanism

### Per answer

1. **Availability** (§Availability). An unavailable member gets status Not
   available, with the reason.
2. **Snapshot.** The daemon opens the member's session file read-only through
   `AgentFiles`: the operator's directories unsandboxed, beneath `<A>/rw` in
   a sandbox, with no link followed. It copies the file up to its last
   complete line. A line still being written is left out, so the copy is
   always valid JSONL. The copy goes into a private per-answer directory
   under the daemon's state, never under `<A>/rw`. The snapshot's last entry
   time is the "context at" time shown with the answer.
3. **Launch** the runtime's aside spec (§Per runtime) in that directory, in
   the member's working directory, with:
   - the member's model, including the long-context tag, effort, endpoint
     environment and credentials;
   - the bytes of the member's own standing instructions;
   - its tone;
   - its tool definitions: crew's catalog, toolbox servers from their pinned
     snapshots, plugins and skills;
   - its setting sources.

   It runs without the member's identity, hooks or crew MCP server.
   - **Credentials.** The daemon puts the endpoint secret into the
     process's environment, as Codex launches already do with
     `secret_env`.
   - **Login files.** A member that authenticates with the operator's login
     files (unsandboxed) runs its aside against that same config directory or
     `CODEX_HOME`, without persistence; the probes show that writes no session
     record. Login files are never copied.
   - **Sandbox.** A sandboxed member's aside runs under the member's sandbox
     profile, with the workspace bound read-only and the per-answer
     directory as its only writable tree.
4. **Prompt.** The operator's question is preceded by a fixed preamble:
   "[crew aside] The operator asks a side question. Answer it from what you
   know now. Your tools cannot change anything; do not continue your work."
   Both are new tail input; the prefix is untouched.
5. **Result.** The daemon reads the answer and the runtime-native usage:
   - Claude Code: `--output-format json` gives `result`, `usage` and
     `total_cost_usd`.
   - Codex: `--json` gives the last `agent_message` and `turn.completed`
     usage: input, `cached_input_tokens`, `cache_write_input_tokens`,
     output and reasoning.
   - pi: `--mode json` gives the final assistant message and `usage`:
     input, output, `cacheRead`, `cacheWrite` and cost.

   The answer is stored truncated to 64 KiB, with a truncation flag. Then the
   per-answer directory is deleted.
6. **Bounds.** A per-answer wall-clock limit (5 minutes), Claude Code's
   `--max-budget-usd`, at most one answer in flight per member and four per
   daemon (the rest wait). Cancel and the time limit kill the process group,
   set the status, keep whatever usage was reported (else "not reported"),
   and delete the directory.

### Per runtime

**Claude Code**:

```text
claude --print --resume <session> --fork-session --no-session-persistence
  --output-format json --max-budget-usd <limit>
  --model <member model> --append-system-prompt-file <member briefing copy>
  --mcp-config <aside mcp.json> [--strict-mcp-config as the member]
  --settings <member settings without hooks or apiKeyHelper, plus disableAllHooks: true>
  [--setting-sources as the member] [--plugin-dir <aside tone mod>] [toolbox plugin dirs]
  --permission-mode dontAsk -- <preamble and question>
```

- Run in `CLAUDE_CONFIG_DIR` = the member's (unsandboxed, operator login) or
  the per-answer directory holding the snapshot (sandboxed, token in the
  environment).
- `mcp.json` names a `crew` server, `crew aside-tools`. It serves the
  member's pinned tool catalog byte for byte and refuses every call.
  Toolbox servers are served the same way from their pinned definitions.
- The aside tone mod carries only the `prompt.compose` tone section of the
  bridge mod: no delivery pump, no reports.
- No `--allowedTools` or bypass flag.

**Codex**:

```text
codex --no-daemon exec --skip-git-repo-check --json -C <member workdir>
  <the member's -c overrides: developer_instructions, model, effort, provider, toolbox servers>
  -c mcp_servers.crew.command=<crew> -c mcp_servers.crew.args=["aside-tools", …]
  --sandbox read-only -c approval_policy="never" -c web_search=<the member's setting>
  fork --ephemeral <thread> <preamble and question>
```

`CODEX_HOME` is the member's (unsandboxed, operator login) or the per-answer
directory with the rollout copied into `sessions/` (sandboxed). An app-server
is not used; the member's sidecar is never contacted.

Inside crew's own sandbox, the member runs Codex with
`--dangerously-bypass-approvals-and-sandbox`. Its aside keeps that flag, so
the tool definitions match. It then relies on crew's read-only profile and on
`crew aside-tools`, which refuses every call. Codex's `--sandbox read-only`
is for members that run outside crew's sandbox. This combination is not
probed here; B11 covers it.

**pi**:

```text
pi --print --mode json --provider <p> --model <m> --append-system-prompt <member's>
  --extension <aside extension> --session-dir <per-answer dir> --fork <snapshot file>
  <preamble and question>
```

Run with `PI_CODING_AGENT_DIR` = the member's. The aside extension stands in
for `crew-pi.ts`: same crew tool definitions, every call refused, built-in
acting tools blocked in `tool_call`. A pi without `--fork` in its `--help` is
unavailable ("Runtime cannot copy sessions").

### Availability

Computed by the daemon, sent with the aside list, and shown disabled in the
picker with its reason:

| Reason | When |
| --- | --- |
| Shell member | `runtime` is shell. |
| No session yet | No `session_id`, or no session file found yet. |
| Runtime cannot copy sessions | The installed CLI lacks the fork options above (version check from `--help`). |
| Session file unreadable | The file cannot be opened beneath the agent's tree, or it is not regular. |
| Usage limit reached until HH:MM | The member's account is at its limit (`agent.limit`); the aside would fail. |
| Already answering an aside | One answer in flight per member; a second aside waits instead (status Waiting), so this is shown only in the picker hint. |

Paused, idle, working and parked members can all answer: the answer comes
from a copy.

### Cost and cache

Every answer is a paid model turn that reads the member's whole context.

- **Warm.** Within the member's cache lifetime, almost all of that input is
  a cache read, and only the preamble, the question and any unfinished
  in-flight content are new. For example, Claude's one-hour writes in the
  baseline.
- **Cold.** After the cache expires, the whole context is written again, and
  one aside can cost more than an ordinary turn.
- **Side effect.** The aside's cache read refreshes the shared prefix, but it
  is not a keep-alive and must not be used as one (`notes/prompt-cache.md`
  D2).
- **Accounts and usage.** An aside uses the member's account and quota. Its
  usage is reported per answer, and recorded for `usage-metrics-design` as
  source `aside` with agent and model. It is not attributed to any task or
  epic, and not mixed into the member's own hit rate.
- **pi on OpenAI (unverified).** pi uses the session id as
  `prompt_cache_key` there, so a fork, which has a new id, may be routed
  away from the member's cache. Anthropic providers are unaffected.

### Privacy

The model answering an aside sees everything the member has seen, so an
answer can contain whatever the context holds. Answers are kept only in the
daemon's memory and are sent only to operator dashboard clients.

- Asking needs the Control grant tier; reading answers needs Read.
- No body appears in events, logs, transcripts, the bus, inboxes, the board,
  the roster, briefings, task results or usage records. Usage records carry
  token counts only.
- Snapshot copies and fork files are deleted when the answer settles.

### Retention

Asides are kept in the daemon's memory: the last 50, with each answer cut to
64 KiB.

- They are not persisted. An aside reflects one moment, and its answers may
  hold anything in a member's context. Keeping them off disk keeps them out
  of backups and the state directory.
- They survive a dashboard reconnect but not a daemon restart.
- The dashboard note in §Asides "Kept where" therefore reads "Asides from
  before the daemon restarted are gone". It is shown when the daemon's start
  time is later than the newest aside the client had.
- **Remove for everyone** deletes the aside from the daemon's list for every
  client. It does not undo usage.

## Protocol and entry points

These records are this design's proposal. The final wire shape, the v6
capability flag and the shared `rpc.rs`, client and FFI ownership are settled
by `feature-protocol-plan` (native-architect, m-58efd3a1449f): additive, with
the aside calls as distinct scoped RPC methods and no unscoped fallback for an
older daemon. A client that does not see the capability offers no aside.

Wire types in `crew-protocol` (operator calls only; no agent tool, so members'
tool catalogs and prefixes are unchanged):

- `Aside { id, question, asked_at, answers: Vec<AsideAnswer> }`.
- `AsideAnswer { agent, runtime, model, status, reason, text, truncated,
  context_at, started_at, finished_at, usage }`.
  - `status`: waiting, answering, answered, failed, cancelled or
    not_available.
  - `usage`: input, cache read, cache write and output, each optional, so
    "not reported" stays distinct from zero; cost when the runtime reports
    it.
- `aside_ask { question, agents?: [id] }` (absent: everyone available) returns
  the new `Aside`. `aside_cancel { id }`, `aside_remove { id }`,
  `aside_list {}` returns asides and, per member, availability with its
  reason and `last_turn_at`.
- Push `aside` with the updated record after every status change. The
  snapshot carries the list for a reconnecting client.
- Events: `aside.asked { id, agents }` when asked, and `aside.answered { id,
  answered, failed, cancelled, not_available }` once, when the last answer
  settles. Neither has the question or an answer.

Entry points:

- web: `/btw` in the composer and command line, **Ask aside…** in Commands,
  `b` for the Asides tab (designer's spec).
- TUI: `/btw <question>` in the command line, and an answers panel.
- CLI: `crew btw [--to a,b] <question>` waits and prints each answer as it
  settles (`--no-wait` prints the id). `crew btw list` and `crew btw cancel
  <id>` complete it.
- GTK and Apple: later, per the designer's table.

## Limits

- An answer reflects the copy: the member's context up to the snapshot,
  including an in-flight prompt but not the unfinished reply. A member busy in
  a long tool call answers as of its last completed step.
- The aside reproduces the member's request only as far as crew reproduces
  the member's launch. Every CLI upgrade can move a flag's effect on the
  request, as Codex's read-only sandbox does for web search. B02 runs per
  supported CLI version, and a failure there turns availability off for that
  version instead of silently paying full price.
- Answers cannot act: no edits, commands with effects, messages, tasks or
  network writes. A question whose answer needs action belongs in a message.
- Each answer costs about one turn per member, more when the member's cache
  has expired. There is no estimate before sending (designer).

## Acceptance cases

QA runs these with fake CLIs or the stub endpoint (B01–B09, B11, B12) and
one live run (B10).

| ID | Case | Pass when |
| --- | --- | --- |
| B01 | Untouched | Asides to every member, idle and mid-turn (each runtime, sandboxed and not). The member's session file, the CLI's thread records and every file under `<A>` and the CLI homes are byte-identical, apart from Codex's own log databases. Roster, inbox, bus, tasks, board, turn state and delivery queue are unchanged. No `crew hook`, `delivered`, `credential` or `mcp` call arrives under the member's identity. The member's next request count and content are as without the aside. |
| B02 | Prefix | Per runtime and supported CLI version, the aside's first request equals the member's next request up to the new tail. It carries the crew catalog, toolbox servers (pinned), tone, long context and, on Codex, the member's search setting. |
| B03 | Refusal | Scripted write, shell, crew MCP and toolbox MCP calls are refused, a read succeeds, and the workspace and every MCP server's call log are unchanged. |
| B04 | Availability | Shell member, no session, CLI without fork, unreadable session and usage limit each show their reason. A member already answering queues a second aside as Waiting. |
| B05 | Mid-turn | An aside during a held live turn: the turn completes normally with its own reply only. The aside includes the in-flight prompt. A snapshot of a file whose last line is half-written ends at the last complete line. |
| B06 | Cancel and timeout | Cancel or the time limit kills the process group, reports Cancelled or Failed with the reason, keeps reported usage or "not reported", deletes the per-answer directory and leaves no process. |
| B07 | Isolation | One member's failure (bad credentials, crash) leaves the others' answers intact. The aggregate state counts it. |
| B08 | Events and privacy | `aside.asked` and `aside.answered` once per aside, without question or answer text. Neither text appears in the event log, daemon logs, transcripts, bus, inbox, board, roster, briefings or usage records. 51 asides keep the newest 50, and a long answer is cut at 64 KiB with the flag set. |
| B09 | Usage | Each answer reports input, cache read, cache write and output from the runtime's own usage, with "not reported" distinct from zero. Usage metrics record it as source `aside` for the agent and model, not attributed to a task or epic. |
| B10 | Cache, live | On a real endpoint with a cheap model (Claude Haiku 4.5; Codex's cheapest supported model), a member turn is followed within the TTL by an aside. The aside reports a cache read of at least the member's prefix, and the member's next turn still reads its cache. Model, TTL and endpoint are stated. |
| B11 | Sandbox | A sandboxed member's aside runs under its profile with the workspace read-only. Nothing is written under `<A>/rw`, and the per-answer directory is gone afterwards. |
| B12 | Grants | A gateway client with Read sees asides but cannot ask or cancel; Control can. |

## Implementation split by file ownership

1. **`aside-runtime` (runtime):**
   - the daemon's aside module (`crates/crew/src/aside/`): snapshot copy
     through `AgentFiles`, per-answer directories, runner with limits, cancel
     and sandbox, bounded in-memory store, availability;
   - RPC handlers, push and events in the daemon dispatch;
   - usage samples with source `aside`.

   Depends on the protocol types of item 3.
2. **`aside-launch` (graph, owner of the launch and toolbox proxy paths):**
   - an `aside` launch spec beside `launch` in `agents/claude.rs`,
     `agents/codex.rs` and `agents/pi.rs`;
   - `crew aside-tools`, serving the pinned crew catalog and toolbox
     definitions and refusing calls;
   - the tone-only Claude mod and the pi aside extension under
     `crates/crew/assets/`;
   - B02 per runtime.
3. **`aside-ui` (ui):**
   - `Aside` and `AsideAnswer` in `crew-protocol` with web fixtures;
   - the web composer mode, the Asides tab and Commands entry;
   - the TUI `/btw` command and panel;
   - `crew btw` in the CLI.

   Protocol types land first, in the shape `feature-protocol-plan` fixes,
   so 1 and 2 build on them.
4. **`aside-acceptance` (qa):** B01–B12 with the btw probes as fixtures; B10
   in one live run with the operator's agreement on credentials and cost.

GTK and Apple follow later, as in the designer's table. No item changes
standing prompts, briefings or members' tool catalogs.

## Open items

- **Operator, through orchestrator, for B10.** It needs a real credential and
  a few cents per runtime.

Settled with the designer (m-302d057f4282, `dashboard-features.md` amended in
217a8311): the picker shows each member's last turn as a fact ("last turn
2 h ago"), from the daemon's last request time. It says nothing about the
cache, whose state the dashboard does not know. `aside_list` carries
`last_turn_at` per member for it.
