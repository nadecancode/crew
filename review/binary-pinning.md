# Binary pinning review: changes requested

Reviewed `bbf528959a8f2a0d0b46d57565da9a304a705983` against
`notes/binary-pinning.md`. The independent baseline workspace suite passed
927 tests with zero failures and two ignored. The required-sandbox jj run
of `binary_pin` and `sandbox` passed 33 tests. Clippy with warnings denied
and formatting passed, including the reviewer tests below.

The four new reproductions fail on this revision. The startup-cache
requirement has also been put to the architect for clarification; the
other three findings do not depend on that answer.

## P2: Recovery forgets an old process after failing to stop it

In `daemon/supervisor/recovery.rs`, an adopted process with another build's
pin is killed before replacement. If `ProcessManager::kill` fails,
recovery only logs the error, clears its PID and backend reference, and
schedules a replacement anyway. `recover()` then returns `true`, so its
caller sweeps old pins despite the old process potentially still running.

A temporary backend failure during an upgrade can therefore leave a live
agent with its old tool path deleted and its process no longer represented
correctly by the daemon. Preserve the surviving process and old pin, and
do not launch a replacement until retirement is confirmed. A failed kill
must also prevent the recovery result from authorizing the sweep; herdr's
optimistic local liveness flag is not proof that a failed close succeeded.

`reviewer_recovery_does_not_forget_an_old_process_whose_kill_failed`
uses the existing recorded backend with a failing kill. It remains alive,
but the daemon has already cleared its PID and backend reference and set
the agent to `Spawning`.

## P2: Two missing pin names are treated as the same build

The adoption check compares `launched_with.as_deref()` directly with
`self.binary.name()`. When a real pin attempt failed and the surviving
agent was unpinned or predates pinning, both values are `None`, so recovery
adopts it. There is no evidence that its existing MCP process or hooks
belong to the newly started daemon's build.

This defeats the recovery rule on the explicitly supported unpinned path:
after upgrading and restarting the daemon, a surviving old CLI can still
retain tools of the old protocol. Restart unknown-build processes when a
real daemon could not pin. Preserve any necessary in-process test behavior
through the distinct `Given` mode, rather than treating missing names as
proof of equality.

`reviewer_unpinned_recovery_does_not_treat_two_missing_build_ids_as_a_match`
starts a daemon whose pin attempt fails because `bin` is a file. The old
agent has no pin recorded. Recovery adopts it without any kill attempt.

## P2: A pin without execute permission passes the repair check

`Binary::check` uses `intact`, which checks group/other write bits and the
recorded device, inode, size and mtime. Removing the execute bit changes
none of those identity fields, so a mode-0400 pin is accepted by both the
launch check and housekeeping. Agent tools then fail to execute even
though the daemon reports that the copy is intact.

`reviewer_a_copy_without_execute_permission_is_repaired_before_launch`
changes only the mode of the existing copy. `check()` incorrectly returns
`Ok(None)`. Validate the permissions needed to execute the copy and repair
an unusable copy before handing it to a launch.

## P2: Startup accepts different bytes under the cached pin's name

`keep` accepts an existing pin when it has the expected size, owner,
mode 0500 and one link. Those properties do not establish that it contains
the running build. It then records the replacement's identity as trusted,
so the later sandbox identity check also accepts that file.

`reviewer_a_same_size_replacement_is_not_accepted_as_the_running_build_at_restart`
replaces a previous daemon's cached copy with different bytes of the same
size and mode. Starting the same build again returns the replacement
unchanged instead of the running binary. Validate the prior copy's
provenance or contents while retaining unchanged valid copies. The
architect was asked to confirm the intended startup reuse criterion in
B3/B6 (`m-88140ec953e5`).

## Coverage and reproduction

The source review covered pin creation and repair, launch propagation,
recovery and sweeping, sandbox identity checks, mismatch reporting through
the CLI/MCP/hooks, and fallback scheduling/return behavior. No additional
finding was identified in B8 or the fallback scheduling changes. Moving
the actual copy into `daemon::start` under the workspace lock, while only
the foreground CLI opts into it, preserves B5's in-process-test behavior.

Run the added cases with:

```
./scripts/build-env cargo test -p crew --lib reviewer_
```

This currently reports four new failures and six older passing reviewer
regressions. The additions are test-only. Logs are
`notes/reviewer-binary-pin-{workspace,regressions,sandbox,clippy}.log` in
the reviewer worktree.
