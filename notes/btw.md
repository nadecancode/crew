# Asides (`/btw`): ask the crew a side question without touching their sessions

Card `btw-mechanism`, 2026-10-08. This card is design and probes only; no
product source changed.

- **Versions probed:** Claude Code 2.1.291, Codex 0.159.0, and the supported
  pi 1.1.0 (`@earendil-works/pi-coding-agent`, installed under `target/` by
  `notes/toolbox-probes/install-pi.mjs`).
- **Probes:** in [btw-probes/](btw-probes/README.md). All are offline except
  the two live cache checks in §Cache, live. Revised after review
  m-8848a6dcb2b3 (R1 to R3).

Inputs:

- The operator's ask (w-2d3dcc6b066fa029): "some sort of /btw mechanism: talk
  to entire crew without impacting context window / workflow state".
- native-architect (relayed in m-041a71ea77e6):
  - the `agent_send` path appends to a member's context and changes turn and
    workflow accounting, so `/btw` must not reuse it;
  - the answer comes from a separate conversation over a read-only snapshot,
    with usage of its own;
  - it always costs tokens.
- reviewer (same relay):
  - it must fit the D1 delivery contract (`notes/prompt-cache.md` §Design D1)
    and never be a wake-up source;
  - it is out of band: an answer in the dashboard, never injected mid-turn;
  - it is clearly distinct from operator steering.
- designer, `notes/dashboard-features.md` §Asides and §Requirements on the
  design cards:
  - answers come from copies;
  - availability per member, with reasons;
  - asynchronous answers with their own status and usage;
  - Cancel;
  - body-less `aside.asked` and `aside.answered` events;
  - retention of the last 50 asides, 64 KiB per answer.
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
- its session files, and the CLI's records of them, are unchanged.

The answer appears in the dashboard (web Asides tab, TUI panel, `crew btw`).
It never goes to the member. **Send to claude-1 as a message…** in the
dashboard is the only way an aside's content reaches a live session, and it
goes through the ordinary composer.

