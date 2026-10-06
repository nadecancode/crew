# Native delivery: final review

Card: `native-final-review`. Verdict: **accepted at main
`f0dd95d4d301644a04f576ca293cec828a5b0513`**, the landed tip that carries the
Linux local and remote GTK app, the iOS and macOS SwiftUI sources with their
generated Rust bindings, the terminal interface on the shared client engine,
and the shared web dashboard, all on the reviewed gateway server and client
integration. Nothing in this review was fixed by the reviewer; every finding
went back to its owner and was retested before landing.

## Provenance

Main advanced from the gateway integration `2185ecb8` to `f0dd95d4` in 117
commits, of which 12 are merges inside composite tips (the Linux polish and
interaction chains, which were cross-merged by their owners and reviewed once
at the composite). Every fast-forward was a tip I had approved on its own
card after verifying it by git identity against the reviewed source:
patch ids for replayed commits, tree or blob equality for imported crates,
and the exact path set of each delta. The landing order was:

| Tip | Content |
| --- | --- |
| `3611fb04` | availability chain `c37bff6f`/`f3d73b16`/`75173b46`, menu notes, transport validation `5515e60e..4cde01b9` |
| `6f6c4e1b` | terminal interface on the shared client (`4739f494..20ea0631` replayed) |
| `1346b084` | private-home test fix `ed8901de` |
| `19b779bf` | Linux local app (`crates/crew-desktop` byte-equal `aafbf0dc`) |
| `729e2b98` | gateway port bindings and live FFI validation (`dc5f40c2..fe5701b7` replayed) |
| `2cb18b2b` | Apple apps (`apps/apple` byte-equal `7893ced9`) |
| `fe0a7813` | Linux remote gateway `39d09457` + focus rule |
| `b408ff7a` | gateway index kept across reconnects (client-only fix) |
| `fc26357d` | Linux graphs, layer bands, request backlog |
| `b3238139` | gateway route test race (test-only) |
| `59740228` | acceptance runner |
| `5b0359c9` | Linux content polish and interaction composite |
| `1e3b8a37`, `2da46639`, `3d40ed8d` | polish V1–V8, connect focus |
| `3c6c3d32` | application integration evidence (no product change) |
| `a8c8d109`, `5c5644c0`, `af0d8a72`, `f0dd95d4` | design notes, sign-off fixes R1–R3 and L1–L5, sign-off addendum |

Identities at `f0dd95d4`: `Cargo.lock` 305 packages, root `Cargo.toml` eight
members and the same five default members; generated Swift bindings sha256
`6039c186…`, header `d5ed3411…`, module map `cea45302…` (equal to the
released `62d2d7b6` and fresh for both binding selections); generated web
policy `adf70f8f…`; `crates/crew/src/process/pty.rs` `5b3aea7f…` (the
reviewed PTY lifecycle source); web bundle stamp `fnv1a64:a249c3169f3b8a02`,
unchanged since the reviewed dashboard.

Between the integration candidate `3c6c3d32` and the landed tip only
`crates/crew-desktop`, `notes/` and one acceptance case file changed; every
other crate, the lock, `web/`, `crates/crew/web-dist` and `apps/apple` are
byte-identical, so the Rust, web and Apple rows below were run once on the
tree they apply to and the Linux rows again on the landed tip.

## Architecture and invariants

- One shared Rust client (`crew-client`: reducer, planner, projections,
  availability query) drives the web dashboard policy, the terminal
  interface (`crew-tui` through `shared.rs`), the GTK app (`crew-desktop`)
  and the Apple apps (`crew-client-ffi`, UniFFI 0.31). No duplicated policy,
  grant or topology logic was found in the GTK or Swift sources; controls
  take eligibility and reasons from the Core's availability query and
  execution revalidates.
- No React Native, web view or WebKit anywhere: grep of the desktop and
  Apple sources and of the lock is empty. The Linux app is GTK 4 and VTE; the
  Apple apps are SwiftUI with SwiftTerm.
- Gateway security: pinned TLS 1.3 before any device proof, the operator's
  independently entered fingerprint as the only trust input (the GTK profile
  check is parse-only; the sole `GatewayProfile::trust` call uses the entered
  value), typed Ed25519 signing over Rust-sealed transcripts with no private
  key or store handle crossing a boundary, Read by default with explicit
  Control and Terminal grants, per-device and per-grant revocation enforced
  by the server and observed by both apps (`AccessRevoked`, device sign-out),
  newer listings that lower a grant generation refused as malformed, and the
  gateway index kept across reconnects so a workspace refusal never signs
  the device out.
- Confirmations: the four destructive actions confirm through the Core,
  Cancel sends nothing, and focus returns to the opener; Detach is a local
  close and no longer leaves a false pending status.
- Terminal: the renderer is keyed by the successful Attach's full-scope
  attachment id before any output; unknown chunks are dropped; PTY output
  closes on exit or after the 500 ms drain; identified sends survive the
  reconnect barrier with the original operation id and the new attempt scope.
- Default CLI: still Rust 1.82 with the committed lock; no GTK, VTE, UniFFI,
  rustls or Ed25519 package enters the default member graph.

