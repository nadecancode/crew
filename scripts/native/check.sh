#!/usr/bin/env bash
# N1's portable extraction checks; later conformance tests live in these crates.
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../.." && pwd)
source "$native_repo/scripts/native/common.sh"
cd "$native_repo"
native_conformance=false
if [ "${1:-}" = --conformance ]; then native_conformance=true; shift; fi
if [ "$#" -ne 0 ]; then echo "usage: scripts/native/check.sh [--conformance]" >&2; exit 64; fi
for crate in crew-protocol crew-client; do
    if [ ! -f "crates/$crate/Cargo.toml" ]; then
        echo "unavailable: crates/$crate/Cargo.toml has not landed" >&2
        exit 2
    fi
done
if ! rustup target list --installed | awk '$0 == "wasm32-unknown-unknown" { found=1 } END { exit !found }'; then
    echo "unavailable: install wasm32-unknown-unknown with rustup target add" >&2
    exit 2
fi
if "$native_conformance"; then
    for checker in fixtures/native/state/check.mjs fixtures/native/actions/check.ts; do
        if [ ! -f "$checker" ]; then echo "unavailable: $checker has not landed" >&2; exit 2; fi
    done
    for tool in node bun; do
        if ! command -v "$tool" >/dev/null; then echo "unavailable: $tool is required for fixture adapters" >&2; exit 2; fi
    done
fi
native_cargo_target
export CREW_NATIVE_FIXTURE_CACHE="$native_repo/target/native-validation/fixture-cache"
./scripts/build-env cargo test --locked -p crew-protocol -p crew-client
./scripts/build-env cargo test --locked -p crew-tui --test render
./scripts/build-env cargo check --locked --target wasm32-unknown-unknown -p crew-protocol -p crew-client
cargo tree --locked -p crew-protocol -e normal
cargo tree --locked -p crew-client -e normal
if "$native_conformance"; then
    ./scripts/build-env node fixtures/native/state/check.mjs
    ./scripts/build-env bun fixtures/native/actions/check.ts --wire
fi
