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
epoch; oversized or backpressured updates become small resync markers. A
blocking domain job retains its worker permit until it actually finishes,
even when connection/scope cancellation has been signalled.

Report/task/message preparation uses the shared preflight validators exported
from this module. A prepared decision retains the authenticated sender; an
operator cannot manufacture a question from an agent.

The first port checkpoint was verified with `cargo test --locked -p crew
--lib features::tests` (12 tests). Watch protocol negatives, grouped task
projection, client discovery and final integrated gates remain separate work.
