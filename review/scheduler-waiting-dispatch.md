# Ready work with parked cards

Card: scheduler-waiting-dispatch. Source checkpoint: 63b77bb1, on reviewed
main c958c233.

A member can now call `agent_status(status: "working", task_id: "next")`
to start an assigned Ready card when it has no executing card and every
dependency is Done. The claim uses the same locked, persist-before-publish
check as automatic dispatch. Foreign and unassigned cards, occupied slots,
and unfinished or missing dependencies are refused before any task, agent,
local state, event, or message changes. A Working or started Blocked card
still occupies the single execution slot.

Explicit start releases the caller's parking-turn hold. It sends the normal
Task dispatch message, with the same renderer, metadata, event and delivery
accounting as an automatic start. The existing status reply remains an Agent
record. This card uses the queued-message option accepted in review
m-771f373f9b5a; it does not add inline dispatch text to that reply. Retrying
the start or applying a stale planner dispatch cannot send the card twice.

The MCP agent_status description changes once in this build to advertise
the explicit Ready-start path. This changes catalog bytes and their
fingerprint, invalidating an existing cached tools block when a member
loads the upgraded catalog. No ongoing wording or runtime catalog churn is
introduced by this card; no cache-benefit claim is made.

The pure planner now receives native turn activity and the parking-turn flag
separately. An active turn does not by itself hold Ready work merely because
the member owns Waiting cards. Only the turn that parked a card must yield
before automatic dispatch. Existing usage-limit, blocked-input and pending
parked-card-cue holds remain separate and retain their precedence. The daemon
previously synthesized those distinctions in its agent view; the planner's
public inputs now express them directly.

Five authenticated daemon Session tests cover normal explicit dispatch,
parked-card preservation and later resume, refusal without state changes,
all dependency states and ownership, retries, and same/different-card races
between explicit and planner claims. The race uses a two-thread runtime and
a barrier. The actual daemon agent-view regression starts only one Ready
card during a non-parking turn. The planner suite includes an active Waiting
owner and independent input/cue holds; the existing parking-turn-yield oracle
is unchanged except for its explicit parking-turn input.

Before-controls temporarily reinstated the previous StatusUpdate gate and
pure takes_work gate while retaining the new tests. They failed at the
expected assertions: explicit Ready start returned "task next has no started
work to report on", and the active Waiting owner produced no Dispatch.
The files were restored byte for byte before validation. These controls are
policy substitutions, not a claim that their entire tree equals the base.

Focused results: authenticated daemon tests 5/5, planner tests 19/19,
existing daemon parking-turn-yield test 1/1. Full workspace: 1,360 passed, zero failed, four ignored; real native gateway
21/21 and policy/inventory 14/14. Strict workspace and native-gateway Clippy,
whole-tree fmt, and the actual Rust 1.82.0 default CLI build/help gate passed.

Review added test-only assertions that an explicit Waiting report on the
parked card and a native turn end preserve that exact card without another
dispatch. The five daemon tests passed with these assertions. The first
Clippy run refused a cloned one-item comparison in that test; replacing it
with from_ref fixed the lint without an allow. Both the initial failure and
final successful run are retained. No production source changed after the
workspace run. Commands, times, counts and log hashes are in
scheduler-waiting-dispatch.json.

Validation uses isolated synthetic workspaces, repository overrides and Crew
identity/socket/launch variables removed from child environments,
`CARGO_INCREMENTAL=0`, and sandbox require mode with bubblewrap and socat.
No operator team, service, installed binary or provider session is changed.

Reviewed main b591744b was forward-merged with b541a0f2 using an append-only
merge. No owned path overlapped or changed. On that merged tree, daemon
Ready-start tests 5/5, planner 19/19, existing parking-turn-yield 1/1,
toolbox unit tests 11/11, real gateway 21/21, and policy/inventory 14/14
passed. Strict workspace and native-gateway Clippy, fmt, and the actual
Rust 1.82.0 CLI gate also passed. The earlier full workspace result is
retained; no unchanged broad suite was repeated.
