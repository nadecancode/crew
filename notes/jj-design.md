# Design: jj as crew's version control, and the VCS animation

Author: `architect` (task `jj-design`). Design only: no source file was changed.
`notes/` is ignored by `.gitignore`; this file and `notes/jj-argv-check.sh` are
tracked by force (`git add -f`).

**Objective (operator).** crew uses jj (Jujutsu) instead of plain git for all
version control, and the dashboard renders an animation whenever a version
control action runs.

**Operator decisions this design is built on.**
1. git stays as a fallback and jj is the default: jj when it is on PATH, git
   when jj is absent or the team configuration asks for git.
2. In a plain-git project the jj state is hidden under `.crew/` and backed by
   the project's `.git`. The user's checkout never changes: no `.jj` in the
   project root, git `HEAD` and index untouched. Agent work is visible to plain
   git as `agents/<id>` branches.

**Revision 2.** Folds in the operator's decisions relayed by the orchestrator
(m-2a5f16b7c08b) and the reviewer's change requests (m-51055bd076c4):
- read actions (`git_status`, `git_diff`) emit events and animate, lightly
  (D11, §events, §anim);
- operations that can take seconds emit a start event and show as in progress
  until their terminal event (D13, §events, §anim);
- the sticky backend is accepted; crew says at team start which backend runs
  and why, and how to move to jj (D2);
- a timeout is recovered from by what the repository says, and a result that
  landed clean is kept and published, never rolled back (D7, §conflicts);
- a project that is a linked git worktree keeps its admin directory (`HEAD`)
  and its common directory (objects, refs) apart (§layout, §ops, §sandbox).

**Revision 3.** The steps for going back from jj to git (D2) publish the work
before anything is removed: the reviewer found (m-1aead106ab54) that a bare
`jj commit` leaves `agents/<id>` and the git branch on the previous commit.
`notes/jj-argv-check.sh` replays the steps on a workspace with uncommitted
work.

**Revision 4.** D2, after the reviewer's m-8d28c5173465: the back-to-git steps
are for the hidden layout only; the optional removal of `refs/jj/` is gone
(those refs can belong to the user's own jj repository; D12 leaves garbage
collection for later); `_integration` publishes `agents/integration`.

**Revision 5.** §sandbox, after the sandbox member's m-a111740fb9fb: inside a
Landlock domain jj cannot store an object, because its git library renames
across directories and Landlock denies that by default. The shim now allows
it (design-b3.md §3.1, revision 8). The `strace` runs behind §sandbox were
made without Landlock and could not show this.

**Verification.** Every jj invocation below was run against jj 0.44.0
(`nix shell nixpkgs#jujutsu`) in scratch repositories. `notes/jj-argv-check.sh`
replays all of them in throwaway projects and asserts what this design relies
on; it prints `PASS` on jj 0.44.0. Findings that came out of those runs are
marked **(observed)**.

