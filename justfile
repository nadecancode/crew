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

# Run the whole test suite (no Claude Code or Codex needed).
test:
    ./scripts/build-env cargo test --workspace

# Run the sandbox tests with real bubblewrap; a host that cannot run them fails
# instead of skipping. On Nix, bwrap and socat come from nixpkgs if missing.
test-sandbox:
    #!/usr/bin/env bash
    set -euo pipefail
    export CREW_TEST_SANDBOX=require
    tests=(./scripts/build-env cargo test -p crew --test sandbox --test sandbox_launch)
    if command -v nix >/dev/null && ! { command -v bwrap && command -v socat; } >/dev/null; then
        exec nix shell nixpkgs#bubblewrap nixpkgs#socat --command "${tests[@]}"
    fi
    exec "${tests[@]}"

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
