#!/usr/bin/env bash
# GTK development/runtime tools; --headless adds isolated capture tools and
# --acceptance adds the AT-SPI, keyboard and Python tools of the app acceptance run.
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

headless=false
acceptance=false
case "${1:-}" in
    --headless) headless=true; shift ;;
    --acceptance) headless=true; acceptance=true; shift ;;
esac
if [ "$#" -eq 0 ]; then
    echo "usage: scripts/native/linux-env.sh [--headless|--acceptance] command [args...]" >&2
    exit 64
fi

packages=(gtk4 vte-gtk4 pkg-config gcc)
required=(cc pkg-config)
if "$headless"; then
    packages+=(xorg-server xwininfo imagemagick dbus at-spi2-core)
    required+=(Xvfb xwininfo magick dbus-run-session dbus-update-activation-environment)
fi
if "$acceptance"; then
    packages+=(xdotool openbox gobject-introspection adwaita-icon-theme gnome-keyring
        "(python3.withPackages (ps: [ ps.pyatspi ps.pygobject3 ]))")
    required+=(xdotool openbox dbus-daemon python3 gnome-keyring-daemon)
fi
available=true
for tool in "${required[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || available=false
done
if "$acceptance" && "$available" && ! python3 -c "import pyatspi" >/dev/null 2>&1; then available=false; fi
if "$available" && pkg-config --exists gtk4 vte-2.91-gtk4; then
    # Rust's linked executable may have no Nix store rpath. Use this selected
    # toolkit's full public link closure, without assuming host library paths.
    # read handles pkg-config's backslash-escaped spaces without evaluating it.
    read -a native_library_flags <<< "$(pkg-config --libs-only-L gtk4 vte-2.91-gtk4)"
    native_library_path=""
    for native_flag in "${native_library_flags[@]}"; do
        case "$native_flag" in
            -L?*) native_libdir=${native_flag#-L} ;;
            *) echo "invalid GTK runtime library flag: $native_flag" >&2; exit 2 ;;
        esac
        case "$native_libdir" in
            *:*) echo "GTK runtime library directory contains a path separator: $native_libdir" >&2; exit 2 ;;
        esac
        if [ ! -d "$native_libdir" ]; then
            echo "GTK runtime library directory unavailable: $native_libdir" >&2; exit 2
        fi
        native_library_path+="${native_library_path:+:}$native_libdir"
    done
    if [ -n "$native_library_path" ]; then
        export LD_LIBRARY_PATH="$native_library_path${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    fi
    unset NATIVE_ENV_REENTERED
    exec "$@"
fi

if [ -n "${NATIVE_ENV_REENTERED:-}" ]; then
    echo "GTK environment unavailable after entering nix-shell: ${required[*]}" >&2
    exit 2
fi
if ! command -v nix-shell >/dev/null 2>&1; then
    echo "GTK environment unavailable: install GTK4 development files and ${required[*]}" >&2
    exit 2
fi
# nix shell adds binaries to PATH but does not set GTK's development search paths.
# printf %q preserves literal arguments through nix-shell's Bash --run interface.
native_nix_args=()
if [ -n "${NATIVE_NIXPKGS:-}" ]; then native_nix_args=(-I "nixpkgs=$NATIVE_NIXPKGS"); fi
native_launcher="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/linux-env.sh"
native_reentry=("$native_launcher")
if "$acceptance"; then native_reentry+=(--acceptance)
elif "$headless"; then native_reentry+=(--headless); fi
native_reentry+=("$@")
# Re-enter with pkg-config now available, before executing the requested command.
printf -v native_command '%q ' "${native_reentry[@]}"
NATIVE_ENV_REENTERED=1 exec nix-shell "${native_nix_args[@]}" -p "${packages[@]}" --run "exec $native_command"
