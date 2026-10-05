# Web revamp, `ui` foundation review (agent: reviewer)

Reviewed: `3ad22455` + `076180cc` on `agents/ui` (tokens, primitives, stylesheet extraction, `ui.busOpen`),
against `a6692c9` (`main`) and `notes/web-revamp.md` at `8675c845`. Checked out in a scratch worktree;
nothing of it is in this branch.

**Verdict: three one-line fixes in `styles/ui.css`, then approved as the base.**

## Measured

`bun run build` in the scratch worktree:

| | measured | allowance | |
|---|---|---|---|
| P1 (F1) | 213,570 (+40) | +1,000 | within |
| P2 (F2) | 29,178 (+691) | +1,400 | within |
| P3 | 332,896 | unchanged | |
| P4 | 576,330 | | |

Reserve after the foundation: 228,000 − 213,570 − 12,200 = **2,230** script; 34,000 − 29,178 − 3,800 =
**1,022** CSS.

`bun run test`: 20 files, 154 tests, green. `playwright test --project=mock` (own `--output`): 146 of 146
green, with Playwright's default light colour scheme, so the suite now runs on the light palette. The
commits touch ten files under `web/src` and nothing under `crates/crew/web-dist`.

## How the extraction was checked

Old `theme.css` and the four new sheets were parsed with css-tree into (media, selector, declaration)
sets and compared: 444 rules before, 439 after; 24 selectors gone, 19 new, 166 declarations changed.
Most of it is what the foundation is for (tokens, `light-dark()`, hard-coded colours replaced by tokens,
`:is()` merges of the same rules). The rest was read one by one; then 60 captures of the build (the
designer's `capture-baseline.mjs`) and a scripted question + usage-limit state in both schemes.

Token values match the design's table, the aliases are all there, `--token-*` are defined, the six icons
are the six of T0, the primitives take data only, and `graph.css` and `content.css` do not import each
other.

## Required

1. **`.badge.ask` is a button, and every button now has `min-height: var(--control-h)`.** On a node that
   has a question the `?` measures 22×37 px and its `.node-head` grows from 21 to 37 px; the rows below
   leave the fixed-height node. In the capture `claude-2` loses its gauge and model row and the
   orchestrator its model row. `076180cc` reset `.node .model` and `.task-foot button` and missed this
   one. `min-height: 0` on `.badge.ask` (the rule is in `ui.css`).
2. **`a { color: var(--orch) }` is gone and its declaration sits on the form fields**: `ui.css:26` is
   `input, select, textarea { padding: 4px 8px; color: var(--orch); }`. The fields are saved by the order
   of the cascade (`theme.css` resets `color: inherit` after the imports), so nothing shows there, but
   links in message text now have the browser's default link colours instead of the palette's.
3. **The help table's cells are muted.** `:is(.help-keys, .help-commands) :is(td, th)` carries
   `color: var(--muted); font-weight: normal`, which before applied to `th` only. Keys, actions and
   commands lost their contrast against the "TUI" column that is meant to be the muted one.

## For the slices that build on it (no change asked of the foundation)

- **graph.** The base font went from 13 px/1.45 to 14 px/1.5 and the node geometry did not: on every node
  the model row touches the bottom border. The design's 244×132 node is the fix.
- **graph, content.** Transcript rows are styled in `content.css` through shared selectors
  (`.bus-row .time, .log-row .time, .tr-row .time` and the like), and `content.css` is imported after
  `graph.css`, so an equal-specificity override from `graph.css` loses. Split those selectors before the
  bus and log rows change height; the transcript stays at 22 px.
- **content.** The usage bars' classes still map to `--gauge`, `--accent`, `--shell` and `--orch`; the
  design's `--token-input`, `--token-output`, `--token-cache-read`, `--token-cache-write` are defined
  and unused. "Output" is currently drawn in the Claude runtime colour.
- **ui, in the shell work.** `.panel-title` lost its size and is the browser's `h2` (21 px); the design
  says 16 px (`--font-lg`). Read-only now tints `.main` only, from canvas to raised, which is hard to
  see; the availability states of §views are where that gets finished. `body`'s 900 px minimum is gone
  before the shell is responsive, so a narrow window clips until it is. `StatusBadge` emits a class
  `neutral` that no rule matches.

## Re-check at `6f5ae5d5`: approved as the base

The commit changes `web/src/styles/ui.css` only, eight lines, the three fixes and nothing else. It sits on
the designer's budget amendment and `qa`'s two commits that move the P1 and P2 limits to 228 KB and 34 KB
in `test/budget.test.ts` and `e2e/perf.spec.ts`, as the orchestrator decided.

- `.badge.ask` measures 22×24 px, the asking node's head 24 px, and every row is inside the node again.
- Links compute to the palette's `--orch` (`rgb(49, 94, 190)` light, `rgb(140, 183, 242)` dark); the form
  fields carry no colour of their own.
- Help cells compute to `--text`, their headers to `--muted`.

Own build: P1 213,570 (+40 of 1,000), P2 29,227 (+740 of 1,400), P3 332,896, P4 576,379. 154 unit tests
and 146 of 146 in the `mock` project green. **Reserve from here: 2,230 bytes of script, 973 of CSS**;
slice allowances are measured against this commit.
