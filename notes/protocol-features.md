# Compatible protocol for epics, asides, Docs and usage metrics

This is the `feature-protocol-plan` contract. It consolidates the approved
[epic](epics.md), [aside](btw.md), [Docs](knowledge-base.md) and
[usage metrics](usage-metrics.md) designs. Source inspected at main
`b4f9a442`; include the reviewed metrics addendum `f6e13265`. It changes no
product code and does not claim that the implementation gates below pass.
Where a feature design calls its wire shape provisional, this note decides
that shape. Its execution, filesystem, privacy, accounting and UI requirements
still apply.

Account pooling remains a separate design: the operator has requested a
broker for personal ChatGPT browser-login accounts in addition to the account
kinds in draft `388eca72`. Metrics reserves nullable account attribution;
this plan neither enables a pool nor settles the broker's auth mechanism.

## Version decision

Keep daemon **protocol v6** and the native gateway's existing authentication
protocol. Do not change signed native `AuthParams`, proof transcripts, grant
tiers, base Hello semantics or the JSON request/response envelope for these
features. Add explicit **post-authentication** feature negotiation and distinct
methods. An old daemon refuses the new negotiation; the client retains legacy
views and offers none of the unnegotiated features.

New feature pushes reach only connections which opted into the matching
version and stream. Existing methods keep their old shapes and meanings on a
legacy connection. This is the additive exception the implementation must
state beside `PROTOCOL_VERSION`: new feature-gated messages are allowed only
when an old peer cannot receive or accidentally invoke their semantics. The
existing blanket comment about new enum variants is not sufficient guidance.

One coordinated base-version bump is required instead if implementation needs
to change an existing unnegotiated meaning, makes an old decoder fail, or
cannot keep a breaking payload out of an old connection. Prove those cases
before choosing a bump; do not independently bump each feature or the native
proof protocol. Optional metadata and new, explicitly negotiated method
families alone do not justify breaking all older clients.

Wire compatibility is separate from the Rust/UniFFI ABI. Updated Swift, header,
module map and native library are packaged together, even though an old app
with its old library can still use a v6 daemon's legacy view.

## Common records and negotiation

Put shared feature types in `crew-protocol/src/features.rs` and the domain
records in `epics.rs`, `asides.rs`, `docs.rs`, `usage_metrics.rs`. Host calls
remain in `crew-core::rpc::Call`; configuration, secret resolution and host IO
never enter the portable crate. Reexport existing public paths rather than
moving them incompatibly.

```text
FeatureVersions = map<string, u32>
FeaturesNegotiate { versions: FeatureVersions }
FeatureSession {
  accepted: FeatureVersions,
  connection_epoch: string,
  workspace_run_id?: WorkspaceRunId,
  unavailable: map<string, bounded reason code>
}
```

The method is `features_negotiate`. V1 feature keys are exactly `epics`,
`asides`, `docs`, `usage_metrics`, each at version `1`. Unknown feature names
are not enabled; unsupported versions are not silently translated. Bound the
request to 16 entries and 32-byte names. The response is a subset of the
client's exact versions, not a grant of authority or a list of arbitrary
backend state.

The caller authenticates by the existing local/web/native route first.
Negotiation runs before the first ordinary Snapshot/Subscribe or feature
operation on that connection. Its first successful version set is fixed;
repeating the same set returns the same accepted versions/connection epoch
and current run metadata. Changing the set requires a new connection, so
representation cannot switch halfway through a stream.

A client sends this one bounded, read-only discovery request. An explicit
unknown-method rejection from an old server means unsupported. Other
well-formed failures make feature discovery unavailable with a reason; they
must not manufacture capabilities. Authentication, grant, framing, timeout
or transport failure still follows normal connection recovery, not a silent
legacy-success path. No feature mutation is retried as a legacy operation.
Old-daemon fixtures must exercise the actual v6 error envelope, including its
existing `invalid` unknown-variant response.

A backend advertises version 1 only when its full method/decoder/authorization
contract is ready. A configured but disabled Docs service can still implement
the version and return its explicit disabled state. API support is not proof
that a GTK/Apple screen exists. Each frontend exposes only its implemented
controls; native Docs and asides UI remain deferred as their designs state.

### Identity, revision and number rules

