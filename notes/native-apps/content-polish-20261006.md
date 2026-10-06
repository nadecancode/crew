# Linux content views: visual system

Task: `native-linux-content-polish`. Findings G5, G6, G8, G10, G11, G12, G13
and G16 from the [design precheck](design-precheck-20261005.md) (commit
`e82b6540`), as settled with the designer: G10 shows no reason line until a
field has had input and then shows the Core text as is. The precheck
captures are the "before" set; the captures here are the "after" set, light
and dark, from the same kind of private session.

## What changed

| Finding | Change |
| --- | --- |
| G5 | Team header: project title, muted path, objective, counts, "<asker> asked a question" with Answer (hidden at zero), warnings with an icon in warning colour, and a "Last known" chip while not live. Rows lead with identity and end with a status chip (text and semantic colour); role · runtime, current work with progress, waiting cards, warnings. Inspector: title and chip, two-column facts that leave out absent values, `heading` section titles, 12/24-pixel rhythm, empty sections hidden. One accent per appearance for selection, suggested actions and the selected tab: green in light, blue in dark. Stop team, Kill and Restart fresh are destructive. |
| G6 | Status line and workspace selector tooltip use the lifecycle phrases (`src/lifecycle.rs`), with the connector's reason on a second line. |
| G8 | Evidence reads show progress and failures inside their tab; Load older shows its own progress. The window status line is for mutations only. |
| G10 | Form reasons wait for input and read as muted notes; only a failed submit is styled as an error. Usage says its shared limit reason once. |
| G11 | Message and Attach terminal in the inspector header; the rest in an Actions menu. Pause or Unpause by state; each unavailable item shows its reason under it. |
| G12 | Cause of the ~775-pixel minimum: GTK derived the window's minimum width from its current height for height-for-width content, so it stayed at sidebar + list + inspector. The adaptive layout now reports a constant size with the compact minimum; a 560-pixel request is honoured (window hint 398 × 452). The inspector split has a visible handle and starts wider; its sections sit under a tab row that wraps instead of scrolling tabs out of reach. |
| G13 | Connections lists project, path and state per row with only the actions that fit, Connect as the suggested action, one Close. Local and gateway builds share `common::entry_row`. |
| G16 | Usage collapses agents that report nothing into one line and groups counts; event levels read as a word and a colour. |

## Evidence

App source: `agents/content` at the commit that adds this note, on main
`fc26357d`. Binaries and toolkit: [tool-versions.txt](content-polish-20261006/tool-versions.txt).
Captures are the window region of a private Xvfb 1440×960 session at 96 DPI
with Openbox, Cairo renderer, an isolated daemon and the precheck's scripted
team; black popover shadows and Openbox title bars come from the capture
setup.

| Capture | Light | Dark |
| --- | --- | --- |
| Disconnected | [01](content-polish-20261006/light-01-disconnected.png) | [01](content-polish-20261006/dark-01-disconnected.png) |
| Connections | [02](content-polish-20261006/light-02-connections.png) | [02](content-polish-20261006/dark-02-connections.png) |
| Team list and header | [03](content-polish-20261006/light-03-team.png) | [03](content-polish-20261006/dark-03-team.png) |
| Agent inspector | [04](content-polish-20261006/light-04-agent-inspector.png) | [04](content-polish-20261006/dark-04-agent-inspector.png) |
| Actions menu with reasons | [05](content-polish-20261006/light-05-agent-actions.png) | [05](content-polish-20261006/dark-05-agent-actions.png) |
| Transcript read status in tab | [06](content-polish-20261006/light-06-transcript-tab.png) | [06](content-polish-20261006/dark-06-transcript-tab.png) |
| Switching agents | [07](content-polish-20261006/light-07-second-agent.png) | [07](content-polish-20261006/dark-07-second-agent.png) |
| Tasks | [08](content-polish-20261006/light-08-tasks.png) | [08](content-polish-20261006/dark-08-tasks.png) |
| Task inspector | [09](content-polish-20261006/light-09-task-inspector.png) | [09](content-polish-20261006/dark-09-task-inspector.png) |
| Messages | [10](content-polish-20261006/light-10-messages.png) | [10](content-polish-20261006/dark-10-messages.png) |
| Events with level markers | [11](content-polish-20261006/light-11-events.png) | [11](content-polish-20261006/dark-11-events.png) |
| Usage | [12](content-polish-20261006/light-12-usage.png) | [12](content-polish-20261006/dark-12-usage.png) |
| Question before input | [13](content-polish-20261006/light-13-question-before-input.png) | [13](content-polish-20261006/dark-13-question-before-input.png) |
| Question after input | [14](content-polish-20261006/light-14-question-after-input.png) | [14](content-polish-20261006/dark-14-question-after-input.png) |
| Stop team confirmation | [15](content-polish-20261006/light-15-stop-confirm.png) | [15](content-polish-20261006/dark-15-stop-confirm.png) |
| 560-pixel request, detail | [16](content-polish-20261006/light-16-narrow-560-detail.png) | [16](content-polish-20261006/dark-16-narrow-560-detail.png) |
| 560-pixel request, list | [17](content-polish-20261006/light-17-narrow-560-team.png) | [17](content-polish-20261006/dark-17-narrow-560-team.png) |

Reproduce: build `crew-desktop` (`native-ui,local`) and `crew` into
`target/native-validation/content-polish/cargo`, then
`start.sh OUT /tmp/cdp-<name> APP CREW` and, once ready,
`x.sh OUT ./capture-all.sh light` (or `dark`). `desktop-tests.sh OUT RUNTIME`
runs the crew-desktop GTK suite on a private display with a short runtime
directory (`--window` for the window test). All scripts are in
[content-polish-20261006/](content-polish-20261006/).

Checks on the final source: crew-desktop `--lib` 7/7 and `--test window` 1/1
on private Xvfb; strict Clippy (`-D warnings`, all targets) for `native-ui,local`
and `native-ui,local,gateway`; `cargo fmt --check` clean.

Not covered here: screen-reader journeys, Apple, and the remote gateway
screens beyond the shared Connections row.
