# crew

Run a team of coding agents — Claude Code, Codex and pi — that talk to each
other directly, and watch the whole thing in a terminal dashboard.

`crew` launches 1–10 real agent sessions, gives each a role and a task, and
connects them with a local message bus. Workers message each other
peer-to-peer; the orchestrator plans and supervises but is never a relay. A
background daemon owns the agents, so closing the dashboard does not stop
anything, and `crew attach` brings the live state back.

```
                          ┌─ ✱ claude-1 ───────┐                 ┌─ ✱ claude-2 ───────┐
                          │ billing     ◐  51% │·················│ queue       ✓ 100% │
                          └━━━━━━━━━━──────────┘                 └━━━━━━━━━━━━━━━━━━━━┘
┌─ ✱ claude-6 ───────┐                ┌─ ◆ orchestrator ─────┐               ┌─ ✱ claude-3 ───────┐
│ search      ✓ 100% │·  ·  ·  ·  ·  ·│ status · 4/6         │·  ·  ·  ·  ·  │ ledger      ◐  70% │
└━━━━━━━━━━━━━━━━━━━━┘                │ ○ waiting            │               └━━━━━━━━━━━━━━──────┘
                          ┌─ ✱ claude-5 ───────┐                 ┌─ ✱ claude-4 ───────┐
                          │ db          ✓ 100% │·················│ notify      ○   0% │
                          └━━━━━━━━━━━━━━━━━━━━┘                 └────────────────────┘
 message bus ── private channel · orchestrator not in loop
 10:14:17.934      claude-3 → claude-5      ‹question› Can you add a committed_at column to transactions?
 10:14:50.274      claude-5 → claude-3      ‹artifact› Added it. Commit: bce26c8b
 10:15:03.042  orchestrator ⇉ claude-4      ‹task› Task "Design notifications" is yours now.
 p2p channel OPEN · agents talking directly
```

Everything in the dashboard is real: each box is a running agent process, and
each row in the bus log is a persisted message.

## Architecture

```
            ┌────────────┐   ┌─────────────┐
            │ crew attach│   │ crew status │      clients (dashboard, CLI)
            └─────┬──────┘   └──────┬──────┘
                  │  Unix socket, newline-delimited JSON, pushes (no polling)
            ┌─────▼─────────────────▼──────┐
            │          crew daemon         │──── SQLite (.crew/crew.db):
            │  scheduler · message bus     │     agents, tasks, messages, events
            │  supervisor · jj / git work- │
            │  spaces                      │
            └──┬───────────────┬───────────┘
   process     │               │      agent API
   backend     │               │      (per-agent token)
   ┌───────────▼──┐      ┌─────▼────────────────────────────┐
   │ pty │ herdr  │      │ crew mcp (MCP tools) · crew ctl  │
   └──┬───────┬───┘      │ hooks · socket delivery          │
      │       │          └─────▲────────────────────────────┘
  ┌───▼──┐ ┌──▼───┐            │
  │claude│ │codex │ … real interactive sessions, one workspace + branch each
  └──────┘ └──────┘
```

- The browser dashboard (`crew web`) is one more client: one per-user server
  (`crew web-server`) shows every joined workspace on the loopback addresses and
  carries the daemon's own protocol over WebSockets.
- The **daemon** is a detached process per workspace. It persists every
  change, so it can be killed and restarted without losing the session.
- **Agents** are ordinary interactive `claude` / `codex` sessions on real
  terminals: daemon-owned PTYs, or panes in [herdr](https://herdr.dev) (where
  they show up in the herdr panel as named agents with role and progress).
