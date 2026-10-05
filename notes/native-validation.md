# Native validation workflows

This is validation preparation for the operator-approved native iOS, Linux and
macOS scope. It does not report a Crew native application pass. Source/app
validation is a separate integration card after the reviewed implementation
lands. The operator deferred Apple compilation, simulator and device execution
to their Apple runner; no remote workflow, signing or publishing was performed.

## Ownership and reproducible environment

QA owns `scripts/native/**`, this document and `review/native/**`.
The source and feature names follow `notes/native-architecture.md`: portable
`crew-protocol`/`crew-client`; Linux `crew-desktop` with `native-ui,local`; FFI
`crew-client-ffi`, generator `crew-client-bindgen`; Apple project
`apps/apple/Crew.xcodeproj`, schemes `CrewMac` and `CrewIOS`.

Run scripts from any directory. Build/capture output stays under this
checkout's `target/native-validation/`; Cargo uses its `cargo/` directory.
Inherited `CREW_IDENTITY`, `CREW_SOCKET` and `CREW_LAUNCH` are removed. Output
directories must be new, stay under that prefix and contain no symlink or
traversal components. Missing source/toolchain produces a named nonzero result,
never a passing skip. No script touches the production listener or team.

On this NixOS 26.11 x86_64 host, use the verified Nix input when replaying:

```sh
export NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source
scripts/native/linux-env.sh --headless bash
```

`linux-env.sh` enters `nix-shell` with GTK4/VTE development files, GCC and
pkg-config, adding Xvfb, xwininfo, ImageMagick, D-Bus and AT-SPI for headless
capture. `nix shell` alone did not populate GTK's pkg-config development paths.
The host's default Rust is 1.93.1; `scripts/build-env` supplies a compiler shell,
**not a Rust version pin**. Native bindings explicitly use
`NATIVE_RUST_TOOLCHAIN=1.93.1` by default. The CLI/portable Rust 1.82 floor is
unchanged; compilation here does not establish that older-toolchain gate.

## Linux build, run and capture

After the Linux package lands:

```sh
scripts/native/linux-build.sh
scripts/native/linux-smoke.sh -- \
  "$PWD/target/native-validation/cargo/debug/crew-desktop"
```

The build is locked and uses `native-ui,local`. The smoke runner creates a
private Xvfb display and D-Bus session, isolated configuration/state/runtime/
cache directories, and an isolated working directory. Its high display number
avoids a lazy Xwayland server's `:0`. It accepts only a live, mapped, nontrivial
native window, then captures `screen.png`. Early exit, no window and capture
failure are failures. Cleanup terminates only the application/display children
that invocation started, with a bounded grace period.

