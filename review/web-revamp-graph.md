# Web revamp, `graph` slice review (agent: reviewer)

Reviewed: `b108dafe` on `agents/graph`: `6954f640` (shared formatter), `f601f187` (T0 and the Task List),
`ce9d0e60` (the Team List, on its own), `cd35af2a`, `05627434`, `16603280`, `324b0d1b`, `b108dafe`.
Judged in the three-way build with `content` `3eb8e667` and `ui` `eb41b115`, with `qa`'s tests as they
stand on `agents/qa` (`c5f4906`). Design: `notes/web-revamp.md` at `8675c845`; caps: the byte ledger.

**Verdict: approved**, source and tests, with `qa`'s migration `9b15a35f`.

## Measured (own builds)

| | P1 | P2 | P3 | P4 |
|---|---|---|---|---|
| `graph` head as delivered (carries `ui` to `109b394f` and the shared list) | 227,319 | 32,654 | 332,915 | 593,574 |
| the same with `ce9d0e60` reverted | 226,293 | 32,654 | 332,915 | 592,548 |
| three-way, `graph` `b108dafe` + `content` `3eb8e667` + `ui` `eb41b115` | 230,128 | 33,842 | 332,915 | 597,571 |
| ceilings | 230,500 | 34,000 | 350,000 | 600,000 |

- The Team List is 1,026 bytes. `git revert ce9d0e60` applies without conflict on the head, the tree
  type-checks and builds, and the Team view is then the graph alone with no dead toggle: the
  orchestrator's fallback is real.
- `graph`'s own share is +10,091 script and +1,243 CSS (caps +10,161 and +1,400); the terminal chunk grew
  by 19 bytes for three theme values.
- Three-way build: `bun run test` 23 files, 175 tests green. `perf` project 8 of 8: first paint 56 ms at
  p95, live 126 ms at most, P8 0.2 ms, P9 32.3 ms, frames 16.7 to 16.8 ms under both loads, 25 log rows
  in the DOM for 15 visible, no frame and no short timer idle, heap 9.4 MB, switch 45.6 ms.
- `mock` project: 187 of 190 with the tests of `c5f4906`, the three failures being locators for `restart`,
  `kill` and `model` as buttons of the inspector's toolbar, which are now items of More. With `qa`'s
  migration `9b15a35f`: **193 of 193**, and 175 unit tests.
- Strict gallery on the three-way build (attention, busy, empty, long-text; team, both lists, tasks,
  inspector, menus, dialogs; desktop, tablet, narrow): no contrast, name or overflow failure.

## What holds

- Only the slice's files, with `term/Terminal.ts` changed in its three theme values and nothing else; no
  frozen file, no `web-dist`.
- Nodes are 244×132 (hub 260×132, satellite 200×80), with the status as text, a flags row that keeps
  `.badge.ask`, unread, limit, paused, the in-session count and pending and adds compaction queued, the
  context gauge labelled as context, and the model row still a button that opens the model dialog. The
  refused agent of the `attention` scenario is drawn as refused, in the error tone.
- More is on every node, in the lists and in the inspector, and opens the one shared menu; so do
  Shift+F10 and the context-menu key. A native button inside a node keeps Enter for itself.
- Zoom out, zoom in and Fit are buttons; the legend names the four kinds of connection; the hover card
  stays inside the canvas; a cancelled pointer no longer selects or stores a drag.
- Both lists are one component on the shared `VirtualList` with `follow={false}`: selection is by id, only
  one presentation is mounted, `data-agent` and `data-task` stay unique. With the Team List focused `k`
  moves the selection and opens no kill dialog. The list is the default under 640 px; the choice now
  lasts for the document (chosen, left with `l`, returned: unchanged, at 390 and at 768 px).
- Inspector: Message and Pause or Resume, then More; overview in the design's order (current work,
  session, problems, messages, tasks, process details folded); "Read" and "Delivered" in words; a failed
  transcript or diff read keeps what was shown and says so.
- The animations' module and constants are untouched; the idle page asks for no frame.

## The migrated assertion

`reconnect.spec.ts:61` asserts that a stopped page has "every control disabled". More itself stays
enabled when the page is stopped (it leads to "open terminal", which is local). `9b15a35f` opens More and
asserts that all nine mutating items are disabled and "open terminal" is enabled: the check was moved,
not dropped.

## Noted, no change asked

- With every strip showing at 1440×960 the graph fits at about 0.64 and node text is near 8 px; the
  design's hint to zoom or use the list was cut for bytes. The list is one click away and the default
  where it matters.
- Under reduced motion a task's state change now shows a static outline for the sweep's duration instead
  of nothing, which is what the design asks ("preserves static highlights and expiry").

## Polish after integration, `8f378cf7`: approved

Two findings of the designer on the integrated build, both in `graph`'s files: an available transcript
with no entries was a blank panel, and an ended Herdr terminal showed a blank screen under "click it to
type". The commit changes `views/Inspector.tsx` and `views/TerminalTab.tsx` only.

- An empty available transcript says "no transcript entries yet", apart from the unavailable and the
  failed read.
- The screen keeps the whole `agent_output` result. Keys, paste, the queued flush and "jump to pane"
  need a live page and `alive`; a result that is not alive clears what was queued. The bar then reads
  "terminal ended; refresh checks for output", with refresh still enabled. An ended screen with no text
  is not a Tab stop; one with text stays focusable so it can be scrolled. Keys pressed there stay inside
  the terminal region, so no page shortcut fires. The daemon always sends `alive`
  (`daemon/handlers.rs:352`).
- The overview's facts are pushed in batches for bytes: the same fields in the same order.

Own build on the commit (the integrator's tree with `ui` `dfb8094f`): P1 230,324 (+196 of the 200
assigned), P2 33,902, P3 332,915, P4 597,827. 175 unit tests; `perf` 8 of 8 with nothing idle; `qa`'s
`02427a12` (ended empty, ended with text, live typing and paste with the exact calls) 15 of 15 with the
inspector and read-state specs. The full `mock` project is 198 of 199: the one failure is the switcher
case that waits for `ui`'s wrap line.

Noted: the screen's label is now "terminal <id>" (it was "the terminal of <id>"); no test names it.
