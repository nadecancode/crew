#!/usr/bin/env bash
# Private interaction session (from the design precheck's harness): owned
# Xvfb, window manager, session and accessibility buses, an isolated Crew
# daemon with a scripted team, and one crew-desktop client. Never uses the
# operator display, daemon, listener or provider CLIs.
# Usage: session.sh OUT SHORT_ROOT DESKTOP_BIN CREW_BIN TEAM_TEMPLATE
set -euo pipefail
umask 077
dp_out=$1 dp_root=$2 dp_desktop=$3 dp_crew=$4 dp_team=$5
mkdir -p "$dp_out" "$dp_root"/{run,state,config,cache,data,etc,ws,codex-home,claude-home}
chmod 700 "$dp_root/run"
for dp_var in ${!GIT_@} ${!CREW_@} ${!HERDR_@}; do unset "$dp_var"; done
unset WAYLAND_DISPLAY AT_SPI_BUS_ADDRESS
export XDG_RUNTIME_DIR="$dp_root/run" XDG_STATE_HOME="$dp_root/state"
export XDG_CONFIG_HOME="$dp_root/config" XDG_CACHE_HOME="$dp_root/cache"
export XDG_DATA_HOME="$dp_root/data" CREW_GLOBAL_CONFIG="$dp_root/etc/config.toml"
export CODEX_HOME="$dp_root/codex-home" CLAUDE_CONFIG_DIR="$dp_root/claude-home"
# The daemon may run provider CLIs (a usage probe); private stubs answer.
mkdir -p "$dp_root/shims"
for dp_provider in claude codex; do
  printf '#!/bin/sh\nexit 127\n' >"$dp_root/shims/$dp_provider"
  chmod 700 "$dp_root/shims/$dp_provider"
done
export PATH="$dp_root/shims:$PATH"
dp_xvfb_pid="" dp_app_pid="" dp_wm_pid="" dp_a11y_pid=""
cleanup() {
  if [ -n "$dp_app_pid" ]; then kill "$dp_app_pid" 2>/dev/null || true; fi
  (cd "$dp_root" && "$dp_crew" stop -C "$dp_root/ws" >"$dp_out/crew-stop.log" 2>&1) || true
  for dp_pid in "$dp_wm_pid" "$dp_a11y_pid" "$dp_xvfb_pid"; do
    if [ -n "$dp_pid" ]; then kill "$dp_pid" 2>/dev/null || true; fi
  done
  wait || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
dp_display=$((500 + $$ % 10000))
for _ in {1..20}; do
  if [ ! -e "/tmp/.X$dp_display-lock" ] && [ ! -e "/tmp/.X11-unix/X$dp_display" ]; then break; fi
  dp_display=$((dp_display + 1))
done
Xvfb ":$dp_display" -screen 0 1440x960x24 -dpi 96 -nolisten tcp >"$dp_out/xvfb.log" 2>&1 &
dp_xvfb_pid=$!
dp_lock_pid=""
for _ in {1..100}; do
  if [ -f "/tmp/.X$dp_display-lock" ]; then read -r dp_lock_pid < "/tmp/.X$dp_display-lock"; fi
  if [ "$dp_lock_pid" = "$dp_xvfb_pid" ]; then break; fi
  kill -0 "$dp_xvfb_pid"
  sleep 0.1
done
test "$dp_lock_pid" = "$dp_xvfb_pid"
export DISPLAY=":$dp_display" GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=atspi
export GDK_SCALE=1 GDK_DPI_SCALE=1 GDK_DEBUG=no-portals
export AT_SPI_BUS_ADDRESS="unix:path=$dp_root/run/a11y-bus"
dp_atspi_prefix=$(pkg-config --variable=prefix atspi-2)
dbus-daemon --nofork --nopidfile --config-file="$dp_atspi_prefix/share/defaults/at-spi2/accessibility.conf" --address="$AT_SPI_BUS_ADDRESS" >"$dp_out/a11y-bus.log" 2>&1 &
dp_a11y_pid=$!
dbus-update-activation-environment DISPLAY GDK_BACKEND GSK_RENDERER GTK_A11Y GDK_DEBUG XDG_RUNTIME_DIR AT_SPI_BUS_ADDRESS
openbox --sm-disable >"$dp_out/window-manager.log" 2>&1 &
dp_wm_pid=$!
sed "s|@WS@|$dp_root/ws|" "$dp_team" >"$dp_root/team.yaml"
(cd "$dp_root" && "$dp_crew" run team.yaml --no-attach) >"$dp_out/crew-run.log" 2>&1
for dp_key in DISPLAY DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS PATH LD_LIBRARY_PATH PYTHONPATH GI_TYPELIB_PATH PKG_CONFIG_PATH XDG_DATA_DIRS XDG_DATA_HOME XDG_RUNTIME_DIR XDG_STATE_HOME XDG_CONFIG_HOME XDG_CACHE_HOME CREW_GLOBAL_CONFIG CODEX_HOME CLAUDE_CONFIG_DIR GDK_BACKEND GSK_RENDERER GDK_DEBUG GTK_A11Y GDK_PIXBUF_MODULE_FILE; do
  printf 'export %s=%q\n' "$dp_key" "${!dp_key-}"
done >"$dp_out/session.env"
{
  printf 'GTK '; pkg-config --modversion gtk4
  printf 'GLib '; pkg-config --modversion glib-2.0
  printf 'AT-SPI '; pkg-config --modversion atspi-2
  xdotool version
  magick -version | head -n 1
  sha256sum "$dp_desktop" "$dp_crew"
} >"$dp_out/tool-versions.txt"
mkdir -p "$dp_root/work"
cd "$dp_root/work"
"$dp_desktop" >"$dp_out/app.log" 2>&1 &
dp_app_pid=$!
printf 'app_pid=%s\nxvfb_pid=%s\nwm_pid=%s\na11y_bus_pid=%s\ndisplay=%s\nroot=%s\n' "$dp_app_pid" "$dp_xvfb_pid" "$dp_wm_pid" "$dp_a11y_pid" "$DISPLAY" "$dp_root" >"$dp_out/processes.txt"
printf 'Private precheck session ready: %s\n' "$dp_out"
dp_end=$((SECONDS + 3600))
while [ ! -f "$dp_out/stop" ] && [ "$SECONDS" -lt "$dp_end" ]; do
  if [ -f "$dp_out/restart-app" ]; then
    rm -f "$dp_out/restart-app"
    kill "$dp_app_pid" 2>/dev/null || true; wait "$dp_app_pid" 2>/dev/null || true
    "$dp_desktop" >>"$dp_out/app.log" 2>&1 &
    dp_app_pid=$!
    sed -i "s/^app_pid=.*/app_pid=$dp_app_pid/" "$dp_out/processes.txt"
  fi
  sleep 1
done
