# Binary pinning re-review: changes requested

Reviewed `399a9ca2`, including the fixes in `aac94ef`, against the original
review and the architect's clarification in `539e94a6`. The original four
regressions pass. The unpinned adoption check, execute permissions, startup
byte comparison and change-time check are corrected. Recovery now retains
the old process and copy after an ordinary failed stop, but the protection
does not survive concurrent operations.

The independent baseline workspace suite passed 937 tests with zero
failures and two ignored. Required-sandbox jj integration passed all 33
`binary_pin` and `sandbox` tests. Clippy with warnings denied and formatting
passed, including the new tests. Five further regressions reproduce the
three findings below. These additions change only the test backend and
tests.

## P2: The hold is released before the old process has stopped

`kill_process` and `stop_other_builds` take `other_build` before awaiting
the backend's stop. During that wait, `binary_housekeeping` can observe no
held processes, sweep the old copy and disable further sweep retries. An
operator stop that eventually fails therefore still loses the executable
of the surviving process. The same temporary release lets `on_exit` act on
a backend connection-loss event and clear the process record while the
stop remains unresolved.

The initial recovery stop has the same exposure: it sets `other_build`
only after the stop fails. Process events are already running beside
`recover`, so a backend event received during that first stop can discard
the PID before the hold is established.

Keep the recovery obligation visible until the old process has been
confirmed stopped. Track a stop in progress separately from that
obligation, including initial recovery, and ensure cancellation does not
silently release it. A process still being stopped must count against
sweeping and remain protected from uncertain backend exit reports.

Reproductions in `daemon/supervisor/binary_tests.rs`:

- `reviewer_an_inflight_operator_stop_keeps_the_old_binary_pinned`: the
  housekeeping tick deletes the old copy while the operator's failing
  stop is waiting.
- `reviewer_a_failed_stop_retry_keeps_its_record_during_backend_events`:
  a connection-loss event during the retry changes the surviving PID
  from `Some(12345)` to `None`.
- `reviewer_initial_recovery_keeps_its_record_during_a_failed_stop`:
  the same record loss occurs during the first recovery stop.

## P2: An obsolete retry clears the replacement's process record

In `stop_other_builds`, the successful retry clears `pid` and `backend_ref`
before checking whether a later stop or restart has taken over. An
operator restart can finish its own stop and launch a replacement while
the housekeeping retry is still waiting for the earlier backend reply.
When that reply arrives, the retry erases the replacement's identity and
only then notices that its stop counter is obsolete.

Check ownership before changing the process record, and make validation
and mutation consistent with concurrent replacement. The failure path
must likewise not restore a hold onto a newer process because an obsolete
stop failed.

`reviewer_a_late_stop_retry_does_not_clear_the_replacements_record` starts
the retry, completes a later operator restart, and releases the retry's
response. The replacement's `(Some(23456), Some("new-pane"))` becomes
`(None, None)`. The backend still considers the replacement alive. The
test also permits a fix that serializes the stops.

## P2: Repairing a cached pin still adopts its surviving agents by name

`keep` now compares the cached bytes with the running executable, but its
return value retains no indication that the cached file was replaced.
`Binary::launched_this` therefore still adopts any agent recorded with
the matching name, even when that name held another build at this start.
An existing MCP process or sandbox may still hold the old inode; repairing
the path does not establish what that surviving process is running.

The architect explicitly requires restarting these agents in B4 of
`539e94a6`, confirmed in `m-76389b37ae2e`. That amendment also requires a
warning when startup replaces a file that was not this build. Startup
currently emits no such warning. Retain the startup validation outcome,
use it to decide recovery eligibility, and report the repair.

`reviewer_replacing_a_cached_pin_restarts_its_old_agents_and_reports_the_repair`
places different bytes in a previously created pin, starts the daemon,
and presents a surviving agent recorded with that pin's name. The path is
repaired, but the kill list remains empty and the agent is adopted. The
same test checks the startup warning once the adoption assertion passes.

## Validation

The baseline log is `notes/reviewer-binary-pin-recheck-workspace.log`.
The original regressions passed in
`notes/reviewer-binary-pin-recheck-regressions.log`. The new cases fail in
`notes/reviewer-binary-pin-recheck-races.log`. A final run of `binary_tests`
with the backend modeling a delayed response for the old process also
fails exactly these five cases and passes its other nine tests;
`notes/reviewer-binary-pin-recheck-races-final.log` records that run.
Sandbox, Clippy and formatting logs use the same prefix followed by
`sandbox`, `clippy` and `fmt`.

Run the reviewer regressions with:

```
./scripts/build-env cargo test -p crew --lib reviewer_
```

On this revision, ten existing reviewer regressions pass and these five
new cases fail. No production code was changed by the review.
