# Independent role scheduling

Task: `scheduler-parallel`. Baseline: `main` at `15de7cf0`. Investigated on
2026-10-05 after the operator reported an idle architect with queued work while
another member was working.

## Evidence and limits

`scheduler::plan` resolves dependencies, then dispatches every ready card whose
assigned agent is available. Its busy set contains task owners, not the whole
team. `Daemon::apply_plan` applies the actions in order, but task messages are
stored on the bus and delivered by independent per-agent tasks. It does not wait
for a worker to finish before dispatching another worker.

A read-only inspection of `.crew/crew.db` found these event intervals:

| Card | UTC dispatch | Ready to dispatch |
| --- | --- | --- |
| `multi-design` | 2026-10-04 14:03:26 | 0 ms |
| `revamp-design` | 2026-10-05 03:03:00 | 0 ms |
| `design-budget` | 2026-10-05 04:01:26 | 0 ms |
| `recover-design` | 2026-10-05 15:20:03 | 1 ms |

The independent neutral-theme source cards were dispatched at
2026-10-05 15:21:56 while design and QA cards were still working. These examples
and the code establish existing concurrency; they do not identify the cause of
every past wait. Earlier session task records and messages were cleared, while
the event history was retained. The exact architect card and its dependencies
could not be recovered. Recorded architect waits concern design review verdicts
or final review awaiting QA; a ready card held by another waiting card on that
same author was not established.

Two deliberate constraints can resemble serialization. A dependency waits for
the predecessor to be `Done`, including its configured review. A `Working`,
`Waiting`, or started `Blocked` card occupies its author. A waiting author still
has one task in progress, so calls such as `task_complete` without an id remain
unambiguous. A card in `Review` frees its author for independent work. This
change preserves all these constraints.

## Planning correction

The former orchestration rule asked for `depends_on` wherever one task needed
another's output, without distinguishing initial inputs from an entire phase.
The task-creation schema did not explain that dependency lists are completion
and review gates. Broad phase barriers are a supported explanation for avoidable
idle roles, not a proven diagnosis of the operator's exact historical case.

The orchestration briefing and `task_create` description now ask for independent
cards immediately, dependencies only on required completed inputs, and separate
cards for initial contracts, later polish, preparation, and final verification
when those deliverables can progress independently. Members share intermediate
artifacts directly and retain any required approval. Supervision distinguishes
queued prerequisite waits from ready cards waiting for an available owner.
There is no automatic removal of dependencies or change to the dispatcher.

For example, a `design-prep` card with a phase dependency on `implementation`
stays queued throughout implementation and review. If that preparation uses
inputs already available, its own card can have no such dependency and starts
on the idle designer immediately. A separate `verify` card still depends on both
`implementation` and `design-prep`, and starts only after both are done.

## Reproduction and regression coverage

`independent_preparation_does_not_wait_for_a_whole_implementation_phase` compares
those two graphs and retains the final approval dependency. The scheduler
algorithm passes the test without modification; this is a planning correction,
not a reproduced dispatch defect.

Two real-daemon tests use scripted agents on PTYs and operator messages as
explicit release gates, without timed worker delays or conditional overlap checks:

- `new_ready_cards_start_while_an_unrelated_worker_is_held` adds cards to an idle
  designer after implementation is already running. The first reaches the
  designer before implementation is released; the designer's next card stays
  ready until its current card completes. Final verification waits for every
  prerequisite, and each card is dispatched once.
- `independent_cards_run_during_review_while_approval_dependents_wait` holds a
  reviewer open, starts both the author's next card and an independent design
  preparation card, and completes preparation before approval. A card requiring
  the reviewed artifact remains queued until the reviewer approves it.

Run the focused behavior checks with:

```sh
./scripts/build-env cargo test -p crew --lib scheduler::tests
./scripts/build-env cargo test -p crew --test team cards_
```

When running the full suite from inside a crew agent, clear the ambient agent
identity and launch marker for the test process:

```sh
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH \
  ./scripts/build-env cargo test --workspace
```

The first workspace run inherited this agent's `CREW_LAUNCH`. Three existing
sandbox tests sent manual fake-Codex hooks with that unrelated launch marker;
the daemon correctly dropped them as stale, and those tests timed out waiting
for their session id. The test command above isolates that ambient context.
No production hook or sandbox code was changed for this test-environment issue.
