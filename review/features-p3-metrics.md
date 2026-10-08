# P3 usage metrics: ledger, queries and service

This implements the metrics backend of `notes/protocol-features.md` stage P3
and `notes/usage-metrics.md` with addendum `f6e13265`, on P1 main `fbe243af`,
merged with main `201e7fdc`. Native session files are read only; no terminal
is scraped, no runtime started, no prompt sent, no credential read. No
feature version is advertised and no daemon method is wired: the scheduler's
P2 card wires dispatch, authorization and connection state once, through the
service interface below.

Every checkpoint is approved by cache-reviewer: C1 at `9da15100`, C2 and C3a
through `cf6bf147` (m-ed9123828341), C3b, C4a and C4b through `1b09215f`
(m-2f030badb193). The final tree is `8245c0d0`.

## Commits

| Checkpoint | Commits | Scope |
| --- | --- | --- |
| C1 pure accounting | `b763bb44`, `4df0a141`, `9da15100` | Counter validation and normalization, price book and exact cost intervals, per-metric coverage, completed-task cohort. |
| C2 native adapters | `dfb0308b`, `ac5d0a86`, `acf92c4f`, `c49f1bfa`, `a1991ca1`, `d5d9c40b`, `ff699482`, `82d5c082`, `cf6bf147` | Bounded read-only reader, continuity, Claude and Codex adapters, pi gated, captured CLI homes, lifecycle fixtures and parity. Codex increments identified by their vectors, never revised, with a high-water mark of every vector the stream has shown, across rewrites and restarts; native batches carry their start position. |
| C3a ledger core | `2c74dca8`, `78fca059`, `82d5c082`, `cf6bf147` | Durable streams, owners, work bindings, observations, seen ids, coverage, aggregates in wide integers, retention, resumed owners; a batch is attributed only when no owner, work or account binding changed anywhere inside it. |
| C3b cohort and asides | `d5152626`, `b0b41688`, `1b09215f` | First completions, task costs, cohort query, aside execution read-back; late corrections make them unavailable; refused and drained sources leave cohort cost open; earlier spend inside a period's first hour. |
| C4a queries | `4e78669c`, `3c3dcf37`, `8ba7da72`, `4536c488`, `a41a0504`, `1b09215f` | Canonical queries over exact half-open ranges (the shared `UsageQuery::resolved_shape`), scoped secondary metrics, bounded cursors, summary, series, breakdown, observations, runs, prices; refused sources as gaps; aside counts unavailable when their range lost detail. |
| C4b service | `18995664`, `1e1d66e3`, `1b09215f` | Collection scheduling, captured homes per registration, monotonic revisions, ready watches, durable source gaps, and every ledger commit and retirement fenced by the stream's persisted registration. |
| U18 fixtures | `8156e7d5`, `8245c0d0` | `crates/crew/tests/web_usage.rs` generates the web dashboard's `usage.json` from native records through the ledger and queries. |
| Main merges | `36d38a27`, `a5960fc4`, `e8399539` | `b591744b`, then `201e7fdc`, no conflicts. |

## Modules

All under `crates/crew/src/usage_metrics/`.

- `counters`: nullable counters (`I N R W W5 W1 O`), validation up to
  2^53 - 1, Claude/Codex normalization and the gated pi mapping.
