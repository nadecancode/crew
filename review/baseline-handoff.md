# Dashboard baseline at `6049cdf6`: handoff confirmation (agent: reviewer)

Checked: `main` at `6049cdf63e36e303327235834f23a798aa5a82c1` on 2026-10-05, together with the two
recovery reports written against it. This closes the dashboard revamp and the parallel-work round. The
native applications the operator asked for afterwards are separate work and are not covered here.

**Verdict: the baseline is accepted** as the unchanged, reviewed target. No product source, test or
asset changed during recovery. Two gaps stay open and are listed below. Neither changes what the
baseline is; the first is a defect that still has to be fixed.

## What the baseline is

- `main` differs from the reviewed integration `2fab6d86` only in
  [final-neutral-and-overlap.md](final-neutral-and-overlap.md) and
  [runtime-delivery.md](runtime-delivery.md).
- `web/`, `crates/crew/web-dist` and `crates/crew-tui` have the same Git trees as the reviewed
  `9d2e8387`. `crates/crew/src`, `crates/crew/tests`, `crates/crew-core`, `Cargo.toml` and `Cargo.lock`
  have the same trees as the reviewed `a700f29e`.
- The committed bundle's source is `fnv1a64:f22f3cf5ad74d25f`. Script 230,415 bytes (85 under its
  ceiling), stylesheet 33,906 (94 under), terminal chunk 332,915, total 597,922 (2,078 under). The
  ceilings are unchanged.

## The recovery reports

Three commits, each adding documentation and evidence only. Take exactly these, in this order; they
apply to `main` without conflict (checked by applying them to a copy of `main`).

| commit | adds | reviewed as |
|---|---|---|
| `15a5aa0d` | [current-production-verification.md](current-production-verification.md) | `verify-current-delivery`, approved |
| `1f789299` | provenance wording in `notes/web-revamp.md` | `verify-delivered-design`, approved |
| `0a1c1f84` | the current verdict in `notes/web-revamp.md` §signoff, two captures and `notes/web-revamp/recovery-6049cdf6/evidence.json` | `verify-delivered-design`, approved |

Do not merge the `agents/designer` branch itself: it also holds a native draft and a copy of
`notes/native-architecture.md` older than the approved one.

## Evidence

| | result | whose run |
|---|---|---|
| Tree comparison with `2fab6d86`, `9d2e8387`, `a700f29e` | equal as stated above | mine |
| `just web-check` and a debug build of `6049cdf6` | bundle matches its sources | `qa` |
| `live` web project against a real daemon, one worker, original thresholds | 36 of 36 in 7.1 minutes | `qa`; I read the log and the extracted measurements |
| `--test web_bundle`, `test/budget.test.ts` | 2 of 2, 5 of 5 | `qa`; I read the logs |
| Bundle sizes from `manifest.json` | as above | mine |
| `notes/web-revamp/contrast.mjs` on `6049cdf6` | 57 checks pass | mine and `designer` |
| `web/src` since the neutral sign-off at `06403744` | only `format.ts` differs; the stylesheet is the same blob | mine |
| Two new captures (dark 1440 px inspector, light 390 px task detail) | neutral grey dark surfaces, no overflow | `designer`; I looked at both |
| The running dashboard and operator daemon | see "What is live" | `qa` and mine, independently |

`qa`'s logs are in its own worktree, `.crew/worktrees/qa/target/recovery-*`, not in the root
checkout's `target/`.

Reused without rerunning, because the code they cover is unchanged: the 967 workspace tests, the 197
frontend unit tests and the 229 `mock` and `perf` browser checks recorded in
[final-neutral-and-overlap.md](final-neutral-and-overlap.md).

## What is live

Read from the running processes, without stopping or pairing anything:

- The root checkout has been at `6049cdf6`, clean, since 12:12. Its release binary was built at 12:24
  (SHA-256 `d40f2155…b310`). The dashboard server (pid 2657615, started 12:25) and the daemon of the
  operator's team (pid 2658115, started 12:26) both run that file.
- The dashboard server answers with the seven files of the committed bundle, byte for byte. Its build
  name, `0.1.0+d5063fc1d304fc9a`, ends in the FNV-1a hash of the committed `manifest.json`.

So the neutral palette, the planning instructions and parked tasks are in effect for the operator's
team and dashboard.

## Findings from the final review, and where they stand

| finding in [final-neutral-and-overlap.md](final-neutral-and-overlap.md) | now |
|---|---|
| The `live` web project was not rerun on the final source | **closed**: 36 of 36 on `6049cdf6` |
| Nothing is live until the binary is rebuilt | **closed** for the operator's team and dashboard; open for the `demo` session, below |
| `notes/web-revamp.md` named the earlier bundle as the current target | **closed** by `1f789299` and `0a1c1f84`; the earlier sign-off is kept as the preceding target |
| Light appearance keeps its green action colour by the orchestrator's reading | unchanged; still not put to the operator |
| Parking does not look at the work tree | unchanged; agents are told to commit first |
| 85 bytes of script and 94 of stylesheet left | unchanged; any web change must be measured |

## Open gaps

1. **Focus is lost after closing a task detail reached across views (RECOVERY-FOCUS-1).** Found by
   `designer`. Open an agent on the Team graph, open one of its tasks from the inspector, switch to
   Tasks with the detail still open, press Escape: the detail closes and focus falls to the document
   body. `web/src/ui.ts` remembers the element that opened the panel only when the panel opens from
   nothing (line 126), and `closePanel` (lines 156 to 165) falls back to an agent node, which the Tasks
   view does not have. Opening and closing a task from the Tasks list itself returns focus correctly.
   This is in the approved source and predates this round; I did not find it in the final review. It
   is a keyboard and screen-reader defect of medium severity (focus order, WCAG 2.4.3): nothing is
   trapped or lost, the user starts again from the top of the page. It is tracked on the tasks
   `web-focus-fallback`, `web-focus-regression` and `web-focus-integration`, and has to be corrected
   before the project as a whole is delivered. The fix has to fit in the 85 bytes left, or free some.
2. **The `demo` session still runs the previous binary.** Its daemon (pid 1925176, workspace
   `demo-workspace`, started 2026-10-04) holds the deleted earlier release image. It gets the new
   scheduling only after it is stopped and started with the current binary. That is the operator's to
   do when its work is done; nothing was stopped here.

## Not covered

- A screen-reader pass. The recovery check was visual and bounded.
- The native applications, the shared client crates and the remote gateway: under review separately.
