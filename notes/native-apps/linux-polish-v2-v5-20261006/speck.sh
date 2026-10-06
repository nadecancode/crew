#!/usr/bin/env bash
# speck.sh: V2 probe, run inside x.sh on a freshly started app. Connects, then
# captures the sidebar status before and after a full window repaint.
set -euo pipefail
d() { python3 drive.py "$@" >/dev/null; }
./resize.sh 1120 720 >/dev/null
./shot.sh speck-0-disconnected
d click 'Choose workspace or view connection details'; sleep 1
d click-in 'Connect to Crew' 'Connect'; sleep 5
./shot.sh speck-1-connected
./resize.sh 1124 720 >/dev/null; ./resize.sh 1120 720 >/dev/null
./shot.sh speck-2-after-repaint
