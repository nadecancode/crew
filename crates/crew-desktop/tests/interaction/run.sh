#!/usr/bin/env bash
# Keyboard and focus journey of the real Linux app against a private scripted
# team: builds the app and the crew CLI, starts session.sh and checks the
# focus and accessibility steps of drive.sh. Called by
# `tests/run-window-test.sh --interaction`, never against the active desktop.
# Usage: run.sh OUT
set -euo pipefail
native_out=$1
native_repo=$(cd "$(dirname "$0")/../../../.." && pwd)
here=$(cd "$(dirname "$0")" && pwd)
source "$native_repo/scripts/native/common.sh"
case "$native_out" in "$native_repo/target/native-validation/"*) ;; *) exit 64 ;; esac
for native_tool in python3 openbox xdotool magick Xvfb dbus-daemon pkg-config; do
    command -v "$native_tool" >/dev/null || { echo "interaction run requires $native_tool" >&2; exit 2; }
done
python3 -c 'import pyatspi' 2>/dev/null || { echo "interaction run requires Python pyatspi" >&2; exit 2; }
pkg-config --exists atspi-2 || { echo "interaction run requires at-spi2-core" >&2; exit 2; }
native_cargo_target
cargo build --locked -p crew-desktop --features native-ui,local -j 2 2>&1 | tee "$native_out/build-app.log"
cargo build --locked -p crew --bin crew -j 2 2>&1 | tee "$native_out/build-crew.log"
# Daemon and bus sockets must fit the 108-byte Unix limit: a short private
# root, removed afterwards.
native_root=$(mktemp -d /tmp/crew-int.XXXXXX)
native_session_pid=""
cleanup() {
    : >"$native_out/stop"
    if [ -n "$native_session_pid" ]; then wait "$native_session_pid" 2>/dev/null || true; fi
    rm -rf "$native_root"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
bash "$here/session.sh" "$native_out" "$native_root" \
    "$CARGO_TARGET_DIR/debug/crew-desktop" "$CARGO_TARGET_DIR/debug/crew" \
    "$here/team.yaml.in" >"$native_out/session.log" 2>&1 &
native_session_pid=$!
native_end=$((SECONDS + 60))
until grep -q "session ready" "$native_out/session.log" 2>/dev/null; do
    kill -0 "$native_session_pid" 2>/dev/null || { echo "session failed: $native_out/session.log" >&2; exit 1; }
    [ "$SECONDS" -lt "$native_end" ] || { echo "session did not start" >&2; exit 1; }
    sleep 0.5
done
bash "$here/drive.sh" "$native_out"
echo "native interaction journey passed; transcript $native_out/atspi-keyboard.txt"
