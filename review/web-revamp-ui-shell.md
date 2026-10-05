# Web revamp, `ui` shell review (agent: reviewer)

Reviewed: `eb41b115` on `agents/ui`, the slice's own commits `0f5fbcb2` and `1bc1640b` (the More menu),
`9712908b` (the shell), `109b394f` (the cuts for the byte budget), `028e56d9` and `eb41b115` (short and
narrow windows), on the approved foundation `6f5ae5d5`. Judged in the three-way build with `graph`
`b108dafe` and `content` `3eb8e667`, with `qa`'s tests through `9b15a35f`. Design: `notes/web-revamp.md`
at `8675c845`; caps: the byte ledger.

**Verdict at `eb41b115`: one small fix in `styles/ui.css`, then approved.** This was read before the
formal request so that the fix can be part of it.

## Measured (own builds)

| | P1 | P2 | P3 | P4 |
|---|---|---|---|---|
| `ui` alone (`028e56d9`; `eb41b115` changes no byte) | 216,821 (+3,251) | 31,409 (+2,182) | 332,896 | 581,812 |
| three-way, `graph` `b108dafe` + `content` `3eb8e667` + `ui` `eb41b115` | 230,128 | 33,842 | 332,915 | 597,571 |
| ceilings | 230,500 | 34,000 | 350,000 | 600,000 |

On the three-way build: `bun run test` 175 tests green; `mock` project 193 of 193; `perf` project 8 of 8
(no frame and no short timer idle); strict gallery without an overflow allowance, 0 failures.

## What holds

- Twelve files, all in the slice's ownership; no frozen file; no `web-dist`.
- Header: identity, switcher, "Dashboard live / reconnecting / stopped / signed out" with `data-conn`,
  Commands, Appearance, help, stop team. The summary row has the objective in two lines with a
  disclosure that opens it (capped at 30% of the height, scrolling inside), path, elapsed time, VCS,
  sandbox, the completion summary, and four counts from the store. It becomes one 40 px row outside
  Team, under 800 px of height, and when warnings, usage and a question are all shown. Navigation reads
  Team, Tasks, Events, Usage; routes and keys are unchanged.
- Appearance is System, Light or Dark, validated in `prefs.ts`, kept in the dashboard's `web_prefs` and
  nowhere in the browser's storage. An explicit Light under a dark system (and the reverse) computes to
  the palette's values with the matching `color-scheme`. On a stopped page the choice still applies
  locally and makes no call and no error. The terminal keeps its own dark colours in both.
- A stopped page disables Commands, Message and stop team and keeps navigation, help and the switcher;
  the bar gives the time and the instruction, with no reload control.
- Under 768 px the panel replaces the main area (`inert` and not displayed), the document is 390 px
  wide at 390 px, the close button and Escape give focus back to what opened the panel, and Back
  restores the panel. The main region keeps 400 px and the page scrolls when the chrome is tall.
- The separator is a Tab stop with its bounds and value, 20 and 80 px steps, clamped; a window resize
  writes no preference.
- The More menu has every agent operation (message, prompt, pause or resume, restart, restart fresh,
  model, compact, open terminal, jump to pane on herdr, kill), disables an item while its call is
  pending or the page is not live, keeps "open terminal" when stopped, is placed by its rendered size
  inside the viewport, and returns focus to its opener on Escape, Tab and choice. Single-key shortcuts
  are off while it is open; Enter on a native button, summary or link is the control's own.
- Switcher, stop confirmation (focus on cancel), model dialog and help (25 keys, 18 commands, the same
  table the dispatcher reads) are inside the viewport at 390 and at 1440 px. Enter during IME
  composition does not send an answer.
- The key and command tables were re-encoded as tuples for bytes; the entries, order and handlers are
  the same, and the keys and commands unit tests pass unchanged.

## Required

**The switcher's row squeezes every cell.** `.ws-dir { flex: 1 }` has a basis of zero and
`.switcher-row > :not(kbd)` lets every cell shrink, so as soon as the name and the fixed cells are wider
than the row the path gets no width at all and the dot, the state and the task count are cut. Measured
at 390 px with the default fixture (`checkout-rewrite`, `/work/demo`): path 0 of 72 px, name 112 of 118,
state 113 of 119, task count 62 of 65 (it reads "tasks 5…"), dot 13 of 28. Any name over about ten
characters does it at 390 px. At 1440 px a 52-character name does the same (dot 11 of 28, tasks 51 of
65), where the old grid kept the dot and the count whole. The design makes the path the way to tell two
workspaces of the same name apart, and its audit lists this popup at 390 px as a finding for `ui`.

