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

# Run the optional end-to-end test against the real Claude Code CLI.
test-real:
    ./scripts/build-env cargo test -p crew --test real_cli -- --ignored --nocapture

# Lint.
lint:
    ./scripts/build-env cargo clippy --workspace --all-targets

# A token-free rehearsal of the six-agent demo with scripted agents.
dry-run:
    ./scripts/build-env cargo run -p crew -- run examples/dry-run.yaml

# The six-agent demo with real Claude Code agents.
demo:
    ./scripts/build-env cargo run -p crew -- run examples/demo.yaml
