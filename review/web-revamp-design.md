# Web revamp design review (agent: reviewer)

Reviewed: `notes/web-revamp.md` and `notes/web-revamp/**` at `12207423` (designer, task `revamp-design`).
Review only; no file under `web/` or `crates/` was changed.

**Verdict: changes requested.** The audit is accurate and the direction is good. The design is not yet
buildable as a four-way parallel split: its scope does not fit the byte budgets it promises to keep, several
things the page does today have no place in it, and the split leaves process and ownership holes that will
surface at integration. One blocking finding (1), five required (2–6), the rest advisory (7).

## How it was checked

- Read the whole document; opened all 14 mockups' scenes (team light/dark/narrow, inspector, tasks, events,
  usage, question) and a sample of the 60 baseline captures; checked every `src`/`href` of the gallery
  (92 links, none missing, no remote URL).
- Compared each claim with the sources at the same commit (`web/` is identical to `main`): `theme.css`,
  `keys.ts`, `prefs.ts`, `protocol.ts`, `Shell.tsx`, `SidePanel.tsx`, `AgentNode.tsx`, the e2e and unit
  suites, `vite.config.ts`, `scripts/stamp.ts`, `crates/crew/src/web/process.rs`, `daemon/handlers.rs`,
  `notes/web-design.md` §scope/§graphs/§perf, `notes/web-multi.md`.
- Recomputed every contrast figure (WCAG relative luminance) for all 20 tokens on all six surfaces.
- Measured the bundle per module: a Vite build with `write: false` and each module of the entry chunk
  minified on its own with esbuild, scaled by 0.876 (the entry chunk is 194,835 bytes, the per-module sum
  222,371). Nothing was written to `crates/crew/web-dist`.

## What holds

- Budgets: P1 213,530 (headroom 6,470), CSS 28,487 (1,513), P3 332,896, P4 575,599 (24,401). Correct.
- Audit: `body { min-width: 900px }`, document 907 px wide in all 30 narrow captures; `--dim` on `--bg`
  2.32:1 and `--muted` 4.68:1 (4.36 on `--panel`, 3.85 on `--node-hi`); 22 px rows in bus, log and
  transcript; switcher `min-width: 680px`; the resizer has `tabindex="-1"` beside a working arrow-key
  handler (`SidePanel.tsx:55`); no light palette (`color-scheme: dark`). All correct.
- Theme preference in the dashboard's `web_prefs`: feasible without a daemon change. Both stores take any
  JSON object up to 16 KiB (`web/process.rs:279`, `daemon/handlers.rs:956`); the page validates.
- The §content inventory matches `protocol.ts` (protocol 6) field by field. Names the design relies on
  exist: `store.openQuestions`, `ui.setMenu`, `ui.select`, `ui.setBusPair`, `STALE_MS`, `KEYS`,
  `link.live()`, `ctrl-k`, `S`, `t`/`l`/`v`/`w`, `f` in logs, `r` in the inspector, the one-second clock.
- Stated contrast figures are right except "action on its surface" (finding 5).
- No new runtime dependency, no remote load, no HTML sink, no HTTP after load, no polling: the document
  restates the contracts of `web-design.md` §security and `web-multi.md` faithfully.

## Findings

### 1. Blocking: the scope is about three times the script headroom, and there is no byte plan

The document keeps P1–P4 unchanged and defers the question: "If the design cannot fit, bring a concrete
byte breakdown and simpler implementation to review." With four agents building in parallel against one
shared remainder of 6,470 bytes of script and 1,513 of CSS, that moment is integration, after the work is
done. Nothing allocates the remainder to slices or says what is dropped first.

What the page's existing views cost in the shipped entry chunk (measured as above):

| existing module | bytes | closest new work in the design |
|---|---|---|
| `Logs.tsx` (virtual list, four filters, load older) | 4.0 KB | Task List with status/assignee filters and search; Team List |
| `Bus.tsx` | 2.6 KB | bus search, type filter, follow/jump with a count, load states |
| `TaskDag.tsx` | 5.6 KB | – |
| `ContextMenu.tsx` | 1.8 KB | Inspector More, Workspace actions, grouped menu with hints, Home/End |
| `Switcher.tsx` | 7.2 KB | – |
| `Shell.tsx` | 17.3 KB | objective/summary row, four counts, attention disclosure, workspace details, appearance control, Message bus control, narrow replacement region with focus hand-over |
| `Inspector.tsx` | 15.2 KB | reordered sections, Problems from events, full fallback/limit facts, per-state copy, Retry |

By analogy, a conservative estimate of the additions: two List modes 6–8 KB; bus/events/`VirtualList`
filters, search and follow/jump 3–4.5 KB; shell 3–5 KB; inspector 2–3.5 KB; task, message, event and usage
detail additions 4–5 KB; 14 icon paths and four primitives 2–3.5 KB; theme preference and control 0.6–1 KB;
menus 0.5–1 KB; state and empty-state copy (strings are bytes too) 2–3 KB. **23–38 KB against 6.47 KB.**
The design removes nothing ("Retain every action and fact"), so the only credit is deduplication through
the primitives, a few KB at best.

