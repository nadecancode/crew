# Native Codex restart fixture

The native Codex toolbox suite now passes all 12 cases. The fix creates the
fixture's explicitly selected `CODEX_HOME` before starting its daemon; no
product code or toolbox policy changes.

## Cause

The fixture set `CODEX_HOME` to a directory that did not exist. Its stand-in
created that directory while recording its first invocation, so default tests
passed. Codex 0.159.0 instead refuses even `mcp list --json` when an explicit
`CODEX_HOME` does not exist.

An isolated direct control reproduces the distinction: the nonexistent home
exits 1 with `failed to resolve CODEX_HOME`; creating the same empty directory
makes the same command exit 0 with `[]`. The full restart test failed before
the fix with the reported inventory error. Its first discovery invocation
strips injected MCP overrides, so the selected fake-MCP server and its state
file are not involved in that failure.

The shared fixture now creates its synthetic Codex home alongside its synthetic
`HOME`. This does not create or modify an operator's actual configuration.

## Validation

Base: approved Claude integration `c958c233`. Native executable:
`codex-cli 0.159.0`, SHA-256
`050b3a798038a9c80066b89fc380b307b2598f7caaf432f3311e482c0840ad53`.

Evidence is retained in graph's `target/codex-t14-native/`:

| Gate | Result | Evidence |
| --- | --- | --- |
| Missing/existing explicit home control | Exit 1 / exit 0 with empty inventory | `missing.json`, `existing.json` |
| Native restart regression before fix | Failed as reported | `before.log` |
| Complete toolbox launch suite, real Codex and sandbox required | 12 passed, no failures or skips | `native-suite.log` |
| Strict Crew Clippy, all targets | Passed | `clippy.log` |
| Formatting | Passed | `fmt.log` |

The native parser accepted every captured TUI and app-server override: 16/18
outside the sandbox and 12/14 inside it. Native inventory/configuration checks
do not make inference requests. The successful localhost T14/T18 model-visible
replays remain separate evidence; this closes the previously failing combined
inventory fixture gate.

```sh
export CARGO_TARGET_DIR="$PWD/target/t"
nix shell nixpkgs#socat nixpkgs#gcc nixpkgs#pkg-config --command \
  env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH -u CREW_PI \
  CREW_TEST_SANDBOX=require \
  CREW_TEST_CODEX_BINARY=/absolute/path/to/codex \
  cargo test --locked -p crew --test toolbox_launch -- --nocapture
./scripts/build-env cargo clippy --locked -p crew --all-targets -- -D warnings
cargo fmt --all -- --check
```