- `workspace_run_id` is feature-neutral: one opaque durable id for a logical
  workspace session. Epics uses it as `EpicBoard.generation`; metrics uses it
  whether or not epics is enabled. Persist its pointer separately from old
  session JSON, preserve it through process/daemon restarts, replace it only
  at a real session-clear/new-run boundary. Old metadata-clear recovery never
  joins archived rows to reused task/agent ids.
- A feature's revision advances only for its own committed state. The metrics
  revision can advance for a downward correction; it is not a usage count.
  Epic work revision, board revision, Docs index revision and aside revision
  remain separate types. A shared run id does not make their cursors reusable.
- `connection_epoch` is an opaque per-authenticated-connection identity. It
  carries no authority by itself. Docs handles and watch ids are bound to it
  as well as workspace/attempt and feature epoch. A replacement connection
  never inherits its leases merely because it has the same grant.
- Agent incarnation changes when an id is removed/reused, not on ordinary
  process restart. Share that private lifecycle fact between usage bindings
  and aside target capture; never identify old work by today's cwd or agent id
  alone.
- New unbounded aggregate counters, money, counts and revisions use canonical
  unsigned decimal strings on the wire and in wide-integer storage. Parse and
  range-check with `u128`; zero is `"0"`, not null. Reject signs, fractions,
  exponents, leading-zero alternatives and overflow. Clients do not round-trip
  them through JavaScript Number. Existing legacy numeric fields stay intact.
- Bounded limits/enums/indices can remain JSON integers. Millisecond timestamps
  must fit JavaScript's exact-integer range; invalid/missing time stays unknown.
  Ratios include exact numerator/denominator plus a finite display value where
  defined. Unknown, invalid, zero and unavailable are not interchangeable.

Every new mutation referring to a current-run task, epic, member or aside
carries the expected `workspace_run_id`. The daemon checks it under the same
lock/transaction as the mutation. A stale id reused in a later team cannot
receive the old command. CLI/MCP adapters obtain this scope through the
connection; they do not make a model paste a run id from prose. Read-only
metrics can explicitly select an authorized historical run. Docs uses its
own index/source epoch and exact connection; it can read the registered
workspace root before a team has loaded.
Current-run Epic and aside reads also carry the expected run and return it
with their revision/epoch. A stale detail/list request fails `stale_scope`
instead of selecting reused ids. Negotiation and Snapshot discover the
current run; a client must explicitly replace its scope after a run change.

## Method inventory and authority

All method names below are fixed. They are never generated from current epic
names, account labels, file paths or runtime availability. Every new method
must appear in daemon dispatch, `Call::method`, native policy/lane inventory,
codec and negative tests before the feature is advertised.

| Family | Methods | Local caller | Scoped native device |
| --- | --- | --- | --- |
| Discovery | `features_negotiate` | Authenticated operator; agent may negotiate only collaborative epic reads/tools | Workspace/read lane with Read; action lane with Control for its negotiated operations; never index/terminal |
| Epic reads | `epics_list`, `epic_get`, `tasks_query`, `task_get_scoped` | Operator or agent, same task-board visibility | Read, workspace/read lane |
| Epic administration | `epic_create`, `epic_edit`, `task_create_scoped`, `task_revise`, `epic_complete` | Operator or privileged orchestrator | Denied, including Control/Terminal grants |
| Aside reads | `aside_members`, `aside_list`, `aside_get` | Operator only | Read, workspace/read lane |
| Aside actions | `aside_ask`, `aside_cancel`, `aside_remove` | Operator only | Control, action lane |
| Aside observation | `aside_watch`, `aside_unwatch` | Operator only | Read, workspace lane only |
| Docs reads | `docs_index`, `docs_read`, `docs_asset`, `docs_history`, `docs_diff` | Operator only | Read, workspace/read lane |
| Docs observation | `docs_watch`, `docs_unwatch` | Operator only | Read, workspace lane only |
| Metrics reads | `usage_summary`, `usage_series`, `usage_breakdown`, `usage_observations`, `usage_runs`, `usage_prices` | Operator only | Read, workspace/read lane |
| Metrics observation | `usage_watch`, `usage_unwatch` | Operator only | Read, workspace lane only |

