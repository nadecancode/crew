# Daemon core production-readiness review

Reviewer: `rev-core`. Review only; no product source, tests, Cargo files, or live daemon state were changed. **Changes required before production use.** Twelve findings below have isolated reproductions; several bypass review/dependency gates or strand accepted work.

## Scope and evidence

Reviewed `crates/crew/src/{daemon,scheduler,tasks,message_bus,persistence,mcp}/`, `crates/crew/tests/team.rs`, and `tests/common/mod.rs`, with supporting reads of domain types, configuration, runtime adapters, and process management. The existing suite covers many normal flows; the failures below concern transitions and concurrency outside those tests.

The checkout has no commits and was being edited by another session during this review. File:line references and quoted anchors below refer to a read-only snapshot copied at **2026-10-03 05:18:11 UTC**, available at `/tmp/crew-rev-core/source-at-report/` with a SHA-256 manifest. In particular, `supervisor.rs` line numbers shifted during review. At **05:24:56 UTC**, all 26 snapshotted source/test files were checked against the current checkout and remained byte-identical; none of these findings had been fixed during review.

Reproductions used baseline's independently built binary, not the live team's executable:

```
/tmp/claude-1000/-home-nade-workspaces-personal-crew/db314645-3354-4c24-9753-fbcfd49537bb/scratchpad/target/debug/crew
build mtime: 2026-10-03 05:05:53 UTC
SHA-256: b7191d060919aed052a3a5c2f83dd80bf0caa81909ff3cfe2f5edeef6b15390a
```

This binary predates the concurrent source edits; affected source conditions were rechecked in the snapshot above. Each reproduction launched a foreground daemon in a fresh `/tmp/rcore-*` workspace with separate runtime, state, and config directories, scrubbed inherited crew/herdr identity variables, and used scripted shell agents only. Daemons were stopped in cleanup. No real model CLIs or ignored tests were run. Test drivers are `/tmp/crew-rev-core/repro{,2,3,4}.py`; outputs are `results{,2,3,4}.jsonl` in the same directory. Each finding includes the operation sequence so the evidence remains understandable without those temporary files.

Baseline was coordinated with `baseline`, rather than repeated: builds and clippy passed; the existing workspace suite passed with a short isolated runtime-directory path; formatting failed, and the socket-length unit test failed with a long runtime-directory path. See [baseline.md](baseline.md) for commands and limitations. The twelve reproduction cases are **additional tests that expose failures**, not claims that the existing suite failed.

## Findings, highest value first

### CORE-01 — P1: an agent can authenticate itself as the operator

**Location:** `crates/crew/src/daemon/server.rs:178`; `daemon/handlers.rs:94` and `:117`.

**Kind:** bug. **Confidence:** confirmed by isolated RPC test and source trace.

The operator authentication branch is simply `Role::Operator => Ok(Who::Operator)`. The same-user socket check does not distinguish an agent from the operator. Moreover, every `Hello` replaces the connection's identity through `conn.who = Some(who)`, even after successful worker authentication.

**Reproduction:** authenticate as the ordinary `author` worker. `Snapshot` returns `forbidden`. On that same connection, send `Hello { role: "operator" }`, without a token; it succeeds. `AgentKill { id: "critic" }` then succeeds and returns `stopped`. This requires neither stealing another identity file nor launching a second connection. A new connection claiming operator also works, so rejecting repeated Hello alone is insufficient. The same role check protects model/CLI changes, shutdown, operator answers, and other operator-only methods.

**Why it matters:** daemon-enforced operator restrictions can be bypassed by an agent making ordinary local RPC calls. The README explicitly trusts the OS user, so this is not an escalation across Unix users; it is a failure of the requested agent/operator authority distinction within that user. Runtime/CLI review covers the related operator-client selection and missing identity-marker paths.

**Suggested change:** make authentication immutable after Hello and require an explicit operator capability on first authentication. Keep it out of agent launch environments and prompts, and have agent-context CLI commands authenticate as the agent. A same-UID token file only prevents accidental/tool-level escalation; enforcing a boundary against arbitrary same-UID agent code also requires a broker or OS isolation that actually denies access to that capability.

### CORE-02 — P1: concurrent startup permits two daemons for one workspace

**Location:** `crates/crew/src/daemon/mod.rs:518`, `:532`, `:533`, and `:579`.

**Kind:** bug. **Confidence:** confirmed by isolated concurrent-process test and source trace.

Startup checks `daemon.json`, then unconditionally executes `remove_file(&socket)` before binding. The new daemon's registration is written only after SQLite/model initialization. There is no exclusive workspace ownership lock covering this interval.

