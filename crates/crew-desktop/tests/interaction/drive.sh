#!/usr/bin/env bash
# Drive a running private interaction session (session.sh) with real key input
# from xdotool on its own display. After each step it reads what the Crew app
# reports as focused over its own accessibility bus, records it in the
# transcript and checks it. Any failed check ends the run with exit status 1.
# Usage: drive.sh OUT   (OUT is the directory session.sh writes to)
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
fail() {
    note "FAIL: $*"
    echo "interaction check failed: $*; transcript $transcript" >&2
    exit 1
}
# Wait up to three seconds for NAME (an extended regular expression matched
# against the whole name) to have focus, then record it.
expect_focus() {
    local end=$((SECONDS + 3))
    until [[ "$(focused)" =~ ^($1)$ ]]; do
        if [ "$SECONDS" -ge "$end" ]; then
            record
            fail "expected focus on '$1', found '$(focused)'"
        fi
        sleep 0.2
    done
    record
}
expect_title() {
    [ "$(title)" = "$1 — Crew" ] || fail "expected destination $1, window title '$(title)'"
    note "Window title: '$(title)'"
}
wait_for() {
    local end=$((SECONDS + ${2:-20}))
    until reader "${3:-prefix}" "$1"; do
        [ "$SECONDS" -lt "$end" ] || fail "timed out waiting for $1"
        sleep 0.25
    done
}
# Press KEY until the focused object's name matches NAME (an extended regular
# expression for the start of the name), at most MAX times.
press_to() {
    local press=$1 name=$2 max=${3:-40} n=0 seen=()
    until [[ "$(focused)" =~ ^($name) ]]; do
        n=$((n + 1))
        seen+=("$(focused)")
        [ "$n" -le "$max" ] || fail "'$name' not reached after $max $press; stops: $(printf '[%s] ' "${seen[@]}")"
        key "$press"
    done
}
# Press Tab COUNT times on the current destination: every stop must be a
# control (never a label or a table cell) and the destination must not change.
walk() {
    local count=$1 step line
    for step in $(seq 1 "$count"); do
        key Tab
        line=$(reader focus | grep -v '^  window' | head -n 1 | sed 's/^  //')
        printf '%s: %s\n' "$step" "$line" >>"$transcript"
        case "$line" in
            *" label "*|*" table cell "*|*"(no focused descendant)"*) fail "Tab stop $step is not a control: $line" ;;
        esac
    done
    expect_title "$2"
}
controls() {
    reader tree "Team — Crew" | grep -E "'(Attach terminal|Detach terminal|Copy selection)'" >>"$transcript" || true
}

: >"$transcript"
note "Linux interaction run (binaries: tool-versions.txt), private Xvfb $DISPLAY,"
note "owned accessibility bus, openbox. Real key input from xdotool."
note "Lines: depth role 'name' states (focused objects of the Crew app only)."
win=$(xdotool search --sync --onlyvisible --name 'Team — Crew' | head -n 1)
xdotool windowactivate --sync "$win" 2>/dev/null || xdotool windowfocus --sync "$win"
sleep 1.5

note ""
note "## G17, G9: startup, disconnected, focus on the page's first control"
expect_focus "Choose workspace"
capture 01-startup-choose-workspace
note "Return opens Connections:"
key Return
wait_for "Connect to Crew" 10 has
expect_focus "Connect.*"
capture 02-connections
key Return
wait_for "claude-1" 30
sleep 2
# Whether the window manager has already handed activation back from the
# closed dialog is a race; focus is read from the active window.
xdotool windowactivate --sync "$win" 2>/dev/null || xdotool windowfocus --sync "$win"
note "connected, focus on the connected page's first control:"
expect_focus "Answer|Refresh"

note ""
note "## G2: the sidebar is one Tab stop"
key ctrl+1
press_to shift+Tab "Team$" 20
expect_focus "Team"
walk 4 Team