Negotiating a feature does not enable a denied method. The gateway checks
purpose, grant tier, workspace and limits for every call, including reads.
Keep admin epic methods denied as existing task-graph administration is today.
No body, title, id or feature flag changes the caller's role.
As with native TaskGet today, a device must name a task explicitly in
task_get_scoped; the id-less active-task form is only for an authenticated
agent. The shared method inventory includes parameter-level guards, not just
method-name/tier membership.

The web workspace pipe forwards the negotiated exchange to the selected
actual daemon. The web/index server must not advertise capabilities for a
different workspace merely because its own build has them. Native connectors
negotiate after signed route authentication and on each auxiliary read/action
channel; they do not add fields to the signed proof or borrow an operator token.
Index and terminal channels remain outside this feature protocol.

## Epics: preserve the old task stream

Use the Epic/EpicSummary/count/status/report records in `epics.md`, including
the clarified timestamps and per-agent counts. `Task.epic` remains optional,
with absent encoding unchanged. Add optional `workspace_run_id` to SessionInfo
and optional `epics: EpicBoard` to Snapshot **only in the negotiated projection**.
There is no aside, Docs or metrics body/list in the ordinary Snapshot.

For an `epics=1` connection, Snapshot captures tasks, EpicBoard and run/board
revision from the same committed state. Its Subscribe stream uses:

```text
EpicBoardChanged {
  workspace_run_id, from_revision, to_revision,
  board: EpicBoard, tasks: Task[]
}
```

The task vector contains changed task upserts, not the full task set. Apply
it and the board atomically. Every task transition/factory participates,
including progress, dispatch, waiting, review, failure and retry. Epic-only
metadata changes have no task upserts. New-run/gap/overflow requires a fresh
Snapshot; stale/duplicate revisions cannot revive old entities.

The internal daemon may produce both legacy Task and grouped records for a
committed change. Its **per-connection forwarder** chooses one representation:
legacy subscribers get legacy Task with new feature-only fields omitted;
epic-aware subscribers get the grouped update, not a second Task update.
Unnegotiated clients never receive the new enum variant. Repeat-Snapshot
barriers preserve this choice. Do not rely on every old client ignoring an
unknown push, even though the current Rust Push has an Unknown fallback.

This refines the earlier proposal to send both on every connection. It avoids
transient task/count disagreement and duplicate work in each client while
preserving a complete legacy stream. Same-state legacy request/response
fixtures, including no-epic TaskSpec, remain byte-identical without feature
negotiation. Naturally new event content is not a claim that old data stays
frozen.

`EpicFilter` is All / Id / NoEpic. `EpicMembershipPatch` is Keep / Set / Clear;
never encode No epic as an id string. Scoped mutation payloads include run id
and the applicable expected epic revision. Add `task_create_scoped` rather
than smuggling a new scope into legacy TaskCreate: an old daemon could ignore
an unknown outer field. Existing TasksList, TaskEdit, TaskCreate and
SessionComplete remain available unchanged. An epic-aware adapter uses the
new family when it intends a scoped operation; unsupported scope returns an
error, never an unscoped retry.

`task_get_scoped` returns a run-tagged TaskCard and optional EpicSummary. It
checks the requested run before selecting a task, including an id-less agent
active-task lookup. This prevents an old detail request from fetching a reused
task id in a new run. Legacy TaskGet remains unchanged for legacy callers.

MCP retains ergonomic `task_create(..., epic?)`, `task_edit(..., epic?)`,
`tasks_list(epic?, done?)` and `session_complete(..., epic?)`, plus epic create/edit/
list/get. Missing/null conversion stays explicit as in the epic design. A
non-epic legacy intent can still use a legacy server; a requested epic action
cannot. Task identities, planner order and dependency semantics do not change.
The compact-board `done?: boolean` display option is orthogonal to Epic
filtering and can precede this release. Its default abbreviated Done rows
still include every task in the selected subset; `done: true` expands them.
It does not add a field to legacy TasksList or replace `epic?` with a view enum.

## Asides: bounded private answers, safe retry

Refine the aside proposal into separate summary/body reads:

