# P3 usage metrics: ledger, queries and service

This implements the metrics backend of `notes/protocol-features.md` stage P3
and `notes/usage-metrics.md` with addendum `f6e13265`, on P1 main `fbe243af`,
merged with main `b591744b`. Native session files are read only; no terminal
is scraped, no runtime started, no prompt sent, no credential read. No
feature version is advertised and no daemon method is wired: the scheduler's
P2 card wires dispatch, authorization and connection state once, through the
service interface below.

## Commits

| Checkpoint | Commits | Scope |
| --- | --- | --- |
| C1 pure accounting | `b763bb44`, `4df0a141`, `9da15100` | Counter validation and normalization, price book and exact cost intervals, per-metric coverage, completed-task cohort. Approved at `9da15100`. |
| C2 native adapters | `dfb0308b`, `ac5d0a86`, `acf92c4f`, `c49f1bfa` | Bounded read-only reader, continuity, Claude and Codex adapters, pi gated, captured CLI homes, lifecycle fixtures and parity. |
| C3a ledger core | `2c74dca8` | Durable streams, owners, work bindings, observations, seen ids, coverage, aggregates, retention. |
| C3b cohort and asides | `d5152626` | First completions, task costs, cohort query, aside execution read-back. |
| C4a queries | `4e78669c` | Canonical queries, summary, series, breakdown, observations, runs, prices, pages and cursors. |
| C4b service | `18995664` | Collection scheduling, watches, readiness, lifecycle seams. |
| Main merge | `36d38a27` | `b591744b`, no conflicts. |

## Modules

All under `crates/crew/src/usage_metrics/`.

- `counters`: nullable counters (`I N R W W5 W1 O`), validation up to
  2^53 - 1, Claude/Codex normalization and the gated pi mapping.
