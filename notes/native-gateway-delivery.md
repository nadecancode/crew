# Native gateway implementation evidence

Task `native-gateway`, contract `bd608ce0` + `0a50c4ca`. Implementation starts
with portable wire `5b4570e9`, connected server `0b1219d2` and security/lifecycle
regressions `47dafe0d`. Reviewed default CLI correction `3b2a1c96` is applied;
`615e8d71` resolves only three disambiguated getrandom lock edges. Reviewed N2A
RPC extraction is aligned at `b345131f`, preserving the native export and
daemon-version guard; released API/terminal records and codec are imported
without modifying their behavior. Other native UI/transport work is not merged.

## Implemented boundary

The optional `native-gateway` feature exposes explicit local administration
and a native-only TLS 1.3 endpoint. Default CLI builds contain none of rustls,
tokio-rustls, ed25519-dalek, ring or zeroize in their normal dependency tree.
There is no listener until an outside-agent operator runs `native start` with
an explicit unicast address, port, public endpoint and protected TLS key.
`native paths` provides the exact runtime locations before provisioning.

The native endpoint accepts only the bounded `/native/ws` upgrade with the
configured Host and no Origin/query/body. It serves no assets or metadata HTTP
routes. A connector verifies TLS chain/name/time and the configured leaf hash
before device proof. The gateway verifies strict Ed25519 proof against a fresh
connection nonce and canonical endpoint/certificate/purpose/workspace bytes.
Unknown handshake envelope fields are rejected; protocol 6 remains unchanged.
The shared comparison vector has 174 bytes and code `a4d3b046e1ee4cf0`.

Enrollment uses a local 120-second window, signed intent, accepted-intent
notice, comparison on the trusted console and exactly one original-request
completion on approval. Only public device keys reach the runtime registry.
Read is default; Control and Terminal require separate explicit host grants.
An exhaustive test covers all 67 daemon methods on every tier combination and
channel purpose. `models_list` is Control on Action purpose. `summary` is an
internal index watcher, never a device-forwarded call. Git reads require a
named agent. Input/resize bytes have no fabricated daemon acknowledgement.

Each device sees only selected memberships, grant metadata and per-device
listing revisions; `others=[]`. The gateway authenticates directly to checked
Unix daemon sockets with protected operator tokens and `web:true`; no device
receives those tokens and no browser membership is changed. Request policy
and live authority are checked before each upstream write.

Revocation commits inactive authority before canceling and draining the
affected channels. Successful reply means no later client byte can be
forwarded; already-forwarded actions may finish. Other devices and local web/
TUI remain live. Grant generations increase on edits/removal/rejoin, preventing
old-context reuse. Deadline-based device expiration closes existing sessions.
Gateway restart preserves records while runtime storage exists; loss of that
storage creates a new endpoint with no grants and requires new-key enrollment.

Registry and private key stay beneath the protected runtime/native directory,
independent of XDG_STATE_HOME. Private files require same-user mode 0600,
regular single-link files and no symlinks. A failed authority save stops the
native service rather than publishing unconfirmed rights. Startup flock is
acquired before opening authority storage; old control sessions are retired
before the socket/credential/lock can be reused.

## Real Linux acceptance coverage

`crates/crew/tests/native_gateway.rs` uses real daemon/gateway binaries,
temporary runtime/state/workspace paths, synthetic keys and only loopback TLS
listeners. The native certificate/key fixtures are explicitly public test-only
identities. Production loading rejects their original paths.

| Contract case | Permanent evidence |
| --- | --- |
| G01 TLS/off/bind | Default build refuses native operations; explicit endpoint validation; wrong TLS pin/name and expired certificate rejected before proof; external/link/symlink key rejected. |
| G02 routes/disclosure | No assets/query/version route; Origin 403; unauthenticated injected token never reflected. |
| G03 enrollment | Signed device proof, host window and local approval against actual server; agent-context enrollment CLI refused; registry contains no private device key. |
| G04 invalid/replay/cancel | Wrong nonce/endpoint/certificate/purpose, reused signature, canceled live intent and five wrong comparisons; one consumed intent cannot approve again. |
| G05 workspace privacy | Two devices/two real daemon workspaces see exactly their own metadata; unauthorized workspace id denied, `others` empty. |
| G06 policy | All 67 source methods, 4 tier combinations, all 5 purposes; raw Read mutations denied; actual Control sends, Terminal echo and named read constraints. |
| G07 secrets | Synthetic injected secret absent from error; private key absent from registry; local control token absent from public profile; proof Debug redacted. |
| G08 order/backpressure | Subscribe reply first, snapshot includes preceding message and later push follows it; slow subscribed reader does not block an independent real read. |
| G09 revocation | Open real state and PTY connections close with device/grant codes after revoke; another device and local daemon commands continue. |
| G10 lifetime | Short device expiry closes an existing index; gateway restart preserves revoke; daemon stop/start requires a fresh connection and snapshot; edited grant generation increases. |
| G11 failure/stop | Failed atomic save reports failure and closes clients; saved old record remains unchanged; native stop/restart and private key path failures exercised. |
| G12 budgets | Fifth unauthenticated connection on one source cannot enter TLS; existing authenticated device/control remain usable; bounded attempt/pending/frame/read limits in server. |
| G13 sandbox | Real sandbox cannot read runtime registry/key/control token with XDG_STATE_HOME both inside/outside home; positive unsandboxed reads precede denial. |
| G14 legacy | Four existing web/operator-auth suites retain their source and protocol fixtures and pass: 14 + 21 + 16 + 9 = 60. |
| G15 route proof | Fresh registry/socket plus same-dial zero-history snapshot verifies workspace/state directory/PID/start for every workspace purpose. Wrong registry start/PID/workspace refuses the open; a listing/open restart race returns the actual new daemon. A held real-daemon hello is canceled by grant revocation without an opened reply; Index has no route and Terminal still cannot request a snapshot. |