Sections: [§current](#current) · [§decisions](#decisions) · [§layout](#layout) ·
[§ops](#ops) · [§conflicts](#conflicts) · [§sandbox](#sandbox) ·
[§events](#events) · [§anim](#anim) · [§tests](#tests) · [§split](#split)

---

<a id="current"></a>
## §current — what exists today

**Git plumbing, `crates/crew/src/git/`.**
- `mod.rs`: free functions over `git -C <dir>`. `init_repo`, `is_root`,
  `has_commits`, `exclude_state_dir`, `add_worktree` (no checkout, runs on the
  main repository), `populate_worktree` (`reset --hard`, where filters can
  run), `commit_all`, `diff`, `status`, `cherry_pick`, `merge`, `integrate`,
  `merge_agents`, `validate_rev`.
- Two invocation styles: `git()` for work in an agent's worktree, and `git_h()`
  for the main repository (no hooks, no fsmonitor, no system config).
- One funnel, `output()`: own process group, piped output, a deadline
  (300 s, 5 s in tests) after which the group is killed. Timeouts are recovered
  from by `output_recovering` (abort, `reset --hard`, stale `index.lock`).
- `launch.rs`: a thread-local `Launcher` that rewrites each command, so an
  agent's git and the helper work run in a sandbox (`SandboxLauncher`,
  `helper_launcher`).
- `beneath.rs`: reads and creates files under `.git/worktrees/<id>` by
  descriptor, never following a symlink.

**Daemon callers.**
- `daemon/gitops.rs`: `git_commit`, `git_diff`, `git_status`, `git_cherry_pick`,
  `git_merge`, `integrate`. One operation at a time per daemon (`git_lock`), on
  a blocking thread, holding the agent's launch lease. Emits `git.commit`,
  `git.cherry_pick`, `git.merge`, `git.integrate`; `handlers.rs::report_git`
  emits `git.conflict`.
- `daemon/team.rs`: decides isolation (`is_root && has_commits`, `init_repo`
  when `workspace.init`), then `exclude_state_dir`.
- `daemon/supervisor/creation.rs`: `member_worktree` per agent; a subagent's
  base is its parent's branch.
- `daemon/sandbox.rs`: `Layout.git_dir` (must be the directory `<ws>/.git`),
  `provision_worktree`, `integrate`, `LedgerLauncher`, `agent_git_launcher`.
- `sandbox/profile.rs::git(admin)`: `.git` read-only, `objects`, `refs`, `logs`,
  `packed-refs` and the worktree's admin directory writable, and
  `commondir`, `gitdir`, `config.worktree` read-only over it.
- `cli/ops/plain.rs`: `is_blank` (only `.crew`/`.git` present) and
  `workspace_is_isolated`.

**Wire and tools.** `crew_core::rpc::{CommitInfo, GitOutcome, ConflictReport}`,
calls `GitCommit`, `GitDiff`, `GitStatus`, `GitCherryPick`, `GitMerge`, and the
MCP tools `git_commit`, `git_diff`, `git_status`, `git_cherry_pick`,
`git_merge`. A commit is attached to the agent's task as an artifact
(`kind: "commit"`, `value: sha`).

**TUI.** `anim.rs` animates messages only: a `Link` from sender to receiver, a
pulse for 900 ms, a highlight for 3 s. `App::apply_push` adds events to the log
and starts nothing. `App::tick` redraws at frame rate (33 ms) only while a
pulse travels; an idle dashboard wakes once a second.

---

<a id="decisions"></a>
## §decisions

**D1. One seam, two backends.** A trait `Vcs` in a new module
`crates/crew/src/vcs/` is the only thing the daemon calls. `vcs::Jj` is new.
`vcs::Git` delegates to the existing `crate::git` functions, whose behaviour
does not change. The contract of every operation (what it returns, what it
leaves behind) is the one git has today.

**D2. Selection: `workspace.vcs: auto | jj | git`, default `auto`.**
- jj is usable when a binary is found (`CREW_JJ` if set, else the first `jj` on
  the daemon's `PATH`) and `jj --version` prints `jj X.Y.Z` with
  `X.Y.Z >= 0.44.0` **(observed: `jj 0.44.0`)**.
- The backend a workspace was provisioned with is sticky. Agent directories
  persist between sessions and are reused, and neither tool can adopt the
  other's directory in place **(observed: `jj workspace add` refuses a
  non-empty directory)**.

| `workspace.vcs` | `.crew/jj` exists | git worktrees left in `.crew/worktrees` | jj usable | result |
|---|---|---|---|---|
| `auto` | yes | – | yes | jj |
| `auto` | yes | – | no | refuse |
| `auto` | no | yes | – | git, with notice |
| `auto` | no | no | yes | jj |
| `auto` | no | no | no | git, with notice |
| `jj` | – | no | yes | jj |
| `jj` | – | yes | – | refuse |
| `jj` | – | no | no | refuse |
| `git` | no | – | – | git, silent |
| `git` | yes | – | – | refuse |

  A "git worktree left" is a directory under `.crew/worktrees/` with a `.git`
  file and no `.jj` directory.
- **What the user is told, at every team start.** The operator accepted the
  sticky backend on this condition: crew says which backend is in use, why, and
  how to move to jj.
  - `load_team` always emits `session.vcs` when the team has a workspace per
    agent: `info` when jj was chosen, `warn` for a fallback to git. A refusal
    fails `load_team` with its text.
  - `SessionInfo` gains `vcs: String` (`jj`, `git`, or empty when agents share
    the checkout) and `vcs_note: String` (the fallback's text, else empty).
  - `crew run` and plain `crew` print `… isolation worktree, vcs git` in their
    "started" line and `note: <vcs_note>` under it, and hand the note to the
    dashboard as its opening notice (the `notice` argument of `crew_tui::run`,
    which `stale_daemon_note` uses today). `crew status` prints both.
    `session.started` names the backend too.
- The texts:
  - jj chosen: `version control: jj 0.44.0 (state in .crew/jj; agent branches
    are exported to git as agents/<id>)`.
    (native layout: `…(the project's own jj repository; …)`).
  - jj absent: `version control: git, because jj is not on PATH. Install jj 0.44
    or newer and start the team again to use jj; workspace.vcs: git silences
    this.`
  - jj too old: `version control: git, because jj 0.41.0 is older than 0.44.`
  - sticky git: `version control: git, because .crew/worktrees holds git
    worktrees from earlier sessions (<ids>). To move to jj: stop the team,
    commit what is uncommitted there, run git worktree remove
    .crew/worktrees/<id> for each and start again (README, "Moving a workspace
    to jj"). The agents/<id> branches are kept.`
  - sticky jj without jj (refusal): `this workspace is managed with jj
    (.crew/jj) and jj is not on PATH; install jj 0.44 or newer. Going back to
    git needs jj once more, to publish the agents' work (README, "Back to
    git"); removing .crew/jj before that loses what is not on a git branch.`
    In the native layout the text ends after `…or newer.`
  - `vcs: git` on a jj workspace (refusal): `this workspace is managed with jj
    (.crew/jj); workspace.vcs: git would fork its history. To go back to git,
    publish every agent's work first (README, "Back to git").` In the native
    layout the last sentence is left out.
  - `vcs: jj` without jj (refusal): `workspace.vcs is jj, but jj is not on PATH`
    (or `…but jj 0.41.0 is older than 0.44`).
  - `vcs: jj` with git worktrees left (refusal): `workspace.vcs is jj, but
    .crew/worktrees holds git worktrees from earlier sessions (<ids>); remove
    them (git worktree remove) or set workspace.vcs: git.`
- With `isolation: shared` nothing is selected and nothing is said.
- **Moving a workspace from git to jj** (by hand; crew never removes an agent's
  directory). The steps go into the README (qa) **(observed: the agent's jj
  workspace starts from its git branch and the next commit continues it)**:
  1. `crew stop`.
  2. For each `.crew/worktrees/<id>`: `git -C .crew/worktrees/<id> status`.
     Commit what should be kept; uncommitted files are not carried over.
  3. `git worktree remove .crew/worktrees/<id>` for each (`--force` discards
     uncommitted files), then `git worktree prune`. The branches `agents/<id>`
     stay.
  4. With jj 0.44 or newer on PATH, start the team again. crew creates
     `.crew/jj`, imports the branches, and gives each agent a jj workspace at
     its branch.
- **Back to git** (by hand; hidden layout only) **(observed, with a workspace
  that still had uncommitted work)**. Under jj a commit alone publishes
  nothing: `jj commit` moves neither the bookmark `agents/<id>` nor the git
  branch **(observed: both stay on the previous commit)**, and the git backend
  starts from the git branch. So nothing is removed until git has every
  agent's last commit:
  1. If the team is still running, let each agent finish with `git_commit`:
     crew moves and exports the branch. Then `crew stop`.
  2. For each directory under `.crew/worktrees`, save what is still uncommitted
     and publish it. `W` is the directory, `<branch>` its branch:
     `agents/<id>` for `.crew/worktrees/<id>`, and `agents/integration` for
     `.crew/worktrees/_integration` (not `agents/_integration`):
     ```
     jj -R W --config 'snapshot.max-new-file-size="1TiB"' status
     jj -R W --config 'snapshot.max-new-file-size="1TiB"' commit -m "<message>"     only when status lists changes
     jj -R W bookmark set <branch> -r @-
     jj -R W git export
     ```
     Without the `--config`, plain jj leaves new files over 1 MiB out of the
     commit, with a warning **(observed)**.
  3. Verify, for each: the workspace is clean and git's branch is the saved
     commit. These two print the same id:
     ```
     jj -R W log --no-graph -r @- -T 'commit_id ++ "\n"'
     git rev-parse <branch>
     ```
     and `jj -R W status` says `The working copy has no changes`. Do not go on
     while they differ.
  4. Only then remove `.crew/jj` and the directories under `.crew/worktrees`.
  5. Set `workspace.vcs: git` (with jj on PATH, `auto` would choose jj again)
     and start the team. The git backend adds its worktrees on the branches.
  jj's keep refs (`refs/jj/keep/*`) stay in the repository; they are left
  alone (D12).
- **Native layout: these steps do not apply.** There `.crew/jj` is only a
  handle; the agents' workspaces and bookmarks are registered in the project's
  own jj repository (`<ws>/.jj`), which stays. Removing directories would
  leave those registrations behind. Going back to git on such a project is
  not covered by this design.

**D3. Layout (operator answer 2).** In a plain-git project the jj repository
lives in `.crew/jj`, created with `jj git init --git-repo <git>`, so its commits
are objects of the project's `.git`. A project that already has `.jj` in its
root is used as it is. Details in §layout.

**D4. One jj workspace per agent, at today's paths.** `.crew/worktrees/<id>` is
a jj workspace named `<id>`. `Agent.workdir`, `Agent.branch`
(`agents/<id>`), `worktrees_dir()` and the integration directory
(`.crew/worktrees/_integration`, branch `agents/integration`) keep their
values. A jj bookmark `agents/<id>` marks the agent's last commit and is
exported to `refs/heads/agents/<id>` after every move.

**D5. Creating a repository stays `git init`.** `workspace.init` on a blank
directory calls today's `git::init_repo` under either backend, and jj is then
layered on top. jj can only create a git repository together with a `.jj`
beside it, which answer 2 rules out, and a colocated repository leaves the
user's git on a detached `HEAD` after the first jj commit **(observed)**. It is
the one place the jj backend needs the
`git` binary; without it the load fails with `workspace.init needs git to
create the repository`.

**D6. The agent-facing interface does not change.** Tool names (`git_commit`
…), RPC calls, `GitOutcome` and `ConflictReport` stay. `CommitInfo` gains
`change_id: Option<String>`. The id in artifacts, in `CommitInfo.sha`, in
messages between agents and in events is the **commit id**: it is immutable,
it is what plain git shows, and it means the same under both backends. A jj
change id is accepted wherever an agent passes a revision. No protocol bump:
the additions are defaulted fields, and event payloads are free-form JSON.

**D7. Conflicts are rolled back by undoing exactly what the operation did**
(§conflicts), never with `jj op restore`, which would also undo what other
workspaces did in the meantime. Only a commit that no bookmark points at is
ever abandoned. A result that landed without a conflict is never rolled back:
after a timeout it is kept, published and reported.

**D8. A stale working copy is an error, not something crew repairs silently.**
`snapshot.auto-update-stale` is pinned to `false`; `jj workspace update-stale`
runs only after a timeout (§conflicts).

**D9. Plain git must not reach the user's repository from an agent's
directory.** An agent workspace has no `.git`, so `git` run there would walk up
to the project root and act on the user's checkout **(observed)**. Each
workspace gets a guard file `.git` (`gitdir: /nonexistent/crew-jj-workspace`):
git fails there, and jj ignores the file **(observed)**. Agents inspect with
the `git_status`/`git_diff` tools or `jj status`, `jj diff`, `jj log`.

**D10. Sandbox.** The store's top-level directories are read-only to agents and
only the subdirectories jj appends to are writable; jj runs inside
agent-writable directories only in a sandbox, as git does today (§sandbox).

**D11. Events.** Every VCS action emits a `vcs.<action>` event, from both
backends, replacing `git.commit`, `git.cherry_pick`, `git.merge`,
`git.integrate` and `git.conflict`. That includes the read actions
`git_status` and `git_diff` (the objective says every action); their events
are `debug` level and their animation is lighter than a write's (§events,
§anim).

**D12. Not done here.**
- No automatic migration of git worktrees to jj workspaces: D2 keeps such a
  workspace on git and tells the user how to move it.
- No garbage collection. jj keeps a ref `refs/jj/keep/<commit>` in the project's
  `.git` for every commit it creates, working-copy snapshots included; they show
  up in `git log --all` and `git for-each-ref`. `jj -R .crew/jj util gc` prunes
  them; running it for the user is left for later.
- No jj-native tools (`jj_*`) for agents.

**D13. Long operations have a start and an end.** Workspace creation,
cherry-pick, merge and integration emit a `started` event when they begin and
exactly one terminal event when they end, tied together by an operation id.
The dashboard shows them as in progress in between. A terminal event renders
fully on its own, so one that arrives without its start loses nothing. Commit,
status and diff emit only their terminal event (§events, §anim).

---

<a id="layout"></a>
## §layout

### Hidden layout (plain-git project; the default)

```
<ws>/                           the user's checkout: never written
├── .git/                       <git>: the project's repository = jj's backing store
│   ├── objects/                + objects of agent commits and snapshots
│   ├── refs/heads/agents/<id>  exported bookmarks (plain git branches)
│   ├── refs/jj/keep/<commit>   jj's keep-alive refs
│   └── logs/refs/heads/agents/ reflogs of the exported branches
└── .crew/
    ├── .gitignore              "*": keeps .crew out of git status and jj snapshots
    ├── jj/                     the handle: what `-R` points at for repository-level commands
    │   └── .jj/
    │       ├── repo/           the jj repository (operation log, index, change ids)
    │       │   └── store/git_target   -> <git>
    │       └── working_copy/   of a forgotten, empty workspace; no files are checked out
    └── worktrees/
        ├── <id>/               jj workspace "<id>"; the agent's working directory
        │   ├── .git            guard file (D9)
        │   └── .jj/
        │       ├── repo        a file: "../../../jj/.jj/repo"
        │       └── working_copy/
        └── _integration/       jj workspace "_integration", bookmark agents/integration
```

What changes in the user's repository, and nothing else **(observed:
`git status --porcelain`, `.git/HEAD` and `git ls-files -s` are identical
before and after a full session)**:
- objects are added;
- `refs/heads/agents/*` and `refs/jj/keep/*` are created and moved, with their
  reflogs.

The handle is a workspace only so that `-R` has a path. `jj git init` checks
the project out into it; `jj sparse set --clear` removes those files again and
`jj workspace forget default` takes its working-copy commit out of the
repository **(observed: the directory then holds only `.jj`, and repository
commands with `-R .crew/jj` work without warnings)**.

`.crew/.gitignore` replaces `git::exclude_state_dir` for the jj backend: it
needs no git and writes nothing under `.git` **(observed: honoured by git and
by jj)**. The git backend keeps writing `.git/info/exclude`.

### Native layout (the project root has `.jj`)

The user's repository is used as it is: bookmarks `agents/<id>` and the agent
workspaces appear in the user's `jj log` and `jj workspace list`.

- The handle `.crew/jj` is a workspace added from the project root once and
  forgotten at once. Adding it runs one jj command in the user's workspace,
  which snapshots their working copy as any jj command does; that command runs
  with the user's own jj configuration (§ops). Every later command goes through
  the handle or an agent workspace and never touches the user's working copy.
- An agent workspace's `.jj/repo` points to `<ws>/.jj/repo`.
- The backing git directory is what `jj git root` prints: `<ws>/.git` when
  colocated, `<ws>/.jj/repo/store/git` otherwise. `jj git export` writes the
  `agents/<id>` branches there.

### `<admin>` and `<git>`: a project that is a linked git worktree

A git checkout has two directories that are the same one in a main checkout
and differ in a linked worktree (`git worktree add`):

| | holds | main checkout | linked worktree |
|---|---|---|---|
| `<admin>` | this checkout's `HEAD` and index | `<ws>/.git` | what the file `<ws>/.git` names: `gitdir: <main>/.git/worktrees/<name>` |
| `<git>` | objects, refs, `packed-refs`, `config` | `<ws>/.git` | `<admin>/` + the content of `<admin>/commondir`, canonical: `<main>/.git` |

crew resolves both once, in `prepare`, before any agent runs, and keeps them
apart:
- the jj repository is backed by `<git>` (`jj git init --git-repo <git>`), so
  jj's `git_target`, `jj git root`, the exported branches and the sandbox
  mounts all name the directory that has the objects and refs;
- the base `HEAD` is read from `<admin>/HEAD`, so agents start from the linked
  checkout's branch, not from the main checkout's.

**(observed)** With `--git-repo <ws>` on a linked worktree, jj records the
`.git` file and `jj git root` answers `<main>/.git/worktrees/<name>`, which has
no objects or refs: that form is not used. With `--git-repo <git>`: `jj git
root` is `<main>/.git`, a workspace based on the linked `HEAD` holds the linked
branch's files, its commits appear as `agents/<id>` in the shared repository,
and `HEAD`, index and files of both the linked and the main checkout are
unchanged.

Sandboxed teams keep today's rule, stated for the store: `<git>` must lie
inside `<ws>` (`<ws>/.git`, or under `<ws>/.jj` in the native layout). A
linked worktree is refused with today's text (`sandboxed agents need the
workspace to be the main checkout of its repository`): a store outside the
workspace is not mounted into a sandbox.

In the native layout `<git>` is what `jj git root` prints; if that directory
has a `commondir` file the same rule gives the common directory (a jj
repository colocated in a linked git worktree; not exercised here).

### Which layout

`<ws>/.jj` is a directory → native. Otherwise `<ws>/.git` exists (a directory,
or the `gitdir:` file of a linked git worktree) → hidden. Otherwise the
workspace is not a repository root (`isolation: auto` shares it; `worktree`
is refused, as today).

### Colocating instead (the answer 2 that was not chosen)

It would be one more branch at the handle step (`jj git init --colocate` in the
project root, then the native layout). Everything else in this design already
works on a native layout, so the switch stays small if the decision changes.

---

<a id="ops"></a>
## §ops

### Invocation

Every jj process crew starts is built by one function, `jj(dir, identity)`:

```
<program> --no-pager --color never --quiet
  --config signing.behavior=drop
  --config signing.backend=none
  --config fsmonitor.backend=none
  --config ui.editor="true"
  --config ui.diff-formatter=":git"
  --config git.abandon-unreachable-commits=false
  --config snapshot.auto-track="all()"
  --config snapshot.max-new-file-size="1TiB"
  --config snapshot.auto-update-stale=false
  -R <dir> <subcommand …>
```

Each `--config` value is one argv element holding TOML, so strings carry their
quotes (`ui.editor="true"`; a bare `true` is a TOML boolean and fails with
`Invalid type or value for ui.editor` **(observed)**). The block is called `G`
below.

- `<program>`: the absolute, canonical path found at selection (D2). It is what
  the sandbox binds.
- Working directory: `<dir>`. jj prints paths relative to it.
- Environment: `JJ_CONFIG=/dev/null` (no user configuration; jj reports
  `/dev/null` as the user config path **(observed)**), `JJ_USER=<identity>`,
  `JJ_EMAIL=<identity>@crew.local`. stdin is `/dev/null`.
- The funnel is today's `git::output` (made `pub(crate)`, its timeout message
  naming the program): process group, deadline, kill. The installed `Launcher`
  applies as it does for git.
- Output: data is read from stdout only, through `-T` templates. With `--quiet`
  stderr carries only warnings and errors, and is used for error text.

Why each pin:

| pin | reason |
|---|---|
| `--no-pager`, `--color never` | no pager process, no escape codes |
| `signing.behavior=drop`, `signing.backend=none` | commits carry the agent's identity; no gpg, no pinentry |
| `fsmonitor.backend=none` | never start watchman |
| `ui.editor="true"` | nothing can open an editor (every description is passed with `--message=`) |
| `ui.diff-formatter=":git"` | no external diff tool; git-format diffs |
| `git.abandon-unreachable-commits=false` | a `git import` after the user rewrote their history must not abandon, and so rebase, what agents built on **(observed: agent commits keep their parent)** |
| `snapshot.auto-track="all()"` | `git add -A` semantics: new files are part of the next commit |
| `snapshot.max-new-file-size="1TiB"` | the default (1 MiB) leaves larger new files out of the commit with only a warning **(observed)** |
| `snapshot.auto-update-stale=false` | D8 |
| `JJ_CONFIG=/dev/null` | the user's aliases, templates and tools do not apply |

jj runs no hooks and no smudge or clean filters, so there is no jj counterpart
of `core.hooksPath=/dev/null`.

**Identity.** jj takes the author from whoever created the change, and the
working-copy change of a workspace is created by `jj workspace add` and then
by every `jj commit`. All of them run as the agent, so an agent's commits have
author and committer `<id> <id@crew.local>` **(observed; a workspace added as
`crew` gives the first commit the author `crew`)**. A cherry-picked commit
keeps its author and gets the picking agent as committer, as with git.
Integration runs as `crew`.

**Per-repository configuration.** jj 0.44 keeps repository and workspace
configuration under `~/.config/jj/`, keyed by an id file inside `.jj`. crew
never runs `jj config`, so no id file exists and nothing is read or written
under the user's home **(observed)**. A `config.toml` found in `.jj/repo/` is
migrated and applied by the next jj that opens the repository **(observed)**;
§sandbox closes that.

### Revisions an agent passes

Agent text is never passed to jj as a revset. It is matched against these forms
and rewritten:

| input | accepted where | revset passed to jj |
|---|---|---|
| `[0-9a-f]{7,64}` | cherry-pick, diff | `commit_id(<hex>)` |
| `[k-z]{7,32}` (a change id) | cherry-pick, diff | `change_id(<id>)` |
| `agents/<valid id>` | diff, merge (from the agent id) | `"agents/<id>"` |
| `HEAD` | diff | `@-` |
| another name of `[A-Za-z0-9._/-]{1,100}`, not starting with `-`, without `..` | diff, `workspace.base` | `"<name>"` |
| anything else (`main~2`, `HEAD^`, `a|b`, `all()`) | – | refused: `invalid revision '<rev>' (use a commit id, a change id or a branch name)` |

The quoted form is a symbol lookup (bookmark, tag, id), never an expression. A
revset must resolve to exactly one commit; none is `unknown commit`
**(observed: `commit_id(<unknown>)` is an empty set, exit 0, and `jj duplicate`
on it does nothing, so crew resolves first)**.

### Templates

```
INFO  = commit_id ++ "\n" ++ change_id ++ "\n" ++ description.first_line() ++ "\n" ++ self.diff().files().len() ++ "\n"
STATE = commit_id ++ "\n" ++ conflict ++ "\n" ++ empty ++ "\n" ++ local_bookmarks.map(|b| b.name()).join(" ") ++ "\n" ++ conflicted_files.map(|f| f.path()).join("\n") ++ "\n"
PRE   = empty ++ "\n" ++ parents.map(|p| p.commit_id()).join(",") ++ "\n"
```

`INFO` fills `CommitInfo { sha, change_id, summary, files_changed }`; `short`
is the first 8 characters of `sha` and `branch` is `agents/<id>`.
`STATE` describes a commit an operation just made: its id, whether it holds a
conflict, whether it is empty, the bookmarks on it, then one conflicted path
per line, relative to the workspace root.
`PRE` is the first command of every state-changing operation: it snapshots
the working copy and says whether it is clean and what its parent is.

### Operations

`R` is the handle (`<ws>/.crew/jj`), `W` an agent's workspace, and the identity
the command runs as is in brackets. Repository-level commands on `R` pass
`--ignore-working-copy`, except the ones that need a working copy.

**Selection** (no repository).

```
<program> --version
```

**Prepare** (`Vcs::prepare`, at `load_team`; replaces `is_root`, `has_commits`,
`exclude_state_dir`). Writes `.crew/.gitignore`, makes or refreshes the handle,
and says whether there is a commit to start from.

Hidden layout, no `R/.jj` yet [crew]:
```
G git init --git-repo <git> R           (no -R; .crew must exist; <git> as in §layout)
G -R R sparse set --clear
G -R R workspace forget default
```
Hidden layout, every time [crew]:
```
G -R R --ignore-working-copy git import
```
Native layout, no `R/.jj` yet [crew]. This one command keeps the user's jj
configuration (no `JJ_CONFIG`, no snapshot pins), because it snapshots their
working copy:
```
<program> --no-pager --color never --quiet
  --config signing.behavior=drop --config signing.backend=none
  --config fsmonitor.backend=none --config ui.editor="true"
  -R <ws> workspace add --name crew --sparse-patterns empty -r 'root()' R
G -R R --ignore-working-copy workspace forget crew
G -R R --ignore-working-copy git root          → the backing git directory
```

**Base** (`workspace.base`, default `HEAD`) → a commit id.
- `HEAD`, hidden layout: read `<admin>/HEAD` (§layout: the checkout's own
  `HEAD`, which for a linked worktree is not the main checkout's) with
  `beneath::Root::read_text`. `ref: refs/heads/<b>` → the bookmark `<b>`; a
  hex id → that commit.
  ```
  G -R R --ignore-working-copy log --no-graph -r 'present(bookmarks(exact:"<b>"))' -T 'commit_id ++ "\n"'
  ```
  No line: the branch is unborn, there is no commit to start from. The file is
  read because jj 0.44 cannot be asked for git's `HEAD` in a repository that is
  not colocated **(observed: there is no `git_head()` revset)**.
- `HEAD`, native layout: the parent of the user's working copy. More than one
  line is refused (`set workspace.base`).
  ```
  G -R <ws> --ignore-working-copy log --no-graph -r @- -T 'commit_id ++ "\n"'
  ```
- anything else: the revision table above, resolved with the same `log`.
- A subagent's base is `agents/<parent>`, as today.

**Workspace** (`add_workspace`, then `populate`). `add_workspace` runs on `R`
and never inside the new directory; `populate` runs inside it, in the helper
sandbox when the team requires one. That is today's split.

```
G -R R --ignore-working-copy workspace list -T 'name ++ "\n"'                                           [crew]
G -R R --ignore-working-copy log --no-graph -r 'present(bookmarks(exact:"agents/<id>"))' -T 'commit_id ++ "\n"'
G -R R --ignore-working-copy bookmark create agents/<id> -r 'commit_id(<base>)'                         [crew]  if there is no bookmark
G -R R workspace add --name <id> --sparse-patterns empty -r '"agents/<id>"' <ws>/.crew/worktrees/<id>   [<id>]  if there is no workspace
        then the daemon writes W/.git (D9)
G -R W sparse reset                                                                                     [<id>]  populate; always, it is idempotent
G -R R --ignore-working-copy git export                                                                 [crew]
```

- `--sparse-patterns empty` creates the workspace without files; `sparse reset`
  puts them in place **(observed)**. `workspace add` cannot take
  `--ignore-working-copy`: it fails and still registers the workspace
  **(observed)**.
- Reuse: the name is listed and `W/.jj` exists → nothing is added, `populate`
  still runs.
- The name is listed and `W` is gone (deleted by hand; today's `worktree
  prune`): `G -R R --ignore-working-copy workspace forget <id>`, then add.
- `W` exists, is not empty and has no `.jj`: refused, naming the directory.
- The bookmark exists and the workspace does not (a member that left and came
  back): the workspace is added at the bookmark.
- The integration workspace is the same sequence with name `_integration`,
  bookmark `agents/integration` and identity `crew`.

**Commit** (`commit_all(W, id, message)`) [`<id>`].

```
1  G -R W log --no-graph -r @ -T '<PRE>'
2  G -R W commit --message=<message>
3  G -R W bookmark set agents/<id> -r @-
4  G -R W git export
5  G -R W --ignore-working-copy log --no-graph -r @- -T '<INFO> ++ if(self.contained_in("remote_bookmarks(exact:\"agents/<id>\", exact:\"git\")"), "exported", "pending") ++ "\n"'
```

- 1 snapshots the working copy. `true` → `nothing to commit: the working tree
  has no changes`. It also records the parent for the timeout check.
- 2 describes the working-copy change and starts a new empty one, in one
  operation. `--message=` keeps a subject that starts with `-` an argument.
- 3 and 4 are the publish phase: the bookmark follows the new commit and git
  sees it.
- 3 refuses to move the bookmark backwards or sideways **(observed)**, which
  happens only when the agent rearranged its history by hand; the error says
  so and names the commit that was made.
- 4 can fail softly: jj exits 0 and warns when a git ref cannot be written
  **(observed with a leftover `.lock`)**. 5 reports it; `pending` becomes a
  `warn` event and the next export retries. The commit still counts.
- `.crew` is not inside an agent workspace, and the guard file and `.jj` are
  never tracked **(observed)**, so there is no counterpart of the `:(exclude)`
  pathspec.

**Status** (`status(W)`) → `branch <bookmarks>\nHEAD <short> <subject>\n<changes>`.

```
G -R W diff --summary                                   empty → "working tree clean"
G -R W --ignore-working-copy log --no-graph -r @- -T 'bookmarks ++ "\n" ++ commit_id.short(8) ++ " " ++ description.first_line() ++ "\n"'
```

**Diff** (`diff(W, against)`), truncated at 48 KiB as today.

```
no revision:  G -R W diff --summary                      G -R W diff --git
revision:     G -R W diff --stat --from <revset> --to @  G -R W diff --git --from <revset> --to @
```

**Cherry-pick** (`cherry_pick(W, id, rev)`) [`<id>`].

```
1  G -R W log --no-graph -r @ -T '<PRE>'
2  G -R W --ignore-working-copy log --no-graph -r <revset> -T 'commit_id ++ " " ++ author.email() ++ "\n"'
3  G -R W --config <DUP> duplicate 'commit_id(<source>)' --insert-before @
4  G -R W --ignore-working-copy log --no-graph -r @- -T '<STATE>'
5a G -R W abandon @-                                     conflict, or an empty result
5b G -R W bookmark set agents/<id> -r @-   then   G -R W git export   then INFO of @-
```

`<DUP>` is one argv element:
```
templates.duplicate_description='description ++ "\n(cherry picked from commit " ++ commit_id ++ ")\n"'
```

- 1: not `true` → `the worktree has uncommitted changes; commit them before
  cherry-picking`.
- 2: exactly one line, else `unknown commit '<rev>'`. The author's address
  names the agent the work comes from (`<agent>@crew.local`), for §events.
- 3 inserts the copy between the agent's last commit and its working copy, so
  the copy is always `@-`. The template adds git's `-x` trailer.
- 4: `conflict` → roll back with 5a and return a `ConflictReport` (§conflicts).
  `empty` (the change is already there) → 5a and `cherry-pick of <source> is
  empty: its changes are already in agents/<id>`. 5a runs only when `STATE`
  lists no bookmark on the copy (D7).
- 5b is the publish phase. From here on the result stays, whatever happens
  (§conflicts, timeouts).

**Merge** (`merge(W, id, branch)`) [`<id>`; `crew` in the integration workspace].

```
1  G -R W log --no-graph -r @ -T '<PRE>'
2  G -R W --ignore-working-copy log --no-graph -r 'present(bookmarks(exact:"<branch>"))' -T 'commit_id ++ "\n"'
3  G -R W --ignore-working-copy log --no-graph -r '"<branch>" ~ ::@-' -T '"x\n"'
4  G -R W new --no-edit --message=Merge <branch> --insert-after @- --insert-after '"<branch>"' --insert-before @
5  G -R W --ignore-working-copy log --no-graph -r @- -T '<STATE>'
6a G -R W rebase -r @ --onto 'commit_id(<parent from 1>)'   then   G -R W abandon 'commit_id(<merge>)'
6b G -R W bookmark set agents/<id> -r @-   then   G -R W git export   then INFO of @-
```

- 1: clean, and exactly one parent.
- 2: no line → `unknown revision '<branch>'` (integration: the agent is
  skipped).
- 3: no line → the branch is already contained; the outcome is `ok` with the
  current head and the text `Already up to date.`, and nothing is created. jj
  would otherwise create a merge with a redundant parent **(observed)**.
- 4 creates the merge commit with parents (`@-`, `<branch>`) and moves the
  working copy on top of it; the merge is `@-`. As with `git merge --no-ff`
  there is always a merge commit.
- 5: `conflict` → 6a, `ConflictReport`. 6a runs only when `STATE` lists no
  bookmark on the merge commit (D7).
- 6b is the publish phase, as 5b of cherry-pick.

**Integrate** (`integrate`, `merge_agents`). The integration workspace is made
as above at the base. Then for each agent, in the integration workspace, steps
2–6 of merge with identity `crew` and message `Merge agents/<id>`: a missing
bookmark or one that is not ahead is skipped, a conflict is reported and
skipped, anything else moves `agents/integration`. One `git export` at the end.
The result is today's `(Workspace, Vec<IntegrationStep>)`; each step is also
handed to an observer as soon as it is done, so its event is emitted while
the integration is still running (§events, §split).

**Branch exists** (`git_merge` from a member that left):

```
G -R R --ignore-working-copy log --no-graph -r 'present(bookmarks(exact:"agents/<id>"))' -T 'commit_id ++ "\n"'
```

---

<a id="conflicts"></a>
## §conflicts

### The contract, unchanged

A cherry-pick or merge that conflicts returns

```
GitOutcome { ok: false, commit: None,
             conflict: Some(ConflictReport { operation: "cherry-pick" | "merge", files, detail }),
             text: "<operation> aborted because of conflicts; the worktree is unchanged" }
```

and leaves no trace: the files in the workspace, the agent's last commit, its
bookmark, the exported branch and `jj status` are what they were
**(observed for both operations)**. `detail` is written by crew
(`<operation> of <source> conflicts in <n> file(s)`): with `--quiet` jj says
nothing about a conflict.

### How, with jj

jj does not stop at a conflict. It records the conflict in the new commit and
exits 0. So the operation is run, the new commit (`@-`, by construction) is
inspected with `STATE`, and a conflicted result is taken back by undoing what
that one operation did:

- **cherry-pick**: `jj abandon @-`. The copy goes away and the working copy
  returns to its old parent, files included.
- **merge**: `jj rebase -r @ --onto commit_id(<old parent>)`, then
  `jj abandon commit_id(<merge>)`. Abandoning the merge directly would hand both
  of its parents to the working copy. If the second command fails, a merge
  commit without a bookmark is left over: no branch points to it and plain git
  does not see it. It is logged and left.

Two rules keep a rollback from ever taking published work with it:
- **A rollback happens before the publish phase, or not at all.** The bookmark
  moves only after `STATE` said the result is clean, so a commit that is being
  rolled back never had the bookmark on it.
- **A commit with a bookmark on it is never abandoned.** `jj abandon` deletes
  the bookmarks on what it abandons, and the exported git branch goes at the
  next export **(observed, as the reviewer reported: after a published merge
  was abandoned, `agents/<id>` was gone)**. `STATE` lists the bookmarks on the
  commit; if there is one (the agent moved it by hand in between), the rollback
  is not run and the operation fails with `<operation> conflicts, and
  agents/<id> already points at the result; it was left as it is, check jj
  status`.

The precondition stays: the working copy must have no uncommitted changes
(`empty` of `@` after the snapshot), so there is nothing of the agent's to lose
while the working copy moves.

What a rolled-back operation does leave: the abandoned commit as an object
(with jj's keep ref) and entries in jj's operation log. Neither is reachable
from a bookmark, a workspace or a git branch.

Rejected alternatives:
- `jj op restore <operation before>` restores the whole repository view. A
  snapshot another workspace made in between is dropped from the view, and that
  workspace becomes stale.
- `--no-integrate-operation` (try first, integrate only when clean) would leave
  truly nothing, but jj prints the operation id only inside a sentence on
  stderr, and `--quiet` removes it **(observed)**.

### Timeouts

Every command goes through the funnel and can time out: the snapshot, the
mutation, the inspection, the rollback, `bookmark set`, `git export`. What is
known about a jj that was killed:
- jj commits an operation to the repository atomically and updates the working
  copy afterwards, so the command has either done nothing or completed its
  operation **(observed: `SIGKILL` at five points during a commit of 4000
  files gave either "not committed, the changes are still in the working copy"
  or "committed"; the next command worked and no lock was left)**.
- jj's own locks (`working_copy.lock`, `op_heads/heads/lock`,
  `store/extra/lock`) are `flock`s and die with the process; a leftover file
  does not block **(observed)**.
- `git export` can leave git's `refs/heads/agents/<id>.lock`. jj then skips
  that ref with a warning and exit 0 **(observed)**.

Which command timed out does not say what state the repository is in, so
recovery does not depend on it. An operation has three phases,

```
mutate          commit | duplicate | new          (the result is @-)
inspect         STATE of @-
  roll back     abandon | rebase + abandon        only for a conflicted (or, for a pick, empty) result
  publish       bookmark set, git export          only for a clean result
```

and after `TimedOut` from any command of an operation, one procedure runs. It
reads the state and finishes the phase the operation was in. `P` is the parent
that `PRE` recorded before the mutation.

```
T1  G -R W workspace update-stale
T2  G -R W log --no-graph -r @- -T '<STATE>'
T3  remove <git>/refs/heads/agents/<id>.lock        beneath::Root::open(<git>).remove(…); a path the daemon builds
then, by what T2 says of @-:
  it is P                       → nothing landed, or the rollback was complete
        G -R W git export
  not P, conflicted (or an empty pick), no bookmark on it
                                → roll back as above, then
        G -R W git export
  not P, clean                  → the result is kept: publish it
        G -R W bookmark set agents/<id> -r @-
        G -R W git export
  not P, conflicted, a bookmark on it
                                → left as it is (second rule above)
```

- T1 finishes a working-copy update that was cut short, and is a no-op
  otherwise **(observed)**.
- `bookmark set` to where the bookmark already is, and `git export` with
  nothing to export, succeed and change nothing **(observed)**, so the
  procedure can run at any point, and again.
- The export at the end of every branch leaves the git branch equal to the
  bookmark, whichever phase was cut.
- In the integration workspace the bookmark is `agents/integration`.

What the caller is told (an error in every case, as today):

| state found | operation | text |
|---|---|---|
| nothing landed | commit | `jj commit timed out (…); nothing was committed, the changes are still in the working copy` |
| nothing landed, or rolled back | cherry-pick, merge | `jj <operation> timed out (…); the worktree was restored, retry later` |
| kept | commit | `jj commit timed out (…) after the commit was made: <short> is on agents/<id>; check the history` |
| kept | cherry-pick, merge | `jj <operation> timed out (…) after it completed: <short> is on agents/<id>; the result was kept, check the history before retrying` |
| a recovery command failed or timed out | any | `jj <operation> timed out (…) and the worktree could not be restored; check jj status` |

This is git's contract (`output_recovering`): a result that landed is reported
and not undone; anything else is restored. The cases, each run on jj 0.44 by
cutting the operation short and running the procedure
(`notes/jj-argv-check.sh`, "timeouts") **(observed)**:

| cut | found | after recovery |
|---|---|---|
| before the mutation landed | `@-` is `P` | unchanged |
| after a clean merge, before `bookmark set` | clean result | kept; bookmark and git branch on the merge |
| after `bookmark set`, before `git export` (the reviewer's case) | clean result, bookmark on it | kept; the git branch follows; nothing is abandoned |
| after `git export` | the same | kept; unchanged |
| after a conflicted merge | conflicted result, no bookmark | rolled back; bookmark and git branch still on `P` |
| in the rollback of a merge, between `rebase` and `abandon` | `@-` is `P` | unchanged; the merge commit is left without a bookmark |
| after a conflicted pick | conflicted result | rolled back; bookmark still on `P` |
| after a clean pick | clean result | kept and published |

### Stale working copies

A workspace is stale when the commit under its working copy was rewritten from
elsewhere. crew's own operations never cause it: each rewrites only the acting
workspace's own working-copy change and commits it has just created. It
happens when
- an agent runs history-changing jj by hand on commits other workspaces sit on
  (`jj squash --into`, `jj rebase`, `jj describe -r`, `jj abandon`): every
  workspace that descends from the rewritten commit turns stale **(observed)**;
- a jj was killed between committing an operation and updating the files
  (handled above).

Every jj command in a stale workspace fails with `The working copy is stale
(not updated since operation …)` and a hint to run `jj workspace update-stale`
**(observed)**. crew returns that error as it is. It does not update on its
own: when the workspace has uncommitted files, the update moves them into a
divergent commit and takes them off the disk **(observed with
`snapshot.auto-update-stale=true`)**. The agent, who knows what it was doing,
runs `jj workspace update-stale` and looks at `jj status`.

### Concurrency

- Operations crew runs are one at a time per daemon (`git_lock`), as today.
  Workspace creation is outside that lock, as today.
- jj an agent runs itself is not behind that lock, and need not be: jj takes no
  repository lock, and operations made at the same time are reconciled by the
  next command **(observed: two commits started together in two workspaces
  both succeed)**. Within one workspace the working-copy lock orders them.
- The user's own git activity: new commits are seen at the next `git import`
  (prepare). A rewritten history abandons nothing (pin).
- `agents/<id>` moved with plain git while jj also moved it becomes a
  conflicted bookmark at the next import, and operations naming it fail with
  jj's error. Not handled further: nothing in crew moves these branches with
  git.

---

<a id="sandbox"></a>
## §sandbox

### What jj reads and writes

Measured with `strace` on jj 0.44.0, hidden layout. `<git>` is the backing git
directory as §layout defines it (the common directory: objects and refs),
`<repo>` the jj repository (`R/.jj/repo`, or `<ws>/.jj/repo` in the native
layout). `<admin>` (the checkout's own `HEAD`) is read only by the daemon,
for the base, and is not part of any sandbox.

| operation | writes |
|---|---|
| any command in `W` (snapshot) | `W/.jj/working_copy/{working_copy.lock, tree_state, checkout}`; when files changed: `<git>/objects/`, `<git>/refs/jj/keep/`, `<repo>/store/extra/`, `<repo>/op_store/{operations,views}/`, `<repo>/op_heads/heads/`, `<repo>/index/{segments,op_links,changed_paths}/` |
| commit, duplicate, new, rebase, abandon | the same, and files in `W` |
| bookmark set | `<repo>/op_store/`, `<repo>/op_heads/heads/`, `<repo>/index/op_links/` |
| git export | `<git>/refs/heads/agents/<id>` (+ `.lock`), `<git>/logs/refs/heads/agents/<id>`, and the operation as above |
| sparse reset (populate) | files in `W`, `W/.jj/working_copy/` |
| workspace add / forget (daemon) | `<repo>/workspace_store/`, the operation, the new `W` |

Reads: the jj binary and its libraries; `W`; `W/.jj/repo` (a file naming
`<repo>`); `<repo>`; `<git>` (`HEAD`, `config`, `packed-refs`, `refs/`,
`objects/`, `info/exclude`); `/etc/gitconfig`, `~/.gitconfig`,
`$XDG_CONFIG_HOME/git/{config,ignore}` when present; `/dev/null`
(`JJ_CONFIG`); `/etc/localtime`. Nothing is written under `$HOME`.

### Mounts, replacing `.git/worktrees/<id>`

For a worker and for the helper (`W` is the worker's workspace, or the one the
helper works on):

| path | mode | |
|---|---|---|
| `<git>` | read-only | as today |
| `<git>/{objects, refs, logs, packed-refs}` | writable | as today |
| `<repo>` | read-only | `store/git_target`, `store/type`, the `type` files |
| `<repo>/{op_store, op_heads, index, store/extra}` | writable | operations, views, index, change ids |
| `W` | writable | as today |
| `W/.jj` | read-only | holds `repo`, the file that says which repository this is |
| `W/.jj/working_copy` | writable | |
| the jj binary | read-only file | through `Request.cli`; the helper needs it too |

All of jj's work goes through with exactly this **(observed with the top
levels made unwritable: populate, commit, export, cherry-pick, merge, both
rollbacks)**. `<repo>/workspace_store` stays read-only: only `workspace add`
and `forget` write it, and the daemon runs those.

These are built-in writable mounts inside read-only ones, like `objects/`
inside `.git` (design-b3 §3.3, mount order); rule 6 there concerns explicit
`read_write` entries and keeps applying to them.

The orchestrator's read-only checkout masks `.jj` in the project root as it
masks `.git` (native layout).

### Why the top levels are read-only

- **Planted configuration.** jj 0.44 migrates a `config.toml` it finds in
  `.jj/repo/` into the invoking user's configuration directory and applies it
  **(observed)**. Configuration can name programs (`signing.backends.*.program`,
  `ui.diff-formatter`, `fsmonitor`). An agent that could create that file would
  have its program run by the next unsandboxed jj: the daemon's, or the user's
  own. With `<repo>` read-only the file cannot be created. `W/.jj` is read-only
  for the same reason (the workspace-level file, `workspace-config.toml`, is
  assumed to behave the same; it was not exercised).
- **Repointing.** `store/git_target` and `W/.jj/repo` decide which store a
  command works on. They take the place of `commondir`/`gitdir`, which are
  bound read-only today.
- crew's own commands also pin every program-naming key on the command line,
  which outranks repository configuration.

### Where jj runs

- **In a sandbox**, when the team requires one: every command with `-R W`. The
  agent's operations (commit, status, diff, cherry-pick, merge) run in the
  agent's sandbox through `LedgerLauncher`, and `populate` and `merge_agents`
  in the helper sandbox, exactly as git's do. jj runs no hooks or filters
  there; the sandbox is kept for what the working copy can contain and because
  the directory is the agent's to write.
- **In the daemon**: commands with `-R R` (prepare, base, workspace list, add
  and forget, bookmark create, branch-exists, export after add), and the one
  bootstrap command of the native layout. This is today's `git_h` set.
  `add_workspace` registers the workspace and creates its directory with no
  files in it; the daemon then writes the guard file, before any sandbox of
  that agent exists (design-b3 §3.4 D2).
- What the daemon takes from jj's output there: commit ids (hex) and workspace
  names (compared with ids). No path that jj prints decides where the daemon
  writes, except `jj git root` in the native layout, which names the user's own
  repository and is read once, before any agent runs.

### What an agent can write in the shared store, and what is left of the risk

- `<git>/objects`, `refs`, `logs`: unchanged from today. An agent can delete or
  corrupt objects and move or delete any branch, the user's included.
- `<repo>/op_store`, `op_heads`, `index`, `store/extra`: an agent can add
  operations of its own making. That lets it move any bookmark, abandon or
  rewrite any agent's commits, and repoint another workspace's working-copy
  commit.
  - Another agent's workspace then turns stale, which is reported and not
    repaired silently (D8), so files on disk are not lost behind anyone's back.
  - A moved bookmark reaches `refs/heads/agents/*` at the next export.
  - Corrupting these directories makes jj fail for the whole team. In the
    hidden layout the commits stay in the project's `.git`, and deleting
    `.crew/jj` and `.crew/worktrees` starts over.
- New compared with git: git gave each agent a private index and `HEAD`
  (`.git/worktrees/<id>`); jj keeps every workspace's working-copy pointer in
  the shared view. Shared refs and objects are not new.
- Native layout: the store is the user's own jj repository, so the above
  applies to the user's operation log. `jj op log` and `jj op restore` are
  theirs to recover with. A sandboxed team that should not have this reach
  belongs in a plain-git project (hidden layout).
- Plain git in the agent's shell can still reach `<git>` where it is mounted
  (`git --git-dir=…`), as today. The guard file (D9) is against habit, not
  against intent: it lies in `W` and the agent can delete it.

### Landlock and renames (revision 5)

Every sandbox enters a Landlock domain (design-b3.md §3.1). Landlock denies
`rename` and `link` into another directory in every domain unless the
ruleset handles `LANDLOCK_ACCESS_FS_REFER` and a rule allows it; the error
is `EXDEV`.

- jj writes a git object through gix as `<git>/objects/.tmp…` and renames it
  to `<git>/objects/<xx>/<rest>`: across directories. In a sandbox every
  snapshot and commit failed ("Could not turn temporary file into persisted
  file … Invalid cross-device link"). **(observed)** by the sandbox member
  with real bwrap and jj 0.44, and with `notes/landlock-refer-check/`:
  the rename `objects/.tmp1` → `objects/19/abc` is `EXDEV` under the
  sockets-only ruleset.
- git is not affected in the object store (it creates `objects/xx/tmp_obj_…`
  and links within that directory), but agents were: `git mv a d/a` and any
  tool that renames across directories without a copy fallback failed in a
  worktree, under either backend.
- **Decision.** The shim handles REFER and allows it beneath `/`, for every
  role and both backends (design-b3.md §3.1, revision 8, with the reasons).
  Nothing in this section's mounts changes: what an agent can write is still
  the table above, and the kernel still refuses a move from one bind to
  another, so a file cannot leave the workspace for the store or the other
  way round. Not a `Spec` field: there is nothing to configure.
- jj's own files (operation store, index, `working_copy`, `store/extra`, ref
  locks) are written by a temporary file in the same directory and need no
  REFER; the sandbox member's `EXDEV` search over the jj end-to-end run is the
  check.

Tests (member sandbox, commit 834c915c): in a sandbox, jj commit, status,
diff, a conflicting and a clean cherry-pick, a merge and the helper's
integration pass; in a worktree `rename` and `link` across directories and
`git mv` pass, under jj and git; a move from the workspace into the object
store or `.jj`, inside a read-only mount, or of a socket into the sidecar
directory is refused.

### `beneath.rs`

| today | with jj |
|---|---|
| find the admin directory by its `gitdir` file | none: `add_workspace` created `W` and knows it; registration lives in the operation store |
| read the worktree's `HEAD` | `Root::open(<admin>).read_text("HEAD")` for the base (hidden layout); `<admin>/commondir` the same way, for `<git>` |
| `create_empty(config.worktree)` | the guard file: a new `Root::write_new(rel, bytes)` (`O_CREAT \| O_EXCL \| O_NOFOLLOW`) on `Root::open(W)` |
| `remove(index.lock)` after a timeout | `Root::open(<git>).remove("refs/heads/agents/<id>.lock")` |

`Launcher::admin_dir()` stays for git; a launcher for jj returns `None`.

### Changes for the sandbox code

- `vcs::Store` (§split) describes the store: `git_dir` is `<git>`, never
  `<admin>`. `profile::Session.git_dir` becomes
  `store: &Store`; `Role::{Worker, Helper}.admin` becomes `Option<&Path>`
  (`Some` only for git). `profile.rs::git(admin)` becomes a function that adds
  the table above for `Store::Jj` and what it adds today for `Store::Git`.
- `daemon/sandbox.rs`: `Layout` takes its paths from `Vcs::store`; the
  "`.git` must be a directory" check becomes: `<git>` and `<repo>` are
  directories inside `<ws>` (§layout: a linked git worktree is refused for a
  sandboxed team, as today).
  `Run::git` becomes `Run::vcs(agent, agent_dir, program)`.
  `provision_worktree` and `integrate` call the trait
  (`add_workspace`/`populate`/`merge_agents`). The helper request passes the
  program as `cli`.
- Nothing new in the environment: `JJ_CONFIG`, `JJ_USER`, `JJ_EMAIL` reach
  `spec.env` from the command, as the `GIT_*` variables do.

---

<a id="events"></a>
## §events

Every VCS action emits events through `Daemon::emit` (so they are stored and
pushed as `Push::Event`), whichever backend ran. `Event.data` is a
`crew_core::vcs::VcsEvent`:

```rust
// crates/crew-core/src/vcs.rs
pub const VCS_SOURCE: &str = "vcs";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VcsAction { Workspace, Commit, CherryPick, Merge, Integrate, Status, Diff }

impl VcsAction {
    /// Status and diff: they change nothing.
    pub fn is_read(self) -> bool;
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VcsOutcome { Started, Ok, Conflict, Error }

impl VcsOutcome {
    /// Everything but `Started`: the operation is over.
    pub fn is_terminal(self) -> bool;
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VcsEvent {
    pub action: VcsAction,
    pub outcome: VcsOutcome,
    /// The operation this event belongs to: on every event, one id per
    /// operation. A `started` event and the terminal event of the same
    /// operation carry the same id.
    pub op: String,
    /// "jj" or "git".
    pub backend: String,
    /// Who acted: an agent id, or "integration".
    pub agent: String,
    /// Whose work moved: the author of a picked commit, the agent merged, a
    /// subagent's parent. None when nobody else is involved or it is unknown.
    #[serde(default)]
    pub peer: Option<String>,
    /// The commit that resulted (full commit id): the new commit, the applied
    /// copy, the merge commit; for a workspace, the commit it starts from.
    #[serde(default)]
    pub commit: Option<String>,
    /// jj's change id of `commit`. None under git.
    #[serde(default)]
    pub change: Option<String>,
    /// What was brought in or looked at: the picked commit's id (on a
    /// `started` event: the revision as the agent gave it), the merged branch,
    /// the revision a diff is against.
    #[serde(default)]
    pub source: Option<String>,
    /// The branch that was created or moved.
    #[serde(default)]
    pub branch: Option<String>,
    /// Conflicted files (outcome conflict).
    #[serde(default)]
    pub files: Vec<String>,
    /// The commit's subject; an error's text; the integration's tally.
    #[serde(default)]
    pub summary: Option<String>,
}

impl VcsEvent {
    /// "vcs.commit", "vcs.cherry_pick", …
    pub fn event_type(&self) -> String;
    /// The payload of a VCS event; None for any other event, or for a payload
    /// this build does not know (an action or outcome added later).
    pub fn from_event(event: &Event) -> Option<Self>;
}
```

Envelope: `source: "vcs"`, `event_type: "vcs.<action>"`, `agent_id`: the acting
agent when it is a team member (none for integration), `task_id`: the agent's
active task for a commit (as today).

| outcome | level |
|---|---|
| `started` | `debug` |
| `ok` | `info`; `debug` for status and diff |
| `conflict`, `error` | `warn` |

### Which events

| event_type | action | `started` | terminal outcomes | `agent` | `peer` |
|---|---|---|---|---|---|
| `vcs.workspace` | a member's workspace is created (not when reused) | yes | `ok`, `error` | the member | the parent, for a subagent |
| `vcs.commit` | `git_commit` | no | `ok`, `error` | the agent | – |
| `vcs.cherry_pick` | `git_cherry_pick` | yes | `ok`, `conflict`, `error` | the agent | the source commit's author when its address is `<agent>@crew.local`; unknown at the start |
| `vcs.merge` | `git_merge` | yes | `ok`, `conflict`, `error` | the agent | `from_agent` |
| `vcs.integrate` | the integration run | yes | `ok`; `conflict` when any branch conflicted; `error` | `integration` | – |
| `vcs.integrate` | one branch of the run, as it finishes, in order | no | `ok`, `conflict` | `integration` | the agent merged |
| `vcs.status` | `git_status` | no | `ok`, `error` | the agent | – |
| `vcs.diff` | `git_diff` | no | `ok`, `error` | the agent | – |

The operator's decision named workspace creation, integration and merge as the
operations that can run for seconds. Cherry-pick is in the same class (the same
number of commands as a merge, each of which starts a sandbox in a sandboxed
team) and gets a start as well. A consumer treats `started` the same way for
any action, so giving commit a start later needs no change to the contract.

### Rules

1. **One operation, one `op`.** `v<n>`, from a counter of the daemon: unique
   while the daemon lives. Every event carries one.
2. **`started` is emitted when the operation begins to run**, on the thread
   that runs it: after it has waited for its turn (`git_lock`), not when it was
   asked for.
3. **Every `started` is followed by exactly one terminal event** with the same
   `op`, `action` and `agent`, for as long as the daemon lives. The operation
   holds a guard on its own thread whose `Drop` emits the terminal event if
   nothing else did (`error`, summary `interrupted`): an early return, a panic,
   and a request whose client went away all end in a terminal event. Events are
   emitted from the job, not from the request: today's `git.*` events are
   emitted after the request's `await` and are lost when the client disconnects
   while the job runs on.
4. **A terminal event stands on its own.** It carries every field its action
   has, whether or not a `started` came before, so a consumer that missed the
   start (or an operation that has none) renders the same.
5. **Workspace.** `started` is emitted before the workspace is made when its
   directory does not exist yet; a terminal event is emitted when the workspace
   was created (`Workspace.created`) or a `started` went out. A workspace that
   is reused emits nothing. The integration workspace has no event of its
   own: making it is part of the integration run.
6. **Integration.** `started`; then one event per branch that was merged or
   conflicted, each with its own `op`, emitted as that branch finishes (the
   backend calls an observer per step, §split); then the run's terminal event
   with the start's `op` and the tally: `summary` `2 merged, 1 conflict`,
   `branch` `agents/integration`, `commit` its head. Branches that are missing
   or not ahead emit nothing.
7. **`error`** is emitted when the backend ran and failed (nothing to commit,
   uncommitted changes, unknown commit, a timeout, a stale workspace). A
   request refused before any VCS ran (shared checkout, unknown agent, invalid
   revision, no sandbox) emits nothing, not even a start. A timeout that kept
   its result (§conflicts) is an `error` whose `commit` names the result.
8. **Reads** emit one event after the command ran. They are not rate-limited:
   an agent that polls `git_status` produces one `debug` event per call.
9. A consumer ignores what it does not know: `from_event` returns `None` for an
   unknown action or outcome, and the event is still an ordinary log line.

### In the daemon

One helper in `daemon/gitops.rs` carries the rules, so no caller emits by hand:

```rust
/// One VCS operation: its events, emitted on the thread that runs it.
pub(crate) struct VcsOp { /* daemon, the event so far (action, backend, agent, op), task, state */ }

