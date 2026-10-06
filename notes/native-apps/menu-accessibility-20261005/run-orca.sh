#!/usr/bin/env bash
set -euo pipefail
umask 077
menu_out=$1
source "$menu_out/session.env"
case "$AT_SPI_BUS_ADDRESS" in unix:abstract=crew-designer-menu-a11y-*) ;; *) exit 2 ;; esac
for menu_var in ${!GIT_@} ${!CREW_@}; do unset "$menu_var"; done
export GSETTINGS_BACKEND=memory
export XDG_DATA_HOME="$menu_out/data"
export XDG_SESSION_TYPE=x11
# No host audio service. The package has no usable spawn command; this private
# absent socket also prevents connecting to the operator's speech dispatcher.
export SPEECHD_ADDRESS="unix_socket:$menu_out/no-speech.sock"
mkdir -p "$XDG_DATA_HOME"
menu_short_runtime=$2
case "$menu_short_runtime" in "$PWD/"*) ;; *) echo 'Short runtime must remain in the current workspace' >&2; exit 2 ;; esac
mkdir -p "$menu_short_runtime"
chmod 700 "$menu_short_runtime"
menu_launcher_pid="" menu_orca_pid=""
cleanup() {
  for menu_pid in "$menu_orca_pid" "$menu_launcher_pid"; do
    if [ -n "$menu_pid" ]; then kill "$menu_pid" 2>/dev/null || true; fi
  done
  wait || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# Real private status service. Orca's explicit AT_SPI_BUS_ADDRESS still points
# at the diagnostic bus already used by the applications.
XDG_RUNTIME_DIR="$menu_short_runtime" /nix/store/cjraidmmw39g7xsxsj51sszbx8wplig5-at-spi2-core-2.60.6/libexec/at-spi-bus-launcher --launch-immediately --a11y=1 --screen-reader=1 >"$menu_out/a11y-launcher.log" 2>&1 &
menu_launcher_pid=$!
sleep 0.5
gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus --method org.freedesktop.DBus.Properties.Get org.a11y.Status IsEnabled >"$menu_out/orca-private-status.txt"
/nix/store/r42ln7vhn9dcahfs8gys99amp0h314hv-orca-50.2/bin/orca --debug-file "$menu_out/orca-fixed-debug.txt" >"$menu_out/orca-fixed.log" 2>&1 &
menu_orca_pid=$!
printf 'launcher_pid=%s\norca_pid=%s\n' "$menu_launcher_pid" "$menu_orca_pid" >"$menu_out/orca-fixed-processes.txt"
menu_end=$((SECONDS + 360))
while [ ! -f "$menu_out/stop-orca" ] && [ "$SECONDS" -lt "$menu_end" ]; do
  kill -0 "$menu_orca_pid" 2>/dev/null || break
  sleep 1
done
