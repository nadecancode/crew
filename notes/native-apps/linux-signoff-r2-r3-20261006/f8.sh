#!/usr/bin/env bash
# f8.sh: L1 probe, run inside x.sh on a freshly started app. Connects, opens
# an agent, presses F8 and prints the focused object.
set -euo pipefail
d() { python3 drive.py "$@" >/dev/null; }
./resize.sh 1120 720 >/dev/null
d click 'Choose workspace or view connection details'; sleep 1
d click-in 'Connect to Crew' 'Connect'; sleep 5
d tab List; sleep 1
./click.sh 400 470; sleep 2
xdotool key F8; sleep 1
python3 drive.py focused; python3 focus.py | head -60; python3 drive.py names "Resize"
./shot.sh f8-pane-handle >/dev/null