impl VcsOp {
    pub fn new(daemon: &Daemon, action: VcsAction, agent: &str) -> Self;   // takes the next `op`
    pub fn peer(self, peer: Option<&str>) -> Self;
    pub fn source(self, source: &str) -> Self;
    pub fn branch(self, branch: &str) -> Self;
    /// Emit `started` (rule 2).
    pub fn start(&mut self);
    /// Emit the terminal event; `fill` adds what the operation found out.
    pub fn finish(self, outcome: VcsOutcome, fill: impl FnOnce(&mut VcsEvent));
}
impl Drop for VcsOp { /* rule 3 */ }
```

- Agent operations build the `VcsOp` in the request and move it into the
  closure given to `agent_git`; `start` and `finish` run inside the closure.
- Workspace: `supervisor/creation.rs`, around `member_worktree` (already on a
  blocking thread).
- Integration: inside the closure given to `Daemon::git`; the per-branch
  events are emitted by the observer passed to `Daemon::integration`.
- `Daemon::emit` is called from blocking threads today (`create_agent`).

### Examples

`vcs.merge`, started, then its terminal event:

```json
{ "action": "merge", "outcome": "started", "op": "v17", "backend": "jj",
  "agent": "ledger", "peer": "db", "source": "agents/db", "branch": "agents/ledger" }
{ "action": "merge", "outcome": "ok", "op": "v17", "backend": "jj",
  "agent": "ledger", "peer": "db", "source": "agents/db", "branch": "agents/ledger",
  "commit": "5e6f7a8b…", "change": "kzvsvsvn…", "summary": "Merge agents/db" }