note ""
note "## G2: arrows move focus, only Enter changes the destination"
key ctrl+1
press_to shift+Tab "Team$" 20
key Down
key Down
expect_focus "Messages"
expect_title Team
key Return
expect_focus "Messages"
expect_title Messages

note ""
note "## G9: Tab on the Team page visits each control once"
key ctrl+1
press_to Tab "Refresh$" 20
expect_focus "Refresh"
walk 4 Team
capture 03-team-tab-walk

note ""
note "## G3: Stop team… opened from the keyboard; Escape returns focus to it"
key ctrl+1
press_to Tab "Stop team…" 20
key Return
expect_focus "Cancel"
sleep 1
note "the opener while the confirmation is open:"
reader tree "Team — Crew" | grep -E "button 'Stop team…'" >>"$transcript" || true
if reader tree "Team — Crew" | grep -qi "already pending"; then
    fail "the page shows the pending reason while the confirmation is open"
fi
note "no pending reason is shown while it is open"
capture 04-stop-team-dialog
key Escape
expect_focus "Stop team…"
sleep 2
note "2 s later:"
expect_focus "Stop team…"

note ""
note "## G3: Kill… and Restart fresh… from the Actions menu return focus to it"
key ctrl+1
press_to Tab "List$" 20
key Return
press_to Tab "claude-1, " 30
key Return
for name in "Kill…" "Restart fresh…"; do
    press_to Tab "Agent actions$" 60
    key Return
    press_to Tab "$name" 20
    key Return
    expect_focus "Cancel"
    capture "04-$(printf %s "${name%…}" | tr " " "-" | tr "[:upper:]" "[:lower:]")-dialog"
    key Escape
    expect_focus "Agent actions"
    sleep 2
    note "2 s later:"
    expect_focus "Agent actions"
    key Escape
done

note ""
note "## G14: New message names its To selector and editor"
key ctrl+1
press_to Tab "New message" 60
key Return
expect_focus "Message"
reader tree "New message" >>"$transcript"
reader tree "New message" | grep -q "combo box 'To'" || fail "no To selector"
reader tree "New message" | grep -q "label 'To'" || fail "no visible To label"
capture 05-new-message
key Escape
expect_focus "New message"

note ""
note "## G15: terminal controls follow the attachment; F6 leaves the terminal"
note "(the claude-1 inspector is still open)"
controls
press_to Tab "Attach terminal" 60
key Return
sleep 2
note "after Attach, focus is on the terminal, which says how to leave it:"
expect_focus "Agent terminal"
reader focus | grep -q "F6 leaves the terminal" || fail "the attached terminal does not announce F6"
capture 06-terminal-attached
controls
note "accessible text of the attached terminal before any new output:"
reader text "Agent terminal" >>"$transcript" || fail "terminal has no accessible text"
reader text "Agent terminal" | grep -q "^  0 characters" && fail "terminal's first screen is not accessible"
press_to Tab "Agent terminal" 40
note "Tab typed into the terminal goes to the agent:"
key Tab
expect_focus "Agent terminal"
note "F6:"
key F6
expect_focus "Detach terminal"
capture 07-terminal-after-f6
note "Return on Detach, focus back on Attach terminal:"
key Return
expect_focus "Attach terminal"
capture 08-after-detach
controls
note "R1: 2 s later the window status does not say an action is pending:"
sleep 2
reader tree "Team — Crew" | grep -E "label '(Action|Detached)" >>"$transcript" || true
if reader tree "Team — Crew" | grep -q "Action pending"; then
    fail "status still says an action is pending after Detach"
fi
capture 08b-status-after-detach

note ""
note "## G15: shortcut help documents the terminal escape"
key F1
wait_for "Keyboard Shortcuts" 5 has
reader tree "Keyboard Shortcuts" | grep -E "terminal|F6|destination|Enter|Up / Down" >>"$transcript"
reader tree "Keyboard Shortcuts" | grep -q "label 'F6'" || fail "help does not name F6"
capture 09-shortcuts
key Escape
note "PASS"
