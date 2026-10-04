set shell := ["bash", "-euo", "pipefail", "-c"]

# Show the available commands.
default:
    @just --list

# Build the crew binary (enters a compiler shell automatically on Nix).
build:
    ./scripts/build-env cargo build --release -p crew

# Install `crew` into ~/.cargo/bin.
install:
    ./scripts/build-env cargo install --path crates/crew --locked

# Link `crew` into ~/.local/bin so the command follows this checkout: after
# `just build` the shell runs the new binary, no reinstall needed.
link: build
    mkdir -p ~/.local/bin
    ln -sfn "{{justfile_directory()}}/target/release/crew" ~/.local/bin/crew
    @echo "crew -> {{justfile_directory()}}/target/release/crew"

# Test teams use workspace.vcs: auto, so a host without jj runs them on git.
# Run the whole test suite (no Claude Code or Codex needed).
test:
    ./scripts/build-env cargo test --workspace

# Every test team is built with workspace.vcs: git.
# Run the whole test suite on git.
test-git:
    CREW_TEST_VCS=git ./scripts/build-env cargo test --workspace

# Every test team is built with workspace.vcs: jj, and a missing or old jj
# fails instead of skipping. On Nix, jj comes from nixpkgs if missing.
# Run the whole test suite on jj.
test-jj:
    #!/usr/bin/env bash
    set -euo pipefail
    export CREW_TEST_VCS=jj
    tests=(./scripts/build-env cargo test --workspace)
    if command -v nix >/dev/null && [ -z "${CREW_JJ:-}" ] && ! command -v jj >/dev/null; then
        exec nix shell nixpkgs#jujutsu --command "${tests[@]}"
    fi
    exec "${tests[@]}"

# The sandbox tests fail on a host that cannot run them instead of skipping.
# On Nix, bwrap, socat and jj come from nixpkgs if missing. The build must not
# be under /tmp or /run, which sandboxes mask: crew and the test binaries are
# run from inside. `vcs` is the backend every test team is built with.
# Run the whole test suite with real bubblewrap, on jj (or git).
test-sandbox vcs="jj":
    #!/usr/bin/env bash
    set -euo pipefail
    export CREW_TEST_SANDBOX=require CREW_TEST_VCS={{vcs}}
    case "${CARGO_TARGET_DIR:-$PWD/target}" in
        /tmp/*|/run/*) echo "CARGO_TARGET_DIR must not be under /tmp or /run: sandboxes mask them" >&2; exit 1 ;;
    esac
    tests=(./scripts/build-env cargo test --workspace)
    need=()
    command -v bwrap >/dev/null || need+=(nixpkgs#bubblewrap)
    command -v socat >/dev/null || need+=(nixpkgs#socat)
    if [ "{{vcs}}" = jj ] && [ -z "${CREW_JJ:-}" ] && ! command -v jj >/dev/null; then
        need+=(nixpkgs#jujutsu)
    fi
    if command -v nix >/dev/null && [ ${#need[@]} -gt 0 ]; then
        exec nix shell "${need[@]}" --command "${tests[@]}"
    fi
    exec "${tests[@]}"

# Run the whole test suite with real bubblewrap, on git.
test-sandbox-git: (test-sandbox "git")

# Every suite: on git and on jj, without and with sandboxes.
test-all: test-git test-jj test-sandbox test-sandbox-git

# Run the optional end-to-end test against the real Claude Code CLI.
test-real:
    ./scripts/build-env cargo test -p crew --test real_cli -- --ignored --nocapture

# Check signal and shutdown cleanup on isolated pseudo-terminals.
test-terminal:
    if command -v python3 >/dev/null 2>&1; then ./scripts/build-env python3 crates/crew-tui/tests/terminal_signals.py; elif command -v nix >/dev/null 2>&1; then ./scripts/build-env nix shell nixpkgs#python3 --command python3 crates/crew-tui/tests/terminal_signals.py; else echo "test-terminal needs python3 or Nix" >&2; exit 127; fi

# Lint.
lint:
    ./scripts/build-env cargo clippy --workspace --all-targets

# A token-free rehearsal of the six-agent demo with scripted agents.
dry-run:
    ./scripts/build-env cargo run -p crew -- run examples/dry-run.yaml

# The six-agent demo with real Claude Code agents.
demo:
    ./scripts/build-env cargo run -p crew -- run examples/demo.yaml

# Build the web dashboard bundle into crates/crew/web-dist (commit it with the sources).
web-build:
    cd web && ([ -d node_modules ] || bun install --frozen-lockfile) && bun run build

# Frontend unit tests (vitest; no daemon).
web-test:
    cd web && ([ -d node_modules ] || bun install --frozen-lockfile) && bun run test

# Browser tests: the mock project and the perf budgets, then the live project against a
# real daemon on jj (or git: `just web-e2e git`).
web-e2e vcs="jj":
    cd web && ([ -d node_modules ] || bun install --frozen-lockfile) && bun run e2e
    ./scripts/web-e2e {{vcs}}

# The committed bundle is the one these sources build: rebuild in a temporary
# directory and compare byte for byte with crates/crew/web-dist.
web-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    [ -d web/node_modules ] || (cd web && bun install --frozen-lockfile)
    mkdir -p "$tmp/web" "$tmp/crates/crew"
    tar -C web --exclude=node_modules --exclude=test-results --exclude=playwright-report -cf - . | tar -C "$tmp/web" -xf -
    ln -s "$PWD/web/node_modules" "$tmp/web/node_modules"
    (cd "$tmp/web" && bun run build >/dev/null)
    diff -r "$tmp/crates/crew/web-dist" crates/crew/web-dist
    echo "web-dist matches the sources"

# Install crew without node or bun and check that it serves the committed bundle.
web-install-check:
    ./scripts/web-install-check
