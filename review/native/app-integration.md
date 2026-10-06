# Native application integration

This is the final combined verification of the native applications. Every
reviewed product source reached main by fast-forward; this card assembled none
of them again. The candidate is main `3d40ed8d` plus three QA commits
(`630b9278`, `75670082`, `3b84836b`) that touch only the acceptance runner,
the binding freshness script, `notes/native-validation.md` and `README.md`.
No product source, manifest or lockfile differs from main. The machine-readable
record is [app-integration.json](app-integration.json).

## What is on main

| Input | Landed |
| --- | --- |
| Gateway server and client integration | `2185ecb8` |
| Availability chain, transport validation (integration prep) | `3611fb04` |
| TUI on the shared client | `6f6c4e1b` |
| Private-home test fix | `1346b084` |
| Linux local app | `19b779bf` |
| Gateway port bindings and live FFI validation | `729e2b98` |
| Apple iOS and macOS apps | `2cb18b2b` |
| Linux remote gateway | `fe0a7813` |
| Gateway restart-then-revoke fix | `b408ff7a` |
| Linux graphs and request backlog | `fc26357d` |
| Gateway route test flake | `b3238139` |
| Acceptance runner | `59740228` |
| Linux content polish and interaction | `5b0359c9` |
| Polish V1, V6, V7 | `1e3b8a37` |
| Polish V2–V5, V8 | `2da46639` |
| Connect focus | `3d40ed8d` |

The earlier gates are ancestors of main: raw input `69fec49c`, const
compatibility `b6ae3cf5`, Attach notice `84befae2`, shared web policy
`e576a7b5`, PTY lifecycle `f94d0d8b` (byte-equal), gateway generation and
listing (in `2185ecb8`), web focus `27cf7c56`, Git isolation `0886ccd0` and
`95688127`.

## Manifests and lock

Root `Cargo.toml` has eight members and the same five default members
(`crew-protocol`, `crew-client`, `crew-core`, `crew-tui`, `crew`). Since
integration prep the lock grew from 302 to 305 packages (`vte4`,
`vte4-sys`, `io-lifetimes`) plus dependency edges for `crew-desktop` and
`crew-client-ffi`; nothing was removed or re-pinned. The actual Rust 1.82
locked default CLI gate passes, so the floor and its compatible pins hold.
No GTK, VTE, UniFFI, rustls or Ed25519 package enters the default CLI graph.
Native code uses Rust 1.93.

## Checks

All runs cleared inherited `GIT_*`, `HERDR_*`, `CREW_*`, `CLAUDE*` and
`CODEX*` variables and used `CARGO_INCREMENTAL=0`, in orchestrator slots.

| Check | Tree | Result |
| --- | --- | --- |
| `cargo metadata --locked`, `cargo fmt --all --check` | 2da46639 | pass |
| Strict Clippy, all targets: default members; `crew` native-gateway; transport `journey-tests`; FFI `bindgen,local` and `bindgen`; `crew-desktop` `native-ui,local,gateway-tests` | 2da46639; desktop again on 3d40ed8d | pass |
| WASM `crew-client`, `crew-protocol` | 2da46639 | pass |
| Default members, locked, no fail-fast | 2da46639 | 1150 passed, 0 failed, 3 ignored |
| `crew` `native_gateway` | 2da46639 | 20/0 |
| Transport `journey-tests` (three real journeys) and `local,real-core-tests` | 2da46639 | 63/0 and 38/0 |
| FFI `bindgen,local` and `bindgen`, with generated-source freshness | 2da46639 | 39/0 each |
| `scripts/native/apple-bindings.sh --check-source` (Rust 1.93) | 2da46639 | binding sources match |
| Rust/Cargo 1.82.0 `scripts/check-msrv` | 2da46639 | default CLI builds; version and help parse |
| TUI live journey on a real daemon | b3238139 | 1/0 |
| Web: `just web-check`, unit, mock, perf, contrast | b3238139 | match, 200, 226, 234, 57 |
| Apple on Linux: `check-source.py`, Swift parse, worker typecheck, SwiftPM manifests | b3238139 | 612 checks; 47 files; pass (six Sendable warnings); 2 manifests |
| `crew-desktop` owner runners: window, `--client`, `--gateway`, `--keyboard`, `--interaction` | 3d40ed8d | 1/0, 7/0, 1/0, pass, pass |
| Acceptance, local L01–L16 | 3d40ed8d | 16 cases, 101/101 checks |
| Acceptance, gateway C01–C08, C10, C12, C14–C16, C18 | 3d40ed8d | 14 cases, 67/67 checks |
| Acceptance at display scale 2: L01, L02, L03, L10, L14 | 3d40ed8d | 5 cases, 33/33 checks |

