# Neutral dark content audit

Task: `neutral-content`, responding to operator `w-763e0ac4eafc8f5b`.

The shared neutral palette fixes the content views without a content source change. Message bus,
Events, task/message/event detail, Usage, and local loading/error/empty states already use semantic
tokens. Their dark backgrounds now resolve to charcoal, selected rows to neutral gray, and actions
and focus to blue. Status, runtime, and the explicitly labelled token categories retain their colors.

## Candidate and scope

- Delivered base: `main` `15de7cf0`, incorporated through merge `387f3434` before verification.
- Shared palette: `ui` `2578c713`, applied here as `a9f6caac`.
- Verified source fingerprint: `fnv1a64:a06acf830c69fb8c`.
- Content source delta: zero. No selector, accessible-name, row-height, list API, RPC, or feature changes.
- Evidence and capture script are review artifacts; they add zero shipped bytes.

`content.css` has no hardcoded themed colors. Canvas, panel, raised evidence blocks, hover,
selection, filters, and missing-data copy inherit the shared palette. Green remains only where
existing semantic tokens identify status/runtime or the labelled output-token series. In particular,
`--token-output` remains separate from the blue action token, preserving the usage legend.

Computed dark canvas is `rgb(18, 18, 18)`, panel is `rgb(26, 26, 26)`, and selected event background
is `rgb(36, 36, 36)`. The list focus indicator is blue `rgb(140, 183, 242)`. The light canvas remains
`rgb(245, 246, 243)`.

## Verification

`bun run build` succeeds. `bun run test` passes 23 files and 175 tests on this candidate.

The following existing browser checks pass 24 of 24:

```sh
cd web
PLAYWRIGHT_EXECUTABLE_PATH=/etc/profiles/per-user/nade/bin/brave bunx playwright test \
  --project=mock e2e/content.spec.ts e2e/lists.spec.ts e2e/reviewer-resources.spec.ts \
  --output=test-results/content-neutral-candidate --workers=2
```

These cover cached task evidence during delayed/failed dependency reads, quiet push refresh,
artifact copy, explicit zero versus unknown usage, quota freshness boundary, single-sample context,
ring bounds, tail following, keyed scroll anchoring, filters, keyboard navigation, and message
request isolation. The same 24 checks also passed before the palette change.

The [capture script](neutral-content/capture.mjs) records 54 real-component screenshots against
the production bundle and the `subagents` fixture. The [summary](neutral-content/summary.json)
contains per-capture dimensions, palette, rendered-text checks, row heights, and failures:

- Six main content surfaces in light and dark at 1440×960 and 390×844.
- The same six surfaces at 720×480 CSS pixels and device scale 2, equivalent to a 200% desktop viewport.
- Empty bus/Events/Usage and long-text bus/Events/task detail at desktop and narrow widths.
- Pending and failed task/message reads, an evicted event, and failed older-event reads at both widths.
- Reduced motion and frozen scenario time throughout.

Result: 5,173 rendered-text checks, zero contrast failures, zero unnamed controls, zero document-width
overflow, zero page errors, zero CSP violations, and zero after-load HTTP requests. Bus rows remain
84px; event rows remain 36px desktop and 64px narrow. The usage table keeps its labelled internal
horizontal scroller. Unknown, zero, stale, pending, failed, and unavailable states remain distinct.

Manual image review confirms charcoal reading surfaces, readable body previews, blue selection/focus,
retained warning/error severity, and legible evidence. The collector checks visible HTML text using
the existing `web/dev/a11y.ts` thresholds; it does not claim a complete assistive-technology audit.
The `agent-browser` session independently checked the selected event background, focus ring, and
row geometry. Local `ui-ux-pro-max` guidance supported text contrast and focus checks.

Replay from repository root after `cd web && bun run build`:

```sh
node review/neutral-content/capture.mjs
```

The full captures are written under `web/test-results/content-neutral`, an ignored directory in the
running workspace. The representative screenshots below are committed with this report.

## Bytes

Measured minified production bytes using the manifest's existing P1–P4 definitions:

| Gate | Delivered main | Shared neutral candidate | Content contribution |
|---|---:|---:|---:|
| P1 nonterminal JS | 230,360 | 230,360 | 0 |
| P2 CSS | 33,888 | 33,903 | 0 |
| P3 terminal JS | 332,915 | 332,915 | 0 |
| P4 manifest asset total | 597,849 | 597,864 | 0 |

The +15 CSS bytes belong to `ui`'s shared palette. All values fit the final approved ceilings in
`review/web-revamp-budget-ledger.md`: 230,500 / 34,000 / 350,000 / 600,000. Generated `web-dist`
output is restored before this audit commit; QA owns the final production rebuild.

## Representative screenshots

| Surface | Evidence |
|---|---|
| Selected message bus, dark desktop | [Screenshot](neutral-content/busy-bus-dark-desktop.png) |
| Selected Events and VCS evidence, dark desktop | [Screenshot](neutral-content/busy-events-dark-desktop.png) |
| Matching light Events | [Screenshot](neutral-content/busy-events-light-desktop.png) |
| Usage with reporting coverage, zero/unknown data, and stale quota | [Screenshot](neutral-content/busy-usage-dark-desktop.png) |
| Failed task read retaining local title and evidence, narrow | [Screenshot](neutral-content/busy-task-error-dark-narrow.png) |
| Uncached message read failure | [Screenshot](neutral-content/busy-message-error-dark-desktop.png) |
| Empty Usage, narrow | [Screenshot](neutral-content/empty-usage-dark-narrow.png) |
| Long Unicode task title and plain-text evidence | [Screenshot](neutral-content/long-text-task-dark-desktop.png) |