## What I ran (slot R, fresh `target/review`, `CARGO_INCREMENTAL=0`, inherited Git/Crew/Herdr context cleared)

| Row | Tree | Result | Log |
| --- | --- | --- | --- |
| `cargo fmt --all --check`, `cargo metadata --locked` | 3c6c3d32 | pass | `target/rt-final-cargo.log` |
| default members `cargo test --locked --no-fail-fast` | 3c6c3d32 | 1150 passed, 0 failed, 3 ignored | same |
| strict Clippy: default members; `crew` native-gateway; transport journey-tests; FFI bindgen,gateway-tests / bindgen,local / bindgen | 3c6c3d32 | clean | same |
| `crew` `native_gateway` (real TLS, daemon, console) | 3c6c3d32 | 20/0 | same |
| transport journey-tests with a fixture CLI built from the tree (`0cedd49e…`); local,real-core-tests | 3c6c3d32 | 63/0; 38/0 | same |
| FFI bindgen,gateway-tests (live facade against an isolated gateway); bindgen,local; bindgen; generated freshness | 3c6c3d32 | 41/0; 39/0; 39/0 | same |
| wasm32 `crew-client`, `crew-protocol` | 3c6c3d32 | pass | same |
| Rust 1.82.0 `scripts/check-msrv` | 3c6c3d32 | default CLI builds, help parses | same |
| `just web-check`; vitest | f0dd95d4 (web = b3238139) | bundle matches sources; 200/0 | `target/rt-final-web.log` |
| Playwright mock + perf (P1 211,802 ≤ 230,500; P2 33,906 ≤ 34,000; P3 330,491 ≤ 350,000; P4 598,777 ≤ 600,000) | same | 234/0 | `target/rt-final-web-e2e.log` |
| `notes/web-revamp/contrast.mjs` | same | 57 pass, 0 fail | `target/rt-final-contrast.log` |
| TUI live journey on a real daemon with the tree's CLI | f0dd95d4 | 1/0 | `target/rt-final-tui-live.log` |
| Apple on Linux: `check-worker.sh` typecheck incl. 4 Foundation test sources; `check-source.py`; `swiftc -frontend -parse`; SwiftPM manifests | f0dd95d4 (apple = 2cb18b2b) | 0 errors (6 known Sendable warnings); 612 checks, 171 objects; 47 files; 2 manifests | `target/rt-final-apple-worker.log` |
| `crew-desktop` strict Clippy ×5 and fmt | f0dd95d4 | clean | shell |
| owner runners: window, `--client`, `--gateway`, `--keyboard`, `--interaction` ×3 | f0dd95d4 | 1/0, 8/0, 1/0, pass, 3/3 | `target/rt-final2-*.log` |
| acceptance on an own build (`crew-desktop` `b540e761…`, `crew` `88c298d7…`): local L01–L16, gateway C01–C08/C10/C12/C14–C16/C18, scale 2 L01/L02/L03/L10/L14 | f0dd95d4 | 16/16, 14/14, 5/5 | `target/rt-final2-accept-*.log` |

The same acceptance suites also passed on my builds of `3c6c3d32` and
`af0d8a72` during the component reviews.

## Handoff and documentation

`README.md` has the native section; `crates/crew-desktop/README.md`
documents the GTK build, runners and the AT-SPI interaction command;
`apps/apple/CrewApp/README.md` and `NATIVE-IMPLEMENTATION.md` give the
signing-neutral Xcode project, the pinned Linux source checks, the test
inventory and the deferred-run checklist; `notes/native-validation.md` has
the acceptance runner; `notes/native-apps/visual-signoff-20261006.md` is the
design verdict with its recheck of R1–R3. `scripts/native/apple-bindings.sh`
regenerates and checks the bindings from the committed FFI source.

## Limits

- Apple execution is deferred by the operator: no Swift compilation,
  linking, simulator, device, Keychain or VoiceOver run has happened. The
  sources, project graph, generated bindings and runner commands are checked
  on Linux only, and the implementation notes list the checks the Apple run
  still owes (Keychain prompt on the cooperative pool, `AccessibilityNotification`
  import, debounce if typing lags).
- Linux was verified under X11 with GTK 4.22.4 and VTE 0.84.1 on private
  displays; Wayland, Orca listening, system large text and high contrast are
  unverified. Unscripted gateway cases: C09 filtered listings, C11 lost
  approval reply, C13 network loss during enrollment, C17 expiry and renewal,
  C19 screen-reader listening.
- The GTK gateway Connections dialog still logs a pre-existing
  "min height of 336 … overall min height of 398" GTK warning; cosmetic, not
  a critical.
- Nothing was published, signed, installed or started: no operator listener,
  gateway, team or device changed.

## Remaining operational steps

1. Run the Apple project on an Apple host from `apps/apple/CrewApp/README.md`
   (build, unit and UI bundles, Keychain on a development-signed build) and
   record the deferred checks.
2. Optionally extend the acceptance runner to C09/C11/C13/C17 and run a
   screen-reader session for C19 and the Apple VoiceOver matrix.
3. Enable the `native-gateway` feature only where a device gateway is wanted;
   the default CLI stays unchanged.
