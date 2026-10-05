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
native_preflight=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target) [ "$#" -ge 2 ] || exit 64; native_target=$2; shift 2 ;;
        --destination) [ "$#" -ge 2 ] || exit 64; native_destination=$2; shift 2 ;;
        --out) [ "$#" -ge 2 ] || exit 64; native_out=$2; shift 2 ;;
        --preflight) native_preflight=true; shift ;;
        *) echo "usage: scripts/native/apple-{build,test}.sh --target macos|ios [--destination specifier] [--out directory] [--preflight]" >&2; exit 64 ;;
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
xcodebuild -version
xcrun --sdk "$native_sdk" --show-sdk-version
xcodebuild -list -json -project "$native_project"
xcodebuild -showBuildSettings -project "$native_project" -scheme "$native_scheme" -sdk "$native_sdk" > /dev/null
if "$native_preflight"; then exit 0; fi
native_new_output "$native_out" "apple-$native_target"
native_out=$NATIVE_RUN_DIR
unset CREW_IDENTITY CREW_SOCKET CREW_LAUNCH
cd "$native_repo"
xcodebuild -project "$native_project" -scheme "$native_scheme" -configuration Debug \
    -sdk "$native_sdk" -destination "$native_destination" \
    -derivedDataPath "$native_out/DerivedData" -resultBundlePath "$native_out/Validation.xcresult" \
    "$native_action" CODE_SIGNING_ALLOWED=NO 2>&1 | tee "$native_out/xcodebuild.log"
