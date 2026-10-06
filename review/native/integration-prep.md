# Native integration preparation

This checkpoint stages the native inputs that were already approved but not yet
on main, so the final native application integration only adds the inputs that
are still open. It starts at landed main `2185ecb8` and adds product source
`178f0063`; this report adds evidence only. Root `Cargo.toml` and `Cargo.lock`
are byte-equal to main (302 lock records, eight members, five default members).

## What landed

| Reviewed source | Integration | Proof |
| --- | --- | --- |
| native-action-availability `c37bff6f` | `8ae35030` | `actions.rs` and new `actions/availability_tests.rs` byte-equal. Main equalled the `c37bff6f` parent. |
| native-availability-core `f3d73b16` | `26a020c7` | `api.rs`, `presentation.rs`, `tests/api_schema.rs`, new `tests/availability_core.rs` byte-equal. `engine.rs` is main plus exactly the `f3d73b16` hunks (clean three-way merge). |
| native-availability-bindings `5165ff56` + `a4252ec4` + `75173b46` (approved `m-cccc61a5702f`) | `8ae35030` + `26a020c7` are tree-equal to `5165ff56` / `a4252ec4`; `75173b46` cherry-picked as `73350a94` | Six FFI/Swift paths byte-equal. Generated `CrewBindings.swift` SHA-256 `db9b3632…`; header and module map unchanged. |
| native-menu-accessibility `9e0bbaa8` | `b7430cfa` | All 14 paths byte-equal (note, 12 evidence files, shell-review cross-reference). |
| native-transport-validation `5515e60e`, `939e97e1`, `b92eea6d`, `ea7488e9`, evidence `4cde01b9` (approved `m-e7f1604a041c`) | Cherry-picked as `da9e224e`, `9c00a5c8`, `e617e6b5`, `6a38a408`, `178f0063` | Every transport and review path byte-equal to `4cde01b9`. The only manifest change is its feature-only `journey-tests` line; no lock change. |

The availability chain was the original candidate (`73350a94`). The transport
approval arrived after the slot run had started, so its chain was added
afterwards and covered by an addendum within the same slot.

These sources are already on main by content, though most were imported
rather than merged: native-linux-shell `c45703ff`/`d4d31a13`/`32ab8f0e` with
the `706bd635` empty-focus correction (an ancestor; `crew-desktop` is a member,
not a default member), native-n1-conformance `418d2085`, native-connection-ux
`5015460a`, native-apple-shell `4479a301`, shell review `6fd9434f`/`841e165e`,
native-binding-runner `d0b1e4e9`, default-cli-msrv `3b2a1c96`, native-web-policy
`5333e1e7`, native-ffi `cc30064c`, retirement `1ff28477`, raw input `c0265aa7`,
const compatibility `19733832`, Attach notice `cda71035`, route proof
`759aea95`, generation `589dca0c`/`17d071cb`, listing `19c62f52`, PTY lifecycle
`f94d0d8b`, fixture/runner Git isolation `1122d936`/`95688127` and root Git
recovery `9749f01c`/`d4e0f979`. Each file these commits touched either equals main
or was later superseded by an approved landed commit. One intentional removal
was confirmed: `crates/crew-client/tests/projection_modules.rs` (`722a656d`).

## Checks

All runs cleared inherited `GIT_*`, `HERDR_*` and `CREW_*` variables and used
`CARGO_INCREMENTAL=0`. They ran serially in heavy slot D and reused QA targets
`target/ci`, `target/native-validation/cargo` and, for Rust 1.82,
`target/native-validation/git-isolation-msrv`. At least 151 GiB was available.

| Check | Result |
| --- | --- |
| `cargo metadata --locked`, `cargo fmt --all --check` | Passed on `73350a94` and `178f0063`. |
| Strict Clippy, all targets, `-D warnings` | Passed: default members; `crew` with `native-gateway`; FFI `bindgen,local` and `bindgen`; transport `gateway-tests,real-core-tests` and `journey-tests`. |
| `cargo test --locked --no-fail-fast` (default members) | 1127 passed, 0 failed, 2 ignored (real Claude CLI and interactive live dashboard). Client: library 75, `availability_core` 4, `web_policy` 5. |
| Rust/Cargo 1.82.0 `scripts/check-msrv` | Locked default CLI build plus version, top-level, `run`, `attach`, `web` and `stop` help passed. |
| WASM `wasm32-unknown-unknown` | `crew-client` and `crew-protocol` check passed. |
| FFI `bindgen,local` and `bindgen` | 23 passed each, before and after the transport chain. Generated Swift is fresh for both selections. |
| Transport `local,real-core-tests` | 35 passed, before and after the transport chain. |
| Transport `journey-tests` | 59 passed, including all three real journeys (local engine, scoped gateway with revocation, mutation reply loss). Fixture CLI was rebuilt from this candidate with `native-gateway`: 200714152 bytes, SHA-256 `7e4bb149155e8d474291bad9680c8375511bfd38c9a78d62c918dcd92ad4623f`. |

Generated web policy `web/src/generated/client-policy.ts` is byte-equal to main
(`adf70f8f…`); no web source, asset or bundle changed. Logs and the fixture
binary are under `target/native-validation/integration-prep/`; the step
summaries are `heavy-summary.txt` and `addendum-summary.txt`.

## What native-app-integration still adds

- native-linux-ui (graph) and native-linux-gateway (linux): local GTK app and
  remote S01–S08 with the OS credential adapter.
- native-apple-ui (ui), including byte-copied `CrewBindings.swift` from
  `75173b46` and its consumption of availability.
- native-tui-adoption (scheduler).
- native-gateway-binding-ports and native-gateway-ffi-validation
  (native-architect).
- Final root manifest/lock union, Rust 1.82 gate on the final lock, the live GTK
  app on an isolated daemon, a scoped gateway, the native-apps.md acceptance
  screenshots, web gates and the README.

No root checkout, operator listener, enrollment, credential, team process or
installed binary changed. Apple compilation, linking, simulator and device
execution remain deferred by the operator and are not claimed.
