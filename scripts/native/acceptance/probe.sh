#!/usr/bin/env bash
# Inspect or drive a running acceptance session (one started with --explore).
# usage: scripts/native/acceptance/probe.sh RUN_DIR tree|act|actions|key|type|capture|resize ARGS...
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../../.." && pwd)
[ "$#" -ge 2 ] || { echo "usage: probe.sh RUN_DIR COMMAND ARGS..." >&2; exit 64; }
run=$(realpath -e "$1"); shift
case "$run" in "$native_repo/target/native-validation/"*) ;; *) echo "run directory must be under target/native-validation" >&2; exit 64 ;; esac
[ -f "$run/session.env" ] || { echo "no session.env in $run" >&2; exit 2; }
exec "$native_repo/scripts/native/linux-env.sh" --acceptance bash -c \
    'source "$1/session.env"; export PYTHONDONTWRITEBYTECODE=1 NATIVE_ACCEPTANCE_OUT=$1 NATIVE_ACCEPTANCE_APP=probe NATIVE_ACCEPTANCE_CREW=probe; shift; exec python3 "$0" probe "$@"' \
    "$native_repo/scripts/native/acceptance/driver.py" "$run" "$@"
