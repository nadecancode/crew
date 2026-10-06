#!/usr/bin/env bash
# Run actual GTK assertions on an owned X11 display/session bus.
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../../.." && pwd)
source "$native_repo/scripts/native/common.sh"
cd "$native_repo"

if [ "${1:-}" != --inside-session ]; then
    case "${1:-}" in
        ""|--keyboard|--client|--gateway) native_mode=${1:-} ;;
        *) echo "usage: tests/run-window-test.sh [--keyboard|--client|--gateway]" >&2; exit 64 ;;
    esac
    native_new_output "${NATIVE_LINUX_TEST_OUT:-}" linux-window-test
    mkdir -p "$NATIVE_RUN_DIR"/{runtime,state,config,cache}
    chmod 700 "$NATIVE_RUN_DIR/runtime"
    unset CREW_IDENTITY CREW_SOCKET CREW_LAUNCH AT_SPI_BUS_ADDRESS
    unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE
    export XDG_RUNTIME_DIR="$NATIVE_RUN_DIR/runtime" XDG_STATE_HOME="$NATIVE_RUN_DIR/state"
    export XDG_CONFIG_HOME="$NATIVE_RUN_DIR/config" XDG_CACHE_HOME="$NATIVE_RUN_DIR/cache"
    export CREW_GLOBAL_CONFIG="$NATIVE_RUN_DIR/config/crew.toml"
    exec "$native_repo/scripts/native/linux-env.sh" --headless dbus-run-session -- \
        "$0" --inside-session "$NATIVE_RUN_DIR" "$native_mode"
fi
native_out=$2
native_mode=${3:-}
native_cargo_target
native_display=$((200 + $$ % 10000))
for _ in {1..20}; do
    if [ ! -e "/tmp/.X$native_display-lock" ] && [ ! -e "/tmp/.X11-unix/X$native_display" ]; then break; fi
    native_display=$((native_display + 1))
