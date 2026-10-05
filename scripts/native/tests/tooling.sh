#!/usr/bin/env bash
# Proves the harness with a GTK toolkit probe, not with the future Crew app.
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../../.." && pwd)
source "$native_repo/scripts/native/common.sh"
if [ "${1:-}" != --in-environment ]; then
    if [ "$#" -ne 0 ]; then echo "usage: scripts/native/tests/tooling.sh" >&2; exit 64; fi
    native_new_output "" tooling
    native_out=$NATIVE_RUN_DIR
    exec "$native_repo/scripts/native/linux-env.sh" --headless \
        "$native_repo/scripts/native/tests/tooling.sh" --in-environment "$native_out"
fi
native_out=$2
read -r -a native_flags <<< "$(pkg-config --cflags --libs gtk4)"
cc "$native_repo/scripts/native/tests/probe.c" -o "$native_out/probe" "${native_flags[@]}"
"$native_repo/scripts/native/linux-smoke.sh" --out "$native_out/window" -- "$native_out/probe"
if "$native_repo/scripts/native/linux-smoke.sh" --out "$native_out/early-exit" -- "$(command -v true)" >"$native_out/early-exit.log" 2>&1; then
    echo "an exited application was accepted" >&2; exit 1
fi
if "$native_repo/scripts/native/linux-smoke.sh" --out "$native_out/no-window" --timeout 1 -- "$(command -v sleep)" 30 >"$native_out/no-window.log" 2>&1; then
    echo "an application without a window was accepted" >&2; exit 1
fi
for result in window early-exit no-window; do
    while IFS='=' read -r key pid; do
        if kill -0 "$pid" 2>/dev/null; then echo "process remained after $result: $key=$pid" >&2; exit 1; fi
    done < "$native_out/$result/processes.txt"
done
echo "tooling checks passed: GTK mapped-window capture, early exit, no window, process cleanup"
echo "artifacts: $native_out"