```text
AsideTarget = { agent_id, incarnation }
AsideMember = { target, runtime, model?, availability, reason?, last_turn_at? }
AsideSummary = {
  id, workspace_run_id, aside_epoch, revision, asked_at,
  question_preview, question_truncated, answer_counts,
  answers: AsideAnswerMeta[]
}
AsideAnswerMeta = {
  target, runtime, model?, status, reason?, context_at?, context_approximate,
  last_turn_at?, started_at?, finished_at?, prefix_reproduced, truncated, usage?
}
AsideDetail = {
  summary_id, workspace_run_id, aside_epoch, revision, question,
  answer_meta?: AsideAnswerMeta, answer_text?: string
}
```

Statuses are waiting, answering, answered, failed, cancelled, not_available.
Unavailable reason is data, never an empty successful answer. A body read for
an unsettled answer returns metadata/status and no partial text. Completed
answers arrive whole. Reuse normalized nullable usage counters and explicit
cost basis from the metrics module; a runtime-reported amount is distinct
from a versioned list-price estimate.
Question previews are the first line clipped to at most 512 UTF-8 bytes; they
are sensitive text in dedicated operator responses, never event or receipt
metadata. Failed native output does not become an empty successful answer.

`aside_members` returns bounded pages of current member availability for the
picker. `aside_list` returns bounded summary pages with all per-answer metadata
for each returned aside, so rendering a list does not fetch every body.
`aside_get(id, agent?)` reads the full question and at most one answer body.
A missing agent asks only for the question/detail header. `aside_watch`
registers before returning its initial summary page and revision, then sends
coalesced id/revision/status invalidations; clients fetch the selected body.
No whole multi-answer record is pushed through a global subscription.
Its delta carries `aside_epoch`, watch id, from/to revision, changed ids and
gone ids with explicit `removed`, `evicted` or `run_reset` causes. Member
availability changes invalidate the picker separately. One aside watch and
one Docs watch (up to two selected documents) coexist with at most four
metric watches on a workspace connection.

`aside_ask` includes run id, a client-chosen request id, question and optional
explicit targets. Omitted targets captures the whole current team once;
explicit targets validate incarnation, and an empty selection is invalid.
Unavailable members receive metadata/reasons, not a hidden omission. A member
changing before a queued fork launches cannot silently substitute a new
incarnation/context. Store the captured target set for retries.
Parameter/run/target-count/pending-capacity/memory admission failures refuse
the whole ask before any fork starts and preserve the client's draft. Runtime
failures after a valid admission remain per-answer states. A stale explicitly
selected incarnation becomes not_available with member_changed; it never
silently asks a replacement member.

Request ids are stable across uncertain transport retries. Same id and same
canonical request while retained returns the existing header; different input
conflicts. After authentication/run checks, resolve a known request id before
checking current capacity or capturing the team again. Record a **body-free
admission receipt** durably before starting any
fork, using the same id as the aside id or a stable mapping. Do not put a
question, answer or low-entropy question hash in that durable receipt. While
the body is in memory it can be compared there. After removal/eviction/restart,
a known accepted id returns gone/outcome-unknown and never starts another paid
fork. Explicit Ask again uses a new id.

Keep exact admission ids for the active run. If the bounded receipt budget
cannot preserve deduplication, refuse new asks instead of forgetting ids and
potentially charging a retry again. Old-run requests are rejected by run scope.
Metadata-only aside events can supply audit references but a substring search
of event text is not the idempotency mechanism.
Receipt rows hold only run/request/aside ids, creation time and outcome. Limit
an active run to 65,536 receipts and 16 MiB of encoded receipt payload; storage
failure/cap refuses new admission rather than pruning active-run deduplication.
Receipts may expire when that run closes, because later old-run requests fail
the run check before admission. There is no age-based expiry inside an active
run. The B08 no-text test covers this store too.

Retain at most 50 asides, 64 KiB of UTF-8 per answer, 8 KiB per question, and
32 MiB of retained/reserved question/answer/metadata memory per workspace.
Limit an ask to 64 targets and a daemon to 128 pending answers, with the
approved four running/one per member. Oversized all-team requests fail visibly;
never ask only the first subset without saying so. Evict settled old records
before admission; active reservations cannot evict one another's results.
A full active budget returns busy. This supplements the count limits so a
large team cannot make 50 retained objects unbounded in practice.
Replies distinguish target_limit, pending_limit, memory_limit and receipt_limit
with their safe numeric limits. Count/memory limits can retain fewer than 50
asides; eviction is distinguishable from Remove for everyone or restart. A
client with an already-open view may retain that local view after confirmed
eviction/restart, with the explicit loss reason and Ask again. That action
opens a draft and needs a new request id when sent. A fresh client has no ghost
row; the server cannot reconstruct text from a body-free receipt. Eviction
copy is "dropped to make room"; restart copy is "the daemon restarted".

