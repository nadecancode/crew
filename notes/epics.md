# Epics in a long-running crew session

Operator request `w-2d3dcc6b066fa029`: keep each substantial ask and its tasks
together, instead of flattening several workflows into one board. This is the
`epics-model` contract. Source was inspected at `77ed2a23`; this card changes
only documentation. The UI contract is coordinated with `feature-ux-spec`.

An epic groups existing tasks. The hierarchy has exactly two levels: **Epic,
then Task**. The UI continues to call a task a task; there is no separate
story object, nested epic, or new executable scheduler node. Different epics
can run at the same time, and a task can depend on a task in another epic.

## Existing boundaries

- Portable `Task`, `TaskStatus` and events are in
  `crates/crew-protocol/src/types.rs`; `crew-core` reexports them. `TaskSpec`
  is host configuration in `crew-core/src/config.rs`.
- Tasks are ordered JSON documents in `crew.db`, not one column per task
  property (`persistence/sqlite.rs`). `clear_session` clears the current team,
  tasks, messages and metadata, while retaining events.
- `daemon/orchestrate.rs`, `tasks/` and `tasks::active_task` own readiness,
  review, execution, parked cards and task identity. Grouping must not change
  those rules or reorder the scheduler's task vector.
- `SessionComplete` currently stores a session report and asks its decisions;
  it does not complete tasks. The scheduler's `Action::AllDone` separately
  supplies `SessionInfo.completed_at` and the completion notification.
- Native gateway policy permits task reads but denies administrative task
  graph mutations. Epic grouping does not widen that authority.

## Records and ownership

Add portable records in `crew-protocol`, reexported through the existing
`crew-core` paths. Put the single pure aggregate function in
`crew-core/src/epics.rs`, used by the daemon. Clients receive its result and
must not implement competing status rules.

| Record/field | Meaning |
| --- | --- |
| `Task.epic: Option<String>` | Membership in one epic, or no epic. Serde defaults to `None`; omit it when serializing `None`. Task id remains session-wide and independent of the epic id. |
| `Epic.id: String` | Same validation as task ids: `[a-z][a-z0-9_-]{0,31}`. Immutable. Epic and task namespaces are separate; APIs always say which kind an id addresses. |
| `Epic.title: String` | Trimmed, nonempty, at most 80 Unicode scalar values; reject overflow instead of truncating identity-bearing text. |
| `Epic.objective: String` | The work requested, bounded by the existing configured message/card byte limit. It is content, never tool-definition text. |
| `Epic.source_message_id: Option<String>` | Immutable reference to the originating operator message when one exists in the current session. Validate by stored sender/visibility, not by an `m-`/`w-` prefix. Initial team objectives and manually organized work may have no source message. |
| `Epic.created_at: i64` | Server creation time in milliseconds, immutable. Stable stored position breaks equal-time ties. |
| `Epic.updated_at: i64` | Last metadata/member-task change affecting this epic, including progress. Report publication does not pretend that its work changed. |
| `Epic.started_at: Option<i64>` | First observed task start/progress out of initial planning. Retained through retries/reassignment so an epic does not become newly planned again. Unknown imported history stays unknown. |
| `Epic.closed_at: Option<i64>` | When this epic most recently entered the derived `done` state. Cleared on reopening. This is the aggregate transition time, not an invented completion time for an old task. |
| `Epic.revision: u64` | Increases by one per committed board change affecting its metadata, membership or member tasks. A batch affecting several of its tasks counts once. Other epics' changes do not invalidate this revision. |
| `Epic.status: EpicStatus` | Derived `planned`, `active`, `blocked` or `done`, by the rules below. Not accepted in an edit request. |
| `Epic.counts: EpicCounts` | `total` and one integer for each existing `TaskStatus`: queued, ready, working, waiting, blocked, review, done, failed. Their sum equals total. |
| `Epic.report: Option<EpicReport>` | Latest report: summary, report time, the epic revision it covers, and linked operator-decision message ids. Preserve older reports in events. |
| `EpicSummary` | Board row: id/title/source reference, `created_at`, `updated_at`, `started_at`, `closed_at`, revision/status/counts, `last_done_at` (latest known successful task completion), per-agent counts, plus report time and covered revision if a report exists. No objective, report body or task bodies. |
| `EpicBoard` | Ordered `epics: Vec<EpicSummary>`, counts for `no_epic`, derived `focus_epic: Option<String>`, a session generation and a monotonic board revision. It advertises the complete v1 read contract, not just the existence of an SQL table. |