Commands recorded during implementation:

```sh
./scripts/build-env cargo check --locked -p crew
./scripts/build-env cargo check --locked -p crew --features native-gateway
./scripts/build-env cargo clippy --locked -p crew --features native-gateway --all-targets -- -D warnings
./scripts/build-env cargo test --locked -p crew --features native-gateway --lib native::
./scripts/build-env cargo test --locked -p crew --features native-gateway --test native_gateway -- --test-threads=1
./scripts/build-env cargo test --locked -p crew --test native_gateway
./scripts/build-env cargo test --locked -p crew --features native-gateway --test web --test web_server --test web_wire --test operator_auth
./scripts/build-env cargo test --locked -p crew-protocol -p crew-core native
cargo tree --locked -e normal -p crew
cargo tree --locked -e normal -p crew --features native-gateway
```

Original reviewed delivery results: 15 real integration tests and 11 native unit tests
passed, including expired TLS identity and the complete tier/lane policy matrix.
The four existing suites passed all 60 tests after compatibility-lock repair.
The all-target clippy gate passed without policy/auth lint exemptions. The
portable native tests and permanent daemon-version equality guard passed.
The final review artifact pins these source and lock checks.

## Verified route correction

Task `native-gateway-route-proof` implements the narrow native-v1 amendment
approved in `m-3560e1e578bf` and `m-c7af4e8af85c`. `GatewayRoute` carries the
protected workspace id, immutable native membership generation and v6
`DaemonRef`. Workspace, Read, Action and Terminal opens require a route;
Index has none. Core/FFI records, v6 bytes, grants and the browser stay unchanged.

`native/route.rs` checks a fresh registry entry and owned derived Unix socket,
then performs the internal zero-history snapshot on the connection used by
subsequent device calls. Its PID/start and workspace/state paths must match
the protected membership, registry and hello. The internal request/reply is
consumed before `GatewayOpened`; no snapshot becomes a device frame or read
outcome. A 3-second shared deadline, existing pending budget and live authority
checks bound verification. Membership/grant checks after verification refuse
revoked routes. A shared immutable membership-generation constant preserves
the existing index generation 1; it is not a grant counter or team start time.

Correction validation used these commands:

```sh
./scripts/build-env cargo test --locked -p crew --features native-gateway --test native_gateway -- --test-threads=1
./scripts/build-env cargo test --locked -p crew --features native-gateway --test native_gateway enabled::grant_revocation_cancels_route_verification_without_an_opened_reply -- --exact --test-threads=1
./scripts/build-env cargo test --locked -p crew-protocol -p crew --features native-gateway --lib native::
./scripts/build-env cargo test --locked -p crew-protocol -p crew-core native
./scripts/build-env cargo clippy --locked -p crew --features native-gateway --all-targets -- -D warnings
./scripts/build-env cargo fmt --all -- --check
./scripts/check-msrv
```

The full integration run passed 17/17, including all four verified purposes,
wrong registry start/PID/workspace and the real listing/open daemon-restart
race. The subsequently added held-verification grant-revocation case passed
1/1 in its targeted run; the suite now contains 18 cases. Native unit tests
passed 11/11, portable native tests 4/4, and all-target feature clippy/fmt passed.
The actual read-only Rust/Cargo 1.82.0 default CLI build and help gate passed
again with this correction. Cargo manifests, lockfile and legacy web/daemon
test sources remain byte-unchanged from the reviewed gateway checkpoint;
the 60 legacy-suite result above belongs to that earlier delivery, not a new
fixture run. No active operator service or listener was changed.

## Default compiler compatibility

Actual read-only Rust/Cargo 1.82.0 binaries from the scheduler's isolated
toolchain were used against this integrated source/lock. Output:

```text
rustc 1.82.0 (f6e511eec 2024-10-15)
Finished dev profile ... in 18.97s
crew 0.1.0
```

The linked default binary and `--help` passed. Build used this worktree's own
`target/native-msrv`; no scheduler target/cache/source or active service was
changed. Native crypto/TLS is separately feature-gated; newer-toolchain native
success is not used as proof of the default CLI floor. Compatible package pins
remain Clap 4.5.61, Lex 1.0.1, UUID 1.20.0, instability 0.3.10/darling 0.20.11,
indexmap 2.13.1/hashbrown 0.16.1 and unicode-segmentation 1.12.0.

## Setup and remaining platform evidence

README documents feature-enabled installation, runtime paths, explicit bind/
endpoint/TLS provisioning, console enrollment/approval, explicit grant powers,
individual revocation, stop and runtime-loss re-enrollment. No active operator
gateway, dashboard or team was started, restarted, exposed or paired by this
task. All service mutation above belongs to isolated tests.

Apple compile/simulator/secure-store verification is deferred by the operator.
The native connector and actual applications own device key storage, TLS
verification and the approved setup screens. This server evidence does not
claim an Apple or GTK end-to-end app run; transport integration remains its
own board gate against this actual server and reviewed shared engine.
