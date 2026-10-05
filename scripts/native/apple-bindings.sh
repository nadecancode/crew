#!/usr/bin/env bash
# Stages binding output; never regenerates source files in apps/apple.
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../.." && pwd)
source "$native_repo/scripts/native/common.sh"
native_target=""
native_out=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --check-source) shift ;;
        --target) [ "$#" -ge 2 ] || exit 64; native_target=$2; shift 2 ;;
        --out) [ "$#" -ge 2 ] || exit 64; native_out=$2; shift 2 ;;
        *) echo "usage: scripts/native/apple-bindings.sh [--check-source | --target macos|ios] [--out directory]" >&2; exit 64 ;;
    esac
done
case "$native_target" in ''|macos|ios) ;; *) echo "unknown Apple target: $native_target" >&2; exit 64 ;; esac
if [ ! -f "$native_repo/crates/crew-client-ffi/Cargo.toml" ] || [ ! -f "$native_repo/crates/crew-client-ffi/src/bin/crew-client-bindgen.rs" ]; then
    echo "unavailable: crew-client-ffi and crew-client-bindgen have not landed" >&2; exit 2
fi
if [ -n "$native_target" ] && { [ "$(uname -s)" != Darwin ] || ! command -v xcodebuild >/dev/null || ! command -v lipo >/dev/null; }; then
    echo "unavailable: Apple verification deferred by operator; packaging needs macOS/Xcode" >&2; exit 2
fi
native_toolchain=${NATIVE_RUST_TOOLCHAIN:-1.93.1}
if ! rustup toolchain list | awk -v wanted="$native_toolchain" '$1 == wanted || index($1, wanted "-") == 1 { found=1 } END { exit !found }'; then
    echo "unavailable: Rust toolchain $native_toolchain is not installed" >&2; exit 2
fi
native_new_output "$native_out" apple-bindings
native_out=$NATIVE_RUN_DIR
export CARGO_TARGET_DIR="$native_out/cargo"
unset CREW_IDENTITY CREW_SOCKET CREW_LAUNCH
cd "$native_repo"
rustc "+$native_toolchain" -Vv > "$native_out/rust-version.txt"
cksum Cargo.lock > "$native_out/lockfile.txt"
./scripts/build-env cargo "+$native_toolchain" build --locked -p crew-client-ffi --features bindings,local --release
case "$(uname -s)" in
    Darwin) native_host_library="$CARGO_TARGET_DIR/release/libcrew_client_ffi.dylib" ;;
    Linux) native_host_library="$CARGO_TARGET_DIR/release/libcrew_client_ffi.so" ;;
    *) echo "unavailable: binding generation host must be Linux or macOS" >&2; exit 2 ;;
esac
./scripts/build-env cargo "+$native_toolchain" run --locked -p crew-client-ffi --features bindgen,local \
    --bin crew-client-bindgen -- generate --library "$native_host_library" --language swift --out-dir "$native_out/generated"
mkdir -p "$native_out/Sources/Generated/Swift" "$native_out/Sources/Generated/Headers"
shopt -s nullglob
native_swift=("$native_out/generated/"*.swift)
native_headers=("$native_out/generated/"*.h)
native_maps=("$native_out/generated/"*.modulemap)
if [ "${#native_swift[@]}" -eq 0 ] || [ "${#native_headers[@]}" -eq 0 ] || [ "${#native_maps[@]}" -ne 1 ]; then
    echo "binding generator output must contain Swift, headers and exactly one module map" >&2; exit 1
fi
cp "${native_swift[@]}" "$native_out/Sources/Generated/Swift/"
cp "${native_headers[@]}" "${native_maps[@]}" "$native_out/Sources/Generated/Headers/"
native_committed="$native_repo/apps/apple/CrewClient/Sources/Generated"
if [ ! -d "$native_committed/Swift" ] || [ ! -d "$native_committed/Headers" ]; then
    echo "unavailable: committed generated bindings have not landed; staged output: $native_out" >&2; exit 2
fi
diff -r "$native_out/Sources/Generated/Swift" "$native_committed/Swift"
diff -r "$native_out/Sources/Generated/Headers" "$native_committed/Headers"
if [ -z "$native_target" ]; then echo "binding sources match; staged output: $native_out"; exit 0; fi

case "$native_target" in
    macos) native_targets=(aarch64-apple-darwin x86_64-apple-darwin); native_features=bindings,local ;;
    ios) native_targets=(aarch64-apple-ios aarch64-apple-ios-sim); native_features=bindings ;;
esac
for target in "${native_targets[@]}"; do
    if ! rustup target list --installed --toolchain "$native_toolchain" | awk -v wanted="$target" '$0 == wanted { found=1 } END { exit !found }'; then
        echo "unavailable: install Rust target $target for $native_toolchain on the Apple runner" >&2; exit 2
    fi
    cargo "+$native_toolchain" build --locked -p crew-client-ffi --features "$native_features" --release --target "$target"
done
mkdir -p "$native_out/headers"
cp "${native_headers[@]}" "$native_out/headers/"
cp "${native_maps[0]}" "$native_out/headers/module.modulemap"
if [ "$native_target" = macos ]; then
    mkdir -p "$native_out/macos"
    lipo -create "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/libcrew_client_ffi.a" \
        "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/libcrew_client_ffi.a" -output "$native_out/macos/libcrew_client_ffi.a"
    xcodebuild -create-xcframework -library "$native_out/macos/libcrew_client_ffi.a" -headers "$native_out/headers" \
        -output "$native_out/CrewClientFFI.xcframework"
else
    xcodebuild -create-xcframework -library "$CARGO_TARGET_DIR/aarch64-apple-ios/release/libcrew_client_ffi.a" -headers "$native_out/headers" \
        -library "$CARGO_TARGET_DIR/aarch64-apple-ios-sim/release/libcrew_client_ffi.a" -headers "$native_out/headers" \
        -output "$native_out/CrewClientFFI.xcframework"
fi
plutil -convert json -o "$native_out/artifact-info.json" "$native_out/CrewClientFFI.xcframework/Info.plist"
find "$native_out/CrewClientFFI.xcframework" -type f -exec shasum -a 256 {} + > "$native_out/artifact.sha256"
shasum -a 256 -c "$native_out/artifact.sha256"
echo "Apple bindings staged: $native_out"