Only `crates/crew-desktop` and notes change between `b3238139` and
`3d40ed8d`, and only `crates/crew-desktop` and notes between `2da46639` and
`3d40ed8d`. The web bundle, generated web policy (`adf70f8f…`), protocol,
client, transport, FFI, Apple sources and lock are byte-equal across those
runs, so the earlier rows hold for the candidate. The `crew-desktop` rows ran
on the candidate itself.

The acceptance app is `crew-desktop` built from the candidate with
`native-ui,local,gateway` (SHA-256 `c7ffc980…`); the daemon and gateway CLI is
`crew` built with `native-gateway` from the same sources (`d851d9be…`). Each
acceptance run uses a private Xvfb display, AT-SPI bus, window manager,
keyring, short runtime directory and scripted team, and plays the trusted host
console with the real `crew native` CLI against a loopback gateway on the
synthetic TLS fixture. Cleanup left no process in any run. Cases, isolation
and repro commands are in `notes/native-validation.md` ("Linux app acceptance
run"); captures are under the run directories below.

## Coverage of notes/native-apps.md

Local operation covers N01–N13, N15 and N17: connection, five destinations,
current work in graph, list and inspector, task inspection without side
effects, questions, messages, send with exactly one delivery, agent controls
and unavailable reasons, the four confirmations with Cancel sending nothing
and focus return, evidence sections, the identified terminal attach with
input and resize, usage reported as not reported rather than zero, close and
reopen, daemon loss with last-known content and reconnect, layout at
1024×600, 560 px and display scale 2, and keyboard traversal.

The gateway cases cover C01–C08, C10, C12, C14–C16 and C18: setup without
starting anything, the public-profile form and its errors, fingerprint
mismatch and trust, key storage in the Secret Service, bounded refusal
without a host window, the comparison code matching the host console, a
refused mismatched approval, a host-selected Read grant, disclosures,
read-only unavailable actions, a Control grant that retires the old
authority, gateway restart with retained grants followed by revocation, and
local removal. C09 filtered
listings, C11 a lost approval reply, C13 network loss, C17 expiry and renewal,
and a screen-reader listening test for C19 are not scripted. The owners'
in-process `--gateway` journey separately exercises restart then revoke and
terminal input through the gateway.

Sharing: `crew-desktop`, `crew-tui` and `crew-client-ffi` depend on
`crew-client` (and the first and last on `crew-client-transport`). The Linux
app is GTK and VTE; the Apple apps are SwiftUI. Neither the lock nor the app
sources contain a web view, WebKit or React Native.

## Limits

Apple compilation, linking, simulator, device, Keychain and VoiceOver checks
were not run: they are deferred by the operator to an Apple runner and none is
claimed. The Linux app was checked under X11 with GTK 4.22.4 and VTE 0.84.1
on private displays; Wayland sessions, older toolkits, Orca listening, system
large-text and high-contrast settings remain unverified. No package was
published or signed. No operator listener, gateway, service or team was
started, changed or stopped.

## Evidence

Logs and captures in QA's worktree under `target/native-validation/app-integration/`:
`final-3d40ed8d/` (`crew-desktop` gates, owner runners, acceptance),
`final-2da46639/` (Cargo gates, binding freshness, MSRV),
`prefinal-b3238139/` (TUI live, web) and `apple-linux/`.
