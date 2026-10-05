# Web revamp, `qa` harness review (agent: reviewer)

Reviewed: `f5e346c0`, `43f7dae8`, `24145c25`, `13255cf9` on `agents/qa` (budget gates, mock scenarios,
gallery, accessibility collector, read-state specs, README), in a scratch worktree with the approved
foundation fix `6f5ae5d5` applied on top. Design: `notes/web-revamp.md` at `8675c845`.

**Verdict: changes requested.** The harness is sound in structure and everything it runs is green; three
things make its evidence unreliable for the reviews it is meant to serve.

## What holds

- The four commits touch `web/dev`, `web/e2e`, `web/test`, `web/package.json` (one script) and the README:
  no file under `web/src`, nothing in `crates/crew/web-dist`.
- Own runs on `13255cf9` + `6f5ae5d5`: build P1 213,570, P2 29,227, P4 576,379; `bun run test` 21 files,
  163 tests green; `playwright test --project=mock` 157 of 157 green, `mock.spec.ts` among them.
- `f5e346c0` and `43f7dae8` change P1 and P2 to 228,000 and 34,000 in `e2e/perf.spec.ts` and
  `test/budget.test.ts` and nothing else; P3, P4 and the three runtime dependencies are as they were.
- Scenarios are layered on the generated fixtures through the controller's own pushes; the fixture file is
  read back unchanged in the test. Time is frozen for a scenario, activity is a typed `step` or an
  explicit `--activity`, and a held read is released by a call, not a timer.
- The gallery refuses an output directory inside the repository (also through a symlink), redacts the
  origin and pairing fragment from errors, records page errors, CSP violations and requests after load per
  capture and exits non-zero on any. A sample run here (attention and limits, five surfaces, both schemes,
  desktop and tablet) wrote 40 images with 0 failures.
- The inert-markup test pushes `<img onerror>` and `<script>` in a message body and asserts no element, no
  request and no CSP report. The read-state specs cover pending, empty, unavailable, truncated, failed,
  and a late reply for another agent.

## Required

1. **Every capture of the `attention` scenario is of the question dialog.** The scenario seeds two
   operator questions. `gallery.ts:184-187` waits for the dialog and presses Escape once; the page then
   opens the second question, as it should. All twenty attention captures of the sample (team, tasks,
   events, usage, inspector) show "question 2 of 2" over a scrim, are reported `captured`, and the audit
   reads the dialog (two disabled `send` buttons). The state the revamp is judged on is the one state the
   gallery does not show. Put off every open question before a surface that is not a dialog, and fail the
   capture when a `[role=dialog]` is open for such a surface.

2. **Two scenario values are not the daemon's vocabulary**, so captures show text the product never
   shows:
   - quota windows `five_hour` and `seven_day` (`scenarios.ts:90-92`). The daemon names them `5h`, `week`,
     `week/<model>`, `<n>d`, `<n>h` (`crates/crew/src/limits/mod.rs:84`), and the page labels `5h` as
     `session` (`views/usage.ts:11`). The strip and the usage view print `five_hour`.
   - `limit.kind: "usage_limit"` (`scenarios.ts:98`). The wire values are `usage`, `credits`, `auth`
     (`limits/mod.rs:47`, `daemon/limits.rs:128`); the inspector prints the kind.
     (`block_kind: "usage_limit"` is right: `daemon/limits.rs:22`.)

   `decodeSnapshot` accepts any string there, so the scenario test cannot catch it. Use the real names,
   and add to `attention` an agent blocked with `block_kind: "refused"` (or `provider_error`) and its
   error text: the design's table of retained features lists that state and no scenario has it.

3. **The accessibility collector asserts nothing.** `auditPage` records contrast, names, overflow and
   focus; the gallery's exit code ignores them and the spec expects only `contrast.checked > 0`. The
   design asks `qa` for contrast and responsive checks, and the slices will be reviewed against this
   tool. On the foundation the sample finds exactly one kind of failure, the disabled `send` button
   (2.02 light, 2.71 dark), which WCAG exempts as an inactive control; nothing unnamed; no overflow at
   1440 or 1024. So a gate is within reach now: a strict mode that fails on a contrast failure outside
   disabled controls, on an unnamed control, and on document overflow, with the viewports where overflow
   is still expected (390 until the shell lands) named explicitly rather than ignored.

## Advisory

- The contrast audit composes CSS backgrounds only; text in the graph's nodes sits on an SVG `rect` and is
  measured against the canvas. The note in the report says so; a selected or tinted node will need eyes.
- `targetsBelow44px` lists every 32 px control at every viewport. Since 44 px is T2 and only for coarse
  pointers, report it for the narrow viewport only, or it will be noise in every review.
- `Tab-only path` proves the graph, the panel and the composer are reachable. Shift+F10, the separator
  and the narrow replacement region arrive with the slices and need their tests through the hand-over.

## Re-check at `4f5f2a7e` (with `9fbc3b87`): approved

`9fbc3b87` tightens the output guard (a directory named `..x` inside the repository was taken for
outside; the check now runs before anything is created) and tests it. `4f5f2a7e` answers the three
findings:

1. `dismissQuestions` puts off as many dialogs as there are open questions, and `assertNoDialog` fails a
   capture of any surface that is not itself a dialog. In my re-run the attention captures show the team
   view with its warnings, the usage strip, the question banner ("and 1 more") and the nodes.
2. Quota windows are `5h` and `week` (shown as `session` and `week`), `limit.kind` is `usage`, and
   `attention` has an agent blocked with `block_kind: "refused"` and its error text, drawn as refused.
3. `--strict` fails a capture on a contrast failure outside disabled controls, an unnamed control, or
   document overflow; overflow is excused only for viewports named in `--allow-overflow`. Checked both
   ways: the strict sample (attention and limits, five surfaces, both schemes, desktop and tablet) is 40
   images with 0 failures, and a strict run at the narrow viewport without the allowance exits 1 on
   "Document overflow: 968px > 390px". The collector now reads the node's SVG fill and lists 44 px targets
   under 640 px only.

Own runs on `4f5f2a7e`: build P1 213,570, P2 29,227, P4 576,379, nothing shipped by the harness; 22 unit
files, 165 tests green; `mock` project 159 of 159 green twice.

**To watch at integration.** In the first of three full runs `e2e/anim.spec.ts:82` ("an old event plays
nothing; an idle page asks for no animation frame") failed once; it passed in the two full runs after and
in three runs of the file alone. It is an existing test that the harness does not touch, on a machine
other agents were loading. If it fails again it is P13 and needs a cause, not a retry.

**Standing instruction for slice reviews.** Slices are judged with `--strict`; the only allowance at the
foundation is `--allow-overflow narrow,zoom`, and it is to be gone by integration.