```

`vcs.cherry_pick`, conflict:

```json
{ "action": "cherry_pick", "outcome": "conflict", "op": "v18", "backend": "jj",
  "agent": "ledger", "peer": "rival",
  "source": "d082aaff1b40f2501fb419c7756171bd4c485d57",
  "branch": "agents/ledger", "files": ["schema.sql"] }
```

Messages (the log view shows them):

```
creating the workspace of ledger…                                              started
workspace for ledger on agents/ledger at 1a2b3c4d
db committed 1a2b3c4d on agents/db: Add schema
ledger: cherry-pick of 1a2b3c4d…                                               started
ledger cherry-picked 1a2b3c4d from db as 5e6f7a8b
ledger: merging agents/db…                                                     started
ledger merged agents/db (5e6f7a8b)
ledger: cherry-pick of d082aaff conflicts with rival in schema.sql; nothing was changed
integrating into agents/integration…                                           started
integration merged agents/db (9c0d1e2f)
integration: merge of agents/rival conflicts in schema.sql; skipped
integration on agents/integration: 2 merged, 1 conflict
db: commit failed: nothing to commit: the working tree has no changes
db: status
db: diff against agents/ledger
```

Replaced: `git.commit`, `git.cherry_pick`, `git.merge`, `git.integrate`,
`git.conflict` and `handlers.rs::report_git`. A conflict is the `conflict`
outcome of its action, not a second event.

To give `peer` for a cherry-pick, the backend returns who the source is:

```rust
pub struct Picked { pub outcome: GitOutcome, pub source: String, pub author_email: String }
```

(git: `rev-parse` and `log -1 --format=%ae`, which `cherry_pick` already has the
means for.)

---

<a id="anim"></a>
## §anim

The TUI animates from `vcs.*` events only (`VcsEvent::from_event`). Every VCS
action shows: a write with a link and a mark, a long one as in progress until
it ends, a read with a glance.

### The pieces

All in `Anims`:
- a **link**: today's `Link`, with a new kind `LinkKind::Vcs(Tone)`. A pulse
  travels from `from` to `to` in `PULSE_MS` (900 ms) and the link stays lit
  until `LINK_MS` (3 s), as for messages. New: a link can be **held** by a
  running operation. A held link is lit, dim, with no pulse and no expiry;
  when the operation ends the pulse travels and the 3 s start.
- a **mark** on one box, of three kinds:
  - **done** (a write ended): a bright segment runs once around the box's
    border in `MARK_SWEEP_MS` (700 ms), and a label sits on the bottom border
    until `MARK_MS` (2400 ms). Bold, in the tone's colour.
  - **running** (a long operation began): the spinner and a label on the
    bottom border, until the operation's terminal event. No sweep.
  - **glance** (a read): a short segment runs once along the bottom border
    only, in `GLANCE_SWEEP_MS` (300 ms), and a label sits there until
    `GLANCE_MS` (900 ms). Not bold, in `theme.muted`: no tone colour, no link,
    about a third of a write's time.

`Tone` is `Ok` (`theme.ok`), `Conflict` (`theme.err`) or `Error`
(`theme.warn`). No new theme colours.

### What each event shows

`started` events (D13):

| event | running mark | held link |
|---|---|---|
| `workspace` | on the parent's box when there is a `peer`, else on the hub: `<spinner> workspace <agent>` (the member has no box yet) | – |
| `cherry_pick` | agent: `<spinner> cherry-pick` | – (the peer is not known yet) |
| `merge` | agent: `<spinner> merge <peer>` | peer → agent |
| `integrate` | hub: `<spinner> integrating` | – |

Terminal events. Each row is complete without a `started` before it:

| event | link | mark |
|---|---|---|
| `workspace` ok | parent → member, `Ok`, when there is a `peer` | member: done, `Ok`, label `+ agents/<id>`, starts when the pulse arrives |
| `commit` ok | – | agent: done, `Ok`, label `◆ <short>` |
| `cherry_pick` ok | peer → agent, `Ok` | agent: done, `Ok`, label `◆ <short>`, on arrival |
| `merge` ok | peer → agent, `Ok` | agent: done, `Ok`, label `◆ <short>`, on arrival |
| `cherry_pick`/`merge` conflict | peer → agent, `Conflict` | agent: done, `Conflict`, label `✗ <n> file(s)`, on arrival |
| `integrate` ok, with peer | peer → hub, `Ok` | – |
| `integrate` conflict, with peer | peer → hub, `Conflict` | – |
| `integrate` without peer (the tally) | – | hub: done, `Ok` or `Conflict`, label = `summary` |
| `status`, `diff` ok | – | agent: glance, label `· status` / `· diff` |
| `status`, `diff` error | – | agent: glance, label `! status` / `! diff` in `theme.warn` |
| any other `error` | – | agent: done, `Error`, label `! <action>`. `workspace`: on the box where it shows as running (parent or hub), label `! workspace <agent>`; `integrate`: the hub |

- `<short>` is the first 8 characters of `commit`.
- Endpoints: `integration` has no box and maps to the hub, as the operator does
  in `endpoint()`. A link with no `peer`, or whose peer equals the agent, is not
  started; the mark then starts at once.
- "On arrival": the mark's `started` is `now + PULSE_MS` when a link was
  started for the same event. A mark that has not started is not drawn.
- The pulse of a VCS link is drawn as `◆` with a `◇` behind it (messages keep
  `●`/`•`), in the tone's colour, along `layout::route` with the same obstacle
  rule as `P2p`. Hub links run along the spoke like `Orchestrator`. A held link
  is drawn in `theme.muted`.
- A VCS link replaces a live link between the same pair (today's
  `Anims::start` rule). `p2p_live` counts only `P2p`, so the "private channel"
  banner is not triggered.
- Label placement: on the bottom border, left; it yields to nothing while live
  (the `+N` badge returns after it). Cut with `fit` to the box width.
- Compact layout (`render_compact`): no links; a live mark's label (spinner
  included) replaces the agent's activity text.
- An agent that has no box (not on the dashboard yet, or gone): its link and
  mark are skipped by the renderer (`rect_of` returns none), as for messages.
  When the box appears while the mark is live, the mark shows.

### Start and end

- A terminal event ends the running mark and releases the held link that carry
  its `op`, then starts what its row says. The held link becomes that row's
  link (same pair), so the pulse runs along the line that was already lit.
- A terminal event whose `op` matches nothing running is rendered by its row
  alone (rule 4 of §events).
- Running state without an end is bounded three ways: the agent's box goes; a
  snapshot is applied (below); and a running mark older than `RUNNING_MAX_MS`
  (10 min) is dropped with its held link.

Which mark a box shows:
- a done or glance mark while it is live, else the newest running mark of that
  box;
- a done mark replaces any done or glance mark of the agent;
- a glance replaces only a glance. It is dropped when a done mark of the agent
  is live or pending, so a read never hides the result of a write.

### `Anims`

```rust
pub const MARK_SWEEP_MS: i64 = 700;
pub const MARK_MS: i64 = 2400;
pub const GLANCE_SWEEP_MS: i64 = 300;
pub const GLANCE_MS: i64 = 900;
pub const RUNNING_MAX_MS: i64 = 600_000;
const MAX_MARKS: usize = 48;

