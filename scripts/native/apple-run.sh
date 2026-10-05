#!/usr/bin/env bash
# Shared local Xcode runner. Apple execution is deferred by the operator.
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../.." && pwd)
source "$native_repo/scripts/native/common.sh"
native_action=${1:-}
case "$native_action" in build|test) shift ;; *) echo "build or test is required" >&2; exit 64 ;; esac
native_target=""
native_destination=""
native_out=""
native_bindings=""
native_preflight=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target) [ "$#" -ge 2 ] || exit 64; native_target=$2; shift 2 ;;
        --destination) [ "$#" -ge 2 ] || exit 64; native_destination=$2; shift 2 ;;
        --out) [ "$#" -ge 2 ] || exit 64; native_out=$2; shift 2 ;;
        --bindings) [ "$#" -ge 2 ] || exit 64; native_bindings=$2; shift 2 ;;
        --preflight) native_preflight=true; shift ;;
        *) echo "usage: scripts/native/apple-{build,test}.sh --target macos|ios [--destination specifier] [--bindings staged-directory] [--out directory] [--preflight]" >&2; exit 64 ;;
    esac
done
case "$native_target" in
    macos) native_scheme=CrewMac; native_sdk=macosx; native_destination=${native_destination:-platform=macOS} ;;
    ios)
        native_scheme=CrewIOS; native_sdk=iphonesimulator
        if [ -z "$native_destination" ]; then
            if [ "$native_action" = test ]; then echo "iOS tests require an explicit simulator destination" >&2; exit 64; fi
            native_destination='generic/platform=iOS Simulator'
        fi ;;
    *) echo "--target macos or --target ios is required" >&2; exit 64 ;;
esac
if [ "$(uname -s)" != Darwin ] || ! command -v xcodebuild >/dev/null || ! command -v xcrun >/dev/null; then
    echo "unavailable: Apple build/test requires macOS with Xcode; operator deferred runner verification" >&2
    exit 2
fi
native_project="$native_repo/apps/apple/Crew.xcodeproj"
if [ ! -d "$native_project" ]; then echo "unavailable: apps/apple/Crew.xcodeproj has not landed" >&2; exit 2; fi
native_new_output "$native_out" "apple-$native_target"
native_out=$NATIVE_RUN_DIR
unset CREW_IDENTITY CREW_SOCKET CREW_LAUNCH
cd "$native_repo"
{
    xcodebuild -version
    xcode-select -p
    xcodebuild -showsdks
    xcrun --sdk "$native_sdk" --show-sdk-version
    rustc -Vv
    rustup target list --installed
    cksum Cargo.lock
} > "$native_out/toolchain.txt"
if [ -z "$native_bindings" ]; then
    native_bindings="$native_out/bindings"
    "$native_repo/scripts/native/apple-bindings.sh" --target "$native_target" --out "$native_bindings"
fi
if [ ! -f "$native_bindings/artifact.sha256" ] || [ ! -d "$native_bindings/CrewClientFFI.xcframework" ]; then
    echo "unavailable: staged bindings/artifact manifest missing at $native_bindings" >&2; exit 2
fi
shasum -a 256 -c "$native_bindings/artifact.sha256"
diff -r "$native_bindings/Sources/Generated/Swift" "$native_repo/apps/apple/CrewClient/Sources/Generated/Swift"
diff -r "$native_bindings/Sources/Generated/Headers" "$native_repo/apps/apple/CrewClient/Sources/Generated/Headers"
native_tree="$native_out/apple-workspace"
mkdir -p "$native_tree"
native_files="$native_out/source-files"
if ! git -C "$native_repo" diff --quiet HEAD -- apps/apple; then
    echo "Apple source differs from HEAD; commit reviewed source before staged verification" >&2; exit 1
fi
git -C "$native_repo" ls-tree -r --name-only -z HEAD -- apps/apple > "$native_files"
if [ ! -s "$native_files" ]; then echo "unavailable: committed Apple project missing from checkout HEAD" >&2; exit 2; fi
while IFS= read -r -d '' path; do
    relative=${path#apps/apple/}
    mkdir -p "$(dirname "$native_tree/$relative")"
    git -C "$native_repo" show "HEAD:$path" > "$native_tree/$relative"
done < "$native_files"
mkdir -p "$native_tree/CrewClient/Artifacts"
cp -R "$native_bindings/CrewClientFFI.xcframework" "$native_tree/CrewClient/Artifacts/"
diff -r "$native_bindings/CrewClientFFI.xcframework" "$native_tree/CrewClient/Artifacts/CrewClientFFI.xcframework"
native_project="$native_tree/Crew.xcodeproj"
xcodebuild -list -json -project "$native_project" > "$native_out/project.json"
xcodebuild -showdestinations -project "$native_project" -scheme "$native_scheme" > "$native_out/destinations.txt"
xcodebuild -showBuildSettings -project "$native_project" -scheme "$native_scheme" -sdk "$native_sdk" > "$native_out/build-settings.txt"
if "$native_preflight"; then echo "Apple project preflight complete: $native_out"; exit 0; fi
xcodebuild -project "$native_project" -scheme "$native_scheme" -configuration Debug \
    -sdk "$native_sdk" -destination "$native_destination" \
    -clonedSourcePackagesDirPath "$native_out/SourcePackages" \
    -derivedDataPath "$native_out/DerivedData" -resultBundlePath "$native_out/Validation.xcresult" \
    "$native_action" CODE_SIGNING_ALLOWED=NO 2>&1 | tee "$native_out/xcodebuild.log"
