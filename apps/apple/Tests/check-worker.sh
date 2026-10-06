#!/usr/bin/env bash
# Portable source validation only: actual generated records and the Foundation
# worker/observer adapter. This does not typecheck SwiftUI, link Rust, or build
# any Apple target. Output stays in this worktree's ignored target directory.
set -euo pipefail
crew_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
if [[ ${1:-} != --inside ]]; then
    printf -v crew_replay 'bash %q --inside' "${BASH_SOURCE[0]}"
    exec nix-shell --pure -E '
      let p = import /nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source {};
      in p.mkShell {
        packages = [ p.swift ];
        CREW_SOURCE_FOUNDATION = p.swiftPackages.Foundation;
        CREW_SOURCE_FOUNDATION_DEV = p.lib.getDev p.swiftPackages.Foundation;
        CREW_SOURCE_DISPATCH = p.swiftPackages.Dispatch;
        CREW_SOURCE_DISPATCH_DEV = p.lib.getDev p.swiftPackages.Dispatch;
        CREW_SOURCE_XCTEST = p.swiftPackages.XCTest;
      }
    ' --run "$crew_replay"
fi
cd "$crew_root"
crew_output="$crew_root/target/native-apple-ui/worker-check"
mkdir -p "$crew_output"
crew_includes=(
    -I "$CREW_SOURCE_FOUNDATION/lib/swift/linux/x86_64"
    -I "$CREW_SOURCE_FOUNDATION_DEV/include"
    -I "$CREW_SOURCE_DISPATCH/lib/swift"
    -I "$CREW_SOURCE_DISPATCH_DEV/include"
    -I apps/apple/CrewClient/Sources/Generated/Headers
    -Xcc -fmodule-map-file=apps/apple/CrewClient/Sources/Generated/Headers/CrewClientFFI.modulemap
)
swiftc -version
swiftc -emit-module -parse-as-library -swift-version 5 -module-name CrewBindings \
    "${crew_includes[@]}" \
    apps/apple/CrewClient/Sources/Generated/Swift/CrewBindings.swift \
    -o "$crew_output/CrewBindings.swiftmodule"
swiftc -typecheck -parse-as-library -swift-version 5 \
    "${crew_includes[@]}" -I "$crew_output" \
    apps/apple/CrewHost/CrewClientWorker.swift \
    apps/apple/CrewHost/CrewGateway.swift \
    apps/apple/CrewHost/CrewSendDraft.swift \
    apps/apple/CrewHost/CrewNoticeRouting.swift \
    apps/apple/CrewHost/CrewExternalLinks.swift
# The Foundation-only unit test sources typecheck together with those host
# sources against the corelibs XCTest module. They are not run here: running
# them needs the Rust FFI library, and their Apple targets are not built.
swiftc -typecheck -parse-as-library -swift-version 5 \
    "${crew_includes[@]}" -I "$crew_output" \
    -I "$CREW_SOURCE_XCTEST/lib/swift/linux/x86_64" \
    apps/apple/CrewHost/CrewGateway.swift \
    apps/apple/CrewHost/CrewSendDraft.swift \
    apps/apple/CrewHost/CrewNoticeRouting.swift \
    apps/apple/CrewHost/CrewExternalLinks.swift \
    apps/apple/Tests/CrewSendDraftTests.swift \
    apps/apple/Tests/CrewNoticeRoutingTests.swift \
    apps/apple/Tests/CrewExternalLinksTests.swift \
    apps/apple/Tests/CrewGatewayTests.swift
printf '%s\n' 'Portable generated-record/worker and Foundation test-source check passed.' \
    'Apple SwiftUI compilation, linking, Xcode and native execution: deferred by operator.'