An aside is not steering. Steering is an operator message that the member acts
on (D1's immediate path). An aside is a question about the member, answered by
a copy of it. Nothing about the live member's work changes.

## Findings

The offline probes point each CLI at a localhost stub
([stub.mjs](btw-probes/stub.mjs)). The stub records request bodies and
answers in the provider's own streaming format, so every CLI completes real
turns and writes real session files without a provider call. Homes,
repositories and credentials are throwaway fixtures.

### Every runtime can fork a session without touching it

| Runtime | Fork | The member's files after the aside | Probe |
| --- | --- | --- | --- |
| Claude Code | `claude --print --resume <session> --fork-session --no-session-persistence` | Transcript byte-identical. Nothing added or changed in the config directory. | [claude.mjs](btw-probes/claude.mjs) |
| Codex | `codex exec … fork --ephemeral <thread>` | Rollout byte-identical. The parent thread's row and the thread list unchanged. The SQLite files' bytes change, their rows do not (§Codex in the member's home). | [codex.mjs](btw-probes/codex.mjs), [codex-isolation.mjs](btw-probes/codex-isolation.mjs) |
| pi | `pi --print --fork <session file> --session-dir <aside dir>` | Session directory and agent directory unchanged. `--fork` cannot be combined with `--no-session`, so the fork is written to the aside's own directory, which crew deletes. | [pi.mjs](btw-probes/pi.mjs) |

With persistence on, both CLIs leave a record of the fork:

- Claude Code with `--fork-session` alone adds one transcript beside the
  member's.
- Codex without `--ephemeral` adds a rollout and a thread row.

Both are therefore always run without persistence.

### Forking a frozen copy without moving the login

The answer must come from a copy taken at the moment of asking. A member that
logs in with the operator's own files cannot have that login copied (see
`agents/home.rs`: a CLI that refreshes a copied token invalidates the
operator's login). So the aside reads a copy while keeping the member's login
directory. That works per runtime as follows.

- **Claude Code: `--resume <path to the copy>`.** The copy lives outside the
  config directory, and `CLAUDE_CONFIG_DIR` stays the member's.
  - The fork saw only the turns in the copy: a member turn made after the
    copy was taken did not appear.
  - Nothing was written to the projects directory.
  - The request matched the member's next turn up to the new tail, the same
    as a fork by session id.

  Evidence: [claude-copy.mjs](btw-probes/claude-copy.mjs).
- **pi: `--fork <path to the copy>`, with `--session-dir` pointing at the
  aside's directory.** `PI_CODING_AGENT_DIR` (models, settings, login) stays
  the member's.
- **Codex: a fork by path does not exist.** `exec fork <rollout path>` fails
  with "Session not found". `-c experimental_resume=<path>` is ignored and
  starts an empty thread ([codex-copy.mjs](btw-probes/codex-copy.mjs)). There
  are two cases:
  - **Credentials from the environment** (sandboxed members, API-key
    endpoints): the copy goes into a per-answer `CODEX_HOME` under
    `sessions/…`, and the fork runs there. The member's home is never
    opened. A copied rollout repeats the whole live history; the fresh home
    only adds its skills item after the history (codex.mjs, `fork-snapshot`).
  - **The operator's login file** (unsandboxed, `auth.json`): the fork runs
    in the member's `CODEX_HOME` by thread id. There is no separate copy.
    The snapshot is Codex's own read of the rollout when the fork starts.
    - A half-written last line was skipped, and the fork started with the
      history before it.
    - Hooks, memories, ambient MCP servers and exec-policy rules are
      switched off for the fork (below).
    - SQLite row contents are checked unchanged (below).

### Codex in the member's home

[codex-isolation.mjs](btw-probes/codex-isolation.mjs) sets up a member home
the way an operator's looks: a user hook and an ambient MCP server, each
leaving a marker when it runs. The member's own runs fired the hook and
started crew's server. The aside's fork ran with:

- `-c features.hooks=false`;
- `-c features.memories=false`;
- every ambient server disabled by name (`-c mcp_servers.<name>.enabled=false`,
  the list from `codex mcp list --json`, as the toolbox closing step builds
  it);
- crew's server replaced by a stand-in;
- `--ignore-rules`;
- `--sandbox read-only -c approval_policy="never"`.

Results:

- No hook ran and the ambient server never started. Only the stand-in did.
- Tools, `instructions` and the whole live history matched the member's
  request, and the cache key was the parent thread's.
- A row-level dump (`sqlite3 .dump`) of every Codex SQLite file was identical
  before and after the fork. The files' bytes change (WAL and checkpoint
  bookkeeping), their rows do not.

B01 therefore checks rows, not bytes, for these files.

### Mid-turn: the live turn finishes normally; the aside sees its prompt

A fork taken while the member's turn was in flight did not disturb it. The
probes held the member's model request at the stub to keep the turn open.
The live turn then completed, and its session gained its own reply and
nothing from the aside (Claude Code and Codex).

The fork includes the in-flight user message, then a marker the CLI adds to
the fork only:

- Claude Code synthesizes the assistant reply "No response requested.".
- Codex adds a `<turn_aborted>` user item ("The user interrupted the previous
  turn on purpose…").

The aside's preamble (§Mechanism) tells the model that it is answering a side
question and must not continue the work.

### Refusing every acting call, whatever the member's sources grant

A member's own settings can grant tools: a project's
`.claude/settings.json` `permissions.allow`, an operator's settings, or a
Codex exec-policy rule. The aside's refusal must hold regardless. It must
also keep the member's tool definitions, or the request no longer repeats the
member's prefix.

In the probes, the stub asked for a write, a shell command, a crew MCP call
and a read. For Claude Code, `.claude/settings.json` and the operator settings
allowed Write, Edit, Bash and the crew MCP tool. For Codex, a rules file
allowed `touch`.

| Runtime | Setting | Write | Shell | crew MCP | Read | Tool definitions |
| --- | --- | --- | --- | --- | --- | --- |
| Claude Code | `--permission-mode dontAsk` | **ran** | **ran** | **ran** | ran | kept |
| Claude Code | `dontAsk` plus `permissions.deny` naming the tools, or with `(*)` patterns | refused | refused | refused by name; **ran** with the pattern | ran | **removed** |
| Claude Code | `dontAsk` plus `permissions.ask: ["*"]` in the aside's flag settings | refused | refused | refused | refused | **kept** |
| Claude Code | default mode with `--permission-prompts none` (no grants) | **ran** | **ran** | refused | ran | kept |
| Codex | `--sandbox read-only`, `approval_policy="never"` | **ran** (rule) | read-only | refused | ran | kept |
| Codex | the same plus `--ignore-rules` | refused | read-only | refused | ran | **kept** |
| pi | `tool_call` guard extension, `--no-extensions` | refused | refused | refused | allowed by the guard's list | kept |

Sources: [claude-allow.mjs](btw-probes/claude-allow.mjs),
[claude-deny.mjs](btw-probes/claude-deny.mjs),
[codex-deny.mjs](btw-probes/codex-deny.mjs), `fork-guard` in
[pi.mjs](btw-probes/pi.mjs).

The aside therefore uses:

- **Claude Code:** `dontAsk` with `permissions.ask: ["*"]` in its flag
  settings. Ask rules outrank allow rules from every source, and `dontAsk`
  refuses whatever would ask, so no tool runs at all, including reads. A
  Claude Code aside answers from context only.
- **Codex:** `--sandbox read-only`, `approval_policy="never"` and
  `--ignore-rules`. Commands can only read, under Codex's OS sandbox. MCP
  calls need approval, which `never` refuses.
- **pi:** the aside's guard, as the only extension besides built-ins. It
  allows `read`, `grep`, `find` and `ls` and blocks every other call,
  including MCP tools. No ambient extension is loaded to compose with it.

Defence in depth: every MCP server the aside starts is crew's stand-in, which
refuses calls. A crew tool call that got through would therefore still never
reach the daemon.

### The request repeats the member's prefix, if the aside keeps the member's tool definitions

A cache read is possible only if the aside's request repeats the member's
prefix: tools, system prompt, then messages. The probes compare the aside's
request with the member's last request, and for Claude Code also with the
member's next one.

- Tools, system prompt and Codex `instructions` are compared byte for byte.
- History items are compared without their own top-level item id (Codex)
  and without cache markers (Claude Code and pi). Each CLI moves the marker
  to the newest message on every request.
- Each report also gives the byte-for-byte message count. For pi, for
  example, 3 of 3 messages are equal without markers and 2 of 3 byte for
  byte, the difference being the moved marker.

Equality makes an aside **eligible** to read the member's
cache. Whether it does depends on the provider (next section).

- **Claude Code.** With the member's own definitions, the fork's tools and
  system prompt are byte-identical. It repeats every message that the
  member's own next request repeats: against the member's next turn, the
  first 6 of 8 messages are identical. The other two are the new prompt and
  the per-request token note, which Claude Code rewrites on every request.
  - None of these enter the request:
    - the permission mode (`dontAsk` or bypass);
    - `--allowedTools`;
    - hooks;
    - `disableAllHooks`;
    - `permissions.ask: ["*"]`;
    - an MCP server with the same name and definitions but a different
      command.
  - Plan mode adds a request parameter and a message.
  - Deny rules, `--disallowedTools` and `--tools` remove definitions. With
    tool search off (a gateway endpoint), so does dropping an MCP server.
- **Codex.** The fork sends the same `instructions`, tools and the whole live
  input. Its `prompt_cache_key` is the parent thread's id.
  - `--sandbox read-only` adds a permissions item and an environment item
    after the history.
  - It also turns `external_web_access` off in the `web_search` definition;
    `-c web_search=<the member's setting>` restores it.
  - `--ignore-rules`, `features.hooks=false` and `features.memories=false`
    leave the request unchanged.
- **pi.** The fork sends the member's tools, system prompt and messages
  unchanged. pi lists the active tools in its system prompt, so `--tools` or
  `--no-tools` change it. Restriction is done at call time instead.

### Cache, live

The card allows minimal model calls with a cheap model. Two live checks
([claude-live.mjs](btw-probes/claude-live.mjs),
[codex-live.mjs](btw-probes/codex-live.mjs)) used the operator's own CLI
logins in place, with no copy. Each ran two member turns, an aside forked from
the member's session, then a third member turn, a few seconds apart. Usage is
the CLI's native report.

**Claude Code**, Claude Haiku 4.5, first-party endpoint, one-hour cache
buckets. The member's transcript lived in the operator's projects directory
and was removed afterwards.

| Request | Cache read | Cache write (1 h) | Input | Output |
| --- | --- | --- | --- | --- |
| member turn 2 | 27,695 | 121 | 10 | 64 |
| aside (forked from a frozen copy) | 27,816 | 141 | 10 | 106 |
| member turn 3 | 27,816 | 111 | 10 | 58 |

The aside read everything the member had written up to its turn 2: the 27,695
it read plus the 121 it wrote. It wrote only its own new tail. The member's
next turn still read its full prefix.

**Codex**, GPT-6-Luna at low effort, the operator's ChatGPT login, the
in-place ephemeral fork. Codex's `turn.completed` counters are cumulative per
thread (a fork inherits its parent's), so these are the differences between
runs. The member's thread was deleted afterwards with `codex delete --force`.

| Request | Input | Cached input | Cache write |
| --- | --- | --- | --- |
| member turn 2 | 23,799 | 23,296 | 0 |
| aside (ephemeral fork) | 23,849 | 23,296 | 0 |
| member turn 3 | 23,820 | 23,296 | 0 |

The aside's cached share (97.7%) matches the member's own next turn. As in the
baseline, Codex's write counter is zero.

Limits of this evidence:

- one run per runtime, warm within seconds;
- no cold case and no TTL-refresh measurement;
- no pi run (no supported-pi login on this host);
- not through crew's own launch.

The cost claims below are conditional on the same model, endpoint and
eligible prefix. B10 repeats the check through crew's aside launch.

### Hooks, extensions and servers must not start

With the member's flags, a fork fired the member's `SessionStart`,
`UserPromptSubmit` and `Stop` hooks and started the member's MCP server. For a
crew member those report to the daemon under its identity. Ambient hooks,
extensions and servers from the operator or the project could act before any
tool guard.

- **Claude Code.**
  - `disableAllHooks: true` in the aside's flag settings stopped project hooks
    ([claude-hooks.mjs](btw-probes/claude-hooks.mjs)).
  - `--strict-mcp-config` with only stand-ins means no other server starts.
  - The crew bridge mod is replaced by a tone-only mod.
- **Codex.** Hooks off, memories off, ambient servers disabled by name, crew's
  server replaced, rules ignored (§Codex in the member's home).
- **pi.** An extension runs its own code when it loads, before any
  `tool_call` guard can act; the reviewer reproduced an ambient extension
  writing to the workspace during a guarded fork. So ambient code must not
  load at all. Crew's tools come only from definition-only stand-ins.
  `--no-extensions` stopped both an ambient extension and an ambient MCP
  server ([pi-isolation.mjs](btw-probes/pi-isolation.mjs)). Restoring
  native MCP with `-e builtin:mcp`, needed for crew's own server, starts the
  operator's servers again. So:
  - Unsandboxed: the aside runs without native MCP. Crew's MCP tools are then
    missing, so the request does not repeat the member's prefix. The answer is
    marked as such (§Protocol `prefix_reproduced`).
  - Sandboxed: the private agent directory holds no ambient servers, so the
    aside restores native MCP with crew's stand-in.

## Mechanism

### Per answer

1. **Availability** (§Availability). An unavailable member gets status Not
   available, with the reason.
2. **Snapshot.** The daemon opens the member's session file read-only through
   `AgentFiles`:
   - the operator's directories when unsandboxed;
   - beneath `<A>/rw` in a sandbox, with no link followed.

   It copies the file up to its last complete line, so a line still being
   written is left out and the copy is always valid JSONL. The copy goes into
   a private per-answer directory under the daemon's state, never under
   `<A>/rw`. The copy's last entry time is the "context at" time shown with
   the answer.

   The one exception is Codex on the operator's login file, which forks in
   place (§Per runtime).
3. **Launch** the runtime's aside spec (§Per runtime) in the member's working
   directory. It reproduces from the member's launch:
   - the model, with its long-context tag and effort;
   - the endpoint environment;
   - the bytes of the member's standing instructions;
   - the tone;
   - the tool definitions: crew's catalog and toolbox servers, served by
     stand-ins from their pinned snapshots, plus plugins and skills;
   - the setting sources.

   It never carries the member's identity, hooks, bridge mod, pi extension or
   crew MCP server.
   - **Credentials.** Endpoint secrets go into the process environment, as
     member launches already do. A login that lives in the operator's files
     stays where it is: the aside keeps the member's config directory, never
     a copy (§Forking a frozen copy).
   - **Sandbox.** A sandboxed member's aside runs under the member's sandbox
     profile. The workspace is bound read-only, and the per-answer directory
     is the only writable tree.
4. **Prompt.** The operator's question is preceded by a fixed preamble:
   "[crew aside] The operator asks a side question. Answer it from what you
   know now. Your tools cannot change anything; do not continue your work."
   Both are new tail input.
5. **Result.** The daemon reads the answer and the runtime-native usage:
   - Claude Code: `--output-format json` gives `result`, `usage` and
     `total_cost_usd`.
   - Codex: `--json` gives the last `agent_message`, and `turn.completed`
     usage minus the snapshot's cumulative counters.
   - pi: `--mode json` gives the final assistant message and `usage`.

   The answer is stored truncated to 64 KiB, with a flag. Then the
   per-answer directory is deleted.
6. **Bounds.**
   - A per-answer wall-clock limit of 5 minutes, plus Claude Code's
     `--max-budget-usd`.
   - At most one answer in flight per member and four per daemon; the rest
     wait.
   - Cancel and the time limit kill the process group, set the status, keep
     whatever usage was reported (else "not reported"), and delete the
     directory.

### Per runtime

**Claude Code**:

```text
claude --print --resume <per-answer copy.jsonl> --fork-session --no-session-persistence
  --output-format json --max-budget-usd <limit>
  --model <member model> --append-system-prompt-file <member briefing copy>
  --mcp-config <aside mcp.json> --strict-mcp-config
  --settings <member settings without hooks or apiKeyHelper,
              plus disableAllHooks: true and permissions.ask: ["*"]>
  [--setting-sources as the member] [--plugin-dir <aside tone mod>] [toolbox plugin dirs]
  --permission-mode dontAsk -- <preamble and question>
```

- `CLAUDE_CONFIG_DIR`: the member's. Unsandboxed, that is where the operator's
  login is. Sandboxed, it is the private home, with the token in the
  environment.
- `mcp.json`: a `crew` server, `crew aside-tools`. It serves the member's
  pinned tool catalog byte for byte and refuses every call. Toolbox servers
  are served the same way from their pinned definitions.
- A member that inherits the operator's MCP servers (no strict loading) gets
  an aside with strict loading anyway. The prefix then differs, and the answer
  says so.
- No `--allowedTools` or bypass flag.

**Codex**:

```text
codex --no-daemon exec --skip-git-repo-check --json -C <member workdir>
  <the member's -c overrides: developer_instructions, model, effort, provider, toolbox servers>
  -c mcp_servers.crew.command=<crew> -c mcp_servers.crew.args=["aside-tools", …]
  -c mcp_servers.<each other listed server>.enabled=false
  -c features.hooks=false -c features.memories=false --ignore-rules
  --sandbox read-only -c approval_policy="never" -c web_search=<the member's setting>
  fork --ephemeral <thread> <preamble and question>
```

- `CODEX_HOME` depends on where the credential comes from:
  - from the environment: a per-answer home holding the copy under
    `sessions/`;
  - from the operator's `auth.json`: the member's home, forking in place.
- An app-server is not used, and the member's sidecar is never contacted.
- Inside crew's own sandbox, the member runs Codex with
  `--dangerously-bypass-approvals-and-sandbox`. Its aside keeps that flag so
  the definitions match. It relies on crew's read-only profile, the stand-ins
  and `--ignore-rules`. This combination is not probed here; B11 covers it.

**pi**:

```text
pi --print --mode json --provider <p> --model <m> --append-system-prompt <member's>
  --no-extensions [-e builtin:mcp, sandboxed only] -e <aside extension>
  --session-dir <per-answer dir> --fork <per-answer copy> <preamble and question>
```

- `PI_CODING_AGENT_DIR`: the member's.
- The aside extension stands in for `crew-pi.ts`:
  - when native MCP is loaded, it registers `crew aside-tools`;
  - its guard blocks every call except `read`, `grep`, `find` and `ls`.
- A pi without `--fork` in its `--help` is unavailable ("Runtime cannot copy
  sessions").

### Availability

Computed by the daemon, sent with the aside list, and shown disabled in the
picker with its reason:

| Reason | When |
| --- | --- |
| Shell member | `runtime` is shell. |
| No session yet | No `session_id`, or no session file found yet. |
| Runtime cannot copy sessions | The installed CLI lacks the fork options above (version check from `--help`), or B02 failed for that version. |
| Session file unreadable | The file cannot be opened beneath the agent's tree, or it is not regular. |
| Usage limit reached until HH:MM | The member's account is at its limit (`agent.limit`); the aside would fail. |
| Already answering an aside | One answer in flight per member; a second aside waits instead (status Waiting), so this is shown only in the picker hint. |

Paused, idle, working and parked members can all answer, because the answer
comes from a copy.

### Cost and cache

Every answer is a paid model turn over the member's whole context.

- **Eligible and warm.** The request repeats the member's prefix, and the
  member's cache is still live on the same model and endpoint. The prefix
  can then be a cache read. That happened in both live checks (§Cache,
  live), but it is not guaranteed: lifetime, routing and eviction are the
  provider's.
- **Cold, or not eligible.** The cache has expired, or the prefix could not
  be reproduced (`prefix_reproduced: false`). The whole context is then
  written or read uncached, so one aside can cost more than an ordinary turn.
- **Not a keep-alive.** An aside may refresh the shared prefix's lifetime;
  that was not measured. It is not a keep-alive and must not be used as one
  (`notes/prompt-cache.md` D2).
- **Accounts and usage.** An aside uses the member's account and quota. Its
  usage is reported per answer. It is recorded for `usage-metrics-design` as
  source `aside`, with agent and model, and is not attributed to any task or
  epic or mixed into the member's own hit rate.
- **pi on OpenAI (unverified).** pi uses the session id as
  `prompt_cache_key`, so a fork (new id) may be routed away from the
  member's cache. Anthropic caches by prefix content.

### Privacy

The model answering an aside sees everything the member has seen, so an
answer can contain whatever the context holds.

- Answers are kept only in the daemon's memory and sent only to operator
  dashboard clients.
- Asking needs the Control grant tier; reading answers needs Read.
- No body appears in events, logs, transcripts, the bus, inboxes, the board,
  the roster, briefings, task results or usage records. Usage records carry
  token counts only.
- Copies and fork files are deleted when the answer settles.

### Retention

Asides are kept in the daemon's memory: the last 50, with each answer cut to
64 KiB.

- **Not persisted.** An aside reflects one moment, and its answers may hold
  anything in a member's context. Keeping them off disk keeps them out of
  backups and the state directory.
- **Lifetime.** They survive a dashboard reconnect but not a daemon restart.
  The dashboard note in §Asides "Kept where" reads "Asides from before the
  daemon restarted are gone". It is shown when the daemon's start time is
  later than the newest aside the client had.
- **Remove for everyone** deletes the aside from the daemon's list for every
  client. It does not undo usage.

## Protocol and entry points

These records are this design's proposal. `feature-protocol-plan`
(native-architect, m-58efd3a1449f) settles the final wire shape, the v6
capability flag, and the shared `rpc.rs`, client and FFI ownership:

- additive, with the aside calls as distinct scoped RPC methods;
- no unscoped fallback for an older daemon;
- a client that does not see the capability offers no aside.

Wire types in `crew-protocol` (operator calls only; no agent tool, so members'
tool catalogs and prefixes are unchanged):

- `Aside { id, question, asked_at, answers: Vec<AsideAnswer> }`.
- `AsideAnswer { agent, runtime, model, status, reason, text, truncated,
  context_at, started_at, finished_at, prefix_reproduced, usage }`.
  - `status` is one of waiting, answering, answered, failed, cancelled or
    not_available.
  - `prefix_reproduced` is false when the aside's request could not repeat
    the member's prefix (§Hooks, extensions and servers).
  - `usage` has input, cache read, cache write and output, each optional so
    that "not reported" stays distinct from zero, and cost when the runtime
    reports it.
- RPC methods:
  - `aside_ask { question, agents?: [id] }` returns the new `Aside`. Without
    `agents`, every available member is asked.
  - `aside_cancel { id }`.
  - `aside_remove { id }`.
  - `aside_list {}` returns the asides and, per member, availability with its
    reason and `last_turn_at`.
- Push `aside` with the updated record after every status change. The
  snapshot carries the list for a reconnecting client.
- Events, neither carrying the question or an answer:
  - `aside.asked { id, agents }` when asked;
  - `aside.answered { id, answered, failed, cancelled, not_available }` once,
    when the last answer settles.

Entry points:

- **Web:** `/btw` in the composer and command line, **Ask aside…** in
  Commands, `b` for the Asides tab (designer's spec).
- **TUI:** `/btw <question>` in the command line, and an answers panel.
- **CLI:** `crew btw [--to a,b] <question>` waits and prints each answer as it
  settles; `--no-wait` prints the id instead. `crew btw list` and `crew btw
  cancel <id>` complete it.
- **GTK and Apple:** later, per the designer's table.

## Limits

- **An answer reflects the copy.** It sees the member's context up to the
  snapshot, including an in-flight prompt but not the unfinished reply. A
  member busy in a long tool call answers as of its last completed step.
- **Tool use differs by runtime.** A Claude Code aside cannot use any tool,
  not even a read, because only a rule covering every tool refuses calls that
  the member's sources grant. Codex and pi asides can read.
- **The prefix is reproduced only as far as crew reproduces the member's
  launch.** A CLI upgrade can change what a flag does to the request, as
  Codex's read-only sandbox does to web search. B02 runs per supported CLI
  version, and a failure there turns availability off for that version.
- **Answers cannot act:** no edits, no commands with effects, no messages,
  tasks or network writes. A question whose answer needs action belongs in a
  message.
- **Cost:** about one turn per member, more when cold or not eligible. There
  is no estimate before sending (designer).

## Acceptance cases

QA runs these with fake CLIs or the stub endpoint (B01–B09, B11, B12) and
live through crew's launch (B10).

| ID | Case | Pass when |
| --- | --- | --- |
| B01 | Untouched | Asides go to every member, idle and mid-turn, on each runtime, sandboxed and not. The following are all unchanged: <ul><li>The member's session file, and every file under `<A>`, are byte-identical.</li><li>The CLI homes are byte-identical, except Codex's SQLite files, whose rows (`.dump`) are identical.</li><li>The thread list.</li><li>The roster, inbox, bus, tasks, board, turn state and delivery queue.</li></ul> No `crew hook`, `delivered`, `credential` or `mcp` call arrives under the member's identity. No sentinel hook, ambient extension or ambient MCP server starts. The member's next request is the same in count and content as without the aside. |
| B02 | Prefix | Per runtime and supported CLI version, where `prefix_reproduced` is true, the aside's first request equals the member's next request up to the new tail. It carries the crew catalog, pinned toolbox servers, tone, long context and, on Codex, the member's search setting. |
| B03 | Refusal | The member's project and operator settings, and a Codex rule, grant Write, Bash and MCP. Scripted write, shell, crew MCP and toolbox MCP calls are all refused, the workspace and every MCP server's call log are unchanged, and Codex and pi reads succeed. |
| B04 | Availability | Shell member, no session, CLI without fork, unreadable session and usage limit each show their reason. A member already answering queues a second aside as Waiting. |
| B05 | Mid-turn and frozen copy | An aside during a held live turn: the turn completes normally, with only its own reply. The aside includes the in-flight prompt. A member turn after the snapshot does not appear in the aside, except for in-place Codex. A copy of a file with a half-written last line ends at the last complete line. |
| B06 | Cancel and timeout | Cancel or the time limit kills the process group, reports Cancelled or Failed with the reason, keeps reported usage or "not reported", deletes the per-answer directory and leaves no process. |
| B07 | Isolation | One member's failure (bad credentials, crash) leaves the others' answers intact. The aggregate state counts it. |
| B08 | Events and privacy | `aside.asked` and `aside.answered` arrive once per aside, without question or answer text. Neither text appears in the event log, daemon logs, transcripts, bus, inbox, board, roster, briefings or usage records. 51 asides keep the newest 50, and a long answer is cut at 64 KiB with the flag set. |
| B09 | Usage | Each answer reports input, cache read, cache write and output from the runtime's own usage, with "not reported" distinct from zero. Codex counters are taken as differences from the snapshot. Usage metrics record it as source `aside` for the agent and model, not attributed to a task or epic. |
| B10 | Cache, live | Through crew's aside launch, on a real endpoint with a cheap model (Claude Haiku 4.5; Codex GPT-6-Luna at low effort), a member turn is followed within the TTL by an aside. The aside reports a cache read covering the member's prefix, and the member's next turn still reads its cache. Model, TTL and endpoint are stated. This card's direct-CLI checks (§Cache, live) are the precedent. |
| B11 | Sandbox | A sandboxed member's aside runs under its profile with the workspace read-only. Nothing is written under `<A>/rw`, and the per-answer directory is gone afterwards. |
| B12 | Grants | A gateway client with Read sees asides but cannot ask or cancel; Control can. |

## Implementation split by file ownership

1. **`aside-runtime` (runtime):**
   - the daemon's aside module (`crates/crew/src/aside/`):
     - the snapshot copy through `AgentFiles`;
     - per-answer directories;
     - the runner, with limits, cancel and sandbox;
     - the bounded in-memory store;
     - availability;
   - RPC handlers, push and events in the daemon dispatch;
   - usage samples with source `aside`.

   Depends on the protocol types of item 3.
2. **`aside-launch` (graph, owner of the launch and toolbox proxy paths):**
   - an `aside` launch spec beside `launch` in `agents/claude.rs`,
     `agents/codex.rs` and `agents/pi.rs`, including the Codex list of
     ambient servers to disable;
   - `crew aside-tools`, which serves the pinned crew catalog and toolbox
     definitions and refuses calls;
   - the tone-only Claude mod and the pi aside extension under
     `crates/crew/assets/`;
   - B02 per runtime.
3. **`aside-ui` (ui):**
   - `Aside` and `AsideAnswer` in `crew-protocol`, with web fixtures;
   - the web composer mode, the Asides tab and the Commands entry;
   - the TUI `/btw` command and panel;
   - `crew btw` in the CLI.

   Protocol types land first, in the shape `feature-protocol-plan` fixes, so
   items 1 and 2 build on them.
4. **`aside-acceptance` (qa):** B01–B12, with the btw probes as fixtures.

GTK and Apple follow later, as in the designer's table. No item changes
standing prompts, briefings or members' tool catalogs.

## Settled

- **Designer** (m-302d057f4282, `dashboard-features.md` amended in 217a8311):
  the picker shows each member's last turn as a fact, such as "last turn 2 h
  ago", from the daemon's last request time. It says nothing about the cache,
  whose state the dashboard does not know. `aside_list` carries
  `last_turn_at` per member for it.
- **Live model calls** are covered by the card ("may make minimal model
  calls with cheap models"). The two live checks touched only:
  - a transcript in the operator's Claude projects directory, removed
    afterwards;
  - a thread in the operator's Codex history, deleted with `codex delete
    --force`;
  - a few cents of usage on each account.
