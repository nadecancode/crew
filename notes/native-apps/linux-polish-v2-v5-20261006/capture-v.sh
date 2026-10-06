#!/usr/bin/env bash
# capture-v.sh PREFIX: the layout follow-up captures (V2-V5, V8) for one
# appearance, run inside x.sh after the app has started disconnected.
# Coordinates are for the 1120x720 window.
set -euo pipefail
p=$1
d() { python3 drive.py "$@" >/dev/null; }
w() { sleep "$1"; }
./resize.sh 1120 720 >/dev/null
if [ "$p" = dark ]; then
  d click 'Application menu'; w 1; ./click.sh 688 160; w 1; ./click.sh 664 166; w 1
fi
d click 'Choose workspace or view connection details'; w 1
d click-in 'Connect to Crew' 'Connect'; w 5
d tab List; w 1
./shot.sh "$p-01-team"
./click.sh 400 470; w 2
./shot.sh "$p-02-agent-inspector"
python3 drive.py names-role 'page tab' >"$CAPTURES/$p-tabs-1120.txt"
d click 'More sections'; w 1
./shot.sh "$p-03-more-menu"
d click 'Host screen'; w 2
./shot.sh "$p-04-host-screen-chosen"
python3 drive.py focused >"$CAPTURES/$p-focus-after-more.txt"
d click 'Attach terminal'; w 4
./shot.sh "$p-05-terminal-attached"
d click 'Detach terminal'; w 2
d tab Overview; w 1
./click.sh 75 147; w 2; d tab List; w 1
./click.sh 400 260; w 2
./shot.sh "$p-06-task-inspector"
./click.sh 75 85; w 2
./click.sh 400 470; w 2
./resize.sh 560 600 >/dev/null; w 2
./shot.sh "$p-07-narrow-560-detail"
python3 drive.py names-role 'page tab' >"$CAPTURES/$p-tabs-560.txt"
d click 'Back to previous pane'; w 1
./resize.sh 1120 720 >/dev/null; w 1