`aside_cancel(id, agent?)` and `aside_remove(id)` stay distinct Control actions.
Cancel stops the chosen/all unsettled answers; already settled answers remain.
Remove cancels unsettled work and removes question/answers for every viewer,
advancing revision and retaining the body-free admission receipt. Use the
owned process/lease retirement rules; neither action can affect the live
member's process group.
Removal fences answer publication before cancelling owned jobs. Retain any
partial usage, but never let a late worker result or an older in-flight read
restore removed text. Every client processing removal drops cached question,
preview and answer text, including open views. Lists lose the row; another
client's open view can show only "This aside was removed" and Back. There is
no Ask again or local text-bearing ghost for explicit removal.

Persist the body-free `removed` receipt outcome before acknowledging removal.
`aside_get` for a gone id returns its explicit receipt-backed cause, never a
body. A reconnect or watch gap reconciles retained local ids before showing
them again; a changed aside epoch alone does not prove that removal never
happened. Preserve the removed cause across daemon restart. Only a confirmed
eviction/restart can retain the already-open local view above; unknown cause,
run reset or loss of read authority clears it. This keeps an offline client
from mislabelling a missed removal as a restart and offering Ask again.
An aside cannot acquire filesystem/runtime authority beyond the copied
member's profile. All approved native isolation and deny-rule probes remain
required; feature negotiation is not a substitute for execution isolation.

`aside_epoch` changes on daemon restart and run change. Replies/watch metadata
carry it, so a reconnect distinguishes lost ephemeral bodies from an empty
new page. Clear-session cancels old-run aside work and clears its bodies;
metrics and body-free audit events retain their original run. Admission
receipts can retire at the closed-run boundary described above. Preserve the existing
"Asides from before the daemon restarted are gone" UI behavior, and identify
a run reset separately. A dropped connection closes its watch, not an
already admitted workspace-owned job. Revocation prevents new admissions and
further publication to that grant; it does not claim to undo model usage
already incurred. Explicit cancel/remove, timeout and daemon/run shutdown own
job termination.

All aside methods, results and watch frames are operator-only. Keep
`aside.asked`/`aside.answered` events body-free and out of agent-readable event
projections; do not attach them as a target member's own activity. No bus,
Inbox, Task, Agent activity, standing prompt or member transcript receives an
aside. Account/usage attribution is separate observer data, not a call to the
live member's limit or workflow handlers.

## Docs: connection-scoped resources

Use the approved closed Doc AST, reference/version/image/index/history/diff
records. Raw HTML is literal data; only validated static raster images can
be fetched. Preserve source-root safety, published-only VCS history, bounded
parsing/leases and explicit unavailable states from `knowledge-base.md`.

All Docs cursors, version/snapshot/asset ids and watch ids are bound to the
issuing authenticated connection, workspace, Docs epoch and applicable source
generation. Source paths remain private. `docs_index`/`docs_history` establish
the ids later calls can use; a path, URL or object id from a client is never
read authority.

The native driver already reuses a matching Read channel (`driver::dispatch`).
Make that **physical-channel affinity** explicit for a Docs operation sequence:
index/history, read, assets/diff. Retain an internal lease-to-channel map;
keep the lane for the bounded sequence and invalidate its resources if it
closes. Do not silently retry a handle-bearing request on a replacement channel
just because ClientScope/attempt matches. Recapture on a new channel. Do not
expose socket handles through FFI or ask each UI to implement transport pinning.

`docs_watch`/`docs_unwatch` use the workspace connection, never the auxiliary
Read channel. Any ids/leases issued there remain on that connection; a UI may
perform the related reads on the same workspace connection or explicitly
capture a fresh independent read-lane sequence. It cannot move a snapshot id
from the workspace watch to a read lane. Preserve the two-document selection,
reply-before-push barrier and bounded coalesced DocsChanged metadata. No Docs
body or index is added to ordinary Snapshot/Subscribe or any agent MCP tool.