**Reproduction:** start two foreground `crew daemon --workspace <same temporary workspace>` processes simultaneously. On the fourth trial, both remained alive and both logs contained `daemon.started`; the test recorded PIDs `3987614` and `3987613`. The second process can unlink the first process's bound socket before the first publishes its registration, then bind that pathname itself.

**Why it matters:** two independent in-memory models and schedulers can share one database and workspace. Existing connections may use one daemon while new clients use the other; either daemon's shutdown can remove the other's discovery/socket pathname. SQLite's writer serialization cannot enforce application-level ownership.

**Suggested change:** acquire and retain an exclusive advisory lock for the canonical workspace before checking registration, removing sockets, opening recovery state, or starting agents. Only its owner may unlink the socket/registration. Add a simultaneous-start regression test; the existing sequential second-start test does not cover this race.

### CORE-03 — P1: successful task completion can be absent from SQLite

**Location:** `crates/crew/src/daemon/mod.rs:381`–`:396`; analogous behavior at `:320`–`:334`, `:284`–`:315`; caller `daemon/orchestrate.rs:517`.

**Kind:** bug. **Confidence:** confirmed by isolated storage-failure injection and source trace.

`update_task` modifies the shared model before saving. Its save failure is only logged:

```rust
if let Err(e) = self.store.save_task(&after) {
    tracing::error!("persisting task {id} failed: {e:#}");
}
self.push(Push::Task(Box::new(after.clone())));
Some(after)
```

**Reproduction:** after dispatching `write`, install a SQLite `BEFORE UPDATE ON tasks` trigger that raises `ABORT` in the isolated database. `TaskComplete(write)` returns `status=done`; reading the database directly shows `status=working`. This exercises a returned database error, without filling disks or modifying the live database.

**Why it matters:** a caller and dashboard observe success and dependencies can be released, but restart restores older work and can execute it again or lose result/artifact records. Agent/session/settings writes follow the same log-and-continue pattern. A successful RPC is not a durability guarantee.

**Suggested change:** make persistent transitions return errors; commit authoritative task/agent/session changes before publishing or acknowledging them, with rollback or a serialized transition mechanism. Where a transition creates a notification, persist it with the transition in a transaction/outbox. Also preserve mutation ordering: releasing the model lock before saving permits concurrent older snapshots to overwrite newer persisted snapshots; that ordering case was traced, not separately stress-tested.

### CORE-04 — P1: simultaneous team loads mix two teams into one session

**Location:** `crates/crew/src/daemon/team.rs:59`–`:77`, `:83`, `:140`–`:162`, and `:208`–`:229`.

**Kind:** bug. **Confidence:** confirmed by isolated concurrent RPC test and source trace.

The one-team check is released before asynchronous preparation: `let model = self.model()` protects only the initial inspection. Both requests can pass it, await workspace preparation, then independently execute `self.store.clear_session()` and replace the model/process backend. Agent creation follows later.

**Reproduction:** issue two `LoadTeam` calls simultaneously, one with project `load-0`/agent `worker-0`, the other with `load-1`/`worker-1`. Both RPCs succeed; both responses report `load-1`. The resulting session is `load-1` but contains **both** workers. This was performed in a new daemon with no previous team.

**Why it matters:** callers can receive success for a team that was not loaded as requested, and agents/tasks can belong to mismatched configuration or process backends. Clearing state after another load has begun also threatens recovery consistency.

**Suggested change:** serialize the full team lifecycle through a dedicated async lock or explicit generation/Loading state, revalidate after preparation, and publish one complete session atomically. Coordinate stop/recovery with the same ownership state. Test simultaneous loads and interruption during preparation.

### CORE-05 — P1: an unavailable configured reviewer silently bypasses review

**Location:** `crates/crew/src/daemon/orchestrate.rs:510`–`:534`.

**Kind:** bug. **Confidence:** confirmed by isolated RPC test and source trace; independently checked by lead.

Completion discards the configured reviewer with:

```rust
.filter(|r| *r != author && model.agent(r).is_some_and(|a| !a.status.is_terminal()))
```

The resulting `None` takes the ordinary completion branch, `t.status = TaskStatus::Done`.

**Reproduction:** dispatch `write` with reviewer `critic`, stop `critic`, then complete `write`. The RPC returns `done` while the task still records `reviewer: "critic"`; nobody approved it. A removed reviewer or author-as-reviewer also loses the gate.

