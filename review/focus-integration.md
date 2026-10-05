# Keyboard-focus correction: integrated acceptance

Closing a task detail after crossing from Team to Tasks now returns keyboard
focus to an available task or the current view control. A saved opener must be
connected and visible. When the task has left the board, focus returns to Tasks.
Direct Task List and Team Graph open/Escape behavior remains covered.

Reviewed source: `a20d78405151b5d91c103929e0efeaddcb08774d` (only
`web/src/ui.ts`), applied in QA as `86c43312`. Independent regression:
`bfe84371df4631d1938bb2e395660aaed6e3960e` (five browser cases only).
Production bundle: `af94debf57a63dada4404680f6230b0823a54bfd`.
Parent baseline and native preparation are the approved `4a67a03e` assembly;
their documentation and source are preserved. No native product branch or
private UI baseline-import commit was merged.

The regression fails on `6049cdf6`/`fnv1a64:f22f3cf5ad74d25f`: 390px resize
and 1440px reading-region Escape leave focus on BODY; a task removed through
snapshot/resync leaves the Tasks control inactive. Both direct-open controls
pass. The exact fixed source passes all five cases with visible task/view
focus, without post-close focus rescue or sleeps. Reviewer independently
reproduced both results.

## Integrated checks

From QA's worktree repository root:

```sh
(cd web && bun run build)
(cd web && bun run test)
(cd web && PLAYWRIGHT_EXECUTABLE_PATH=/etc/profiles/per-user/nade/bin/brave \
  bunx playwright test --project=mock --workers=2 --output=../target/focus-integration-mock)
(cd web && PLAYWRIGHT_EXECUTABLE_PATH=/etc/profiles/per-user/nade/bin/brave \
  bunx playwright test --project=perf --no-deps --grep 'P1–P4' \
  --workers=1 --output=../target/focus-integration-sizes)
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH \
  ./scripts/build-env cargo test -p crew --test web_bundle
just web-check
bun notes/web-revamp/contrast.mjs
```

Build/typecheck passed; **197/197 unit**, **226/226 full mock**, **1/1 size**,
**2/2 bundle** and **57/57 contrast/neutrality** checks passed. The source-match
check reports `web-dist matches the sources`. Manifest asset set, sizes and
hashes were independently recomputed. Logs are under
`/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/focus-integration-*`;
before/after logs are `target/focus-regression-{baseline-final,fixed}.log` there.

Live and runtime performance matrices were not repeated: this correction
changes only `ui.ts::closePanel`, and reviewer retained the bounded gate
(`m-e1fb934e919b`). Net, store, render loop, Rust source and manifests are
unchanged. The 36-case live evidence remains the accepted baseline result.

## Bundle and release headroom

New manifest source: `fnv1a64:b1e2e05bd16fb63d`.

| Gate | Bytes | Ceiling | Remaining |
| --- | ---: | ---: | ---: |
| P1 nonterminal scripts | 230,497 | 230,500 | **3** |
| P2 stylesheets | 33,906 | 34,000 | 94 |
| P3 terminal scripts | 332,915 | 350,000 | 17,085 |
| P4 manifest payload | 598,004 | 600,000 | 1,996 |

The correction adds 82 bytes of nonterminal script. CSS is byte-identical to
the neutral baseline. Original ceilings, runtime dependencies, dark palette
and light appearance are preserved. The later web-policy adoption shares this
same headroom; it must measure the combined shipped candidate and reclaim
bytes rather than treating the earlier 85-byte allowance as still available.

## Closure and activation

`RECOVERY-FOCUS-1` is closed in this source/test/bundle target. The
[baseline review](baseline-handoff.md) retains its historical finding with a
closure reference. Native application/gateway implementation and the older
`demo-workspace` daemon remain separate work.

No active listener, team or root checkout was restarted or modified here.
The existing production dashboard still serves the earlier neutral bundle;
this source correction becomes active after the reviewed target is landed,
the binary is rebuilt and the dashboard listener is reloaded using the new
executable. The orchestrator owns the root fast-forward; an active team
daemon does not need restarting for this frontend correction.
