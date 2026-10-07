# Readable native gateway fingerprint

Task `native-cli-fingerprint`, based on main
`e3ba08ba641459f3cb06a01931b14d7159a0c997` (the operator's macOS build change).
`crew native status` now prints a public fingerprint line on stderr as sixteen
groups of four lowercase hexadecimal characters. Its pretty JSON stdout,
including the original contiguous fingerprint, is unchanged. Other native
commands keep their existing output. Administrative authentication, pairing,
grants, listeners, protocol records and dependency graphs are unchanged.

The real CLI regression compares stdout byte-for-byte with the existing
pretty-serialized protected control response. It compiles the desktop app's
actual pure `group_hex`/`groups` source as a tiny fixture, then compares stderr
with that helper's output and checks the sixteen lowercase groups and the
original fingerprint. This avoids adding GTK or the desktop's higher compiler
floor to the default CLI dependency graph.

Verified on this Linux host:

- Focused real CLI/app grouping conformance: 1/1.
- Complete real TLS/daemon `native_gateway` suite: 21/21.
- All-target clippy with `-D warnings`, gateway feature on and off.
- Formatting of both changed Rust files.
- Actual Rust/Cargo 1.82.0 locked default CLI build and help checks.

Whole-tree `cargo fmt --all --check` reports pre-existing formatting differences
introduced by the operator's macOS commit in `os.rs`, `daemon/binary.rs`,
`git/beneath.rs`, and `sandbox/{landlock,run}.rs`. Those files remain byte-equal
to the base; the CLI patch contains none of that unrelated formatting.

Logs are retained in `target/native-cli-fingerprint/`. Inherited `GIT_*`,
`CREW_*` and `HERDR_*` are removed for fixture runs. Tests use temporary
runtime/state/config paths and explicit loopback TLS; no operator service,
real device or installed binary changes. The sequential pipeline used slot A,
existing owned targets and `CARGO_INCREMENTAL=0`; the volume had over 100 GiB
available before starting. Apple execution remains deferred by operator.

The preceding runtime tip is preserved at `agents/runtime-pre-fingerprint`
(`f94d0d8b`). No existing commit was rewritten or abandoned. Owned disposable
non-executable Cargo artifacts were cleaned separately; cited logs, binaries,
backups and evidence remain intact.
