#!/usr/bin/env bash
# Sourced by runners. Build/capture output stays in this checkout.
native_new_output() {
    local requested=$1 prefix=$2 base="$native_repo/target/native-validation" part cursor
    for part in "$native_repo/target" "$base"; do
        if [ -L "$part" ]; then echo "artifact directory must not be a symlink: $part" >&2; exit 64; fi
        mkdir -p "$part"
    done
    if [ -z "$requested" ]; then
        NATIVE_RUN_DIR=$(mktemp -d "$base/$prefix-XXXXXX")
        return
    fi
    case "$requested" in /*) ;; *) requested="$native_repo/$requested" ;; esac
    case "$requested" in "$base/"*) ;; *) echo "output must be under $base" >&2; exit 64 ;; esac
    local -a components
    IFS='/' read -r -a components <<< "${requested#"$base/"}"
    cursor=$base
    for part in "${components[@]}"; do
        case "$part" in ''|.|..) echo "output must name a new directory without traversal" >&2; exit 64 ;; esac
        cursor="$cursor/$part"
        if [ -L "$cursor" ]; then echo "output must not traverse a symlink: $cursor" >&2; exit 64; fi
    done
    if [ -e "$cursor" ]; then echo "output must be a new directory: $cursor" >&2; exit 64; fi
    mkdir -p "$cursor"
    NATIVE_RUN_DIR=$cursor
}

native_cargo_target() {
    for part in "$native_repo/target" "$native_repo/target/native-validation"; do
        if [ -L "$part" ]; then echo "cargo target must stay in this checkout: $part" >&2; exit 64; fi
        mkdir -p "$part"
    done
    if [ -L "$native_repo/target/native-validation/cargo" ]; then echo "cargo target must not be a symlink" >&2; exit 64; fi
    export CARGO_TARGET_DIR="$native_repo/target/native-validation/cargo"
    unset CREW_IDENTITY CREW_SOCKET CREW_LAUNCH
}
