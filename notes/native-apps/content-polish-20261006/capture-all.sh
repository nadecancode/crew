#!/usr/bin/env bash
# capture-all.sh PREFIX: one appearance's capture set, run inside x.sh after
# the app has started disconnected. Coordinates are for the 1120x720 window.
set -euo pipefail
p=$1
d() { python3 drive.py "$@" >/dev/null; }
w() { sleep "$1"; }
./resize.sh 1120 720 >/dev/null
if [ "$p" = dark ]; then
  d click 'Application menu'; w 1; ./click.sh 688 160; w 1; ./click.sh 664 166; w 1
fi
./shot.sh "$p-01-disconnected"
d click 'Choose workspace or view connection details'; w 1
./shot.sh "$p-02-connections"
d click-in 'Connect to Crew' 'Connect'; w 5
d tab List; w 1
./shot.sh "$p-03-team"
./click.sh 400 470; w 2
./shot.sh "$p-04-agent-inspector"
d click 'Agent actions'; w 1
./shot.sh "$p-05-agent-actions"
xdotool key Escape; w 1
d tab Transcript; w 2
./shot.sh "$p-06-transcript-tab"
./click.sh 400 550; w 2
d tab Overview; w 1
./shot.sh "$p-07-second-agent"
./click.sh 75 147; w 2; d tab List; w 1
./shot.sh "$p-08-tasks"
./click.sh 400 260; w 2
./shot.sh "$p-09-task-inspector"
./click.sh 90 209; w 2
./shot.sh "$p-10-messages"
./click.sh 80 271; w 2
./shot.sh "$p-11-events"
./click.sh 78 333; w 2
./shot.sh "$p-12-usage"
./click.sh 75 85; w 2
d click 'Answer'; w 2
./shot.sh "$p-13-question-before-input"
xdotool type --delay 30 'Hold it'; xdotool key ctrl+a BackSpace; w 1
./shot.sh "$p-14-question-after-input"
xdotool key Escape; w 1
d click 'Stop team…'; w 2
./shot.sh "$p-15-stop-confirm"
xdotool key Escape; w 1
./click.sh 400 470; w 2
./resize.sh 560 600 >/dev/null; w 2
./shot.sh "$p-16-narrow-560-detail"
d click 'Back to previous pane'; w 1
./shot.sh "$p-17-narrow-560-team"
./resize.sh 1120 720 >/dev/null; w 1
