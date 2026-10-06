# Shared core and local bridge integration

This checkpoint composes the reviewed portable client engine, action planner,
projections, local Rust session driver and UniFFI bridge. Production web policy
uses the shared Rust descriptors; its embedded assets were rebuilt and checked.
The combined default CLI still builds with Rust/Cargo 1.82.0. Native Rust checks
use 1.93.1. Apple verification remains deferred by the operator; this checkpoint
does not claim a connected native application.

The integration starts at landed `1fd2272a29226b9245d5d6fa1e0b4446c08783b0`.
Final product source is `84befae2f02b3dac38c5fc08d5a1cd6ee7f90774`; this report
adds evidence only. The [retained inventory](core-integration.json) records
original/imported commits, 65 exact source blobs, 385 unchanged protected files,
manifest and lock details, executed checks and log hashes. The recovery and
Git-isolation baseline remains intact.

## Exact inputs

| Reviewed source | Integration |
| --- | --- |
| Core `93266067` | `b2554a04`: exact client crate, including the approved action/projection prerequisites and retirement behavior. Subsequent differences are only the amendments below. |
| Raw input `c0265aa7`, const compatibility `19733832`, Attach notice `cda71035` | `69fec49c`, `b6ae3cf5`, `84befae2`. Successful Attach identifies its attachment with the operation ID before output; unrelated successes retain `None`. No record, ABI or wire change. |
| Projection cleanup `c0911ff6` | `722a656d` removes only the temporary seven-case duplicate projection harness. Production projection modules still equal Core `93266067`. |
| Local transport `aa0dc24a` | `ae15405b`: all 15 crate files exact. No private provisional core import or gateway module. |
| FFI `cc30064c`, generated ABI `66be8de8` | `bf5b0af2`: all 13 crate files and three generated Swift/header/module-map files exact. Both feature selections regenerate identical unformatted output. |
| Shared web policy `5333e1e7` | `e576a7b5`: six production/test/emitter files exact. W12 amendment applied onto `1fd2272a`, with all 14 approved baseline authorization lines retained; no prose dropped. |
| Architecture `252ecc31` | `884705a9`: exact approved bridge and Attach-notice clarifications. |
| Metadata and source references | `bd42a40e` plus ordering-only `78b46af9`; `b7c928ec` relocates four N7 anchors and records their historical definitions. Content authorization `m-4742323e57e4`, reviewer approval `m-94924b9fb271`. All cases, checker, 85 reference IDs and byte goldens remain unchanged. |

The protocol, CLI/runtime source, core compatibility exports, TUI, desktop,
existing Apple shell, validation scripts and earlier reports equal the baseline.
Only the three generated Apple files are added there. Gateway source/connectors,
platform UI changes, TUI adoption and the later action-availability API are
outside this checkpoint and remain final native-app integration inputs.

## Package and verification results

Eight workspace members retain the same five portable/CLI default members.
The combined lock has 280 package records: all 249 baseline versions/checksums
plus 31 additions from the reviewed FFI graph. The earlier 32-record FFI delta
included `semver 1.0.28`, already present through GTK. Existing edge changes are
client `base64`, `indexmap`/`semver` serde, and the reviewed `windows-sys 0.61.2`
choices in `errno`, `rustix` and `tempfile`, plus duplicate-name annotations.
Clap 4.5.61, UUID 1.20.0, indexmap 2.13.1 and the other compatible pins remain.
Native packages and GTK/UniFFI/rustls/Ed25519 dependencies do not enter the
portable/default CLI normal graph. No Rustls or Ed25519 package was added.