CSS is in the same position. Of the 28,487 bytes about 5.2 KB is xterm's own sheet, which leaves about
24.8 KB for the application under the 30 KB ceiling. `mockups/mockups.css` is 15.2 KB minified for six
static scenes, and it has no terminal, transcript, diff, switcher, help, confirmation, context menu, model
picker, hover card, DAG, marks and pulses, usage chart, toasts, or any tier between 640 and 1099 px. Those
surfaces are well over 10 KB of today's sheet.

P4 bounds any other answer: 24,401 bytes for script and CSS together, even if P1 and P2 were raised.

Required, one of:

- **A, inside the budgets.** A `§budget` section with (a) a net allowance per slice for script and for CSS,
  summing to the headroom left after the foundation commit; (b) the rule that every slice's review request
  carries the figures of a local `bun run build`, and that a slice over its allowance is not approvable;
  (c) the features in tiers with an explicit cut order. A possible order, cheapest value first: tokens,
  contrast and the light palette; shell hierarchy; 84 px bus rows; inspector order and More; no document
  overflow from 640 px; focusable resizer; event row. Last, and first to go: the tier below 640 px with
  44 px touch targets (finding 7a), Team List, usage isolate, `Event.data` disclosure, bus text search,
  workspace details, icons beyond the handful actually needed.
- **B, a decided raise.** `web-design.md` §perf "What P1 is for" calls P1 "a ceiling against growth nobody
  decided on" and "not a target to cut features for". The task card lists the budgets under §keep, so this
  is the operator's decision, through the orchestrator, with numbers for P1, P2 and P4, and with
  `budget.test.ts` and §perf amended in the same change. A tier list is still needed, because of P4.

Either way the foundation commit should report the sizes it leaves, so the allowances start from a
measured number.

### 2. Required: things the page does today that the new design gives no place

`web-design.md` §graphs is not among what the document says it replaces (it names §ui), yet it changes
what §graphs specifies. State what happens to each of these, and add §graphs' node, interaction and usage
strip paragraphs to the list of what is superseded:

