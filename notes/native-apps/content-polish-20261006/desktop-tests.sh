#!/usr/bin/env bash
# desktop-tests.sh OUT RUNTIME [--window]: crew-desktop GTK assertions on a private
# Xvfb display and session bus, as tests/run-window-test.sh does, with this
# card's own Cargo target and a short private runtime directory for sockets.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
if [ "${1:-}" != --inside ]; then
    out=$1 runtime=$2 mode=${3:-}
    mkdir -p "$out"/{state,config,cache} "$runtime"
    chmod 700 "$runtime"
    for name in ${!GIT_@} ${!CREW_@} ${!HERDR_@}; do unset "$name"; done
    unset AT_SPI_BUS_ADDRESS WAYLAND_DISPLAY
    export XDG_RUNTIME_DIR="$runtime" XDG_STATE_HOME="$out/state"
    export XDG_CONFIG_HOME="$out/config" XDG_CACHE_HOME="$out/cache"
    export CREW_GLOBAL_CONFIG="$out/config/crew.toml"
    exec "$repo/scripts/native/linux-env.sh" --headless dbus-run-session -- "$0" --inside "$out" "$mode"
fi
out=$2 mode=${3:-}
cd "$repo"
display=$((300 + $$ % 10000))
for _ in {1..20}; do
    if [ ! -e "/tmp/.X$display-lock" ] && [ ! -e "/tmp/.X11-unix/X$display" ]; then break; fi
    display=$((display + 1))
done
Xvfb ":$display" -screen 0 1440x960x24 -dpi 96 -nolisten tcp >"$out/xvfb.log" 2>&1 &
xvfb=$!
trap 'kill $xvfb 2>/dev/null || true; wait $xvfb 2>/dev/null || true' EXIT
for _ in {1..100}; do [ -e "/tmp/.X11-unix/X$display" ] && break; sleep 0.1; done
export DISPLAY=":$display" GDK_BACKEND=x11 GSK_RENDERER=cairo GDK_SCALE=1 GDK_DPI_SCALE=1 GDK_DEBUG=no-portals
dbus-update-activation-environment DISPLAY GDK_BACKEND GSK_RENDERER GDK_DEBUG XDG_RUNTIME_DIR
export CARGO_INCREMENTAL=0 GTK_A11Y=test
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo/target/native-validation/content-polish/cargo}"
if [ "$mode" = --window ]; then
    cargo test --locked -p crew-desktop --features native-ui --test window -- --nocapture 2>&1 | tee "$out/test.log"
else
    export CREW_NATIVE_UI_TEST_PRIVATE=1 CREW_NATIVE_UI_EVIDENCE_DIR="$out"
    cargo test --locked -p crew-desktop --features native-ui,local --lib -- --nocapture 2>&1 | tee "$out/test.log"
fi
