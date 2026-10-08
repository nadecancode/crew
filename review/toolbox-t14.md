# Explicit MCP catalog refresh

`crew resume --restart x` now refreshes the member's managed MCP definitions
while resuming its confirmed conversation. Automatic recovery still uses the
old pinned catalog. The fingerprint stays the same when only upstream tool
definitions change; the definition digest changes, and the daemon emits one
`toolbox.changed` after the new process starts.

## Cause and change

The CLI already sent `Resume { restart: true }`, but `resume_limited` launched
every restarted member with `Start::Resume`. Toolbox preflight refreshed a pin
only for `Start::Fresh`, which deliberately discards the conversation. The
proxy's existing T14 test called `pin(..., true)` directly and did not exercise
the operator command or launch dispatch.

Production commit `4adfa536` adds `Start::Refresh`: resume the confirmed session
and request a catalog refresh. Only explicit operator `resume --restart` selects
this new intent. Ordinary agent restarts, crash recovery, automatic usage-limit
resumption and model restarts retain their previous behavior; fresh-session
restarts continue to refresh. No RPC shape, proxy API or snapshot format changes.

The inherited-catalog guard remains `refresh && inherited.is_none()`. Subagents
continue to restore their immutable inherited snapshots, without reopening a
parent's mutable pin or widening the inherited selection.

Scheduler reviewed the production seam in `m-7eff7e3f1074`. Follow-up `ff9eb12d`
addresses the suggested regression improvement: assertions inspect the newly
captured native command arguments, not only the daemon's recorded session id.

## Real-daemon regression

The three added `toolbox_launch` tests run actual Crew daemons, PTYs, preflight
helpers and `crew resume --restart`; runtime stand-ins record their launches.
Each case proves:

- Initial upstream definitions are pinned.
- After the upstream description changes, a crashed process resumes with the
  old session, identical snapshot bytes and the old digest. The differing
  upstream listing is logged, without a `toolbox.changed` event.
- Explicit resumed restart retains the session and fingerprint, refreshes the
  description and digest, and emits exactly one `toolbox.changed`.
- The fresh launch record contains Claude `--resume <id>`, Codex `resume <id>`
  or pi `--session-id <id>`, as appropriate.

On unchanged production source `52c502f9`, all three tests failed at the
unchanged-digest assertion. With the fix, all three pass, including the later
resume-argument assertions. These are dispatch and pinning regressions; native
model-visible catalog acceptance is a separate check.

## Validation

Evidence is retained under `target/toolbox-t14/` in graph's worktree.

| Gate | Result | Log |
| --- | --- | --- |
| Before control, three runtime dispatch regressions | 0 passed, 3 failed at unchanged digest | `before.log` |
| Fixed dispatch regressions | 3 passed | `after.log` |
| Added native resume-argument assertions | 3 passed | `argv-after.log` |
| Locked workspace, sandbox required | 1,304 passed, 1 timing failure, 4 ignored | `workspace.log` |
| Isolated unchanged web P16 timing gate | Passed, p95 115.558 microseconds against 1 millisecond | `web-p16-rerun.log` |
| Strict workspace Clippy, all targets | Passed | `clippy.log` |
| Formatting | Passed | `fmt.log` |
| Rust 1.82 default CLI build and help | Passed | `msrv.log` |

The workspace invocation hit a web P16 timing flake: round-trip p95 was
3.840222 milliseconds. The isolated rerun passed without changing its threshold
or source. Both results are retained. Included workspace checks passed: toolbox launch 11, proxy 15,
team 89 (including T20), keep-alive 8, and the immutable subagent inheritance
regressions. The four ignored cases are the existing explicit benchmark,
paid/native or interactive gates; no new ignore was added.

QA's receipt-aware native replay uses harness `9cb66ef0` on `3d31804b` and the
verified candidate binary below. Codex 0.159.0 and pi 1.1.0 pass T14 and T18.
Claude Code 2.1.291 passes T18, but T14 still fails its model-visible catalog
check: Crew refreshes the pin and digest and emits exactly one changed event,
while the resumed Claude session sends the old tool descriptions. QA reported
these results in `m-859ff5fb0ca1`; native reports remain under
`target/toolboxes/fix-native-*/` in QA's worktree.

The dispatch fix is approved (`m-24f77d1c3eda`, `m-9fb99cb1b70c`) and landed.
The orchestrator split the remaining Claude cache/discovery issue into
`toolbox-t14-claude` (`m-0387e7257fee`). This report does not claim full Claude
T14 acceptance. That follow-up must establish a supported refresh mechanism
and report any conversation-continuity or cache cost.

## Reproduction

Use a short absolute target path for the Unix-socket fixtures and provide
`socat`, bubblewrap, a C compiler and pkg-config. The commands used here were:

```sh
export CARGO_TARGET_DIR="$PWD/target/t"
export CARGO_BUILD_JOBS=4
nix shell nixpkgs#socat nixpkgs#gcc nixpkgs#pkg-config --command \
  env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH -u CREW_PI \
  CREW_TEST_SANDBOX=require cargo test --locked --workspace --no-fail-fast
nix shell nixpkgs#socat nixpkgs#gcc nixpkgs#pkg-config --command \
  env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH -u CREW_PI \
  CREW_TEST_SANDBOX=require cargo test --locked -p crew --test toolbox_launch \
  resumed_restart_refreshes_mcp_catalog -- --nocapture
nix shell nixpkgs#socat nixpkgs#gcc nixpkgs#pkg-config --command \
  env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH -u CREW_PI \
  CREW_TEST_SANDBOX=require cargo test --locked -p crew --test web \
  a_frame_crosses_the_server_within_a_millisecond_at_p95 -- --exact --nocapture
./scripts/build-env cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
CREW_MSRV_TARGET_DIR="$PWD/target/msrv" ./scripts/check-msrv
```

The native replay candidate is a stable copy of the production `4adfa536` build
at `target/toolbox-t14/crew-4adfa536`, SHA-256
`5bfdffac042c41a8c4252f1cc23fa2404979903853a70fdc94d404d1687c50b1`.
`ff9eb12d` changes tests only. Native runs use disposable homes, dummy credentials
and localhost provider stubs; they do not measure provider cache usage or cost.
