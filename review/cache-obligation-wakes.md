# Obligation-aware follow-up prompts

Task: `cache-obligation-wakes`. Design: `notes/prompt-cache.md`, D4.
Implementation: `4717606243894c2c033b5b7c77a10e625b523c40`, based on
`77ed2a238d0689101557a5f9b5bea7b043d7c5f0`.

When an old task dispatch reaches a turn after its card was parked, submitted
for review or completed, crew no longer sends another prompt demanding an
acknowledgement. The check uses the card's current typed status and owner.
An identified answer (`reply_to`) also settles the corresponding question,
message or review request. A formal review request can be settled by its
card's verdict; an independent artifact review remains actionable.

A genuine unanswered question or message still receives the existing single
follow-up, regardless of the worker's parked/review state or wording such as
“FYI”. A Working task remains actionable. A typed changes-requested result
now receives the same check when its rework is executing. Ready rework stays
queued for normal dispatch instead of demanding that the worker abandon its
current card. Unknown task references are treated conservatively as work.

`agent_status(waiting, task_id=...)` on the author's submitted Review card is
an acknowledgement only. It changes neither task nor agent state, writes no
notice, leaves the reviewer in charge, and does not occupy an execution slot.
An explicit acknowledgement preserves a different executing card. Without a
task id, an executing or parked card keeps the existing default targeting;
only a worker with no such card can acknowledge its pending reviews this way.
The operation retains no Waiting self-status that could hide subsequent
changes requested.

The status-message gather policy, turn accounting, reply routing, transcript
format and 90-second/three-attempt idle reminder defaults are unchanged. The
health code only extracts its existing policy behind an explicit clock input
for deterministic boundary tests. Runtime owns the separate D1 delivery and
delayed-transcript race work; this change does not claim to resolve every
false-positive turn classification.

## Deterministic before/after evidence

The same ignored measurement fixture was run against the base implementation
and the candidate. It creates actual isolated daemons and synthetic Claude,
Codex and pi transcript records, then drives `check_turn`. It counts generated
follow-up bus prompts. It does not launch any of those CLIs or a model.

The base was exported from `77ed2a23`, with only the candidate's `cfg(test)`
additions in `turns.rs`. Its production prefix was byte-identical to the base
(hash recorded in the [machine-readable evidence](cache-obligation-wakes-counts.json)).
Both sides ran
`daemon::turns::empty_turn_tests::scripted_obligation_request_counts` with
`--ignored --exact --nocapture`. The fixture also verifies that checking a
turn leaves its serialized transcript unchanged.

All three transcript formats produced the same results:

| Scenario | Before, per runtime | After, per runtime |
| --- | ---: | ---: |
| Working task | 1 | 1 |
| Parked task's old dispatch | 1 | 0 |
| Review card's old dispatch | 1 | 0 |
| Completed card's old dispatch | 1 | 0 |
| Status-only turn | 0 | 0 |
| Answer-only turn | 0 | 0 |
| Genuine unanswered question | 1 | 1 |
| Genuine unanswered message | 1 | 1 |
| Replayed question already answered by id | 1 | 0 |
| Formal review request after verdict | 1 | 0 |
| Changes requested on executing rework | 0 | 1 |

Across 33 cells, targeted duplicate prompts fell from 15 to 0. Three necessary
rework follow-ups were added. Total generated follow-ups fell from 24 to 12;
their message bodies totaled 12,255 bytes before and 6,144 bytes after. Those
are byte counts, not token counts. The full per-runtime cells are in the JSON
evidence.

Provider request counts, cost and cache-hit changes remain unknown. They are
explicitly `null` in the evidence. Per orchestrator decision
`m-562daefe90d1`, paid, matched D4 cost evidence belongs to QA's
`cache-acceptance` subcase. These offline results establish daemon prompt
suppression and preserved obligations, not a measured provider saving.

## Validation

Focused checks passed: 10 turn tests, 3 review acknowledgement tests and 2
deterministic health tests. They cover all three transcript formats, bounded
question/message retry, parked/review/completed cards, another executing card,
an actual changes-requested transition, unchanged serialized prompts, and the
unchanged reminder deadline and retry budget. The measurement fixture passed
on both implementations separately.

Full gates passed on the implementation above:

- `cargo test --workspace --locked`: 1,207 passed, 0 failed, 4 ignored.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean.
- `scripts/check-msrv`: the default CLI built with rustc 1.82.0 and parsed
  its version and command help using the committed lockfile.

Cargo build/test commands used `scripts/build-env`, `CARGO_BUILD_JOBS=2` and
`CARGO_TARGET_DIR=$PWD/target/cw`. The compatibility gate used
`CREW_MSRV_TARGET_DIR=$PWD/target/cw-msrv`. Workspace tests and the compatibility
gate removed inherited `CREW_IDENTITY`, `CREW_SOCKET`, `CREW_LAUNCH`, `GIT_DIR`
and `GIT_WORK_TREE`. Logs live under `target/cache-obligation-wakes/` in the
content workspace: `turns.log`, `review-waiting.log`, `health.log`,
`before-counts.log`, `after-counts.log`, `workspace-final.log`, `clippy.log`
and `msrv.log`.

The first workspace run reached two pre-existing `sandbox_launch` tests that
create Unix sockets beside the test binary. The long target directory exceeded
`SUN_LEN`. Moving the artifacts alone retained old compiled binary paths and
caused the next run to fail at `binary_pin`. The crew package was then rebuilt
at the shorter, owned `target/cw` path. No product or test behavior was changed
to accommodate these environment failures.
