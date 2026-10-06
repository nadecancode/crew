#!/usr/bin/env bash
# resize.sh WIDTH HEIGHT [TRIGGER]: resize the Crew window, then optionally touch a measurement trigger.
set -euo pipefail
win=$(xdotool search --onlyvisible --name ' — Crew$' | head -1)
xdotool windowsize "$win" "$1" "$2"
sleep 1
xdotool getwindowgeometry "$win"
if [ -n "${3:-}" ]; then touch "$3"; sleep 1.5; fi
