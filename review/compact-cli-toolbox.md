# Opt-in compact CLI toolbox

Card `compact-cli-toolbox`, based on main `a9979a4e`. Production change
`a4fc069c`; acceptance refinements and evidence `d8b65ebf`, `2437e26d`.

Members can select `toolbox.include: [compact-cli]` to discover a fixed skill
for viewing already captured cargo test logs and recovering exact raw bytes.
Nothing selects it by default. Existing bundle overrides, exclusions, resets,
roster persistence and subagent selection apply. A same-name user bundle
replaces the built-in whole, and a user file skill remains distinct.

The normal immutable skill store holds the instructions, a view-only launcher
and a manifest containing the actual daemon executable's SHA-256, format
version and parser id. The compiled executable contains all parsing, rendering
and storage rules. Runtime paths stay outside canonical bytes. Both view and
raw recovery verify their executing image before authentication or file access.
An inherited different-build helper becomes unavailable instead of silently
using a new binary. The helper retains the original member authentication,
artifact scope and filesystem permissions.

No hook, runtime extension, MCP server, tool schema or permission grant is
added. Native Codex `workspace-write` cannot reach the daemon socket, so that
configuration explicitly reports the built-in skill as unavailable; the
bundle does not relax it. Existing unrestricted Codex settings and Crew's
supported OS sandbox path are covered separately. See the
[user guide](../crates/crew/src/output/README.md) and
[native reproducer](../notes/compact-cli-probes/README.md).

## Final gates

The full workspace run covers the final source and test tree at `2437e26d`.
Later edits are this report only. All model-runtime probes used a localhost
stub in a network-isolated namespace; no provider calls or savings measurement.

| Gate | Result | Retained evidence under UI `target/` |
| --- | --- | --- |
| Full required-sandbox workspace | 1,326 passed, 0 failed, 4 ignored; 73 harness results | `compact-workspace-final.log` |
| Toolbox launch, included above | 10 passed, including actual selected script execution in the required member sandbox, immutable helper binding, pending exclusion and guarded Codex unavailability | Same workspace log |
| New core selection/roundtrip tests | 2 passed; optional inclusion, nesting, user override, reset/exclusion and team/style serialization | Same workspace log; initial `compact-core.log` |
| New materialization/pin tests | 2 passed across Claude/Codex/pi; path-independent bytes, changed helper refusal and unavailable states | Same workspace log; initial `compact-prepare.log` |
| Strict workspace/all-target Clippy | Passed | `compact-clippy-final.log` |
| Formatting | Passed | `compact-fmt-final.log` |
| Rust 1.82 default CLI and output help | Build and help passed; compiler `1.82.0` | `compact-msrv.log`, `compact-msrv-{view,raw}-help.txt` |
| Skill validator | Passed | `compact-skill-check.log` |
| Native acceptance | All eight baseline/selected arms passed: Claude 2.1.291, Codex 0.159.0 guarded and unrestricted, pi 1.1.0 | `cpg/report.json`, `compact-native-final-v2.log` |

Reproduction commands, from this worktree:

```sh
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR="$PWD/target/cw"
CREW_TEST_SANDBOX=require nix shell nixpkgs#socat --command \
  ./scripts/build-env cargo test --locked --workspace --no-fail-fast -- --test-threads=1
./scripts/build-env cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
CARGO_BUILD_JOBS=2 CREW_MSRV_TARGET_DIR="$PWD/target/compact-msrv" ./scripts/check-msrv
node notes/compact-cli-probes/run.mjs --crew target/cw/debug/crew --scratch target/NEW-DIR
```

The four workspace ignores are the separately invoked obligation measurement,
paid real-Claude integration, explicit-binary shared-terminal adapter and
interactive-terminal dashboard. No compact-cli case is skipped.

Native assertions verify the selected catalog name and exact description,
with baseline absence; original permission denials; all 150 requested source
lines and Claude's unchanged line metadata; summary provenance and exact raw
recovery; old transcript bytes and resumed tool results; and byte stability
after a clock tick and a completed member restart. The restart barrier checks
a new PID, new launch identity, the new CLI's readiness banner after its
session-start hook, and an active unchanged toolbox fingerprint before reading
the rematerialized files. The detailed runtime-policy limits are in the
reproducer README. Compact evidence is committed in
[`evidence.json`](../notes/compact-cli-probes/evidence.json); native prompt and
request captures remain only in ignored scratch.

The final native executable and the final workspace CLI are byte-identical:
SHA-256 `690bd3b0b4b8e3923c8ea97ff870a53e8876c67453891f295d919c86f2553d9b`.
The exact retained pin is
`target/cpg/workspace/.crew/bin/crew-7db71e056af94ecb`.

## Earlier attempts

The initial workspace pass (`compact-workspace.log`) found five fixture
failures because `socat` was missing, an expected bundle-list assertion that
needed the new built-in name, and a doctest include removed during requested
Cargo-cache cleanup. The final run rebuilds generated inputs, supplies
`socat`, and tests orchestrator selection and roster persistence of the new
bundle. A focused fixture initially waited for an empty Codex skill-root RPC
that is intentionally never sent; it now verifies its absence.

Native `cp2` exposed the guarded Codex socket denial, which informed the
explicit unavailable state. Earlier `cp1`, `cp3` and `cp4` captures predate the
stronger catalog/restart assertions. `cpf` passed every native arm but used an
incorrect idle-only readiness assertion for the working recorded CLI. `cpg`
is the authoritative full replay with the corrected process/launch/readiness
barrier. These attempts remain retained and are not counted as final passes.

This establishes deterministic packaging, native behavior and exact recovery.
Whether the skill reduces total model usage after discovery, recovery and
repair turns remains the later paired QA measurement.
