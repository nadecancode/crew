# Offline output-filter reproducer

This is a research artifact, not a crew feature or hook installer. It replays
fixed repository files and captured test output through three pinned Linux
x86_64 executables. No model provider is contacted. The optional native probe
uses Claude Code with a localhost stub inside a network namespace.

Requirements: Linux with bubblewrap, Node 24, ordinary shell/awk/sed tools on
PATH, and the executables in `pins.json`. Binary archives and executable
SHA-256 values are both recorded. Download the pinned archives into private
scratch, verify the archive digest before extraction, and place each binary
at `BIN_DIR/lowfat/lowfat`, `BIN_DIR/rtk/rtk`, `BIN_DIR/tokf/tokf`. The runner
checks the executable hashes again. It downloads and installs nothing.

Prepare a private tokenizer installation; lifecycle scripts are disabled:

```sh
mkdir -p target/token-saver-tools/tokenizer
cp notes/token-savers/tokenizer/package*.json target/token-saver-tools/tokenizer/
touch target/token-saver-tools/empty-user.npmrc target/token-saver-tools/empty-global.npmrc
env NPM_CONFIG_USERCONFIG="$PWD/target/token-saver-tools/empty-user.npmrc" \
  NPM_CONFIG_GLOBALCONFIG="$PWD/target/token-saver-tools/empty-global.npmrc" \
  npm ci --prefix target/token-saver-tools/tokenizer \
  --cache "$PWD/target/token-saver-tools/npm-cache" --ignore-scripts --no-audit --no-fund
```

Run from the worktree, supplying the verified binary directory:

```sh
node notes/token-savers/run.mjs \
  --scratch target/token-saver-run-01 \
  --bin target/token-saver-tools/bin \
  --tokenizer target/token-saver-tools/tokenizer/node_modules/js-tiktoken
```

`--scratch` must name a new directory inside this worktree's `target`.
All benchmark writes stay there. Each filter child has a read-only host
filesystem, writable scratch, private configuration and no network. This
does not hide every readable host file; it is a bounded replay harness, not
a general-purpose hostile-code sandbox. Use only the hash-checked binaries.
Each process has a 10-second timeout and an 8 MiB output cap.

Add `--native` only with the examined Claude Code 2.1.291 installed. The
runner creates fresh homes, empty MCP config and a private `/tmp`, supplies
only a dummy key, and isolates the stub and native child from external
network access. It deliberately demonstrates the upstream hook's permission
change on a disposable fixture. No real project file is modified. The hook
also compresses the first 150 lines of the fixed Cargo.lock fixture. Native
request captures remain under scratch; they are not committed here.

The runner asserts 57 output observations against the checked-in evidence,
including exit status and token counts, verifies exact RTK raw recall, and
replays the three separate child-exit cases.
It also checks that lowfat's raw-output footer changes after a clock tick.
With `--native`, it verifies denied raw command, successful hooked fixture
write, and replaced Read metadata. A failure is an experiment failure,
not a reason to rewrite the expected results automatically.

## Inputs and interpretation

`corpus.json.gz` contains 15 named UTF-8 fixtures with SHA-256 hashes in
`pins.json`. Decompression is bounded to 1 MiB. Nine are real snapshots:
Cargo.lock, crew-core's toolbox source, the toolbox note, web index.html,
directory/find output, the `c4d2d671` to `93a81f1f` TUI fix diff, and actual
passing/failing reviewer test logs. The remaining six are synthetic edge
cases: a warning prepended to the passing log, zero tests, unknown runner,
machine-format output, mode-only diff and rename-only diff. The snapshots
preserve their original text, including local paths printed by the test
runner. No native conversation, model credential or source-home state is
included.

The cargo shim validates that its stdout equals the entire fixture and
retains exit 101 before any filter runs. It lets RTK's real cargo adapter
process recorded output without rebuilding crew. `tokf apply` selects a
filter and exit branch directly; it does not prove `tokf run` preserves a
child status. `evidence/exit-probe.json` records that separate check.

Counts use `js-tiktoken@1.0.21` with `cl100k_base`, a common comparison proxy.
They are not native Claude/Codex/pi billing tokens or end-to-end savings.
The runner repeats each filter in a separate process. Per-run paths and the
intentional variable footer are not compared as fixed strings. Golden
outputs are observations of particular upstream builds, including their
information loss; matching them does not mean the tool is safe to enable.

## Provenance and licenses

The five exact filter files are copied from the pinned lowfat release and
tokf source. Their upstream URLs and SHA-256 hashes are in `pins.json`;
their Apache-2.0 and MIT texts are in `licenses/`. Preserve those notices.
The executables themselves are not distributed by this artifact. Eight
repository metadata pins record the source survey; a source-tree pin and a
release-binary pin are intentionally distinct when upstream differs.

The packaged replay passed all 57 observations and the native probe in
`target/token-savers-repro-02` on Node 24.19.0; the added child-exit assertions
and all 57 observations then passed in `target/token-savers-repro-03`.
An earlier attempt gave the
native child a read-only `/tmp`; its Bash step failed with EROFS before the
fixture command ran. The final harness binds a private scratch directory
there, and the permission difference reproduces. This environment repair
did not change the fixture, hook, filter or measured corpus.
