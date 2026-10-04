#!/usr/bin/env bash
# Replays the measurements and checks behind notes/web-design.md.
#
# Builds crew, the relay and the prototype; starts the scripted dry-run team
# (no model, no tokens) with its own runtime and state directories; serves the
# prototype through the relay; runs the unit tests and the browser checks, and
# prints the report §perf quotes. Prints PASS at the end.
#
# Needs: cargo (through scripts/build-env), bun, and a Chromium: Playwright's
# own (`bunx playwright install chromium`), or any Chromium-based browser named
# by PLAYWRIGHT_EXECUTABLE_PATH (NixOS).
#
#   notes/web-spike/run.sh            # everything
#   KEEP=1 notes/web-spike/run.sh     # leave the team and the relay running
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
# A short path: the daemon's socket lives under it (sun_path is ~108 bytes).
work=${CREW_WEB_SPIKE_DIR:-$(mktemp -d /tmp/crew-ws.XXXXXX)}
port=${PORT:-47615}
export XDG_RUNTIME_DIR=$work/r XDG_STATE_HOME=$work/s
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$work/target}
unset CREW_IDENTITY
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"

echo "== build (crew, relay, bundle)"
(cd "$root" && ./scripts/build-env cargo build -q -p crew)
(cd "$here/relay" && "$root/scripts/build-env" cargo build -q --release)
crew=$CARGO_TARGET_DIR/debug/crew
relay=$CARGO_TARGET_DIR/release/relay
cd "$here/web"
bun install --frozen-lockfile >/dev/null
bun run build
first=$(cd dist && find . -type f | sort | xargs sha256sum)
rm -rf dist && bun run build >/dev/null
[ "$first" = "$(cd dist && find . -type f | sort | xargs sha256sum)" ] || { echo "the bundle is not reproducible" >&2; exit 1; }
echo "bundle: two builds, identical bytes"

echo "== unit tests (vitest)"
bunx vitest run

echo "== scripted team"
cp "$root/examples/dry-run.yaml" "$work/"
(cd "$work" && "$crew" run dry-run.yaml --no-attach)
ws=$work/demo-workspace
pids=()
stop() {
    [ -n "${KEEP:-}" ] && { echo "left running: crew stop -C $ws; kill ${pids[*]}"; return; }
    kill "${pids[@]}" 2>/dev/null || true
    "$crew" stop -C "$ws" >/dev/null 2>&1 || true
}
trap stop EXIT
sock=$(ls "$XDG_RUNTIME_DIR"/crew/*.sock)
# Wait for the script to play out (six tasks), so every run measures the same state.
for _ in $(seq 60); do
    # Captured first: `crew status` panics when its reader closes the pipe early.
    status=$("$crew" status -C "$ws")
    grep -q '^tasks *6/6 done' <<<"$status" && break
    sleep 0.5
done
"$relay" "$sock" "$here/web/dist" "$port" spiketoken & pids+=($!)
"$relay" "$sock" "$here/web/dist" "$((port + 1))" spiketoken strict & pids+=($!)
sleep 0.5

echo "== HTTP checks"
get() { curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$@"; }
up=(-H Upgrade:websocket -H Connection:Upgrade -H Sec-WebSocket-Version:13 -H Sec-WebSocket-Key:dGhlIHNhbXBsZSBub25jZQ==)
check() { [ "$2" = "$3" ] && echo "ok   $1: $2" || { echo "FAIL $1: got $2, want $3" >&2; exit 1; }; }
check "index"                         "$(get "http://127.0.0.1:$port/")" 200
check "foreign Host (DNS rebinding)"  "$(get -H "Host: evil.example:$port" "http://127.0.0.1:$port/")" 421
check "POST"                          "$(get -X POST "http://127.0.0.1:$port/")" 405
check "upgrade without Origin"        "$(get "${up[@]}" "http://127.0.0.1:$port/ws")" 403
check "upgrade from another site"     "$(get "${up[@]}" -H "Origin: http://evil.example" "http://127.0.0.1:$port/ws")" 403
check "upgrade from another local port" "$(get "${up[@]}" -H "Origin: http://127.0.0.1:5173" "http://127.0.0.1:$port/ws")" 403
# A listener made by tokio has no SO_REUSEPORT: nobody else can share its port.
rebind=$(PORT=$port bun -e 'try { Bun.listen({ hostname: "127.0.0.1", port: Number(process.env.PORT), reusePort: true, socket: { data() {} } }); console.log("bound"); process.exit(0) } catch (e) { console.log(e.code) }')
check "second bind of the port with SO_REUSEPORT" "$rebind" EADDRINUSE

echo "== browser checks (Playwright)"
PORT=$port CREW=$crew WS=$ws bunx playwright test

echo PASS