An expired cursor/snapshot returns a typed stale/expired result. It never
substitutes current bytes for a historical image or silently switches the
selected document. Disconnect/revocation/source removal cancels the related
jobs/watches and invalidates previews according to the source design.

## Metrics: exact values with explicit uncertainty

Use the approved `UsageScope`, query, observation, totals, coverage and price
records, with decimal wide values above. Counters are nullable **independently**.
Keep NoEpic, known NoTask and unknown/multiple attribution distinct in typed
records and coverage even if a view combines some rows. Do not infer account,
task or epic from current membership or the selected tab.

Queries state run, live/aside scope, half-open time range `[from_ms,to_ms)`,
grouping, bucket and price-book version. A missing bound means unbounded;
unknown-time observations enter only unfiltered totals, with coverage. A
request count is not a turn count or the cardinality of an arbitrary cumulative
batch. Server-side computation supplies exact numerator/denominator and the
metric-specific eligible intersection; clients format, not recompute it.
Resolve defaults to a canonical query and return that query's effective scope,
range, bucket and price-book id. Cursors and watches pin the resolved price
book; a new price book does not silently change a chart's basis halfway
through pagination. An explicit refresh can select the new book.

`CostEstimate` distinguishes finite estimate/range, partial known subtotal and
unavailable. No numeric upper bound is manufactured for unbounded missing
cost. First-completion cohort membership must itself be complete before a
whole-period per-completed-task metric can have a bound. Missing cost within
that known cohort is separate coverage. An observed zero cost is valid;
absence of observed cost is not zero.

Carry the `f6e13265` retention amendment into fixtures: an append-admitting
logical stream retains exact seen-id membership after detail expiry. A byte
watermark does not identify a late old id appended beyond it. An uncorrectable
old contribution invalidates affected rollups; it is not necessarily a lower
bound because the correction can reduce spend. At the storage boundary,
pause/close collection with a visible gap instead of billing a replay again.
No retention operation mutates a native session file or blocks task execution.

`usage_watch` takes the same canonical query as the initial summary, registers
before capture, and emits revision/invalidation metadata at most once per
second. Page cursors pin the query/revision or return stale. Up to four watches
per connection, one coalesced pending update each. Historical run queries stay
inside the same authorized workspace; watch them only while their retained
ledger can still change. Corrections retain original time and attribution.
Neither collection nor watching produces a runtime Activity signal or model
prompt.

## Frames, watches and errors

Every complete escaped JSON frame remains below the existing **1 MiB** limit;
new result pages target at most **512 KiB**. Apply the check to the final
serialized envelope before queueing it. Raw text/chunk estimates, unescaped
lengths and base64 input size are not sufficient. Bound allocation, nesting
and collection work before serialization as each design requires.

Pages stop at both entry count and byte budget. At most 200 metadata/detail
rows or 720 metrics buckets; a single oversized record returns too_large,
not an endless empty page. Aside bodies fit individually, Docs assets remain
at most 192 KiB raw chunks with downward adjustment for the envelope, and
unavailable/limited states remain explicit. A grouped Epic delta too large
for its bounded stream produces Resync; a repeatedly oversized Snapshot gives
a visible too_large failure, never an unbounded resend loop or a raised
transport cap. This plan does not claim to paginate the legacy core Snapshot.

Add independent feature watch state/forwarders to `Conn`; do not reuse its
single `stream_task`, which already serves Subscribe/terminal streams. Normal
Snapshot barriers do not cancel a Docs/aside/metrics watch. Disconnect and
scope loss cancel their forwarders and pending reads; cancellation does not
release a worker permit while a blocking job still owns it.

Watch responses include feature epoch, watch id and captured revision. Queue
the initial reply before later updates. Coalesce bounded pending metadata;
on overflow/gap emit a small resync marker for that feature, not a full body
backlog. No observer watch can replace an agent Listen channel, write into a
terminal or trigger scheduling/health activity.

