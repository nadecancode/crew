# Native validation storage recovery

Task: `validation-storage-recovery`, 2026-10-05 (America/Chicago).
Machine-readable evidence: `storage-recovery.json` alongside this report.

The isolated temporary-directory/Git probe passes after owner-scoped cache
cleanup. The latest recorded sample has 180,832,505,856 bytes available
(168.4 GiB); inode use remains 12%. No additional cleanup is needed.

## Original failure and limits of attribution

Runtime's retained `target/pty-lifecycle/native-gateway.log` spans
20:49:19–20:49:33 CDT (01:49:19–01:49:33 UTC on October 6). Seventeen tests
passed; these two failed during fixture setup, before agents started:

- `enabled::explicit_terminal_grant_echoes_then_grant_revoke_closes_attachment`:
  `git init -q -b main` failed at `/tmp/crew-t4SrEus/ws/.git`.
- `enabled::explicit_control_messages_are_allowed_but_do_not_grant_terminal`:
  the same command failed at `/tmp/crew-tzKrNg5/ws/.git`.

Both report `No space left on device`. The exact error instant was not
timestamped. Runtime reported approximately 4 GiB available after fixture
teardown, with 12% inode use. This supports transient block pressure; no
free-space sample at the error establishes literal zero available bytes.

`/tmp`, the workspace and `/nix/store` share `/dev/nvme1n1p2` (ext4). Current
kernel mount options include `noquota`. The observed evidence does not support
inode exhaustion or a filesystem quota as the cause. Rounded `df` output at
100% must not be interpreted as an exact zero-byte reading.

The original log is preserved, with SHA-256
`171a6cd57d4af12d80976cd584c4aedd02c96c6264511be99c703ad8ba4316fd`.
Runtime's subsequent unchanged gateway run passed 19/19 after its own cleanup;
the recovered log spans 20:54:28–20:54:58 CDT and has SHA-256
`1682ee614a459d6092dc02921e3017695f80e66d338848768213093f2fe4139d`.
Copies and file timestamps are retained under
`target/native-validation/storage-recovery/` in the content worktree.

## Cleanup and preservation

Each owner removed its own quiescent, reproducible caches. No global cache,
unattributed `/tmp` directory, other project, root history or service was changed.

| Owner | Removed paths under its own worktree | Evidence |
| --- | --- | --- |
| Runtime | `target/debug/incremental` | Owner reports approximately 13 GiB; exact bytes were not recorded. Binaries, source and logs retained. |
| QA | `target/ci/debug/incremental`, `target/native-validation/cargo/debug/incremental` | Owner reports approximately 11 GiB; exact bytes were not recorded. `gateway-integration/cache-recovery.json` retained. |
| Reviewer | 49 obsolete Cargo target directories and three attributed GTK scratch extracts | Owner reports approximately 136 GiB plus 3.3 GiB; exact directory list is in the JSON. Sixteen logs, source archives and memory files retained. |
| Content | Three incremental directories listed below | Exactly 7,373,410,304 allocated bytes reclaimed; 22,762 directory entries removed. |

Content removed only:

```text
target/debug/incremental
target/native-validation/cargo/debug/incremental
target/wasm32-unknown-unknown/debug/incremental
```

All three resolved inside this worktree, contained no symlinks and had no open
process handles. No compiler process was running. The newest cache modification
preceded cleanup by more than two hours. Counts account for repeated hard links:
29 inodes also linked outside these directories were retained through those
links. The measured available-space increase exactly matched the fully
attributed allocated bytes; free inodes increased by 13,814.

Before and after content cleanup, 509 retained logs, captures and other evidence
files matched their recorded SHA-256 hashes. Source, binaries, toolchains,
accepted artifacts and recovery backups were preserved. Remaining QA, graph and
scheduler cache candidates were left in place because headroom was sufficient.

Owner cleanup measurements have different times and precision. They must not be
summed and presented as one exact physical-space delta. Unrelated host writes
continue to change the live available-space reading.

## Resource probe and restart rule

The small probe removed every inherited `GIT_*`, `CREW_*` and `HERDR_*` variable,
created its own `/tmp/crew-content-storage-probe-TUpi0R/repo`, then ran Git
init/add/commit/show/fsck and checked a clean fixture status. It also wrote and
read back a 1 MiB buffer. Every command passed. Identity was supplied per Git
command; no operator-root sentinel or global Git configuration was used.
The tiny probe directory is retained for review.

Recommended restart policy: one heavy validation build at a time initially,
`CARGO_INCREMENTAL=0`, and reuse each owner's existing targets per toolchain and
profile. Pause new heavy builds if available space drops below 100 GiB. Preserve
logs and accepted evidence before selecting any further disposable cache.

The orchestrator released the operational pause at 21:10 CDT
(`m-d82b42e19775`, `m-1a27ff9e9ef2`), with QA taking the first heavy slot.
Both original failed tests explicitly pass at lines 24 and 25 of the retained
post-recovery 19/19 log. The orchestrator authorized reusing that result; the
prepared two-case rerun was canceled. No unchanged 2/19/60-test suite was
repeated for bookkeeping. Subsequent heavy slots remain coordinated, with the
100 GiB floor and incremental compilation disabled.