- **Messages** go sender → bus → recipient's session. See
  [Agent messaging](#agent-messaging).

## Installation

The default CLI requires Rust 1.82+ and a C compiler (SQLite is bundled).
Use the committed lockfile when installing with the minimum Rust version.

```sh
cargo install --locked --path crates/crew        # installs `crew`
# on NixOS / without a compiler on PATH:
./scripts/build-env cargo install --locked --path crates/crew     # or: just install
```

### macOS

crew builds for macOS 11 or newer (Apple silicon and Intel) the same way. On a Mac the C compiler comes with the Xcode command line tools
(`xcode-select --install`), and Rust with [rustup](https://rustup.rs):

```sh
cargo install --locked --path crates/crew
```

What differs from Linux:

- **No agent sandbox.** Sandboxes need Linux (bubblewrap, Landlock, seccomp),
  so `workspace.sandbox.mode: required` refuses to start and the default,
  `off`, runs agents unsandboxed, as on a Linux host without them.
- **Paths are opened the macOS way.** Where Linux uses `openat2` to refuse a
  symlink anywhere in a path, macOS uses `O_NOFOLLOW_ANY`; process
  identities come from `proc_pidinfo` and `kern.bootsessionuuid` instead of
  `/proc`.
- **The binary pin** (agents run the crew binary their daemon runs) copies
  the file the daemon was started from, read as it starts. Linux reads the
  running file itself, so on macOS a rebuild in that first moment is the gap.
- Sockets and runtime files are in `/tmp/crew-<uid>` (macOS has no
  `XDG_RUNTIME_DIR`). The web dashboard works in Chrome and Firefox; Safari
  cannot resolve its `*.localhost` address.

The macOS build is type-checked on Linux (`aarch64-apple-darwin`); its first
runs are on a Mac.

Working on crew itself? `just link` symlinks `~/.local/bin/crew` to this
checkout's release build instead, so `just build` is all it takes to update
the command.

Check the default CLI's minimum compiler with
`rustup toolchain install 1.82.0 --profile minimal`, then `just msrv-check`.
The gate uses the committed lockfile and a separate build directory; it builds
the CLI and parses help without installing it or starting a daemon.

## Prerequisites

- **Claude Code** 2.1+ on `PATH` as `claude`, logged in. crew passes its
  configuration per session (`--mcp-config`, `--settings`, `--plugin-dir`,
  `--append-system-prompt-file`); nothing is written to `~/.claude`.
- **Codex** 0.15x+ on `PATH` as `codex`, logged in (only for Codex agents).
  Configured per session with `-c` overrides; nothing is written to `~/.codex`.
- **pi** 1.0+ ([pi.dev](https://pi.dev), `@earendil-works/pi-coding-agent`),
  with a provider set up (only for pi agents): `npm install -g
  @earendil-works/pi-coding-agent`, or `bun add -g …` where npm's global
  prefix is read-only (Nix). Configured per session with `--extension`,
  `--append-system-prompt`, `--session-dir` and `--session-id`; nothing is
  written to `~/.pi`. crew runs the first `pi` it finds on `PATH`, then in
  `~/.bun/bin`, `~/.local/bin` and `~/.npm-global/bin`, that is pi: GSD
  (`gsd-pi`) installs a `pi` of its own with a different command line, which
  crew recognises and passes over, so pi need not come first on `PATH`.
  `CREW_PI=/path/to/pi` (in the environment `crew run` / `crew start` is run
  from) names the program outright. The [model picker](#changing-model-and-cli)
  says what it found, or what is missing.
- **jj** 0.44+ (`jj` on `PATH`, or `CREW_JJ=/path/to/jj`), the default for
  isolating agents: each agent gets a jj workspace, published as the git branch
  `agents/<id>`. Without jj, or older, crew runs on **git** worktrees and says
  so at every team start (see [Workspace isolation](#workspace-isolation)).
  git is needed either way: an empty directory is made a repository with
  `git init`, and the repository jj works on is git's.
- **bun** and node, only to rebuild the web dashboard's frontend (`web/`); the
  built bundle is committed, so installing crew needs neither.
- **herdr** (optional). Started inside a herdr pane, `backend: auto` puts
  every agent in its own pane of a `crew:<project>` workspace.
- **bubblewrap** 0.10+ (`bwrap` on `PATH`), unprivileged user namespaces and
  Linux 7.1+ with Landlock enabled (in the kernel's `lsm=` list; Landlock
  ABI 9), for agent sandboxes (not yet on by default). On an older kernel
  `workspace.sandbox.mode: required` refuses to start; `mode: off` runs
  agents unsandboxed. Each sandboxed agent runs in its own mount, PID and IPC
  namespace behind `crew sandbox-exec`, which drops inherited descriptors,
  refuses terminal-injection ioctls and, with Landlock, confines signals and
  refuses every socket made outside the sandbox but crew's own. The sandbox
  sees the system trees, the toolchain on `PATH` (read-only), the agent's
  worktree and its own state, and never the operator's token, `crew.db`,
  other agents or the rest of `$HOME`. The host network stays. `nix-shell` in
  this checkout provides bubblewrap; on Ubuntu 24.04 unprivileged user
  namespaces also need `kernel.apparmor_restrict_unprivileged_userns=0` (or
  an AppArmor profile for bwrap).

**Permissions.** By default agents run without their runtime's permission
checks: Claude Code with `--dangerously-skip-permissions`, Codex with
`--dangerously-bypass-approvals-and-sandbox` (yolo). They never stall on a
prompt — and nothing stops them from running any command. Set
`defaults.permissions: guarded` (or `permissions: guarded` on one agent) to
use Claude Code's `acceptEdits` mode and Codex's `workspace-write` sandbox
instead; an agent that then hits a prompt shows as `blocked` until you answer
it (`crew agent attach <id>`, or its herdr pane).

## Quick start

In any project directory — an empty one is fine:

```sh
mkdir payments && cd payments
crew                             # pick a work style, say what to do; then the dashboard
crew "design a payments service: billing, ledger, queue, notifications"
crew --style research "how do the two queue designs compare?"   # skip the chooser
```

That starts an orchestrator, and the orchestrator does the rest: it decides
which members the objective needs, launches them, breaks the work into tasks
and supervises. No team file is involved. What plain `crew` does depends on
the directory:

| The directory…                    | `crew` …                                                    |
|-----------------------------------|-------------------------------------------------------------|
| has a team running                | opens its dashboard; `crew "…"` hands the orchestrator a new objective |
| has a `crew.yaml` / `crew.toml`   | launches that team (as `crew run` would)                     |
| anything else                     | opens the launcher: choose a [work style](#work-styles), type the objective |

An empty directory is made a git repository so each agent gets its own
workspace. A directory that already has files is left as it is: if it is a
repository with a commit the agents get workspaces (jj, or git worktrees
without jj), otherwise they share it
(crew never commits your files for you). `--runtime codex` and
`--model <name>` choose what the team runs on, `--no-attach` skips the
dashboard. Plain `crew` refuses to use your home directory as a workspace.

Each agent commits to its own branch (`agents/<id>`) in its own workspace, so
when the team is done the directory itself has not changed yet. Land the
result with:

```sh
crew integrate                    # merge every agent branch into agents/integration
git merge agents/integration      # bring it into your checkout
```

Running `crew` in a directory is taken as the decision to let agents work
there: a team started this way answers the runtimes' "trust this folder?"
prompt itself (`defaults.auto_trust`), and, like every team, runs its agents
without permission prompts unless the team file says `permissions: guarded`
(see [Prerequisites](#prerequisites)).

The examples are team files, launched with `crew run`:

```sh
crew run examples/dry-run.yaml   # token-free: six scripted agents, same bus and dashboard
crew run examples/demo.yaml      # six Claude Code agents + an orchestrator
crew run examples/mixed.yaml     # Claude Code and Codex on one team
crew run examples/objective.yaml # only an objective: the orchestrator designs the team
```

Press `q` to leave the dashboard; the team keeps running. `S` (or `/stop`)
stops the team from inside it.

```sh
crew                 # dashboard again, current state (same as crew attach)
crew status          # plain-text summary   (--json, --frame 120x40)
crew logs -f         # structured event stream   (crew logs claude-3)
crew messages        # the bus history
crew questions       # what the team wants you to decide   (crew answer …)
crew stop            # stop the agents and the daemon
```

A line that starts with a slash is a command for crew, wherever you type it,
and never an objective or a message: `/exit` in the launcher leaves without
starting a team, and `crew "/exit"` is refused rather than handed to an
orchestrator. See [slash commands](#slash-commands).

`crew` finds the session from the current directory, or use `-C <workspace>`
/ `--project <name>` (`crew sessions` lists what is running).

## Work styles

A workspace is not always worked on the same way: one day the team
investigates a question, another day it runs experiments, another day it
builds, or cleans up and reviews what was built. A **work style** is a
preset for that: who is on the team and how the team works. You choose one
when a team is launched.

The built-in `cleanup` style is for the last of those. A survey comes first:
reviewers run the project's build, tests and linters as a baseline and write
findings to `review/<area>.md`. Cleaners then take those findings, each in a
part of the tree no one else edits, in small commits that change structure,
not behaviour. Every cleanup task is reviewed by a member on the other model
family. A final task integrates the branches, runs the baseline again and
writes `review/SUMMARY.md`. An objective that asks only for a review gets the
findings and the summary, and no code is changed.

```
 crew · new team                                                       /work/payments
┌ work style ────────────┐┌ research ────────────────────────────────────────────────┐
│   default    built-in  ││ Investigate a question. Gather evidence, cross-check it, │
│ ▸ research   built-in  ││ write up findings.                                       │
│   experiment built-in  ││                                                          │
│   cleanup    built-in  ││ team       staffed by the orchestrator                   │
│   field-notes project  ││ runtime    claude                                        │
│                        ││ last time  3 member(s): scout, skeptic, writer           │
│                        ││                                                          │
│                        ││ How the team works                                       │
│                        ││ This is a research team. The deliverable is knowledge,   │
│                        ││ written down: not code.                                  │
│                        ││                                                          │
│                        ││ Ground rules for every member:                           │
│                        ││ - Every claim in a deliverable says where it comes from: │
└────────────────────────┘└──────────────────────────────────────────────────────────┘
┌ objective ─────────────────────────────────────────────────────────────────────────┐
│ › how do the two queue designs compare?                                            │
└────────────────────────────────────────────────────────────────────────────────────┘
 ↑↓ style · type the objective · enter launch · pgup/pgdn scroll · esc cancel
```

`crew` opens this launcher whenever it is about to start a new team in a
terminal. `↑`/`↓` (or `tab`) pick the style, typing fills in the objective,
`enter` launches the team and opens the dashboard, `esc` leaves without
starting anything. `crew --style <name> "objective"` skips it; without a
terminal, the style used last in the workspace (or `default`) is used.

A style is a YAML file named after the style:

```yaml
# ~/.config/crew/styles/review.yaml
description: Review a change from several angles and report what to fix.
instructions: |            # part of every member's briefing, the orchestrator's too
  - Read the change before forming an opinion; quote the lines you refer to.
  - Report findings ordered by how much they matter. No style nitpicks.
  For the orchestrator: one reviewer per concern (correctness, security,
  tests), then a summary task that depends on all of them.
# Optional — everything a team file can say about the team:
# agents: [...]            # a fixed roster; left out, the orchestrator staffs it
# defaults: { runtime: codex, model: gpt-5.1 }
# isolation: shared        # auto | worktree | shared (vcs: auto | jj | git is under workspace)
defaults:
  model_fallbacks:         # while a model's window is used up, go on on these
    fable: [opus]          # (see "Fallback models" under Usage limits)
  refusal_fallbacks: [glm-5.3, deepseek-v4-flash]  # take over what a model refuses
```

Styles are found in fixed places; a later one replaces a style of the same
name:

| origin   | folder                              |
|----------|-------------------------------------|
| built-in | `default`, `research`, `experiment`, `cleanup` |
| global   | `/etc/crew/styles/`                 |
| user     | `~/.config/crew/styles/`            |
| project  | `<workspace>/.crew/styles/`         |

```sh
crew style list                        # what is available here, and the folders
crew style show research               # one style in full
crew style new review                  # a template in your user folder (--scope project)
crew style new my-research --from research   # a copy of a style, to adapt
```

Several styles can be used in the same workspace, one at a time. Each keeps
its own memory there: the team its orchestrator settled on
(`.crew/rosters/<style>.yaml`) and the objective it last worked on, both
offered again in the launcher. The style file itself is read fresh at every
launch, so an edit applies to the next team. To switch, `crew stop` the
running team and run `crew` again. The dashboard header and `crew status`
show which style a team runs in.

## Model catalog

A mixed team is worth having because the models differ: one is the better
planner, another the better second opinion, another is cheap enough for bulk
work. crew ships a catalog of the current Claude Code and Codex models with
what each is good for, and the orchestrator uses it when it staffs a team.

```sh
crew models                  # the catalog, what can be launched from here, which to pick for what
crew models show gpt-6-astra # one profile in full, with its sources
crew models init             # a template for your own additions and corrections
```

| model | runs on | tier | in short |
|---|---|---|---|
| `fable` | Claude Code | frontier | orchestration, architecture, research carried through to a write-up |
| `opus` | Claude Code | strong | large or long-running implementation, refactors |
| `sonnet` | Claude Code | balanced | everyday implementation, tests, docs |
| `haiku` | Claude Code | fast | helpers, lookups, cheap trial runs |
| `gpt-6-astra` | Codex | frontier | plan, design and code review of Claude's work; hard debugging |
| `gpt-6.1-sol` | Codex | balanced | everyday implementation on the Codex side |
| `gpt-6-luna` | Codex | fast | bulk extraction and summaries, helpers |

How it is used:

- **The orchestrator is briefed with it.** Its briefing lists the models that
  can be launched on this machine, what each is good and not good for, and
  which to pick for each kind of work (plan review → `gpt-6-astra`, then
  `fable`; and so on). It is told to have work checked by the other model
  family than the one that wrote it, and to keep frontier models for the few
  members whose judgment decides the outcome.
- **A model decides the runtime.** `agent_add(id, role, model)`, a team
  file, a style or `crew agent add --model gpt-6-astra` only need to name the
  model; crew starts the member on the CLI that runs it. A member without a
  model gets the team default, but only if that default can run on the
  member's CLI (a Codex member of a Claude team is not started with
  `--model sonnet`). Model names crew does not know are passed through as
  they are.
- **Only what is available is offered.** A model is left out of the
  briefing when its CLI is not on `PATH`, or when this Codex account's own
  model list does not include it.

The catalog belongs to crew, not to a project. It is compiled in, and two
files can add, replace (`[models.<name>]`) or remove (`disabled = true`)
entries and picks: `~/.config/crew/models.toml` and `/etc/crew/models.toml`.

Where the content comes from: each summary is the vendor's own description
(Anthropic's and OpenAI's model pages, and the model list Codex itself
serves); the "good for" lines and the picks are crew's reading of those
pages and of Artificial Analysis' benchmark write-up, checked on the date
`crew models` prints. They are a starting point. If a model does better or
worse for you at some kind of work, say so in your `models.toml`.

A `model` set under [Endpoints and keys](#endpoints-and-keys) still wins
over everything: an overridden endpoint decides which models exist.

## Starting a team

A team file (YAML or TOML) declares the project, the agents and optionally the
tasks:

```yaml
project: checkout-rewrite
objective: Rewrite the checkout service while preserving API compatibility.
agents:
  - id: architect
    role: architecture
  - id: backend
    role: backend implementation
    runtime: codex
  - id: tests
    role: testing
tasks:
  - id: design
    agent: architect
    description: Write the design for the new checkout flow.
  - id: implement
    agent: backend
    depends_on: [design]
    reviewer: architect
```

`crew run team.yaml` starts the daemon for the workspace, creates a worktree
and branch per agent, starts every session and opens the dashboard. Name the
file `crew.yaml` and plain `crew` in that directory does the same.

**Let the orchestrator staff the team.** Leave `agents:` out and the
orchestrator decides who is needed: it adds members with `agent_add`
(each a real session) and creates the tasks. Whoever is on the team when its
first turn with a plan ends is the **founding team** (the event log says
`team.founded`). It is saved to `<workspace>/.crew/roster.yaml` (a complete
team file) and reused by the next `crew run`; `--replan` lets the orchestrator
design the team again. The orchestrator can still remove a founding member
with `agent_remove` when its role is no longer needed.

Members the orchestrator adds after that are **temporary**: extra hands or a
passing need. They are marked `temp` on the dashboard and `(temporary)` in
`agents_list`, and they are not saved with the roster. When a temporary
member's last task finishes or fails, the orchestrator is told that it is
about to leave. Once the orchestrator's turn after that notice has ended,
crew takes the member off the team, unless it was given more work in that
turn. A member with a task still open, a review still to do, or subagents
still working stays on. Its branch is left in place: `integrate` still merges
it and teammates can still `git_merge` it, and a failed task of a member who
has left goes back to the pool when it is retried.

Members you add or remove yourself are always part of the roster:
`crew agent add --name docs --role "writes docs"`, `crew agent remove docs`.

**pi members.** `runtime: pi` puts a member on [pi](https://pi.dev); its
`model` is whatever pi's providers offer, named as pi names it
(`pi --list-models`: `anthropic/claude-opus-4-6`, `openai/gpt-5`, a
`:high` thinking suffix welcome). crew gives the session a small extension
of its own (written next to the agent's identity file, loaded with
`--extension`): it registers crew's MCP server with the session, so the crew
tools are declared to the model like pi's built-in ones; it pushes team
messages in over the daemon socket; it reports the session's life (the
model, each turn and tool call, how full the context is) and compacts the
context when asked. The standing instructions are appended to pi's system
prompt; the session lives under the agent's own directory under an id crew
chooses, so a restart opens exactly that session; and the inspector's
transcript is read from pi's session file. pi runs tools without permission
prompts, so `permissions` does not apply; project-local pi files in the
workspace are trusted with `auto_trust` and ignored otherwise. The model
catalog does not describe pi's models (they depend on the providers set
up), so the orchestrator is told it may add pi members with the model named
as pi names it. There is no `[pi]` endpoint section: pi keeps its own
providers and logins.

**Subagents.** Any member can launch a subagent for a self-contained part of
its task (`agent_spawn`). A subagent is a full agent — its own session,
terminal and worktree branched from its parent's — that reports straight back
to its parent (with commit ids) and is retired when done. Subagents appear in
the dashboard as satellites of their parent and in herdr as panes;
`limits.max_subagents` caps how many run at once. Claude Code's own in-session
subagents are counted too and shown as a `⑂N` badge.

## Agent messaging

Every agent gets a `crew` MCP server (and the same operations as
`crew ctl …` for shell agents):

Each message that reaches an idle session starts a turn, and a turn costs
the agent a pass over its whole context. So crew keeps turns few:

- **Bursts are gathered.** Messages for a resting session are held until 1.5
  seconds after the latest one came (5 seconds at most after the first), then
  go in as one prompt: one turn, not one each. A session mid-turn gets them at
  once, folded into that turn.
- **News waits for company.** A teammate's `status` message (news, not a call
  to act) doesn't start a turn. It goes in with the next message that does,
  such as crew's notice that a task finished. This always applies to the
  orchestrator, and to a resting member unless it said it is waiting, when
  news may be what it waits for. crew's own notices (a task finished, failed
  or stalled, a question, a review) are never held.
- **Nothing around the message.** A message is its `[crew] Message from …`
  header and its body. Claude Code's frame around a plugin's prompt is left
  off (crew's mod submits it as the session's own prompt), and how to answer
  is in the briefing rather than in every message.

Members are also told not to send the orchestrator progress reports, since
crew tells it how their tasks end.

**Agents talk as people would.** Working with crew should feel like
running two sessions side by side and pasting each one's answer into the
other, with crew doing the pasting:

- **Replies come back by themselves.** When a teammate's question or message
  starts a resting agent's turn, the reply that turn ends with is carried
  back to that teammate as an `answer`, with no tool call. If the agent
  already wrote to them with `agent_send` during the turn, nothing more is
  sent.
- **An `@` line starts a conversation.** A line of that reply beginning
  `@<id>:` (`@operator:` for you) and everything up to the next such line
  goes to that agent as a message, and its answer comes back the same way.
  `@db @lead:` sends it to several agents and `@all:` to the whole team
  except you; each gets its own copy and answers on its own.
  Lines inside code fences, e-mail addresses and annotations are text.
- **A reply to an answer goes nowhere,** so a "thanks" doesn't start another
  round. On top of that, crew carries at most 8 replies from one agent to
  another in 10 minutes and logs `message.held` beyond that.
- **Not for these:** tasks (finished with `task_complete`), reviews
  (`review_submit`), and messages that arrive mid-turn, since that turn's
  last reply is about its work. `agent_send` remains for typed messages,
  broadcasts and anything said mid-turn.

**How agents write.** Only the orchestrator writes for a person. Members write
for other models, so what they put on the coordination channel is in the
team's *voice*: messages, `task_update` notes, `task_complete` results, review
comments and `decline` reasons. The orchestrator writes to members in the
voice too, and to you in plain English: questions (`operator_ask`), the final
report and its decisions, and its replies in its own terminal. What lasts for
people is never in the voice: code, comments, commit messages, docs, reports
and other files, and anything a member sends to the operator.

```yaml
defaults:
  voice: ultracave   # ultracave (default) | megacave | caveman | plain
```

| voice | a member's answer to "why does `ProductCard` re-render?" |
| --- | --- |
| `ultracave` (default) | fragments: Inline `style` object, new ref each render, re-render. Hoist to const or `useMemo`. |
| `megacave` | Classical Chinese (文言), code verbatim: `style={{…}}` 每繪新生物件，參照異，故重繪；提為常數或 `useMemo` 則免。 |
| `caveman` | short sentences, no filler |
| `plain` | concise English |

The voices follow [caveman](https://github.com/JuliusBrussee/caveman)'s
skills. By caveman's own measurement, against a plain "Answer concisely."
instruction (output tokens, n=10), megacave writes about 9% less and ultracave
about 35% less. That is why ultracave is the default: megacave mostly saves
characters on screen.

Every agent's briefing is part of its system prompt on every runtime: who it
is, its role, the team, the tools and the voice. It is never a prompt of its
own. So it is said once, cached with the rest of the system prompt, and
still there after a compaction. For members, the voice also takes the place
of each runtime's own guidance on writing for a person, rather than just
sitting beside it:

- **Claude Code:** the briefing goes in with `--append-system-prompt-file`.
  crew's mod replaces the prompt's `tone` section (or adds to the short
  prompt's body) through `prompt.compose`. That's why a Claude member loads
  the mod even on terminal delivery; there it only does this.
- **Codex:** the briefing is the session's `developer_instructions`, after
  any your own Codex config sets. For a member they also override Codex's
  commentary, final-answer and formatting guidance, and
  `model_verbosity="low"` is pinned.
- **pi:** the briefing is appended to its system prompt.

The orchestrator keeps each runtime's own guidance, since you read it.

| Tool | Purpose |
| --- | --- |
| `agents_list` | who is on the team, role, status, task |
| `agent_send(to, body, type?)` | message a teammate directly; `to` may list several (`db,lead`), each getting its own copy, and nothing is sent if one is unknown |
| `agent_broadcast(body)` | message everyone |
| `agent_inbox(id?)` | read (and acknowledge) messages; with an id, that one message, if you sent or received it |
| `tasks_list(done?)` / `task_get(id?)` | the board, with done tasks one line each (`done: true` shows them in full); one task's card (default: your task in progress) |
| `agent_status(status, note, task_id?)` | working / waiting / blocked / idle; name a parked card explicitly |
| `task_update(percent, message)` | report progress |
| `task_complete(result)` / `task_fail(reason)` | finish the current task |
| `git_commit` / `git_diff` / `git_status` / `git_cherry_pick` / `git_merge` | work with your workspace; same tools under jj and git, a jj change id is shown beside the commit id and accepted as a revision |
| `review_request` / `review_submit` | reviews between agents |
| `agent_spawn(task, role?)` | launch a subagent |
| `agent_compact(keep?)` | compact your own context when the turn ends; the orchestrator may name a member (`id`) — see [Context windows](#context-windows) |
| `agent_add` / `agent_remove` / `task_create` / `task_edit` / `task_assign` / `task_retry` / `integrate` | orchestrator only |
| `agent_set_model(id, model, note?)` | orchestrator only: put a member on another model its CLI runs; its session is restarted there, resumed, and reads the note first (see [Usage limits](#usage-limits)). The orchestrator's own model, and moving anyone to another CLI, are refused: they are [the operator's](#changing-model-and-cli) |
| `session_complete(summary, decisions?)` | orchestrator only: the final report; each of `decisions` is put to you as a question (see [Questions for you](#questions-for-you)) |
| `operator_ask(question, options?, asked_by?)` | orchestrator only: put a question to you (see [Questions for you](#questions-for-you)) |

A direct message is persisted, published to subscribers (the dashboard
animates the link immediately) and delivered into the recipient's session. The
orchestrator is not on that path and does not see the body; with
`orchestrator.notify_p2p: true` it gets a one-line note that two agents talked.

**Delivery is over Unix sockets, not typed into terminals:**

- *Claude Code*: crew loads a small mod (`--plugin-dir`) into each session.
  It runs `crew bridge`, which holds a connection to the daemon; a message is
  pushed down that socket and the mod injects it — folded into the running
  turn, or as a new turn when the session is idle — then confirms it.
- *Codex*: each agent gets a private `codex app-server` on a Unix socket and
  its TUI attaches to it (`--remote`); crew injects with
  `codex queue --thread … --message …`.
- *Fallback*: if neither is available (older CLI, the mod refused by policy,
  `defaults.delivery: terminal`), the message is submitted to the agent's
  terminal as a prompt.

Messages have ids; re-sending the same id is a no-op, the sender is stamped by
the daemon from the connection's token, and bodies are size-limited
(`limits.max_message_bytes`). Types: `task`, `message`, `question`, `answer`,
`status`, `review_request`, `review_result`, `artifact`, `error`.

## Task graph

Tasks are first-class: `queued → ready → working → (waiting | blocked) →
(review) → done | failed`, with `depends_on` edges. The scheduler is a pure
function of the graph and agent availability, re-run on every change:

- a task becomes `ready` when all dependencies are `done`, and is dispatched
  to its agent; unassigned tasks go to the least-loaded free worker;
- an agent executes one task at a time; Waiting cards stay parked with their
  owner while independent Ready work can start after the current turn ends;
- a failed dependency blocks its dependents; `crew task retry <id>` requeues;
- a task with a `reviewer` is `done` only after `review_submit(approve=true)`;
- if nothing can make progress for a while, a `scheduler.stall` event is
  raised and the orchestrator is told why; cycles are rejected up front.

`crew task list|show|add|edit|assign|retry|done` reads and edits the graph by
hand.

Calls without a task id address the executing card (Working or a started
Blocked card). If none executes, they select the Waiting card with the latest
`started_at`; equal start times keep board order. A started Blocked card keeps
its execution slot. Use an explicit task id to inspect, update or finish a
parked card during other work. To resume it with `agent_status("working",
task_id: "…")`, finish or park the executing card first.

Before parking source changes, checkpoint them or record which card owns
the pending edits. `git_commit` commits the whole worktree; preserve that
ownership when committing the next card's changes.

Messages still reach the agent during independent work. Teammate or operator
news leaves a durable cue for parked cards, even after the inbox is read;
scoped messages target their card, and unscoped news lets each parked card
inspect it once. Generic scheduler notices and new task/review requests do
not create cues. When capacity returns, cards with cues resume before new
Ready work, latest start first, with links to the original messages. If an
answer is still missing, reporting Waiting again consumes that cue; another
resume requires new news. Parked time does not use the task's timeout, and
parking never completes a dependency or bypasses review approval.

An answer delivered while the owner is at rest resumes its matching parked
card in that delivery, without a second notice. Stopping an owner blocks its
executing card, or its selected Waiting card if nothing executes; its other
Waiting cards remain parked. Restart with a confirmed session requeues the
card blocked by the stop and retains the other parked cards. Fresh or
sessionless restart redispatches every open owned card to the new session.

### A board of cards

The graph works like a kanban board, and work is referred to by id rather
than pasted from session to session:

- **The card is written once.** `task_create` writes it: what is to be done,
  how to tell it is done, and pointers to what already exists (a doc's path
  and §, a task id, a message id, a commit). It doesn't restate them.
- **A dispatch names the card.** Its `task` message is a single line:
  `Task ledger is yours: "implement ledger". task_get("ledger") reads its
  card.` That line is all there is when the card says no more than its title.
- **`task_get` reads the card.** It shows the description, the reviewer,
  and every task it builds on, with that task's result and commits.
  `task_get(id)` reads any other card, and `crew task show <id>` shows one
  to you.
- **Pointers elsewhere follow suit.**
  - A review request points the reviewer at the card, so the review is
    against what the task asked for.
  - After a compaction, or in a fresh session after a move to another CLI,
    the agent is pointed back to its card.
  - `agent_inbox(id)` reads a message someone refers to, if you sent or
    received it. To anyone else that message doesn't exist.
- **A change goes on the card.** `task_edit` (or `crew task edit`) revises
  the card. Whoever is at work on it, or reviewing it, gets a one-line notice
  from the scheduler to read it again. A failed task can be revised before
  `task_retry`; a done one is closed.

## Questions for you

Nobody watches an agent's terminal, so a question asked there would never be
seen. Questions travel over the bus instead:

1. A member that cannot settle something itself asks the **orchestrator**
   (`agent_send`, type `question`). The orchestrator answers what it can from
   the objective, the code and the team's work.
2. What only a human can decide — a preference, a requirement nobody stated,
   a risky or irreversible choice — the orchestrator puts to **you** with
   `operator_ask`: one self-contained question, the answers it sees, and whose
   question it is. A member's attempt to ask you directly is refused and
   pointed at the orchestrator; in a team without an orchestrator, members
   ask you themselves (`agent_send` to `operator`, type `question`).
3. The question stays open until you answer it. The dashboard opens it at
   once, over whatever view you are in, and rings the terminal's bell (so a
   window or pane that is not in front gets flagged; `CREW_BELL=0` turns the
   bell off). If you are in the middle of something — typing a message, typing
   into an agent's terminal — it opens as soon as you are done, and an Enter
   pressed in its first moment is not taken for an answer. It shows who asks,
   for whom, how long ago, the whole text (scrollable), the suggested answers
   and a line for your own words:

   ```text
   ┌ question · orchestrator, for claude-2 · 3m05s ago ─────────────────────────┐
   │ The legacy export writes amounts as integer cents; the new ledger API      │
   │ wants decimal strings. Converting in the adapter keeps every downstream    │
   │ report working but hides the mismatch; changing the export is cleaner and  │
   │ breaks the two finance dashboards until they are updated. Which do you     │
   │ want?                                                                      │
   │                                                                            │
   │ ▸ 1. Convert in the adapter                                                │
   │   2. Change the export and update the dashboards                           │
   │   3. › type your own answer                                                │
   │                                                                            │
   │ ↑↓ choose · enter send · tab next question · pgup/pgdn scroll · esc later  │
   └────────────────────────────────────────────────────────────────────────────┘
   ```

   `esc` puts it off: it stays in the band across the top, and `a` opens it
   again. Without the dashboard: `crew questions` lists them, `crew answer 2` picks
   a suggested answer, `crew answer "in your own words"` writes one (with
   several open, name the question: `crew answer <id> "…"`). `crew status`
   says when something is waiting.
4. Your answer is delivered to the agent that asked, and to the member whose
   question it passed on. Meanwhile the team keeps working on whatever does
   not depend on it.

A finished team can still be waiting for you. What the final report leaves
for you to decide, the orchestrator lists as the `decisions` of
`session_complete`, and each one is asked like any other question — a
decision that is only mentioned in the report would look like a team that
is simply done. Your answers go back to the orchestrator, which acts on
them.

While anything is open the overview says so, not only the band: the
orchestrator's box turns to `? 2 answers needed` with its border in the
warning colour, the box of a member whose question it is shows `?` in place
of its status, and the footer reads `✓ all tasks done · ? 2 answers are
needed`.

Claude Code's built-in ask-the-user dialog (`AskUserQuestion`) is switched off
for crew's agents (a `deny` rule in the session's private settings): it would
hold the agent at a prompt only `crew agent attach` could reach.

## Web dashboard

The same sessions in a browser, in light or dark: the agent graph with its
message and VCS animations (or a list of the team), the bus, the task graph
(or a list), the event log, usage, the inspector (a live terminal included)
and every operator action of the TUI except starting a team. It fits a narrow
window as well as a wide one. **One dashboard server per user shows every
workspace you join to it**, and you switch between them in the page. It runs
on the loopback addresses only (no other machine, no TLS, no accounts), and
works in Chromium-based browsers and Firefox. Safari cannot resolve its address.

```sh
cd ~/work/payments && crew web      # join this workspace, start the server if needed, open the browser
cd ~/work/search   && crew web      # join this one too: same address, the open tab shows it in its switcher
crew web --no-open                  # print the addresses only
crew web --json                     # WebInfo as JSON (scripts): url, port, pair_url, workspace, workspace_url, joined
crew web --port 23417               # exactly this port (default: derived from your runtime directory)
crew web --all                      # join every running session
crew web --list                     # what the dashboard shows, and what runs beside it
crew web --remove                   # take this workspace out of the dashboard
crew web --restart                  # replace the server with this build's, keeping its workspaces
crew web --stop                     # stop the server: every browser session ends, for every workspace
```

`crew web` prints two addresses:

```
web        http://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417/#/w/946d4617aa702870/
open       http://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417/#pair=3f9a…e1&w=946d4617aa702870   (works once, for 2 minutes)
```

**Open the `open` address in your own browser and paste it nowhere else.** Its
`#pair=` code signs one browser in, once; the page removes the code from the
address at once and keeps its session in that browser's `localStorage` (no
cookie, no service worker). Another tab of the same address needs no code. To
sign in another browser, run `crew web` again: it prints the same address with
a new code. `crew status` shows the `web` line while the workspace is joined.

### The page

- **The header** names the workspace and its objective (click to read it
  whole), with counts of agents, those working, tasks complete and open
  questions. Under it are the views: Team, Tasks (`t`), Events (`l`) and
  Usage (`v`), the message bus and Message (`m`).
- **Team and Tasks** each show as a Graph or a List. The choice lasts as long
  as the tab; a window narrower than 640px opens on List. Both keep the
  selection and keyboard navigation when you switch.
- **The side panel** holds the message bus (each row shows who wrote to whom,
  the type and the first two lines of the body) or what you opened: an
  agent's inspector, a task, an event, a message. Drag its edge (or focus it
  and use the arrow keys) to resize it; below 768px wide it takes the place of
  the main area until you close it.
- **The inspector** leads with what the agent is doing, its problems and its
  mailbox. Message and pause are buttons; every other action (prompt,
  restart, restart fresh, model and CLI, compact, jump to pane, open
  terminal, kill) is in its More menu, which a right-click on the agent or
  `shift+F10` also opens.
- **Usage** totals the tokens the runtimes reported and says how many agents
  reported any; an agent that reported nothing shows "not reported", never
  zero.
- **The status bar** has the workspace switcher (`w`), Commands (`ctrl+k`, the
  command line), Appearance, help (`?`) and stop team (`S`, asks first).
  Appearance follows the system's light or dark preference unless you choose
  Light or Dark.

### Several workspaces

- **Joining is explicit.** A workspace is in the dashboard only after
  `crew web` was run in it (or `--all`, or `[web] join = "all"` in your user
  or global `config.toml`; a project's own file is not read for this, so a
  workspace cannot opt itself in). It stays joined for the server's life,
  across restarts of its team. Running sessions you did not join are listed in
  the switcher under "running, not in this dashboard", with the command that
  adds them, and nothing can reach them through the server. That is why the
  default is not "everything": sessions started by scripts, test harnesses or
  an agent that cleared its environment appear in your registry like any other,
  and joining them by default would put them in front of your browser.
- **Switching** is the workspace button in the status bar (`w`, arrows,
  `enter`, `1`–`9`) or a link `#/w/<id>/…`. Each workspace is a connection of
  its own through the server; what the one you leave showed is gone from the
  page, and the page keeps the view it had if you come back.
- **A question** in a workspace that is not on screen shows as a badge on the
  switcher, in the title and as one notice with a way there; its text is shown
  only in its own workspace.
- **A team that stops and starts again** does not end the tab: the workspace is
  shown as not running (what it last showed stays, read-only) and comes back in
  the same tab, with the same session, when its daemon does. This holds for
  `crew stop` and for a killed daemon.
- **`crew web --remove`** takes a workspace out: its connections end, nothing
  the page sends afterwards reaches it, and the page says it was taken out.
  `crew web` in it again brings it back into the same tab.
- **`crew web --restart`** starts a server of this build in place of the old
  one and joins what it held. Open tabs stop (their address is new); no team
  stops.

### What keeps it yours

- The address has a random name made when the server starts and never used
  again, so a page left over from an earlier run (or served by anything that
  later took the port) shares nothing with this one. The server answers only
  to that name: `http://127.0.0.1:<port>/` and `http://localhost:<port>/`
  show nothing (421).
- A page of another origin cannot open the socket. An agent's own token is not
  a web session, and a web session cannot start or stop the server, join a
  workspace or load a team. Sandboxed agents cannot see the server's files.
- The server proves itself to the page with a secret that is not in the
  browser's storage; a tab whose server was killed refuses whoever takes the
  port next and stops.
- Text and terminal output from agents are shown as text, under a strict
  Content-Security-Policy; after it has loaded the page makes no HTTP request
  at all.
- **After the server is restarted (`crew web --restart`, `--stop`, or a crash),
  a tab from before stays stopped by design**: run `crew web` for the new
  address. A restart of a team is different: the tab goes on (above).

The layout (node positions, log filters) is kept per workspace by its daemon;
the dashboard's own preferences (appearance, keys, panel, the last workspace)
by the server. The wire is the daemon's own protocol (version 6) over
WebSockets through the server; `crates/crew/tests/fixtures/web/` has its
reference frames. A team started by an older crew keeps its own dashboard until
it is restarted (restart it after upgrading, then run `crew web`).

### Building the frontend

The built bundle is committed in `crates/crew/web-dist/` and embedded in the
binary, so `cargo install --path crates/crew` needs no node. Only changing the
frontend (`web/`, Solid and Vite) needs [bun](https://bun.sh) (and node):

```sh
just web-build        # bun run build in web/, writes crates/crew/web-dist
just web-test         # vitest
just web-e2e          # Playwright: the mock and perf projects, then the live one
just web-check        # the committed bundle is what the sources build (byte for byte)
just web-install-check # cargo install with node and bun failing; GET / serves the bundle
```

Commit the rebuilt `web-dist/` with every change to `web/`: `cargo test` fails
(`web_bundle`) when the bundle is older than the sources. The browser tests use
`PLAYWRIGHT_EXECUTABLE_PATH` for a Chromium-based browser when Playwright's own
does not start (NixOS). `scripts/web-e2e [jj|git]` runs the live project alone:
real daemons (one or several, sharing one server) with scripted agents that
commit, cherry-pick, merge and ask a question, checked through the page
(pairing, live updates, the VCS animations, operator actions, the terminal,
switching workspaces, daemons that stop and return, `--remove`, `--restart`,
what stays outside, a killed server and a squatter on its port, the
performance budgets).

The running dashboard serves assets embedded in its `crew` binary. To see a
frontend change, rebuild or reinstall `crew`, run that new binary with
`crew web --restart`, and open the fresh pairing address it prints. Tabs from
the previous dashboard server stay stopped; the team daemon keeps running.

For the frontend development loop, start the mock and Vite in separate
terminals:

```sh
cd web && bun install
bun run mock --fixture subagents --scenario busy  # terminal 1
bun run dev                                     # terminal 2
```

Open the development pairing address printed by the mock. Each code works
once for two minutes; Enter prints fresh codes. Without `--scenario`, the
existing fixture and two-workspace development setup are unchanged.
Scenarios layer protocol-v6 data over `--fixture`; generated Rust fixtures
are never rewritten:

| Scenario | Content |
| --- | --- |
| `baseline` | The generated fixture as it is |
| `busy` | Every task status, mixed runtimes, dependencies, replies, artifacts, reported/zero/missing usage, and fresh/stale quotas |
| `attention` | Two operator questions, warnings, blocked/failed agents, and paused usage limits |
| `empty` | An unloaded team with no agents, tasks, messages, events, or quota reports |
| `long-text` | Long objectives, titles, evidence, and Unicode text |
| `limits` | Fallback, reset/retry times, context pressure, and queued compaction |
| `disconnected` | Cached evidence ready for the `disconnect` control |
| `multi` | Running, stopped, incompatible, unreachable, and unjoined workspaces |
| `parked` | One architect with independent Working and two parked Waiting cards; `step` submits independent work for review, then resumes the older card |

Scenario clocks are fixed and activity is off. Type `step` in the mock's
terminal for one controller update; `--activity` opts into timed updates.
Type `disconnect`, `stop`, `remove`, or `shutdown` to exercise availability.
`--reads pending|error|empty|unavailable|truncated|ready` sets panel RPC
responses; typing `ready` releases held reads and restores fixture handlers.
Browser tests use the same `startMock({ scenario, fixture })`,
`advanceScenario`, and `setPanelRead` controller helpers.

Capture the built application, including its production CSP, with:

```sh
bun run build
PLAYWRIGHT_EXECUTABLE_PATH=/path/to/chromium bun run gallery --out /tmp/crew-gallery
```

The output directory must be outside the repository. `index.html` shows
real component screenshots with routes, fixture/scenario names, dimensions,
and reproduction commands; `manifest.json` records contrast, control names,
focus, document overflow, runtime errors, CSP violations, and requests after
load. Gallery files contain no pairing codes, tokens, browser storage, or
real workspace content, and never enter `web-dist`. Capture errors produce
a nonzero exit. `--strict` also fails on active text contrast below its WCAG
threshold, unnamed controls, or document overflow. Disabled text is exempt.
Every viewport, 390px wide and 200% zoom included, fits without horizontal
document overflow; there is no exception. Non-dialog surfaces fail if a
question or other dialog covers them.

Use `--scenario baseline --surface team,tasks,events,usage --theme light,dark
--viewport desktop,narrow` for a short pass. The full matrix includes
1440×960, 1024×768, 390×844, and a 200% desktop equivalent: 720×480 CSS pixels
at device scale 2. Reduced motion and fixed browser time keep captures
repeatable. Inspect screenshots and keyboard behavior manually; the solid
color contrast collector includes SVG node backgrounds but does not evaluate
canvas text, gradients, overlap,
control boundaries, or screen-reader announcements. The browser tests cover
Tab reachability, modal containment, focus return, inert evidence, and panel
RPC states. Give every Playwright run its own output directory:

```sh
PLAYWRIGHT_EXECUTABLE_PATH=/path/to/chromium bunx playwright test --project=mock --output /tmp/crew-web-mock
```

## Native apps

Crew has native applications for Linux (GTK 4 with a VTE terminal), macOS and
iOS (SwiftUI). They show the same sessions as the dashboards: the five
destinations Team, Tasks, Messages, Events and Usage, the inspector with its
evidence and a terminal, questions, messages and the operator actions. They
are native toolkits, not a web view or React Native. The state, histories,
projections, command policy and action planning they use come from the
`crew-client` Rust crate, which the terminal dashboard uses as well; the Apple
apps reach it through the `crew-client-ffi` UniFFI bridge. Closing an app
never stops the team: Stop team is its own confirmed action.

An app connects one of two ways:

- **This computer.** On Linux and macOS, the app finds the running sessions of
  your user (the same registry `crew` uses) and connects as the local
  operator, with the full action set. Nothing is exposed to the network.
- **A remote host,** through the opt-in native gateway: a TLS endpoint on a
  host you choose, with per-device keys you approve at that host's console
  and per-workspace grants. This is the only way the iOS app connects.

### Linux app

Building the GTK app needs GTK 4 and VTE development files; the native code
was verified with Rust 1.93 (the CLI's 1.82 floor is unchanged). On NixOS,
`scripts/native/linux-env.sh` supplies the reviewed toolkit:

```sh
export NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source   # optional pin
scripts/native/linux-build.sh --features gateway   # target/native-validation/cargo/debug/crew-desktop
scripts/native/linux-env.sh target/native-validation/cargo/debug/crew-desktop
```

Without `--features gateway` the app has local connections only. Remote
profiles keep their device key in the desktop keyring (Secret Service); where
none is available the app says so, and offers to keep the key in memory until
it closes, never in a file. **Choose workspace** in the header opens
Connections: pick this computer's workspace and **Connect**, or **Add remote
connection**. Ctrl+1–5 switch destinations, Ctrl+B and Ctrl+I show or hide
the sidebar and the details, F1 lists the shortcuts.
`crates/crew-desktop/README.md` has the desktop file and icon for a local
installation.

### Apple apps

`apps/apple/Crew.xcodeproj` has the `CrewMac` and `CrewIOS` schemes (iOS 17,
macOS 14). They need Xcode, a Swift 6 compiler and the Rust bridge built for
the Apple targets; `apps/apple/CrewApp/README.md` ("Connected application
runner") gives the commands. Device keys stay in the Keychain, which needs a
development-signed build. **These apps have not been built or run here**: Apple
compilation, simulator, device, Keychain and VoiceOver checks wait for an Apple
runner. On Linux only the project files, Swift sources and generated bindings
are checked (see `notes/native-validation.md`).

### Connecting a device to a remote host

The gateway is not in the default build and nothing listens until you start
it. On the host, from an ordinary terminal (never from inside an agent):

```sh
cargo install --path crates/crew --locked --features native-gateway
crew native paths           # the private runtime directory; the TLS key must be a file directly in native_dir
crew native start --listen 192.0.2.10:7443 --endpoint wss://crew-host.example:7443/native/ws \
    --tls-cert /path/to/cert.pem --tls-key "$NATIVE_DIR/key.pem" --name "Work host"
crew native status          # the public host profile and the TLS fingerprint
```

The listen address must be an explicit address and port, and the endpoint a
`wss://host:port/native/ws` address whose certificate is valid for that host.
On the device, **Add remote connection**, paste the output of
`crew native status` as the public profile, and type the fingerprint you read
at the host console: the app trusts the host only when they match, group by
group. Name the device and choose **Request access**. Then, at the host:

```sh
crew native enroll          # opens a 120-second window for one request
crew native pending         # the request: device label, key hash and comparison code
crew native approve <enrollment-id> --code <comparison code> --key-hash <hash> \
    --workspace ~/work/payments [--control] [--terminal]
```

Approve only when the four groups of the comparison code match what the
device shows. Read is always granted; `--control` (messages, answers, agent
and team controls) and `--terminal` (terminal input) are separate. Agents act
with their existing host permissions either way: a workspace grant limits
what Crew lets the device do, it is not an OS or filesystem sandbox. Grants
can change later, and a device can be removed:

```sh
crew native devices                                   # devices, grants and expiry
crew native grant <device-id> --workspace ~/work/payments --control
crew native revoke <device-id> [--workspace <grant>]  # ends its channels; other devices are unaffected
crew native stop                                      # stops the gateway; devices and grants stay
```

**Disconnect** in the app closes its connection and keeps the profile and key;
**Remove from this app** retires the device key locally but does not revoke
the host's grant, so revoke it at the host as well. A normal gateway restart
keeps grants and devices reconnect. If the host loses its private runtime
state (for example after logout or reboot of a tmpfs runtime directory), the
endpoint must be provisioned again and each device approved again with a new
key. The gateway never exposes the browser dashboard, never accepts `crew web`
pairing codes, and the app has no way to approve itself or list other devices.

### Limits

- Apple apps are source-complete but unbuilt and unrun here (see above).
- The Linux app is verified on GTK 4.22.4 and VTE 0.84.1 under X11 with
  private test displays; a real Wayland session, older distribution toolkits,
  screen-reader listening tests and system large-text settings are not yet
  verified. Packaging, signing and store distribution are not provided.
- The gateway serves remote devices only through its own grants; it starts
  no team, joins no workspace and has no remote administration.

## Dashboard controls

| Key | Action |
| --- | --- |
| `tab` / `shift-tab` / arrows | select an agent |
| `enter` | inspect: task, status, live terminal, session transcript, messages, errors, tokens |
| `m` | message the selected agent (as the operator) |
| click the terminal pane | in the inspector: type straight into the agent's terminal — every key goes to it (arrows, ctrl-c, esc included) until `ctrl-]`, a right-click or a click elsewhere; the pane refreshes four times a second meanwhile |
| `f` | in the inspector: the terminal pane fills the view (toggle) |
| `i` | in the inspector: type a line into the agent's terminal in a box (it goes in as a prompt; an empty one presses ⏎) |
| `shift+enter` | a new line in any box (message, answer, terminal). Needs a terminal that speaks the kitty keyboard protocol; elsewhere `alt+enter` or `ctrl+j` |
| `t` | task graph |
| `l` | event log (`f` filters to the selected agent) |
| `p` / `r` / `k` | pause-resume / restart / kill the selected agent |
| `o` | select the orchestrator |
| `O` / `s` | choose the [model and CLI](#changing-model-and-cli) the orchestrator / the selected agent runs on |
| `g` | focus the agent's herdr pane |
| `u` / `U` | continue the agents paused at a [usage limit](#usage-limits) / restart them on a new login |
| `a` | answer the [questions the team has for you](#questions-for-you) |
| `/` | type a [slash command](#slash-commands) |
| `S` | stop the team: every agent and the daemon (asks first) |
| `?` | help |
| `q` | leave the dashboard (agents keep running) |
| click | an agent's box opens its inspector (live terminal, transcript, messages); its bottom border, where it says what it runs on, opens the [model picker](#changing-model-and-cli); a row of the message bus opens that message in full; the question band opens the answer box |
| wheel | scroll the inspector, the logs, the task graph, an open message or question |
| right-click | back / close, like `esc` |

In the inspector the pane on the right is the agent's live terminal, in the
terminal's own colours. Scroll
up (wheel, `↑`, `pgup`) and it becomes the session's transcript: every
prompt the agent was given, what it wrote, and the tools it called, one
line each. That is read from the file the runtime itself keeps of the
session (Claude Code's transcript, Codex's rollout) rather than from the
terminal's scrollback, so it is complete, free of screen furniture and the
same on both backends. It is read when you scroll up and then left alone
while you read (`r` reads it again); scroll back down for the terminal.
`crew agent transcript <id> [-n 60]` prints the same thing. A shell agent
keeps no session file; its pane scrolls the terminal output instead.

The glyph before an agent's name, and the colour of the name, say which CLI
it runs on: an orange `✱` for Claude Code, a teal `❂` for Codex, a gold `π`
for pi, a violet `$` for a shell agent. The same colours mark the agents in the message bus, the
task list and the logs, and the rows of the usage strip. The orchestrator
keeps its blue `◆` whichever CLI it runs on; its CLI's glyph stands before
its model in the hub's bottom border (`└─ ✱ Fable 5.1 · xhigh ─┘`).

Each box leads with the task its member is on (the one it finished last
once it is done, its role while it has had none), and under that what it is
doing right now: the tool it is running, as the runtime reports each call
(`▸ Edit report.md`, `▸ $ cargo test`), with how long it has been on the
task; or its state (`idle 5m`, `✓ finished 2h ago`, a limit or an error in
its colour). That row is there whenever the screen has room for it without
narrowing the boxes, and boxes widen to 30 columns where they can; on a
screen too small for boxes, the one-line list carries the task and what the
member is doing.

Each box says what its agent runs on in its bottom border,
`└ Sonnet 5.5 · high ─┘`: the model (as named in the team, else as the
session reports it) and the reasoning effort. Both come from the runtime
itself — Claude Code states them in its hooks, Codex in its session file — so
the effort appears once the agent has taken its first turn.

The dashboard asks the terminal to report the mouse, so selecting text with
it needs the terminal's override (usually shift-drag). `/mouse` hands the
mouse back to the terminal and takes it again; `CREW_MOUSE=0` starts with it
off. The launcher takes clicks too: a click picks a work style.

### Slash commands

`/` opens the message box as a command line; a line starting with a slash
typed into the message box (`m`) is treated the same way. Such a line is
never sent to an agent: it runs the command, or, when there is no such
command, stays in the box with a note. Tab completes the name. To send text
that really starts with a slash, put a space in front of it (a path such as
`/src/main.rs is slow` is recognised as text anyway).

| Command | Does |
| --- | --- |
| `/exit` (`/quit`, `/q`) | leave the dashboard; the team keeps running |
| `/stop` (`/terminate`, `/shutdown`) | stop the team: every agent and the daemon, after a confirmation — what `crew stop` does |
| `/kill [agent]` | stop one agent's process (asks first) |
| `/restart [agent]` | restart an agent, resuming its session |
| `/pause [agent]` / `/unpause [agent]` | suspend an agent / let it continue |
| `/compact [agent]` | queue a compaction of an agent's [context](#context-windows) |
| `/model [agent]` (`/cli`, `/harness`) | choose the [model and CLI](#changing-model-and-cli) an agent runs on (the orchestrator's when no agent is selected) |
| `/resume` | continue the agents paused at a usage limit |
| `/answer` | answer the questions the team has for you |
| `/tasks` / `/logs` / `/help` | the task graph, the event log, the key reference |
| `/mouse` | turn mouse clicks off (drag then selects text as usual) or back on |

Without a name, the agent commands act on the agent the message box was
addressed to. The launcher's objective box knows `/exit`, `/style <name>` and
`/help`; there, and on the command line, anything that looks like a command
is refused as an objective.

`crew agent attach <id>` puts your terminal on an agent's PTY (`Ctrl-]`
detaches); on the herdr backend it focuses the pane instead.

### Changing model and CLI

`O` opens the picker for the orchestrator, `s` for the selected agent (or
the one in the inspector); so do a click on a box's bottom border and
`/model [agent]`. It has a tab per CLI — Claude Code, Codex, pi (`←`/`→`, or
`1`–`3`) — with the agent's own marked `●`, and under it that CLI's models
from the [catalog](#model-catalog), the one the agent runs on marked `now`.
pi's tab offers pi's own default and every model `pi --list-models` reaches.
A model this machine cannot start (its CLI is not installed, it is not in
the Codex account's list) is shown with why, and is not taken.

```text
┌ orchestrator · model and CLI ──────────────────────────────────────────────┐
│                                                                            │
│  1 ✱ Claude Code ●    2 ❂ Codex    3 π pi                                  │
│                                                                            │
│  ● Fable 5.1   frontier  $10 / $50 per MTok                            now │
│ ▸  Opus 5.5    strong    $4 / $20 per MTok                                 │
│    Sonnet 5.5  balanced  $2 / $10 per MTok                                 │
│    Haiku 4.5   fast      $1 / $5 per MTok                                  │
│                                                                            │
│ For long-running agentic coding and knowledge work. Anthropic's            │
│ recommended starting point for most workloads.                             │
│                                                                            │
│ same CLI: the session is restarted on the model, resumed where it was      │
│ ←→ CLI · ↑↓ model · enter switch · esc cancel                              │
└────────────────────────────────────────────────────────────────────────────┘
```

Enter puts the agent on the highlighted model:

- **Another model of its CLI**: the session is restarted on it and resumed
  where it was — what `agent_set_model` does.
- **Another CLI**: a session cannot move between CLIs (each keeps its own),
  so the agent starts a fresh one there, and its first prompt is a handoff:
  for the orchestrator, every task with its assignee and status, every member
  with what it runs on and is doing, and its predecessor's last three replies
  (read from the old session's transcript); for a member, its working
  directory and branch (uncommitted work included: it is told to read `git
  status` and `git diff` first, or `jj status` and `jj diff` under jj), its task (dispatched to it again) and its
  predecessor's last replies. Runtime-specific settings of the agent
  (permission mode, allowed tools, extra arguments) are dropped; the team's
  defaults apply. The new session id is crew's again on Claude Code and pi.

Either way the choice is kept: in the roster for a member, in the team's
`orchestrator` section (and `.crew/roster.yaml`) for the orchestrator, so a
restarted daemon runs it on the same CLI. The log says what moved where
(`agent.model_set`, `agent.runtime_set`).

**Only you choose what the orchestrator runs on, and only you move anyone
to another CLI.** The daemon refuses both from any agent: the orchestrator
can put a member on another model of the member's own CLI
(`agent_set_model`), and is told to ask you (`operator_ask`) when it needs a
different model itself. From the command line:

```bash
crew agent model orchestrator gpt-6-astra          # to Codex, handed the work
crew agent model orchestrator fable --note "…"      # back on Claude Code
crew agent model design --runtime pi                # pi on its own default model
crew agent model design anthropic/claude-opus-4-6 --runtime pi
```

## Configuration reference

```yaml
project: name                 # required
objective: free text given to every agent
instructions: how this team works; part of every member's briefing
workspace:
  path: .                     # relative to where `crew run` is invoked
  isolation: auto             # auto | worktree | shared
  init: false                 # create the dir and `git init` it if needed
  vcs: auto                   # auto | jj | git (see Workspace isolation)
  base: HEAD                  # revision agent branches start from
backend: auto                 # auto | pty | herdr
herdr:
  layout: tabs                # tabs (one tab per agent) | tiled
  workspace_label: crew:name
defaults:
  runtime: claude             # claude | codex | pi | shell
  model: sonnet
  permissions: bypass         # bypass | guarded
  permission_mode: acceptEdits    # guarded, Claude Code
  sandbox: workspace-write        # guarded, Codex
  allowed_tools: []           # extra Claude Code permission rules
  extra_args: []              # appended to the runtime's command line
  inherit_mcp: false          # also load your own MCP servers into agents
  auto_trust: false           # answer the workspace-trust dialog
  delivery: auto              # auto | socket | terminal
  long_context: true          # start models that have a 1M-token context window in it (Claude and Codex)
  model_fallback: true        # let Claude Code move a session to another model at a limit (the orchestrator is told)
  model_fallbacks:            # while a model's window is used up, crew moves its members to these, and back
    fable: [opus]             # (the built-in styles say this; any catalog model, of either CLI)
  refusal_fallbacks: []       # what takes over a turn a model refuses: catalog models, or pi models (see "Refusal fallbacks")
  voice: ultracave            # how members write to one another: ultracave | megacave | caveman | plain (see "How agents write")
  max_restarts: 1             # restarts after a crash (resuming the session)
  command: []                 # default command for `shell` agents
orchestrator:
  agent: true                 # false: the scheduler alone orchestrates
  runtime: claude
  model: sonnet
  notify_p2p: false
agents:                       # 0–10; omit to let the orchestrator decide
  - id: claude-1              # [a-z][a-z0-9_-]{0,31}
    role: billing
    model: gpt-6.1-sol        # a catalog model also decides the runtime
    runtime: codex            # per-agent overrides: runtime, model, permissions,
    task: "shorthand: a task for this agent"   # permission_mode, allowed_tools,
tasks:                                         # extra_args, command
  - id: ledger
    title: Implement the ledger
    description: …
    agent: claude-3
    depends_on: [db]
    reviewer: claude-2
    timeout_secs: 1800
limits:
  max_message_bytes: 16384
  idle_nudge_secs: 90         # remind an agent that went quiet mid-task
  max_nudges: 3
  unresponsive_secs: 900
  max_subagents: 4
  auto_resume: true           # resume agents paused at a usage limit by themselves
  limit_retry_secs: 1800      # … this often, when the runtime gave no reset time
  context_warn_percent: 75    # tell the orchestrator when a context window is this full (0: never)
```

## Endpoints and keys

By default team members use whatever your own `claude` and `codex` use. To
run the team somewhere else — an API key instead of your subscription, a
gateway, another provider, a local model — give crew its own endpoints and
keys. They apply only to the agents crew launches; your own `claude` and
`codex`, and `~/.claude` / `~/.codex`, are not touched.

Three files are read, each later one overriding the earlier ones field by
field:

| scope   | file                                   | for                              |
|---------|----------------------------------------|----------------------------------|
| global  | `/etc/crew/config.toml`                | every user of the machine        |
| user    | `~/.config/crew/config.toml`           | all your projects                |
| project | `<workspace>/.crew/config.toml`        | this workspace (never committed) |

```toml
[claude]                               # every Claude Code member
base_url = "https://gateway.example.com"
api_key_env = "TEAM_ANTHROPIC_KEY"     # or api_key = "sk-…", or api_key_file = "~/.secrets/anthropic"
# auth_token_env = "GATEWAY_TOKEN"     # instead of a key: sent as "Authorization: Bearer"
model = "claude-sonnet-4-5"            # every member on this endpoint runs this model

[claude.env]                           # extra environment for those sessions
ANTHROPIC_DEFAULT_HAIKU_MODEL = "my-small-model"

[codex]                                # every Codex member
base_url = "https://gateway.example.com/v1"
api_key_env = "TEAM_OPENAI_KEY"
wire_api = "responses"                 # or "chat"

[agents.claude-3]                      # one member and its subagents
base_url = "http://localhost:4000"
api_key = ""                           # "" takes back what an earlier layer set
```

```sh
crew config                  # the three files, what is in effect, and where each value comes from
crew config init             # write a commented template (--scope global | user | project)
```

How it resolves:

- **Layers.** A field set in a later file replaces the same field from an
  earlier one; `env` tables merge by name. A credential is one field however
  it is written (`api_key`, `api_key_env`, `api_key_file`, `auth_token…`), so
  a later layer's key replaces an earlier layer's token and vice versa.
- **Members.** `[agents.<id>]` sections go on top of the `[claude]` /
  `[codex]` section, whatever file they are in: the more specific setting
  wins, then the nearer file. `orchestrator` is an id like any other.
- **When.** The files are read at every agent launch, so an edit or a rotated
  key applies on the next `crew agent restart <id>`. `api_key_env` is read
  from the daemon's environment — the shell you started the team from.
- **Failing closed.** If a file exists but cannot be used (unparsable,
  writable by other users, a key that cannot be resolved), nothing is
  launched on it and the agent shows the reason. It never falls back to your
  own login.

Where the key goes:

- **Claude Code** gets the endpoint through its per-session settings file.
  An `api_key` is not written anywhere: the session fetches it from the
  daemon over the socket (`apiKeyHelper`), authenticated as that agent. An
  `auth_token` has no such mechanism and is written to the agent's private
  settings file (`.crew/agents/<id>/settings.json`, mode 0600). Keys your
  shell exports (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`) are blanked
  for the session so they are not sent along.
- **Codex** gets a model provider of its own (`-c model_providers.crew…`),
  so your Codex login plays no part; the key is passed in the environment of
  the process that calls the model. (With `delivery: terminal` on the herdr backend that
  process is the pane, so the key is part of the pane's environment.)
- Keys never appear on a command line, in the event log, the session
  database or the roster. `crew config` shows them masked.

Two things to know:

- An endpoint without a key is not the same on both runtimes. Claude Code
  sends **your own credentials** to it (useful for a pass-through proxy,
  wrong for anything else — `crew config` points this out); Codex calls it
  without credentials.
- The project file is trusted because you put it there. crew refuses one
  that is tracked by git, since a repository could otherwise redirect your
  keys to its own endpoint, and any config file that other users can write.

### Credentials in a sandbox

With `workspace.sandbox.mode: required` an agent's CLI runs in a private home
(`.crew/agents/<id>/rw/home`), seeded with your model settings only. **Your CLI
login files are not copied** (`~/.claude/.credentials.json`, `~/.codex/auth.json`,
pi's `auth.json`): a CLI that refreshes a copied token could invalidate your own
login. **Credentials in your shell's environment are not enough either**: the
sandbox passes an agent only the variables crew names for it, so an exported
`ANTHROPIC_API_KEY` or `OPENAI_API_KEY` never arrives. Give each runtime its
credential in the endpoint config, which is how crew passes it:

<!-- sandbox-credentials-example -->
```toml
# ~/.config/crew/config.toml  (or .crew/config.toml, untracked)
[claude]
api_key_env = "TEAM_ANTHROPIC_KEY"   # a variable in the shell you start the team from

[codex]
api_key_file = "~/.secrets/openai"   # a file only you can read
```

Claude Code fetches an `api_key` from the daemon (`apiKeyHelper`); Codex gets it
in its environment as `CREW_CODEX_API_KEY`.

**Subscription logins and token files.** When sandboxed, use `setup-token` or
API-key files; subscription login files are not bound. For a subscription, run
`claude setup-token` once, keep the token in a file only you can read
(`chmod 600`, owned by you), and name the variable the CLI reads:

<!-- sandbox-token-files-example -->
```toml
[agents.claude-1.env_files]          # or [claude.env_files] for every Claude member
CLAUDE_CODE_OAUTH_TOKEN = "~/.secrets/claude-oauth"

[codex.env_files]
OPENAI_API_KEY = "~/.secrets/openai"
```

`CLAUDE_CODE_OAUTH_TOKEN` is the variable the installed Claude Code (2.1.288)
reads. For Codex and pi, name the variable that CLI reads for a key (for pi a
provider key such as `ANTHROPIC_API_KEY`); crew does not check that the CLI
acts on it. The file is read at every launch without following links and must
be a regular file of yours with no group or other permissions; its contents go
into that agent's environment only, never into a settings file, the event log,
the roster or `crew config` (which shows the variable and the file). Binding
the CLIs' own login files read-write, so a refreshing subscription login works
in a sandbox, is deferred until the real-CLI run; auth_token_env
(`ANTHROPIC_AUTH_TOKEN`) is a different variable from
`CLAUDE_CODE_OAUTH_TOKEN` and is not a substitute. See `crew config` for what
each agent will use.

## Workspace isolation

`workspace.vcs` chooses the tool: `auto` (the default), `jj` or `git`. `auto`
uses [jj](https://github.com/jj-vcs/jj) 0.44 or newer when it is found and
falls back to git otherwise. crew tells you which is in use at every team
start: the `session.vcs` event (`info` for jj, `warn` for a fallback), the
`crew run` / `crew status` output and the dashboard's opening notice.
Workspaces are provisioned once and stay on their backend: neither tool can
adopt the other's directory.

With `isolation: worktree` (the default when the workspace is the root of a
git repository) each agent works in `.crew/worktrees/<id>` on branch
`agents/<id>`. Agents cannot see each other's uncommitted files. Work moves
explicitly: `git_commit` → send the commit id → `git_cherry_pick` /
`git_merge`. A conflicting cherry-pick or merge is undone, the workspace is
left as it was, and the conflicting files are reported. `crew integrate`
merges every agent branch into `agents/integration` and lists what conflicted.
The tools and the commit ids are the same under both backends.

Under jj:

- In a plain git project the jj repository is `.crew/jj`, sharing the
  project's `.git`, so every commit is an object of your repository and
  `agents/<id>` is an ordinary git branch plain `git` sees. A project that
  already has a `.jj` is used as it is. Your checkout is not touched.
- Each agent directory is a jj workspace with a guard `.git` file, so plain
  `git` run there fails instead of reaching your repository. Agents use the
  `git_*` tools, or `jj status`, `jj diff`, `jj log`.
- jj records its own working-copy snapshots as `refs/jj/keep/*` in your
  `.git`. They show up in `git for-each-ref`; `jj -R .crew/jj util gc` prunes
  them. crew does not run it.
- Sandboxed teams work with jj; the store's top directories are read-only to
  agents.

### Moving a workspace to jj

A workspace that has git worktrees under `.crew/worktrees` stays on git. To
move it (crew never removes an agent's directory):

1. `crew stop`.
2. For each `.crew/worktrees/<id>`: `git -C .crew/worktrees/<id> status`.
   Commit what should be kept; uncommitted files are not carried over.
3. `git worktree remove .crew/worktrees/<id>` for each (`--force` discards
   uncommitted files), then `git worktree prune`. The `agents/<id>` branches
   stay.
4. With jj 0.44+ on `PATH`, start the team again. crew creates `.crew/jj`,
   imports the branches and starts each agent's jj workspace at its branch.

### Back to git

Only for the hidden layout (`.crew/jj`); with a project's own `.jj` the
workspaces are registered in your jj repository and stay. Under jj a commit
alone publishes nothing, so nothing is removed until git holds every agent's
last commit:

1. Let each running agent finish with `git_commit`, then `crew stop`.
2. For every directory `W` under `.crew/worktrees` (`<branch>` is `agents/<id>`,
   and `agents/integration` for `_integration`):
   ```sh
   jj -R W --config 'snapshot.max-new-file-size="1TiB"' status
   jj -R W --config 'snapshot.max-new-file-size="1TiB"' commit -m "<message>"   # only if status lists changes
   jj -R W bookmark set <branch> -r @-
   jj -R W git export
   ```
3. Check that these print the same id, and that `jj -R W status` says the
   working copy has no changes; do not go on while they differ:
   ```sh
   jj -R W log --no-graph -r @- -T 'commit_id ++ "\n"'
   git rev-parse <branch>
   ```
4. Only then remove `.crew/jj` and the directories under `.crew/worktrees`.
5. Set `workspace.vcs: git` (`auto` would choose jj again) and start the team.

### Version control events

Every action leaves a `vcs.<action>` event with the same fields under both
backends: `workspace`, `commit`, `cherry_pick`, `merge`, `integrate`, and
`debug`-level `status` and `diff`. The data holds `action`, `outcome`
(`started`, `ok`, `conflict`, `error`), `op` (shared by an operation's
`started` and its end), `backend`, `agent`, `peer`, `commit`, `change`,
`source`, `branch` and `files`. They replace the earlier `git.*` events. The
dashboard animates each one on the agent's box and on the link to the agent whose
work moved; reads are lighter than writes and a conflict is marked as one.

## Usage limits

Sooner or later an account runs out: the plan's five-hour or weekly window, an
API rate limit, credits. crew treats that as a pause, not a failure.

**What happens.** The agent is parked: it keeps its task, nothing is typed at
it, reminders and the unresponsive check leave it alone, messages for it wait
on the bus, and its task's timeout stops counting. The orchestrator is told
once (so it can move the task to the other model family if the work should
not wait). Every other agent carries on; a limit on the Claude account does
not stop the Codex members.

```
 ✱ claude ⊘ usage limit reached · 2 paused (claude-1, claude-3) · resets in 1h57m · resumes in 1h58m · u continue now · U restart on a new login
 ✱ claude session ██████████████ 100%  ↻ 1h57m    week █████████▏     65%  ↻ 1d2h     Fable week ██████████████ 100%  ↻ 1d2h
 ❂ codex                                          week ███████▉       56%  ↻ 3d10h
```

**The dashboard** shows a usage strip at the top: for each runtime, how much
of each of the account's windows is spent, as a solid bar on a darker track
(amber from 70%, red from 90%) with the share, and how long until it resets
(`↻ 2h14m`, `↻ 1d2h`). The bars form a grid: the
same window sits in the same column for every runtime, and a column that does
not fit beside the others on a narrower screen goes on a further row rather
than off the edge; and, when agents are parked, what happened, who, when it
resets and when crew resumes them. `crew status` prints the same.
The figures are the runtimes' own: Claude Code's rate-limit windows through
the crew mod, Codex's from its session file. They appear after an agent's
first turn, and only for subscription logins.

Claude Code reports the session and the all-models weekly window. A model's
own weekly window (the `Fable week` column) is only in Claude Code's usage
cache, `~/.claude.json`, which Claude Code writes only when it shows usage.
So while the team has Claude members crew has it do that: it runs
`claude -p /usage` (a local command — no model call, so no usage; no session
left behind; no MCP servers started) every 5 minutes while a Claude member
works, every 20 while they all rest, and within a minute of a session falling
back to another model or stopping at a limit. crew reads the cache once a
minute and right after asking, and takes from it whatever is fresher than
what the sessions reported. A figure nobody has reported for twenty minutes
is drawn muted on the strip, and `crew status` says `as of 2h ago` beside it.

**Going on again**

| | |
|---|---|
| the limit resets | crew resumes the agent by itself a minute after the reset time the runtime stated; with no stated time it retries every `limits.limit_retry_secs` (30 min) |
| it lifted sooner, or you bought more | `crew resume`, or `u` in the dashboard |
| you switched account or key | `claude /login` / `codex login` (or edit the key in the crew config), then `crew resume --restart`, or `U`. The sessions are restarted on the new credentials with their conversations resumed |
| the runtime continued by itself, or you typed into the session | noticed; the agent is simply no longer parked |

`crew resume claude-1 codex-2` continues only those. `limits.auto_resume:
false` turns the automatic resume off. A login that stopped working
("sign-in needed") never resumes by itself.

**A session moved to another model.** Claude Code does not always stop at a
limit: when a model's own weekly window is used up (Fable's, say) it moves
the session to another model and goes on (`Switched to Opus 5.5 for this
session · Fable 5.1 requires usage credits`); it does the same when the
model is overloaded or unavailable, or after a refusal. The member then
works on a model nobody chose for its role. crew notices — from the model
each request is made with, which the crew mod reports, or from the
transcript — and:

- marks the member: `↓ Opus 5.5 · xhigh` on its box, a `fallback` line in
  the inspector with Claude Code's own words on it, `fallback design runs on
  Opus 5.5 instead of Fable 5.1 …` in `crew status`, `on Opus 5.5 instead
  of Fable 5.1` in the orchestrator's `agents_list`, and
  `agent.model_fallback` in the log;
- tells the orchestrator what happened, with the accounts' usage, and what
  it can do: leave the member on that model if it will do for the role;
  `agent_set_model(id, model, note)` to put it on a model of its choosing,
  with instructions revised for that model (its session is restarted on the
  new model, resumed where it was, and reads the note first; asking for the
  used-up model again only falls back again); or `agent_add` a member on a
  model that is not affected, the other family say, and `task_assign` the
  work there.

You have the same choice: `crew agent model design sonnet --note "…"`, or
the [picker](#changing-model-and-cli) (`s`), which can also move the member
to the other CLI. The roster keeps it. A member on the runtime's default model is held to the
model its session started on. When a session is back on its model the mark
goes and the orchestrator hears (`agent.model_restored`). `defaults.
model_fallback: false` forbids the substitution instead
(`CLAUDE_CODE_NO_MODEL_FALLBACK`): the turn fails at the limit and the
agent is parked as above.

**Fallback models.** A work style (or team file) can say what its members
go on while their model cannot be used, so that nobody has to decide when
it happens:

```yaml
defaults:
  model_fallbacks:
    fable: [opus]               # Fable's week used up → Opus
    gpt-6-astra: [opus, sonnet] # Codex's window used up → Claude Code
```

A model is *out* while one of the usage windows it draws on is at 100% and
has not reset: its own weekly window (`Fable week`) or its account's
(`session`, `week`) — the figures crew follows for the usage strip. While
it is, crew puts each member on it on the first model of its list that can be
started here and is not out itself:

- when the runtime moves the session by itself (Claude Code's "Fable 5.1
  requires usage credits"): crew leaves it there if that is the fallback, or
  moves it to the one the style names;
- when a turn stops at a usage limit, instead of parking the member until
  the reset;
- between turns, as soon as the usage figures show the model out, before the
  member runs into it;
- when a member is started (added, or the team launched) on a model that is
  out: it starts on the fallback.

A fallback of the same CLI resumes the session on that model (`Your session
was restarted on Opus 5.5: Fable week is used up (it resets Fri 12:00 AM);
the work style puts it on Opus 5.5 until then`); one of the other CLI
starts a fresh session there, [handed the work](#changing-model-and-cli).
The box shows `↓ Opus 5.5`, `crew status` says it goes back by itself, the
log has `agent.model_policy`, and the orchestrator is told, with nothing to
decide. The roster keeps the member's own model. Once a reading since the move
(or the reset time) says the model can be used again, the member goes back:
between turns on the same CLI (its session resumed), between tasks from the
other CLI. From then on it is handed no new task until it is back, and once
it has finished the one it is on it goes when its turn ends (or, if crew
does not see that turn end, once its session has been quiet for a minute).
Choosing a model yourself (`s`, `crew agent model`) or by the
orchestrator (`agent_set_model`) replaces the fallback, and nothing goes back
by itself. It applies to the orchestrator as well: the rule is yours, written in
the style. The built-in styles say `fable: [opus]`, which the launcher shows
under `fallbacks`.

**Refusal fallbacks.** A model can refuse a turn for its content, and then
it refuses the same work every time. Codex refuses with `cyber_policy`
("This content was flagged for possible cybersecurity risk") when its filter
takes security work for an attack: an adversarial review of auth code is
enough. crew recognises a refused turn in three forms, at the end of each
turn:

- **Codex's filters** end the turn in an error, read from its session file
  (Codex does not call its `notify` hook for such a turn).
- **Claude's safeguards** flag the turn. Claude Code first moves the session
  to another model by itself (crew's sessions have `switchModelsOnFlag` on, so
  it never stops to ask). When it cannot, the turn ends in its notice
  ("…'s safeguards flagged this message"), which crew reads from the transcript
  and the `StopFailure` hook.
- **The member says so** with the `decline(reason)` tool, which members are
  told about. This is the only form on pi: its providers turn a refusal into
  the same "unknown error" as any other failure.

Nothing else is a refusal. crew does not judge the wording, and a turn that
ends with nothing to show is not one either. Text in a crew session reaches
nobody, so crew looks at what the turn did: a turn that was given a task, a
question, a review request or a message, and ended without a single tool
call, produced nothing anyone can see. crew asks once more
(`agent.unanswered`): do it, answer with `agent_send`, say what holds you up,
say there is nothing to do, or `decline`. A turn that answers that without a
tool call either (in words, or with nothing at all, as after a message that
needed no answer) had nothing to do (`agent.idle_turn`): the member stays as
it was, its task where it was, and it gets the next message as usual. A
member that just answered in words gets pointed at `agent_send` along the
way. This applies to members only; the orchestrator turning a teammate down is
part of its job.

A style can name models to take such work over:

```yaml
defaults:
  refusal_fallbacks: [glm-5.3, deepseek-v4-flash, opus]
```

The member moves to the first that can be started here, in a fresh session
handed what was refused: the messages its turn was given, with who sent them.
A name the catalog knows runs on its CLI (`opus`: Claude Code). Any other
name is a pi model (see "pi members"): written as pi names it
(`zai/glm-5.3`), or bare (`glm-5.3`) to match it under whichever provider
`pi --list-models` has it. Names pi does not list are passed over, so a
style can list models for several setups.

The member is marked (`↓ GLM 5.3`), the log has `agent.model_policy`, and
the orchestrator is told, with nothing to decide. The roster keeps the
member's own model, and it goes back to that model once the work is done:
when a turn on the other model has ended, nothing has happened for 15
seconds, and it has no task under way. Once a turn of its there has ended or
it has finished a task, it is handed no new task until it is back (and if
crew does not see its turn end, it goes once its session has been quiet for
a minute). A member refused a second time stays
on the other model; move it back with `s` when you want. This never applies
to the orchestrator, whose model is yours to choose. The built-in styles say
`[glm-5.3, deepseek-v4-flash]`, shown in the launcher under `refused`.

Without a refusal fallback that can be started, the member is set aside as
blocked (see Failure handling).

**How it is detected.** Claude Code reports a failed turn through its
`StopFailure` hook, with the kind of error; Codex has no such hook, so the
message on its screen is read ("You've hit your usage limit … try again at
3:19 PM"). Any other API error (overloaded, server error) just ends the turn
and is logged as `agent.turn_failed`; the usual reminder picks the task up
again.

## Context windows

A session that fills its context window is compacted by its runtime anyway —
at whatever moment that happens, and with no say in what the summary keeps.
crew lets the orchestrator do it deliberately instead:

- **Watching.** Every agent's fill is tracked and shown: on the dashboard
  each box has a row with the window's size, a bar and the share used
  (`│ 1M ███▌        35% │`; left out on a screen too short for it),
  `ctx 78%` in `agents_list` and `crew status`, a `context` row in the
  inspector. Claude
  Code reports it through the crew mod after every response; for Codex it is
  read from the session file Codex keeps.
- **Warning.** When a window passes `limits.context_warn_percent` (75) the
  orchestrator is told once, for members and for itself, and decides when a
  compaction fits: between tasks, after a commit, while the agent waits.
- **Queueing.** `agent_compact(id, keep)` queues it; an agent may also queue
  its own. The operator has `crew agent compact <id> [--keep "…"]` and
  `/compact [agent]` in the dashboard. A queued compaction runs when the
  agent's current turn ends, never in the middle of one.
- **Afterwards** the agent is told where it stands: who it is, its open
  task (by id; `task_get` reads the card), what was to be kept, and where to
  re-read the rest (`tasks_list`, `agent_inbox`, `git_diff`). It then
  carries on. Its briefing is in its system prompt, which a compaction keeps,
  so it is not sent again. A compaction the runtime did on its own is noticed
  the same way, and the orchestrator hears of it.

**The 1M-token window.** Models that have one (`long_context = true` in the
[model catalog](#model-catalog): Fable, Opus and Sonnet; GPT-6 Astra, GPT-6.1
Sol and GPT-6 Luna) are started in it. Claude Code gets the model's 1M variant
(`--model opus[1m]`); an account without it refuses every request of such a
session, so crew notices, restarts that agent on the standard window and does
not ask again (`agent.long_context_refused` in the log). Codex gets the two
overrides its team documents for the window, `-c model_context_window=1000000
-c model_auto_compact_token_limit=900000`; Codex itself caps them at what the
account's model allows, and the box on the dashboard shows the window the
session actually has. `defaults.long_context: false` turns the request off
for a team. A model set by the [endpoint config](#endpoints-and-keys) is
passed as written.

crew does not summarise anything itself: the compaction is the runtime's
own. For Claude Code it is the engine's compact call (what `/compact` runs),
made by the crew mod with `keep` as its instructions. For Codex it is
`/compact` submitted at the session's prompt, so Codex's own (remote)
compaction does the work; that command takes no instructions, so `keep`
reaches the agent in the note that follows.

## Failure handling

| Failure | What happens |
| --- | --- |
| agent process crashes | `agent.crashed`; restarted up to `max_restarts` (resuming its session), then `failed` and its task fails |
| daemon killed | the next `crew start`/`run`/`attach` recovers agents, tasks, messages and events from SQLite; herdr panes are re-adopted, PTY agents restarted |
| agent goes quiet mid-task | reminded up to `max_nudges`, then flagged `blocked` (unresponsive) |
| permission prompt / trust dialog | agent shows `blocked` with the reason |
| a menu that needs no answer (Codex's "Giving this request a little extra thought") | dismissed as soon as crew sees it — Enter on "Dismiss and keep waiting", never Esc, which would interrupt the turn — and logged as `agent.notice_dismissed`; the agent is not counted as blocked |
| a Codex turn ends in an error | Codex calls its `notify` hook only for a turn that completed, so crew reads the end from the session file (every 10 s while a turn runs): `agent.turn_failed` with Codex's message, and the agent is at rest again instead of looking busy for good |
| the model refuses the turn (Codex's `cyber_policy`, shown as "This content can't be shown"; Claude's safeguards notice; a `decline` call; or a turn asked twice that still calls no tool, see Refusal fallbacks) | with a refusal fallback in the work style (see Usage limits), the member moves to that model, handed what was refused, and back once that work is done. Without one that can be started, the same work would be refused again, so the agent is `blocked` (`refused`) and nothing re-prompts it; its task is blocked so it can be reassigned. The orchestrator is told why and to hand the work to a member on another model family, or to ask you to move the agent to another CLI (`s` on its box). Whoever sent the refused messages is told not to wait for an answer. `agent.refused` in the event log |
| duplicate message | same id is delivered once |
| task deadlock / stall | `scheduler.stall` event, orchestrator notified |
| version control conflict | operation undone, `vcs.<action>` event with outcome `conflict` and the files |
| task timeout | task fails (`timeout_secs`) |

## Architecture internals

Cargo workspace:

| Crate | Contents |
| --- | --- |
| `crew-protocol` | portable domain types, configuration-free wire records and VCS event payloads |
| `crew-client` | shared state, histories, questions, projections, command policy and codec; caller-supplied clocks and transport outcomes |
| `crew-core` | domain and wire reexports, host requests, blocking client, team config and paths |
| `crew` | `daemon/` (state, server, supervisor, delivery, orchestration, roster, subagents, git ops), `message_bus/`, `persistence/`, `scheduler/`, `tasks/`, `agents/` (Claude, Codex, pi, shell), `process/` (pty, herdr), `vcs/` (jj backend and selection) and `git/` (git backend, shared helpers), `web/` (the browser dashboard's server: HTTP, WebSocket, pairing; `build.rs` embeds `web-dist/`), `mcp/`, `cli/` |
| `crew-tui` | the ratatui dashboard and local host adapter for `crew-client`; terminal rendering, navigation, key coalescing and socket workers |
| `web/` (not a crate) | the browser dashboard's frontend (Solid, Vite, bun); its built bundle is `crates/crew/web-dist/` |

The seams, each a trait with one or two implementations:

- `MessageBus` (`LocalBus`: SQLite + in-process broadcast) — swap the transport here;
- `Store` = `AgentStore + TaskStore + EventStore + MessageStore + …` (`SqliteStore`);
- `ProcessManager` (`PtyManager`, `Herdr`) — where agent processes run;
- `AgentRuntime` (`ClaudeRuntime`, `CodexRuntime`, `PiRuntime`, `ShellRuntime`)
  — how a CLI is launched, how its lifecycle is observed, how messages are
  injected. A new agent CLI is one more implementation of this trait.

Security model: the socket lives in a `0700` directory, is `0600`, and the
peer uid is checked on accept; agents authenticate with a per-agent random
token kept in a `0600` identity file (never on a command line), and the daemon
stamps the sender, so one agent cannot speak as another. All processes are
started from argument arrays — no shell strings — and revisions, ids and
paths that originate from agents are validated. Nothing listens on the
network and there is no telemetry. Agents run as your user: the token scheme
stops accidental and tool-level impersonation, not a process that deliberately
reads another agent's files.

## Development

```sh
just test        # ./scripts/build-env cargo test --workspace   (no agent CLIs needed)
just test-git    # the same, every team on git
just test-jj     # the same, every team on jj (fails without jj; nix supplies it)
just test-all    # test-git, test-jj, test-sandbox, test-sandbox-git
just test-real   # optional end-to-end test with the real Claude Code CLI
just test-terminal # isolated pseudo-terminal signal/shutdown checks (no agent CLIs)
just lint
just dry-run
```

The integration tests (`crates/crew/tests/team.rs`) start a real daemon and
drive real agent processes on PTYs; the agents are `crew scripted-agent`, a
deterministic stand-in that uses the same bus, task and git calls as a real
runtime. `examples/dry-run.yaml` uses it too.

`crates/crew/tests/sandbox_launch.rs` runs the sandbox launch path through
real bubblewrap, and `crates/crew/tests/sandbox.rs` checks what a sandboxed
agent cannot reach (its probes use `socat`). Without bwrap, user namespaces or
Landlock those tests are skipped with the reason printed;
`CREW_TEST_SANDBOX=require` makes them fail instead. `just test-sandbox` runs
the whole suite that way (jj by default, `just test-sandbox git` or
`just test-sandbox-git` for git), from `nix-shell` or wherever bwrap, socat
and jj are installed. `CREW_TEST_VCS=jj|git` picks the backend of every team
the integration tests start; unset, the tests use `auto` and the jj unit
tests skip with a message when jj is missing.

The same holds for the other tests that launch sandboxed agents
(`crates/crew/tests/pty_sandbox.rs`, `git_helper_sandbox.rs`,
`private_home.rs`). Keep `CARGO_TARGET_DIR` out of `/tmp` and `/run`: the
sandbox masks them, so a test binary or crew build under them cannot be
reached from inside and the launch fails ("the crew binary cannot be
bound: ... lies inside the protected /tmp"); `just test-sandbox` refuses
to start there.

## Limitations

- The Claude Code mod API and Codex's app-server are both early-access
  interfaces; if either changes, delivery falls back to the terminal.
- A daemon restart cannot keep PTY-backend agents alive (the PTYs die with
  it); they are restarted with their sessions resumed. herdr panes survive.
- Token usage is shown for Claude Code only (read from its transcript).
- One team per workspace at a time.
