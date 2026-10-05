# Final dashboard and parallel-work acceptance

Integrated source checkpoint: `752b0b7f` on `agents/qa`. This combines the
reviewed dashboard/planning milestone `a7c54a10`, projection source
`fb0c3a66` + `b11dd5a9`, paired QA `97de7b68`, and runtime's eight reviewed
commits through `a700f29e`. No moving source branch was merged.

The agent can park Waiting work, execute an independent Ready card, retain an
answer received during that card, then resume the original parked card once.
One executing card, genuine dependency/review gates, explicit parked-card
operations, original task identity/start order, and restart recovery remain
covered. Default task operations follow executing work. Web/TUI current work
shows executing work ahead of parked cards; Waiting cards keep their own
status and remain individually inspectable.

## Integrated checks

The final core Rust tree equals `a700f29e`. Web, production assets, and TUI
equal the reviewed `9d2e8387` tree. README retains the neutral reload note,
parked scenario row, and runtime's parking/explicit-task guidance. There were
no merge conflicts or conflict markers.

Fresh commands on this merged source, from the repository root:

```sh
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH ./scripts/build-env cargo test --workspace
./scripts/build-env cargo fmt --all -- --check
./scripts/build-env cargo clippy --workspace --all-targets -- -D warnings
just web-check
```

Workspace: **967 passed, zero failures, two ignored**, across 27 suites.
This includes eight real-daemon overlap regressions, five private runtime
regressions, the original team suite, TUI projection checks, and `web_bundle`
2/2. Fmt and strict clippy pass. `just web-check` reports
`web-dist matches the sources`. Logs are in QA's ignored `target/` as
`runtime-delivery-{workspace,clippy,web-check}.log`.

Reviewer independently checked this unchanged web tree: **197 unit tests,
229 mock/performance checks, and 57 contrast/neutrality checks**, all green.
First paint at most 72ms, live at most 153ms, workspace switch p95 49ms,
zero idle frames/timers. Heap varied from 10.6 to 17.3MB across runs, below
the 60MB ceiling. A quiet host window was coordinated for performance.
These results apply after the core/README-only merge; no web suite was repeated.

## Bundle and evidence

Bundle commit: `9d2e838747fefae2684c66dd56ae854215446bbd`.
Source: `fnv1a64:f22f3cf5ad74d25f`.

| Gate | Bytes | Ceiling | Remaining |
| --- | ---: | ---: | ---: |
| P1 nonterminal scripts | 230,415 | 230,500 | 85 |
| P2 stylesheets | 33,906 | 34,000 | 94 |
| P3 terminal scripts | 332,915 | 350,000 | 17,085 |
| P4 manifest payload | 597,922 | 600,000 | 2,078 |

The palette and layout retain the
[approved neutral appearance](../notes/web-revamp/neutral-signoff/index.html).
The later projection correction changes task selection logic only.
[Current production task-view samples](runtime-presentation/index.html)
retain six captures from graph's 30-image matrix on the final source:
independent work, older resumed work, started Blocked work, separately
inspectable Waiting details, and both themes. Review-to-Waiting transition is
covered by the four paired browser cases rather than that screenshot matrix.
The mock is reproducible with `bun run mock --fixture subagents --scenario parked`;
its first `step` submits independent work for review, the next resumes the
older card with its original start time and preserves the newer Waiting card.

[Core review](runtime-overlap.md),
[neutral review](web-neutral-integrate.md), and
[planning review](scheduler-parallel.md) record the accepted source and scope.
The before-source projection tests failed nine unit cases and all four browser
cases; the corrected source passes twenty projection cases and all four
browser cases. The full unit count includes scenario determinism and phase
identity checks.

## Landing and activation

QA's worktree is `/home/nade/workspaces/personal/crew/.crew/worktrees/qa`.
The operator checkout is `/home/nade/workspaces/personal/crew`, Git branch
`main`, clean at `15de7cf0` before final landing. The jj workspaces share that
checkout's Git object store, so the reviewed ready commit is already available
to Git. The orchestrator performs the final fast-forward of that checkout to
the exact reviewed QA head; no jj repository is initialized in the root.

The new executable's `--help`, `web --help`, `run --help`, and `stop --help`
were checked locally. After landing, build with
`./scripts/build-env cargo build -p crew` or install with
`./scripts/build-env cargo install --path crates/crew`.

For the dashboard alone, use the new executable explicitly:

```sh
./target/debug/crew -C /path/to/project web --restart --no-open
```

Open its new pairing address. This replaces the web listener and keeps the
team daemon running. The new scheduling code becomes active in a newly
started team daemon because the old daemon pins its original binary. After
ongoing work has finished, stop that team with the new executable and start
its existing team file again, for example `crew -C /path/to/project stop`
then `crew -C /path/to/project run team.yaml`. No active team was stopped here.

Light appearance keeps its existing green action colour by the operator's
dark-only scope decision. Dark structural surfaces are neutral gray/black.
Script and CSS headroom remain small and must be measured for future changes.