pub enum Tone { Ok, Conflict, Error }
pub enum LinkKind { P2p, Orchestrator, Broadcast, Vcs(Tone) }

pub struct Link {
    pub from: String, pub to: String, pub kind: LinkKind, pub started: i64,
    /// The running operation (`VcsEvent.op`) that keeps this link lit.
    pub held: Option<String>,
}

pub enum MarkKind { Running { op: String }, Done, Glance }

pub struct Mark {
    /// The box it is drawn on.
    pub agent: String,
    pub kind: MarkKind,
    pub tone: Tone,
    pub label: String,
    pub started: i64,
}

impl Mark {
    /// Position of the sweep (done: around the border; glance: along the
    /// bottom) while it runs, 0..1.
    pub fn sweep(&self, now: i64) -> Option<f64>;
    pub fn is_live(&self, now: i64) -> bool;
}

impl Anims {
    pub marks: Vec<Mark>,
    /// Start, or end, what the tables above say for this event.
    pub fn on_vcs(&mut self, event: &VcsEvent, now: i64);
    /// From a snapshot's events, oldest first: operations that started and
    /// have not ended. Restores their running marks and held links, and
    /// starts nothing else. `age` is how long ago each event happened.
    pub fn resume<'a>(&mut self, events: impl Iterator<Item = (&'a VcsEvent, i64 /* age, ms */)>, now: i64);
    /// Something is moving: a pulse, a sweep or a glance. Replaces
    /// `pulse_live` in `tick` and `next_wake`.
    pub fn animating(&self, now: i64) -> bool;
    /// An operation is running: the spinner turns.
    pub fn running(&self) -> bool;
    /// Also covers marks: the next start of a pending one, the next end of a
    /// live one, the cap of a running one. Held links have no expiry.
    pub fn next_expiry(&self, now: i64) -> Option<i64>;
    /// Also drops marks that ended, and running ones past the cap.
    pub fn gc(&mut self, now: i64) -> bool;
}
```

`Link::pulse` is `None` and `Link::is_live` is `true` while `held` is set.
`MAX_MARKS` bounds the list as `MAX_LINKS` does.

### Frame rate and idle

The rules of `App::tick` and `App::next_wake` stay; "a pulse is in flight"
becomes "something is moving":
- `tick`: dirty when `anims.gc(now) || anims.animating(now)`.
- `next_wake`: 33 ms while `anims.animating(now)`; otherwise the minimum of the
  next second, the spinner step (when `has_spinner()`, which now also holds
  while `anims.running()`), and `anims.next_expiry(now)`, never under 5 ms.
- What costs frames: a pulse (900 ms), a done sweep (700 ms), a glance
  (300 ms). A running mark redraws at the spinner's rate (150 ms), like a
  working agent. A lit or held link and a label that only waits to expire cost
  one wake at their expiry; a pending mark one wake at its start.
- An idle dashboard still wakes once a second
  (`idle_app_wakes_about_once_a_second` keeps passing).

### When an animation starts

- Live: in `App::apply_push`, `Push::Event` branch, for a `fresh` event (the
  existing `seq` test) whose payload parses and whose `timestamp` is at most
  10 s behind `server_now(now)`: `anims.on_vcs`.
- From a snapshot (`apply_snapshot`: attach, reconnect, `Resync`): VCS marks
  and VCS links are cleared, then `anims.resume` restores what is still
  running. Events in a snapshot are history: no pulse, no done mark, no glance.
  An operation whose start fell out of the snapshot's window is not shown as
  running; its terminal event still renders.

### Tests (tui)

- `anim.rs`:
  - each row of both tables;
  - a mark is pending until the pulse lands;
  - `started` then terminal with the same `op`: the running mark goes, the held
    link pulses; a terminal event alone gives the same link and mark;
  - a `started` whose end never comes is dropped at `RUNNING_MAX_MS`;
  - a glance does not replace a live or pending done mark, a done mark replaces
    a glance;
  - `animating` is false between the end of a sweep and expiry, and for a
    running mark; `running` is true for it; `gc` and `next_expiry` cover marks;
    the bounds.
- `app.rs`:
  - a live `vcs.commit` push makes the app wake at frame rate and return to
    once a second after `MARK_MS`;
  - a live `vcs.merge` `started` makes it wake at the spinner's rate, not at
    frame rate, until the terminal event;
  - a snapshot holding `started` without its end restores the running mark; one
    holding both, or only finished events, shows nothing;
  - an event 11 s old starts nothing; an event with an unknown `action` or
    `outcome` is logged and ignored.
- `fixture.rs`: the `git.conflict` sample becomes `vcs.*` samples with
  payloads (a write, a conflict, a running merge, a read), so the fixture
  dashboard shows each piece.

---

<a id="tests"></a>
## §tests

### Getting jj

- `shell.nix`: add `jujutsu` to `packages`.
- `CREW_JJ=<path>` overrides the PATH lookup (D2). Tests use it to put a
  wrapper script in front of jj that hangs when its arguments contain a
  chosen subcommand (`duplicate`, `new`, `bookmark`, `git export`, `abandon`)
  and runs the real jj otherwise: jj has no hooks to hang in, and this cuts an
  operation at a chosen phase.
- `CREW_TEST_VCS`, read by `crates/crew/tests/common/mod.rs` when it builds a
  team, and by the unit tests of `vcs/jj.rs`:
  - unset: `workspace.vcs` is left at `auto`. A host without jj runs everything
    on git; the jj unit tests print `skipped: jj is not on PATH` and pass.
  - `git`: every team is built with `vcs: git`.
  - `jj`: every team is built with `vcs: jj`, and a missing or old jj is a
    failure, in the manner of `CREW_TEST_SANDBOX=require`.
- `justfile`:
  - `test` stays (`auto`).
  - `test-git`: `CREW_TEST_VCS=git ./scripts/build-env cargo test --workspace`.
  - `test-jj`: `CREW_TEST_VCS=jj`, entering `nix shell nixpkgs#jujutsu` when jj
    is missing and nix is there, like `test-sandbox` does for bubblewrap.
  - `test-sandbox`: adds `nixpkgs#jujutsu` to its shell and runs with
    `CREW_TEST_VCS=jj`; `test-sandbox-git` is the same with `git`.
  - `test-all`: `test-git`, `test-jj`, `test-sandbox`, `test-sandbox-git`.
  `scripts/build-env` is not changed: it provides the compiler, not test tools.
