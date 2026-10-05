#!/usr/bin/env bash
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../.." && pwd)
source "$native_repo/scripts/native/common.sh"
cd "$native_repo"
if [ ! -f crates/crew-desktop/Cargo.toml ]; then
    echo "unavailable: crates/crew-desktop/Cargo.toml has not landed" >&2
    exit 2
fi
native_cargo_target
exec ./scripts/native/linux-env.sh cargo build --locked -p crew-desktop --features native-ui,local "$@"
