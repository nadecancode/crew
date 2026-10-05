#!/usr/bin/env bash
# Run actual GTK assertions on an owned X11 display/session bus.
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../../.." && pwd)
source "$native_repo/scripts/native/common.sh"
cd "$native_repo"

if [ "${1:-}" != --inside-session ]; then
    native_new_output "${NATIVE_LINUX_TEST_OUT:-}" linux-window-test
    mkdir -p "$NATIVE_RUN_DIR"/{runtime,state,config,cache}
    chmod 700 "$NATIVE_RUN_DIR/runtime"
    unset CREW_IDENTITY CREW_SOCKET CREW_LAUNCH AT_SPI_BUS_ADDRESS
    export XDG_RUNTIME_DIR="$NATIVE_RUN_DIR/runtime" XDG_STATE_HOME="$NATIVE_RUN_DIR/state"
    export XDG_CONFIG_HOME="$NATIVE_RUN_DIR/config" XDG_CACHE_HOME="$NATIVE_RUN_DIR/cache"
    export CREW_GLOBAL_CONFIG="$NATIVE_RUN_DIR/config/crew.toml"
    exec "$native_repo/scripts/native/linux-env.sh" --headless dbus-run-session -- \
        "$0" --inside-session "$NATIVE_RUN_DIR"
fi
native_out=$2
native_cargo_target
native_display=$((200 + $$ % 10000))
for _ in {1..20}; do
    if [ ! -e "/tmp/.X$native_display-lock" ] && [ ! -e "/tmp/.X11-unix/X$native_display" ]; then break; fi
    native_display=$((native_display + 1))
done
Xvfb ":$native_display" -screen 0 1440x960x24 -dpi 96 -nolisten tcp >"$native_out/xvfb.log" 2>&1 &
native_xvfb_pid=$!
cleanup() {
    kill "$native_xvfb_pid" 2>/dev/null || true
    wait "$native_xvfb_pid" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
native_end=$((SECONDS + 10))
native_lock_pid=""
while [ "$native_lock_pid" != "$native_xvfb_pid" ]; do
    if ! kill -0 "$native_xvfb_pid" 2>/dev/null || [ "$SECONDS" -ge "$native_end" ]; then
        echo "private display failed: $native_out/xvfb.log" >&2; exit 1
    fi
    if [ -f "/tmp/.X$native_display-lock" ]; then read -r native_lock_pid < "/tmp/.X$native_display-lock"; fi
    sleep 0.1
done
export DISPLAY=":$native_display" GDK_BACKEND=x11 GSK_RENDERER=cairo GDK_SCALE=1 GDK_DPI_SCALE=1 GDK_DEBUG=no-portals
# GTK's test accessibility context retains the widget metadata queried below.
# It is not a screen-reader/AT-SPI journey result.
export GTK_A11Y=test
unset WAYLAND_DISPLAY
dbus-update-activation-environment DISPLAY GDK_BACKEND GSK_RENDERER GDK_DEBUG XDG_RUNTIME_DIR
{
    rustc -Vv
    cargo -V
    pkg-config --modversion gtk4 glib-2.0
    printf 'screen=1440x960x24\ndpi=96\nGDK_SCALE=1\nGDK_DPI_SCALE=1\nGDK_BACKEND=x11\nGSK_RENDERER=cairo\n'
} >"$native_out/tool-versions.txt"
cargo test --locked -p crew-desktop --features native-ui --test window -- --nocapture 2>&1 | tee "$native_out/test.log"
echo "native window assertions passed; evidence $native_out"
