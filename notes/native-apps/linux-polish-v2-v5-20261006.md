# Linux app layout follow-ups V2–V5, V8

Task `native-linux-polish-v2-v5`: the designer's non-blocking items V2–V5
(visual check of `5ef3ed75`) and V8 (check of the composite `5b0359c9`).
V1, V6 and V7 are scheduler's [native-linux-polish-v1](linux-polish-v1-20261006/README.md).

## What changed

| Item | Change |
| --- | --- |
| V2 | The speck right of "Live" was not a widget: it was the top pixel row of "Not connected", left behind by the Cairo renderer when the status line changed (a full repaint of the window removed it). The sidebar's status and result lines are now as wide as their text and paint the sidebar colour behind them, so a text change repaints the old line's whole box. The result line is hidden while it is empty. |
| V3 | Inspector tabs stay on one line. Tabs that do not fit move into a More menu at the end of the row; the selected tab always stays in the row, and a tab chosen from the menu takes its place there and takes focus. Tabs carry the tab role and selected state for assistive technology. At 1120 pixels Terminal and Host screen are in More; at 560 all six tabs fit. |
| V4 | The agent and task inspectors show their own title, so the pane heading ("Agent claude-1", "Task queue") appears only as the compact layout's page title. Other details (evidence, no selection) keep it. |
| V5 | A task's note and result are rows of its facts grid. |
| V8 | The terminal tab's status line and Copy/Detach row keep the 12/24-pixel inset of the other tabs; the terminal itself stays edge to edge. |
| Size warnings | The fixed-width sidebar measured its height at whatever wider width GTK proposed, where the status text wraps into fewer lines, but at its fixed width otherwise. GTK warned "Widget reports min height of 135 for width of 303, but overall min height of 150" (widths 213 to 926) whenever the status or result line wrapped. Heights are now measured at the fixed width. |

## Evidence

App source: `agents/content` 9c0f9b34 (merge of main `1e3b8a37`). "Before" is
main `59740228`'s binary in the same harness, the same scripted team and the
same steps; binaries in [before](linux-polish-v2-v5-20261006/before-tool-versions.txt)
and [after](linux-polish-v2-v5-20261006/after-tool-versions.txt). Captures are the window
of a private Xvfb 1440×960 session at 96 DPI with Openbox and the Cairo renderer,
as in [content-polish-20261006](content-polish-20261006.md); black popover shadows
and Openbox title bars come from the capture setup.

| Capture | Before light | Before dark | After light | After dark |
| --- | --- | --- | --- | --- |
| Team, sidebar status (V2) | [01](linux-polish-v2-v5-20261006/before-light-01-team.png) | [01](linux-polish-v2-v5-20261006/before-dark-01-team.png) | [01](linux-polish-v2-v5-20261006/after-light-01-team.png) | [01](linux-polish-v2-v5-20261006/after-dark-01-team.png) |
| Agent inspector at 1120 (V3, V4) | [02](linux-polish-v2-v5-20261006/before-light-02-agent-inspector.png) | [02](linux-polish-v2-v5-20261006/before-dark-02-agent-inspector.png) | [02](linux-polish-v2-v5-20261006/after-light-02-agent-inspector.png) | [02](linux-polish-v2-v5-20261006/after-dark-02-agent-inspector.png) |
| More menu (V3) | | | [03](linux-polish-v2-v5-20261006/after-light-03-more-menu.png) | [03](linux-polish-v2-v5-20261006/after-dark-03-more-menu.png) |
| Host screen selected (V3) | [04](linux-polish-v2-v5-20261006/before-light-04-host-screen.png) | [04](linux-polish-v2-v5-20261006/before-dark-04-host-screen.png) | [04](linux-polish-v2-v5-20261006/after-light-04-host-screen-chosen.png) | [04](linux-polish-v2-v5-20261006/after-dark-04-host-screen-chosen.png) |
| Terminal attached (V8) | [05](linux-polish-v2-v5-20261006/before-light-05-terminal-attached.png) | [05](linux-polish-v2-v5-20261006/before-dark-05-terminal-attached.png) | [05](linux-polish-v2-v5-20261006/after-light-05-terminal-attached.png) | [05](linux-polish-v2-v5-20261006/after-dark-05-terminal-attached.png) |
| Task inspector (V4, V5) | [06](linux-polish-v2-v5-20261006/before-light-06-task-inspector.png) | [06](linux-polish-v2-v5-20261006/before-dark-06-task-inspector.png) | [06](linux-polish-v2-v5-20261006/after-light-06-task-inspector.png) | [06](linux-polish-v2-v5-20261006/after-dark-06-task-inspector.png) |
| 560-pixel detail page (V3, V4) | [07](linux-polish-v2-v5-20261006/before-light-07-narrow-560-detail.png) | [07](linux-polish-v2-v5-20261006/before-dark-07-narrow-560-detail.png) | [07](linux-polish-v2-v5-20261006/after-light-07-narrow-560-detail.png) | [07](linux-polish-v2-v5-20261006/after-dark-07-narrow-560-detail.png) |