done
Xvfb ":$native_display" -screen 0 1440x960x24 -dpi 96 -nolisten tcp >"$native_out/xvfb.log" 2>&1 &
native_xvfb_pid=$!
native_window_manager_pid=""
native_keyring_pid=""
native_keyring_run=""
cleanup() {
    if [ -n "$native_keyring_pid" ]; then
        kill "$native_keyring_pid" 2>/dev/null || true
        wait "$native_keyring_pid" 2>/dev/null || true
    fi
    if [ -n "$native_keyring_run" ]; then rm -rf "$native_keyring_run"; fi
    if [ -n "$native_window_manager_pid" ]; then
        kill "$native_window_manager_pid" 2>/dev/null || true
        wait "$native_window_manager_pid" 2>/dev/null || true
    fi
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
unset WAYLAND_DISPLAY
dbus-update-activation-environment DISPLAY GDK_BACKEND GSK_RENDERER GDK_DEBUG XDG_RUNTIME_DIR
if [ "$native_mode" = --client ] && [ "${CREW_NATIVE_UI_TEST_WINDOW_MANAGER:-0}" = 1 ]; then
    command -v openbox >/dev/null || { echo "window-manager capture requires openbox" >&2; exit 2; }
    openbox --sm-disable >"$native_out/window-manager.log" 2>&1 &
    native_window_manager_pid=$!
    sleep 0.25
    kill -0 "$native_window_manager_pid" || { echo "private window manager failed" >&2; exit 1; }
    printf 'openbox_pid=%s\n' "$native_window_manager_pid" >"$native_out/window-manager.txt"
else
    printf 'no window manager; functional capture only\n' >"$native_out/window-manager.txt"
fi
{
    rustc -Vv
    cargo -V
    pkg-config --modversion gtk4 glib-2.0
    printf 'screen=1440x960x24\ndpi=96\nGDK_SCALE=1\nGDK_DPI_SCALE=1\nGDK_BACKEND=x11\nGSK_RENDERER=cairo\n'
} >"$native_out/tool-versions.txt"
if [ "$native_mode" = --keyboard ]; then
    unset GTK_A11Y
    cargo build --locked -p crew-desktop --features native-ui,local -j 2 2>&1 | tee "$native_out/build.log"
    "$native_repo/crates/crew-desktop/tests/keyboard.sh" "$native_out" "$CARGO_TARGET_DIR/debug/crew-desktop"
elif [ "$native_mode" = --gateway ]; then
    # Crew and GNOME Keyring sockets need short paths: the long per-run
    # runtime directory would make Crew fall back to the shared /tmp/crew-<uid>.
    # This private short directory is removed on exit.
    native_keyring=$(nix-build ${NATIVE_NIXPKGS:+-I "nixpkgs=$NATIVE_NIXPKGS"} "<nixpkgs>" -A gnome-keyring --no-out-link)/bin/gnome-keyring-daemon
    native_keyring_run=$(mktemp -d /tmp/crew-gw-run-XXXXXX)
    chmod 700 "$native_keyring_run"
    CREW_TEST_XDOTOOL=$(nix-build ${NATIVE_NIXPKGS:+-I "nixpkgs=$NATIVE_NIXPKGS"} "<nixpkgs>" -A xdotool --no-out-link)/bin/xdotool
    export CREW_TEST_XDOTOOL
    export XDG_DATA_HOME="$native_out/data"
    mkdir -p "$XDG_DATA_HOME"
    CREW_TEST_KEYRING_PASSWORD=$(head -c 18 /dev/urandom | base64 | tr -d "/+=")
    export CREW_TEST_KEYRING_PASSWORD
    export XDG_RUNTIME_DIR="$native_keyring_run"
    printf "%s" "$CREW_TEST_KEYRING_PASSWORD" | \
        "$native_keyring" --unlock --components=secrets --foreground >"$native_out/keyring.log" 2>&1 &
    native_keyring_pid=$!
    for _ in {1..100}; do
        if dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
            org.freedesktop.DBus.NameHasOwner string:org.freedesktop.secrets 2>/dev/null | grep -q "boolean true"; then break; fi
        sleep 0.1
    done
    printf "gnome-keyring=%s\nxdotool=%s\n" "$native_keyring" "$CREW_TEST_XDOTOOL" >>"$native_out/tool-versions.txt"
    export GTK_A11Y=test CREW_NATIVE_UI_TEST_PRIVATE=1 CREW_NATIVE_UI_EVIDENCE_DIR="$native_out"
    cargo test --locked -p crew-desktop --features native-ui,gateway-tests --test gateway -- --nocapture 2>&1 | tee "$native_out/test.log"
    echo "native gateway assertions passed; evidence $native_out"
elif [ "$native_mode" = --client ]; then
    export GTK_A11Y=test CREW_NATIVE_UI_TEST_PRIVATE=1 CREW_NATIVE_UI_EVIDENCE_DIR="$native_out"
    native_manifest=$(realpath "${CREW_NATIVE_UI_TEST_MANIFEST:-$native_repo/Cargo.toml}")
    # A lagging worker may stage exact reviewed runtime prerequisites below its
    # own artifact root; never substitute another checkout or operator state.
    case "$native_manifest" in "$native_repo/Cargo.toml"|"$native_repo/target/native-validation/"*/Cargo.toml) ;;
        *) echo "test manifest must stay in this checkout" >&2; exit 64 ;;
    esac
    cargo test --locked --manifest-path "$native_manifest" -p crew-desktop --features native-ui,local --lib -- --nocapture 2>&1 | tee "$native_out/test.log"
    echo "native client assertions passed; evidence $native_out"
else
    # GTK's test context retains metadata queried by the native assertions.
    # This is not a screen-reader/AT-SPI journey result.
    export GTK_A11Y=test
    cargo test --locked -p crew-desktop --features native-ui --test window -- --nocapture 2>&1 | tee "$native_out/test.log"
    echo "native window assertions passed; evidence $native_out"
fi
