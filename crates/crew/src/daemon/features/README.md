# Feature service ports

The daemon owns authentication, negotiated versions, workspace run identity and
connection forwarding. Domain services implement `FeatureService` and return
their P1 typed results through the dispatcher. No service is registered by
default; a compiling DTO or adapter does not enable a feature.

`ConnCtx` captures the exact authenticated physical `connection_epoch`, current
`RunCtx`, actual `Caller`, and cancellation signal. A replacement connection
gets a new epoch. Updating run metadata keeps the physical epoch but cancels
old jobs; a job's cloned context is never retagged. Caller information is
internal routing state, not a field supplied by a request body.

`FeatureDb` shares the `SqliteStore` connection and writer mutex. Domain reads
use `read`; writes use `transaction(Some(expected_run), ...)`. The run check
and all writes share one SQLite transaction. The caller holds any required
model fence, and publishes projections/events only after commit. Callbacks
receive the transaction directly and must not lock the store or commit it.
The transaction-local helpers in `persistence::sqlite` reuse the legacy task,
message, delivery and event SQL, including message-id idempotency.

Receipt admission/removal uses `durable_transaction`, which selects SQLite
`synchronous=FULL` before beginning the transaction and restores the previous
setting under the writer mutex after commit, rollback or unwind. Restoration
failure keeps FULL; it does not report a committed receipt as rolled back.
Paid execution and acknowledgements follow the successful durable return.

A service registers its observer before capturing the initial watch result.
`watch_stream` supplies its own bounded, coalesced `FeatureWatch` metadata
queue. The daemon creates the forwarder before replying and starts it only
after the initial reply is queued. These tasks are independent of Subscribe,
Listen and terminal forwarding. Metadata must match feature, watch id and
epoch. Epoch replacement preserves bounded terminal selection metadata;
oversized or backpressured updates become small resync markers. A
blocking domain job retains its worker permit until it actually finishes,
even when connection/scope cancellation has been signalled.

The daemon admits one aside watch, one Docs watch and four metrics watches per
physical connection. A Docs watch replaces its selection: failed validation
keeps the previous watch alive, and successful preparation aborts and awaits
the previous forwarder before queuing the new reply. Physical connection
closure is separate from run cancellation. A watch on an open connection
ends with final resync metadata; disconnect sends none.

Hello captures an agent incarnation without extending its wire response.
Negotiation and feature dispatch refuse a removed/reused member connection.
Scoped Epic handlers use `commit_tasks_for` and the immutable `TaskModel` view
to repeat identity/run/cancellation checks under the transaction fence. Metrics
queries may name archived runs from this workspace's run table; a run from
another workspace is rejected before the typed service handler.

`FeatureService::shutdown` cancels and awaits owned execution/retirement.
Daemon shutdown refuses new feature admission and waits for these hooks
before ordinary member teardown. A dropped observer connection only cancels
its reads/watches, not a previously admitted workspace-owned execution.

Report/task/message preparation uses the shared preflight validators exported
from this module. A prepared decision retains the authenticated sender; an
operator cannot manufacture a question from an agent.

`TaskProjection` hydrates the board and task membership under the model fence.
Its `apply_delta` saves hydrated upserts inside the supplied transaction and
returns `TaskChange { tasks, publication }`; a no-op need not bump the board.
`Daemon::commit_tasks_with` gives a scoped domain handler the same model and
transaction, validates its committed upserts, and updates memory only after
commit. Existing task order is preserved, with new cards appended. Grouped
and legacy task publication share that result; persisted decisions use
`MessageBus::publish_committed`, not another insertion. A durable outbox
sequence is acknowledged through `TaskProjection::acknowledge` after
publication. Failed acknowledgement leaves the domain receipt for replay.
`TaskProjection::clear_run` runs before legacy session clearing in that same
transaction, leaving private finalized report history to the domain.

The foundation registers no capability. Production service registration,
source/config/launch adapters, lifecycle collection and composed daemon/client
proof belong to `features-p6-daemon-wiring` after approved domain services land.
Test-only ready services exercise the routing ports without advertising a
partial backend. The final review report records foundation gates separately
from that integrated proof.
