# Linux confirmation and terminal focus details

Task `native-linux-polish-v1`: design items V1, V6 and V7. This directory holds
the transcript and captures of a passing run of
`crates/crew-desktop/tests/run-window-test.sh --interaction`, the real app
driven with real keys against a private scripted team. After each step the run
checks focus over the app's own accessibility bus; any failed check fails it
([atspi-keyboard.txt](atspi-keyboard.txt), tools in
[tool-versions.txt](tool-versions.txt)).

- V1: while the Stop team confirmation is open, Stop team… is unavailable and
  the page shows no "this action is already pending" reason
  ([04-stop-team-dialog.png](04-stop-team-dialog.png)). After Escape, focus is
  on Stop team… again. Kill… and Restart fresh… return focus to the Actions menu
  ([04-kill-dialog.png](04-kill-dialog.png),
  [04-restart-fresh-dialog.png](04-restart-fresh-dialog.png)).
- V6: after Attach terminal, focus is on the terminal. Its accessible
  description, which a screen reader reads on reaching it, is "Attached. F6
  leaves the terminal." ([06-terminal-attached.png](06-terminal-attached.png)).
  An attachment the operator did not ask for in this panel does not move focus.
- V7: Return on Detach terminal puts focus back on Attach terminal
  ([08-after-detach.png](08-after-detach.png)).

Restart after limit needs an agent paused at a usage limit, which a scripted
team cannot reach. The controller suite (`--client`) checks it on the actual
controls: no pending reason while its confirmation is open, and focus back on
Restart after limit once Core reports it available.

The announcement is the terminal's accessible description, not a live
announcement: `gtk_accessible_announce` needs GTK 4.14 and the app is built
against the GTK 4.6 API.
