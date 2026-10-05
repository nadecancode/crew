# Reviewed native foundation integration

The foundation combines portable protocol/client records and shared queries with
the native Linux and Apple navigation shells. Linux builds and opens a real,
disconnected GTK window. The default CLI still builds with Rust/Cargo 1.82.0 and
the combined lockfile. Apple project, resource, manifest and syntax checks pass;
**Apple verification deferred by operator** applies to compilation and execution.
This checkpoint does not provide a connected native application.

The integration starts at root `f3e9f2343934c665ec9a557688c3bcf4b5eaac4d`.
Final product source is `706bd6358b4e9d4390b3ac907350c0a5ecc70ce3`;
subsequent evidence imports and this report do not change that source. Portable
results from the reviewed API checkpoint apply through exact source equality;
the integrated lock, default CLI and final GTK window were checked after package
registration. The [retained inventory](foundation-integration.json)
records exact original/imported commits, compared source hashes, lock changes,
executed checks and the [actual integrated capture](foundation-integration.png).

## Source provenance

| Accepted input | Integrated result |
| --- | --- |
| N1 `c8894ee0`, `a8f3a762` | `6c00b532`, `8e221c61`: portable `crew-protocol`, shared `crew-client` order/current-work functions and existing core/TUI compatibility exports. |
| State `dced9fed`, `1b86bd5e`, `d44d3ba0`; actions `b8d29004`, `2b18fb1d`, `7f23df92`; permanent consumer `418d2085` | Exact corpus/helper/test commits. Common native/TUI expectations and explicit legacy-web overrides stay separate. The two terminal-agent fixture inputs use `Stopped`; `Done` is not a terminal process status. |
| N2A `93c852cb`, `0cad5304`, `ef742690`, `c8535406`, `a159bd03` | Exact portable RPC extraction and API/schema source chain. No reducer, command codec, transport or live service implementation. |
| API amendments `df8ecf1c`, `982cca4e` | Reviewed API-only alignment `bfadc1cd`: six blobs equal `982cca4e`; the old `a159bd03` command schema loses only the removed `CommandResult::Null` constructor. Sent-unacknowledged notices preserve attachment identity; core-owned outcomes remain typed records. Unreviewed N2C support/codec files are absent. |
| GTK shell `c45703ff`, `d4d31a13`; registration `32ab8f0e` | Source imports `1d350b22`, `5477493a`; coordinated registration `173431b6` retains compatible default-CLI pins. Empty-pane correction `706bd635` changes only the static empty-state scroller and its focused regression. The other 18 desktop files equal `d4d31a13`. |
| Apple shell `4479a301` | Import `163fe6fc`: all 39 files equal the reviewed source, including shared CrewMac/CrewIOS schemes, the presentation-only CrewUI package, resources and deferred unit/UI test sources. No CrewClient/FFI artifact is present. |
| Approved contracts and inventories | `b410c1c1` imports 12 exact snapshots: architecture `70cc103e`, core inventory `9b17ce15`, transport inventory `51183326`, gateway contract `0a50c4ca`, UX `5015460a`, immutable feasibility/probe `fc87ae36`, web-policy spike note `becbeea6`, and the 14-line W12 contract exception from `28681ceb`. These are documents/probe sources, not gateway or web-policy implementation. |

Reviewer `m-ca64201efca2` independently verified the 14 prerequisite imports by
stable patch identity and the API-only alignment by exact bytes. The retained
inventory independently compares all 12 document snapshots, six amended API
blobs, 39 Apple files and 20 Linux files with their stated source objects.
The original [tooling preparation](tooling-preparation.json), immutable feasibility
probe and [runtime environment evidence](linux-runtime-environment.md) remain
intact; the older development-binary capture is not used as evidence for this
integrated source.

## Package and lock integration

The workspace contains protocol, client, core, TUI, CLI and desktop packages.
The five portable/CLI packages are the default members. Desktop requires Rust
1.93, has empty default features and a binary requiring `native-ui`; GTK is
optional and Linux-only. Its reserved `local` feature currently enables no
transport. The build uses GTK4-rs 0.10.3 with the reviewed `v4_6` feature/API
floor; local toolkit execution is GTK 4.22.4.

Registration adds exactly the reviewed 42 native package records beside the two
new portable packages: 44 additions against `f3e9f234`, zero removed locked
versions. Existing MSRV pins remain Clap/Builder/Derive 4.5.61, Clap Lex 1.0.1,
UUID 1.20.0, instability 0.3.10, darling/core/macro 0.20.11,
indexmap 2.13.1/hashbrown 0.16.1 and unicode-segmentation 1.12.0.

The initial native lock overlay needed Cargo's dependency disambiguation for
duplicate native versions. An offline metadata resolution changed only edge
annotations in `crew`, `crew-core`, `futures-util`, `toml` and `toml_edit`:
no package version was added or removed by that resolution. Locked metadata,
the actual 1.82 default-CLI gate, native build and both normal portable dependency
trees subsequently pass. The protocol/client normal trees contain no GTK,
socket/runtime, configuration or credential dependency.

## Executed verification