The server stores the editable metadata, timestamps, revisions and reports;
status/counts/focus are projections from that metadata and the task set. It may
cache them, but recomputes them on restore and never accepts a client-written
status or percentage. Use counts, not an average of task progress percentages.
`done` and `failed` remain separate counts even though both are terminal.
Per-agent counts include assigned total, unfinished assigned total and current
review count, in stable agent-id order. An unassigned task still contributes
to the epic total. A removed agent's id remains usable as historical identity;
assignment and review counts must not be added as if they were disjoint tasks.
The header uses agents whose unfinished assigned total is positive (at most
three ids, then `+N`); detail uses assigned total and review count. The board
timestamps support the created/first-started/closed timeline without a detail
fetch. Completed epics display newest `closed_at` first; unknown times come
last, with stored position breaking ties.

`Task.completed_at` continues to mean the task's terminal transition, including
failure. Preserve the existing review gate: submission for review is not task
completion. Clear completion time on retry, as the current code does. New
terminal transitions always have a time; missing historical times remain
unknown and sort after known terminal times, with board position as the tie
breaker. Do not assign the migration time to old completed tasks.

### Exact status and focus rules

Apply these status rules in order:

1. No tasks: `planned`, never automatically done.
2. All tasks terminal (`Done` or `Failed`): `done`.
3. Every remaining non-terminal task is `Blocked`: `blocked`.
4. Any task is Working, Review or Waiting; any task has started; the epic has a
   first-start marker; or any child is already terminal: `active`.
5. Otherwise, only not-yet-started Queued/Ready work remains: `planned`.

Plain Waiting is parked work, often awaiting another member. It is **active**,
not evidence of an operator blocker. There is no distinct operator-wait task
state today, so this feature does not infer one from notes or message text.
An active epic may contain failed or blocked tasks: its counts remain visible.
A terminal epic with one failed task is shown as **done · 1 failed**, not as
successful delivery or as a fifth status. An epic whose last unfinished task
is blocked is blocked even when earlier tasks succeeded.

Focus is the most recently created epic containing a non-terminal task;
otherwise it is the most recently created epic. Equal creation times use
stored position. If there are no epics, focus is `None`. This is a presentation
suggestion only. A user may select a different group locally without mutating
shared state. There is no daemon-wide current-epic pointer, and task creation
never inherits membership from the UI focus. The No epic group stays visible
after open epics and before the completed-epic archive, including when it
contains unfinished work outside the focus epic.

## Creating, filing and editing work

The orchestrator opens an epic for a distinct substantial operator ask before
creating that ask's task cards. It chooses a concise title, records the source
message and uses explicit epic ids on those tasks. Several independent asks
in one message may share that source reference; do not make source id a unique
constraint. Follow-up corrections to the same ask reuse its epic. Informational
questions, status checks and `/btw` conversations do not create epics. An
explicit promotion of a `/btw` answer to real work is a new operator ask and
belongs to the normal workflow, outside the isolated observer channel.

Add `epic: Option<String>` to `TaskSpec`. Omission creates a task in **No epic**
for compatibility; it never guesses from the agent, its latest parked card, the
session objective or a global focus. Successful creation returns the assigned
epic or explicitly says No epic. The orchestrator briefing tells it to supply
the field for newly planned work. Files with the new field fail clearly on
old binaries because `TaskSpec` already denies unknown fields.

