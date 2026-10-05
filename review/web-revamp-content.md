# Web revamp, `content` slice review (agent: reviewer)

Reviewed: `f74bcc9a` on `agents/content` (bus, events, task, message and event panels, usage, the virtual
list), with `qa`'s paired tests `92a2f81c` and `f4781699` applied, against the approved foundation
`6f5ae5d5`. Design: `notes/web-revamp.md` at `8675c845`; caps: `review/web-revamp-budget-ledger.md`.

**Verdict: two small fixes, then approved.** The slice does what T0 asks of it, inside its bytes, and
keeps the lists' guarantees.

## Measured (own build and runs)

| | measured | cap | |
|---|---|---|---|
| P1 | 217,580 (+4,010) | +4,050 | within; T0 +3,443, one T1 item +567 |
| P2 | 30,404 (+1,177) | +1,200 | within; the CSS grant is returned |
| P3 | 332,896 | unchanged | |
| P4 | 581,566 | | |

- `bun run test`: 23 files, 172 tests green.
- `mock` project: 166 of 167 in the first run, 167 of 167 in the second (as the `perf` project's
  dependency). The one failure was `e2e/graph.spec.ts:149` "+ - 0 zoom and fit": it read the scale as 1
  before the first fit had run. It passes six times of six alone, on this candidate and on the foundation,
  and the slice touches nothing in the graph: an existing race in the test (it reads `k0` straight after
  `openPage`), shown by a loaded machine. For `qa`: wait for the fit before reading `k0`.
- `perf` project green: P8 0.2 ms at p95, P12 27 rows in the DOM for 17 visible with 5,000 events, P13 no
  frame and no short timer in 3 s idle, heap 10.6 MB.
- Strict gallery over the slice's surfaces (busy, long-text, empty, limits; team, events, event detail,
  usage, task detail, message detail; both schemes; desktop and tablet): 84 images, 0 failures, 12
  skipped for want of an entity in `empty`.

## What holds

- Ten files, all the slice's own, plus three lines in `format.ts` (`absoluteTime`, carried from `graph`'s
  commit as the design's rule for shared formatting asks).
- Bus rows are 84 px with metadata and two lines of body; event rows 36 px, 64 px in two lines under
  768 px, and the keyed anchor survives the change (tested with a fractional offset). The transcript is
  still 22 px. `VirtualList` keeps every existing prop; `follow` is optional.
- Class and data hooks are kept (`.bus-row`, `.log-row`, `.vrow`, `.vlist-pad`, `data-seq`,
  `data-message`). Two accessible names changed on purpose, with their tests: the toolbar "log filters"
  is "event filters", the button "vcs" is "Version control".
- Loading, failure and absence are distinct in the task, message and event panels; a failed `task_get`
  keeps the cached card; a reply that arrives after the panel is gone is dropped. Errors of "Load older"
  are shown where the button is.
- Usage: an agent that reported zero and one that reported nothing are told apart, the total names its
  coverage, a stale window says "Stale", a single sample is a point, the token categories use the
  `--token-*` colours. Text is plain JSX throughout.

## Required

1. **The task panel jumps on every push.** `TaskPanel.tsx` renders `<p class="fetch-state">` with
   "Loading task details…" whenever a fetch is in flight, a fetch runs on every `task` push for the open
   task, and `.fetch-state:empty { display: none }` removes the row afterwards. Measured: the title is
   at 181 px, at 216 px while the fetch is in flight, at 181 px after. A working task pushes progress
   every few seconds, so what the operator is reading moves each time, and `role="status"` announces it
   each time. Show the row for the first load only, or reserve its height.
2. **Two focus rings on a list.** `.vlist` lost `outline: none`; the foundation's global
   `:focus-visible` outline now shows together with the list's inset ring.

## Worth doing in the same commit

- `Bus.tsx` runs `oneLine` over the whole body and puts all of it in the row. Five messages of 400 KB
  appear after 282 ms, against 65 ms on the foundation; at 50 KB there is no difference. Cut the input to
  a few hundred characters.
- `usageTotals` still computes `turns` and `limited`, which nothing reads since those aggregates were cut.
- Every context gauge and token stack has `tabindex="0"`: 12 extra Tab stops for eight agents, and focus
  shows a sighted keyboard user nothing (`title` appears on hover only). Keep `role="img"` and the label,
  drop the tab stop.

## For integration (owner: `ui`)

The bus header is now a `PanelHead`, which always draws its close button. With nothing selected the
button does nothing but move focus: a dead control on the persistent bus. `ui` has to make it close the
bus below 768 px and leave it out at 768 px and above.

The `ViewHeader` count ("12 loaded") is drawn at the title's size; the designer has already given that to
`ui`.

## Re-check at `95f182ee`: source approved

The commit changes five of the slice's files by seven lines: the two required fixes and the three that
were worth doing.

- The task panel's loading row shows for the first load only (`!card() && !error()`). Measured again: the
  title is at 181 px before, during and after a push's fetch, and the status is empty during a refresh.
- A focused list has one ring (outline none, the inset shadow).
- The bus preview reads the first 2,048 characters: five messages of 400 KB appear after 64 ms, as on the
  foundation.
- `usageTotals` returns `tokens` and `reported`; the gauges and stacks keep their `role="img"` and label
  and are no longer Tab stops (9 stops in the usage view for eight agents, down from 21).

Own build: P1 217,518 (+3,948 of 4,050), P2 30,417 (+1,190 of 1,200), P3 332,896, P4 581,517.
`mock` project 167 of 167. Unit tests 170 of 172: the two failures are `test/usage.test.ts` expecting the
`turns` and `limited` fields that this review asked to remove.

**Condition for the task.** `qa` updates those two expectations (and adds the regressions for the first
load and the quiet refresh that `content` named); the task is approved when the suite is green with that
commit. The slice is reviewed against +4,050 script and +1,200 CSS; 102 bytes of script and 10 of CSS are
unused.

## Follow-up `3eb8e667`: approved

One line in `VirtualList.tsx`, found at integration: when a row is at least as tall as the list's view
(the 160 px rows of the graph's lists in a short pane), selecting it aligns its top instead of its bottom,
so the row's name stays in view. Lists whose rows are shorter than the view behave as before. Own build:
P1 217,539 (+3,969 of 4,050), P2 30,417; 172 unit tests; the lists, content, reviewer and scenario-read
specs 33 of 33. It costs the three-way build 21 bytes of its 322 of margin.