All local commands run from the QA worktree. Native runners clear
`CREW_IDENTITY`, `CREW_SOCKET` and `CREW_LAUNCH` and create private runtime,
state, configuration, cache, display and D-Bus resources. The retained logs live
under `/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/native-validation/`,
not root checkout `target/`.

| Check | Result |
| --- | --- |
| `scripts/native/check.sh --conformance` | 33 portable/API tests: client 8/7/5/4/1/2 and protocol 6; TUI render 60; both portable crates compile for WASM; normal dependency trees checked. |
| Shared state corpus | 31 Rust DTO round trips preserve supplied fields and canonical equality; 31 TUI steps and five web cases pass. |
| Shared action corpus | 127 cases, 15 surfaced methods, 85 source anchors, 24 real pure-web cases; 64 Request byte goldens, nine valid and 11 invalid wire cases pass. This is not a daemon/dialog action-fixture consumer. |
| Default CLI compatibility | Actual Rust/Cargo 1.82.0 builds the locked `crew` binary with native packages registered; version/top-level/run/attach/web/stop help parses. No native 1.82 claim. |
| Strict static checks | Portable and default-feature workspace Clippy, all targets, `-D warnings`; native GTK Clippy and formatting pass. Default workspace Clippy runs without a GTK development environment. |
| Existing web/auth contracts after RPC extraction | Sanitized isolated `operator_auth` 14, `web` 21, `web_server` 16 and `web_wire` nine pass: 60/60. |
| Embedded dashboard | `web_bundle` 2/2; `web`, `web-dist`, CLI runtime/test source, build script and server manifest equal the accepted root baseline. No frontend/live browser matrix is repeated. |
| Actual integrated GTK startup | Rust/Cargo 1.93.1 builds `native-ui,local`; final incremental build 0.38 seconds. The real Team window is mapped at 1120×720, shows No team loaded/Disconnected and is captured. App and Xvfb children are gone after runner cleanup. |
| Native empty-focus regression | The focused test fails before the fix with `empty Tasks detail must focus the visible named destination control` (0/1); `706bd635` passes 1/1, preserving populated Entry/Button restoration and existing layout/lifecycle checks. |
| Independent real keyboard/AT-SPI replay | Designer `841e165e` rebuilds the exact corrected crate: empty Tasks detail and Alt+Left main focus the visible, showing Destination: Tasks control; F1 focuses Close; Escape restores Destination: Tasks. This is programmatic AT-SPI evidence, not a complete screen-reader journey. |
| Apple source checks | All 39 integrated files equal `4479a301`: the independently staged source graph has 390 structural checks, 101 objects, six targets and two shared schemes; 13 Swift files parse with Linux Swift 5.10.1; SwiftPM manifest has no external dependencies, processed CrewUI resources, iOS 17/macOS 14 and Swift 5 mode. No SwiftUI type checking or Apple SDK is used. |

The final startup capture records a 1440×960×24 private Xvfb display, 96 DPI,
GDK scale/DPI scale 1, X11/Cairo and disabled portals. Versions are GTK 4.22.4,
VTE 0.84.1, GLib 2.88.3, xwininfo 1.1.6, ImageMagick 7.1.2-29 and D-Bus 1.16.2.
VTE is supplied by the runtime helper; the shell does not yet render a terminal.
The MESA DRI3 warnings are expected with Xvfb and do not prevent the mapped
window or capture. Exact binary and image hashes are in the retained inventory.

Useful reproduction commands are:

```sh
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH scripts/native/check.sh --conformance
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH scripts/check-msrv
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH \
  ./scripts/build-env cargo test --locked -p crew \
  --test operator_auth --test web --test web_server --test web_wire
export NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source
scripts/native/linux-build.sh -j 2
scripts/native/linux-smoke.sh -- "$PWD/target/native-validation/cargo/debug/crew-desktop"
crates/crew-desktop/tests/run-window-test.sh
```

Install the actual 1.82.0 compatibility toolchain first, as `scripts/check-msrv`
requires; QA used a separate rustup home under its ignored artifact tree.
Full Apple runner commands and source-check limits are in the byte-equal
[Apple shell README](../../apps/apple/CrewApp/README.md). The complete-app
binding runners correctly require the later FFI/service artifacts and do not
substitute this shell for them.

## Remaining acceptance and activation

Reducer/planner/projections, local/gateway drivers, the FFI service facade,
connected platform content/actions and terminal rendering remain later reviewed
source and live integration gates. These foundations neither discover a daemon
nor enroll a device. The early shell review's native GMenu label/provider
observation NSR-L03 remains unclassified; no full Linux screen-reader acceptance
is claimed. Apple compilation, linking, simulator/device/UI behavior, VoiceOver,
signing and distribution remain deferred/unperformed. Complete-app N/C/G
acceptance rows are not passed by the API or shell checks above.

The accepted dashboard remains source `fnv1a64:b1e2e05bd16fb63d`, with P1/P2/P3/P4
230497/33906/332915/598004 bytes and only three P1 bytes of headroom. Native
web-policy implementation must still measure its combined candidate. No active
service or team restart occurred. Dashboard activation still requires a rebuilt
release image and reload; the old demo daemon PID 1925176 remains the separately
recorded operator activation gap.
