# crew — production-readiness review: summary

Lead reviewer: `lead`. Date: 2026-10-03. This is a review only: no product code, tests or Cargo files were changed, and nothing was committed. This file is uncommitted in the shared checkout, as the orchestrator instructed.

Area reports: [baseline.md](baseline.md) · [core.md](core.md) (daemon core) · [runtime.md](runtime.md) (agent runtime) · [core-lib-and-tui.md](core-lib-and-tui.md) (crew-core and crew-tui).

---

## 1. Verdict: not production-ready

**What is in good shape.**

- It builds without warnings and clippy is clean.
- 274 tests pass deterministically, and they run in good isolation.
- The core designs are sound: a pure scheduler, a transactional message bus, and private file permissions.

**What blocks production.** The tests cover the normal flows only. Outside them, the team made 37 claims. The lead confirmed 31 in code, marked 5 plausible and rejected 1 (section 4). After merging duplicates, that leaves 27 bugs (section 5): 10 blockers, 9 high, 8 medium or low. The blockers fall into three groups:

- **Harm to the operator's machine or repository.**
  - A shared-checkout `git_commit` commits secrets and teammates' work.
  - The herdr backend can type a message into a shell and press Enter.
  - Any agent can act as the operator.
- **Work that stops without anyone being told.**
  - Several task states are never reported as a stall.
  - The review gate can be skipped silently.
  - A rejected review gives one agent two active tasks.
- **Concurrency and durability.**
  - Two daemons can run for one workspace.
  - The daemon acknowledges success for writes that never reached SQLite.
  - Two simultaneous team loads merge into one session.
  - Killing an agent can leave its child processes running.

**Fit for now:** attended use by the operator, under four conditions:

1. Use worktree isolation, in a repository that already has a commit.
2. Use the PTY backend, not herdr.
3. Start the daemon once.
4. Watch the dashboard.

**Not fit for:** unattended runs, other people's repositories or machines, the herdr backend, or shared-checkout mode.

### Must-fix blockers

1. **Shared-checkout `git_commit` commits `.crew/`, including agent tokens and `crew.db`, plus every teammate's uncommitted files.** Bug B1.
2. **The herdr delivery fallback types message text plus Enter into a pane that may be back at a shell.** Bug B2.
3. **The operator-impersonation chain: any agent can act as the operator, so the operator-only model/CLI control is not enforced.** Bugs B3a–c.
4. **The stall detector misses tasks that can no longer progress, so the team hangs without a signal.** Bugs B4a–c.
5. **A rejected review gives its author two active tasks.** Bug B5.
6. **The review gate is skipped silently when the reviewer is unavailable.** Bug B6.
7. **Two daemons can run for one workspace.** Bug B7.
8. **The daemon acknowledges task transitions that SQLite never stored.** Bug B8.
9. **Killing a PTY agent leaves descendants running.** Bug B9.
10. **Simultaneous team loads merge two teams.** Bug B10.

**Before any of these: hygiene.** Make the first commit with the right exclusions, then a standalone `cargo fmt` commit. See section 10.

---

## 2. Method and source state

**Who reviewed what.**

- **baseline** ran build, tests, lint, fmt and dependency checks.
- **rev-core** and **rev-runtime** (GPT-family models) reviewed the daemon core and the agent runtime. Each reproduced its findings in isolated harnesses:
  - rev-core: `/tmp/crew-rev-core/repro*.py` → `results*.jsonl`, daemons in `/tmp/rcore-*`.
  - rev-runtime: `/tmp/crew-runtime-review-20261003`.
- **rev-tui** reviewed crew-core and crew-tui, using probe crates.

**What the lead did (cross-family check).**

- Read the cited code for every finding marked bug and every risk that blocks production, including callers and error paths.
- Checked whether an existing test covers each one.
- Re-ran the suite and clippy.
- Results are in section 4.

**The source changed during the review.**

- The operator edited from another session between 00:03 and 00:29: `agents/{claude,codex,mod,prompt}.rs`, `daemon/{limits,supervisor,delivery}.rs`, `tests/team.rs` and four `crew-tui` files.
- The latest edits (00:25:44) make teammates' status messages to the orchestrator wait for the next message that wakes it (`daemon/delivery.rs` `waits_for_company`/`wakes`, and recovery at `supervisor.rs:1688`).
- At 00:28 the lead re-located every finding below by its quoted code in the current checkout. **No finding was fixed during the review.**
- Line numbers are as of 00:28. If they move again, search for the quoted code.

**The evidence is temporary.** It lives in `/tmp` and may not survive a reboot. Each area report describes its reproduction steps in full, so the evidence can be rebuilt.

---

## 3. Baseline

The lead re-ran these on the current tree, with `CARGO_TARGET_DIR` outside the checkout and an isolated `HOME`/`XDG_*`. The live daemon's `.crew/` was not touched.

| Check | Result |
|---|---|
| `cargo build` debug and release | **green**, 0 warnings (baseline) |
| `cargo clippy --workspace --all-targets` | **green**, 0 warnings (lead, current tree) |
| `cargo test --workspace --locked` | **green**: 274 passed, 2 ignored, 0 failed (lead, tree as of 00:28; an earlier lead run at 00:24 gave 273/2/0). Baseline had 272 passed ×3; the operator's edits added tests. The only later edit (00:28:52, `supervisor.rs`) is a one-character borrow change. |
| `cargo fmt --all --check` | **already failing**: 72 files, about 1,120 hunks. Not a config issue, since `max_width=120` still leaves 757 hunks. |
| One unit test depends on the environment | **already failing under a long `XDG_RUNTIME_DIR`**: `crew-core/src/paths.rs:274` `socket_paths_are_short_and_stable` builds its path from the live env. It passes with a short one. |
| `cargo audit` / `cargo deny` | **not run**, because the tools are not installed |
| Ignored tests | `crew/tests/real_cli.rs:30` needs the real Claude CLI and spends tokens; `crew-tui/src/net.rs:650` needs a TTY. Neither was run, by design. |

