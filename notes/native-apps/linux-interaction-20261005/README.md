# Linux app keyboard and focus journey

Task `native-linux-interaction` (design precheck findings G1, G2, G3, G7, G9,
G14, G15 and G17, and acceptance findings L09, L11 and L15). This directory
holds the transcript and captures of one passing run of
`crates/crew-desktop/tests/run-window-test.sh --interaction`. The run uses the
real GTK app against a private daemon running the scripted team in
`crates/crew-desktop/tests/interaction/team.yaml.in`, on its own Xvfb display
with Openbox and its own session and accessibility buses. It sends real keys
with xdotool and, after each step, checks what the app reports as focused over
AT-SPI. The checks are in `tests/interaction/drive.sh`; any failed check fails
the run. Tool versions are listed in [tool-versions.txt](tool-versions.txt).

[atspi-keyboard.txt](atspi-keyboard.txt) covers these steps:

- Startup, disconnected: focus is on Choose workspace, which opens Connections.
  After connecting, focus is on the connected page's first control.
- G2: the sidebar is one Tab stop. Up and Down move focus between destinations
  without changing the destination; Enter opens the focused one.
- G9: Tab on the Team page visits each control once (no labels, no flow box
  cells).
- G3: Stop team… is opened from the keyboard and is unavailable while its
  confirmation is open. After Escape, focus is on Stop team… again. Kill… and
  Restart fresh…, chosen from the Actions menu, return focus to Actions.
- G14: New message has a visible To label on a To selector, and its editor is
  named Message.
- G15: once a terminal is attached, focus moves on from Attach. The terminal's
  first screen is in its accessible text before any new output. Tab goes to the
  agent, F6 leaves the terminal for Detach, and Detach hands focus on. Shortcut
  help lists F6.

G1 (non-selectable row labels) and G7 (fixed sidebar width) are checked in the
window and controller suites. So are the Restart after limit confirmation,
which needs an agent paused at a usage limit, and the edge cases (an opener
that has gone, a user who moved focus before Core settled, one answer per
confirmation). This is a keyboard journey through AT-SPI, not a screen reader
run.