**Why it matters:** review is most needed when a reviewer cannot perform it, yet the scheduler releases dependents and can declare the session complete. No explicit override or skipped-review explanation is required.

**Suggested change:** preserve review-required state when its reviewer is unavailable, notify the coordinator, and require explicit reassignment/waiver by an authorized actor. Reject self-review configurations when creating/loading tasks. Add tests for stopped, failed, removed, and self reviewers, with dependents kept blocked.

### CORE-06 — P1: rejecting a review gives its author two active tasks

**Location:** `crates/crew/src/daemon/orchestrate.rs:748`–`:752` and `:794`; `crates/crew/src/tasks/mod.rs:76` and `:86`; `scheduler/mod.rs:95`–`:99`.

**Kind:** bug. **Confidence:** confirmed by isolated RPC test and source trace; independently checked by lead.

Review frees the author for another task, but rejection directly writes `t.status = TaskStatus::Working` on the previous task without checking the author's current assignment. It also replaces the author's displayed task. `active_task` returns only the first occupying task with `.find(...)`.

**Reproduction:** assign `first` and `second` to `author`, with a reviewer on `first`. Complete `first`, wait until `second` is Working, then reject `first`. Both become Working. An author `TaskComplete` without `task_id`, intended for `second`, selects `first` instead; the isolated test observed exactly that target.

**Why it matters:** implicit progress, completion, commits, crash handling, and reminders can be attached to the wrong work. This violates the scheduler's central one-active-task-per-agent invariant.

**Suggested change:** enqueue rework through the scheduler, retaining review comments/artifacts, or explicitly coordinate preemption before activating it. Make occupancy checks part of the same transition as rejection. Test rejection after dispatch of the author's next task and verify both explicit and implicit operations target the right task.

### CORE-07 — P1: retired subagents remain eligible for new work

**Location:** `crates/crew/src/daemon/subagents.rs:202`–`:226`; `daemon/orchestrate.rs:39`; `scheduler/mod.rs:120`–`:123`; `daemon/delivery.rs:261`–`:263`.

**Kind:** bug. **Confidence:** confirmed by isolated process/RPC test and source trace.

Retirement kills a completed subagent but preserves `status=Done`. Scheduler availability is `runnable: !a.paused && !a.status.is_terminal()`; AgentStatus's terminal predicate deliberately includes only Failed/Stopped, so a retired Done subagent is still eligible. The planner also has no distinction between standing workers and retired subagents. Delivery to the dead process simply returns at `if !procs.is_alive(id) { return; }`.

**Reproduction:** keep `parent` busy, spawn `child` for a short task, wait for child retirement (`Done`, `pid=null`), then create an unassigned task. The new task becomes Working with `assigned_agent=child`; child remains Done with no PID. No delivery path restarts it.

**Why it matters:** accepted work disappears into a dead worker while Working suppresses stall detection. Ordinary live Done workers are correctly reusable; the defect is specifically the absent retired process.

**Suggested change:** represent retirement separately from completion and exclude retired workers from automatic assignment, or explicitly restart them before dispatch. Preserve legitimate scheduling to agents that are still starting. Test new queued work after subagent retirement.

### CORE-08 — P1: restart loses an in-progress reviewer and the scheduler never reports a stall

**Location:** `crates/crew/src/daemon/supervisor.rs:1681`–`:1712` (originally read at `:1644`–`:1675`); `scheduler/mod.rs:151`–`:154`.

**Kind:** bug. **Confidence:** confirmed by isolated daemon-restart test and source trace; independently checked by lead.

Recovery considers an agent needed only when it has assigned nonterminal tasks, orchestrator work, or undelivered messages:

```rust
.any(|t| t.assigned_agent.as_deref() == Some(agent.id.as_str()) && !t.status.is_terminal());
open || (agent.orchestrator && model.session.summary.is_none())
```

Review obligations live in `task.reviewer`, so an already-delivered review request does not keep its reviewer alive. Separately, the planner treats every Review task as progress with `matches!(t.status, TaskStatus::Working | TaskStatus::Review)`, regardless of reviewer availability.

**Reproduction:** submit a task for review; let `critic` acknowledge its review request, without a verdict; kill and restart the isolated PTY daemon. The task stays Review, but `critic` becomes Stopped with no PID. No task failure, retry, or stall transition repairs this state. A reviewer stopped or permanently failed after submission has the same missing recovery/stall handling.

**Why it matters:** the entire dependency chain can hang indefinitely without the stall signal promised to the coordinator.