**Flakiness.** None was observed across 7 full runs. The likely future source is the fixed sleeps in `tests/team.rs` (400–1500 ms, used to prove that nothing happened) and the `sleep 300` scripted agents. The 33 end-to-end tests take about 26 s, run serially.

**Test quality.** The tests are good:

- Scripted agents stand in for real CLIs.
- Each test gets its own tempdir and `XDG_*` environment.
- No network access is needed.
- The suite leaves nothing behind.

**What the suite misses** is what section 9 lists: concurrency, failures and alternate modes.

---

## 4. Verification of every bug finding and blocking risk

**Legend.**

- **CONFIRMED:** the lead read the code path and it does what the finding says, and the area reviewer reproduced it, unless the row says otherwise.
- **PLAUSIBLE:** the code supports it, but it was not reproduced, or it depends on outside behaviour.
- **REJECTED:** the claim does not hold.

| Id | Finding | Verdict | One-line reason (lead's check) |
|---|---|---|---|
| CORE-01 / tui 3 | Agent can become operator | CONFIRMED | `server.rs:178` `Role::Operator => Ok(Who::Operator),` needs no credential; `handlers.rs:117` `conn.who = Some(who);` replaces an authenticated agent identity. Repro: worker re-Hello then `AgentKill` succeeds. |
| CORE-02 | Two daemons per workspace | CONFIRMED | `daemon/mod.rs:519` check, then `:532` `remove_file(&socket)` and `:533` bind, with no lock. Repro 4/4. Existing `a_second_daemon_…_is_refused` test is sequential only. |
| CORE-03 | Unpersisted success | CONFIRMED | `daemon/mod.rs:392-394` logs `save_task` errors and carries on. Repro with an SQLite trigger. `busy_timeout` 5 s and WAL (`persistence/sqlite.rs:92-94`) make it rare, but disk-full or a second daemon (CORE-02) can trigger it. |
| CORE-04 | Concurrent team loads merge | CONFIRMED | `team.rs:58-75` check, awaits, then `:141` `clear_session()`, with no lifecycle lock. Repro: both RPCs succeed and the roster has both workers. |
| CORE-05 | Review gate skipped silently | CONFIRMED | `orchestrate.rs:514` filter drops a terminal or self reviewer, and the task goes straight to Done with no event. `a_reviewed_task_is_done_only_after_approval` covers the happy path only. |
| CORE-06 | Two active tasks after rejection | CONFIRMED | `tasks/mod.rs:78` (Review does not occupy) and `scheduler/mod.rs:95-99` let the author take new work; `orchestrate.rs:748-752` reactivates without checking; `active_task` uses `.find`. |
| CORE-07 | Retired subagent gets new work | CONFIRMED | `subagents.rs:202-237` kills the process but leaves the status Done; `orchestrate.rs:39` `runnable: !a.paused && !a.status.is_terminal()`. Repro. |
| CORE-08 | Reviewer lost or stopped, so the task hangs in Review with no stall | CONFIRMED | Recovery at `supervisor.rs:1681-1688` ignores `task.reviewer`; `scheduler/mod.rs:153` counts every Review task as progress; nothing reconciles a dead reviewer. Repro across a restart. |
| CORE-09 | Duplicate agent id under concurrency | CONFIRMED | `supervisor.rs:169` check, awaits at `:184` and `:198`, unconditional `:152` `model.agents.push`. Repro. |
| CORE-10 | Never-started blocked task can be completed | CONFIRMED | `orchestrate.rs:489` accepts `Blocked` without checking `started_at`. Repro. Lower impact: the caller must be the assignee or the orchestrator, using an explicit id. |
| CORE-11 | Failed dispatch leaves the task Working forever | CONFIRMED | `orchestrate.rs:184-191` sets Working first, and `:223-233` only emits `task.dispatch_failed`. `create_task` does not check size against `max_message_bytes` (default 16 KiB). Repro with 70 KB. |
| CORE-12 | Long subagent id leaves an orphan | CONFIRMED | Ids are valid up to 32 characters (`util.rs:64`), but `subagents.rs:156` `format!("{id}-task")` is then too long, and fails after the agent was persisted. Repro. |
| R1 | Shared-checkout commit leaks `.crew/` and teammates' work | CONFIRMED | `git/mod.rs:267` `add -A`; `team.rs:107-108` excludes `.crew/` only `if result.0` (worktrees); `gitops.rs:61-66` checks only for a repo root; the prompt (`agents/prompt.rs:157`, `:498`) tells every agent to `git_commit`. This run was in that mode. |
| R2 | herdr fallback types into a shell | CONFIRMED | `herdr.rs:612` checks liveness once; after 12 × `agent_not_ready`, `:631-641` sends `pane.send_input` with `"keys": ["enter"]`. Only newlines and controls are replaced. The pane is a shell after the CLI exits, per the comments at `:366-368` and `:535`. The sink is reproduced with a fake herdr. That herdr answers `agent_not_ready` for an exited agent is what the code's own fallback comment assumes. |
| R3 | PTY kill leaves descendants | CONFIRMED | `pty.rs:373-385` returns as soon as `alive` (the leader's `wait`) is false, before `SIGKILL` to the group. Repro with a TERM/HUP-ignoring child. Children in other process groups are never signalled at all. |
| R4 | Codex sidecar lacks `CREW_IDENTITY` | CONFIRMED | `codex.rs:133` `env: env.clone(),` for the sidecar vs `:180` for the TUI; `supervisor.rs` `start_sidecar` passes only `sidecar.env`. Launch test. |
| R5 | `workspace.init` commits staged work, omits untracked | CONFIRMED | `git/mod.rs:137` `commit … --allow-empty` commits the existing index. Git test. |
| R6 | Codex sidecar socket path too long | CONFIRMED | `codex.rs:120` `agent_dir.join(format!("codex-{}.sock", …))` has no length check, unlike the daemon's hashed socket. A 144-byte path fails to bind. |
| R7 | herdr ignores the resolved pi binary | CONFIRMED | `herdr.rs:540-548` `agent.start` sends `kind` and `args`, never `spec.program`. Fake-herdr test. On this host the `pi` on `PATH` is GSD's, not pi.dev. |
| R8 | "usage limit reached" in normal text parks the agent | CONFIRMED | `limits.rs:537` and `:558` substring match on screen text; the caller is `daemon/limits.rs:377`. Parser test. |
| R9 | `crew config` prints `env` secrets | CONFIRMED | `endpoints.rs:650` `row(out, "env", &format!("{name}={value}"), *scope);`, with `mask()` used only for key fields. Config test. |
| R10 / tui 11 | Agent id `integration` blocks integration | CONFIRMED | `git/mod.rs:22` `"agents/integration"` vs `branch_for`; `config.rs:684` reserves only orchestrator and operator. Git test. |
| R11 | pi probe deadline does not cover stdout | CONFIRMED | `agents/pi.rs:145` `Ok(Some(_)) => break` then an unbounded `reader.join()`. Timed test: a 10 s limit returned after 12.0 s. |
| R12 | Hung git hook holds the global git lock | PLAUSIBLE | `git/mod.rs:50-54` `.output()` has no timeout, and `gitops.rs:39-42` holds `git_lock`. Traced, not run. |
| R13 / tui 4 | Overflowing quota breaks typed pushes and snapshots | CONFIRMED | `limits.rs:89`, `:138`, `:197` `as_f64()? as f32` turns `1e300` into inf, which serializes as `null`, and the `Quota` decode fails. Reproduced. Very unlikely input. |
| tui 1 | Escape-sequence injection in the inspector | CONFIRMED | `views/detail.rs:409` `format!("   ⬡ {} {}", artifact.kind, artifact.value)` renders raw through `:196` `Paragraph`; `description` is sanitized but kind and value are not. Probe. |
| tui 2 | Pushes buffered during `Listen` are lost | CONFIRMED | `client.rs:111` buffers them, `:102-104` `next_frame` ignores the buffer; the bridge (`cli/agent_side.rs:127-131`) calls `call_value(Listen)` then `next_frame`; `handlers.rs` `listener_joined` (calls `request_delivery`) runs before the reply. Probe. |
| tui 4 | No version negotiation | CONFIRMED | `rpc.rs:19` `PROTOCOL_VERSION: u32 = 1`, no `#[serde(other)]` anywhere, `Push` adjacently tagged at `rpc.rs:524`. Probe. |
| tui 5 | 3 s TUI timeout reports completed actions as failed | PLAUSIBLE | `net.rs:29` `REQUEST_TIMEOUT` applies to kill and restart; a PTY kill alone can take 3 s (`pty.rs:379-391`). Not run. |
| tui 6 | `/tmp/crew-<uid>` fallback follows symlinks | PLAUSIBLE | `paths.rs:67` `is_dir()` and `:74` `set_permissions` follow symlinks. Needs `XDG_RUNTIME_DIR` unset and a hostile local user. |
| tui 7 | Message and question scroll counts characters | CONFIRMED | `input.rs:580` and `:752` `body.chars().count()`. Probe. |
| tui 8 | Quadratic input-line rendering | CONFIRMED | Measured: about 30 ms per frame at the 8000-character cap. |
| tui 9 | Bracketed-paste breakout | PLAUSIBLE | `input.rs:148` wraps the text without stripping `\x1b[201~`. Not run. |
| tui 10 | Config limits unvalidated | CONFIRMED | `config.rs:638` validates only `max_message_bytes`. |
| tui 14 | MCP bridge reuses its client after a timeout | PLAUSIBLE | `mcp/mod.rs:83` `looks_disconnected` matches on text, and timeouts do not match. Misframing not reproduced. |
| baseline 2 | `socket_path` has no `sun_path` guard | CONFIRMED | `paths.rs:110` with no length check or fallback. Prod risk only with a long `XDG_RUNTIME_DIR`. |
| baseline 6 | Mistyped `crew <word>` is sent to a live team | **REJECTED** | See section 7. |

---

## 5. Bugs, ranked by production impact (confirmed first, duplicates merged)

Each bug lists its location (with the quoted code), what goes wrong, the fix, and **the test** that should land with the fix, in its own commit.

### Blockers

**B1. Shared-checkout `git_commit` publishes crew's private state and other agents' work.** *(R1; core cross-ref)*

- **Where:**
  - `crates/crew/src/git/mod.rs:267` `out(dir, &["add", "-A", "--", "."])?;`
  - `crates/crew/src/daemon/team.rs:107-108` `if result.0 { git::exclude_state_dir(&workspace)?; }`
  - `crates/crew/src/agents/prompt.rs:157` `When your task is done: git_commit your work`
- **What goes wrong:**
  - **When:** `isolation: shared`, or `isolation: auto` in a repository without commits (which silently falls back to shared).
  - **What gets committed:** `.crew/crew.db`, each `.crew/agents/<id>/identity.json` (`{socket, agent_id, token}`), and every teammate's uncommitted edits, all attributed to one agent.
  - **Why it matters:** once pushed, the leak cannot be undone.
  - **This review was in that mode.** The orchestrator had to ban `git_commit`. This repository's own `.gitignore` happens to cover `.crew/`.
- **Fix:**
  - Call `exclude_state_dir` for every git-backed workspace.
  - Refuse `git_commit` in shared mode, or require explicit paths.
  - Make the shared-mode prompt say how work is handed over instead.
- **Test:** an integration test in shared mode on an unborn repository. `git_commit` must not stage `.crew/`, and must fail or commit only the caller's paths.

**B2. herdr delivery can run teammate text as a shell command.** *(R2)*

- **Where:** `crates/crew/src/process/herdr.rs:612` (liveness checked once) and `:631-641`: `Err(e) if e.to_string().contains("agent_not_ready") => {` … `"pane.send_input"` … `"keys": ["enter"]`.
- **What goes wrong:** the agent CLI exits while a delivery is retrying. After about 6 s of `agent_not_ready`, the text is typed into the pane's shell and Enter is pressed. A message containing `;`, `$( )` or `>` runs with the user's privileges.
- **Fix:** delete the raw fallback for agent runtimes. Fail and requeue the delivery, and re-check liveness inside the retry loop.
- **Test:** against a fake herdr that answers `agent_not_ready` 12 times, `submit_prompt` must return an error and send no `pane.send_input`.

**B3. Operator impersonation chain: the operator-only model/CLI control is not enforced.** *(CORE-01, tui 3, R4)*

This is ranked high because the operator explicitly wants operator-only control of models and CLIs enforced in the daemon. Three parts combine.

- **B3a. The daemon accepts any operator hello.**
  - **Where:** `crates/crew/src/daemon/server.rs:178` `Role::Operator => Ok(Who::Operator),` and `daemon/handlers.rs:117` `conn.who = Some(who);`.
  - **What goes wrong:** an authenticated agent sends `Hello{role:"operator"}` on its own connection, or opens a new connection, and can then run `AgentSetModel` on the orchestrator, `AgentKill`, `Shutdown`, or answer operator questions. rev-core reproduced it: a worker killed the reviewer.
- **B3b. Every explicit `crew` subcommand connects as operator.**
  - **Where:** `crates/crew/src/cli/ops.rs:40-42` `let (c, _) = Client::operator(Path::new(&info.socket), client)?;`.
  - **What goes wrong:** only plain `crew` checks `CREW_IDENTITY` (`ops.rs:268`). An agent that runs `crew agent model …` or `crew stop` in its shell acts as the operator, by honest mistake or not.
- **B3c. The Codex sidecar does not get `CREW_IDENTITY`.**
  - **Where:** `crates/crew/src/agents/codex.rs:133` `env: env.clone(),` (sidecar) vs `:180` `std::iter::once((IDENTITY_ENV.to_string(), identity))` (TUI only).
  - **What goes wrong:** Codex tool shells run under the sidecar, so they have no identity. Even plain `crew "…"` from a Codex agent then reaches `ops.rs:289-313` and is delivered to the orchestrator as operator input. `crew ctl` cannot find its identity either.
- **Fix:**
  1. Make a connection's identity fixed after its first hello (reject a second `Hello`).
  2. Make every operator subcommand refuse to run while `CREW_IDENTITY` is set.
  3. Pass the identity to the Codex sidecar.
  4. Require an operator credential that is never placed in agent environments or prompts.
  5. Document the remaining boundary: same-uid code that is actively hostile can still read anything the operator can.
- **Tests:**
  - A daemon test: re-Hello as operator is refused, and an operator hello without the credential is refused.
  - A CLI test: `crew stop`, run with `CREW_IDENTITY` set, exits non-zero without connecting.
  - A launch test: the sidecar env contains `CREW_IDENTITY`.

**B4. Tasks that can never progress are invisible to the stall detector.** *(CORE-08, CORE-11, CORE-07)*

- **Shared cause:** `crates/crew/src/scheduler/mod.rs:153` `.any(|t| matches!(t.status, TaskStatus::Working | TaskStatus::Review))` counts every Working or Review task as progress. Each of three paths parks a task there for good:
- **B4a. A task stays in Review after its reviewer stopped, failed or was removed.**
  - Nothing reconciles it.
  - Restart recovery does not count review obligations either: `daemon/supervisor.rs:1681-1688` `open || (agent.orchestrator && model.session.summary.is_none())`. A daemon restart alone therefore causes this (reproduced).
- **B4b. A dispatch fails, but the task stays Working.**
  - `daemon/orchestrate.rs:184-191` sets Working, then `:223-233` emits only `task.dispatch_failed`.
  - Triggered by a task description over `max_message_bytes` (reproduced with 70 KB), or by large dependency results inflating the dispatch.
- **B4c. A task is dispatched to a retired subagent.**
  - `daemon/subagents.rs:202-237` kills the process and keeps the status Done; `orchestrate.rs:39` still treats it as runnable (reproduced).
- **Fix:**
  - Have the scheduler count Review as progress only if the reviewer is live, and report it to the orchestrator otherwise.
  - Have recovery restart reviewers that have open reviews.
  - Make dispatch a single transition with its message (outbox), or roll back to Ready on failure.
  - Validate task size at `create_task`.
  - Add a Retired state, or exclude retired agents from `runnable`.
- **Tests:** one integration test per path:
  - Stop the reviewer of a task in Review: a Stall is reported within the grace period.
  - Create a task over `max_message_bytes`: rejected at creation, or back to Ready.
  - After subagent retirement, a new unassigned task does not go to the dead subagent.

**B5. A rejected review gives its author two active tasks.** *(CORE-06)*

- **Where:** `crates/crew/src/daemon/orchestrate.rs:748-752` `t.status = TaskStatus::Working;` … `"changes requested by {reviewer}"`, and `crates/crew/src/tasks/mod.rs:78`. Review does not occupy the author, so the scheduler hands out new work.
- **What goes wrong:**
  - **When:** any team with reviewers and more than one task per agent.
  - **Effect:** after a rejection, both tasks are Working. `task_complete`, `git_commit` and `task_update` without an id go to whichever comes first in `active_task` (`.find`). rev-core reproduced completing the wrong task.
- **Fix:** put the rejected task back in the queue (Ready, keeping the same assignee, comments and artifacts), and let the scheduler dispatch it once the author is free.
- **Test:** author on `second` → reject `first` → only one task occupies the author, and `task_complete()` closes `second`.

**B6. The review gate is skipped silently.** *(CORE-05)*

- **Where:** `crates/crew/src/daemon/orchestrate.rs:514` `.filter(|r| *r != author && model.agent(r).is_some_and(|a| !a.status.is_terminal()))`.
- **What goes wrong:** when the reviewer is stopped, failed or removed, or is the author, the task goes straight to Done. No event says the review was skipped, and its dependents are released.
- **Fix:**
  - Keep the task in Review.
  - Notify the orchestrator, and require it to reassign the review or waive it explicitly (as an event).
  - Reject self-review at create and load.
- **Test:** a stopped reviewer means `task_complete` leaves the task in Review, the orchestrator is notified, and dependents stay Blocked.

**B7. Two daemons can run against one workspace.** *(CORE-02)*

- **Where:** `crates/crew/src/daemon/mod.rs:519` `if let Some(info) = paths::read_daemon_info(&state_dir)` … then `:532` `let _ = std::fs::remove_file(&socket);`.
- **What goes wrong:** two starts at once (two `crew` invocations, or auto-start racing) both pass. The second unlinks the first's socket. Both then open `crew.db` and drive the same agents (reproduced 4/4).
- **Fix:** take a `flock` on `state_dir/daemon.lock` before the check and hold it for the daemon's lifetime. Only the lock holder may remove the socket or registry entry.
- **Test:** start two daemons on one workspace at once: exactly one survives, and the other exits with "already running".

**B8. The daemon acknowledges transitions that were never persisted.** *(CORE-03)*

- **Where:** `crates/crew/src/daemon/mod.rs:392-394` `if let Err(e) = self.store.save_task(&after) { tracing::error!("persisting task {id} failed: {e:#}"); }`. The same pattern is used for agents and the session.
- **What goes wrong:** with disk full, or SQLite busy beyond 5 s (more likely with B7), `task_complete` returns Done while the database says Working. A restart then re-runs the work or loses its result.
- **Fix:**
  - Persist first, then mutate and publish.
  - Return an error to the caller.
  - Serialize writers, so an older snapshot cannot overwrite a newer one.
- **Test:** inject an SQLite `ABORT` trigger on `tasks`: the RPC returns an error, and the in-memory state equals the database.

**B9. Killing a PTY agent leaves descendants running.** *(R3)*

- **Where:** `crates/crew/src/process/pty.rs:373-385`. `if !proc.alive.load(…) { return Ok(()); }` runs before and during the TERM grace period, ahead of `signal_group(pid, libc::SIGKILL)?;`.
- **What goes wrong:**
  - The leader exits on TERM while a child that ignores TERM or HUP (a dev server, an MCP server, a build) keeps running.
  - That child keeps editing the worktree after the agent shows as Stopped, or alongside its restarted replacement.
  - Children in other process groups are never signalled.
- **Fix:** track the process group or session (or a cgroup) independently of the leader. Always finish the TERM→KILL sequence for the group, and reap.
- **Test:** spawn an agent whose child ignores TERM and HUP; after `kill`, no process from that group is alive.

**B10. Simultaneous team loads merge two teams.** *(CORE-04)*

- **Where:** `crates/crew/src/daemon/team.rs:58-75` (the one-team check), then awaits, then `:141` `self.store.clear_session()`.
- **What goes wrong:** two `crew run` calls at once both succeed. The session ends up with one project and both rosters.
- **Fix:** hold a team-lifecycle async lock (or a Loading state) from the check through publishing. Use the same lock for stop and recovery.
- **Test:** two concurrent `LoadTeam` calls: one succeeds, and the other gets a conflict.

### High (fix right after the blockers)

| # | Bug | Where (quoted) | What goes wrong | Fix | Test |
|---|---|---|---|---|---|
| H1 | Escape sequences from agents reach the operator's terminal (tui 1) | `crew-tui/src/views/detail.rs:409` `format!("   ⬡ {} {}", artifact.kind, artifact.value)`; also title `:242`, model `:257`, fallback note `:269`, workdir/session `:306`/`:315`; launcher `:446-460` | An agent completes a task with an artifact containing `\e]52;c;…\a`; opening the inspector writes the operator's clipboard. Under the default `Bypass` policy agents already have a shell, so the main risk is against `Guarded` (sandboxed) agents, plus terminal corruption. | One `printable()` helper applied to every span in `panel_in` and the launcher preview; strip controls at ingestion in the daemon too | Render test: salt every fixture string with ESC/BEL; no buffer cell contains a control character |
| H2 | Codex sidecar socket path can exceed `SUN_LEN` (R6) | `agents/codex.rs:120` `.join(format!("codex-{}.sock", …))` | In a nested workspace the Codex agent cannot start | Hashed short name in `runtime_dir()`, plus a length check | Launch test with a 120-byte workspace path |
| H3 | herdr ignores the resolved pi binary (R7) | `process/herdr.rs:540-548` (`"kind": kind` … no `spec.program`) | `CREW_PI` and a validated pi selection are ignored; herdr starts its own `pi`, which on this host is the wrong tool | Pass the program, or refuse the configuration with an error | Fake-herdr test: the request contains the selected path, or the spawn fails clearly |
| H4 | Screen text containing "usage limit reached" parks the agent (R8) | `limits.rs:537`, `:558` `line.contains("usage limit reached")` | An agent quoting the error, or this very codebase under review, is parked and may switch models | Match the runtime's error block only; exclude prompt and assistant lines | Negative fixtures: a quoted error in a prompt or reply yields `None` |
| H5 | Bridge loses pushes buffered during `Listen` (tui 2) | `crew-core/src/client.rs:111` `self.pushes.push_back(push);` vs `:102-104` | Messages pending at session start are stuck for 20 s, then fall back to terminal typing | `next_frame` drains the buffer first; the daemon sends the reply before starting delivery | Fake server sends `Deliver` before the reply; `next_frame` returns it |
| H6 | `workspace.init` commits the operator's staged work (R5) | `git/mod.rs:137` `.args(["commit", "-q", "--allow-empty", "-m", "Initial commit"])` | Staged WIP is committed under crew's identity; untracked source is missing from every worktree | Refuse a non-empty unborn repository and explain the required first commit | Git test: staged plus untracked files lead to a refusal, with no commit |
| H7 | `crew config` prints secrets passed via `env` (R9) | `endpoints.rs:650` `row(out, "env", &format!("{name}={value}"), *scope);` | `ANTHROPIC_API_KEY` is printed in full in diagnostics | Show names only, or mask | Config test: the report does not contain the value |
| H8 | No protocol negotiation; unknown enum values break clients (tui 4, R13) | `crew-core/src/rpc.rs:19` `PROTOCOL_VERSION: u32 = 1`; `:524` `Push` has no fallback; `limits.rs:89/138/197` `as f32` | After `just link` rebuilds while a team runs, the dashboard reconnects in a loop or never connects; an inf quota breaks snapshots | `#[serde(other)]` on `Push` and status enums; bump and compare the protocol version; reject non-finite quotas | A frame-from-the-future test; a quota round-trip with `1e300` |
| H9 | Concurrent `agent_add` with the same id both succeed (CORE-09) | `daemon/supervisor.rs:169` (check) … `:152` `model.agents.push(agent.clone());` | Duplicate agents share token and config paths | Reserve the id under the model lock before any await; roll back on failure | Two concurrent adds: one conflict |

### Medium and low

| # | Bug | Where | Fix and test |
|---|---|---|---|
| M1 | Long subagent ids leave a Spawning orphan (CORE-12) | `daemon/subagents.rs:156` `id: format!("{id}-task"),` | Validate the agent and task pair before side effects. Test: a 28-character id leaves no agent behind. |
| M2 | A never-started blocked task can be completed (CORE-10) | `daemon/orchestrate.rs:489` `… \| TaskStatus::Blocked => {}` | Allow Blocked only when `started_at` is set. Test: completing a never-started blocked task fails. |
| M3 | An agent named `integration` blocks `crew integrate` (R10, tui 11) | `crew-core/src/config.rs:684`; `git/mod.rs:22` | Reserve the id, or use a separate branch namespace. Test: validation rejects `integration`. |
| M4 | pi probe can exceed its deadline (R11) | `agents/pi.rs:145` `Ok(Some(_)) => break` then `reader.join()` | One deadline covering exit and draining; kill the group. Test: a fake pi with a stdout-holding child returns within the limit. |
| M5 | Message and question boxes scroll by characters, not lines (tui 7) | `crew-tui/src/input.rs:580`, `:752` `body.chars().count()` | Use `message::line_count`. Test: End then ↑ moves the view. |
| M6 | Quadratic input rendering (tui 8) | `views/compose.rs:131`, `launcher.rs:496` | Walk back from the cursor. Test or benchmark: 8000 characters render in under 2 ms. |
| M7 | Config limits unvalidated (tui 10) | `crew-core/src/config.rs:638` | Range checks naming the key. Test: `idle_nudge_secs: 0` is rejected. |
| M8 | Socket-length unit test depends on the environment (baseline 2) | `crew-core/src/paths.rs:274-281` | Fix the runtime dir in the test. A production guard is under risks. |

---

## 6. Risks (not reproduced as bugs), ranked

| Rank | Kind | Risk | Where | Suggested action |
|---|---|---|---|---|
| 1 | process lifecycle | A hung git hook or helper holds the global `git_lock` indefinitely; git output is unbounded (R12) | `git/mod.rs:50-54` `.output()`; `daemon/gitops.rs:39-42` | Bounded git subprocesses with group kill; report timeouts |
| 2 | concurrency | Most task and agent mutators do not share `plan_lock`; several validate a clone, then write unconditionally (core "riskiest paths") | `daemon/orchestrate.rs`, `daemon/mod.rs:381` | Route transitions through one serialized path (also fixes B5, B8, H9) |
| 3 | persistence | No retention for messages, events or logs; history grows without bound over long sessions | `persistence/`, `message_bus/` | Retention or compaction policy |
| 4 | security | The `/tmp/crew-<uid>` fallback follows symlinks when `XDG_RUNTIME_DIR` is unset (tui 6) | `crew-core/src/paths.rs:67-74`, `:103`, `:123` | `symlink_metadata`, owner and mode check |
| 5 | robustness | `socket_path` has no `sun_path` guard (baseline 2) | `crew-core/src/paths.rs:110` | Length check with a fallback to a short dir |
| 6 | UX/correctness | The 3 s TUI timeout reports kill, restart or set-model as failed even when they succeed; a single requester thread (tui 5) | `crew-tui/src/net.rs:29` | Per-command timeouts; long calls off the thread |
| 7 | robustness | MCP bridge reuses its client after a timeout; disconnects are detected by matching error text (tui 14) | `crew/src/mcp/mod.rs:83`; `crew-core/src/client.rs:215-236` | Poison the client after a timeout; `ClientError::is_disconnect()` |
| 8 | security | Bracketed paste can be broken out of (tui 9) | `crew-tui/src/input.rs:148` | Strip `\x1b[201~` |
| 9 | operational | `~/.local/bin/crew` links to `target/release/crew` in this checkout, so any release build replaces a running team's binary; combined with H8 this is how the dashboard breaks | `justfile` `link` | Install by copy, or version the link |
| 10 | dependencies | `serde_yaml 0.9.34+deprecated` (unmaintained); no `cargo audit`; MSRV 1.82 unverified (only 1.93.1 tested); toolchain unpinned | `Cargo.toml`, `shell.nix` | Run audit; migrate YAML; pin a toolchain or test the MSRV in CI |
| 11 | test flakiness | Fixed sleeps in `tests/team.rs` (e.g. `:156`, `:1470`, `:2102`, `:2839`) | — | Poll for conditions; keep negative waits short and documented |
| 12 | terminal hygiene | SIGTERM, SIGHUP or SIGQUIT leave the terminal raw; `Push::Shutdown` shows as "reconnecting…" forever (tui 15) | `crew-tui/src/lib.rs:193-247`, `net.rs:87` | Signal handler that restores the terminal; exit on Shutdown |

---

## 7. Rejected findings

- **"A worker marked Done is never given new work again"** (rev-core, early). Rejected: `AgentStatus::is_terminal` is `Failed | Stopped` only (`crew-core/src/types.rs:84-86`), so a live Done worker stays runnable. rev-core withdrew it. The narrow true case is a *retired* subagent, which is B4c.
- **baseline 6, "a bare or mistyped word (`crew statuss`) is sent to a running team as an objective".** Rejected: plain `crew` with a single word fails as an unknown command (`crew/src/cli/mod.rs:485-489`, `split_whitespace().count() < 2`), and `/command` text is refused (`ops.rs:232-241`). Only text of two or more words is forwarded, which is the intended operator behaviour.
- **The baseline agent's accidental `crew "style list"` as evidence that the agent guard fails.** Rejected as evidence: that shell had unset `CREW_IDENTITY` itself, for test isolation. The real gaps are B3b and B3c.
- **"`.crew` is never excluded"** (an early phrasing). Narrowed: it *is* excluded in worktree mode (`git/mod.rs:153` `exclude_state_dir`). B1 is limited to shared mode and auto mode without commits.

---

## 8. Cleanup themes, highest value first

1. **Formatting.** 72 files are unformatted. One standalone `cargo fmt --all` commit before anything else; then add `cargo fmt --all --check` to `just lint`, which currently runs only clippy.
2. **One transition path for task and agent state.** The blockers B4, B5, B6 and B8 and the high bug H9 share a cause: checks and writes are scattered across `orchestrate.rs`, `supervisor.rs` and `subagents.rs`, and persistence happens after publishing. Fold them into a single serialized "transition" function that does check, persist, then publish. This is the highest-value structural change, but do it *after* the bug fixes have their regression tests.
3. **Oversized modules.**
   - `crew-tui/src/input.rs` (1,992 lines), `crew/src/daemon/supervisor.rs` (1,735), `crew/src/cli/ops.rs` (1,632), `crew/src/limits.rs` (1,213), `crew/tests/team.rs` (3,042).
   - Suggested splits:
     - `ops.rs`: setup/launch/discovery vs report rendering.
     - `limits.rs`: external-format parsing vs filesystem lookup.
     - `supervisor.rs`: spawn vs signals vs recovery.
4. **Duplication in crew-tui.**
   - The launcher re-implements the compose editor: `launcher.rs:100-290` duplicates `input.rs` `byte_at`/`insert`/`edit`, and `render_objective` duplicates `views/compose.rs::input_line`.
   - There are three sanitizers: `util::one_line`, `views/detail.rs:532 sanitize`, `launcher.rs:290 sanitize`.
   - One line editor and one `printable()` also fix H1 and M6 in one place.
5. **Dead code.** The lead confirmed each of these is referenced only at its definition:
   - `crew-core/src/config.rs:744 parse_key_value`
   - `crew-tui/src/layout.rs:9-10 AGENT_H_CONTEXT`, `CENTER_H_CONTEXT`
   - `crew-tui/src/widgets/canvas.rs:36 put_cell`
6. **Stale or misplaced comments.**
   - `crew-core/src/config.rs:645`: the doc for `validate_agent_spec` sits on `MAX_FALLBACK_MODELS`.
   - `crew-tui/src/net.rs:111-115`: two stacked doc comments.
   - `crew-core/src/types.rs:76`: `impl Default for MessageType` placed before the type.
7. **Error matching on text.** `mcp/mod.rs:83 looks_disconnected` should become typed `ClientError` predicates.
8. **Public surface.** crew-tui exports almost every module `pub` (`lib.rs:9-21`) only so tests can reach them.

---

## 9. Deliberately left, and test-coverage gaps

**Not done, on purpose.**

- **No code was changed:** the objective was review only.
- **Not run:**
  - real-CLI tests (`--ignored`), because they spend tokens and touch user config
  - herdr against a real herdr session, to stay away from the live team's herdr
  - `just link`/`install`/`demo`, which would overwrite the live `crew` binary
  - `cargo audit`/`deny`, which are not installed
- **Not reviewed in depth:** the operator's uncommitted edits made during the review (orchestrator status-message batching in `daemon/delivery.rs`, prompt wording). Only their effect on these findings was checked: none was fixed.
- **Not decided:** the operator trust boundary (B3). How strong the operator credential should be, and whether to sandbox agents, is a design decision for the operator.

**Test-coverage gaps.** Every blocker lives in one of these:

- concurrent RPCs: daemon start, team load, agent add
- reviewer unavailability, and review rejection while the author is busy
- storage failure injection
- process-tree teardown
- **shared-checkout mode**
- **the herdr backend**: only helper parsing is tested, with no fake-server lifecycle tests
- Codex sidecar environment and socket
- protocol compatibility between builds
- `crew-core/src/client.rs`, which has no unit tests
- `paths::discover` and `list_sessions`
- control characters in rendered output
- TUI terminal setup and restore (only an ignored test)

---

## 10. Next steps

### Step 0: repository hygiene, before anything else

1. **Make the initial commit.** `main` has no commits.
   - **Commit:** `Cargo.toml`, `Cargo.lock` (untracked today; `just install` uses `--locked`), `crates/`, `examples/`, `scripts/`, `justfile`, `shell.nix`, `README.md`, `.gitignore`, and `review/` if the reports should be kept.
   - **Exclude, and add to `.gitignore`:**
     - `findings/`, `notes/` and `report.md`, which are leftovers from an unrelated research run
     - `.serena/`
     - the `*-workspace/` directories (`/demo-workspace`, `/objective-workspace`, `/mixed-workspace`; already ignored)
     - `.crew/` (already ignored; it holds live daemon state and tokens)
     - `/target` (already ignored)
2. **Make a standalone formatting commit**: `cargo fmt --all` and nothing else, so later diffs stay reviewable. Then add `fmt --check` to `just lint`.

This order also helps B1: once the repository has a commit, `isolation: auto` uses worktrees, so it no longer falls back to the leaking shared mode.

### Follow-up plan: three areas that don't overlap

Each bug fix is **its own commit with its regression test**, written failing first, from the "Test" entries in section 5. Refactors come only after the fixes they would touch.

**Area A: daemon core.** Owns `crates/crew/src/{daemon,scheduler,tasks,persistence,message_bus,mcp}/` and `crates/crew/tests/team.rs`.

1. B7: single-instance `flock`. Own commit and test.
2. B3a: identity fixed after the first hello, plus an operator credential required in `server.rs`. Own commit and test. This needs the operator's decision on the credential (section 9).
3. B4a–c: reviewer-aware stall detection and recovery, dispatch rollback and size check, retired agents not runnable. Three commits, three tests.
4. B5: re-queue the rejected task. Own commit and test.
5. B6: no silent review skip; reject self-review. Own commit and test.
6. B8: persist before publish, with errors returned. Own commit and test.
7. B10, then H9: lifecycle lock, then id reservation. One commit and test each.
8. M1, M2. One commit and test each.
9. Cleanup (after 1–8): one transition path (theme 2), split `supervisor.rs`, move hard-coded test sleeps to condition polling.

**Area B: agent runtime, process, git and CLI.** Owns `crates/crew/src/{agents,process,git,cli}/`, `endpoints.rs`, `limits.rs`, `models.rs`.

1. B1: always exclude `.crew/`, and refuse a whole-tree commit in shared mode, plus the prompt text. Own commit and test. It touches `daemon/team.rs:107` and `daemon/gitops.rs`; agree those two lines with Area A's owner first.
2. B2: remove the herdr raw fallback. Own commit and test.
3. B3b and B3c: CLI subcommands refuse to act as operator under `CREW_IDENTITY`; the Codex sidecar gets the identity. Two commits, two tests.
4. B9: process-group teardown. Own commit and test.
5. H2, H3, H4, H6, H7. One commit and test each.
6. H8 quota part (finite check), M3, M4. One commit and test each.
7. Risk R12: git subprocess deadlines.
8. Cleanup: split `cli/ops.rs` and `limits.rs`; typed error predicates in `mcp/` (agree with Area A, which owns `mcp/`).

**Area C: crew-core library and crew-tui.** Owns `crates/crew-core/` and `crates/crew-tui/`.

1. H1: `printable()` at the widget boundary, plus the control-character render test. Own commit and test.
2. H5: client push buffering. Own commit and the first unit tests for `client.rs`.
3. H8 protocol part: `#[serde(other)]`, version bump and compare, compatibility test. Own commit and test.
4. Risks 4 and 5: `/tmp` fallback hardening, `socket_path` guard. M8: environment-free test.
5. M5, M6, M7, risk 6 (per-command timeouts), risk 8 (paste), risk 12 (signals and Shutdown). One commit each.
6. Cleanup: one line editor and one sanitizer (theme 4), dead code (5), stale comments (6), narrower `pub` surface (8).

**Cross-area protocol changes.**

- `crew-core/src/rpc.rs` (Hello and credential, Push fallback) is owned by Area C. Area A consumes it.
- Land C3 before A2, so both ends change once.

**Before calling it production-ready.**

- Re-run the full baseline.
- Add CI running fmt, clippy and tests, plus `cargo audit`.
- Run the ignored real-CLI test.
- Do one supervised end-to-end run on the herdr backend after B2 and H3.