Use existing RpcError envelopes with finite codes such as unsupported_feature,
stale_scope, stale_cursor, snapshot_expired, too_large, busy, forbidden and
gone. Unavailable domain content is its typed result where the feature design
specifies one, not empty success. Debug/log output for aside question/answer,
Doc content/assets, native records and sensitive errors must redact bodies;
never log a full new request/response by default. Preserve original method
permission checks even when capability discovery succeeded.

## Shared client, bindings and frontend boundaries

One Rust core remains the state/availability owner. Extend existing
ResourceKind, ReadOptions, ResourceKey/Request/Value and ViewQuery/ViewPayload
with typed feature variants; do not add a second browser/native state engine
or a JSON-string FFI escape hatch. New entity/resource keys include the
applicable run, feature epoch, query/version and selection identity.

- Epics extends TasksData and has an on-demand EpicDetail resource/view.
- Asides adds bounded member/list/detail resources and Control action intents
  for ask/cancel/remove. A request id is allocated on user submission, never
  by an availability query; preserve it through an uncertain result/retry.
- Docs adds the typed index/snapshot/asset/history/diff resources plus explicit
  workspace watch effects and internal read affinity. Rendered native Docs
  UI remains deferred; DTO support does not claim a screen.
- Metrics adds typed query/results/pricebook and watch resources beside the
  existing legacy Usage view. Do not coerce unknown new counters through the
  legacy zero-filled Usage struct.

Negotiate each transport channel before dispatching feature commands. The
read/action channel's feature set must satisfy the command's version as well
as existing ClientScope/attempt/daemon/grant fences. A lost action reply remains
OutcomeUnknown; reconcile by the idempotent receipt/read path where specified,
never blindly resend a paid operation with a new id.

Keep run/feature scope separate from `ClientScope.generation`, which already
represents native authorization/transport identity. Do not reuse that field
for a workspace run. Run changes invalidate run-bound keys, pending details
and feature intents even if the daemon pid/connection attempt did not change.
Do not bypass the existing local/native lane verification while adding
negotiation. A result still belongs to the operation/selection that requested
it; refreshing the view does not retag an in-flight reply.

UniFFI records, exhaustive converters, Swift generation and package consumers
land together with feature support. Use typed decimal strings for wide values
and test recursive/bounded AST conversion on the pinned generator. If a
binding limitation requires a different representation, revise this contract
before replacing typed data with arbitrary JSON. Apple compile/device results
remain separately stated under the operator's existing execution boundary.

Web/TUI/GTK/Apple use server projections for status, counts, metrics and
coverage, and the same unavailable reasons. UI filtering does not become
server authorization or change scheduler order. Source-message reads still
use MessageGet authorization. The designer's view, accessibility, keyboard,
retention and byte-budget requirements remain in force.

## One MCP surface change and implementation order

Only the collaborative Epic tools/arguments expand the member-facing MCP
surface in this tranche. Asides, Docs and detailed metrics are operator
observer APIs, not agent tools. Tool definitions are fixed per build and role;
never register/remove them as an epic appears or a watch opens.

Land the final Epic tool set/argument shape once, with the complete service,
not as successive half-functional schemas. The toolbox role/pinning rules
still apply. A running MCP server keeps its launch's catalog; upgrading a
member is a deliberate build/catalog transition, not a mid-turn listChanged
mutation. Fingerprint exact ordered names/descriptions/schemas for worker and
orchestrator catalogs. The static orchestration rule belongs to the same
reviewed release; live feature data remains in results/tail input only.

Suggested cards, with one assigned owner per central seam:

| Stage | Files/owner boundary | Depends on / may proceed independently |
| --- | --- | --- |
| P1 portable contract | `crew-protocol/{features,epics,asides,docs,usage_metrics}.rs`, existing types/rpc reexports; new host Call variants and codec schema | Native architect owns final schema/goldens. Types alone advertise no feature. Freeze all shared names once. |
| P2 run and connection foundation | persistence run pointer/lifecycle, `daemon/{mod,server,handlers}.rs`, `crew-core::client`, per-connection projection/negotiation and bounded watch plumbing | One runtime/integration owner; P1 first. Preserve existing Subscribe/Listen/terminal and old clear-session behavior. |
| P3 domain services | Epic store/transitions; aside execution/controller; Docs safe index/parser/history; metric ledger/normalization/query | Separate owners, using P1/P2 APIs. Keep source-design ownership boundaries. Each exports a readiness/handler interface rather than independently editing central dispatch. |
| P4 transport and binding integration | native policy/forwarding, `crew-client` API/state/codec, `crew-client-transport`, FFI records/converters/generation | One shared-client owner, after P1; add each ready service through the central inventory. Docs connection affinity is owned here, not in a UI. |
| P5 frontend slices | Epic grouping, Asides, Docs, Usage views and routes | Designer-approved slices can use typed fixtures early; enable real controls only when their negotiated backend and local implementation are ready. |
| P6 one public surface and integrated proof | MCP Epic catalog, CLI adapters, capability enablement, old/new peer matrix, full source-design fixtures, budgets/build packages | Orchestrator/integration owner assembles reviewed services/consumers. No feature flag is enabled merely because records compile. |

