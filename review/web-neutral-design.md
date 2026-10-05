# Web dashboard, neutral dark theme: review of the contract and the checks (agent: reviewer)

Reviewed: the design contract `60db8e08` (`designer`), the checks `d35a8fcc` (`qa`), and the source they
describe: `ui` `2578c713`, `graph` `15ed4cdf`, `e4b58f21` and `89034e85`, `content`'s audit `65484119`.
Against `main` at `15de7cf`. This is the review of the design and the test approach; the integrated
target is reviewed separately.

**Verdict: approved.** No finding is open.

## Why

The operator rejected the dark appearance delivered at `15de7cf` as green ("Dark mode means gray /
blackish"). The complaint is correct, and my approval in
[web-revamp-integrate.md](web-revamp-integrate.md) missed it: I checked contrast, not hue. Two things
made the page green:

- The dark neutrals were green-tinted: canvas `#101716`, surface `#17201e`, raised `#1e2a26`, text
  `#e5eeea`, line `#2c3c35`, control border `#79978a`, terminal `#111815`.
- The action colour was mint (`#71d5ae`) and it is everywhere: the mark, the active view, pressed
  buttons, the send button, gauges, selected rows and nodes.

## The contract (`60db8e08`)

- The dark structural tokens are achromatic: canvas `#121212`, surface `#1a1a1a`, raised and selection
  `#242424`, text `#ededed`, muted `#b5b5b5`, quiet `#a3a3a3`, line `#363636`, control border `#858585`,
  terminal `#101010` / `#ededed` / `#414141`. The dark action is blue, `#8cb7f2`.
- Green stays where it means something: the live dot, done, version control, diff additions, the codex
  runtime, and the usage "output" series, which now has its own literal value instead of aliasing the
  action colour.
- The palette table equals the shipped `theme.css` for every changed value. The figures quoted in the
  document recompute exactly (dark: muted on surface 8.49, quiet at least 5.54, control border at least
  3.79, action on surface 8.43, primary button 9.07).
- The earlier visual sign-off is kept as history and marked superseded; nothing presents the green
  palette as current. The mockups and the old sign-off gallery carry a notice to that effect.
- The work-split section now says that a dependency is a required artifact, not another agent being
  busy, and that candidates are reviewed as they arrive. This round was run that way.

## The checks

- `notes/web-revamp/contrast.mjs` reads `web/src/theme.css` instead of holding a second copy of the
  colours. On the corrected sources: 57 checks pass. On the sources of `15de7cf`: it fails on the twelve
  structural tokens and on the action colour.
- `web/e2e/theme.spec.ts` (five tests, in the `mock` project, so part of `bun run e2e`) resolves the
  tokens in the browser and checks: dark neutrals within 3 of grey and never green-dominant, a non-green
  action, a visible pressed state, text at 4.5:1 and control borders at 3:1 on all six surfaces in both
  appearances, four distinct usage colours, and a selected node or task that differs from both keyboard
  focus and the orchestrator's frame. Own run on the corrected sources: 5 of 5. With
  `graph.css:31` put back to the action colour: the dark selection test fails, as it should.
- `web/dev/gallery.ts` falls back to jj for the revision it records; in an agent workspace `git
  rev-parse` fails, so that fallback is the path taken.

## Found and closed during the review

- **R-GREEN-1** (blocker): the green palette. Closed by `2578c713` and `15ed4cdf`.
- **R-GREEN-3** (major): with a blue action, a selected graph node had the same hue as keyboard focus
  and as the orchestrator's frame, and differed from focus by half a pixel of stroke. Closed by
  `89034e85`: selection is the bright neutral in dark, and stays green in light.
- **R-GREEN-4** (minor): hovered and selected list rows share one grey in dark. Accepted by the designer:
  selected rows keep their 3 px blue marker.

## Measured

Own build of `main` plus the four source commits: source `fnv1a64:cf8956b55a22c53c`; CSS 33,927 bytes
(39 more than `main`, 73 under the ceiling); bundle 597,888 (2,112 under); the script and the terminal
chunk unchanged. `qa`'s bundle commit `a1823f6d` is byte for byte that build.

## Known, left as it is

- Light appearance is unchanged by decision, so its action colour is still green (`#116e59`) and its
  neutrals keep their slight warm-green tint. The operator's words were about dark mode; if the same is
  wanted for light, it is the same kind of change.
- `theme.spec.ts` does not pin the light values (it avoids exact colours on purpose). That light is
  unchanged rests on the source diff, which touches only dark values.
- "Non-green" in the spec means green is not the largest channel; a cyan would pass. The contract's own
  checker requires blue outright.
- `review/neutral-content/capture.mjs:18` falls back to a browser path specific to this machine.
