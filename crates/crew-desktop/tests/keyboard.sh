#!/usr/bin/env bash
# Called only by the owned-display runner, never against the active desktop.
set -euo pipefail
native_out=$1
native_executable=$2
native_repo=$(cd "$(dirname "$0")/../../.." && pwd)
case "$native_out" in "$native_repo/target/native-validation/"*) ;; *) exit 64 ;; esac
case "${DISPLAY:-}" in :[2-9][0-9]*|:1[0-9][0-9][0-9]*) ;; *) echo "owned test display required" >&2; exit 64 ;; esac
if [ "${XDG_RUNTIME_DIR:-}" != "$native_out/runtime" ] || [ "${GDK_BACKEND:-}" != x11 ]; then
    echo "owned test runtime required" >&2; exit 64
fi
command -v xdotool >/dev/null || { echo "keyboard capture requires xdotool in the native test environment" >&2; exit 2; }
xdotool version >"$native_out/keyboard-tools.txt"
sha256sum "$native_executable" >"$native_out/binary.sha256"
"$native_executable" >"$native_out/app.log" 2>&1 &
native_app_pid=$!
cleanup() {
    kill "$native_app_pid" 2>/dev/null || true
    wait "$native_app_pid" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
native_end=$((SECONDS + 10))
native_window=""
while [ -z "$native_window" ]; do
    if ! kill -0 "$native_app_pid" 2>/dev/null || [ "$SECONDS" -ge "$native_end" ]; then
        echo "native application did not map; see $native_out/app.log" >&2; exit 1
    fi
    native_window=$(xdotool search --all --onlyvisible --pid "$native_app_pid" --name 'Team — Crew' 2>/dev/null | head -n 1 || true)
    sleep 0.05
done
xdotool windowfocus --sync "$native_window"
xdotool windowsize "$native_window" 1024 600
sleep 0.2
capture() {
    xwininfo -id "$native_window" >"$native_out/$1-window.txt"
    magick import -display "$DISPLAY" -window "$native_window" "$native_out/$1.png"
}
check_title() {
    local native_title=$1 native_deadline=$((SECONDS + 3))
    while [ "$(xdotool getwindowname "$native_window")" != "$native_title — Crew" ]; do
        if [ "$SECONDS" -ge "$native_deadline" ]; then echo "keyboard destination failed: $native_title" >&2; exit 1; fi
        sleep 0.05
    done
}
native_index=1
for native_destination in Team Tasks Messages Events Usage; do
    xdotool key --clearmodifiers "ctrl+$native_index"
    check_title "$native_destination"
    sleep 0.1
    capture "$native_index-$native_destination-light"
    printf 'Ctrl+%s selected %s\n' "$native_index" "$native_destination" >>"$native_out/keyboard.txt"
    native_index=$((native_index + 1))
done
# Exercise the same real local GAction as the native appearance menu. No domain
# action, test-only app API, or daemon connection is introduced for captures.
gdbus call --session --dest dev.crew.Crew --object-path /dev/crew/Crew/window/1 \
    --method org.gtk.Actions.Activate appearance "[<'dark'>]" '{}' >"$native_out/appearance-action.txt"
sleep 0.2
capture 6-Usage-dark
xdotool key --clearmodifiers ctrl+2 ctrl+i
check_title Tasks
sleep 0.2
capture 7-Tasks-wide-details
xdotool windowsize "$native_window" 560 600
sleep 0.2
capture 8-Tasks-narrow-details
xdotool key --clearmodifiers alt+Left
sleep 0.1
capture 9-Tasks-narrow-main
xdotool key --clearmodifiers ctrl+b
sleep 0.1
capture 10-narrow-sidebar
xdotool key --clearmodifiers ctrl+3
check_title Messages
sleep 0.1
capture 11-Messages-narrow
xdotool key --clearmodifiers F1
sleep 0.2
native_help=$(xdotool search --all --onlyvisible --pid "$native_app_pid" --name 'Keyboard Shortcuts' | head -n 1)
magick import -display "$DISPLAY" -window "$native_help" "$native_out/12-shortcuts.png"
xdotool windowfocus --sync "$native_help"
xdotool key --clearmodifiers Escape
sleep 0.1
if xdotool search --all --onlyvisible --pid "$native_app_pid" --name 'Keyboard Shortcuts' >/dev/null 2>&1; then
    echo "Escape did not dismiss shortcut help" >&2; exit 1
fi
sleep 0.1
xdotool windowfocus --sync "$native_window"
xdotool key --clearmodifiers ctrl+q
native_end=$((SECONDS + 3))
while kill -0 "$native_app_pid" 2>/dev/null; do
    if [ "$SECONDS" -ge "$native_end" ]; then echo "local Quit did not end client" >&2; exit 1; fi
    sleep 0.05
done
wait "$native_app_pid"
printf 'Ctrl+I/Alt+Left detail/back; Ctrl+B/sidebar; F1/Escape help; Ctrl+Q quit\n' >>"$native_out/keyboard.txt"
echo "native keyboard navigation passed; captures $native_out"