Team files may declare `epics: Vec<EpicSpec>` (default empty), with id, title
and objective, and refer to them from task specs. These initial declarations
have no source message. Validate every reference before clearing/loading a
team; create the epic records before their tasks. A team file without these
fields remains valid. Do not invent an epic from old objective text or try to
classify historical tasks with a model.
Live epics are session data, not founding-team configuration. The reusable
roster/style save path must not copy them into a future run merely because it
saves the current members; explicit team-file epics remain that file's input.

A task may be moved to another epic or cleared to No epic by the operator or
orchestrator. Membership is independent of execution: moving a Working,
Waiting or Review task changes no assignee, reviewer, dependency, execution
slot, timer, wakeup identity, progress or result. Update source and destination
counts/revisions/report freshness together. There is no cascading move of
other tasks merely because their agents have a parent relationship.
Membership-only changes publish board state/events without waking the assignee
or reviewer to reread a task. Epic metadata edits likewise do not rewrite task
requirements; a changed work contract uses the existing task-edit/notification
path. A terminal task may be refiled, but a combined title/description edit
still respects the existing task-edit restrictions and fails atomically.

For `agent_spawn`, capture the parent's existing active-task identity at the
accepted spawn request and copy that task's epic into the child task. Persist
that selection through asynchronous launch; do not read whichever task the
parent happens to hold later. A parent with no active task gives No epic. Use
the current executing/most-recent-started-Waiting rule, not a new epic-based
selector. Other internal task factories carry an explicit epic where their
request provides one, otherwise No epic. Tests cover all factories, not just
MCP `task_create`.

There is no epic delete, cancel, pause, priority or dependency operation in
v1. Collapsing a group is a view preference. Removing an epic must not become
an accidental way to delete its tasks or bypass review.

## Persistence and migration

Keep the existing ordered task JSON table. Add `epics` as ordered JSON
metadata rows, plus a `task_epics` membership table. Membership is authoritative
in that table; the portable `Task.epic` is hydrated from it when loading tasks.
The JSON copy may retain the field for inspection, but cannot override the
membership table. This avoids losing membership when an older binary reads
and rewrites a task using its older Rust struct.

Scope both new tables by an epic-board generation. Store the current
generation and board revision in separate `meta` keys, not only as new fields
of the existing session JSON (an older writer would drop those fields).
Use `(generation, id)` for epic keys and `(generation, task_id)` for membership
keys; validate references to an existing current-generation task and epic.
No membership row represents No epic. The ordinary task id remains unchanged.

On first opening an old database, add the tables and generation metadata in a
transaction. Existing task rows get no membership rows and therefore appear
under No epic. Do not rewrite task ids, statuses, timestamps, dependency edges,
results or events. The migration is repeatable. Missing/null JSON epic fields
are valid; unknown referenced epic ids in the authoritative membership table
are a data-integrity error, not a silently invented group.

Expose one transactional store operation for a board change. Task writes and
membership changes, affected epic timestamps/revisions/report invalidations,
and durable change/notification records commit together. Extend the existing
message-with-task transaction seam rather than making it a bypass. Compute
against the post-mutation model under the existing model lock, commit, then
publish memory and pushes. A failed store write leaves the visible board and
its task scheduling inputs unchanged. Retry, review verdict, failure, dispatch,
parking/resumption, automatic task creation and reassignment must all use this
path; an epic's completion cannot depend on which API changed its last task.

A new `clear_session` clears current epic records, memberships and metadata
alongside the current tasks, in its existing transaction. Events remain. Every
epic event therefore carries the generation as well as the epic id; future
sessions may reuse a short id without rewriting historical meaning.

**Compatibility floor:** an older binary may open this database and ignore the
new tables without crashing or deleting those tables. It can still rewrite
old task JSON; the overlay preserves membership on the next new-binary load.
This is read compatibility, **not supported downgrade operation**. In
particular, an old `clear_session` removes the separate metadata but leaves
unknown tables. On a later upgrade, create a new generation and leave those
orphaned old-generation rows untouched; never attach them to reused task ids.
Report that boundary in the migration log. Restoring old summaries/revisions
or task history after operating an older binary is not guaranteed.

## Tools and RPC

All definitions are static per build/role. Epic ids, titles, objective text and
counts occur only in tool arguments/results and live snapshots, never in a
schema enum, tool description or standing prompt.

