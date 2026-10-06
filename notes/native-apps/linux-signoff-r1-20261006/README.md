# Linux status after detaching a terminal

Task `native-linux-signoff-r1`, sign-off finding R1: after Detach terminal, or
after leaving an attached terminal by selecting another agent, the window
status kept saying "Action pending · awaiting the daemon's result" until the
next Attach.

## Cause

The controller set that status for every dispatched action except reads and
terminal writes. A detach has no daemon command. Core closes it locally and
reports `ClosedLocally`, and the controller deliberately leaves the status
unchanged for that outcome. So nothing ever replaced the pending line.

## Fix

A detach is now quiet, like terminal input and resize
(`Controller::planned`): it does not set the pending status. The terminal
already shows "Detached · read-only". Selecting another agent goes through the
same detach.

## Evidence

- `--client` regression: after a detach is planned and closed locally, the
  status is unchanged and the operation is finished. This fails without the
  fix.
- `--interaction` checks the status 2 s after Detach. Three runs in a row
  passed ([interaction-runs.txt](interaction-runs.txt)). The transcript is
  [atspi-keyboard.txt](atspi-keyboard.txt), and
  [08b-status-after-detach.png](08b-status-after-detach.png) shows the window
  at that point: the status reads "Action acknowledged by the daemon." (from
  the Attach) and the terminal reads "Detached · read-only".
- Focus after Detach is unchanged (V7): it goes back to Attach terminal
  ([08-after-detach.png](08-after-detach.png)).
