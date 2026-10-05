#!/usr/bin/env bash
# GTK development tools; --headless also provides the isolated capture tools.
set -euo pipefail
unset CREW_IDENTITY CREW_SOCKET CREW_LAUNCH

headless=false
if [ "${1:-}" = --headless ]; then headless=true; shift; fi
if [ "$#" -eq 0 ]; then
    echo "usage: scripts/native/linux-env.sh [--headless] command [args...]" >&2
    exit 64
fi

packages=(gtk4 vte-gtk4 pkg-config gcc)
required=(cc pkg-config)
if "$headless"; then
    packages+=(xorg-server xwininfo imagemagick dbus at-spi2-core)
    required+=(Xvfb xwininfo magick dbus-run-session dbus-update-activation-environment)
fi
available=true
for tool in "${required[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || available=false
done
if "$available" && pkg-config --exists gtk4 vte-2.91-gtk4; then exec "$@"; fi

if ! command -v nix-shell >/dev/null 2>&1; then
    echo "GTK environment unavailable: install GTK4 development files and ${required[*]}" >&2
    exit 2
fi
# nix shell adds binaries to PATH but does not set GTK's development search paths.
# printf %q preserves literal arguments through nix-shell's Bash --run interface.
native_nix_args=()
if [ -n "${NATIVE_NIXPKGS:-}" ]; then native_nix_args=(-I "nixpkgs=$NATIVE_NIXPKGS"); fi
printf -v native_command '%q ' "$@"
exec nix-shell "${native_nix_args[@]}" -p "${packages[@]}" --run "exec $native_command"