| MCP tool | Contract |
| --- | --- |
| `epic_create(id, title, objective, source_message_id?)` | Operator/orchestrator. Validate source, limits and uniqueness. An exact retry of the same id/content returns the existing epic; a conflicting reuse is an error. Empty epic is planned. |
| `epic_edit(id, title?, objective?, expected_revision?)` | Operator/orchestrator. At least one editable field; id/source/status/times cannot be edited. Optional revision protects against overwriting a newer edit. Return the full current epic. |
| `epics_list()` | Read-only, same workspace visibility as the task board. Returns `EpicBoard`, including counts/focus/No epic. |
| `epic_get(id)` | Read-only epic detail and report, with task ids in board order. Full task cards remain available via `task_get`. Unknown id is an error. |
| `task_create(..., epic?)` | Existing tool plus optional validated membership. Omitted means No epic. Unknown epic is an error before any task is saved or dispatched. |
| `task_edit(id, title?, description?, epic?)` | Missing epic means keep; a string means move; explicit JSON null means No epic. Apply all supplied changes atomically, with existing privilege and task-edit constraints. Do not implement missing/null with a lossy `Option<String>` patch. |
| `tasks_list(epic?)` | Missing means all tasks; string means that epic; explicit null means No epic. Preserve each subset's board order. Unknown epic is an error, not an unfiltered fallback. Default retains all-task semantics. |
| `session_complete(summary, decisions?, epic?, expected_revision?)` | Missing epic preserves the existing global-report tool; expected_revision then must also be absent. A string records the scoped epic report through a distinct RPC described below. Explicit null is rejected: No epic is a group, not a finalizable epic. |

Add `EpicCreate`, `EpicEdit`, `EpicsList`, `EpicGet`, `TasksQuery`,
`TaskRevise` and `EpicComplete` to `crew-core::rpc::Call`. Keep existing
`TasksList`, `TaskEdit` and `SessionComplete` wire methods intact. MCP routes
filtered reads to `TasksQuery`, edits carrying membership to `TaskRevise`, and
scoped completion to `EpicComplete`. The new methods use typed filters/patches
(All / Id / No epic; Keep / Set / Clear), not a string sentinel that could
collide with an id. `TaskRevise` handles title/description/membership together.

This method separation is a compatibility requirement: an old daemon can
ignore a new optional field on `SessionComplete` and otherwise record a global
report. An unknown `epic_complete` method instead fails without that side
effect. The same rule prevents an old task-edit method from silently ignoring
a requested move. An old server must never return apparent success for an
unsupported scoped mutation. New clients gate the feature on the full
`EpicBoard` capability and surface unsupported responses rather than falling
back to an unscoped mutation.

Existing no-epic calls preserve their old behavior. `TaskSpec`'s optional epic
field is skipped when absent, so legacy `TaskCreate` encoding is unchanged.
Serialize every explicitly assigned epic, and expect an old server to reject
that unknown TaskSpec field rather than dropping it.

### Per-epic reports and session completion

`EpicComplete` does not force task statuses or close an unfinished epic. It
requires at least one task and every child terminal. Review, Waiting, Blocked,
Ready and Queued all prevent it. Terminal failures are accepted for an honest
report and remain visible as failures. Empty epics cannot be finalized; a real
ask needs at least one task, even if that task is a research/report result.

Store the summary against the current epic revision. Decisions use the existing
operator-question mechanism, tagged with epic id/generation and persisted with
the report before delivery. An identical retry at the same revision returns
that report and the same decision ids; it never asks the questions twice.
Validate every decision before committing. A concurrent retry, new task or move
must either precede the report transaction or make its revision check fail.
Reporting advances the board revision but not the epic's work revision, so it
does not immediately make its own report stale. Scoped completion may supply
`expected_revision`; a mismatch refuses the whole operation. The daemon still
validates terminal state under the transaction when that optional check is
absent.

