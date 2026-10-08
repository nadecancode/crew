# Toolbox launch review

Members now load their resolved toolbox through runtime-native discovery
controls. Selected files and MCP catalogs are frozen in an immutable store;
reference values stay in daemon memory and reach only the selected proxy.
Unavailable and failed items remain visible, required items stop the launch,
and reload stages a new selection without changing a running session.

## Composition

The full-matrix source commit is `bb3b57be`, approved with this report at
`12dd7b9c`. The later landing composition is recorded below. The baseline merge
contains main `cb95b650` and all three coordinated slices:

- Toolbox launch: graph's implementation through `65a3d0f2` and the integration
  merges. I1 is fixed by immutable MCP snapshot references captured before
  subagent creation can await; both default and selected-bundle paths are tested.
- Toolbox MCP controls: approved `aaea82f4`, plus review-note correction
  `99956858`. The daemon owns pinning; session proxies only read the pins.
- Pi registration barrier: approved `89650283`, plus comment-only `476879df`.
  The guarantee covers managed servers; inherited sources are outside it.

Scheduler's keep-alive code and stall fix `c1566c70` are included. Its code
approval is separate from D2's policy decision. The setting remains experimental,
opt-in and off by default. No default or style enables it. Three priced long-gap
pairs and production evidence are still required before default/style enablement;
this card makes no savings claim. See [launch notes](../notes/toolbox-launch.md).

## Final gates

All results below use the composed source, including the stall fix and current
bundle minifier. No test failure or bundle-budget exception remains.

| Gate | Result | Local evidence under `target/toolbox-launch/` |
| --- | --- | --- |
| Locked workspace, real sandbox required | 1,294 passed, 0 failed, 4 explicitly ignored | `final-workspace-tests.log` |
| Crew library (included above) | 541 passed, 0 failed, 1 ignored | same |
| Team integration (included above) | 89 passed | same |
| Toolbox launch / proxy (included above) | 8 / 15 passed | same |
| Private home (included above) | 3 passed | same |
| Sandbox / sandbox launch (included above) | 29 / 29 passed | same |
| Web bundle (included above) | 2 passed, original size caps | same |
| Workspace Clippy, all targets, warnings denied | Passed | `final-clippy.log` |
| `cargo fmt --all -- --check` | Passed | `final-fmt.log` (empty on success) |
| Rust 1.82 locked default CLI build and help | Passed | `final-msrv.log` |
| Native pi 1.1 provider-payload tests | 8 passed, no skips | `final-pi-tests.log` |
| Native Codex 0.159 inventory/parser integration | 8 passed, no skips | `final-native-codex-tests.log` |

The four workspace ignores are the explicit obligation benchmark, the paid
real-Claude test, the live TUI adapter requiring a separately named binary, and
the interactive live dashboard. Pi's opt-in native cases are run explicitly in
the separate eight-case gate, not inferred from the default workspace run.

Codex's native gate creates operator skill directories to exercise
`skills.config`, and parses **every** captured TUI and app-server `-c` pair with
the installed CLI: 16/18 overrides unsandboxed and 12/14 sandboxed. It checks
ambient names with dots, spaces and embedded quotes, same-name HTTP/stdio
collisions, and both process launch paths. The generated values are TOML, not
JSON arrays of objects. Native inventory performs no model request.

The pi suite captures actual localhost provider requests across randomized
registration timing, restarts, reconnects, required/strict failures, optional
late answers, namespace collisions and zero-tool resource servers. A local stub
responds; no request reaches a provider. The independent Codex TUI root
propagation runner also uses only a localhost HTTP 400 stub. Neither proves real
login, provider cache hits, or priced savings.

## Reproduction

Use an absolute, short target path: sandbox unit tests find the built binary
through `CARGO_TARGET_DIR`, and socket fixtures have a platform path limit.
The initial gate attempt used a relative/deep target and lacked `socat`; it
failed those harness prerequisites. The clean run below supplies them without
skipping or weakening sandbox checks.

```sh
export CARGO_TARGET_DIR="$PWD/target/t"
nix shell nixpkgs#socat nixpkgs#gcc nixpkgs#pkg-config --command \
  env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH -u CREW_PI \
  CREW_TEST_SANDBOX=require cargo test --locked --workspace --no-fail-fast
./scripts/build-env cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
CREW_MSRV_TARGET_DIR="$PWD/target/msrv" ./scripts/check-msrv
CREW_TEST_PI="$PWD/target/toolbox-audit/pi-runtime/node_modules/.bin/pi" \
  ./scripts/build-env cargo test --locked -p crew --test pi_tool_order -- --nocapture
CREW_TEST_CODEX_BINARY=/absolute/path/to/codex CREW_TEST_SANDBOX=require \
  ./scripts/build-env cargo test --locked -p crew --test toolbox_launch -- --nocapture
```

The pi installation and standalone propagation runner are documented in
[native toolbox probes](../notes/toolbox-probes/README.md). Integration test
homes are isolated. Custom fake Codex sessions delegate inventory-only commands
to the shared CLI stand-in, so an inventory no longer increments session counts
or emits simulated lifecycle hooks. No product discovery bypass was added for
tests.

## Approved landing update

`44eef93b` composes the approved launch with main `c2f690cb` and scheduler's
approved K1 fix `acd945fe`. The only merge conflict was the Operator MCP row in
`notes/toolboxes.md`; main's complete two-pass Discovery/Verification wording
was retained. No toolbox adapter, snapshot-store or reference-resolution code
changed. The product delta is the bounded keep-alive health fix and main's
reviewed MCP tool wording changes.

The full keep-alive code through `acd945fe` has a code-only approval; the default
remains off and the D2 policy and priced experiment remain separate. Per the
orchestrator's scoped integration gate, the merged source passes:

- Crew library `keepalive::tests`: 8 passed (`final-k1-tests.log`).
- Crew library `mcp::tools`: 6 passed (`final-tools-tests.log`).
- `team`: 89 passed; `web_bundle`: 2 passed (`final-merge-tests.log`).

Those logs are under `target/toolbox-launch/`. The integration commands use the
same absolute short target and sandbox-capable environment as the matrix above.
This update changes only the review report after the tested merge.
