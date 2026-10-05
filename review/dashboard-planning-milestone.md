# Dashboard and planning integration milestone

Candidate source tree: `f1ec1453`, based on the approved neutral integration
`e7206475`. Runtime overlap and presentation corrections remain separate work
for `runtime-delivery`; this milestone contains only reviewed artifacts.

The web tree and production bundle are byte-identical to `e7206475`:
bundle `06403744`, source `fnv1a64:50c88732a97c7d7b`, P1/P2/P3/P4
230,360 / 33,906 / 332,915 / 597,867 bytes. No web rebuild or browser suite
repeat was needed. [Neutral integration verification](web-neutral-qa-integration.md)
records the 225 browser/performance checks, 56 strict captures, and final
[visual sign-off](../notes/web-revamp/neutral-signoff/index.html).

The Rust and planning-note delta equals scheduler's reviewed commits
`c2d31596`, `5e639b57`, and `8d619d9e` exactly. The correction documents
completion/review gates and independent role preparation, with two real-daemon
regressions and one scheduler example. The separate date fixture repair makes
fake Codex rollout lookup independent of a fixed calendar date.
See [planning review](scheduler-parallel.md) and
[scope/verification note](../notes/scheduler-parallel.md).

## Fresh checks on this integrated tree

```sh
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH ./scripts/build-env cargo test --workspace
just web-check
```

Workspace tests pass: 948 passed, zero failures, two ignored. This includes
`web_bundle` 2/2, fake CLI 3/3, and the two new daemon overlap/review regressions.
The test inventory contains 950 cases. Dependency build artifacts were copied
read-only from scheduler's warm target into QA's own target; all three workspace
crates rebuilt against QA paths. No shared target was modified.

`just web-check` reports `web-dist matches the sources`. Read-only tree
comparisons confirm the exact scheduler delta, identical web/assets/README,
and no unreviewed runtime or presentation source in this milestone.

## Loading each change

Supported commands were checked locally with the built binary's `--help`,
`run --help`, `stop --help`, and `web --help`.

To load the dashboard assets, use the newly built executable explicitly:

```sh
./target/debug/crew -C /path/to/project web --restart --no-open
```

Open its fresh pairing address. This replaces the web listener and leaves
the team daemon running. Alternatively, install with
`./scripts/build-env cargo install --path crates/crew`, then use that new
executable for the web restart.

The team daemon pins the binary it started with. Planning or scheduling code
becomes active in a newly started team daemon; reinstalling or restarting only
the web listener does not update it. After ongoing work is complete, the
supported lifecycle is to stop that team with the new executable and run its
existing team file again, for example `crew -C /path/to/project stop` followed
by `crew -C /path/to/project run team.yaml`. The saved roster is reused unless
`--replan` is requested. No active team was stopped during this integration.

Same-agent Waiting/Ready overlap is still required for the full operator
request and is accepted separately in `runtime-delivery`.
