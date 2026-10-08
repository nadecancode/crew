# Codex source controls: T12

Source: `20f48cc76fb2e99a8dea37420ff4d228b4f7a57e`, on the authorized merge
`f4817528` of main `201e7fdc`. The merge preserved both histories and its tree
was identical to main. This report and the sanitized evidence add no product
changes.

## Result

Crew now reports present, excluded Codex project MCP servers and native child
roles as **Cannot be excluded on Codex**. Strict mode refuses these sources
before launching the member. This is an honest limitation, not a claim that
Codex stopped loading them.

Codex 0.159.0 omits an untrusted project's MCP servers from its launch
inventory, then can load them after the TUI trust prompt. A disabled-only
override before trust is invalid because it has no transport. Crew continues
disabling names actually returned by native inventory, while reserving all
known project names for collision checks: an ambient `crew` name stops launch;
a selected item with a colliding name is Shadowed and omitted. This prevents
later trust from merging a hidden project definition into a managed mapping.

`features.multi_agent=false` removes the legacy function-tool surface, but
does not remove code-mode children or their operator and project role
definitions. The existing flag remains useful for the legacy path. Source
discovery now includes `agents.<name>` configuration tables as well as native
TOML files, including roles configured outside an agents directory. Private
homes still exclude operator roles. The mapping version is now 2.

## Validation

All commands used an environment with inherited `CREW_*` variables removed,
`CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=4`, and the isolated
`CARGO_TARGET_DIR=target/t12`. Cargo jobs ran serially under granted slot E;
the slot was released when they finished.

| Gate | Result | Evidence in cache-reviewer's worktree |
| --- | --- | --- |
| `cargo test --locked -p crew --lib toolbox` | 28 passed | `target/toolbox-t12/logs/lib-toolbox-final.log` |
| `CREW_TEST_SANDBOX=require CREW_TEST_CODEX_BINARY=<native Codex> cargo test --locked -p crew --test toolbox_launch -- --test-threads=1 --nocapture` | 16 passed | `target/toolbox-t12/logs/toolbox-launch-native-02.log` |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed | `target/toolbox-t12/logs/clippy.log` |
| `cargo fmt --check` | Passed | `target/toolbox-t12/logs/fmt.log` |
| `node notes/toolbox-probes/codex-trust.mjs` | Four native cases passed | `target/toolbox-t12/codex-trust/report.json` |
| Native code-mode parent/child capture, Codex 0.159.0 | Both negative and positive controls observed; source rows truthful | [Sanitized evidence](toolbox-t12/native-evidence.json) |

The first launch-suite run passed 14 tests and failed the two new fixtures
because their project configuration existed before an initial repository
commit. The fixture now commits that configuration before `crew run`.
The complete rerun passed all 16 tests. The original failure remains in
`target/toolbox-t12/logs/toolbox-launch-native.log`.

The new integration cases exercise actual daemon dispatch and both launch
processes, strict refusal for project MCP and roles, and collision reservation
for untrusted project names. Native inventory runs in those cases make no
model request. No RPC method changed; the native-gateway inventory gate is
not triggered. This card does not claim a full workspace test or MSRV rerun.

## Native capture and reproduction

The retained executable is
`target/toolbox-t12/crew-20f48cc7`, SHA256
`58820f187b7a2b62d422323c070527655f8e2921eaf9e49a116b7efe899a0c65`.
It is a read-only copy of the exact source build used by the passing suite.
`target/toolbox-t12/native-provenance.json` records the executable and runner
hashes.

The native source/child harness was extracted from QA commit `378d0b43` into
`target/toolbox-t12/qa-runner`: `scripts/toolboxes/*.py`,
`fixtures/toolboxes/*.py`, and `scripts/cache/{bench,usage}.py`. Its only
change is in `discovery.py`'s negative member setup:

```python
# Was: first.pop("toolbox")
first["toolbox"] = {"inherit": {"operator": "none", "project": "none"}}
```

Run that harness with the pinned executable and a new output directory:

```text
python3 -B scripts/toolboxes/discovery.py --crew <pin> --sha256 <sha256> \
  --runtime codex --case sources --child --out <new-output-directory>
```

For this capture the entire runner, local provider, Crew daemon and native
children ran inside bubblewrap with `--unshare-net --unshare-pid`, a private
`/tmp`, read-only host mounts and writable access only to the fixture evidence
tree. Only loopback fixture responses were possible. All homes and credentials
were synthetic; no operator login was read or copied. The local provider
supplied deterministic responses, not paid inference. Cleanup stopped the
fixture daemon and reported no remaining owned process.

The negative member's actual child request 12 still contains both role canaries
in the `agent_type` description, at
`/input/0/tools/2/tools/4/parameters/properties/agent_type/description`.
The inherited-role control's child request 15 contains them too. The negative
member's parent argv contains `features.multi_agent=false`, and its source
rows identify both excluded role scopes as `cannot_be_excluded`. Its post-start
native inventory has the project MCP server enabled and the operator server
disabled; the source rows report those states truthfully. Native transcript
receipts prove both parent turns finished. Full synthetic captures remain in
`target/toolbox-t12/native-code-mode`; only hashes, source states and canary
locations are committed.

The separately tracked `codex-child-session-id` issue remains visible on this
base: a child's notification changes the daemon's parent session id. The
fixture deliberately matches the original parent's native transcript for
completion. This card changes no session identity code and makes no claim
that the separate issue is fixed.

QA owns the T12/T13 acceptance-matrix updates and integrated native replays.
