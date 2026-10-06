#!/usr/bin/env bash
# Private acceptance session, run by linux-acceptance.sh inside the acceptance
# toolset and a private session bus. Starts an X display, accessibility bus,
# window manager and a scripted Crew team, then runs the driver against the app.
# Cleanup stops only processes and the daemon this session started.
set -euo pipefail
out=$NATIVE_ACCEPTANCE_OUT
repo=$NATIVE_ACCEPTANCE_REPO
crew=$NATIVE_ACCEPTANCE_CREW
app=$NATIVE_ACCEPTANCE_APP
runtime=$XDG_RUNTIME_DIR
ws="$out/work/ws"
logs="$out/logs"

xvfb_pid="" a11y_pid="" wm_pid="" daemon_started=false
# Processes of this run that are not this session's own helpers or ancestors.
# Daemons and agents inherit the private runtime directory; nothing is matched
# by name and no other process is touched.
owned_processes() {
    local skip=" $$ $xvfb_pid $a11y_pid $wm_pid " pid=$$ environ
    while [ "$pid" -gt 1 ]; do
        pid=$(awk '{ print $4 }' "/proc/$pid/stat" 2>/dev/null) || break
        skip="$skip$pid "
    done
    for environ in /proc/[0-9]*/environ; do
        pid=${environ#/proc/}; pid=${pid%/environ}
        case "$skip" in *" $pid "*) continue ;; esac
        # The session bus started by dbus-run-session ends when this session exits.
        [ "$(cat "/proc/$pid/comm" 2>/dev/null)" = dbus-daemon ] && continue
        [ -r "$environ" ] || continue
        if tr '\0' '\n' 2>/dev/null < "$environ" | grep -qxF "XDG_RUNTIME_DIR=$runtime"; then
            printf '%s ' "$pid"
        fi
    done
}
cleanup() {
    local status=$?
    set +e
    {
        echo "status=$status"
        if [ -f "$out/app.pid" ]; then
            local app_pid; app_pid=$(cat "$out/app.pid")
            if kill -0 "$app_pid" 2>/dev/null; then
                kill "$app_pid"; sleep 1; kill -KILL "$app_pid" 2>/dev/null
                echo "app_terminated=$app_pid"
            fi
        fi
        if [ "$NATIVE_ACCEPTANCE_SUITE" = gateway ]; then
            timeout 10 "$crew" native stop >"$logs/native-stop.log" 2>&1
            echo "native_stop=$?"
        fi
        if "$daemon_started"; then
            timeout 30 "$crew" stop -C "$ws" >"$logs/crew-stop.log" 2>&1
            echo "crew_stop=$?"
            sleep 1
        fi
        local leftover pid
        leftover=$(owned_processes)
        if [ -n "$leftover" ]; then
            echo "owned_after_stop=$leftover"
            for pid in $leftover; do
                echo "  $pid $(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | cut -c1-200)"
            done
            kill $leftover 2>/dev/null; sleep 1; kill -KILL $leftover 2>/dev/null; sleep 0.2
        fi
        for pid in "$wm_pid" "$a11y_pid" "$xvfb_pid"; do
            [ -n "$pid" ] && kill "$pid" 2>/dev/null
        done
        for pid in "$wm_pid" "$a11y_pid" "$xvfb_pid"; do
            [ -n "$pid" ] && wait "$pid" 2>/dev/null
        done
        echo "remaining=$(owned_processes)"
        find "$runtime" -mindepth 1 -printf '%P %y\n' 2>/dev/null | sort > "$logs/runtime-contents.txt"
        rm -rf --one-file-system "$runtime"
    } > "$out/cleanup.txt" 2>&1
    if grep -q '^remaining=.' "$out/cleanup.txt"; then
        echo "processes remained after cleanup; see $out/cleanup.txt" >&2
        [ "$status" -ne 0 ] || status=1
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Private X display; a high number avoids a lazy Xwayland server's :0.
display=$((300 + $$ % 10000))
for _ in {1..20}; do
    if [ ! -e "/tmp/.X$display-lock" ] && [ ! -e "/tmp/.X11-unix/X$display" ]; then break; fi
    display=$((display + 1))
done
scale=$NATIVE_ACCEPTANCE_SCALE
screen="$((1440 * scale))x$((960 * scale))x24"
Xvfb ":$display" -screen 0 "$screen" -dpi 96 -nolisten tcp >"$logs/xvfb.log" 2>&1 &
xvfb_pid=$!
lock_pid=""
for _ in {1..100}; do
    if [ -f "/tmp/.X$display-lock" ]; then read -r lock_pid < "/tmp/.X$display-lock"; fi
    [ "$lock_pid" = "$xvfb_pid" ] && break
    kill -0 "$xvfb_pid" 2>/dev/null || break
    sleep 0.1
done
[ "$lock_pid" = "$xvfb_pid" ] || { echo "private display failed; see $logs/xvfb.log" >&2; exit 1; }

export DISPLAY=":$display" GDK_BACKEND=x11 GSK_RENDERER=cairo GDK_SCALE=$scale GDK_DEBUG=no-portals
export GTK_A11Y=atspi NO_AT_BRIDGE=0 PYTHONDONTWRITEBYTECODE=1
# A private accessibility bus: the driver refuses any other address.
export AT_SPI_BUS_ADDRESS="unix:abstract=crew-qa-acceptance-$$-$RANDOM"
atspi_prefix=$(pkg-config --variable=prefix atspi-2)
dbus-daemon --nofork --nopidfile \
    --config-file="$atspi_prefix/share/defaults/at-spi2/accessibility.conf" \
    --address="$AT_SPI_BUS_ADDRESS" >"$logs/a11y-bus.log" 2>&1 &
a11y_pid=$!
dbus-update-activation-environment DISPLAY GDK_BACKEND GSK_RENDERER GDK_DEBUG GTK_A11Y \
    XDG_RUNTIME_DIR AT_SPI_BUS_ADDRESS
openbox --sm-disable >"$logs/window-manager.log" 2>&1 &
wm_pid=$!
sleep 0.3
kill -0 "$wm_pid" 2>/dev/null || { echo "private window manager failed" >&2; exit 1; }

# Animations off in the private GTK configuration; screenshots stay stable.
mkdir -p "$XDG_CONFIG_HOME/gtk-4.0"
printf '[Settings]\ngtk-enable-animations=false\n' > "$XDG_CONFIG_HOME/gtk-4.0/settings.ini"

{
    printf 'app %s\n' "$(sha256sum "$app")"
    printf 'crew %s\n' "$(sha256sum "$crew")"
    "$crew" --version
    printf 'GTK '; pkg-config --modversion gtk4
    printf 'VTE '; pkg-config --modversion vte-2.91-gtk4
    printf 'GLib '; pkg-config --modversion glib-2.0
    printf 'AT-SPI '; pkg-config --modversion atspi-2
    xdotool version
    magick -version | sed -n 1p
    python3 --version
    python3 -c 'import pyatspi; print("pyatspi", getattr(pyatspi, "__version__", "?"))'
    openbox --version | sed -n 1p
    printf 'display=%s screen=%s dpi=96 GDK_SCALE=%s GDK_BACKEND=x11 GSK_RENDERER=cairo\n' "$DISPLAY" "$screen" "$scale"
    printf 'animations=false window_manager=openbox\n'
    printf 'nixpkgs=%s\n' "${NATIVE_NIXPKGS:-<channel>}"
} > "$out/tool-versions.txt" 2>&1

for key in DISPLAY DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS PATH LD_LIBRARY_PATH PYTHONPATH \
    GI_TYPELIB_PATH XDG_DATA_DIRS XDG_RUNTIME_DIR XDG_STATE_HOME XDG_CONFIG_HOME XDG_CACHE_HOME \
    XDG_DATA_HOME CREW_GLOBAL_CONFIG CLAUDE_CONFIG_DIR CODEX_HOME GDK_BACKEND GSK_RENDERER GDK_DEBUG \
    GTK_A11Y GDK_PIXBUF_MODULE_FILE; do
    printf 'export %s=%q\n' "$key" "${!key-}"
done > "$out/session.env"

# Remote profiles keep their device key in the Secret Service: a private GNOME
# Keyring on this session bus, unlocked with a random password, with its files
# in the run's data home. The operator's keyring is never reachable.
if [ "$NATIVE_ACCEPTANCE_SUITE" = gateway ]; then
    head -c 18 /dev/urandom | base64 | tr -d '/+=' > "$out/home/keyring-password"
    gnome-keyring-daemon --unlock --components=secrets --foreground \
        < "$out/home/keyring-password" >"$logs/keyring.log" 2>&1 &
    for _ in {1..100}; do
        if dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
            org.freedesktop.DBus.NameHasOwner string:org.freedesktop.secrets 2>/dev/null | grep -q 'boolean true'; then
            break
        fi
        sleep 0.1
    done
    printf 'keyring %s\n' "$(gnome-keyring-daemon --version 2>&1 | sed -n 1p)" >> "$out/tool-versions.txt"
fi

# The scripted team. Every agent is a real `crew scripted-agent` process on a
# daemon-owned PTY; nothing calls a model provider.
mkdir -p "$ws"
agent() {
    local id=$1 role=$2; shift 2
    printf '  - id: %s\n    role: %s\n    command:\n      - "{crew}"\n      - scripted-agent\n' "$id" "$role"
    local step
    for step in "$@"; do printf '      - %s\n' "$(printf '%s' "$step" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')"; done
}
{
    printf 'project: native-acceptance\nobjective: Native Linux acceptance fixture\n'
    printf 'workspace:\n  path: %s\n  init: true\n  vcs: git\n  sandbox:\n    mode: off\n' \
        "$(printf '%s' "$ws" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')"
    printf 'backend: pty\norchestrator:\n  agent: false\ndefaults:\n  runtime: shell\nagents:\n'
    agent dev builder "--on-task=progress 35 Building the first part"
    agent echo echo "--on-message=reply ack from echo"
    agent asker asker "--on-start=send operator question Which deployment target should we use?"
    agent reviewer reviewer "--on-task=status blocked Waiting on the schema decision"
    agent idle idle
} > "$out/work/team.yaml"

cd "$out/work"
"$crew" run team.yaml --no-attach >"$logs/crew-run.log" 2>&1
daemon_started=true
for _ in {1..400}; do
    if "$crew" status --json -C "$ws" >"$logs/status-start.json" 2>/dev/null &&
        python3 - "$logs/status-start.json" <<'PY'
import json, sys
snapshot = json.load(open(sys.argv[1]))
agents = snapshot.get("agents", [])
sys.exit(0 if len(agents) == 5 and all(a.get("status") != "spawning" for a in agents) else 1)
PY
    then break; fi
    sleep 0.1
done
daemon_pid=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["session"]["daemon_pid"])' "$logs/status-start.json")
printf 'daemon_pid=%s\nworkspace=%s\n' "$daemon_pid" "$ws" > "$out/daemon.txt"

