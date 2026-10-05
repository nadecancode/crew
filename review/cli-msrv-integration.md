# Default CLI Rust 1.82 integration

The reviewed compatibility correction is integrated onto the reviewed root
baseline `d0b1e4e9b5140982254e83c98b588ee25f8bd592`. Original source commit
`3b2a1c960211d52a5b2261394d6447253e4ced9d` was picked as
`ad26c9015b0a983a6aad0bded4db66bdeeea7e8f`. No N1, native app or gateway
implementation ancestry was imported.

The five source paths are `Cargo.lock`, `README.md`, `justfile`,
`scripts/check-msrv` and `notes/default-cli-msrv.md`. This report is the only
additional path. The compatible dependency selections, primary-source
references and upstream regression evidence remain in the
[compatibility report](../notes/default-cli-msrv.md). The workspace manifest,
production Rust/TUI source, web source/bundle and accepted native preparation
scripts match `d0b1e4e9`. The bundle remains
`fnv1a64:b1e2e05bd16fb63d`, with P1/P2/P3/P4 totals
230,497 / 33,906 / 332,915 / 598,004 bytes.

## Exact landing validation

QA installed a separate toolchain under this worktree's
`target/cli-msrv-integration/rustup`, leaving the host's default toolchain
unchanged. Actual versions:

```text
rustc 1.82.0 (f6e511eec 2024-10-15)
cargo 1.82.0 (8f40fc59f 2024-08-21)
```

On the exact picked source tree, with `CREW_IDENTITY`, `CREW_SOCKET` and
`CREW_LAUNCH` removed:

```sh
export RUSTUP_HOME="$PWD/target/cli-msrv-integration/rustup"
export CARGO_TARGET_DIR="$PWD/target/cli-msrv-integration/build"
export RUSTC="$(rustup which --toolchain 1.82.0 rustc)"
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH \
  ./scripts/build-env rustup run 1.82.0 cargo check --locked -p crew --bin crew
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH \
  CREW_MSRV_TARGET_DIR="$CARGO_TARGET_DIR" ./scripts/check-msrv
```

The locked check passed in 10.83 seconds; the gate's linked build passed in
16.89 seconds. The binary reported `crew 0.1.0` and parsed top-level help plus
`run`, `attach`, `web` and `stop` help. Check ran at 20:35:16–20:35:27 UTC on
2026-10-05; the linked/help gate ran at 20:35:27–20:35:44 UTC.

Local ignored evidence is in
`/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/cli-msrv-integration/`:
`toolchain.log`, `check.log`, `gate.log` and the `.started`/`.finished` timestamps.
This landing did not rerun unchanged web/native suites or the upstream 291
focused tests. Their previously reviewed result belongs to the source card;
the exact landing proof here is the actual compiler's locked check/build/help
gate.

## Scope and activation

Rust declarations and default features remain unchanged. This establishes the
default Linux CLI gate with the committed lockfile; it does not establish a
Rust 1.82 full-workspace or optional native-feature build. Final native
integration must preserve these selections and rerun the same actual compiler
gate after combining optional native dependencies.

No daemon, production listener or operator desktop was changed. No service
restart is required for this source/tooling landing.