- `price`: the `standard-2026-10-08` book (rates in micro-USD per million,
  OpenAI's whole-request tier above 272,000 input tokens), checked interval
  arithmetic, `Tiering` (a whole request decides its tier; a batch or partial
  increment spans every tier).
- `metrics`: totals with per-metric coverage over weighted facts, counted in
  wide integers.
- `cohort`: cost per completed task over first completions.
- `source`: bounded batches (4 MiB, 1,000 lines, size captured at start),
  8 MiB record limit with draining, 128-level nesting check, checkpoint with
  a head/tail continuity digest, generation loss on replace, truncate or
  rewrite, seeding of existing and ambiguous prefixes, lenient allowlisted
  field types that keep nothing of unexpected values.
- `claude`, `codex`, `pi`: adapters. Ownership (sidechain, other session) is
  decided before any state change. A Codex increment is keyed by the vectors
  it starts from and reaches; one starting below the highest vector the
  logical stream has shown (charged, seeded or a baseline) is a gap, not
  usage.
- `ledger`: additive `usage_` tables, one transaction per batch of at most
  128 changes, stale cursors and stale registrations refused, seen-id
  membership with value digests, quarantine of conflicting replays,
  corrections outside the retention horizon, aggregates partitioned so
  retained figures price and count like their observations, first
  completions, task costs, aside executions, source gaps.
- `collect`: one bounded read of a stream into the ledger; the ledger is held
  only around database steps.
- `query`: canonical queries and answers; pages stop at their row limit and
  the 512 KiB page budget; cursors pin query and revision.
- `service`: the daemon-facing service.
- `parity` (tests only): lifecycle counter parity.

`agents/files.rs` adds `CliHome`, a CLI home captured at launch and opened
directory-only and nonblocking; transcripts and rollouts open beneath it
without following links, as private homes already did.

## Service interface for P2

`UsageMetrics::new(Ledger, Arc<dyn Opener>, epoch)`; `recover(now)` at start
resumes every stream with an active owner; `spawn(clock)` runs collection
(one job per stream, at most two blocking readers, at most 64 streams,
rechecks after 1 s while growing and 5 s while idle, a stopped source drains
for 30 s, retention hourly). Handlers: `summary`, `series`, `breakdown`,
`observations`, `runs`, `prices`, `watch`, `unwatch`, `disconnect`,
`pending` (at most one notice per watch per second, none before its initial
reply), `changes()` as the wake signal, and `readiness()`. Lifecycle seams:
`begin_run`, `end_run` (only that run's streams), `set_expected_agents`,
`register` (identity and runtime checked before any home is captured or file
opened; the same launch registered again keeps its owner), `bind_work`,
`stop`, `first_completion`, `execution_status`, `aside_usage`,
`aside_total_cost`. Errors use `invalid_feature_request`, `not_found`,
`stale_cursor`, `too_large` and `busy`.

## Acceptance

| Gate | Evidence |
| --- | --- |
| U01 / P09 | `scripts/cache/usage-fixtures.mjs` extracts allowlisted fields (types, ids, models, counters, timestamps, turn markers) from the nine sessions the committed lifecycle reports name, bounded to each report's finish time, with the Codex native prefix digest checked; `--check` reproduces them. `parity.rs` reproduces every reported request's counters, model, time and ids and every member's totals and hit rate; all 68 Codex increments are proven single requests. `usage-fixtures.test.mjs` shows `baseline.mjs` reads the same totals. Documented differences come from the report's watchdog parser: it zero-fills absent Claude counters and leaves out Codex's explicit write zero. |
| U02 | Streamed copies replace counters once, including a later copy missing a field and a lower copy; synthetic and sidechain records do not count. |
| U03 | Duplicate vectors, first-known fields seeded without catch-up, intersection-only matches and repeated direct vectors are not request proof, decreases use direct evidence or a gap, batches across a model change are unpriced, reasoning stays inside output, foreign vectors neither seed nor charge. Old Codex records read again after a decrease, a rewrite (also below a seeded pre-tracking prefix), a restart or an append never charge twice; a replayed increment with other metadata is quarantined. |
| U04 | 300 messages read with a crash after every transaction and decoded work dropped before each, against one uninterrupted read: identical table dumps; a stale-cursor commit is refused. |
| U05 | Partial final lines, byte/line budgets with explicit caught-up state, oversized records, inode replacement, truncation, rewrite and ambiguous replay; source files stay byte-identical. |
| U06 | Session id validation, captured CLI homes refusing links, FIFOs and traversal and kept per registration, one active owner per stream, pi refused as unsupported before any source is opened. |
| U07 | Missing versus explicit zero, invalid and overflowing counters, invalid relations, zero denominators, overflow without numeric bounds. |
| U08 | Hit rate, ordinary input, output, cost and complete-category splits each on their own rows; aggregates equal per-observation totals on the lifecycle fixtures. |
| U09 | Reference book rows, 272k boundary, TTL mixtures, flat writes, missing writes, unpriced models, batch tiering. |
| U10 | First completions, recompletion, completion epic, earlier spend (also inside a period's first hour, conservatively once its detail expired), untimed spend after completion widening only the upper bound, missing task cost, refused or drained sources, incomplete membership before tracking, no completions. |
| U11 | Owners and work bindings captured at file positions; late copies keep their binding; a backlog written before re-registration keeps its old owner; a batch across any binding change (A, B, A included), any account change or another owner is attributed to neither side; a batch without a known start goes to no one. |
| U12 | Runs tracked by id with their tracking start; pre-tracking history kept in its own scope, including its later corrections. Durable run pointer and clear-session archiving belong to P2. |
| U13 | Copied fork context only seeds; new answer usage only in aside scope; failed spend kept; an execution without a source leaves totals partial; aside counts are unavailable when their range needs expired detail. |
| U14 | Detail expiry keeps aggregates and seen ids; an identical old copy is a no-op; a changed old id is outside the horizon (also after restart and rewrite) and makes affected aggregates, cohort and aside figures unavailable; closed admission refuses with a gap. |
| U15 | Request-id history diagnostics, file order under regressing timestamps, unknown gaps, foreign diagnostics ignored. |
| U16 | No body, prompt, tool output or path text in any table; no Activity, scheduling or prompt effect. |
| U17 (backend) | Page byte budget with working cursors, `too_large` for a single oversized row, revision- and query-pinned cursors with bounded offsets, at most four coalesced watches per connection with monotonic revisions, reads and retirements fenced by registration (barrier tests at a read's first ledger step and between drain and retirement), no capability advertised. Dispatch, gateway lanes and cross-workspace negatives belong to P2 and P4. |
| U18 (backend) | Server-computed figures, ranges and coverage; exact wire round-trip of the summary; `crates/crew/tests/fixtures/web/usage.json` holds the replies for the F12-F14 scenario as the ledger computes them. Cross-client parity belongs to the P5 view cards. |

Aside sources that run without native session files (Claude
`--no-session-persistence`, Codex `--ephemeral`) are the follow-up card
`metrics-aside-streams`, using ui's captures in `notes/aside-probes/`.

## Gates

On `8245c0d0` (the final tree), slot D, `CARGO_INCREMENTAL=0`,
`CARGO_TARGET_DIR=target/p3-metrics`, logs in the designer worktree under
`target/p3-metrics/logs/8245c0d0-*`:

- `cargo test --locked --workspace --no-fail-fast`: 1,498 passed, 0 failed,
  4 ignored. The crew library: 700 passed, 1 ignored.
- `cargo test -p crew --lib usage_metrics` from the pinned binary
  `target/p3-metrics/pinned/8245c0d0-crew-lib-tests` (sha256
  1f4a800735c6ac1102835cf497c58d26d4bfcc758a8dea16f0a17668cdc9486e):
  120 passed.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: clean.
- `cargo fmt --all --check`: clean.
- `scripts/check-msrv` (Rust 1.82.0, committed lockfile, target
  `target/p3-metrics/msrv`): passes.
- `node --test scripts/cache/usage-fixtures.test.mjs scripts/cache/baseline.test.mjs`:
  24 passed; `node scripts/cache/usage-fixtures.mjs --check`: 9 fixtures
  match.

The workspace run on `55bd4da9`, the tree before `8245c0d0`, is kept as
`target/p3-metrics/logs/55bd4da9-workspace.log`: 1,496 passed, 2 failed.
`web_wire::every_fixture_file_is_generated` failed because the new
`usage.json` was not in its list; `8245c0d0` adds it.
`output::store::tests::busy_store_is_bounded_and_interrupted_publication_recovers`
failed with "output store is busy", the intermittent failure from main
(`a9979a4e`) reported to the orchestrator; it passed in the `8245c0d0` run
and alone (`55bd4da9-output-store-rerun.log`, 7 passed).

No `rpc.rs` method was added, so the native-gateway suite is not affected
by this card.
