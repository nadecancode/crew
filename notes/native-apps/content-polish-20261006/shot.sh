#!/usr/bin/env bash
# shot.sh NAME: capture the Crew window (inside x.sh).
set -euo pipefail
win=$(xdotool search --onlyvisible --name ' — Crew$' | head -1)
eval "$(xdotool getwindowgeometry --shell "$win")"
magick import -window root -crop "${WIDTH}x${HEIGHT}+${X}+${Y}" +repage "$CAPTURES/$1.png"
sha256sum "$CAPTURES/$1.png"
