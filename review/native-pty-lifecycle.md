# Terminal stream closure after process exit

Task `native-pty-lifecycle`. Before this correction, `PtyProc` retained a
strong broadcast sender in the process map after EOF and exit. The daemon
could report a confirmed kill as Stopped with no PID while its attachment
receiver waited forever. The existing handler emits `Push::PtyClosed` only
when that receiver closes. Both affected production files were byte-equal to
reviewed main `e27cb12d` before work began.

The production change is confined to `crates/crew/src/process/pty.rs`. Its
sender now occupies a shared `Mutex<Option<Sender>>`. The reader borrows it
only while publishing each chunk, with no persistent sender clone. EOF takes
and drops the sender after the final fan-out and before drain completion. The
waiter also closes it after the unchanged 500 ms `EXIT_DRAIN`, before emitting
Exited. These operations are idempotent. Existing receivers drain queued
bytes and then observe Closed even if a descendant retains the PTY. Bytes
read after that cutoff still update the retained parser/screen.

Attaching to an ended process returns its retained screen snapshot and an
already-closed receiver. Resize becomes a no-op after exit or sender closure,
so the handler's resize-before-replay path does not change the retained
screen. Live resizing and input keep their existing behavior; input to an
exited process still fails. Process spawning, signals, group retirement,
supervision and wire types are unchanged. There is no client inference from
AgentStatus and no handler, web, TUI, Core or driver workaround.

Three new unit cases join the thirteen unchanged PTY cases. They cover
natural exit and kill, two attached receivers receiving final bytes, restart
isolation, retained replay after resize, the EOF-before-alive=false state,
and a descendant holding the PTY beyond the drain bound. The descendant
waits on a test-controlled file: its late output is released only after the
test checks attachment closure, then must appear on the retained screen.
Cleanup releases it even when the old sender behavior fails the assertion.

The new actual daemon/client regression runs a scripted agent on a real PTY.
It attaches, resizes, observes terminal input echoed into the retained screen,
and issues a real operator Kill. After verifying Stopped and no PID, it
requires the existing `Push::PtyClosed`; that agent state is evidence of the
kill, not a substitute for terminal EOF. Late attach must replay the retained
marker and then end. Input to that ended process remains refused. Native
Core confirmation policy is exercised separately by the unchanged K5
consumer fixture `d06db944`, not claimed by this raw daemon test.

Before-controls against the original production code fail:

- The new unit drains final queued bytes, then sees an empty live channel
  instead of Closed (`target/pty-lifecycle/before-unit.log`).
- The real confirmed-kill handler test reaches Stopped/no PID, then times out
  awaiting `Push::PtyClosed` (`target/pty-lifecycle/before-handler.log`).

Corrected checks on this Linux host:

- Complete PTY unit suite: 16/16, including all thirteen existing cases.
- Actual daemon/client lifecycle regression: 1/1.
- Complete real TLS/daemon gateway suite: 19/19.
- Unchanged web, web_server, web_wire and operator_auth suites: 60/60.
- All-target clippy with `-D warnings`, native-gateway enabled and disabled.
- `cargo fmt --all --check`.
- Actual Rust/Cargo 1.82.0 locked default CLI build and help checks.

The first broader gateway run passed seventeen cases and failed two during
fixture `git init` with ENOSPC, before agents started. The log is retained as
`target/pty-lifecycle/native-gateway.log`. Only this worktree's reproducible
`target/debug/incremental` cache was removed; source, logs, binaries, backups,
accepted artifacts and other worktrees were preserved. After storage recovery,
the complete gateway run passed 19/19; its log is
`target/pty-lifecycle/native-gateway-recovered.log`. Subsequent checks disabled
incremental compilation to limit cache regrowth.

All test invocations remove inherited `GIT_*`, `CREW_*` and `HERDR_*`.
Daemons, scripted workers, registry/state/config paths, and any TLS listeners
belong to isolated temporary fixtures. The compatibility check reuses the
scheduler's read-only 1.82.0 toolchain with this worktree's target directory.
No root repository, active operator service, real device or installed binary
is changed. Cargo manifests/lockfile, native/v6 records, existing PTY cases
and legacy web/auth test sources remain unchanged. Apple verification stays
deferred by operator.

Focused reproduction commands, run with the environment isolation above:

```sh
./scripts/build-env cargo test --locked -p crew --lib process::pty::tests::
./scripts/build-env cargo test --locked -p crew --test pty_lifecycle
./scripts/build-env cargo test --locked -p crew --features native-gateway --test native_gateway
```

Consumer acceptance, using an explicit feature-built fixture binary and the
reviewed transport fixture graph:

```sh
CREW_NATIVE_VALIDATION_BIN=/absolute/path/to/fixture/crew ./scripts/build-env cargo test --locked -p crew-client-transport --features journey-tests --test gateway_daemon journey::real_local -- --nocapture
```
