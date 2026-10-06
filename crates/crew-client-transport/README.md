# Native session transport

`crew-client-transport` owns Rust native IO and request correlation. The portable
`ClientApi` owns domain state, action policy, confirmations, retry decisions and
final operation notices. `Protocol6Codec` delegates to the shared
`Command::encode` and `Command::decode_result`; it contains no second encoder.

Default features are empty. Feature `runtime` enables the generic Rust service
and bounded event queue on native targets independently of an IO adapter.
Feature `local` enables `runtime` and compiles protected Unix discovery,
authentication and IO only on Linux/macOS. iOS has no local discovery or Unix
credentials in its dependency graph. Feature `gateway` adds the shared scoped
TLS/WebSocket device adapter on native targets. This crate declares Rust 1.93;
CLI and portable package floors are unchanged.

## Gateway service and platform signing port

The gateway source is an implementation checkpoint pending connector review;
these callable ports do not claim completed live gateway validation.

`GatewayProfile::trust(id, PublicProfile, independently_verified_fingerprint)`
validates the public profile and explicitly approved trust. A matching pin must
come from the independent trusted host-console comparison, not from accepting
unverified network metadata. URL, pin and trust fields cannot be changed after
construction. Only `wss://host:port/native/ws` is accepted. TLS validates the
chain, hostname, validity and handshake signatures, then checks the leaf SHA-256
before upgrade or signing. TLS 1.3, no early data and no resumption are explicit
runtime settings, independent of Cargo feature union.

`DeviceSigner` returns bounded asynchronous results for `public_key`,
`sign_auth(AuthTranscript)`, `sign_enrollment(EnrollTranscript)` and `retire`.
The transcript types have private constructors; only the authenticated Rust
connector creates them. A foreign bridge exposes opaque transcript objects with
read-only `signing_bytes`, never a constructor accepting arbitrary bytes. Public
keys have 32 bytes; signatures have 64. The connector validates strict Ed25519
proofs before sending. Unlock/public-key retrieval precedes every short network
authentication deadline. Key/store handles remain in the platform adapter;
locked, unavailable, canceled and retired keys have finite typed failures.
Only `public_key` may unlock or request user presence. The two `sign_*` methods
must not prompt; they fail promptly if the unlocked key is unavailable, within
the Rust connector's five-second proof deadline.

`GatewayClient::new(profile, signer)` owns one profile's local setup/fencing.
Its asynchronous `session(core, Protocol6Codec, config)` produces a real
`NativeSession` after key unlock. Creation performs no connection. Connect with
`(profile_id, endpoint_id, None)` to the authenticated Index, then select a
workspace from its filtered listing. `GatewayOpened.route` is the only source
of scope generation, daemon PID and startup time. Captured expectations are
compared with route facts and never substitute for them. Every workspace lane
requires route metadata; Index has no invented workspace scope.

Run `enroll(label, SetupObserver)` on a Rust setup worker. It returns
`EnrollmentResult { context, outcome }`; progress carries the identical
`SetupContext { profile_id, endpoint_id, attempt }`, unique across instances.
`cancel_enrollment(attempt)` cannot cancel a newer ceremony. Comparison codes
appear only after an accepted intent from the pinned host and local transcript
comparison. Enrollment is never retried. Cancellation before a proof write is
`Canceled`; a started proof write or lost approval reply is `OutcomeUnknown`.
Recover by authenticating the same key. Approval waits use the accepted intent's
UTC expiry sampled by Rust and a monotonic delay capped at 120 seconds. A
platform facade forwards these typed
records, with its own callback generation guard; it does not implement a
handshake or pass raw frames to a view. Native-v1 notices are a closed enum;
unknown native notices close as `Malformed`, while additive unknown daemon-v6
pushes retain the shared driver's tolerant behavior.

`close()` fences this profile's sessions and setup while retaining the OS key.
`retire()` also retires that key locally; neither operation revokes the host's
grant or stops the gateway/team. Linux's explicit `MemoryDeviceSigner` choice
keeps a zeroized key in memory only. It is never an automatic fallback to a
plaintext file. Apple/Linux secure-store adapters implement the same port.

## Gateway validation

The optional development-only `gateway-tests` feature enables the reviewed
gateway library as a test dependency and TLS 1.2 on the fixture server. The
client still permits only TLS 1.3; a real handshake regression verifies that
Cargo feature union cannot broaden its runtime TLS policy. Test certificates
and keys are synthetic, public fixtures used only on ephemeral loopback
listeners. Native server tests re-execute with all `GIT_*`, `HERDR_*` and
`CREW_*` overrides removed and temporary runtime/state/config/workspace paths.

```sh
./scripts/build-env cargo test --locked -p crew-client-transport \
  --features gateway-tests,real-core-tests
./scripts/build-env cargo clippy --locked -p crew-client-transport \
  --features gateway-tests,real-core-tests --all-targets -- -D warnings
./scripts/build-env cargo check --locked -p crew-client-transport --features gateway
```

The gateway tests exercise route-only scope proof, captured mismatches, grant
and epoch fencing, lane policy, strict cryptographic results, before-proof
pin/name/expiry refusal, native notice mapping, bounded WebSocket framing,
comparison codes and local key retirement. The isolated real daemon/gateway
journeys cover reverse enrollment, cancellation before/after a proof write,
same-key recovery, replayed signer proofs, Read/Control changes, per-device
revocation, expiry and gateway restart. They compose the actual shared Core
and native service; mocked peers are labeled separately.

The source acceptance uses the gateway's reviewed membership-generation and
listing-coherence corrections. Membership generation is independent of
`WorkspaceGrant.grant_generation`; entries and grants describe the same visible
authorized set. The client retains strict comparisons and constructs no
membership, daemon PID or startup facts from an incomplete listing.

The complete Linux source run passed 56 tests with Rust 1.93.1: framing/queue/
observer (6), gateway protocol/TLS/fencing (15), driver (16), isolated local
daemon (6), negative local IO (2), isolated real gateway/daemon (6), actual Core
foreground retry (1), and attachment notice/dispatcher composition (4).
All-target Clippy with warnings denied and formatting passed. The test graph
used exact Core `93266067` with released actions `cda71035`, protocol route
records `759aea95`, and reviewed server `589dca0c`/`17d071cb`/`19c62f52`.
The actual control edit/revoke/regrant test failed on the earlier server's
generation and orphan-grant defects, then passed with both server corrections;
the client kept its strict route and listing checks.

Only this transport crate is a client source handoff. Development imports of
Core/server files and temporary root/lock registration are excluded from that
handoff. QA registers the crate against the integrated Rust 1.82-compatible
workspace lock; copying this private branch's root or lockfile is unsupported.

Final `native-transport-validation` repeats the local/gateway journeys against
the integrated source and lockfile. These crate tests do not establish a
connected GTK/Apple application or tested platform secure storage. Apple
execution remains deferred by the operator.

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
scope. A workspace connection failure can leave its index authenticated. Once
a selection has authenticated a gateway index, every reconnect reopens that
index before the selected workspace, so a later workspace-only revocation or
grant change leaves the device signed in with a current listing. A gateway
refusal of one workspace route while the index is open marks only that
workspace `AccessRevoked`; the index alone decides whether the device signs
out. Local connections have no index and are unchanged. An action socket that
fails with pending work conservatively retires the workspace lanes and
establishes a new state attempt/barrier; core alone may authorize the
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
the real shared `ClientCore`, without a copied retry policy. The final N3 review
ran it against approved `crew-client` source `93266067`. It verifies that a
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