V2 in detail: [before](linux-polish-v2-v5-20261006/speck-before.png) and
[after](linux-polish-v2-v5-20261006/speck-after.png), 4× zoom of the status
area. [speck.sh](linux-polish-v2-v5-20261006/speck.sh) connects a freshly
started app, captures, then forces a full repaint (a 4-pixel resize and back)
and captures again. With this source the status area is identical in both
captures (0 differing pixels, 2 of 2 runs). A build of the first follow-up
commit (f71e9a4f) without the status-line background leaves an 89×1-pixel row
of the old text, which the repaint removes.

Assistive technology, from the capture runs: inspector tabs are `page tab`
objects with `selected` on the current one
([1120](linux-polish-v2-v5-20261006/after-light-tabs-1120.txt),
[560](linux-polish-v2-v5-20261006/after-light-tabs-560.txt)). A tab moved
into More is unmapped, and GTK then leaves it out of the accessibility tree,
as it did for the notebook's scrolled-out tabs; the More button ("More
sections") offers it instead. After choosing Host screen from More, focus is
on the Host screen tab ([focus](linux-polish-v2-v5-20261006/after-light-focus-after-more.txt)).

## Checks

On an export of 9c0f9b34, with `NATIVE_NIXPKGS` pinned and the harness tools
on `PATH`, `crates/crew-desktop/tests/run-window-test.sh` passes in every mode:
window 1/1, `--client` 7/7, `--gateway` 1/1, `--keyboard`, and `--interaction`
([transcript](linux-polish-v2-v5-20261006/interaction-atspi-keyboard.txt)).
Strict Clippy (`-D warnings`, all targets) for `native-ui,local`,
`native-ui,local,gateway` and `native-ui,gateway-tests`; `cargo fmt --check`
clean.

The in-process GTK test (`--client`) checks each follow-up: the sidebar needs
no less height for any proposed width than for none, and an empty result line
is hidden (V2); one tab row at 320 pixels with Host screen in More, the
selected tab moving into the row, More offering exactly the tabs out of the
row, focus on the tab chosen there, and no More at 1000 pixels (V3); the pane
heading hidden in the split layout for an inspector and shown for other
details (V4); a task's result in the same grid as its other facts (V5). With
the sidebar measure reverted, the overflow rule keeping the selected tab
removed, or the heading always shown, the test fails.

GTK size warnings: the capture session logs 8 on main and none with this
source. `--client` logs 11 on main `1e3b8a37` and 6 here: the sidebar's are
gone; the remaining "min height of 362 for width of 926, but min width of 750
for height of 266" family comes from the live-daemon part of that test and is
also on main. `--gateway` logs only the Connections dialog's "min height of
336 for width of 592, but overall min height of 398", which predates this work;
the smaller ones (widths 213–303) are gone. Asking the adaptive layout's
minimum width for no particular height (af246f08) did not remove that family
and made GTK refuse an allocation ("Allocation height too small. Tried to
allocate 872x544, but GtkBox needs at least 872x555"), so 9c0f9b34 reverts
it and says why in the code.

Seen in passing, not changed here: after Detach terminal the sidebar result
line reads "Action pending · awaiting the daemon's result" and stays, on main
too ([before 06](linux-polish-v2-v5-20261006/before-light-06-task-inspector.png)).

Reproduce: build `crew-desktop` (`native-ui,local`) and `crew` into an owned
target, then `start.sh OUT /tmp/cdp-<name> APP CREW` and, once ready,
`x.sh OUT ./capture-v.sh light` (or `dark`), `x.sh OUT ./capture-before.sh light`
for main's binary, and `x.sh OUT ./speck.sh` on a freshly started app. All
scripts are in [linux-polish-v2-v5-20261006/](linux-polish-v2-v5-20261006/).