Tried by injection: `flex-wrap: wrap` on the row under 768 px (29 bytes) leaves name, state, count and
dot whole at 390 px with the popup inside the viewport; keeping the dot, state and count from shrinking
handles the long name on a wide window. `ui` offsets the bytes in its own sheet if it can; up to 60
bytes of CSS from the 158 of margin are granted for this fix otherwise. `qa` has the regression to add.

## Noted, no change asked

- At 768 px and above, with nothing selected, the Message bus button (or a pair filter) sets `busOpen`;
  the next Escape only clears it, which shows nothing, and a second Escape clears the status line.
- The objective's disclosure keeps the title "Expand objective" when it is open.
- At 390 px a view key pressed with the inspector open closes the panel and leaves focus on `body`; the
  keys keep working.
- The inspector's "fill" button does nothing visible under 768 px, where the panel already fills.

## Optional items not delivered

To be named in the task's result, as the design asks: grouping, shortcut hints and Home/End in the
menu; the sticky dialog footer and inline errors; two-line switcher rows; Help grouped by scope; the
visible toggle for single-key shortcuts; the attention count. Workspace actions is one plain "stop team"
button with the existing confirmation and `S`, not a menu.

## Re-check at `dfb8094f`: approved, with one line to follow

The commit changes `styles/ui.css` only: the dot, state, count, badges and key of a switcher row no longer
shrink, name, path and command share the rest from a basis of 12 characters, and under 768 px the row
wraps. It adds 60 bytes of CSS, the whole grant, and no script.

Three-way build with `graph` `b108dafe` and `content` `3eb8e667`, `qa`'s tests at `7193b351`:
**P1 230,128, P2 33,902, P3 332,915, P4 597,631**. 175 unit tests; `mock` and `perf` 203 of 203, the two
new switcher cases among them, idle with no frame and no short timer; strict gallery of the shell's
surfaces (switcher, team, help, confirmation, model, menu, stopped dashboard, stopped workspace; four
scenarios, four viewports, both themes) 256 images, 0 failures. At 390 px the default fixture's row
shows name and path on one line and state and count on the next, all whole.

**The line to follow.** At 768 px and above the row does not wrap, so a long state still takes the line
from the identity: in the `multi` scenario the incompatible workspace reads "old-d…" (name 55 of 84 px,
path 48 of 116) at 1440, 1024 and 800 px, where the old grid kept the name. Wrapping at every width
(the declaration moved from the narrow block to the base rule, about 14 bytes less) leaves every cell
whole in all five rows; only that row grows, from 37 to 66 px. `qa` adds the case to its switcher spec.
The task is submitted when that commit is in and green.

## Re-check at `0156102f` and `6e6856c2`: approved

- `0156102f` moves the wrap to the base rule (14 bytes of CSS less). In the `multi` scenario at 1440 px
  "old-daemon" is whole, its path too, and its long state sits on a second line; the other rows are one
  line as before.
- `6e6856c2` (card `polish-help-focus`, a finding of the designer on the integrated build: Help opened
  scrolled to its end, because the close button took the first focus). The dialog's title can take
  focus (`tabindex="-1"`) and Help asks for it: it opens at its heading at 390, 720 and 1440 px, Tab
  goes to close and stays in the dialog, Escape and close give focus back to the button. The title is
  not in the Tab cycle; the question, model and confirmation dialogs keep their own first focus. 36
  bytes of script, of the 80 assigned.

Measured on the full final tree (the integrator's base, `graph`'s `8f378cf7`, these two, `qa`'s tests
at `e3a30211`): **P1 230,360, P2 33,888, P3 332,915, P4 597,849**, source `fnv1a64:14e8acd06e0d4cac`.
175 unit tests; `mock` 201 of 201; `perf` 8 of 8 run alone. Strict gallery of Help, switcher, ended
terminal and empty transcript in two scenarios: 64 images, 0 failures.

In one run of `mock` and `perf` together, with the host at a load average of 15 from other agents'
builds, first paint measured 204 ms against the 150 ms of P5. Alone it is 52 to 68 ms in three runs
(P6 132 to 155 ms of 300). The gate run of the integration has to be on a quiet host.
