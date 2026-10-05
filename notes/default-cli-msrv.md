# Default CLI Rust compatibility

Source baseline: approved N1/conformance at `418d2085`, production derived from
`6049cdf6`. Check performed on Linux x86_64, 2026-10-05. The compatibility target
is the default `crew` CLI with its committed lockfile, not optional native app
or gateway features. No active daemon or dashboard was stopped or replaced.

`Cargo.toml` declares Rust 1.82. The original lock selected thirteen packages
in the default Linux normal/build dependency closure declaring a higher floor:
Clap and its builder/derive/lexer, UUID/getrandom, instability/darling,
indexmap/hashbrown, and unicode-segmentation. Cargo 1.82 failed before compiling
crew, parsing `indexmap 2.14.2`'s edition-2024 manifest. Newer-compiler success
did not establish the declared compatibility.

## Smallest correction

Only the lockfile selects older compatible releases; package versions, features,
Rust declarations and product source stay unchanged. Selection used the primary
crates.io sparse index, then the downloaded crate manifests/checksums. The
chosen root releases are the latest non-yanked versions in their existing
dependency ranges compatible with 1.82, except unicode-segmentation has no
published `rust-version` on its latest compatible 1.12 release; the actual
compiler build establishes that case.

| Dependency | Original | Selected | Selected published Rust floor | Primary manifest |
| --- | --- | --- | --- | --- |
| clap / clap_builder / clap_derive | 4.6.7 | 4.5.61 | 1.74 | [Clap 4.5.61](https://docs.rs/crate/clap/4.5.61/source/Cargo.toml) |
| clap_lex | 1.1.1 | 1.0.1 | 1.74 | [clap_lex 1.0.1](https://docs.rs/crate/clap_lex/1.0.1/source/Cargo.toml) |
| uuid | 1.26.1 | 1.20.0 | 1.63 | [UUID 1.20.0](https://docs.rs/crate/uuid/1.20.0/source/Cargo.toml) |
| instability | 0.3.14 | 0.3.10 | 1.64 | [instability 0.3.10](https://docs.rs/crate/instability/0.3.10/source/Cargo.toml) |
| darling / darling_core / darling_macro | 0.24.1 | 0.20.11 | 1.56 | [darling 0.20.11](https://docs.rs/crate/darling/0.20.11/source/Cargo.toml) |
| indexmap | 2.14.2 | 2.13.1 | 1.82 | [indexmap 2.13.1](https://docs.rs/crate/indexmap/2.13.1/source/Cargo.toml) |
| hashbrown (indexmap edge) | 0.17.1 | 0.16.1 | 1.65 | [hashbrown 0.16.1](https://docs.rs/crate/hashbrown/0.16.1/source/Cargo.toml) |
| unicode-segmentation | 1.13.3 | 1.12.0 | Not declared; tested on 1.82 | [unicode-segmentation 1.12.0](https://docs.rs/crate/unicode-segmentation/1.12.0/source/Cargo.toml) |

Their required transitive changes add `fnv`, remove `getrandom 0.4`/`r-efi 6`,
and unify compatible existing Windows dependency edges. No unrelated package
upgrade was requested. A normal/build tree audit now has no selected Linux
dependency declaring a Rust floor above 1.82. This is metadata corroboration;
the real compiler check below is the compatibility proof.

Clap's existing CLI declarations compile without changes. UUID's v4 generation
and token format remain the existing API. Unicode and TUI rendering checks run
against the selected segmentation version; no native UI support claim is made
by the lockfile correction.

## Reproduction and gate

The exact compiler installed under the worktree's ignored `target/` directory
reported:

```text
rustc 1.82.0 (f6e511eec 2024-10-15)
cargo 1.82.0 (8f40fc59f 2024-08-21)
```

The unchanged-lock failure was reproduced with that compiler, using a separate
target directory. On the repaired lock these commands use the actual 1.82
compiler, not `--ignore-rust-version` or a newer compiler:

```sh
./scripts/build-env rustup run 1.82.0 cargo check --locked -p crew --bin crew
./scripts/build-env rustup run 1.82.0 cargo build --locked -p crew --bin crew
```

Both pass; check took 11.86 s, linked build 16.99 s. The resulting `crew 0.1.0`
parses its top-level help and `run`, `attach`, `web`, `stop` help. The permanent
gate pins the compiler path, uses a separate target directory and executes only
those read-only help/version commands:

```sh
rustup toolchain install 1.82.0 --profile minimal
just msrv-check
# equivalent:
./scripts/check-msrv
```

`RUSTUP_HOME` can select an isolated toolchain installation;
`CREW_MSRV_TARGET_DIR` can select an isolated build directory. This card used
`target/default-cli-msrv-rustup` and `target/default-cli-msrv-build`.

Relevant regressions also ran on actual Rust 1.82 with `--locked`:

```sh
./scripts/build-env rustup run 1.82.0 cargo test --locked -p crew-core -p crew-tui -p crew-client -p crew-protocol
./scripts/build-env rustup run 1.82.0 cargo test --locked -p crew --lib cli::
```

Core/TUI/client/protocol: 271 passed, zero failed, one ignored, including all
60 unchanged TUI render tests and the N1 corpus consumer. CLI: 20 passed,
zero failed. The compiler built the complete default library test target for
the CLI-filtered run. These checks verify the affected argument parser,
Unicode/rendering, config/indexmap, UUID helpers and unchanged domain behavior.
Unchanged full N1 workspace evidence remains separate; this card does not
claim a new full-workspace or native-feature run.

Dependency updates must rerun this gate. README installation commands use
`--locked`, as `just install` already did, because Cargo otherwise ignores the
packaged lockfile during installation. [Cargo install lockfile behavior](https://doc.rust-lang.org/cargo/commands/cargo-install.html#dealing-with-the-lockfile)

Cargo's modern `incompatible-rust-versions = "fallback"` setting helped select
compatible candidates, but it is respected only from Cargo 1.84. It is not a
replacement for the locked Rust 1.82 gate, and no workspace resolver policy was
changed. [Cargo resolver configuration](https://doc.rust-lang.org/cargo/reference/config.html#resolverincompatible-rust-versions)

Optional native/gateway source uses its separately approved newer toolchain.
This check neither builds nor certifies that feature graph, Apple binaries,
signing, simulator behavior or deployment. QA must rerun the default gate after
integrating the independent N2A/native manifest changes.

Local ignored logs: `target/default-cli-msrv-{before,check,build,gate,regressions,cli-tests}.log`.