**Suggested change:** count outstanding review obligations during recovery, reconcile Review tasks whose reviewer is unavailable, and make stall detection depend on a reviewer that can actually progress. Preserve/reissue the durable review request with an idempotent identity. Add restart and permanent-reviewer-failure integration tests.

### CORE-09 — P2: concurrent agent creation accepts the same ID twice

**Location:** `crates/crew/src/daemon/supervisor.rs:168`–`:173`, `:184`, `:218`–`:223`, `:137`–`:152`; analogous reservation gap in `daemon/subagents.rs:56`–`:89` and `:129`.

**Kind:** bug. **Confidence:** confirmed by isolated concurrent RPC test and source trace.

The duplicate-ID and capacity checks happen before awaited work and before the ID is reserved. `create_agent` saves a fresh token and then executes `model.agents.retain(|a| a.id != agent.id)`, replacing a raced record rather than rejecting it.

**Reproduction:** two simultaneous `AgentAdd` calls for `duplicate` both return success. The isolated daemon logs two `agent.added` events; one startup fails with `No such file or directory` during competing setup, while the other continues. Both operations use the same identity/config paths and agent slot. Auto-generated subagent IDs use the same unreserved lookup pattern.

**Why it matters:** callers do not get a reliable uniqueness result; token/settings/process setup can race, and separate requests can exceed the checked capacity. The latter is a source-traced consequence, not separately load-tested.

**Suggested change:** reserve IDs and capacity under the model lock before any await/filesystem work, with a creation generation and rollback on failure. Persist token, agent record, and settings as one logical creation. Reject a second creator instead of replacing its record; cover explicit and generated IDs in parallel tests.

### CORE-10 — P2: a failed dependency's never-started task can be completed

**Location:** `crates/crew/src/daemon/orchestrate.rs:473`–`:489`; `scheduler/mod.rs:54`–`:79`; `tasks/mod.rs:74`–`:81`.

**Kind:** bug. **Confidence:** confirmed by isolated RPC test and source trace.

`complete_task` correctly rejects Queued/Ready tasks but accepts every Blocked task through `TaskStatus::Working | TaskStatus::Waiting | TaskStatus::Blocked => {}`. Elsewhere Blocked means either an agent blocked while working or a task that **never started because a dependency failed**, distinguished by `started_at`.

**Reproduction:** fail prerequisite `dep`, wait for assigned child `child` to become Blocked, then complete `child` by explicit ID as its assigned worker. The result is `Done`, `started_at=null`, while `dep` remains Failed.

**Why it matters:** the public completion API bypasses the dependency gate and can unlock descendants for work that was never authorized to start. This can happen when a worker uses `tasks_list` and supplies a preassigned task ID.

**Suggested change:** permit completion of Blocked only when it was actually started/occupied, and check the transition under the same lock as mutation. Add a negative test for an unstarted dependency-blocked task and a positive test for a started self-blocked task.

### CORE-11 — P2: a rejected dispatch leaves accepted work permanently Working

**Location:** `crates/crew/src/daemon/orchestrate.rs:184`–`:189` and `:221`–`:233`; `:238`–`:255`; `message_bus/mod.rs:139`–`:145`.

**Kind:** bug. **Confidence:** confirmed by isolated RPC test and source trace.

Dispatch sets Working/started_at before sending the task message. If the bus rejects it, `if let Err(e) = self.send_message(message).await` only emits `task.dispatch_failed`; the task remains Working. Task creation does not limit the description or validate the size of its eventual dispatch message.

**Reproduction:** create a task with a 70,000-byte description through RPC. Creation succeeds. The scheduler moves it to Working, but history contains **zero task messages** and the event states `message body is 70175 bytes; the limit is 16384` (the default team limit). It cannot be redispatched in that state. Large dependency result/artifact sections can also inflate an otherwise small description's dispatch.

**Why it matters:** a valid-looking accepted task silently occupies its worker without giving that worker its instructions. Working prevents a scheduler stall report. A crash between the persisted state change and message insertion is the related durability window.

**Suggested change:** persist the dispatch message/outbox and transition together, or leave the task retryable when enqueue fails. Validate or safely shorten generated instructions to the configured bus limit, supplying a way to retrieve the full description, and surface failure to the coordinator. Test both deterministic size rejection and storage/recipient failures.

### CORE-12 — P2: valid subagent IDs can create a permanently Spawning orphan

**Location:** `crates/crew/src/daemon/subagents.rs:85`–`:89`, `:129`–`:163`; related `daemon/supervisor.rs:230`–`:239`.

**Kind:** bug. **Confidence:** confirmed by isolated RPC test and source trace.