- **The usage strip** (`Shell.tsx:287`, `data-region="usage"`, §graphs "Usage: Strip, always visible when
  there is something to show"): quota bars per runtime on every view, and the row with "continue now" /
  "restart on the current login" for agents paused at a limit. The new shell lists header, objective row,
  navigation, warning and question strips; the strip is absent. `u`/`U` and five e2e selectors depend on it.
- **The model label on a node** and its click (the model dialog: "the TUI's click a model"). The design
  moves the model to hover and inspector. If the label goes, say so as a deliberate change with its test
  (`.node .model`), or keep it.
- **Other node interactions of §graphs** not in "Interactions stay": click on the task title (the task's
  card), click on the `?` badge (the question dialog), mark chips and their click and copy, a pulse's click,
  a VCS link's hover and click. Also the in-session subagent count `⑂N`, the pending badge, and the
  provider-refused / provider-error state (`Agent.block_kind`, `format.ts:55`), which is in neither the
  node description nor §content.
- **The p2p banner** ("p2p channel OPEN", `AgentGraph.tsx:299`) and **toasts** (`Shell.tsx:369`: errors,
  and the notice with an action when a question arrives in another workspace). Neither is described.

### 3. Required: the 1100 px breakpoint takes away a layout that works today

Below 1100 px the panel replaces the main region. Today main and panel are side by side down to 900 px,
and the design's own minimums (`--panel-min` 320, main 420) allow it from 740 px. As written, a
1024×768 window (in the document's own screenshot matrix) can never show the graph and an inspector, or the
graph and the bus, together; walking the agents with the inspector open is lost there. Derive the
side-by-side threshold from the minimums, or justify 1100.

Related, advisory: the fixed chrome grows from about 34 px to 56 + 94 + 46 = 196 px before any strip. At
768 px height with a warning, the usage strip and a question the main region is near 400 px. Give a rule
(for instance the objective row collapses to one line under 800 px height, or outside Team).

### 4. Required: holes in the split

- **Build output.** `bun run build` writes `crates/crew/web-dist` (`vite.config.ts`, `emptyOutDir`), the
  e2e suites need that bundle, and crew's `git_commit` commits everything in the worktree. Three slices
  committing rebuilt, hash-named assets and `manifest.json` conflict at integration. Say explicitly: slices
  build locally to measure and to run e2e, restore `crates/crew/web-dist` before committing, and
  `crates/crew/tests/web_bundle.rs` is expected to fail on a slice branch until `qa` rebuilds.
- **Tests owned by `qa` alone.** A slice that changes row heights, labels or the position of a control
  cannot make its own branch green, yet each slice's acceptance says the suites pass. Define the hand-over
  (patch to `qa`, `qa` commits, the slice cherry-picks that commit, the review request names both), or give
  each slice the spec files of its own surface. Without it a slice cannot be reviewed against its tests.
- **The selector contract.** §keep preserves the `data-*` identities. The e2e suites also select about
  sixty class names (`.node.member` 14 times, `.node-head` 12, `.badge.ask` 9, `.panel-title`,
  `.usage-strip`, `.question-banner`, `.vrow`, `.vlist-pad`, `.bus-row`, `.log-row`, `.hover-card`,
  `.task.dim`, `.pulse`, `.mark`, `.sweep`, …) and exact lowercase names (`"model"`, `"copy"` with
  `exact: true`). Either these are kept, or every rename goes to `qa` as a list.
- **Files nobody owns.** `src/term/Terminal.ts` holds the xterm theme the design asks to change ("its text
  and xterm theme must form their own matching palette"); `graph` owns `TerminalTab.tsx` only. Also
  `src/app.ts`, `clock.ts`, `text.tsx`, `route.ts`, `web/index.html`, `vite.config.ts`: name an owner or
  say frozen.
- **`VirtualList` is shared.** `content` owns it and changes it (follow/jump API, reactive row height);
  `graph`'s transcript in `Inspector.tsx` uses it at 22 px. Fix its interface in the foundation, or require
  the change to be compatible with the existing props.
- The card gave help and empty states to `content`; the document gives help to `ui` (it lives in
  `dialogs.tsx`) and empty states to each view's owner. Reasonable; say it is deliberate.

### 5. Required: tokens

- Two text pairs fail the document's own rule ("all meaningful text tokens meet 4.5:1 on their actual
  surfaces"): light `--quiet` on `--err-surface` 4.44, dark `--quiet` on `--action-surface` 4.48 (the
  metadata line of a selected bus row). Light `--quiet` on `--surface-raised` is 4.54, with no margin.
- `--control-border` is under 3:1 where the mockups put controls: light on `--canvas` 2.95 and on
  `--surface-raised` 2.81 (the Graph/List toggle, zoom buttons and filters sit on the canvas); dark on
  `--action-surface` 2.73. The figures given (3.20 / 3.70) are for `--surface` only.
- "action on its surface 6.18 / 9.38" are `--action` on `--surface`; on `--action-surface` it is 5.42 light
  and 6.93 dark. Passing, but the sentence is wrong.
- `--p2p` has no value, only "a readable purple tone". `--shell` is purple (`#6954ab` / `#bda5ec`); two
  purples with different meanings on one graph. Today it is a pink-red. Give values.
- The four token categories of the usage bars have no tokens; the mockup uses green, blue, purple and
  yellow, which are the action, info, shell and warn hues. Name the mapping, since "runtime colour
  identifies runtime".
- In `mockups.css`, `--accent` is the action green. In the document and in the application `--accent` is
  the Claude runtime colour. Rename in the mockup, or an implementer copying from it inverts the meaning.

### 6. Required: mockups

Fourteen images of six scenes show the calm case only: every agent Claude, one working, the rest done.

- No scene shows what the workspace is for, attention: blocked, failed, paused or limited agents, the
  sandbox warning, the usage strip, the question strip, an attention count, mixed runtimes. No runtime
  token (`--accent`, `--codex`, `--pi`) appears in any mockup; every glyph chip is action green. One
  desktop scene, light and dark, would settle most of findings 2 and 5 by showing them.
- The most novel layout rule, the panel as a replacement region, has no image; nor has the task DAG in its
  new node style (the tasks scene is List only).
- The narrow team mockup contradicts the text: no connection state in the header (the text keeps the index
  connection state there, and `data-conn` is what says a document is stopped); three counts, Questions
  missing; a "Show all 8 agents" truncation the text does not describe; `claude-3-s1` listed before
  `claude-2`, against "satellites beneath parents, TUI order". Fix the image or mark each as not normative.

### 7. Advisory

- **(a) The P0 ranking.** The server is loopback only (`web-design.md` W5; Safari unsupported). A 390 px
  viewport is a tiled desktop window, not a phone, and a coarse pointer is a rare touch laptop. No document
  overflow and a usable shell down to about 640 px is worth the top rank; the tier below 640 px and the
  44 px touch targets are the natural first cut in finding 1.
- **(b)** With an explicit theme that differs from the system's, the page paints the system palette until
  `web_prefs` arrives, on every load. Acceptable on loopback; write it down as accepted so nobody adds
  browser storage to fix it.
- **(c)** The test table omits `e2e/mock.spec.ts` (33 tests of the mock itself, which the new scenarios
  must keep green), `test/page.test.tsx`, `e2e/live/open.spec.ts` and `e2e/live/vcs.spec.ts`.
- **(d)** Help "grouped by task" must still satisfy `keys.test.tsx` "the help overlay lists exactly the
  table": the grouping has to come from the table, not from a second list.
- **(e)** `format.ts` goes to `graph`, but `content` and `ui` import it and the new task timeline and usage
  totals need formatting. Route additions through its owner, as for `theme.css`, to avoid three copies.

## To approve

Findings 1–6 answered in the document (and the mockups for 5 and 6). I will re-review the diff.