Each image has actual window geometry, tool versions and observed settings:
1440×960×24 screen, 96 DPI, GDK scale 1 and Cairo renderer. Capture disables
desktop portals to keep service activation off the operator's desktop; native
portal/Wayland behavior is a separate real-session gate. These are supported
GTK capture settings, not app appearance acceptance.
([GTK running/debugging](https://docs.gtk.org/gtk4/running.html))

`linux-smoke.sh` proves startup/capture only. The final Linux card must add RPC
assertions for an actual disposable daemon: workspace read, named message send,
question answer, reversible control, confirmation cancellation/confirmation,
and terminal output/input/resize/detach. A screenshot or fixture service is
insufficient for that acceptance. Run destructive cases solely against the
isolated scripted team; do not pass the operator's socket/token/listener.

The transport owner supplies protected Unix setup and isolated daemon tests.
Reuse its real-daemon fixture rather than duplicating credential/bootstrap
logic here. Keep runtime/state/config private per run. Unix socket paths must
fit the platform limit; a long checkout requires a short owned runtime alias,
whose backing state remains in the artifact directory. Record that alias and
cleanup in final integration evidence. Existing `web/e2e/live/harness.ts`
documents the working scripted-team isolation, but its `/tmp` scratch layout
is not the output contract for these preparation scripts.

## Shared core and one conformance corpus

After N1 lands, run:

```sh
scripts/native/check.sh
scripts/native/check.sh --conformance
```

The first command runs locked protocol/client tests, unchanged TUI render
tests, WASM portability checks and normal dependency trees. The second also
consumes the reviewed fixture owners' checkers in place:
`fixtures/native/state/check.mjs` and `fixtures/native/actions/check.ts --wire`.
Node/Bun and the C linker are required. Both receive
`CREW_NATIVE_FIXTURE_CACHE=target/native-validation/fixture-cache` and the
worktree-local Cargo target. State/action data is not copied or reauthored.

Existing checkers establish baseline DTO/TUI/web and Request-byte compatibility,
not the future native engine's behavior. The engine/transport owners add actual
reducer/action/daemon consumers, stale-attempt and authorization regressions.
Retain explicitly labelled web completed-work differences. A first real-daemon
consumer must execute daemon/index/admission expectations and treat mismatches
as fixture defects. N1 relocates legacy source anchors; source references need
the fixture owners' reviewed relocation resolution before integrated replay.

Reuse current security suites (`web_wire`, `web`, `web_server`, `operator_auth`)
and the transport owner's new native regressions. Their passing baseline is
not a scoped-gateway or native-adapter pass. Device trust, workspace grants,
individual revoke/teardown, bad proof, version mismatch, snapshot/reconnect,
stale attempt and no mutation/input replay remain actual integration gates.

## Apple runner commands: verification deferred by operator

Prerequisites: committed Apple/FFI source, macOS with selected Xcode, Rust
1.93.1, an ordinary Git checkout of the reviewed commit, and the Rust targets
required by the chosen platform. Mac packaging
requires both `aarch64-apple-darwin` and `x86_64-apple-darwin`. iOS packaging
requires device `aarch64-apple-ios` and Apple-silicon simulator
`aarch64-apple-ios-sim`; Intel simulator support is an explicit additional gate.
The project owns iOS 17/macOS 14 deployment settings and Swift 5 language mode.

```sh
# Linux or Mac: real host generation and a stale-source comparison, once FFI exists.
scripts/native/apple-bindings.sh --check-source

# Apple runner, build and tests in a private source copy:
scripts/native/apple-build.sh --target macos
scripts/native/apple-test.sh --target macos --destination 'platform=macOS'
scripts/native/apple-build.sh --target ios
xcrun simctl list devices available
scripts/native/apple-test.sh --target ios \
  --destination 'platform=iOS Simulator,id=THE_ACTUAL_LISTED_UDID'
```

Use an actual listed simulator ID. Xcode's build/test commands take the shared
scheme and destination; result bundles remain under the run directory.
([Apple command-line testing](https://developer.apple.com/library/archive/technotes/tn2339/_index.html),
[Apple test results](https://developer.apple.com/documentation/xcode/running-tests-and-interpreting-results))

`apple-bindings.sh` builds the pinned host library and bindgen with the same
lockfile, generates Swift/headers/modulemap, and diffs the committed generated
sources. Differences fail. The FFI source owner regenerates source separately.
Darwin `--target macos|ios` builds the matching static-library slices and stages
`CrewClientFFI.xcframework` with checksum/Info.plist evidence. iOS excludes
`local`; its exported thin local connector must return `UnsupportedTransport`.
That ABI consistency is the binding owner's product test, not a shell inference.

`apple-build.sh`/`apple-test.sh` verify staged checksums and generated-source
freshness, read committed Apple files into a private artifact copy, install
the staged framework there, and invoke Xcode with private DerivedData/package
checkouts/result bundles. They record Xcode/developer path/SDKs/destinations,
Rust and lockfile identity. They never modify `apps/apple/**`. `--bindings DIR`
reuses an already staged, verified binding build. `--preflight` still requires
the real project/bindings; absence is not success. The optional developer
source-tree artifact installation command belongs to the Apple source owner.

Unsigned Debug/simulator checks are distinct from signed device, local Mac
transport, iOS gateway lifecycle, accessibility and distribution verification.
No Apple compile/link/simulator/device or signing result is claimed here.

## Preparation evidence, 2026-10-05

Executed on QA's worktree at `/home/nade/workspaces/personal/crew/.crew/worktrees/qa`:

```sh
bash -n scripts/native/*.sh scripts/native/tests/tooling.sh
NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  scripts/native/tests/tooling.sh
```

The actual GTK toolkit probe mapped at 480×240 and captured successfully.
The helper also rejected a command exiting before a window and a live command
with no window; all recorded application/display children were gone afterward.
Measured GTK DPI was 98,304 (96×1,024), font `Sans 10`. The probe is explicitly
not a Crew app or native daemon-flow acceptance. Tool versions were GTK
4.22.4, VTE 0.84.1, GLib 2.88.3, xwininfo 1.1.6, ImageMagick 7.1.2-29 and D-Bus
1.16.2. The operator's Wayland display/team/listener were preserved.

Artifacts: ignored `target/native-validation/tooling-1TPWyk/`; replay log
`target/native-tooling-prep-final.log`. Source/tool absence gates return 2 with
named reasons on this checkpoint: no `crew-desktop`, no `crew-protocol`/
`crew-client`, no `crew-client-ffi`/bindgen, and no macOS/Xcode. Apple shell
syntax and the Linux rejection path were checked; Apple execution remains
**deferred by operator**. Final source, conformance, native accessibility,
real-session Wayland and real-daemon GUI verification remain separate gates.