Subagent IDs allow up to 32 characters, but the task ID is formed later with `id: format!("{id}-task")`, under the same 32-character limit. The agent is already persisted before task validation; on error execution returns before spawning or rolling back it. Default generation keeps 26 characters of a parent's ID then adds `-s1`, which can also produce a derived task ID longer than allowed.

**Reproduction:** spawn a subagent whose ID is 28 `a` characters. RPC returns `invalid task id 'aaaaaaaaaaaaaaaaaaaaaaaaaaaa-task'`. AgentsList nevertheless contains that agent as Spawning with no PID, and no task exists. This also consumes roster capacity and can make the team's start-in-progress check reject a later load. `AgentAdd` with a task shorthand catches the analogous task error and returns success after dropping the task.

**Suggested change:** allocate valid task IDs independently or budget suffix space, validate the complete agent/task pair before side effects, and roll back a partial creation. Test both maximum explicit IDs and long-parent automatic IDs; assert failure leaves no agent, token, or task record.

## Related findings and limits

- `rev-runtime` owns the shared-workspace `git_commit` exposure of `.crew` state/secrets, terminal/herdr fallback behavior, and CLI/runtime identity-marker handling. Core references: `daemon/team.rs:107` only excludes `.crew` when worktrees are enabled; `daemon/gitops.rs:61` forwards commits after checking only repository-root suitability. See [runtime.md](runtime.md) when available.
- `rev-tui` reproduced client push loss when `Listen` delivery races the RPC reply, and terminal escape injection through raw agent text. Core entry points are `daemon/handlers.rs:299`/`daemon/delivery.rs:99` (delivery begins before the reply is enqueued) and `daemon/orchestrate.rs:500` (artifacts are truncated without control-character sanitization). Those primary findings remain in [core-lib-and-tui.md](core-lib-and-tui.md); CORE-01 is the primary report for operator-role escalation.
- This review did not validate real Claude/Codex/pi behavior or herdr recovery against installed external services. It did inspect daemon-side authorization for model changes: worker calls are denied and the orchestrator is prevented from changing its own model/CLI or another member's CLI; CORE-01 bypasses those otherwise useful checks.
- No finding claims that ordinary Done workers are terminal: `AgentStatus::is_terminal` intentionally excludes Done. CORE-07 depends on the process having actually been retired.

## What is solid

- The scheduler is a pure, readable function with tests for dependencies, one-task occupancy, retry recovery, assignment, and stalls. That makes the missing transition cases straightforward to isolate.
- Message insertion and recipient fan-out are transactional in SQLite; stable client-chosen IDs suppress duplicate insertion/delivery and reject another sender's reuse. Inbox acknowledgments are scoped to the authenticated recipient. The bus uses persisted history as its source of truth.
- The socket has private directory/file permissions and checks the peer UID. RPC input frames are bounded. Agent sender identity and most tool authorization are enforced in the daemon rather than trusted from MCP schemas.
- IDs, git revisions, and process argument handling have explicit validation/seams. Git operations use `spawn_blocking` behind an async serialization lock; the queue helper has a timeout and `kill_on_drop`.
- `tests/common` provides genuinely isolated daemon/PTY integration tests with deterministic scripted agents. Existing tests exercise approval, daemon restart, direct delivery, deduplication, ordinary lifecycle, and access denials. The gaps above can be covered without real model calls.

## Riskiest untested paths

- Concurrent task reassignment/retry/completion/failure while a plan or housekeeping timeout is being applied; most mutators do not share `plan_lock`, and several validate a clone before later unconditional writes.
- Inverted persistence order between concurrent updates, disk-full/locked-database errors across whole transitions, and crash injection between token/agent/settings or task/message writes. The injected task-save failure above is only one representative fault.
- Kill/remove/model-change while an agent is still preparing its launch; process-generation fencing across delayed hooks, startup watchers, and existing authenticated connections.
- First-prompt delivery racing a new socket listener, delayed confirmations after terminal fallback, and message recovery after a consuming Inbox reply is lost. MCP automatically retries some disconnected calls without a general mutation idempotency key.
- Delivery-channel backpressure and disconnected writers, huge pending-mail batches, and indefinitely growing history/events/logs during long sessions. Current persistence has no retention mechanism.
- Real herdr adoption and shutdown, real sidecar startup/failure, simultaneous runtime fallback and operator model changes, and quota/timeout behavior across restarts. These need dedicated integration and fault-injection coverage before reliability claims extend beyond the scripted PTY path.
