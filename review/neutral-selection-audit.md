# Persistent selection in neutral list views

Task: `neutral-selection`. Finding: R-GREEN-5.

Dark hover and selection both use `#242424`. The original blue selection marker covered message
and event rows only, leaving Team/Task lists and Transcript dependent on the identical fill. When
one row was selected and a different row hovered, both looked selected.

Source `0b072f44` changes the existing selector from `.vrow.selected :is(.bus-row, .log-row)` to
`.vrow.selected > *`. Every current `VirtualList` row has one full-height direct child, so the same
3px inset marker now persists in all five lists. Keeping the marker on that child ensures opaque
warning/error event backgrounds cannot cover it. The selected event background override remains.
Graph confirmed the entity/transcript structure and satellite lineage border; no graph markup change
was needed.

Dark markers are blue. Light Team/Task/Transcript rows now receive the existing green marker used
by message/event rows. Designer explicitly accepted this light interaction cue addition; light
palette tokens are unchanged. Focus, selection identity, and keyboard operations retain their
existing behavior.

## Candidate and validation

- Integrated source base: QA merge `ce003f21`, with source fingerprint `fnv1a64:cf8956b55a22c53c`.
- Source correction: `0b072f44`.
- Paired regression: QA `2069b4c2108dc0935b0f98d85b3aacc090e8dee7`, applied here as `3543e249`.
- Verified corrected source: `fnv1a64:50c88732a97c7d7b`.

`bun run build` succeeds. `bun run test` passes 23 files and 175 tests. The paired browser run passes
55 of 55:

```sh
cd web
PLAYWRIGHT_EXECUTABLE_PATH=/etc/profiles/per-user/nade/bin/brave bunx playwright test \
  --project=mock e2e/theme.spec.ts e2e/lists.spec.ts e2e/content.spec.ts \
  e2e/graph.spec.ts e2e/keyboard.spec.ts e2e/reviewer-resources.spec.ts \
  --output=test-results/neutral-selection-paired --workers=2
```

The new ten regression cases exercise Team, Tasks, Transcript, messages, and event log in both
themes. A keyboard-selected row retains `aria-selected="true"` and a visible direct-child shadow
while a neighboring row is hovered. Warning/error events test their opaque backgrounds explicitly.
QA's pre-fix run failed the six Team/Task/Transcript cases and passed the four message/event cases;
the corrected source passes all ten. Existing checks cover scroll anchoring, tail following, bounded
rings, keyboard/graph operations, cached evidence, and the 36px/64px responsive event transition.

The [capture script](neutral-selection/capture.mjs) records actual selected-versus-hover states at
1440×960, using the `busy` scenario, `subagents` fixture, frozen time, and reduced motion. It writes
under ignored `web/test-results/neutral-selection`. Run after building:

```sh
PLAYWRIGHT_EXECUTABLE_PATH=/path/to/local/chromium node review/neutral-selection/capture.mjs
```

The committed [summary](neutral-selection/summary.json) contains 14 successful captures: all five
lists in both themes plus selected warning/error events in both themes. Every selected direct child
has the 3px marker; every hovered unselected direct child has `box-shadow: none`. Dark selected and
hovered fills remain the same neutral gray, with the marker providing the persistent distinction.
Rendered contrast, names, and document-width overflow checks have zero failures. Manual image
inspection confirms the Team, Task, and Transcript cues below.

Measured row heights remain Team/Task 160px, Transcript 22px, messages 84px, Events 36px desktop.
The existing responsive event test also verifies the unchanged 64px narrow rows and scroll anchor.
The source change is box-shadow only, so it does not alter layout bounds.

## Bytes and handoff

| Gate | Before R-GREEN-5 | Corrected candidate | Delta |
|---|---:|---:|---:|
| P1 nonterminal JS | 230,360 | 230,360 | 0 |
| P2 CSS | 33,927 | 33,906 | −21 |
| P3 terminal JS | 332,915 | 332,915 | 0 |
| P4 manifest asset total | 597,888 | 597,867 | −21 |

All approved ceilings hold. The shorter shared selector saves 21 minified CSS bytes. Review evidence
adds zero shipped bytes. Generated `web-dist` output is restored before this artifact commit; QA
owns the rebuilt integrated bundle (`06403744`, matching source `fnv1a64:50c88732a97c7d7b`). The
[original content audit](neutral-content-audit.md) is marked with this follow-up so its earlier no-op
finding cannot be mistaken for the final selection result.

## Representative selected-versus-hover captures

| View | Dark | Light |
|---|---|---|
| Team List | [Screenshot](neutral-selection/team-dark.png) | [Screenshot](neutral-selection/team-light.png) |
| Task List | [Screenshot](neutral-selection/task-dark.png) | [Screenshot](neutral-selection/task-light.png) |
| Transcript | [Screenshot](neutral-selection/transcript-dark.png) | [Screenshot](neutral-selection/transcript-light.png) |
