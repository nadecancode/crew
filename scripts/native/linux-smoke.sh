#!/usr/bin/env bash
# Observe a real mapped native window on a private display and session bus.
# This checks startup/capture, not daemon actions or accessibility semantics.
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../.." && pwd)
source "$native_repo/scripts/native/common.sh"

inside=false
if [ "${1:-}" = --inside-session ]; then inside=true; shift; fi
native_out=""
native_timeout=20
while [ "$#" -gt 0 ]; do
    case "$1" in
        --out) [ "$#" -ge 2 ] || exit 64; native_out=$2; shift 2 ;;
        --timeout) [ "$#" -ge 2 ] || exit 64; native_timeout=$2; shift 2 ;;
        --) shift; break ;;
        *) echo "usage: scripts/native/linux-smoke.sh [--out directory] [--timeout seconds] -- executable [args...]" >&2; exit 64 ;;
    esac
done
if [ "$#" -eq 0 ] || ! [[ "$native_timeout" =~ ^[1-9][0-9]*$ ]]; then
    echo "a command and positive integer timeout are required" >&2
    exit 64
fi
if ! "$inside"; then
    # Resolve the executable before moving into an isolated working directory.
    native_executable=$(command -v "$1") || { echo "executable unavailable: $1" >&2; exit 2; }
    native_executable=$(realpath "$native_executable")
    shift
    native_new_output "$native_out" linux
    native_out=$NATIVE_RUN_DIR
    mkdir -p "$native_out"/{runtime,state,config,cache,work}
    chmod 700 "$native_out/runtime"
    unset CREW_IDENTITY CREW_SOCKET CREW_LAUNCH AT_SPI_BUS_ADDRESS
    export XDG_RUNTIME_DIR="$native_out/runtime" XDG_STATE_HOME="$native_out/state"
    export XDG_CONFIG_HOME="$native_out/config" XDG_CACHE_HOME="$native_out/cache"
    export CREW_GLOBAL_CONFIG="$native_out/config/crew.toml"
    exec "$native_repo/scripts/native/linux-env.sh" --headless dbus-run-session -- \
        "$native_repo/scripts/native/linux-smoke.sh" --inside-session --out "$native_out" --timeout "$native_timeout" -- "$native_executable" "$@"
fi

native_xvfb_pid=""
native_app_pid=""
cleanup() {
    # Only processes this invocation started; never match by name or global PID list.
    for pid in "$native_app_pid" "$native_xvfb_pid"; do
        if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
    done
    for _ in {1..20}; do
        native_alive=false
        for pid in "$native_app_pid" "$native_xvfb_pid"; do
            if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then native_alive=true; fi
        done
        if ! "$native_alive"; then break; fi
        sleep 0.05
    done
    for pid in "$native_app_pid" "$native_xvfb_pid"; do
        if [ -n "$pid" ]; then
            kill -KILL "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
native_display=$((200 + $$ % 10000))
for _ in {1..20}; do
    if [ ! -e "/tmp/.X$native_display-lock" ] && [ ! -e "/tmp/.X11-unix/X$native_display" ]; then break; fi
    native_display=$((native_display + 1))
done
# A high display number also avoids a lazy Xwayland server's reserved :0.
Xvfb ":$native_display" -screen 0 1440x960x24 -dpi 96 -nolisten tcp >"$native_out/xvfb.log" 2>&1 &
native_xvfb_pid=$!
native_end=$((SECONDS + native_timeout))
native_lock_pid=""
while [ "$native_lock_pid" != "$native_xvfb_pid" ]; do
    if ! kill -0 "$native_xvfb_pid" 2>/dev/null || [ "$SECONDS" -ge "$native_end" ]; then
        echo "private display failed; see $native_out/xvfb.log" >&2; exit 1
    fi
    if [ -f "/tmp/.X$native_display-lock" ]; then read -r native_lock_pid < "/tmp/.X$native_display-lock"; fi
    sleep 0.1
done
printf '%s\n' "$native_display" > "$native_out/display"
export DISPLAY=":$native_display" GDK_BACKEND=x11 GSK_RENDERER=cairo GDK_SCALE=1 GDK_DPI_SCALE=1
# Capture has no desktop portal dependency. Keep activation off the live desktop.
export GDK_DEBUG=no-portals
unset WAYLAND_DISPLAY
dbus-update-activation-environment DISPLAY GDK_BACKEND GSK_RENDERER GDK_DEBUG XDG_RUNTIME_DIR
{
    printf 'GTK '; pkg-config --modversion gtk4
    printf 'VTE '; pkg-config --modversion vte-2.91-gtk4
    printf 'GLib '; pkg-config --modversion glib-2.0
    xwininfo -version
    magick -version | head -n 1
    dbus-run-session --version
} > "$native_out/tool-versions.txt"
cd "$native_out/work"
"$@" >"$native_out/app.log" 2>&1 &
native_app_pid=$!
printf 'app_pid=%s\nxvfb_pid=%s\n' "$native_app_pid" "$native_xvfb_pid" >"$native_out/processes.txt"
native_window=""
while [ "$SECONDS" -lt "$native_end" ]; do
    if ! kill -0 "$native_app_pid" 2>/dev/null; then
        echo "application exited before a mapped window; see $native_out/app.log" >&2; exit 1
    fi
    xwininfo -root -children >"$native_out/windows.txt"
    while read -r id; do
        xwininfo -id "$id" -stats >"$native_out/window.txt" 2>/dev/null || continue
        if awk '/Width:/ { w=$2 } /Height:/ { h=$2 } /Map State:/ { mapped=($3 == "IsViewable") } END { exit !(mapped && w>1 && h>1) }' "$native_out/window.txt"; then
            native_window=$id; break
        fi
    done < <(awk '$1 ~ /^0x[0-9a-f]+$/ { print $1 }' "$native_out/windows.txt")
    if [ -n "$native_window" ]; then break; fi
    sleep 0.1
done
if [ -z "$native_window" ]; then echo "no mapped native window within ${native_timeout}s" >&2; exit 1; fi
sleep 0.5
kill -0 "$native_app_pid" 2>/dev/null || { echo "application exited before capture" >&2; exit 1; }
magick import -display "$DISPLAY" -window root "$native_out/screen.png"
magick identify "$native_out/screen.png" >"$native_out/image.txt"
printf 'display=%s\nwindow=%s\napp_pid=%s\nxvfb_pid=%s\nscreen=1440x960x24\ndpi=96\nGDK_SCALE=1\nGDK_DPI_SCALE=1\nGDK_BACKEND=x11\nGSK_RENDERER=cairo\nGDK_DEBUG=no-portals\n' "$DISPLAY" "$native_window" "$native_app_pid" "$native_xvfb_pid" >"$native_out/observed.txt"
echo "native window observed; capture $native_out/screen.png"
