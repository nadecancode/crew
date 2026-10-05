# Web dashboard, neutral dark theme: review of the integrated target (agent: reviewer)

Reviewed: bundle commit `06403744` on `agents/qa` at `6f851ae0`, the integration of `ui` `2578c713`,
`graph` `15ed4cdf`, `e4b58f21` and `89034e85`, `content` `0b072f44`, `qa`'s checks `d35a8fcc` and
`2069b4c2`, the designer's contract `60db8e08` and `content`'s audits; then the README note `83b72e07`
and the designer's visual sign-off `b36fbc1d`, neither of which touches `web/` or `crates/`. Against
`main` at `15de7cf`. The contract and the test approach are reviewed in
[web-neutral-design.md](web-neutral-design.md); this file is about what ships.

**Verdict: approved.** No finding is open.

## The tree

- `web/src` differs from `main` in four files and nothing else:
  - `theme.css`: the dark values of ten tokens (eight neutrals, the action colour and its surface), the
    three terminal tokens, and `--token-output` as its own literal. No light value changes, except that the terminal, which is dark
    in both appearances, takes the neutral values in light too.
  - `term/Terminal.ts`: the four fallback colours, now equal to the tokens.
  - `styles/graph.css:31`: a selected node or task is outlined in the bright neutral in dark, and in the
    action colour in light as before.
  - `styles/content.css:9`: the 3 px selection marker is on the row of every list, not only on message
    and event rows.
- No selector is renamed, no geometry changes, no script logic changes. No Rust source, no dependency,
  no `web/index.html`, no build configuration.
- `README.md` gains four lines (`83b72e07`): the page is served from the binary, so a frontend change is
  seen after rebuilding `crew` and `crew web --restart`. Each statement agrees with `build.rs`, the CLI
  and what the README already says about a restart.
- Tests: `web/e2e/theme.spec.ts` is new (15 tests); `web/dev/gallery.ts` records its revision through jj
  where there is no git checkout, and its viewer page lost its green tint. Nothing else under `web/e2e`,
  `web/test` or `web/dev` changes.

## The bundle

| | measured | on `main` | ceiling | under by |
|---|---|---|---|---|
| P1 script without the terminal | 230,360 | 230,360 | 230,500 | 140 |
| P2 CSS | 33,906 | 33,888 | 34,000 | 94 |
| P3 terminal chunk | 332,915 | 332,915 | 350,000 | 17,085 |
| P4 `web-dist` | 597,867 | 597,849 | 600,000 | 2,133 |

The committed `web-dist` is byte for byte my own build of the same sources (source
`fnv1a64:50c88732a97c7d7b`), and a rebuild of the reviewed tree in a temporary directory finds no
difference. `cargo test -p crew --test web_bundle`: 2 of 2. The gates in the tests are unchanged.

## Own runs on the reviewed tree, against the committed bundle

- `bun run test`: 23 files, 175 tests.
- `mock` and `perf` projects: 225 of 225 in 1.8 minutes on a quiet host (the 210 of before, and the 15
  of `theme.spec.ts`). First paint 92 ms at most (150), live 175 ms at most (300), workspace switch 49 ms
  at p95, heap 7.9 MB, 25 log rows in the DOM for 15 visible with 5,000 events, no frame and no short
  timer in 3 s idle, no index frame and no call in 60 s.
- `node notes/web-revamp/contrast.mjs`: 57 checks pass.
- Two mutations, to see that the new tests bite. With `graph.css:31` back on the action colour, the dark
  selection test fails. With `content.css:9` back on message and event rows only, six of the ten list
  tests fail (team list, task list and transcript, in both appearances).
- Own captures of the built page, dark and light: Team graph and list, Tasks graph, list and detail,
  Events with a selected warning row, Usage, inspector, terminal, switcher. Canvas, panels, nodes, text
  and terminal are neutral charcoal and grey; actions, focus and the orchestrator are blue.

Not rerun: the `live` project against a real daemon (36 of 36 at `a8829763`). Nothing it exercises
changed: the difference is colour values and one CSS selector. `qa`'s and the designer's galleries are
theirs; I read their figures, not every image.

## Found during the review, all closed

- **R-GREEN-1** (blocker): green-tinted dark neutrals and a mint action colour. `2578c713`, `15ed4cdf`.
- **R-GREEN-3** (major): a selected graph node had the hue of keyboard focus and of the orchestrator's
  frame once the action turned blue. `e4b58f21`, `89034e85`.
- **R-GREEN-4** (minor): hovered and selected rows share one grey in dark. Accepted for message and event
  rows, which carry a marker. My analysis stopped there, which was a mistake:
- **R-GREEN-5** (major, found by the designer on the integrated page): the team list, the task list and
  the transcript had no marker, so in dark a selected row and a hovered row were the same. `0b072f44`
  puts the marker on every list. In light those three lists gain the marker too; the designer accepts it.

## Known, left as it is

- Light appearance keeps its palette by decision: the action colour there is still green (`#116e59`) and
  the neutrals keep a slight warm-green tint.
- Green remains in dark where it carries meaning: the live dot, done, version control, diff additions,
  the codex runtime and the usage "output" series.
- The mark and the gauges follow the action colour, so they are blue in dark.
- 140 bytes of script and 94 of CSS are left under the ceilings. Any later change has to be measured
  first.
- Everything listed under "Left as it is, known" in
  [web-revamp-integrate.md](web-revamp-integrate.md) still holds; this round changed colour only.
- The mockups under `notes/web-revamp/mockups` and the first sign-off gallery still show the green
  palette. They are marked as history.
