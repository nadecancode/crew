# Native session transport

`crew-client-transport` owns Rust native IO and request correlation. The portable
`ClientApi` owns domain state, action policy, confirmations, retry decisions and
final operation notices. `Protocol6Codec` delegates to the shared
`Command::encode` and `Command::decode_result`; it contains no second encoder.

Default features are empty. Feature `runtime` enables the generic Rust service
and bounded event queue on native targets independently of an IO adapter.
Feature `local` enables `runtime` and compiles protected Unix discovery,
authentication and IO only on Linux/macOS. iOS has no local discovery or Unix
credentials in its dependency graph. This crate declares Rust 1.93; CLI and
portable package floors are unchanged. Gateway IO is a separate reviewed slice.

## Host service

`NativeSession::local(core, Protocol6Codec, profiles, config)` creates one Rust
event loop with a single wall/monotonic clock origin. Its synchronous methods
belong on a platform worker or actor:

- `query(ViewQuery) -> ViewData` and `workspaces() -> Option<Workspaces>` return
  typed retained data without a network read.
- `plan(ActionIntent) -> ActionPlan`, `confirm(id)` and
  `cancel_confirmation(id)` invoke shared policy. Only core request effects
  dispatch commands; a dispatch plan contains an operation ID.
- `connect`, `select_workspace`, `reconnect`, `disconnect`, `lifecycle` and
  `close` control client resources. Closing never sends a team shutdown RPC.
- `observe` registers one `ClientObserver`; `unregister_observer` removes it.
  `changed(ClientNotice)` and `terminal(TerminalChunk)` run on a separate Rust
  dispatcher outside every driver/queue lock. Observers can query again.

Transport `HostEvent`/`HostEffect`, frame JSON and connector handles are internal
Rust host interfaces. They do not cross the Swift UI bridge. The facade forwards
typed notices and projections to the platform executor and UI thread.

`ClientUpdate.outcomes` is the sole source of final notices. A held identified
message retry emits none until its final completion or expiry; the driver does
not duplicate policy or infer completion from an effect string. Core policy
timers remain registered through a permitted connection closure or background
suspension until core
cancels them. IO timers retain channel/attempt guards.

## Local authority and ordering

Local discovery reads bounded regular records from the current user's private
Crew registry without pruning, following links, joining teams or scanning ports.
An explicit socket profile is also supported. Authentication first refuses any
`CREW_IDENTITY` presence and uses the existing protected operator-token reader.
The connector checks socket ownership, peer UID/PID and exact protocol 6 hello.
Tokens and credential-bearing hello data never enter a host event or diagnostic.

Native state subscribes and snapshots on one socket using the daemon barrier.
Before the first live snapshot, provisional local scope is replaced by the
authenticated canonical workspace/PID/startup scope; the exact normalization
reason is `canonical_scope`. Every new local read/action/terminal socket takes a
zero-history snapshot and checks that identity before dispatch. No late data
from a retired channel or attempt can change the new scope. Existing TUI
two-connection behavior is unchanged.

Each terminal channel has one immutable `(scope, attachment_id)`. Attach's
operation ID is its attachment ID. Input/resize have new operation IDs and keep
that captured attachment. Local detach produces `ClosedLocally`; it is not a
daemon acknowledgement. Successful protocol 6 input/resize replies are
suppressed by the daemon: the driver reports `SentUnacknowledged` only after
full write. A late error ends that attachment once, without a second outcome.
Partial/interrupted writes are outcome-unknown and terminal bytes never replay.
Late error tracking uses a bounded per-channel ID interval, not one retained
entry per keystroke.

Index authentication/listings have endpoint identity independently of workspace
scope. A workspace connection failure can leave its index authenticated. An
action socket that fails with pending work conservatively retires the workspace
lanes and establishes a new state attempt/barrier; core alone may authorize the
identified-message exception. Generic timeouts do not trigger mutation replay.

## Bounds and teardown

Lines are capped at 1 MiB before growing a string or decoding JSON. Empty,
invalid UTF-8, malformed, oversized and unterminated framing closes the socket.
Known malformed pushes fail; additive unknown pushes are ignored. Explicit
nullable results remain distinct from missing or ambiguous result/error keys.

Native defaults are eight channels, 64 pending requests and at most 16 pending
requests per channel. IO and observer queues have 256 records/4 MiB limits.
Each registered IO channel has one reserved FIFO end notification, so overflow
cannot hide the event that fences its data. Observer overflow retires IO,
preserves final core outcomes in a reserved notification and requires a fresh
synchronization. Terminal overflow ends visibly. Shutdown cancels timers,
closes channels, unregisters observers and releases client references.

## Validation and workspace registration

N3 was tested on Linux with Rust 1.93.1 using a temporary QA-authorized workspace
registration: add `crates/crew-client-transport` to `members`, keep the five N1
packages as explicit `default-members`, and regenerate the lockfile. Source
handoffs restore root/Cargo.lock and import this crate separately; QA owns final
registration, compatible dependency pins and lock integration.

```sh
./scripts/build-env cargo test --locked -p crew-client-transport --features local
./scripts/build-env cargo clippy --locked -p crew-client-transport \
  --all-targets --features local -- -D warnings
./scripts/build-env cargo check --locked -p crew-client-transport --no-default-features
./scripts/build-env cargo check --locked -p crew-client-transport --features runtime
./scripts/build-env cargo tree --locked -p crew-client-transport -e normal
./scripts/build-env cargo fmt -p crew-client-transport -- --check
```

The 30 independent tests cover framing/FIFO/observer backpressure (6), scoped driver behavior
(16), real isolated-daemon authentication, snapshot barrier, observer query
reentry, client-only teardown, failure fencing and new-PID/token reconnect (6),
and negative hello/frame cases (2). Environment-sensitive tests re-execute in
temporary XDG/runtime/workspace directories; none touches the active team's
daemon. Driver tests use a `ClientApi` probe to isolate transport behavior.
Real shared reducer/action/projection composition remains the separate
`native-transport-validation` gate. These checks prove neither a connected
native UI nor gateway readiness. Apple build/device verification is deferred by
the operator and has not run here.

One additional lifecycle regression is enabled by `real-core-tests` and uses
the real shared `ClientCore`, without a copied retry policy. Its provisional
development prerequisite is the exact `crew-client` source at `2dde20fc`
(including `8a376891` and the approved actions/projections). It verifies that a
backgrounded fully written identified Send has no final notice, keeps its
original deadline, and replays once after foreground authentication and a
newer matching snapshot barrier, with the same logical/message IDs and bytes.
It then receives one final success and ignores the obsolete retry timer.
The final integration gate repeats this against the accepted shared engine.

```sh
./scripts/build-env cargo test --locked -p crew-client-transport \
  --features local,real-core-tests --test real_core_lifecycle
```

Historical checkpoint `1c8b83e9` included temporary root/lock registration after
a failed restoration command. Corrective `dadde50b` restored both files to the
exact `e647b593` foundation; no history was rewritten. Later handoffs are
crate-only source. Apply the reviewed final crate directory onto the integrated
branch instead of importing that historical root delta or merging this branch.
