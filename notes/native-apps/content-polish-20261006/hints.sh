#!/usr/bin/env bash
# hints.sh: the Crew window's size hints as the window manager sees them.
set -euo pipefail
win=$(xdotool search --onlyvisible --name ' — Crew$' | head -1)
xwininfo -id "$win" -size | sed -n '1,20p'