| Check on the composed source | Result |
| --- | --- |
| Locked metadata; formatting; scoped Clippy, all targets, `-D warnings` | Passed. Client, local/real-core transport and FFI; FFI also without local. |
| `cargo test --locked --workspace -- --nocapture` | 1127 passed, zero failed, two explicitly ignored. No sandbox self-skip messages. Client library 70, actions 35; TUI render 60. |
| Local transport, `local,real-core-tests` | 31 passed, including the actual shared-core lifecycle consumer. |
| FFI, `bindgen,local` and `bindgen` | 20 passed each; actual host-library generation fresh for both `bindings` and `bindings,local`. |
| Actual Rust/Cargo 1.82.0 | Locked default CLI build, version and top-level/run/attach/web/stop help passed; portable check and concrete const consumer passed. |
| Portable WASM and shared vectors | Both crates passed. State: 31 wire/TUI steps and five web cases. Actions: 127 cases, 15 methods, 85 anchors, 24 pure cases; 64 Request byte goldens, nine valid/11 invalid decode cases. |
| Production web and security | Units 200, mock 226 including five focus cases, bundle budgets five, `web_bundle` two, Rust wire/auth/server 60, contrast 57; `just web-check` passed. |

The two ignored tests are `two_real_claude_code_agents_talk_directly` (requires
token-spending CLI access) and `net::tests::live_dashboard_against_a_scripted_daemon`
(requires an interactive terminal). Optional GUI targets were not enabled;
zero-test targets do not count as app verification.

Every runner cleared `GIT_*`, `HERDR_*`, `CREW_IDENTITY`, `CREW_SOCKET` and
`CREW_LAUNCH`. Daemon tests used private runtime/state/configuration directories.
The first workspace attempt encountered two unchanged Unix socket fixtures:
its long target path made `late.sock` 112 bytes, beyond the 108-byte limit.
The shorter worktree-local `target/ci` executed both controls and the complete
final suite successfully. No source workaround was applied. The first action
checker failure identified N7 reference relocation; the reviewed provenance
amendment corrected it without changing fixture behavior.

Logs remain under
`/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/native-validation/core-integration/`.
The initial runner preflight, failed workspace/reference checks and their
successful corrections remain retained alongside final logs. Cargo caches are
inside this worktree; the MSRV cache is
`target/native-validation/git-isolation-msrv`.

## Embedded assets and measured performance

The rebuilt manifest source is `fnv1a64:a249c3169f3b8a02`.

| Budget | Bytes | Ceiling | Remaining |
| --- | ---: | ---: | ---: |
| P1 dashboard scripts | 230497 | 230500 | 3 |
| P2 stylesheet | 33906 | 34000 | 94 |
| P3 terminal scripts | 332915 | 350000 | 17085 |
| P4 complete bundle | 598004 | 600000 | 1996 |

All measurements use `CREW_PERF_SCALE=1`. The authoritative complete quiet perf
run passed eight cases in 89.0 seconds: cold FCP maximum 76 ms, live maximum
158.4 ms, frame p95 at most 16.8 ms with no long tasks, terminal echo p95 5.1 ms,
zero idle animation frames and zero idle index traffic. Bounded real-daemon
P18/P20 cases passed: question badge p95 251 ms, six connections with no
unexplained wake-ups and server RSS 28.1 MB using the private debug binary.
The cached P16 bridge measured p95 84.73 microseconds across 300 samples.

The initial eight-case perf attempt passed while a peer's GTK check overlapped;
its log/JSON is retained as the first attempt. Four focused quiet repeats also
passed, then reviewer `m-a41e68bee8c2` required the complete quiet serial repeat.
That final result is authoritative. No source, scale or ceiling changed.
The accepted N7 owner's earlier 208 ms cold-FCP failure and subsequent unchanged
124/144.9 ms pass remain separate owner evidence, identified in the inventory.

No operator service, listener, device or root checkout was changed. Production
activation still requires the operator's rebuilt binary and dashboard reload;
the previously recorded old demo daemon image remains an activation gap.
Full native app journeys, gateway/availability bindings, Attach ordering through
the real native observer, platform accessibility and Apple build/link/simulator
or device execution remain their separately assigned acceptance gates.