- `notes/jj-argv-check.sh` is the reference for the argvs; the unit tests of
  `vcs/jj.rs` take over its assertions.

### How the git fallback stays covered

- `test-git` runs the whole suite on git, as today.
- The unit tests in `git/mod.rs` do not depend on the selection and always run.
- New selection tests (`vcs/mod.rs`): each row of the D2 table, with a fake
  `CREW_JJ` (absent, a script printing `jj 0.41.0`, a script printing
  `jj 0.44.0`), and the notice texts.
- e2e in `tests/team.rs`: `auto` with `CREW_JJ` pointing at nothing loads on
  git, emits `session.vcs` at `warn` and returns `SessionInfo.vcs == "git"`
  with the note; a workspace holding a git worktree from an earlier session
  stays on git and its note names the worktree and the way to move; with jj
  present and nothing in the way, `session.vcs` is `info` and names jj.

### Existing tests that change

`crates/crew/tests/team.rs`
- `agents_work_in_their_own_worktrees_and_exchange_commits`: runs under both
  backends. `has_event("git.conflict")` becomes a `vcs.integrate` event with
  outcome `conflict`, peer `rival`, files `["schema.sql"]`. Add: `vcs.commit`
  for `db`; `vcs.cherry_pick` with agent `ledger` and peer `db`; for each
  member a `vcs.workspace` pair; for the integration a `started`, the
  per-branch events in order, then the tally with the start's `op`; every
  `started` in the log has exactly one terminal event with its `op` after
  it; under jj also "plain git sees it"
  (`git rev-parse refs/heads/agents/db` equals the artifact) and "the checkout
  is untouched" (`git status --porcelain` empty, `HEAD` unchanged, no `.jj` in
  the workspace).
