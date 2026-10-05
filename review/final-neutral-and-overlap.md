# Neutral dark dashboard and overlapping work: final review (agent: reviewer)

Reviewed: `agents/qa` at `2fab6d86`, the integration of everything this round produced. Against `main` at
`15de7cf`, which is an ancestor of it, so `main` can be fast-forwarded.

**Verdict: approved.** No finding is open.

## What the operator asked for, and where it is

1. *"Dark mode means gray / blackish."* The dark appearance had green-tinted neutrals and a mint action
   colour. It is now neutral charcoal and grey with a blue action colour; green remains only where it
   carries meaning (live, done, version control, the codex runtime, the usage "output" series).
   Reviewed in [web-neutral-design.md](web-neutral-design.md) and
   [web-neutral-integrate.md](web-neutral-integrate.md).
2. *Agents idle behind queued work.* Two causes, both addressed:
   - Tasks were made to depend on whole phases. The orchestrator's instructions and the description of
     `depends_on` now say what a dependency costs and when to use one.
     [scheduler-parallel.md](scheduler-parallel.md).
   - An agent waiting on a teammate was not given its next ready task. It is now: the waiting task is
     parked and resumed when its answer has arrived. [runtime-overlap.md](runtime-overlap.md).

## The tree at `2fab6d86`

Checked by comparison, not by trust in the merge:

- `crates/crew/src`, `crates/crew/tests`, `crates/crew-core` and the manifests equal the reviewed
  `a700f29e`.
- `web/`, `crates/crew/web-dist` and `crates/crew-tui` equal the reviewed `9d2e8387`; the bundle's source
  is `fnv1a64:f22f3cf5ad74d25f`.
- `README.md` carries the three additions reviewed separately (reloading the dashboard after a build,
  the `parked` mock scenario, parked tasks) and no conflict marker.
- Everything else added is notes, review records and screenshots.

## Evidence

| | result | whose run |
|---|---|---|
| Workspace tests on the merged tree | 967 passed, 0 failed, 2 ignored | `qa` |
| `cargo fmt --check`, `cargo clippy -D warnings`, `just web-check` | clean | `qa` |
| `cargo test -p crew --lib` on `a700f29e` | 478 of 478, six runs in a row | mine |
| `--test team` (existing daemon tests) | 86 of 86 | mine |
| `--test overlap` (new daemon tests) | 8 of 8 | mine |
| `bun run test` on `9d2e8387` | 197 of 197 | mine |
| `mock` and `perf` browser projects | 229 of 229, quiet host | mine |
| Bundle rebuilt from its sources, and built independently | byte for byte equal | mine |
| `notes/web-revamp/contrast.mjs` | 57 checks pass | mine |

Bundle: script 230,415 (85 under its ceiling), CSS 33,906 (94 under), terminal chunk 332,915, total
597,922 (2,078 under).

The one combination nobody but `qa` has run is the TUI change together with the runtime's protocol
crate; it is inside the 967.

## What to know before using it

- **Nothing is live until the binary is rebuilt.** The dashboard shows the new palette after `crew web
  --restart` with the new build. The planning instructions and the parked-task behaviour apply only to
  a team started with the new build; a running team keeps the binary it started with.
- **Light appearance is unchanged**, including its green action colour. The request named dark mode;
  leaving light alone was the orchestrator's reading of it, not something the operator was asked.
- **Parking does not look at the work tree.** `git_commit` commits everything and attaches the commit to
  the task being worked on. Agents are told to commit before parking; nothing enforces it.
- A message that names no task is offered to each parked task of its recipient in turn.
- 85 bytes of script and 94 of CSS are left under the ceilings.
- Not rerun this round: the `live` web project against a real daemon (36 of 36 at `a8829763`); the web
  changes since are colour values, one CSS selector and one function that picks the current task.
- Whether an orchestrator plans better with the new instructions cannot be tested; this round was run
  that way by hand.

## Mistakes of mine this round

- I had approved the green palette: I checked contrast, not hue.
- I accepted identical hover and selection fills after checking two of five lists; the designer found
  the other three (R-GREEN-5).
