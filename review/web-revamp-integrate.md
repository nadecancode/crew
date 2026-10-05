# Web revamp, final review of the integrated branch (agent: reviewer)

Reviewed: `a8829763` on `agents/integrator` (the bundle commit `d092f4a7` and one README sentence after
it), the merge of `ui` `6e6856c2`, `graph` `8f378cf7`, `content`
`3eb8e667`, `qa` `e3a30211` and the designer's `823abf3e`, with the bundle rebuilt and committed. Against
`main` at `a6692c9`. Design: `notes/web-revamp.md` as amended at `823abf3e`. The slices are reviewed in
`review/web-revamp-{ui-foundation,ui-shell,graph,content,qa-harness}.md`; this file is about the whole.

**Verdict: approved.**

## The tree

- Outside `web/src` the branch changes `web/dev`, `web/e2e`, `web/test`, one script line of
  `web/package.json`, `crates/crew/web-dist`, `notes/web-revamp*` and `README.md`. No Rust source, no
  dependency, no `web/index.html`, no build configuration.
- `web/src` is exactly the tree approved slice by slice: it equals my own merge of the approved commits,
  file for file. The frozen files (`app.ts`, `clock.ts`, `text.tsx`, `route.ts`, `protocol.ts`, `net/`,
  `store/`) equal the foundation `6f5ae5d5`.
- The tests are `qa`'s head plus two commits of the integrator: `c1b6110f` removes the gallery's overflow
  allowance altogether (stricter), `fd9d868d` reads the event count from the filter bar in the two perf
  specs (the new Events header put another muted span first; the live P10 warm-up read 0).
- In the lines the branch adds to `web/src` there is no HTML sink, no network call, no browser storage,
  no dynamic import, no timer and no animation frame; the listeners added are for `resize` and two media
  queries.

## The bundle

| | measured | ceiling | under by |
|---|---|---|---|
| P1 script without the terminal | 230,360 | 230,500 | 140 |
| P2 CSS | 33,888 | 34,000 | 112 |
| P3 terminal chunk | 332,915 | 350,000 | 17,085 |
| P4 `web-dist` | 597,849 | 600,000 | 2,151 |

- The committed manifest is byte for byte the one my own build of the same sources wrote (source
  `fnv1a64:14e8acd06e0d4cac`); `just web-check` rebuilds in a temporary directory and finds no
  difference; `cargo test -p crew --test web_bundle` 2 of 2. No stale asset is left in `web-dist`.
- Both List modes are in; the Team List fallback (`ce9d0e60`) was not needed.
- The gates in `web/e2e/perf.spec.ts` and `web/test/budget.test.ts` are 230,500, 34,000, 350,000 and
  600,000, as decided.

## Own runs on `d092f4a7`, against the committed bundle

- `bun run test`: 23 files, 175 tests.
- `mock` and `perf` projects: 210 of 210.
- `perf` alone, for the figures: first paint 56 ms at most (150), live 146 ms at most (300), 25 log rows
  in the DOM for 15 visible with 5,000 events, heap 7.9 MB, workspace switch 48 ms at p95, key to echo
  4.9 ms at p95, no frame and no short timer in 3 s idle, no index frame and no call in 60 s with five
  workspaces.
- `live` project against a real daemon on jj (`scripts/web-e2e jj`): 36 of 36 in 3.3 minutes.
- The integrator's strict gallery, read from its manifest: 2,432 captures (8 scenarios, 38 surfaces, two
  themes, four viewports), 2,248 images, 184 skips that are all the `empty` scenario's missing entities,
  0 failures, 0 document overflow, 0 contrast or name failures, 0 page errors, 0 CSP reports, 0 requests
  after load; no allowance exists any more.

## What was fixed between the first integration and this head

Found on the integrated build `fd9d868d` and closed before this review, each with a test of `qa`'s:

- The switcher's rows squeezed every cell (path at no width, state and task count cut) at 390 px, and
  the name under a long state on a wide window: `dfb8094f`, `0156102f`.
- Help opened scrolled to its end: `6e6856c2`.
- An empty transcript was a blank panel, and an ended Herdr terminal invited typing: `8f378cf7`.

## Corrected in review

- `README.md` said "It fits a phone as well as a wide screen"; the dashboard listens on the loopback
  addresses only, so no phone reaches it. `a8829763` says a narrow window; its diff is those two lines,
  and the sources and the bundle are those of `d092f4a7`.

## Left as it is, known

- Delivered: all of T0; of the funded T1, the Team and Task Lists (default under 640 px, kept for the
  document) and the usage totals with their coverage. Not delivered, by measured bytes: task lifecycle
  dates and artifact provenance, Retry copy, the visible toggle for single-key shortcuts, the attention
  count, grouping, hints and Home/End in the menu, the sticky dialog footer and inline errors, two-line
  switcher rows as designed (rows wrap instead), Help grouped by scope, the zoom hint. All of T2.
- A graph fitted to the window can be small: fifteen tasks in nine layers beside the bus read at about
  6 px. Zoom and the List are one click away; the design's hint to that effect was cut for bytes.
- At 200% zoom or 390 px with warnings, the usage strip and a question all showing, the chrome fills the
  first screen; the main region keeps 400 px and the page scrolls.
- At 768 px and above, the first Escape after the Message bus button only clears the bus flag.
- 140 bytes of script and 112 of CSS are left. Any later change to the page has to be measured first.
- P5 measured 204 ms once in a run that shared the host with other agents' builds (load average 15),
  and 52 to 68 ms in every run alone. The budgets want a quiet host.
- `e2e/anim.spec.ts:82` and `e2e/graph.spec.ts:149`, which each failed once under load early in the
  work, passed in every run of the final tree.
