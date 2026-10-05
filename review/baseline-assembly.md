# Reviewed baseline and native validation tooling assembly

The dashboard recovery baseline is accepted at
`6049cdf63e36e303327235834f23a798aa5a82c1`. This assembly retains its exact
production Rust, web, TUI, manifests and `web-dist`; it adds accepted recovery
documentation/evidence and accepted native validation preparation. No native
application, shared-client implementation or gateway is included.

The orchestrator extended this card's scope to retain approved QA preparation
(`m-546c84223d01`). Its source and evidence remain explicitly preparation,
including the operator's deferred Apple execution. No branch was rewritten and
no root checkout, active team or listener was changed here.

## Exact artifact inventory

| Original reviewed commit(s) | Included paths |
| --- | --- |
| `15a5aa0dbe2d504d65d82cc60c312d8d428c1693` | [current-production-verification.md](current-production-verification.md) |
| `1f789299bef340ff40db1bcefa22629f26a9cf67`, `0a1c1f842253f3c11b6193b86ce11bbefb6fa7b4`, `8958d97db48a1698381ee809cb033aed098cd0aa` | [web revamp sign-off](../notes/web-revamp.md), two focused PNGs and [recovery evidence](../notes/web-revamp/recovery-6049cdf6/evidence.json) |
| `cc430b5de8406f76371faaa476683f6726410338`, `a1aa85d6b32cb331c51c38b0f840f25ab0d61a08` | [baseline-handoff.md](baseline-handoff.md) |
| `a356a1b419ce6858162ec8804fdfcd48537d6420`, `d28e76f697ca5d59e22accde8719a8e26d043fc1` | `scripts/native/**`, [native validation plan](../notes/native-validation.md), [toolkit capture metadata](native/tooling-preparation.json) and its PNG |
| This assembly's report-only commit | This file; explicit QA log directory, activation timestamp chain, completed designer evidence and open focus gap in `current-production-verification.md` |

QA cherry-picked only the named recovery/reviewer commits; designer/reviewer
branches carry unrelated or stale parents. QA's recorded recovery sequence is
`15a5aa0d`, `2d4bb0f5`, `cafc16a4`, `fe4e2f7f`, `ee4c6f0f`, `3042a0fe`;
`a356a1b4` and `d28e76f6` are the separate native preparation checkpoints.
No other artifact is part of this assembly.

## Verification reused and checked

Production tree comparison against `6049cdf6` shows zero changed files for
`web`, `crates/crew`, `crates/crew-core`, `crates/crew-tui`, `Cargo.toml` and
`Cargo.lock`. Recovery report links, image paths and artifact metadata resolve.
The nonproduction diff is limited to the inventory above; native scripts/plan/
toolkit evidence equal their accepted `d28e76f6` checkpoint. The candidate
worktree is clean after this report is committed.

The [production verification](current-production-verification.md) records
`just web-check`, fresh debug build, 36/36 isolated live cases, 2/2 bundle tests
and 5/5 budget checks. Its exact logs remain in QA's
`/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/recovery-*`.
Reviewer independently reproduced the source/bundle equality and read the
logs/measurements. Unchanged full Rust, web unit, mock/performance and contrast
matrices retain the accepted records; assembly does not rerun them.

Bundle source remains `fnv1a64:f22f3cf5ad74d25f`: P1 230,415/230,500 bytes,
P2 33,906/34,000, P3 332,915/350,000 and P4 597,922/600,000. Headroom remains
85 bytes of dashboard script and 94 bytes of CSS.

The [native preparation review](../notes/native-validation.md) distinguishes
the actual GTK toolkit capture/negative-case/cleanup proof from unperformed
native application/real-daemon GUI, shared-engine and Apple tests. Reviewer
replayed the toolkit runner and absence gates independently. This assembly
does not turn preparation into app acceptance.

## Live status and retained gaps

As observed on 2026-10-05: root clean at `6049cdf6` since 12:12:21, release
binary built at 12:24:53, production listener PID `2657615` started 12:25:44,
operator-team daemon PID `2658115` started 12:26:01. Both executable images
equal root release SHA-256
`d40f215508b87e3d5cbf7a9e86336d5c63b93ee3059f3eb7dbc7eba70fdbb310`;
all seven served files equal the committed bundle. Dashboard and operator
team are already active; no reload is needed for this evidence/tooling landing.

Two gaps remain in the [accepted baseline review](baseline-handoff.md):

- `RECOVERY-FOCUS-1`, medium: closing a focused task detail after crossing
  views can drop focus to BODY at 1440px and after a 390px resize. The direct
  Task List path restores focus. Correction/regression/bundle integration are
  separate `web-focus-*` cards and are not complete here.
- Separate `demo-workspace` daemon PID `1925176` retains the deleted previous
  executable. The operator can stop/start it with the current binary after
  its work finishes. No active process was stopped here.

Native iOS/Linux/macOS products and the scoped remote gateway remain separate
implementation and validation work. Apple execution remains deferred by the
operator. The reviewed assembly head is handed to the orchestrator by exact
commit for the root fast-forward after its final review; no moving branch merge
or listener/team restart is requested.