- New: `git_status` and `git_diff` each leave a `debug` `vcs.status` /
  `vcs.diff` event; a `git_commit` with nothing to commit leaves `vcs.commit`
  with outcome `error`; a refused call (shared checkout) leaves none.
- The briefing and resume texts (`read git status and git diff…`, around the
  `/w/.crew/worktrees/design` fixtures) follow the wording of the backend.
- Everything else is backend-neutral and runs under both.

`crates/crew/tests/shared_git.rs`: unchanged (no VCS runs in a shared checkout;
its `git status` check is on the user's repository).

`crates/crew/tests/sandbox.rs`
- `integrate_hooks_have_no_authority` and `git_status_ignores_agent_fsmonitor`
  are about git hooks and git configuration: they pin `vcs: git`.
- New jj twins: (1) from inside an agent's sandbox, creating
  `<repo>/config.toml`, `W/.jj/workspace-config.toml`, or rewriting
  `<repo>/store/git_target` or `W/.jj/repo` fails; (2) a commit, a cherry-pick
  with a conflict and an integration succeed in sandboxes; (3) the helper has
  no socket, identity or network, as the git test asserts.
- The reach tests that name `.git/HEAD`, `.git/late.sock` and
  `.git/worktrees/a` take their paths from the backend under test.
- `a_restart_writes_the_agents_next_identity_only_after_its_git_job_ends`:
  backend-neutral.

`crates/crew/tests/{sandbox_launch, pty_sandbox, git_helper_sandbox}.rs` build
profiles by hand from `git_dir` and `admin`: they move to the new `Session` and
`Role` shapes and keep testing git; `git_helper_sandbox.rs` pins git (it is
about a smudge filter). One profile test per role is added for `Store::Jj`
(the mount table of §sandbox, in order).

`daemon/gitops.rs`: `an_agents_git_job_keeps_its_launch_lease_after_the_request_is_gone`
gains the event side: a merge whose request is aborted while the job runs
still ends in a terminal `vcs.merge` event with the start's `op` (§events
rule 3); a job that panics ends in `error`.

Unit tests: `daemon/sandbox.rs` (fixtures gain a jj variant),
`sandbox/profile.rs` (the mount list for `Store::Jj`), `crew-core`
(`VcsEvent` round trip; the wire manifest test is untouched: no enum or push
kind changes, `PROTOCOL_VERSION` stays 4).

### New tests for the jj backend (`vcs/jj.rs`)

The list of `git/mod.rs`, for jj:
- prepare creates the handle, leaves no file in it and nothing in the project
  root; is idempotent; a project without a commit is "nothing to start from".
- each agent gets its own workspace and bookmark; a subagent starts from its
  parent's branch; a removed directory is re-added; a foreign non-empty
  directory is refused.
- commit: identity, change id in `CommitInfo`, nothing to commit, a file over
  1 MiB is committed, the guard file and `.jj` are not, the branch is visible to
  plain git.
- commit and cherry-pick move work between agents; the `-x` trailer; the
  source author.
- a conflicting cherry-pick and a conflicting merge leave no trace (files,
  head, bookmark, exported ref, clean status); an empty pick is refused and
  rolled back.
- refusals: uncommitted changes; every rejected revision form; an unknown
  commit.
