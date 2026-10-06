#!/usr/bin/env bash
# Scripted acceptance of the real Linux app against an isolated scripted team.
# Drives the actual GTK binary through AT-SPI and X11 keyboard input on a private
# display, accessibility bus, session bus and Crew daemon. It never uses the
# operator's display, daemon, gateway listener, credentials or team.
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../.." && pwd)
source "$native_repo/scripts/native/common.sh"

usage() {
    cat >&2 <<'USAGE'
usage: scripts/native/linux-acceptance.sh [--out DIR] [--suite local|gateway]
         [--cases ID,ID...] [--scale 1|2] [--explore SECONDS] -- APP CREW

APP   crew-desktop built with native-ui,local (gateway suite: its gateway build)
CREW  crew CLI for the private daemon (gateway suite: built with native-gateway)
USAGE
    exit 64
}

native_out=""
native_suite=local
native_cases=""
native_scale=1
native_explore=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --out) [ "$#" -ge 2 ] || usage; native_out=$2; shift 2 ;;
        --suite) [ "$#" -ge 2 ] || usage; native_suite=$2; shift 2 ;;
        --cases) [ "$#" -ge 2 ] || usage; native_cases=$2; shift 2 ;;
        --scale) [ "$#" -ge 2 ] || usage; native_scale=$2; shift 2 ;;
        --explore) [ "$#" -ge 2 ] || usage; native_explore=$2; shift 2 ;;
        --) shift; break ;;
        *) usage ;;
    esac
done
[ "$#" -eq 2 ] || usage
case "$native_suite" in local|gateway) ;; *) usage ;; esac
case "$native_scale" in 1|2) ;; *) echo "--scale takes 1 or 2" >&2; exit 64 ;; esac
[[ "$native_explore" =~ ^[0-9]+$ ]] || { echo "--explore takes seconds" >&2; exit 64; }
[[ "$native_cases" =~ ^[A-Z0-9,]*$ ]] || { echo "--cases takes case IDs such as L01,L05" >&2; exit 64; }

native_app=$(realpath -e "$1") || { echo "unavailable: app $1" >&2; exit 2; }
native_crew=$(realpath -e "$2") || { echo "unavailable: crew CLI $2" >&2; exit 2; }
for native_binary in "$native_app" "$native_crew"; do
    [ -x "$native_binary" ] || { echo "not executable: $native_binary" >&2; exit 2; }
done
native_new_output "$native_out" "acceptance-$native_suite"
native_out=$NATIVE_RUN_DIR
umask 077
mkdir -p "$native_out"/{state,config,cache,data,home,shims,work,captures,logs}

# The daemon socket path must fit the 108-byte Unix limit, so runtime state uses
# a short private directory. The session removes it; runtime-alias.txt records it.
native_runtime=$(mktemp -d /tmp/crew-acc.XXXXXX)
chmod 700 "$native_runtime"
printf 'runtime=%s\n' "$native_runtime" > "$native_out/runtime-alias.txt"

# The daemon may look for provider CLIs on PATH (for example a usage probe).
# Private stubs answer instead, so no operator provider configuration is read.
for native_provider in claude codex; do
    printf '#!/bin/sh\necho "%s is unavailable in native acceptance" >&2\nexit 127\n' "$native_provider" \
        > "$native_out/shims/$native_provider"
    chmod 700 "$native_out/shims/$native_provider"
done

# Inherited Git, agent, Crew, provider and desktop-session context never reaches
# the fixture. Toolchain, Nix and locale settings stay available.
native_clear_inherited_context
for native_name in "${!CREW_@}" "${!CLAUDE@}" "${!CODEX@}" "${!ANTHROPIC@}" "${!OPENAI@}"; do
    [ -z "$native_name" ] || unset "$native_name"
done
unset AT_SPI_BUS_ADDRESS DBUS_SESSION_BUS_ADDRESS WAYLAND_DISPLAY DISPLAY XAUTHORITY \
    SESSION_MANAGER DESKTOP_SESSION XDG_SESSION_ID XDG_SEAT XDG_VTNR
export XDG_RUNTIME_DIR="$native_runtime" XDG_STATE_HOME="$native_out/state"
export XDG_CONFIG_HOME="$native_out/config" XDG_CACHE_HOME="$native_out/cache"
export XDG_DATA_HOME="$native_out/data"
export CREW_GLOBAL_CONFIG="$native_out/config/crew.toml"
export CLAUDE_CONFIG_DIR="$native_out/home/claude" CODEX_HOME="$native_out/home/codex"
export PATH="$native_out/shims:$PATH"
export NATIVE_ACCEPTANCE_SUITE=$native_suite NATIVE_ACCEPTANCE_CASES=$native_cases
export NATIVE_ACCEPTANCE_SCALE=$native_scale NATIVE_ACCEPTANCE_EXPLORE=$native_explore
export NATIVE_ACCEPTANCE_APP=$native_app NATIVE_ACCEPTANCE_CREW=$native_crew
export NATIVE_ACCEPTANCE_OUT=$native_out NATIVE_ACCEPTANCE_REPO=$native_repo
if [ "$native_suite" = gateway ] && ! "$native_crew" native paths >"$native_out/logs/gateway-paths.txt" 2>&1; then
    echo "unavailable: $native_crew is not built with the native gateway" >&2
    rm -rf --one-file-system "$native_runtime"
    exit 2
fi

native_status=0
"$native_repo/scripts/native/linux-env.sh" --acceptance dbus-run-session -- \
    "$native_repo/scripts/native/acceptance/session.sh" || native_status=$?

# The session removes the runtime directory; a leftover is a cleanup failure.
if [ -e "$native_runtime" ]; then
    echo "runtime directory remained: $native_runtime" >&2
    rm -rf --one-file-system "$native_runtime"
    [ "$native_status" -ne 0 ] || native_status=1
fi
echo "artifacts: $native_out"
exit "$native_status"
