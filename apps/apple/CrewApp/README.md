# Apple navigation foundation

This checkpoint provides native iOS and macOS projects, window scenes, a
presentation-only SwiftPM package, the five work destinations, appearance
settings, help, and a genuine unconnected state. It does not authenticate,
load a workspace, take a Crew action, or implement terminal rendering.
`native-apple-ui` supplies those later through the real Rust bridge. No domain
records or transport implementation are duplicated in Swift.

The foundation description and executed results below refer to approved
`4479a301`. Subsequent substantive implementation is tracked separately in
[Native implementation](NATIVE-IMPLEMENTATION.md); its source checkpoints do
not establish a connected app or an Apple build.

`Crew.xcodeproj` is checked in, with shared `CrewMac` and `CrewIOS` schemes.
Both schemes include unit and UI test targets. App sources under `CrewApp` and
`CrewHost` are shared; native tabs and per-destination stacks serve compact iOS,
while regular iOS and macOS use a native sidebar. The containers use Apple's
[navigation API](https://developer.apple.com/documentation/swiftui/navigation).
Every window has its own
in-memory selection and navigation paths. Appearance alone persists in
UserDefaults. Native text styles, standard controls, labelled symbols, scrollable
reading panes, and iOS activation areas of at least 44pt are used. Layout changes
do not replace the navigation owner. Closing an app window has no team effect.

The app-owned palette preserves the neutral dark surfaces and warm light
appearance. Named colour assets include increased-contrast variants; system
text and focus remain native. The icon reuses the existing filled Crew diamond
from `web/index.html`, with its original `#6ebef0` fill on `#121212`. The SVG is
canonical; PNG exports are app resources, including 16pt and 32pt previews.

## Local source validation

The pinned Linux environment supports project/resource validation and Swift
syntax parsing. These are not SwiftUI type checking, an Xcode build, or an
accessibility result. Run from the repository root:

```sh
nix-shell --pure -I nixpkgs=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  -p python3Packages.openstep-plist \
  --run 'python3 apps/apple/Tests/check-source.py'

nix-shell --pure -I nixpkgs=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  -p swift --run 'find apps/apple -name "*.swift" -type f -print0 | xargs -0 swiftc -frontend -parse'

nix-shell --pure -E 'let p = import /nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source {}; in p.mkShell {
  packages = [ p.swift p.swiftpm ];
  LD_LIBRARY_PATH = p.lib.makeLibraryPath [ p.swift-corelibs-libdispatch ];
}' --run 'swift package --package-path apps/apple/CrewUI dump-package'
```

`openstep-plist` is 0.5.2. The pinned Linux Swift compiler and SwiftPM are 5.10.1
and are used only for parsing and manifest evaluation. The Apple product runner
uses a Swift 6 compiler in Swift 5 language mode; deployment floors are iOS 17
and macOS 14. The Linux manifest command supplies libdispatch for the manifest
executable; an unqualified SwiftPM invocation in this Nix environment fails to
load that library. This is a tool environment requirement, not an app dependency.
The shell adds no external Swift package. Consequently it needs no remote
package resolution or `Package.resolved` entry at this checkpoint.

At `4479a301`, the Linux source validator reported **390 checks passed**, covering
101 project objects, six targets, two shared schemes, plists, resources and icon
dimensions. Syntax parsing of 13 Swift files passed. SwiftPM manifest evaluation
reported no external dependencies, the `CrewUI` library target with processed
resources, iOS 17/macOS 14, and Swift 5 language mode. Those are the only executed
checks; the six unit tests and two UI tests are source for deferred Apple runs.

To reproduce the icon exports, use the same pinned Nix path with `-p librsvg`
(2.62.3), then run `rsvg-convert --width <size> --height <size> --output
<AppIcon-size.png> apps/apple/CrewApp/Resources/AppIcon.svg` for sizes
16, 32, 64, 128, 256, 512 and 1024. The asset catalog names the matching files.

## Deferred Apple runner commands

**Apple verification deferred by operator.** None of the following commands,
unit/UI tests, simulator/device runs, native focus checks, or VoiceOver checks
has run on this Linux host. Signing, entitlement validation, device installation,
and publication are also unperformed. No development team or signing identity
is selected in the project; source build commands disable signing.
The app identifier `dev.crew.Crew` is a provisional local packaging identifier,
coordinated with Linux. It is not a domain ownership or publication claim.

Use an Apple runner with Xcode and a Swift 6 compiler. Record `xcodebuild
-version`, `xcode-select -p`, `xcrun swiftc -version`, SDKs, destinations, and
the source commit. Make a private copy of the committed `apps/apple` tree under
`target/native-validation/apple-shell/`, preserving its layout; direct all
DerivedData and result bundles there. These shell-only commands intentionally
do not use the full native runner's Rust/ABI acceptance path:

```sh
# A normal committed Git checkout on the Apple runner:
native_shell_stage="$PWD/target/native-validation/apple-shell-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$native_shell_stage"
git rev-parse HEAD > "$native_shell_stage/source-commit.txt"
git archive HEAD apps/apple | tar -x -C "$native_shell_stage"
native_shell_tree="$native_shell_stage/apps/apple"
native_shell_results="$native_shell_stage/results"
mkdir -p "$native_shell_results"
xcodebuild -list -json -project "$native_shell_tree/Crew.xcodeproj"
xcodebuild -showdestinations -project "$native_shell_tree/Crew.xcodeproj" -scheme CrewMac
xcodebuild -showdestinations -project "$native_shell_tree/Crew.xcodeproj" -scheme CrewIOS

xcodebuild -project "$native_shell_tree/Crew.xcodeproj" -scheme CrewMac \
  -configuration Debug -destination 'generic/platform=macOS' \
  -derivedDataPath "$native_shell_results/mac-arm64" \
  build ARCHS=arm64 CODE_SIGNING_ALLOWED=NO
xcodebuild -project "$native_shell_tree/Crew.xcodeproj" -scheme CrewMac \
  -configuration Debug -destination 'generic/platform=macOS' \
  -derivedDataPath "$native_shell_results/mac-x86_64" \
  build ARCHS=x86_64 CODE_SIGNING_ALLOWED=NO
xcodebuild -project "$native_shell_tree/Crew.xcodeproj" -scheme CrewIOS \
  -configuration Debug -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath "$native_shell_results/ios-simulator" \
  build ARCHS=arm64 CODE_SIGNING_ALLOWED=NO
xcodebuild -project "$native_shell_tree/Crew.xcodeproj" -scheme CrewIOS \
  -configuration Debug -destination 'generic/platform=iOS' \
  -derivedDataPath "$native_shell_results/ios-device" \
  build ARCHS=arm64 CODE_SIGNING_ALLOWED=NO

# Replace destinations with actually listed runner destinations. Run each once.
xcodebuild -project "$native_shell_tree/Crew.xcodeproj" -scheme CrewMac \
  -destination 'platform=macOS,arch=arm64' \
  -derivedDataPath "$native_shell_results/mac-tests" \
  -resultBundlePath "$native_shell_results/mac-tests.xcresult" \
  test CODE_SIGNING_ALLOWED=NO
xcodebuild -project "$native_shell_tree/Crew.xcodeproj" -scheme CrewIOS \
  -destination 'platform=iOS Simulator,id=<listed-simulator-UDID>' \
  -derivedDataPath "$native_shell_results/ios-tests" \
  -resultBundlePath "$native_shell_results/ios-tests.xcresult" \
  test CODE_SIGNING_ALLOWED=NO
```

Expected results, when run: both shared schemes resolve the real local `CrewUI`
package; required slices build; six navigation-state tests and two foundation
UI tests pass for each supported test destination. Test result bundles and
Xcode output are required before claiming those results. Later connected
integration must also run `scripts/native/apple-{bindings,build,test}.sh` with
the real bridge and matching staged `CrewClientFFI.xcframework`; shell results
do not satisfy that gate.

Manual Apple checks remain required: small phone portrait and landscape,
regular-to-compact resizing, largest Dynamic Type, macOS 1024×600 and narrow
windows, restore hidden sidebar, independent windows, sheet/window focus return,
Full Keyboard Access, VoiceOver, system/light/dark, increased contrast, and
reduced motion. This checkpoint has no decorative timers or animations.