New work in a closed epic reopens it, clears `closed_at`, and makes the prior
report stale; retain the report with its covered revision instead of presenting
it as the current verdict. Metadata/task edits and membership moves likewise
make an older report stale when they change that epic's revision. A change to
another epic does not. Decision replies remain associated with their originating
epic, even if the user is viewing a different group.
The detail can show the latest summary, report time and decision links with
`covers revision R · N changes since`, where N is current minus covered work
revision in the same generation. The summary row's report time/covered
revision suffices for an available/stale marker without loading the report
body. N counts committed changes as defined above, not tasks or operator asks.

Closing/reporting one epic must not set the whole session's completion time,
stop agents, satisfy another task's dependencies or suppress work in another
epic. Keep global `session_complete` as the existing whole-session report
operation; it still does not complete tasks. At the daemon notification layer,
announce an epic's terminal transition once per revision/closure. When it
coincides with global AllDone, coalesce the notice with that existing notice
instead of starting two orchestrator turns. A newly created empty epic is
unfinished planning: suppress an automatic claim that all asks are finished
while it remains empty. This affects reporting, not the task planner's
eligibility, ordering or slot decisions.

## Events, wire format and clients

Use `epic.created`, `epic.updated`, `epic.closed`, `epic.reopened`,
`epic.reported` and `task.epic_changed`. Include generation, epic id, revision
and the relevant before/after values in `Event.data`; task events capture their
epic association at event time. A move records both old and new ids, including
null. Do not later join historical usage/events to a task's current epic and
call that historical attribution. The existing Event envelope and SQL event
columns need not change.

The new read contract is additive within protocol v6:

- `Task.epic` defaults to None and is omitted when absent.
- `Snapshot.epics: Option<EpicBoard>` defaults to None. None means the peer
  does not supply epic state; an empty board means support with no epics.
- Add `Push::EpicBoardChanged { board, tasks }`, carrying the resulting board
  and task upserts from the same committed change. Epic-only edits have an
  empty task list. A new client applies that frame atomically.
- Keep legacy `Push::Task` messages for old clients. A client that received a
  supported EpicBoard snapshot uses the grouped frame as its task-update
  authority and ignores the redundant legacy task frame. Every task mutation
  must emit the grouped frame, not just membership edits. Old clients ignore
  the unknown push and continue consuming Task.
- Generation and revision fence stale/duplicate frames. A new generation or
  detected gap requires a fresh snapshot under the existing resync path.
  Snapshot/subscribe ordering and native attempt/workspace fences still apply.

This intentionally duplicates task upserts on the wire during compatibility;
it does not duplicate the full task set on every push. Do not ship the support
marker until every task mutation path and gateway forwarder satisfies this
contract. The board contains epic summaries, not copies of all completed task
bodies. Keep existing frame/record limits, and test a large completed board.
If a deployed decoder cannot ignore the new push as specified, that client
must be updated or use the legacy view; do not claim additive compatibility
without a serialized fixture against its decoder.

Update `crew-client` snapshot state, scoped reduction and `TasksData` to carry
EpicBoard and Task.epic into projections. Task detail should retain epic
identity while filtering or reconnecting. Clients display the server counts,
not counts computed from a filtered/partially visible list. A feature-unaware
server still gives the existing flat task view; it is not falsely described
as supporting No epic grouping.

Full epic detail is loaded on demand via `EpicGet`, with a typed shared-client
resource and `ViewQuery::EpicDetail`/`ViewPayload::EpicDetail`, following the
existing task-card resource pattern. The FFI exposes that same typed record.
Do not repeat objective/report bodies in every board update. A source-message
link still goes through authorized `MessageGet`; an epic does not grant access
to an otherwise private message, and an unavailable source is shown as such.

Web, TUI, GTK and Apple consume the same status/count/focus facts. Canonical
epic data is ordered by creation/stored position and task data by board order.
The UI projection from `feature-ux-spec` groups open epics first, then No epic,
then the completed-epic archive. Within each group it orders unfinished tasks
by status with board-order ties, folds successful Done tasks by completion
time, and keeps Failed tasks visible. None of those presentation operations
reorders the scheduler's task vector or removes tasks from state. A completed
epic containing failures retains its failure count even when collapsed. UI
wording, selection, navigation and dense rendering belong to that design.
Cross-epic edges are
labeled from each task's epic id and the board's epic title; their endpoints
remain ordinary task ids.