These boundaries require coordination with current toolbox and delivery work:
`rpc.rs`, `Agent`/FFI mirrors, `daemon/server.rs`, `supervisor/launch.rs` and
`crew-client/api.rs` are not simultaneous independent edit areas. Backend
services should land through small modules; the central owner wires their
calls/policy once. Model quota fields/account-pool actions remain a later
capability until that auth contract is settled, while nullable attribution
fields can be present now.

## Compatibility and integration gates

Use the frozen pre-feature v6 implementation `b4f9a442` as the old peer and the
candidate as the new peer. Test actual decoders/clients as well as JSON goldens;
local stubs and isolated daemons suffice for protocol gates, without model
calls or the operator's live service.

| Gate | Required evidence |
| --- | --- |
| P01 Legacy | Old client/new daemon without negotiation preserves existing request/response shapes, absent-field bytes and Task stream; old peer never receives a feature push. No-epic TaskSpec and base Hello/native proof stay unchanged. |
| P02 Missing feature | New client/old daemon handles actual unknown negotiation as unavailable, retains legacy views, and never retries Epic/aside intent as an unscoped mutation. Scoped mutation attempts have zero side effects. |
| P03 Negotiation | Partial/unknown/version-mismatched requests, repeated identical and changed sets, unauthorized roles and every native lane behave as specified. Capability is not authorization. |
| P04 Run/identity | Clear/reload, reused task/agent ids, process/daemon restart and historical metrics queries preserve the correct run/incarnation; stale writes cannot affect a new run. |
| P05 Epic coherence | Every task factory/transition produces one coherent grouped update for negotiated peers and complete legacy updates for old peers; snapshot races, gaps and oversized state fail safely. Scheduler trace unchanged by grouping. |
| P06 Aside retry/privacy | Lost reply, same/different request input, remove, eviction, restart, run reset and budget pressure cannot launch duplicate paid forks. Removal fences pending publication/stale reads and clears every client's text; a disconnected client missing removal then seeing a daemon restart cannot revive it as a ghost. No agent-facing body/event/workflow leakage; cancel/removal touch only owned jobs. |
| P07 Observer barriers | Edits/answers/corrections between watch registration, capture and reply are not lost. Independent watches coexist with Subscribe and terminal connections, and slow readers stay bounded. |
| P08 Docs leases | Read-lane reuse, replacement, root/watch versus read handles, revoked grants, source generation and concurrent selection tests prevent cross-connection/stale content. All K01–K20 remain required. |
| P09 Metrics truth | Nullable/wide round-trips and U01–U18, including f6e13265's late old-id/correction and incomplete-cohort negatives. Counter parity against committed lifecycle native fixtures; events remain associations, not causal proof. |
| P10 Bounds | Worst escaping/base64, giant per-record metadata, recursive ASTs and pagination obey final frame/allocation limits. Unknown data never becomes zero or an empty successful body. |
| P11 Native/bindings | Exhaustive policy/lane coverage, codec parity, real converter tests, generated Swift freshness, portable/default/Rust1.82 gates; old library/new v6 daemon remains usable in legacy mode. |
| P12 UI/MCP | One fixed ordered Epic catalog per role/build, no live names in definitions; all designer F-cases and existing web/native budgets pass. No new native screen claim without its renderer and platform evidence. |

Each feature retains its domain gates E01–E16, B01–B12, K01–K20 and U01–U18.
A passing new UI mockup or a generated binding is not permission to skip them.