- integration merges what merges, reports conflicts, skips what is not ahead;
  a second run does nothing.
- timeouts, one test per row of the table in §conflicts, with the wrapper
  hanging on that phase's subcommand: jj is killed with its children; cut in
  the mutation → nothing landed; cut in `bookmark set` and cut in `git
  export` after a clean merge and after a clean pick → the result is kept,
  the bookmark and `refs/heads/agents/<id>` point at it, the error says so;
  cut after a conflicted merge and pick → rolled back, bookmark and ref
  still on the old parent; cut between `rebase` and `abandon` → restored; a
  commit cut in `git export` → the commit is reported as made.
- a conflicted result that already carries the bookmark is not abandoned.
- a project that is a linked git worktree whose `HEAD` is on another branch
  than the main checkout's: the store is the common directory, agents start
  from the linked `HEAD`, commits appear as branches of the shared
  repository, and `HEAD`, index and files of both checkouts are unchanged; a
  sandboxed team is refused there.
- moving from git: after `git worktree remove`, an agent with a branch from
  the git backend gets a jj workspace at that branch.
- going back to git (`notes/jj-argv-check.sh`, "jj -> git"): after the steps
  of D2 on a workspace with uncommitted work, the git branch holds it and a
  git worktree on the branch has the files; and the hazard itself: after a
  bare `jj commit` the bookmark and the git branch have not moved.
- a stale workspace is an error that names `jj workspace update-stale`.
- every command goes through the installed launcher (the test that exists for
  git, with the jj argv).
- native layout: bootstrap, base, one commit, export.

---

<a id="split"></a>
## §split

Four members. No file is edited by two of them. Where one needs something in
another's file, it is listed under "interfaces".

### The seam (`crates/crew/src/vcs/mod.rs`)

```rust
pub enum Kind { Jj, Git }

/// `git::Worktree` (re-exported), which gains `created`: this call made it.
pub struct Workspace { pub path: PathBuf, pub branch: String, pub created: bool }

/// `git::IntegrationStep`, re-exported.
pub struct IntegrationStep { pub agent: String, pub outcome: GitOutcome }
pub struct Picked { pub outcome: GitOutcome, pub source: String, pub author_email: String }

/// Where the store is, for the sandbox profile. Paths the daemon made or
/// read before any agent ran. `git_dir` is `<git>` of §layout: the directory
/// with the objects and refs (a linked worktree's common directory), never
/// the checkout's admin directory.
pub enum Store {
    Git { git_dir: PathBuf },
    Jj { git_dir: PathBuf, repo_dir: PathBuf },
}

pub trait Vcs: Send + Sync {
    fn kind(&self) -> Kind;
    /// The binary, absolute and canonical: what a sandbox binds and runs.
    fn program(&self) -> &Path;
    fn store(&self) -> Store;

    /// Make the store ready (idempotent) and say whether there is a commit
    /// to start from. Err: `ws` is not the root of a repository. Resolves
    /// `<admin>` and `<git>` (§layout).
    fn prepare(&self, ws: &Path) -> Result<bool>;
    fn branch_exists(&self, branch: &str) -> bool;

    /// Register (or find) the agent's workspace. Runs nothing inside it.
    /// The bool: its files still have to be put in place.
    fn add_workspace(&self, dir: &Path, agent: &str, base: &str) -> Result<(Workspace, bool)>;
    /// Inside the workspace; under the helper launcher when there is one.
    fn populate(&self, workspace: &Workspace) -> Result<()>;

    // Inside the agent's workspace; under the agent's launcher when there is one.
    fn commit_all(&self, dir: &Path, agent: &str, message: &str) -> Result<CommitInfo>;
    fn diff(&self, dir: &Path, against: Option<&str>) -> Result<String>;
    fn status(&self, dir: &Path) -> Result<String>;
    fn cherry_pick(&self, dir: &Path, agent: &str, rev: &str) -> Result<Picked>;
    fn merge(&self, dir: &Path, agent: &str, branch: &str) -> Result<GitOutcome>;

    /// Inside the integration workspace; under the helper launcher. `on_step`
    /// is called with each step as soon as it is done (§events rule 6).
    fn merge_agents(&self, workspace: &Workspace, agents: &[String],
                    on_step: &mut dyn FnMut(&IntegrationStep)) -> Result<Vec<IntegrationStep>>;
}

/// D2. What was chosen, and what to tell the user about it.
pub struct Selection { pub vcs: Arc<dyn Vcs>, pub note: String, pub fallback: bool }
pub fn select(choice: VcsChoice, ws: &Path, state_dir: &Path) -> Result<Selection>;
```

The daemon keeps the selected backend for the session (`Daemon::vcs()`), set in
`load_team` and restored with the session.

### Step 0: the contract (member **vcs**, first, one small commit)

Everyone else cherry-picks it before starting. No behaviour changes.
- `crates/crew-core/src/vcs.rs` (§events) and `pub mod vcs;` in `lib.rs`.
- `crew_core::config`: `VcsChoice { Auto, Jj, Git }`, `WorkspaceConfig.vcs`
  (`#[serde(default)]`, skipped when default).
- `crew_core::rpc`: `CommitInfo.change_id: Option<String>`, `SessionInfo.vcs:
  String`, `SessionInfo.vcs_note: String` (all defaulted).
- `crates/crew/src/vcs/mod.rs`: the types and trait above, `vcs::Git`
  delegating to `crate::git`, `select` returning git for now, `Daemon::vcs()`.
  `Workspace` and `IntegrationStep` are the `git` types re-exported, so
  signatures that name `git::Worktree` keep compiling.
- The `git::` calls in `daemon/gitops.rs`, `daemon/team.rs`,
  `daemon/supervisor/creation.rs` and `daemon/sandbox.rs`
  (`provision_worktree`, `integrate`) go through `Daemon::vcs()`. This is the
  only time vcs edits `daemon/sandbox.rs`; after step 0 the file is
  sandbox's.

### Member **vcs**: backend, daemon callers, events

Owns:
- `crates/crew/src/vcs/**` (new: `mod.rs`, `jj.rs`, selection).
- `crates/crew/src/git/mod.rs`, `git/beneath.rs` (`output` made `pub(crate)`
  with the program's name in its timeout text; `beneath` made `pub(crate)`;
  `Root::write_new`; `cherry_pick` returning the source for `Picked`).
- `crates/crew/src/daemon/gitops.rs` (calls the trait; `VcsOp` and every
  `vcs.*` event, emitted from the job;
  `report_git` moves here and goes away), `daemon/handlers.rs` (only the removal
  of `report_git`), `daemon/team.rs` (selection, `prepare`, the `session.vcs`
  notice, `session.started` text), `daemon/supervisor/creation.rs` (the
  `vcs.workspace` events), `daemon/mod.rs` (`vcs` and its note in the session
  state and in `SessionInfo`; the `op` counter).
- `crates/crew/src/cli/ops/plain.rs` (`is_blank` also allows `.jj`;
  `workspace_is_isolated` treats a root with `.jj` as isolated).
- `crates/crew/src/cli/ops/{team, session}.rs` (the backend and the note in
  the "started" line and in `crew status`; the note handed to the dashboard
  as its opening notice; `landing_hint` unchanged: `agents/integration` is a
  git branch under both backends).
- `crates/crew-core/src/{vcs.rs, config.rs, rpc.rs, lib.rs}`.
- The unit tests in those files.

### Member **sandbox**: §sandbox

Owns:
- `crates/crew/src/sandbox/profile.rs`, `crates/crew/src/daemon/sandbox.rs`,
  `crates/crew/src/git/launch.rs`, `crates/crew/src/daemon/retire.rs` (the
  ledger label, if it changes).
- `crates/crew/tests/{sandbox, sandbox_launch, pty_sandbox,
  git_helper_sandbox}.rs`.

### Member **tui**: §anim

Owns `crates/crew-tui/**`. Consumes `crew_core::vcs` and nothing else of this
work; may show `SessionInfo.vcs` where isolation and sandbox are shown. The
fallback note reaches the dashboard through `crew_tui::run`'s existing
`notice` argument and needs nothing new in the TUI.

### Member **qa**: e2e tests, agent-facing wording, docs, integration

Owns:
- `crates/crew/tests/{team, shared_git}.rs`, `crates/crew/tests/common/mod.rs`
  (`CREW_TEST_VCS`).
- `shell.nix`, `justfile`, `README.md` (jj as the default, `workspace.vcs`,
  the fallback and its notes, "Moving a workspace to jj" and back with the
  steps of D2, the `vcs.*` events), `examples/`.
- Agent-facing wording: `crates/crew/src/agents/prompt.rs` (`TeamView.vcs`; the
  workspace paragraph and the resume text say, under jj: the directory is a jj
  workspace published as branch `agents/<id>`; inspect with the `git_status`
  and `git_diff` tools or `jj status`, `jj diff`, `jj log`; plain `git` does
  not work there; do not run history-changing jj commands, and never on a
  teammate's commits), `crates/crew/src/mcp/tools.rs` (descriptions;
  `render_commit` shows the change id when there is one),
  `crates/crew/src/daemon/supervisor/launch.rs` (fills `TeamView.vcs`; adds
  `JJ_USER=<id>` and `JJ_EMAIL=<id>@crew.local` to the agent's launch
  environment under jj, so jj run by the agent itself records its name).
- Integration: merges the three branches, runs `just test-all` and
  `just lint`, rebuilds the release binary (`just build`).

### Interfaces between members

| between | what | fixed by |
|---|---|---|
| vcs → all | `crew_core::vcs::VcsEvent`, `VcsChoice`, `CommitInfo.change_id`, `SessionInfo.vcs`, the `Vcs` trait and `Store` | step 0 |
| vcs ↔ sandbox | `Daemon::member_worktree(team, id, base) -> Result<Workspace>`, `Daemon::integration(config, base, agents, on_step) -> Result<(Workspace, Vec<IntegrationStep>)>` (`on_step` passed through to `merge_agents`), `Daemon::agent_git_launcher(agent)`: names and meaning stay; sandbox changes their bodies, vcs calls them | this section |
| vcs → sandbox | `Vcs::store()`, `Vcs::program()`; `git::output` and the `Launcher` hooks (`apply`, `started`, `active`) keep their signatures | step 0 |
| vcs → tui | events exactly as §events, and `SessionInfo.vcs`; tui reads nothing else | step 0 |
| vcs → qa | event names and payloads for assertions; `Daemon::vcs().kind()` for `TeamView.vcs` | step 0 |
| sandbox → vcs | none: `vcs/jj.rs` never names a mount | – |

### Order

1. vcs: step 0. Announce the commit; sandbox, tui and qa cherry-pick it.
2. In parallel:
   - vcs: `vcs/jj.rs` with its unit tests (unsandboxed), then the daemon
     callers and events, then selection (D2).
   - sandbox: profile and daemon sandbox code against `Store`, testing with
     git first and with jj once vcs's `jj.rs` is available (cherry-pick).
   - tui: §anim against the contract, with fixture events.
   - qa: test harness (`CREW_TEST_VCS`, justfile, shell.nix), wording; e2e
     assertions once vcs's events land.
3. qa integrates and runs the matrix of §tests.

### Risks to watch during implementation

- jj version drift: the argvs and templates are pinned to 0.44; `select`
  refuses older versions, and `notes/jj-argv-check.sh` is the quick check
  against a newer one.
- Agent CLIs in a directory that is not a git repository (D9). Not verified
  here: the real-CLI test (`just test-real`) should be run once under jj for
  each runtime, and Codex's trust dialog watched for.
- `refs/jj/keep/*` grow with every snapshot (D12).
- Read events are not rate-limited (§events rule 8). If an agent that polls
  `git_status` floods the log, coalescing belongs in `VcsOp`, not in the TUI.
