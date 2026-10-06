# Crew native bindings

This package binds the portable Crew client records to Swift through pinned
UniFFI 0.31.0. Record conversion and the `CrewClient` methods delegate to the
portable records and actual Rust `NativeSession`. `CrewClient.local()` composes
the real shared `ClientCore` with the local connector and native policy presets.
`CrewClient::from_session` remains a Rust-only composition seam for approved
connectors. Both the core and local driver use the reviewed producer sources
named below. No substitute engine or placeholder action is used. Gateway
construction and device signing are a separate binding integration.

The portable crates do not depend on UniFFI. `records/shared.rs` uses remote
declarations to implement conversion on the actual Rust types; generated struct
construction and exhaustive enum matches detect upstream shape drift. The
records in `records/views.rs` adapt opaque JSON, host-sized counts and owned
view payloads. Metadata null becomes `None`; other metadata becomes compact JSON
with sorted keys. `Event.seq` stays signed, and file counts widen to `u64`.
Boxed Rust view values lower as Swift value-type aliases, not object handles.
`messageQuestionInfo(message:)` delegates question classification, asker copy
(`asker` and optional `askedBy`) and suggested answers to the existing Rust protocol helpers. Swift consumers
receive typed facts without implementing a second metadata policy. The helper
does not authorize answering; the shared action planner still decides that.

## Build and generate

Native tooling uses Rust 1.93.1. The default feature set does not compile the
foreign-language runtime; it remains outside the default CLI dependency graph.
Enable `bindings` for the records and synchronous session facade, or `bindgen`
for the host generator. Bindings use the shared transport's platform-neutral
`runtime` feature. `local` adds only the Linux/macOS Unix connector; it does not
change the exported Swift API. iOS does not compile Unix discovery. Without the
local feature or on an unsupported platform, the same `local()` method returns
`ClientError::InvalidInput { field: "local", reason: "local_unavailable" }`.
The iOS app offers only its separately approved gateway connection setup.
The generated `ClientObserver` protocol provides `changed(notice:)` and
`terminal(chunk:)` signatures for platform adapters. Implementations enqueue
onto their actor/UI executor. The Rust service delivers callbacks outside its
state and queue locks. `observe` returns a fresh subscription ID and replaces
the previous observer. `removeObserver` ignores an obsolete ID. Removal stops
queued delivery; an already executing callback may finish. Platform callbacks
must check the client instance and full scope before publishing queued UI work.
Use weak references back to the facade to avoid retaining an observer cycle.
Explicit `close` releases callbacks, timers and channels without stopping Crew.

`local()` reads only the protected operator registry through the shared local
adapter; it refuses agent context and does not connect to a daemon. Run discovery
on the platform worker. `localProfiles()` returns the construction-time discovery
snapshot as opaque profile ID, workspace and optional project. It contains no
socket, token, PID, start time or credential handle, and never supplies verified
scope. The IDs belong to that client instance; creating a fresh discovery
snapshot uses a fresh client. An unknown ID or a call on a disposed client is a
typed error before any dial. `connect(profileId:)` schedules the actual protected
authentication and snapshot flow.

The facade's `query`, `plan`, confirmation and lifecycle methods call the
service directly. `plan` returns only `ActionPlan`; effects are executed in
Rust. `connect(profileId:)` resolves an opaque configured selection, so callers
cannot supply an authenticated endpoint or grant. No facade method exposes
protocol frames, a caller-supplied clock or transport command encoding.
`workspaces()` returns the authenticated endpoint's optional current listing;
before an index is verified it stays absent. `connectionContext()` likewise
returns only an established context, without a synthetic workspace identity.

From the repository root, after applying the separate workspace registration:

```sh
./scripts/build-env cargo build --locked -p crew-client-ffi --features bindings,local
```

Run the generator from this crate's directory, with absolute artifact paths:

```sh
../../scripts/build-env cargo run --locked --features bindgen,local \
  --bin crew-client-bindgen -- generate --library <absolute-host-library> \
  --language swift --no-format --out-dir <absolute-staging-directory>
```

The generated Swift file is `CrewBindings.swift`; the C module is
`CrewClientFFI`, with `CrewClientFFI.h` and `CrewClientFFI.modulemap`. Their
committed copies live under
`apps/apple/CrewClient/Sources/Generated/{Swift,Headers}`. No formatter may
rewrite them. The Apple package/project owner stages the separately built
XCFramework before linking; a Linux `.so` is not an Apple binary.

## Verification and provenance

```sh
./scripts/build-env cargo test --locked -p crew-client-ffi --features bindgen,local
./scripts/build-env cargo clippy --locked -p crew-client-ffi --all-targets --features bindgen,local -- -D warnings
```

Tests exercise real UniFFI buffer converters with the repository's generated
wire fixtures, integer extremes, nullable fields, metadata, scoped terminal
identity, boxed views and malformed buffers. The generator test compares the
unformatted output byte for byte and checks the full Swift identity types are
`Hashable`. Protocol frames, commands, effects, authentication records and
credentials are excluded from the foreign surface.
The freshness test builds its own actual host library, so it also works on a
clean target directory without relying on an earlier build command.
It generates from both `bindings` and `bindings,local` host libraries and
compares each against the same committed sources. This detects an accidental
local-only exported class or method; it is not an Apple cross-compilation test.
Service tests use the actual `NativeSession` with an explicit test-only
`ClientApi` probe. They verify typed delegation, callback query reentry,
replacement, stale removal, blocked-callback teardown, disposal from a callback,
last-reference cleanup and finite panic errors. Separate tests compose the real
core, reduce a generated repository snapshot and query it through the facade.
The real local factory is tested in a child process with a private empty registry,
including its agent-context refusal, and profile IDs are checked without a dial.
These tests do not claim a real-daemon application journey; that remains an
integration gate.

This checkpoint consumes reviewed N2A `a159bd03`, following portable RPC
extraction `93c852cb` and the approved N1 source, plus API-only alignment
`bfadc1cd` of the released `df8ecf1c` / `982cca4e` amendments. Both
SentUnacknowledged and ClosedLocally retain their attachment identity in notices;
Core-owned final outcomes stay in the Rust update/driver path. Later approved
API amendments must update the remote declarations and regenerate the committed
output before live integration. The codec/policy files are exact reviewed
`134ac54b` source; the transport crate is exact reviewed `aa0dc24a` source.
Its source-only import and workspace/lock registration are separate commits.
The complete client package, including its test set, is byte-equal to the
reviewed producer `93266067`, including the actual action/projection modules and
unchanged reviewed API. Its 133 tests and the 20 FFI tests pass together. The 31
transport tests also pass with that exact core and the FFI on the same graph,
including real isolated-daemon authentication, the shared-core retry lifecycle,
bounded terminal chunks and observer behavior. All Git, Crew and Herdr context
overrides were removed from the transport test environment. These results do
not replace final native app journeys or gateway composition.

Strict Clippy, formatting, both-feature generation freshness and the default
dependency graph checks pass. The same lockfile also builds and links the
default `crew` binary with Rust 1.82.0; running that binary's `--help` passes.
Native-only dependencies remain outside that CLI build. Workspace membership
and lockfile changes are isolated for QA's final integration.
Source generation and Rust tests run on Linux. Apple Swift
compilation, linking, simulator/device execution and signing remain explicitly
deferred by the operator; this checkpoint claims none of those results.
