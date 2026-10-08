# Shared feature client and native bindings

P4 adds the negotiated epics, asides, Docs and usage contracts to the shared Rust
client, transport and Swift bindings. Daemon service readiness remains P2/P3's
responsibility; defining a client record never advertises a daemon capability.

## Implementation

The app command inventory includes 28 new methods (59 total). Epic administration
is excluded. Native policy separates read, action and workspace observer lanes,
while preserving the existing protocol 6 Hello and signed gateway transcript.
Every eligible physical channel negotiates once before Snapshot/Subscribe or any
feature request. Old-peer `invalid`, intermediary `forbidden` and unsupported
feature responses retain the legacy views. Authentication failures close the
connection. The exact authenticated connection epoch accompanies feature replies.

The shared reducer applies epic task/board transactions atomically, ignores
repeated revisions and resynchronizes on gaps. Typed resources include the query,
workspace attempt and, for Docs, a lease for the issuing physical connection.
Replacing a read channel cannot revive its old cursors, snapshots or assets.

Observer registration uses the workspace lane and independent reply/revision
barriers. Watch state is bounded to one aside watch, one Docs watch and four
metrics watches. Removed asides purge retained bodies and pending ask bytes;
stale replies cannot restore them. Gaps hide retained aside text until an
authoritative read resolves the gone cause. Seen-here expiry invalidates retained
Docs views without silently selecting a newer version. Usage replies must preserve
explicit query filters, including run, range, grouping and price book.

Native projections use checked remote UniFFI records and converters, including
bounded strings/lists, exact decimal counters, nullable coverage, safe links and
the closed recursive document AST. Feature data never crosses the FFI as JSON.
Generated Swift is committed without formatting. New Docs/aside screens and Apple
execution remain outside P4.

## Validation

The six P4 checkpoints were replayed without conflicts onto main
394a189ece5e5b6f8e7a3818b725741121121824. Stable patch ids match each original;
`review/features-p4-validation.json` records the pairs, toolchain, evidence hashes
and pinned native binary. Subsequent commits preserve the legacy native grant
catalog and share exact usage query default resolution with P3.

All required author gates passed on the submitted source:

- Native-gateway suite: 891 passed, 0 failed, 2 pre-existing ignored tests.
  Bubblewrap and socat were available; sandbox tests ran in require mode.
- Protocol/client/transport/FFI suites: 294 passed, 0 failed. This includes nine
  feature driver cases, 14 real binding converter cases, two exact generated
  Swift freshness checks, real gateway enrollment/revocation and three native
  journeys using the pinned feature-built CLI.
- Strict workspace Clippy with all targets and native transport/FFI features,
  `cargo fmt --all --check`, portable WASM, Rust 1.82 CLI/core compilation and
  default native-package compilation all passed.

Reproduce from the repository checkout (with a C compiler, pkg-config,
bubblewrap and socat available):

```sh
cargo build --locked -p crew --features native-gateway --bin crew
export CREW_NATIVE_VALIDATION_BIN="$PWD/target/debug/crew"
export CREW_TEST_SANDBOX=require
cargo test --locked -p crew --features native-gateway
cargo test --locked -p crew-protocol -p crew-client -p crew-client-transport -p crew-client-ffi --features crew-client-transport/journey-tests,crew-client-ffi/gateway-tests,crew-client-ffi/bindgen
cargo clippy --locked --workspace --all-targets --features crew/native-gateway,crew-client-transport/journey-tests,crew-client-ffi/gateway-tests,crew-client-ffi/bindgen -- -D warnings
cargo fmt --all --check
cargo check --locked --target wasm32-unknown-unknown -p crew-protocol -p crew-client
cargo +1.82.0 check --locked -p crew --bin crew
cargo check --locked -p crew-client-transport -p crew-client-ffi --no-default-features
```

Clear inherited Crew/Git/Herdr fixture context as
`scripts/native/common.sh` does. Raw logs remain under `target/features-p4/`;
the tracked validation manifest hashes the completed gate logs. The fixtures use
isolated daemons, synthetic agents and local gateway credentials, with no provider
calls and no interaction with the live team's runtime.

The live gateway gate found a compatibility failure in the first implementation:
adding feature names to signed grant metadata exceeded an old native decoder's
31-entry bound. The final implementation preserves the original 29-entry catalog
byte for byte and uses negotiated feature capability checks under the existing
tiers. A pinned legacy catalog fixture and exhaustive connector/daemon lane tests
prevent that regression. An initial journey invocation lacked its explicitly
required CLI binary; final runs supplied the pinned build and passed.

P2/P3 own daemon readiness, source services and their integration. These tests
establish the shared client/transport contract and legacy fallback, not complete
feature-service or native-screen acceptance. Apple execution remains deferred;
Linux generation and converter tests do not establish an Apple build.
