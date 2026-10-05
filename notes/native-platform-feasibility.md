# Native platform feasibility

Evidence date: 2026-10-05. Repository inventory: reviewed main
`6049cdf63e36e303327235834f23a798aa5a82c1`. Scope: native iOS, Linux and macOS
applications, with shared Rust logic and platform UI. This document supplies
tooling evidence and implementation candidates; `notes/native-architecture.md`
owns the final package, API and platform contract.

Linux development is feasible on this host after entering a GTK development
environment. Apple source and project work can proceed, but Apple compilation,
simulator execution and device testing remain deferred: the operator has an
Apple runner and chose to use it later (`m-d5c12d4290c9`, relayed in
`m-560c64be145d`). No runner connection or remote workflow was requested or used.

## Confirmed host and repository

| Item | Observed result | What this establishes |
| --- | --- | --- |
| Host | NixOS 26.11, Linux, x86_64 | Linux build host; no macOS execution environment |
| Rust | `cargo 1.93.1`, `rustc 1.93.1`, default `stable-x86_64-unknown-linux-gnu` | Compiler available; this is the tested toolchain, not a verified minimum version |
| Installed Rust targets | `x86_64-unknown-linux-gnu`, `wasm32-unknown-unknown`, `wasm32-wasip2` | Linux and browser-WASM compilation possible; Apple targets not installed |
| Existing compiler helper | `scripts/build-env` supplies GCC 15.3.0 and pkg-config 0.29.2 through Nix | C compiler/linker provision works; neither is on the initial PATH |
| Apple commands | `xcodebuild`, `xcrun`, `xcode-select`, `swift`, `swiftc`, `codesign`, `security` absent on PATH | No local Apple build, signing or Swift compilation claim |
| Apple installation locations | No `/Applications/Xcode.app`, `/Applications/Xcode-beta.app`, `/Library/Developer/CommandLineTools` or `/Library/Developer/CoreSimulator` | No detected Xcode, Apple SDK or simulator installation |
| Initial Linux development environment | `pkg-config` cannot find GTK, GLib, GIO, Pango, Cairo or libadwaita through `scripts/build-env` | Existing Rust helper alone does not supply GTK development dependencies |
| GTK development environment | `nix-shell -p gtk4 pkg-config gcc` reports GTK 4.22.4, GLib/GIO 2.88.3, Pango 1.57.1, Cairo 1.18.4 | GTK headers, libraries and pkg-config metadata can be provisioned here |
| Accessibility development metadata | Same pinned Nix input with `at-spi2-core` reports `atspi-2` 2.60.6 | AT-SPI dependency available; application/provider/bus and screen-reader behavior not tested |
| Linux session | DISPLAY, WAYLAND_DISPLAY and DBUS_SESSION_BUS_ADDRESS set; Hyprland session definitions present | Session configuration detected; no native application interaction verified by this inventory |
| Packaging tools | Flatpak 1.18.1 present; flatpak-builder, appimagetool, meson and ninja absent on initial PATH | Flatpak runtime presence does not prove packaging capability |
| Other tools | Node 24.19.0, Nix 2.34.8, jj 0.44.0 | Inventory and repository tooling available |

Reviewed main has three Rust workspace members: `crew-core`, `crew-tui` and
`crew`; edition 2021 and declared Rust floor 1.82. It has no tracked Swift source,
`Package.swift`, Xcode project/workspace, native app packaging manifest or CI
workflow. External CI availability is unknown beyond the operator's statement;
no accounts, credentials, certificates or external runners were inspected.

`crew-core` already shares protocol/domain types, but is not a portable native
client package as a whole. `src/client.rs` imports `std::os::unix::net::UnixStream`;
`src/paths.rs` performs local discovery, ownership and credential-file operations.
The web server in `crates/crew/src/web` also has local authentication and transport
contracts. Sharing these types and pure state logic is plausible. Copying the
whole daemon/client into a mobile app or a WASM module is not established by
target availability. The architecture and secure-connectivity inventories own
that extraction and transport boundary.

