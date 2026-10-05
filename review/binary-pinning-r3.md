# Binary pinning re-review: one change requested

Reviewed `02d07798`, including `c8901f1`. The five regressions from the
second review now pass, as do the author's cancellation and startup-warning
cases. The stop lock preserves the hold while a stop is pending or
cancelled, prevents the housekeeping sweep from racing an operator stop,
and prevents a late stop response from erasing a replacement. A cached pin
replaced at startup is now reported and its surviving agents restarted.

The independent workspace suite passed 944 tests with zero failures and
two ignored. The focused `binary` run passed 31 tests, and the required
sandbox jj run of `binary_pin` passed all four integration cases. Clippy
with warnings denied and formatting passed, including the new test.
One B4 gap remains.

## P2: A pin recreated after deletion still qualifies for adoption

`keep` records `replaced` only when a file exists under the pin's name.
When that file is missing, it creates the copy but returns `replaced =
false`. `Binary::launched_this` therefore still permits adoption of a
surviving agent recorded with that name. The new unit assertion for
`made_anew.launched_this(Some(&name))` explicitly preserves this behavior.

B4 requires the copy to have been found intact at this start, not merely
to exist after startup repairs it. The architect confirmed this in
`m-a14cdc66ba92` and made the missing-file case explicit in `31a18c87`.
While the pin was absent, surviving sessions' hooks and MCP launches could
have failed with `ENOENT`; recreating the executable does not restore
those sessions' tools. Restart them to establish working tools again.

Retain whether the startup pin was actually reused after comparison, and
require that outcome for adoption. Keep ordinary intact-cache adoption.

`reviewer_only_a_pin_found_intact_at_start_can_adopt_a_surviving_agent`
creates a valid pin before startup and tests two cases with the same
surviving-agent record. Keeping the pin passes the adoption control.
Deleting it before startup recreates the path correctly, but recovery
still adopts the survivor: no kill is recorded instead of the expected
restart.

## Validation

```
./scripts/build-env cargo test -p crew --lib reviewer_only_a_pin_found_intact
```

This regression fails on `02d07798` after its intact-cache control passes.
Only test code changed. Logs in the reviewer worktree:

- `notes/reviewer-binary-pin-r3-regressions.log`: 31 focused tests pass.
- `notes/reviewer-binary-pin-r3-workspace.log`: 944 pass, two ignored.
- `notes/reviewer-binary-pin-r3-missing.log`: the new regression fails.
- `notes/reviewer-binary-pin-r3-sandbox.log`: four required-sandbox pin
  integration tests pass.
- `notes/reviewer-binary-pin-r3-clippy.log` and
  `notes/reviewer-binary-pin-r3-fmt.log`: checks pass.

Source re-review is complete; the missing-cache adoption rule is the only
remaining finding.
