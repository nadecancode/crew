#!/usr/bin/env bash
# Replays the spike of notes/web-multi.md: two real daemons, the hub in front
# of them, and the measurements. Prints PASS at the end.
#
#   notes/multi-spike/run.sh            needs target/debug/crew (or CREW_BIN) and node
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
CREW=${CREW_BIN:-$repo/target/debug/crew}
[ -x "$CREW" ] || { echo "build crew first: scripts/build-env cargo build -p crew (or set CREW_BIN)"; exit 2; }
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$repo/target/multi-spike}
(cd "$here/hub" && "$repo/scripts/build-env" cargo build --release --offline >/dev/null 2>&1) || (cd "$here/hub" && "$repo/scripts/build-env" cargo build --release)
HUB=$CARGO_TARGET_DIR/release/hub

# A short root: Unix socket paths are limited to ~108 bytes.
ROOT=$(mktemp -d "${TMPDIR:-/tmp}/cms.XXXXXX")
export ROOT CREW
export XDG_RUNTIME_DIR=$ROOT/r XDG_STATE_HOME=$ROOT/s XDG_CONFIG_HOME=$ROOT/c CREW_GLOBAL_CONFIG=$ROOT/etc/config.toml
unset CREW_IDENTITY CREW_SOCKET HERDR_ENV HERDR_SOCKET_PATH HERDR_PANE_ID
mkdir -p "$ROOT/r" && chmod 700 "$ROOT/r"

team() { # name agents
    {
        printf 'project: %s\nobjective: multi-workspace spike\nworkspace:\n  path: %s\n  init: true\n  vcs: git\n  sandbox:\n    mode: "off"\nbackend: pty\norchestrator:\n  agent: false\ndefaults:\n  runtime: shell\nagents:\n' "$1" "$ROOT/$1"
        for i in $(seq 1 "$2"); do printf '  - id: w%s\n    role: worker\n    command:\n      - "{crew}"\n      - scripted-agent\n' "$i"; done
    } > "$ROOT/$1.yaml"
}
team a 10
team b 2
pids=()
cleanup() {
    for p in "${pids[@]:-}"; do kill "$p" 2>/dev/null || true; done
    (cd "$ROOT" && "$CREW" stop -C "$ROOT/a" >/dev/null 2>&1; "$CREW" stop -C "$ROOT/b" >/dev/null 2>&1) || true
}
trap cleanup EXIT
(cd "$ROOT" && "$CREW" run a.yaml --no-attach >/dev/null && "$CREW" run b.yaml --no-attach >/dev/null)

export LABEL=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
export CODE=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
TOKEN=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
export SECRET=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
VERIFIER=$(printf %s "$SECRET" | sha256sum | cut -d' ' -f1)
port=$((24000 + RANDOM % 900))
export HUB_PORT=$port HUB_PORT_NOCHECK=$((port + 1))
"$HUB" "$HUB_PORT" "$LABEL" "$CODE" "$TOKEN" "$SECRET" "$VERIFIER" 2>"$ROOT/hub.log" & pids+=($!)
"$HUB" "$HUB_PORT_NOCHECK" "$LABEL" "$CODE" "$TOKEN" "$SECRET" "$VERIFIER" nocheck 2>"$ROOT/hub2.log" & pids+=($!)
sleep 0.5
(cd "$here" && node measure.mjs)
