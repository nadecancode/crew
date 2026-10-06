# Native gateway binding validation

Card: `native-gateway-ffi-validation`. Scope: the Swift-facing gateway and
availability binding surface of `crates/crew-client-ffi`, verified on Linux
against the reviewed Core, driver and gateway connector and a real isolated
Crew daemon and native gateway. Apple compilation, linking, Keychain and
device or simulator execution remain deferred by the operator and are not
claimed here.

## Source under test

- Integrated main `6f6c4e1b` (later `1346b084`, which changes only `crew` CLI
  test fixtures), containing the reviewed Core, local and gateway transport,
  gateway server and the availability chain `c37bff6f`, `f3d73b16`,
  `75173b46`.
- Reviewed gateway port binding `dc5f40c2`, `bcbbb322`, `62d2d7b6`,
  `19c15498` (approved in `m-0c2df95d853c`).
- This card: `f3126502` (live facade test, `gateway-tests` feature, lock
  edges) and the README and report commit that follows it. No binding source,
  transport, Core or server file changes; the generated Swift, header and
  module map are byte-identical to `62d2d7b6`:
  - `CrewBindings.swift` sha256 `6039c1861bb7b7725d79212ac93dc1fc121f8e0683a26e7fc9d53d90ee430568`
  - `CrewClientFFI.h` sha256 `d5ed3411b4e30e8b2549bed55b33cd2e1a80bf2216c140ef12304b5cf9bcf714`
  - `CrewClientFFI.modulemap` sha256 `cea45302e48911ea1339d55f1615ddf0a8e3abe2f924526957d7dd9828391a45`

The root lock delta of `f3126502` adds only `crew` and `crew-core` as
dependency edges of `crew-client-ffi` (dev-only, Linux and macOS). No package
is added or re-pinned; QA owns root lock reconciliation.

## Live facade journeys

`crates/crew-client-ffi/tests/gateway_live.rs`, enabled by the Linux-only
`gateway-tests` feature. Each test re-runs itself in a child process with
private `XDG_RUNTIME_DIR`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME` and global
config, and with every inherited `GIT_*`, `CREW_*` and `HERDR_*` variable
removed. The child starts a real daemon on a temporary workspace and the real
native gateway on an ephemeral loopback port with the transport's synthetic
TLS fixture. Client steps use only the exported objects; console steps use the
gateway's private admin socket, as `crew native` does. A Rust stand-in answers
the `DeviceSigner` callbacks through the reply objects.

1. `swift_facade_enrolls_connects_and_follows_real_grant_changes_then_retires_locally`
   - `parseGatewayProfile` reads the complete pretty-printed `native_status`
     output; `normalizeFingerprint` and `GatewayProfile.trust` accept the
     fingerprint typed as uppercase groups of four.
   - `CrewClient.gateway` returns a disconnected client and signs nothing.
   - Enrollment shows `Connecting`, then `AwaitingApproval` whose comparison
     code and key hash match the host's pending intent; console approval with
     a read-only grant ends `Approved` with the attempt's own context, and the
     host lists exactly that device.
   - `connect`, the authenticated listing and `selectWorkspace` give a live
     `ScopedGateway` context whose scope carries the real endpoint, daemon
     PID, start time and workspace session ID.
   - Read-only grant: the availability query and `plan` both refuse a send
     with the same reason; a history read dispatches, succeeds and its
     evidence is queryable.
   - A real Control grant change ends the route with `AccessRevoked`; the old
     scope stays unavailable, a fresh attempt keeps the membership
     generation, availability turns available and a send succeeds.
   - Workspace revocation ends the route while the endpoint stays
     authenticated; availability and planning refuse the revoked scope.
   - `retireDeviceAccess` retires the signer key and closes the client; the
     host still lists the device.
2. `swift_facade_enrollment_cancel_unknown_devices_and_key_recovery_follow_the_host`
   - An unenrolled key is signed out with `authentication` and gets no
     listing.
   - Cancel while enrollment signing is pending ends `Canceled`, closes the
     reply and leaves no pending intent at the host.
   - Cancel after the host accepted the proof ends `OutcomeUnknown` and the
     host created no device.
   - After approval, a new client with the same key recovers access by
     authenticating; enrolling that key again fails with `authentication`.
   - Device revocation at the console signs the device out.

Stability: 15 consecutive runs of the live test binary passed. Mutation
check: removing the client close from `retireDeviceAccess` fails journey 1.

The binding-level contract tests (`tests/gateway.rs`, 12 cases with a pinned
loopback TLS peer) and the reviewed transport gateway evidence
(`review/native/gateway-integration.md`, `review/native-transport-validation.md`)
are reused unchanged.

## Commands and results

Rust 1.93.1, `CARGO_INCREMENTAL=0`, owned target directory. Log:
`target/native-validation/gateway-ffi-validation.log`.

```sh
./scripts/build-env cargo test --locked -p crew-client-ffi --no-default-features --features bindgen,gateway-tests
./scripts/build-env cargo test --locked -p crew-client-ffi --no-default-features --features bindgen,local
./scripts/build-env cargo test --locked -p crew-client-ffi --no-default-features --features bindgen
./scripts/build-env cargo clippy --locked -p crew-client-ffi --all-targets --no-default-features --features <selection> -- -D warnings
./scripts/build-env cargo fmt -p crew-client-ffi -- --check
./scripts/build-env cargo metadata --locked --format-version 1
```

- `bindgen,gateway-tests`: 41 passed (unit 5, gateway 12, gateway_live 2,
  generated 2, records 11, service 9).
- `bindgen,local` and `bindgen`: 39 passed each; the live tests compile out.
- Generated-output freshness passes for both `bindings` and `bindings,local`.
- Strict Clippy passes for `bindgen,gateway-tests`, `bindgen,local`,
  `bindgen`, `bindings` and default features; formatting and the locked
  metadata check pass.

## Not covered

- Apple: Swift compilation, XCFramework linking, Keychain or Secure Enclave
  signing and device or simulator runs (operator-deferred).
- The platform signer in these tests is a Rust stand-in, not an OS store.
- Terminal channels through the gateway facade; the transport's reviewed K5
  journeys cover them below the binding.
