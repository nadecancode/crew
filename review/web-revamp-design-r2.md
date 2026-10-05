# Web revamp design, second review (agent: reviewer)

Reviewed: `notes/web-revamp.md` and `notes/web-revamp/**` at `b4408392`, as a diff against `12207423`
(first review: [web-revamp-design.md](web-revamp-design.md)). The commit touches nothing under `web/` or
`crates/`.

**Verdict: approved**, with two small amendments to the document that need no further review, and one
risk that is the operator's to decide (below).

## The six findings

1. **Scope and budgets: answered.** §budget keeps P1–P4, makes T0 (palettes, type, shell hierarchy, bus
   and event rows, inspector order, graph controls, the panel below 768 px) the acceptance baseline, and
   makes the new surfaces conditional (T1, T2) with a fixed cut order. Allowances are net bytes per slice
   against a measured foundation; the arithmetic holds (script 1,000 + 1,800 + 1,300 + 1,700 + 670 = 6,470;
   CSS 700 + 200 + 200 + 300 + 113 = 1,513; with 256 bytes for the terminal chunk the bundle is at most
   583,838, 16,162 under P4). Every slice review request carries its build figures; a slice over its
   allowance is not approvable; the reserve moves only by a recorded review comment; a T0 that does not fit
   stops for design review instead of raising a budget.
2. **Features without a place: answered.** A table places the usage strip (below warnings, above the
   question strip, on every view), the node's model label and its click (nodes grow to 244×132 for it), the
   task-title, `?` badge, mark, pulse and link interactions, `⑂N`, pending, provider error, the p2p banner
   and the toasts, each with the selector it keeps. Every identifier the table names exists in the sources
   (`data-task-link`, `data-mark`, `p2pLive`, `providerError`, `statusTail`, `.toasts`). §graphs is now
   named among what is superseded.
3. **Breakpoint: answered.** Side by side from 768 px, derived from 420 + 320 + 1; replacement below.
   The objective row is 40 px under 800 px of height, outside Team, or when the strips coexist.
4. **Split: answered.** Bundle output is cleaned and restored before every commit that is not `qa`'s, and
   the stale-bundle test is expected to fail on slice branches. The test hand-over is a defined sequence
   that leaves a slice branch green with a named test commit. Class selectors and exact names are a
   contract until a migration list reaches `qa`. `term/Terminal.ts` goes to `graph` for theme values;
   `app.ts`, `clock.ts`, `text.tsx`, `route.ts`, `protocol.ts`, `net/**`, `store/**` and `index.html` are
   frozen; `vite.config.ts` is `qa`'s. `VirtualList` stays compatible with its present props.
5. **Tokens: answered.** `--quiet` and `--control-border` changed, `--p2p` is a pink with values, the
   usage categories have four named tokens, the mockup's `--accent` is the Claude colour again.
   `contrast.mjs` passes (exit 0); I get the same minimums with my own computation: quiet 4.87 light and
   5.29 dark, border 3.30 and 3.87, lowest text pair 4.87 (light `--pi` and `--quiet`).
6. **Mockups: answered.** 22 images. The attention scene shows mixed runtimes in their colours, blocked,
   paused, failed and working nodes, the sandbox warning, the usage strip with its paused row, the question
   strip, the p2p banner and link, a mark chip, a toast with its action, and the 40 px summary. There are
   images of the DAG, the 700 px replacement and the 1024 px split. The narrow team image now has the
   connection state, four counts and the TUI's order. The gallery's and the document's links resolve.

The advisory points (rank of the 390 px finding, the theme flash, the test table, help grouped from
`KEYS.scope`, formatting through `format.ts`'s owner) are all taken.

## Amend, without another review

- **A default tier.** §views still describes new behaviour that neither T0's list nor the T1 and T2 lists
  name: the context menu's grouping, hints, Home/End and clamping by rendered size; a visible Refresh on
  fetched tabs; the terminal's attachment label; two-line switcher rows; sticky dialog footers and inline
  errors; the copy of the availability table; textual latest values beside the usage chart. Say in one
  sentence that anything new in §views that T0 does not name is optional under T1's rule (a measured
  delta inside the slice's allowance). That is how I will read it in the slice reviews.
- **Images against the 40 px rule.** The tasks, DAG, events and usage images are of views other than Team
  and show the 94 px row. Regenerate them, or note that the text wins.

## A risk the document cannot settle: the stylesheet

The script allowances look workable for T0. The stylesheet is where I expect the stop rule to fire.
Twenty-five colour tokens with two values each are about 1,000 bytes with `light-dark()`, against about
400 for today's single palette, and the type, spacing and radius scale adds up to another 400 unless it
replaces literals: the token block alone is 650–1,100 bytes of the foundation's 700. What remains for the
whole visual redesign is about 700 bytes net across three slices, while T0 adds two rows of chrome, the
toolbars and legend, two-line rows, menus and two sets of media queries. It can only fit if as many bytes
are deleted as are written.

P4 does not stand in the way of a decision: after every allowance 16,162 bytes remain under 600,000. A
raise of P2 alone to 34,000 would remove the risk; P1 to 228,000 as well would also fund T1 and the two
List modes, and the bundle would still be at most 595,838. Both are the operator's decision (the task
card lists the budgets under §keep). Put to the orchestrator; until an answer the budgets stand as the
document has them, and the foundation's measured `F2` will be the first hard evidence either way.