- `price`: the `standard-2026-10-08` book (rates in micro-USD per million,
  OpenAI's whole-request tier above 272,000 input tokens), checked interval
  arithmetic, `Tiering` (a whole request decides its tier; a batch or partial
  increment spans every tier).
- `metrics`: totals with per-metric coverage over weighted facts.
- `cohort`: cost per completed task over first completions.
- `source`: bounded batches (4 MiB, 1,000 lines, size captured at start),
  8 MiB record limit with draining, 128-level nesting check, checkpoint with
  a head/tail continuity digest, generation loss on replace, truncate or
  rewrite, seeding of existing and ambiguous prefixes, lenient allowlisted
  field types that keep nothing of unexpected values.
- `claude`, `codex`, `pi`: adapters. Ownership (sidechain, other session) is
  decided before any state change.
- `ledger`: additive `usage_` tables, one transaction per batch of at most
  128 changes, stale cursors refused, seen-id membership with value digests,
  quarantine of conflicting replays, corrections outside the retention
  horizon, aggregates partitioned so retained figures price and count like
  their observations, first completions, task costs, aside executions.
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

`UsageMetrics::new(Ledger, Arc<dyn Opener>, epoch)`; `spawn(clock)` runs
collection (one job per stream, at most two blocking readers, at most 64
streams, rechecks after 1 s while growing and 5 s while idle, a stopped
source drains for 30 s, retention hourly). Handlers: `summary`, `series`,
`breakdown`, `observations`, `runs`, `prices`, `watch`, `unwatch`,
`disconnect`, `pending` (at most one notice per watch per second),
`changes()` as the wake signal, and `readiness()`. Lifecycle seams:
`begin_run`, `end_run`, `set_expected_agents`, `register`, `bind_work`,
`stop`, `first_completion`, `execution_status`, `aside_usage`,
`aside_total_cost`. Errors use `invalid_feature_request`, `not_found`,
`stale_cursor`, `too_large` and `busy`.

## Acceptance

| Gate | Evidence |
| --- | --- |
| U01 / P09 | `scripts/cache/usage-fixtures.mjs` extracts allowlisted fields (types, ids, models, counters, timestamps, turn markers) from the nine sessions the committed lifecycle reports name, bounded to each report's finish time, with the Codex native prefix digest checked; `--check` reproduces them. `parity.rs` reproduces every reported request's counters, model, time and ids and every member's totals and hit rate; all 68 Codex increments are proven single requests. `usage-fixtures.test.mjs` shows `baseline.mjs` reads the same totals. Documented differences come from the report's watchdog parser: it zero-fills absent Claude counters and leaves out Codex's explicit write zero. |
| U02 | Streamed copies replace counters once, including a later copy missing a field and a lower copy; synthetic and sidechain records do not count. |
| U03 | Duplicate vectors, first-known fields seeded without catch-up, intersection-only matches and repeated direct vectors are not request proof, decreases use direct evidence or a gap, batches across a model change are unpriced, reasoning stays inside output, foreign vectors neither seed nor charge. |
| U04 | 300 messages read with a crash after every transaction and decoded work dropped before each, against one uninterrupted read: identical table dumps; a stale-cursor commit is refused. |
| U05 | Partial final lines, byte/line budgets with explicit caught-up state, oversized records, inode replacement, truncation, rewrite and ambiguous replay; source files stay byte-identical. |
| U06 | Session id validation, captured CLI homes refusing links, FIFOs and traversal, one active owner per stream, pi refused. |
| U07 | Missing versus explicit zero, invalid and overflowing counters, invalid relations, zero denominators, overflow without numeric bounds. |
| U08 | Hit rate, ordinary input, output, cost and complete-category splits each on their own rows; aggregates equal per-observation totals on the lifecycle fixtures. |
| U09 | Reference book rows, 272k boundary, TTL mixtures, flat writes, missing writes, unpriced models, batch tiering. |
| U10 | First completions, recompletion, completion epic, earlier spend, untimed spend after completion widening only the upper bound, missing task cost, incomplete membership before tracking, no completions. |
| U11 | Owners and work bindings captured at file positions; late copies keep their binding; a backlog written before re-registration keeps its old owner. |
| U12 | Runs tracked by id with their tracking start; pre-tracking history kept in its own scope, including its later corrections. Durable run pointer and clear-session archiving belong to P2. |
| U13 | Copied fork context only seeds; new answer usage only in aside scope; failed spend kept; an execution without a source leaves totals partial. |
| U14 | Detail expiry keeps aggregates and seen ids; an identical old copy is a no-op; a changed old id is outside the horizon (also after restart and rewrite) and makes affected aggregates unavailable; closed admission refuses with a gap. |
| U15 | Request-id history diagnostics, file order under regressing timestamps, unknown gaps, foreign diagnostics ignored. |
| U16 | No body, prompt, tool output or path text in any table; no Activity, scheduling or prompt effect. |
| U17 (backend) | Page byte budget with working cursors, `too_large` for a single oversized row, revision- and query-pinned cursors, at most four coalesced watches per connection, no capability advertised. Dispatch, gateway lanes and cross-workspace negatives belong to P2 and P4. |
| U18 (backend) | Server-computed figures, ranges and coverage; exact wire round-trip of the summary. Cross-client parity belongs to the P5 view cards. |

Aside sources that run without native session files (Claude
`--no-session-persistence`, Codex `--ephemeral`) are the follow-up card
`metrics-aside-streams`, using ui's captures in `notes/aside-probes/`.

## Gates

On `36d38a27` (merge) and `c49f1bfa`, slot D, `CARGO_INCREMENTAL=0`, logs
in the designer worktree under `target/p3-metrics/logs/`:

- `cargo test --locked -p crew --lib usage_metrics`: 88 passed.
- `cargo test --locked --workspace --no-fail-fast`: 1,446 passed, 4
  ignored, 1 failed: `output::store::tests` (from `a9979a4e` on main) fails
  intermittently with "output store is busy" in full parallel runs, also
  with every metrics test skipped, and passes alone; reported to the
  orchestrator.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: clean.
- `cargo fmt --all --check`: clean.
- `scripts/check-msrv` (Rust 1.82.0, committed lockfile): passes.
- `node --test scripts/cache/usage-fixtures.test.mjs scripts/cache/baseline.test.mjs`: 24 passed.
