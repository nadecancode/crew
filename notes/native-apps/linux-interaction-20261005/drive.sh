#!/usr/bin/env bash
# Drive a running private interaction session (run-session.sh) with real key
# input from xdotool on its own display, and record after each step what the
# Crew app reports as focused over its own accessibility bus.
# Usage: drive.sh OUT   (OUT is the directory run-session.sh writes to)
set -euo pipefail
out=$1
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck disable=SC1091
source "$out/session.env"
transcript="$out/atspi-keyboard.txt"
reader() { python3 "$here/atspi.py" "$@"; }
note() { printf '%s\n' "$*" >>"$transcript"; }
record() { reader focus >>"$transcript"; }
focused() { reader focused | tail -n 1; }
key() { xdotool key --clearmodifiers "$@"; sleep 0.5; }
capture() { magick import -window root "$out/$1.png"; }
title() { xdotool getwindowname "$win"; }
wait_for() {
    local end=$((SECONDS + ${2:-20}))
    until reader "${3:-prefix}" "$1"; do
        if [ "$SECONDS" -ge "$end" ]; then note "TIMEOUT waiting for $1"; exit 1; fi
        sleep 0.25
    done
}
# Press Tab until the focused object's name starts with PREFIX.
tab_to_prefix() {
    local prefix=$1 max=${2:-40} n=0
    until [[ "$(focused)" == "$prefix"* ]]; do
        n=$((n + 1))
        if [ "$n" -gt "$max" ]; then note "NOT REACHED: $prefix after $max Tab"; exit 1; fi
        key Tab
    done
}
# Press Tab until NAME has focus; record every stop on the way.
tab_to() {
    local name=$1 max=${2:-40} n=0
    while [ "$(focused)" != "$name" ]; do
        n=$((n + 1))
        if [ "$n" -gt "$max" ]; then note "NOT REACHED: $name after $max Tab"; exit 1; fi
        key Tab
    done
}
walk() {
    local count=$1 step
    for step in $(seq 1 "$count"); do
        key "${2:-Tab}"
        printf '%s: ' "$step" >>"$transcript"
        reader focus | grep -v '^  window' | head -n 1 | sed 's/^  //' >>"$transcript"
    done
    note "Window title after the walk: '$(title)'"
}

: >"$transcript"
note "Linux interaction run (binaries: tool-versions.txt), private Xvfb $DISPLAY,"
note "owned accessibility bus, openbox. Real key input from xdotool."
note "Lines: depth role 'name' states (focused objects of the Crew app only)."
win=$(xdotool search --sync --onlyvisible --name 'Team — Crew' | head -n 1)
xdotool windowactivate --sync "$win" 2>/dev/null || xdotool windowfocus --sync "$win"
sleep 1.5

note ""
note "## G17/G9 Startup, disconnected"
record
capture 01-startup-choose-workspace
note "Return:"
key Return
wait_for "Connect to Crew" 10 has
record
capture 02-connections
reader press "Connect to local workspace"
wait_for "claude-1" 30
sleep 2
note "connected (Connect to local workspace pressed through its accessible action):"
record

note ""
note "## G2 Sidebar is one Tab stop (Ctrl+1, Shift+Tab into the sidebar, then Tab)"
key ctrl+1
record
key shift+Tab
record
walk 6
capture 03-tab-walk

note ""
note "## G2 Arrows move focus, only Enter changes the destination"
key ctrl+1
key shift+Tab
note "Down, Down:"
key Down
key Down
record
note "Window title: '$(title)'"
note "Return:"
key Return
record
note "Window title: '$(title)'"
key ctrl+1

note ""
note "## G9 Tab walk on the Team page (Ctrl+1 first)"
key ctrl+1
record
walk 5

note ""
note "## G3 Stop team, keyboard open (Tab to Stop team, Return, Escape)"
key ctrl+1
tab_to "Stop team"
record
key Return
note "open:"
record
capture 04-stop-team-dialog
key Escape
note "after Escape (0.5 s):"
record
sleep 2
note "after 2.5 s:"
record

note ""
note "## G3 Kill and Restart fresh from the agent inspector (Return, Escape)"
key ctrl+1
tab_to_prefix "claude-1," 30
note "Return on the claude-1 row opens its inspector:"
key Return
record
for name in "Kill" "Restart fresh"; do
    tab_to "$name" 80
    record
    key Return
    note "open:"
    record
    key Escape
    note "after Escape (0.5 s):"
    record
    sleep 2
    note "after 2.5 s:"
    record
done

note ""
note "## G14 New message (keyboard open, Escape)"
key ctrl+1
tab_to "New message"
key Return
note "open:"
record
reader tree "New message" >>"$transcript"
capture 05-new-message
key Escape
note "after Escape:"
record

note ""
note "## G15 Terminal: controls follow the attachment; F6 leaves the terminal"
key ctrl+1
tab_to "Kill" 80
tab_to "Overview" 80
key Right Right Right Right
sleep 0.5
reader tree "Team — Crew" | grep -E "Attach PTY terminal|Detach terminal|Copy selection" >>"$transcript"
tab_to "Attach PTY terminal" 20
key Return
sleep 2
note "after Attach:"
reader tree "Team — Crew" | grep -E "Attach PTY terminal|Detach terminal|Copy selection" >>"$transcript"
tab_to "Agent terminal" 10
record
note "Ctrl+2 typed into the terminal goes to the agent:"
key ctrl+2
note "Window title: '$(title)'"
note "F6:"
key F6
record
capture 06-terminal-after-f6
key Return
sleep 1
note "after Detach (Return on the focused control):"
reader tree "Team — Crew" | grep -E "Attach PTY terminal|Detach terminal|Copy selection" >>"$transcript"

note ""
note "## G15 Shortcut help documents the terminal escape"
key F1
wait_for "Keyboard Shortcuts" 5 has
reader tree "Keyboard Shortcuts" | grep -E "terminal|F6|destination|Enter|Up / Down" >>"$transcript"
capture 07-shortcuts
key Escape
record
note "done"