Update UniFFI remote records/converters and generate Swift/header/module-map
outputs from the same pinned generator. Changed record layout is not binary
compatible with an old linked binding library: rebuild/package the binding and
apps together. An old app with its own old library may still use the additive
v6 daemon wire protocol. Preserve the Linux tests, generator-freshness checks
and the operator's existing Apple execution boundary; source generation alone
does not establish an Apple build/device result.

New epic list/detail/query reads may use the native gateway's existing Read
scope for that workspace. Add them explicitly to its exhaustive method policy,
channel checks and allowed-action list. Epic create/edit/complete and TaskRevise
remain administrative mutations denied to scoped native devices, just as
TaskCreate/TaskEdit/SessionComplete are now. Do not infer permission from an
epic title, group selection, or its source message. Local operator/orchestrator
authority remains checked by daemon handlers; ordinary members can read groups
but cannot create or move them through these administrative tools.

## Prompt and scheduler contract

The orchestrator's standing instructions gain one static rule, once per build:
open an epic for a distinct substantive ask, reuse it for corrections, and
supply its id on the task cards; use per-epic reports while other asks continue.
The initial task/dispatch, task card and tool result can name the epic. Never
render current epic names, objectives, counts, selected group or the accumulated
operator-ask history into a standing prompt or dynamic MCP schema. This follows
[prompt-cache Design](prompt-cache.md#design), including the narrowed D3 gate.

Adding, renaming, reporting or selecting an epic must not change task readiness,
DAG acyclicity, reviewer checks, least-busy assignment, parked-task resumption,
id-less active-task selection or agent execution slots. The pure planner sees
the same ordered task graph. Cross-epic prerequisites are neither blocked by
an epic label nor automatically satisfied when an epic is done. A terminal
failed task still fails the existing dependency's Done requirement.

## Acceptance

| ID | Case | Required result |
| --- | --- | --- |
| E01 | Old database and old team file | All existing tasks load under No epic with unchanged ids/order/status/times/dependencies/results. Migration repeated safely. No inferred historical grouping. |
| E02 | Two asks, concurrent work | Two epics, tasks explicitly filed; independent ready tasks dispatch as before. Completing one epic does not finish or pause the other. |
| E03 | Status table | Empty planned; Queued/Ready-only unstarted planned; working/review/waiting active; all unfinished Blocked blocked; mixed working+blocked active; Done+Failed terminal done with failure count. Counts sum exactly. |
| E04 | Retry/reopen | New work or retry clears closed_at, preserves first-start history, marks old report stale, and restores normal task readiness. Review submission is never terminal. |
| E05 | Move/clear | Move Working/Waiting/Review/terminal tasks, and clear to No epic. Only membership/epic facts change; old/new aggregates and report freshness commit together. Unknown epic and rejected combined edit have no partial effect. |
| E06 | Cross-epic dependencies | Existing cycle/missing-dependency validation and dispatch trace unchanged; a failed prerequisite does not become satisfied by epic done. UI retains the external-edge label. |
| E07 | Provenance | Existing operator source accepted; nonexistent/non-operator/invisible source refused without disclosing hidden contents. Two epics may cite one source. Id/source immutable on edit. |
| E08 | Scoped completion | Empty or unfinished epic refused. Terminal failures remain explicit. Same-revision retry creates one report/question set. Other epics/session fields untouched. Global AllDone notice coalesces; empty planned asks are not announced finished. |
| E09 | Mutation failure/crash | Inject store failures and crashes around membership/task/report writes and notification publication. Reload gives a coherent committed board; no phantom group, duplicate question, lost transition or changed scheduler input on failure. |
| E10 | Wire compatibility | Old Task/Snapshot decode with no new fields; absent Task.epic preserves old bytes. Old client ignores EpicBoardChanged and processes Task; new client atomically applies grouped updates and rejects stale generation/revision. Missing support disables scoped writes. Old daemon rejects epic_complete/TaskRevise rather than succeeding unscoped. |
| E11 | Every task factory/transition | Bootstrap/config tasks, task_create, agent initial work and subagent work carry explicit/captured membership or No epic. Subagent async launch cannot pick up a parent's later task. Dispatch, waiting, retry, review and failure all refresh the board. |
| E12 | Read-only clients and gateway | All four clients show identical counts/status/focus; scoped Read sees only its workspace and cannot mutate epics with Control or Terminal. Method/channel inventory remains exhaustive. |
| E13 | Dense finished board | At least 500 terminal tasks across multiple epics plus active/No epic work: terminal groups collapse, selection/search/detail still reach every task, counts are global and failure labels stay visible. Exercise existing wire/DOM/accessibility budgets rather than raising them implicitly. |
| E14 | Cache stability | Creating/editing/reporting multiple epics leaves standing-prompt bytes and MCP definition order/bytes identical within a build. Only tail messages, results and live state change. `/btw` creates no epic. |
| E15 | Restore and older binary | New daemon restart preserves membership/reports/revisions. Old binary opens/ignores added tables without crashing/deleting them. Old task JSON rewrite does not erase overlay; old clear-session boundary cannot attach stale membership to reused ids on a later upgrade. No broader downgrade claim. |
| E16 | Bindings and portable clients | Converter/serialization fixtures cover EpicBoard, null membership, statuses and empty/large boards; generated Swift freshness passes; portable/default/MSRV checks pass; Apple build/runtime evidence remains separately stated. |

## Implementation split

Orchestrator assigns owners and creates cards. Do not split task persistence
from aggregate transitions in a way that permits partially updated boards.

| Slice | Files/ownership | Dependencies and checks |
| --- | --- | --- |
| Portable records and aggregate policy | `crew-protocol/src/{types,rpc}.rs`, `crew-core/src/{config,rpc,epics}.rs`, reexports | Freeze records/filters/patches first. Pure E03/E06 fixtures; serde E10; Rust 1.82 and portable target gates. Coordinate protocol.rs ownership with delivery/toolbox work. |
| Persistence and daemon lifecycle | `persistence/{mod,sqlite}.rs`, `daemon/{mod,team,orchestrate,subagents,handlers}.rs`, task factories/planner adapter | Depends on records; owns transactional membership, transition/report revisions, restore, grouped pushes and notification coalescing. E01/E02/E04/E05/E08/E09/E11/E15. Task scheduler remains the existing scheduler. |
| Tools, local CLI and gateway policy | `mcp/tools.rs`, task/epic RPC dispatch, `cli/`, `native/policy.rs` and policy fixtures | Depends on daemon API. Static schemas; keep missing/null distinctions and administrative authorization. E07/E10/E12/E14. Add local `crew epic list/get/create/edit` operations through the same RPCs; no separate storage writer. |
| Shared client and bindings | `crew-client/src/{api,state,presentation}.rs`, transport push forwarding, `crew-client-ffi` records/converters/generated outputs | Depends on portable wire and grouped-update contract. E10/E12/E16, workspace/attempt fences, snapshot/subscribe races. Coordinate generated records with toolbox dashboards. |
| UI grouping | Web task views/protocol, TUI tasks/detail, GTK task views, Apple task lists/detail | Depends on DTO and designer's `feature-ux-spec`. E12/E13; common semantics, no client-owned status derivation or scheduler changes. |
| Integration and documentation | README/team-file examples, shared corpus, old/new compatibility fixtures, large-board and browser/native acceptance | Combine exact reviewed slices. Re-run affected workspace, web, native binding/MSRV gates; preserve existing budgets and platform execution limits. |

Token/cache metrics can later aggregate by epic only when the measurement has
an event-time task association. A currently focused epic or a task's current
membership is not sufficient evidence for assigning historical model usage.
Knowledge-base pages may link to an epic; their live document bodies remain
outside the scheduling graph and standing instructions.
