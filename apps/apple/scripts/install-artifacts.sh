#!/usr/bin/env bash
# Explicit developer installation; QA uses its private staging tree instead.
set -euo pipefail
native_apple_root=$(cd "$(dirname "$0")/.." && pwd)
native_framework=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --framework) [ "$#" -ge 2 ] || exit 64; native_framework=$2; shift 2 ;;
        *) echo "usage: apps/apple/scripts/install-artifacts.sh --framework <staged-CrewClientFFI.xcframework>" >&2; exit 64 ;;
    esac
done
if [ -z "$native_framework" ]; then echo "--framework is required" >&2; exit 64; fi
if [ "$(uname -s)" != Darwin ] || ! command -v plutil >/dev/null; then
    echo "unavailable: artifact installation requires the Apple runner; Apple verification deferred by operator" >&2
    exit 2
fi
if [ ! -d "$native_framework" ] || [ ! -f "$native_framework/Info.plist" ]; then
    echo "missing staged XCFramework/Info.plist" >&2; exit 2
fi
native_framework=$(cd "$native_framework" && pwd)
native_destination="$native_apple_root/CrewClient/Artifacts/CrewClientFFI.xcframework"
if [ "$native_framework" = "$native_destination" ]; then
    echo "source must be a separate staged XCFramework" >&2; exit 64
fi
if [ -e "$native_destination" ]; then
    echo "destination already exists; choose a fresh checkout/staging tree before installation" >&2; exit 1
fi
native_install_stage=$(mktemp -d "$native_apple_root/CrewClient/Artifacts/.install.XXXXXX")
trap 'rm -rf "$native_install_stage"' EXIT
plutil -convert json -o "$native_install_stage/info.json" "$native_framework/Info.plist"
/usr/bin/python3 - "$native_install_stage/info.json" "$native_framework" "$native_apple_root" <<'PY'
import json
import pathlib
import sys

info = json.loads(pathlib.Path(sys.argv[1]).read_text())
source = pathlib.Path(sys.argv[2])
apple = pathlib.Path(sys.argv[3])
libraries = info.get("AvailableLibraries", [])
if not libraries:
    raise SystemExit("XCFramework has no available libraries")
platforms = {}
for library in libraries:
    identifier = library["LibraryIdentifier"]
    if pathlib.PurePosixPath(identifier).name != identifier:
        raise SystemExit("invalid library identifier")
    platform = (library["SupportedPlatform"], library.get("SupportedPlatformVariant", ""))
    if platform not in {( "macos", ""), ("ios", ""), ("ios", "simulator")}:
        raise SystemExit("unreviewed platform slice")
    platforms.setdefault(platform, set()).update(library["SupportedArchitectures"])
    directory = source / identifier
    library_file = library["LibraryPath"]
    if library_file != "libcrew_client_ffi.a" or not (directory / library_file).is_file():
        raise SystemExit("missing real Crew static library")
    with (directory / library_file).open("rb") as stream:
        magic = stream.read(8)
    if magic != b"!<arch>\n" and magic[:4] not in {b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"}:
        raise SystemExit("library is not a static archive or universal archive")
    if library["HeadersPath"] != "Headers":
        raise SystemExit("invalid headers directory")
    headers = directory / library["HeadersPath"]
    for staged_name, generated_name in (("CrewClientFFI.h", "CrewClientFFI.h"), ("module.modulemap", "CrewClientFFI.modulemap")):
        expected = apple / "CrewClient/Sources/Generated/Headers" / generated_name
        if (headers / staged_name).read_bytes() != expected.read_bytes():
            raise SystemExit("staged ABI header/module map differs from committed generated source")
if any(key[0] == "macos" for key in platforms):
    if not {"arm64", "x86_64"}.issubset(platforms.get(("macos", ""), set())):
        raise SystemExit("macOS requires arm64 and x86_64")
if any(key[0] == "ios" for key in platforms):
    if "arm64" not in platforms.get(("ios", ""), set()) or "arm64" not in platforms.get(("ios", "simulator"), set()):
        raise SystemExit("iOS requires device arm64 and simulator arm64")
print("Validated archive format, platform metadata and matching generated ABI headers.")
PY
cp -R "$native_framework" "$native_install_stage/CrewClientFFI.xcframework"
diff -r "$native_framework" "$native_install_stage/CrewClientFFI.xcframework"
mv "$native_install_stage/CrewClientFFI.xcframework" "$native_destination"
echo "Installed matching staged artifact: $native_destination"