## Native UI and dependency candidates

| Platform | Candidate | Rationale and boundary |
| --- | --- | --- |
| iOS | SwiftUI, with UIKit integration where necessary | Native Apple UI; Swift adapter consumes a shared Rust library |
| macOS | SwiftUI, with AppKit integration for desktop behavior | Native Mac windows, commands and controls; reuse Apple adapters and selected views with iOS |
| Linux | GTK4-rs application | Native GTK widgets and desktop integration; direct dependency on the same Rust core |
| Web | Existing web UI with the approved generated TypeScript policy module | W12 exception allows one generated policy file; no WASM adoption in this delivery |
| TUI | Existing ratatui UI with direct Rust dependency | Reuses the core; terminal rendering and local transport remain platform concerns |

Apple documents SwiftUI across Apple platforms and its interoperability with
UIKit and AppKit. A multiplatform Xcode target can share settings and sources,
while allowing differences per platform. Separate iOS/macOS targets also remain
possible when entitlements or distribution differ. These are implementation
candidates, not a common rendered UI layer. [SwiftUI](https://developer.apple.com/swiftui/),
[multiplatform target configuration](https://developer.apple.com/documentation/xcode/configuring-a-multiplatform-app-target).

GTK4-rs exposes native GTK widgets. GTK supports Linux Wayland and X11 backends;
building a Rust GTK application needs the system libraries and development
metadata, rather than a source build of GTK itself. libadwaita is optional for
this candidate and has not been provisioned or required by this card.
[GTK4-rs documentation](https://gtk-rs.org/gtk4-rs/stable/latest/docs/gtk4/),
[GTK build documentation](https://docs.gtk.org/gtk4/building.html).

### Version evidence and compatibility

The following are exact compatibility candidates inspected using `cargo info`
and their downloaded crate manifests, not moving `latest` dependencies:

| Package | Inspected version | Declared Rust version | Result/constraint |
  | --- | --- | --- | --- |
| gtk4 | 0.10.3 | 1.83, edition 2021 | GTK dependency probe checks on host Rust 1.93.1 |
| gtk4, newer alternative | 0.11.5 | 1.92, edition 2024 | Metadata inspected; this alternative was not compiled |
| uniffi | 0.31.0 | Not declared | Host probe checks; full dependency floor must be verified with the committed lockfile |
| uniffi, newer alternative | 0.32.2 | Not declared | Metadata inspected; this alternative was not compiled |
| wasm-bindgen | 0.2.128 | 1.77, edition 2021 | Separate browser adapter candidate; exact CLI version must match the crate |

Package sources: [gtk4 0.10.3](https://crates.io/crates/gtk4/0.10.3),
[gtk4 0.11.5](https://crates.io/crates/gtk4/0.11.5),
[UniFFI 0.31.0](https://crates.io/crates/uniffi/0.31.0),
[UniFFI 0.32.2](https://crates.io/crates/uniffi/0.32.2),
[wasm-bindgen 0.2.128](https://crates.io/crates/wasm-bindgen/0.2.128).

GTK4-rs 0.10.3 exceeds the existing workspace's declared Rust 1.82 floor. Keep
native-shell requirements explicit in their manifests/toolchain, or deliberately
revise and test the workspace floor in the architecture contract. Do not claim
1.82 compatibility from a successful 1.93.1 build. A crate's declared minimum
also does not establish the floor of its whole resolved dependency graph.
The upstream moving stable docs still said 1.83 during this inventory; the
downloaded 0.11.5 manifest says 1.92. Versioned manifests control this decision.

The probe's resolved metadata already includes Rust 1.85 requirements in
system-deps' TOML/cfg-expr dependencies, and Rust 1.88 in textwrap with the UniFFI
bindgen feature. A native-only 1.83 declaration needs a deliberately compatible
lockfile or a higher declared floor. These are manifest observations, not tests
with older Rust compilers.

The native architect selected a native-only Rust 1.93 floor and tested-host
toolchain 1.93.1 (`m-d7b5a71e8837`), retaining the root/shared-core 1.82 contract.
This avoids inferring a complete graph floor from GTK's 1.83 declaration.

Intended dependency split:

| Package boundary | Dependencies owned there | Consumers |
| --- | --- | --- |
| `crew-protocol` / `crew-client` | Portable wire/domain types and pure policy/state logic | TUI, native hosts and generated web policy |
| Existing `crew-core` local client | Protected Unix transport, local discovery and credential files | CLI/TUI and local desktop session adapter |
| Linux shell package | GTK4-rs and VTE4; direct Rust client dependency | Linux GUI executable only |
| Native FFI facade/generator packages | UniFFI and pure client dependency; Rust staticlib/cdylib outputs | Apple native library/binding generation only |
| Apple app/Swift package targets | Generated Swift/FFI wrappers, SwiftUI, SwiftTerm | iOS/macOS app targets only |
| Existing `crew` binary | Existing CLI/runtime/TUI graph | No GTK, VTE, UniFFI or new browser-WASM dependency |

Final native paths/workspace membership belong to the platform contract. The
probe is a separate `[workspace]` under notes and adds no edge to `cargo tree
-p crew -e normal` or the product lockfile. Shared client packages must not
depend on the facade or a GUI shell.

GTK4-rs defaults to GTK 4.0 APIs. Features such as `v4_8` and `v4_12` raise the
system-library requirement. Start with the smallest API floor needed by the UI;
testing only GTK 4.22.4 does not validate older supported distributions. Pin the
Rust dependencies in Cargo.lock and the CI system-library environment separately.
[GTK4-rs version features](https://gtk-rs.org/gtk4-rs/stable/latest/docs/gtk4/#features).

UniFFI generates Swift bindings for a Rust library; it does not build, package or
ship the Apple application. Its Swift output includes a Swift source file,
C header and module map. The official guide notes partial Swift 6/Sendable
support, particularly for asynchronous APIs. Initial Swift 5 language mode under
a pinned Swift 6 compiler is a conservative candidate until Apple verifies the
actual generated surface. Use an explicit main-thread adapter for native UI
updates; generated bindings are not a guarantee of correct UI isolation.
[UniFFI overview](https://mozilla.github.io/uniffi-rs/latest/),
[Swift bindings and Swift 6 limitations](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html).

UniFFI's WASM scaffolding needs an external binding generator and a feature that
changes wasm32 Send/Sync checks. It is not an official browser-JavaScript
generator bundled with UniFFI. The standalone wasm-bindgen probe evaluates a
separate technical path without coupling it to that generator. It is not web
adoption: the current architecture's narrow W12 exception permits only one
generated TypeScript policy module, with differential fixtures against Rust.
No browser dependency, CSP, startup or bundle-budget change is authorized here.
[UniFFI WASM configuration](https://mozilla.github.io/uniffi-rs/latest/wasm/configuration.html),
[wasm-bindgen guide](https://rustwasm.github.io/docs/wasm-bindgen/).

### Required native terminal

Native terminal parity has a concrete widget path; it is not omitted from the
application candidate. Keep both dependencies in the platform shell rather
than the pure Rust state core.

| Platform | Exact terminal candidate | Evidence and limits |
| --- | --- | --- |
| Linux | `vte4 = "=0.9.0"` alongside `gtk4 = "=0.10.3"` | Crate depends on GTK 0.10, GLib/GIO 0.21. Native VTE widget type checks on this host with VTE 0.84.1; no widget launch or screen-reader test yet |
| iOS/macOS | SwiftTerm `v1.19.0`, pinned in SwiftPM | Tagged manifest uses Swift tools 6.0, Swift 5 language mode, iOS 14 and macOS 11 minimums with benchmarks disabled. Native UIKit/AppKit view source inspected; Apple compilation deferred |

The VTE system dependency is `vte-2.91-gtk4`; the 0.9.0 sys crate declares a
0.65 baseline, raised by selected version features. The tested Nix input
supplies it through `vte-gtk4`. Newer `vte4 0.10.0` uses GTK 0.11 and GLib/GIO
0.22, so it is not a drop-in dependency for the chosen GTK 0.10 candidate.
[vte4 0.9.0](https://crates.io/crates/vte4/0.9.0),
[vte4-sys 0.9.0](https://crates.io/crates/vte4-sys/0.9.0),
[vte4 0.10.0](https://crates.io/crates/vte4/0.10.0).

VTE's `feed` method accepts terminal output and its `commit` signal exposes user
input. SwiftTerm's UIKit/AppKit `TerminalView` accepts an application-provided
source through its delegate. Use those views with the approved Crew session
adapter, rather than spawning a local process or importing the example SSH
application. Existing snapshot/ANSI output must be represented as snapshots;
feeding repeated full screens as append-only output would produce incorrect
scrollback. The terminal surface needs explicit input-capability, lifecycle,
selection, keyboard and accessibility acceptance on each platform.
[VTE output](https://gnome.pages.gitlab.gnome.org/vte/gtk4/method.Terminal.feed.html),
[VTE input signal](https://gnome.pages.gitlab.gnome.org/vte/gtk4/signal.Terminal.commit.html),
[SwiftTerm tagged package](https://github.com/migueldeicaza/SwiftTerm/blob/v1.19.0/Package.swift),
[SwiftTerm native view integration](https://github.com/migueldeicaza/SwiftTerm/blob/v1.19.0/README.md).

SwiftTerm's moving main describes a changed 2.0 API; use the inspected release
and resolved revision when implementing this candidate. Its declared minimums
fit the possible iOS 17/macOS 14 app floor. Neither upstream feature lists nor
source inspection establish VoiceOver/AT-SPI parity; those remain native UI
tests. The approved connectivity direction is the scoped Crew gateway
(`m-8b3375948f11`), not full-account SSH.

## Apple OS, SDK and source-controlled project

The native architect selected product minimums iOS 17 and macOS 14 for SwiftUI
APIs (`m-d7b5a71e8837`). These match Observation's documented introduction
versions, rather than an upstream SwiftTerm minimum or an operator OS demand.
Simulator support, compiler-host requirements and deployment minimums are three
separate constraints.
[Observation availability](https://developer.apple.com/documentation/swiftui/migrating-from-the-observable-object-protocol-to-the-observable-macro).

Apple's current support table offers these concrete toolchain examples. Neither
was installed or selected here:

| Xcode | Compiler host | Bundled Swift compiler | iOS/macOS SDK examples |
| --- | --- | --- | --- |
| 26.3 | macOS 15.6 through macOS 26.x | 6.2.3 | iOS 26.2, macOS 26.2 |
| 27 | macOS 26.6 or later | 6.4 | iOS 27, macOS 27 |

Pin an available non-beta Xcode build and compatible runtime in the actual Apple
runner, then record `xcodebuild -version`, SDK versions and simulator runtime
identifiers with every verification result. The table changes; consult it when
choosing the runner rather than treating these examples as the project's pin.
[Apple SDK and system requirements](https://developer.apple.com/xcode/system-requirements).

For reviewable app source, commit an Xcode `.xcodeproj` with shared app/test
schemes, `.xcconfig` build settings, assets, Info.plist/entitlements, test plans,
and any Swift package manifests/resolved dependencies. Use the existing
repository; do not create a nested source-control repository. Exclude
`xcuserdata`, DerivedData, local account/team settings and generated build
products. A generator such as XcodeGen/Tuist would add another pinned dependency;
neither exists here and neither is required for this approach. Source control
and shared schemes are supported by Apple's project workflow.
[Xcode source control](https://developer.apple.com/documentation/xcode/configuring-your-xcode-project-to-use-source-control),
[shared scheme requirement](https://developer.apple.com/library/archive/documentation/IDEs/Conceptual/xcode_guide-continuous_integration/ConfigureBots.html).

Use a local Swift package for the adapter/bindings if the architecture selects
that organization. A Rust `staticlib` with generated headers/module map can be
packaged as an XCFramework, with separate iOS-device, iOS-simulator and macOS
variants. Apple supports `-library` and `-headers`; do not merge device and
simulator ARM64 slices into one library. A local Swift package binary target can
reference the result. SwiftPM reuse does not replace the app bundle/project or
the simulator test workflow.
[XCFramework packaging](https://developer.apple.com/documentation/xcode/creating-a-multi-platform-binary-framework-bundle),
[binary Swift packages](https://developer.apple.com/documentation/xcode/distributing-binary-frameworks-as-swift-packages).

Rust targets for this path:

| Destination | Rust target | Execution requirement |
| --- | --- | --- |
| iPhone/iPad device, ARM64 | `aarch64-apple-ios` | iOS SDK and a provisioned device for running tests |
| Apple Silicon iOS simulator | `aarch64-apple-ios-sim` | iOS Simulator SDK and installed compatible runtime |
| Intel iOS simulator, if supported | `x86_64-apple-ios` | Intel-compatible simulator runtime and Mac |
| Apple Silicon Mac | `aarch64-apple-darwin` | macOS SDK and a Mac for native tests |
| Intel Mac, if supported | `x86_64-apple-darwin` | macOS SDK and an appropriate Mac/Rosetta test plan |

`rustc --print target-list` recognizes these target names on this host; none of
their standard-library targets is installed. `rustup target add` alone does not
provide Apple SDKs or runnable simulators. Keep Rust deployment settings aligned
with Xcode (`IPHONEOS_DEPLOYMENT_TARGET`, `MACOSX_DEPLOYMENT_TARGET`). Cargo host
tests do not execute an iOS binary.
[Rust iOS targets](https://doc.rust-lang.org/rustc/platform-support/apple-ios.html),
[Rust macOS targets](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html).

## Reproducible verification commands

Commands below distinguish measurements from future acceptance steps. App/package
environment variables intentionally name architecture-owned paths and schemes:
the corresponding product source did not exist at the reviewed checkpoint.
Missing prerequisites must fail the native verification job, not silently pass.

### Inventory performed here

```sh
uname -sm
cat /etc/os-release
cargo --version
rustc --version
rustup target list --installed
./scripts/build-env bash -c 'cc --version | head -n 1; pkg-config --version'
nix-shell -p gtk4 pkg-config gcc --run \
  'pkg-config --modversion gtk4 glib-2.0 gio-2.0 pango cairo'
nix-shell -I nixpkgs=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  -p gtk4 at-spi2-core pkg-config gcc --run \
  'pkg-config --modversion atspi-2; pkg-config --variable=prefix atspi-2'
cargo info gtk4@0.10.3
cargo info gtk4@0.11.5
cargo info uniffi@0.31.0
cargo info uniffi@0.32.2
cargo info wasm-bindgen@0.2.128
```

The Nix GTK probe used local nixpkgs source
`/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source` (`26.11pre-git`), separate
from the running NixOS version. `nix-shell -p` alone follows the local channel;
CI must pin the nixpkgs input/lock or supply an exact source path. For the same
probe, `nix-shell -I nixpkgs="$CREW_NIXPKGS_PATH" -p gtk4 pkg-config gcc ...`
selects the agreed input without changing this repository's compiler shell.

### Linux and shared-core acceptance after implementation

```sh
set -eu
: "${CREW_NIXPKGS_PATH:?Pinned nixpkgs source path required}"
: "${CREW_CLIENT_CORE_PACKAGE:?Shared core Cargo package required}"
: "${CREW_LINUX_PACKAGE:?Native Linux Cargo package required}"
export CREW_CLIENT_CORE_PACKAGE CREW_LINUX_PACKAGE
nix-shell -I nixpkgs="$CREW_NIXPKGS_PATH" -p gtk4 pkg-config gcc --run \
  'cargo test --locked -p "$CREW_CLIENT_CORE_PACKAGE"'
nix-shell -I nixpkgs="$CREW_NIXPKGS_PATH" -p gtk4 pkg-config gcc --run \
  'cargo build --locked --release -p "$CREW_LINUX_PACKAGE"'
```

Add `vte-gtk4` to that pinned Nix environment when building the terminal candidate.

Use fixed package names in the committed runner script rather than accepting
arbitrary shell input. The native shell still needs a real Wayland/X11 session
or an isolated Xvfb session with a session bus for launch/UI tests. Validate
keyboard navigation, AT-SPI semantics, resize/scroll behavior and lifecycle;
compiler success is not UI verification. Existing Playwright tests cover the
browser UI only. Xvfb and native UI test tooling were not exercised in this card.

The shared-core job should test recorded protocol/state fixtures on Linux,
check portable crates for `wasm32-unknown-unknown`, and compare equivalent
behavior across Rust, generated Swift and generated web policy. A portability
check is not browser integration. Test the agreed
minimum Rust version separately. Keep the existing web build/embedded bundle
checks and P1–P4 budgets in the integration plan.

### Apple runner preflight, deferred

Use an already installed and initialized full Xcode, with simulator components.
The standalone Command Line Tools package does not supply the iOS SDK/simulator
workflow. A process-local developer-directory override avoids changing the Mac's
global selection. [Command-line tools settings](https://developer.apple.com/documentation/xcode/configuring-command-line-tools-settings),
[additional Xcode components](https://developer.apple.com/documentation/xcode/downloading-and-installing-additional-xcode-components).

```sh
set -eu
test "$(uname -s)" = Darwin
: "${CREW_XCODE_DEVELOPER_DIR:?Pinned Xcode developer directory required}"
: "${CREW_APPLE_PROJECT:?Source-controlled .xcodeproj path required}"
: "${CREW_IOS_SCHEME:?Shared iOS scheme required}"
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcodebuild -version
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcodebuild -showsdks
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcrun swift --version
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcrun --sdk iphoneos --show-sdk-path
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcrun --sdk iphonesimulator --show-sdk-path
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcrun simctl list runtimes -j
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcrun simctl list devices available -j
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcodebuild \
  -project "$CREW_APPLE_PROJECT" -list
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcodebuild \
  -project "$CREW_APPLE_PROJECT" -scheme "$CREW_IOS_SCHEME" -showdestinations
```

Choose an available device UUID from that runner and record its runtime. Do not
hard-code a phone name/OS from another machine. Provision missing Xcode/runtime
components as runner setup; no installation or account setup happened here.
Expected preflight result: command exit 0, nonempty SDK paths, the committed
shared scheme, and a compatible available simulator runtime/device.

### Apple build and tests, deferred

Build the Rust Apple variants and XCFramework first using architecture-owned
scripts. Then run, in a fresh job/artifact directory:

```sh
set -eu
: "${CREW_XCODE_DEVELOPER_DIR:?Pinned Xcode developer directory required}"
: "${CREW_APPLE_PROJECT:?Source-controlled .xcodeproj path required}"
: "${CREW_IOS_SCHEME:?Shared iOS scheme required}"
: "${CREW_MAC_SCHEME:?Shared macOS scheme required}"
: "${CREW_IOS_SIMULATOR_UDID:?Available simulator UUID required}"

# Compilation only; this does not run an app or validate device distribution.
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcodebuild \
  -project "$CREW_APPLE_PROJECT" -scheme "$CREW_IOS_SCHEME" \
  -configuration Debug -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath "$PWD/target/native/apple-simulator" \
  build CODE_SIGNING_ALLOWED=NO

# Actual unit/adapter/UI test execution. Simulator builds may use local ad hoc
# signing; no distribution certificate or provisioning updates are requested.
env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcodebuild \
  -project "$CREW_APPLE_PROJECT" -scheme "$CREW_IOS_SCHEME" \
  -destination "platform=iOS Simulator,id=$CREW_IOS_SIMULATOR_UDID" \
  -derivedDataPath "$PWD/target/native/apple-ios-tests" \
  -resultBundlePath "$PWD/target/native/ios-tests.xcresult" test

env DEVELOPER_DIR="$CREW_XCODE_DEVELOPER_DIR" xcodebuild \
  -project "$CREW_APPLE_PROJECT" -scheme "$CREW_MAC_SCHEME" \
  -destination 'platform=macOS' \
  -derivedDataPath "$PWD/target/native/apple-mac-tests" \
  -resultBundlePath "$PWD/target/native/mac-tests.xcresult" test
```

Xcode supports `test`, `build-for-testing` and `test-without-building`; split the
latter two only when test products and the selected environment are preserved.
Use Swift Testing or XCTest for adapter/logic tests and XCTest/XCUIAutomation
for native UI workflows. Keep `.xcresult` artifacts with compiler/SDK/runtime
metadata. No command in this section was executable on the Linux host.
Expected acceptance result: command exit 0, `BUILD SUCCEEDED` for the compile
job, and `TEST SUCCEEDED` plus test records in both `.xcresult` bundles. These
expected results are deferred checks, not measured results.
[Xcode command-line build/test actions](https://developer.apple.com/library/archive/technotes/tn2339/_index.html),
[native testing frameworks](https://developer.apple.com/documentation/xcode/testing).

## Packaging and signing boundaries

For Linux, the initial reviewable artifact can be a native GTK executable built
in the pinned host environment, with a desktop entry and application icons.
GTK documents the desktop entry, icon and resource conventions. Dynamic GTK/
GLib and glibc requirements still determine which systems can run that artifact;
a release build on this NixOS host is not a portable multi-distribution package.
Validate installed resource lookup and launcher behavior in the intended package.
[GTK application packaging](https://docs.gtk.org/gtk4/getting_started.html#building-applications).

Nix/source packaging fits the existing host build approach. Flatpak is a possible
later distribution artifact, not selected or verified here. Its sandbox changes
filesystem/socket access and therefore daemon/credential discovery. Resolve that
with the secure connectivity contract; do not grant blanket host/home or session
bus access to make discovery work. No remotes, manifests or permissions were
changed. AppImage/deb/rpm publication is likewise outside this inventory.
[Flatpak sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html).

For Apple, distinguish these acceptance levels:

| Artifact/operation | Requirement | Status |
| --- | --- | --- |
| App source, Xcode project and generated Swift bindings | Reviewable source and pinned generation | Authorized implementation can proceed after contract approval |
| Simulator compilation/tests | Compatible macOS, full Xcode, SDK/runtime; no distribution enrollment needed | Deferred to operator's Apple runner |
| Personal iOS device installation | Apple Account in Xcode, Personal Team provisioning, device setup | Not attempted; free provisioning has limits and expires |
| TestFlight/App Store/ad hoc distribution | Appropriate Developer Program account, signing/provisioning and distribution configuration | Separate distribution work; no account access or publication performed |
| Local Mac development build | Mac SDK/host, native project and applicable local signing settings | Deferred; no distributable artifact claim |
| Direct Mac distribution | Developer ID signing, hardened runtime and notarization workflow | Separate distribution work; no certificate or notary changes performed |
| Mac App Store | Store distribution signing/configuration and app submission | Separate distribution work; no submission performed |

Apple distinguishes free Personal Team device testing from Developer Program
distribution and documents its provisioning limits. TestFlight uses App Store
Connect. Direct Mac distribution uses Developer ID and notarization; App Store
distribution has its own signing path. Availability of source or an unsigned
simulator build establishes none of those shipping states.
[Developer account and Personal Team](https://developer.apple.com/help/account/basics/about-your-developer-account/),
[membership capabilities](https://developer.apple.com/support/compare-memberships/),
[App Store Connect workflow](https://developer.apple.com/help/app-store-connect/get-started/app-store-connect-workflow),
[Mac distribution signing](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/),
[Mac notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

The iOS adapter must handle suspension and rebuild/resynchronize state when
foregrounded. A permanent background socket connection cannot be its freshness
guarantee. This is an application lifecycle constraint, not authorization for
push services or a new network listener. Preserve the existing local dashboard
and security contracts; transport work belongs to its own approved slice.
[iOS background transition](https://developer.apple.com/documentation/uikit/preparing-your-ui-to-run-in-the-background).

## Evidence limits

The [standalone dependency probe](native-platform-feasibility/probe/Cargo.toml)
preserves `Cargo.toml`, `Cargo.lock`, `src/lib.rs` and `src/bindgen.rs` outside the
product workspace.
It checks pinned GTK/UniFFI/VTE types on Linux. It is
toolchain evidence, not an implemented Crew app, native UI launch, integration
test or supported-distribution test. Build products live under ignored
`target/native-platform-feasibility`; product manifests, root Cargo.lock, web assets
and runtime/security code were not changed by this card.

| Probe | Actual result |
| --- | --- |
| Archived GTK4-rs/VTE4/UniFFI dependency check | Passed on Rust 1.93.1, 23.16 seconds |
| Archived Linux UniFFI cdylib/staticlib build | Passed, 5.56 seconds |
| Swift bindings generated from Linux library | Passed; produced `crew_platform_probe.swift`, `crew_platform_probeFFI.h`, `crew_platform_probeFFI.modulemap`; missing swift-format produced a formatting warning |
| Archived wasm-bindgen adapter check | Passed with host C linker provision, 4.60 seconds; an initial direct Cargo invocation failed because host build scripts needed `cc` |

Selected output from the archived replay:

```text
Checking gtk4 v0.10.3
Checking vte4 v0.9.0
Checking uniffi v0.31.0
Finished `dev` profile [unoptimized + debuginfo] target(s) in 23.16s
Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.56s
Warning: Unable to auto-format .../crew_platform_probe.swift using swift-format.
Checking crew-platform-probe .../notes/native-platform-feasibility/probe
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.60s
```

Re-run the archived probe with its committed lockfile; this does not require
framework changes to Crew:

```sh
set -eu
: "${CREW_NIXPKGS_PATH:?Pinned nixpkgs source path required}"
export CARGO_TARGET_DIR="$PWD/target/native-platform-feasibility/probe-build"
nix-shell -I nixpkgs="$CREW_NIXPKGS_PATH" \
  -p gtk4 vte-gtk4 pkg-config gcc --run '
  set -eu
  rustc --version
  cargo check --locked \
    --manifest-path notes/native-platform-feasibility/probe/Cargo.toml \
    --features terminal-smoke -j 2
  cargo build --locked \
    --manifest-path notes/native-platform-feasibility/probe/Cargo.toml --lib -j 2
  cargo run --locked \
    --manifest-path notes/native-platform-feasibility/probe/Cargo.toml \
    --features bindgen --bin uniffi-bindgen -j 2 -- generate \
    --library "$CARGO_TARGET_DIR/debug/libcrew_platform_probe.so" \
    --language swift --out-dir "$PWD/target/native-platform-feasibility/generated"
  cargo check --locked \
    --manifest-path notes/native-platform-feasibility/probe/Cargo.toml \
    --target wasm32-unknown-unknown --lib -j 2
  '
```

The checked-in probe lockfile SHA-256 is
`6b2d9aa75591ae2cef4ad7a017bf5df70c6fe4256909d92f42360fc94b3663f5`.
Its tiny exported function is a transport-free fixture, not the proposed shared
client API. The generated Swift files have not been compiled; generation
success on Linux only establishes that the pinned generator can emit them.

Apple compilation, simulator/device runs, Swift-binding compilation, signing,
packaging installation and publication have not been verified. The operator's
deferred Apple checks are explicit acceptance work for the later runner, not a
reason to leave the source/project implementation incomplete.