# Seed the board: dev executes one task with a second one waiting behind it, a
# dependent task stays unresolved, and the reviewer reports Blocked work.
"$crew" task add -C "$ws" --id schema --title "Design the schema" \
    --description "Define tables for the acceptance fixture." --agent dev >"$logs/task-schema.log"
"$crew" task add -C "$ws" --id docs --title "Write the operator notes" \
    --description "A second card queued behind the schema." --agent dev >"$logs/task-docs.log"
"$crew" task add -C "$ws" --id api --title "Build the API" --depends-on schema \
    --description "Depends on the schema." >"$logs/task-api.log"
"$crew" task add -C "$ws" --id release --title "Review the release" \
    --description "Reviewer reports Blocked." --agent reviewer >"$logs/task-release.log"
for _ in {1..300}; do
    "$crew" status --json -C "$ws" >"$logs/status-seeded.json" 2>/dev/null &&
        python3 - "$logs/status-seeded.json" <<'PY' && break
import json, sys
s = json.load(open(sys.argv[1]))
tasks = {t["id"]: t for t in s.get("tasks", [])}
ok = tasks.get("schema", {}).get("status") == "working" and tasks.get("release", {}).get("status") in ("blocked", "working")
sys.exit(0 if ok else 1)
PY
    sleep 0.1
done

python3 "$repo/scripts/native/acceptance/driver.py"
