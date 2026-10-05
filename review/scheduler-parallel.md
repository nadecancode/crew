# Idle roles behind queued work: review of the planning correction (agent: reviewer)

Reviewed: `c2d31596`, `5e639b57` and `8d619d9e` (`scheduler`), against `main` at `15de7cf`.

- `c2d31596` changes the orchestrator's briefing (`crates/crew/src/agents/prompt.rs`), the description of
  `task_create` and of its `depends_on` (`crates/crew/src/mcp/tools.rs`), adds one unit test to
  `crates/crew/src/scheduler/mod.rs` and two daemon tests to `crates/crew/tests/team.rs`, and records the
  investigation in `notes/scheduler-parallel.md`. The dispatcher is not changed.
- `5e639b57` repairs a test fixture that had started to fail with the calendar (below).
- `8d619d9e` adds the limits and the verification to the note.

**Verdict: approved as what it is**, a correction of how the orchestrator is told to plan. It does not
give the operator the overlap they asked for in one case, described below; that is a change to the
runtime and is being made separately (`runtime-overlap`).

## What the operator saw, and what explains it

The operator reported a role (the architect) idle with a task pending while another agent worked, starting
only once that agent "came back". Two rules of the runtime can produce that, and both were intended:

1. **A dependency waits for the whole task, review included** (`scheduler/mod.rs:82-85`: every dependency
   must be `Done`). An orchestrator that sets `depends_on` for phase order holds the dependent role for
   the entire phase and its review, even when it could start from what exists.
2. **A waiting task still occupies its agent** (`tasks/mod.rs:101-108`: `Working`, `Waiting`, and a
   started `Blocked` task). An agent that reports `waiting` on a teammate is not handed its next ready
   task until the teammate answers. While that teammate is busy the scheduler sees progress, so no stall
   is reported; the idle agent is silent.

Own query of the events the daemon kept (2026-10-02 19:08 to 2026-10-05 15:52 UTC, 13,989 events, 108
tasks made ready):

- Rule 2 never bit. Every ready task was dispatched within ten seconds, except one that had no assignee
  for 38 seconds. No stall names an agent "still on" another task.
- Rule 1 did, repeatedly. On 2026-10-04 the architect's `web-design` was created at 06:50:32 and became
  ready only at 07:11:10, the instant the reviewer approved the architect's previous card after three
  rounds: twenty minutes behind someone else's verdict. Other cards sat queued for hours behind whole
  phases (`design-signoff` 689 minutes, `integrate` 674 and 710, the fix slices about 470).
- The architect's own `waiting` reports were for the verdict on a card already in review, which frees
  its author; no ready card of the architect's existed at those moments.

So the recorded cause is rule 1, and the patch addresses rule 1. The task records of those sessions were
cleared, so the exact card the operator watched cannot be named; `notes/scheduler-parallel.md` says so.

## The change

- The briefing no longer says "depends_on wherever one task needs another's output". It says: only where
  the card cannot start without another task's finished, approved output; create independent cards at
  once; split an initial contract from later polish and test preparation from final verification; members
  share intermediate artifacts directly; a dependency waits for the review too.
- The supervision step tells the orchestrator how to read an idle role: queued waits on dependencies,
  ready waits on its owner, and a working, waiting or started-blocked card occupies its owner.
- `depends_on` is described in the tool schema as a completion gate.

Every statement the new text makes about the scheduler is true of the code at this commit. Nothing in it
invites bypassing a review gate.

## Tests

- `independent_preparation_does_not_wait_for_a_whole_implementation_phase` compares the two task graphs.
  It exercises no changed code; it is an executable example of the guidance.
- `new_ready_cards_start_while_an_unrelated_worker_is_held` and
  `independent_cards_run_during_review_while_approval_dependents_wait` run a real daemon with scripted
  agents held by explicit messages, not by timing. They pin what must stay true: a free role starts while
  another is held, an author is free during review, a card that needs the reviewed artifact waits for the
  approval, one card per agent, each card dispatched once.

## The fixture repair (`5e639b57`)

The fake Codex server wrote its session file under a fixed `sessions/2026/10/03`. The daemon looks for it
under the agent's launch date and the days either side, so from 2026-10-05 the file was no longer found
and `compaction_does_not_read_through_symlinks` timed out. The author reproduced that on `main` itself.
The fake now dates the file from its clock, and the two tests find it through the daemon's own lookup
instead of a literal path. The window of a day either side also covers a sandbox whose clock zone differs
from the daemon's. The other fixed dates left in the fake (Claude and pi) are not looked up by date.

## Runs

Own, on a copy of `8d619d9e` built outside `/tmp`: `--test fake_cli` 3 of 3; `--test team cards_` 2 of 2;
`--lib scheduler::tests` 15 of 15. `compaction_does_not_read_through_symlinks` reported ok in 0.00 s,
which means it skipped itself: no sandbox is available where I run. That test's real result is the
author's.

The author's: `cargo test --workspace` 948 passed, 0 failed, 2 ignored; `cargo fmt --check` and `cargo
clippy --workspace --all-targets -D warnings` clean. One web daemon test failed once on a port already
taken and passed on the rerun; no web or port code is touched.

## Left as it is, known

- **Rule 2 is unchanged here.** An agent waiting on a teammate still does not receive its next ready
  card. The new guidance creates more independent cards per role, which makes that case reachable where
  it was not before. The orchestrator has decided the overlap is required, and `runtime-overlap` changes
  the rule; this patch should not be read as delivering it.
- Whether an orchestrator plans better with the new text cannot be tested with scripted agents. This
  session ran under the old briefing with the new practice applied by hand: design, source slices, checks
  and review overlapped, and candidates were reviewed as they arrived.
