#!/usr/bin/env bash
# start.sh OUT ROOT DESKTOP CREW: private session in the background (see run-session.sh).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
exec nix-shell "$here/shell.nix" --run "exec dbus-run-session -- $repo/scripts/native/linux-env.sh --headless $here/run-session.sh $1 $2 $3 $4 $here/team.yaml.in"
