#!/usr/bin/env bash
# Diagnostic-only process group. Never uses the operator display or Crew service.
set -euo pipefail
umask 077
menu_out=$1
menu_binary=$2
menu_comparison=$3
mkdir -p "$menu_out"/{runtime,state,config,cache,data,work}
chmod 700 "$menu_out/runtime"
for menu_var in ${!GIT_@} ${!CREW_@}; do unset "$menu_var"; done
unset WAYLAND_DISPLAY AT_SPI_BUS_ADDRESS
export XDG_RUNTIME_DIR="$menu_out/runtime" XDG_STATE_HOME="$menu_out/state"
export XDG_CONFIG_HOME="$menu_out/config" XDG_CACHE_HOME="$menu_out/cache"
export XDG_DATA_HOME="$menu_out/data"
export CREW_GLOBAL_CONFIG="$menu_out/config/crew.toml"
menu_xvfb_pid="" menu_app_pid="" menu_wm_pid="" menu_a11y_pid="" menu_comparison_pid=""
cleanup() {
  for menu_pid in "$menu_app_pid" "$menu_comparison_pid" "$menu_wm_pid" "$menu_a11y_pid" "$menu_xvfb_pid"; do
    if [ -n "$menu_pid" ]; then kill "$menu_pid" 2>/dev/null || true; fi
  done
  wait || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
menu_display=$((400 + $$ % 10000))
for _ in {1..20}; do
  if [ ! -e "/tmp/.X$menu_display-lock" ] && [ ! -e "/tmp/.X11-unix/X$menu_display" ]; then break; fi
  menu_display=$((menu_display + 1))
done
Xvfb ":$menu_display" -screen 0 1440x960x24 -dpi 96 -nolisten tcp >"$menu_out/xvfb.log" 2>&1 &
menu_xvfb_pid=$!
menu_lock_pid=""
for _ in {1..100}; do
  if [ -f "/tmp/.X$menu_display-lock" ]; then read -r menu_lock_pid < "/tmp/.X$menu_display-lock"; fi
  if [ "$menu_lock_pid" = "$menu_xvfb_pid" ]; then break; fi
  kill -0 "$menu_xvfb_pid"
  sleep 0.1
done
test "$menu_lock_pid" = "$menu_xvfb_pid"
export DISPLAY=":$menu_display" GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=atspi
export GDK_SCALE=1 GDK_DPI_SCALE=1 GDK_DEBUG=no-portals
export AT_SPI_BUS_ADDRESS="unix:abstract=crew-designer-menu-a11y-$$"
menu_atspi_prefix=$(pkg-config --variable=prefix atspi-2)
dbus-daemon --nofork --nopidfile --config-file="$menu_atspi_prefix/share/defaults/at-spi2/accessibility.conf" --address="$AT_SPI_BUS_ADDRESS" >"$menu_out/a11y-bus.log" 2>&1 &
menu_a11y_pid=$!
dbus-update-activation-environment DISPLAY GDK_BACKEND GSK_RENDERER GTK_A11Y GDK_DEBUG XDG_RUNTIME_DIR AT_SPI_BUS_ADDRESS
openbox --sm-disable >"$menu_out/window-manager.log" 2>&1 &
menu_wm_pid=$!
for menu_key in DISPLAY DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS PATH LD_LIBRARY_PATH PYTHONPATH GI_TYPELIB_PATH PKG_CONFIG_PATH XDG_DATA_DIRS XDG_DATA_HOME XDG_RUNTIME_DIR XDG_STATE_HOME XDG_CONFIG_HOME XDG_CACHE_HOME GDK_BACKEND GSK_RENDERER GDK_DEBUG GTK_A11Y GDK_PIXBUF_MODULE_FILE; do
  printf 'export %s=%q\n' "$menu_key" "${!menu_key-}"
done >"$menu_out/session.env"
{
  printf 'GTK '; pkg-config --modversion gtk4
  printf 'GLib '; pkg-config --modversion glib-2.0
  printf 'AT-SPI '; pkg-config --modversion atspi-2
  xdotool version
  magick -version | head -n 1
  python3 --version
  sha256sum "$menu_binary" "$menu_comparison"
} >"$menu_out/tool-versions.txt"
cd "$menu_out/work"
"$menu_binary" >"$menu_out/app.log" 2>&1 &
menu_app_pid=$!
python3 "$menu_comparison" >"$menu_out/comparison.log" 2>&1 &
menu_comparison_pid=$!
printf 'app_pid=%s\ncomparison_pid=%s\nxvfb_pid=%s\nwm_pid=%s\na11y_bus_pid=%s\ndisplay=%s\n' "$menu_app_pid" "$menu_comparison_pid" "$menu_xvfb_pid" "$menu_wm_pid" "$menu_a11y_pid" "$DISPLAY" >"$menu_out/processes.txt"
printf 'Private menu session ready: %s\n' "$menu_out"
menu_end=$((SECONDS + 1500))
while [ ! -f "$menu_out/stop" ] && [ "$SECONDS" -lt "$menu_end" ]; do
  kill -0 "$menu_app_pid" 2>/dev/null || break
  sleep 1
done
