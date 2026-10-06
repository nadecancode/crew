#!/usr/bin/env bash
# click.sh X Y: pointer click at window-relative coordinates of the Crew window.
set -euo pipefail
win=$(xdotool search --onlyvisible --name ' — Crew$' | head -1)
xdotool mousemove --window "$win" "$1" "$2" click 1
sleep 0.5
