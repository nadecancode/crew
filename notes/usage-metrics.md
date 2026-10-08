# Native usage and cache metrics in crew

Design card: `usage-metrics-design`; operator request
`w-2d3dcc6b066fa029`. This is an implementation contract, not a deployed
collector or a claim about savings. Source inspected at main `2c7a1c3f`.

The daemon incrementally reads usage already written by each managed
runtime, persists sanitized observations and their coverage, and serves
consistent aggregates to web, TUI and native clients. It makes no model or
usage-probe request, scrapes no terminal, changes no transcript or task, and
adds no data to an agent's standing prompt or tool definitions.

The measurement foundation is runtime's
[baseline collector](../scripts/cache/baseline.mjs), its
[contract](../scripts/cache/README-baseline.md) and
[observed baseline](prompt-cache/baseline.md). The UI contract is
[dashboard-features.md, Usage](dashboard-features.md#usage-metrics).
Epics follow [epics.md](epics.md); final wire/capability names belong to
`feature-protocol-plan`. Do not change those shared files independently.

## What exists and what changes

| Source | Existing behavior / required boundary |
| --- | --- |
| `agents/claude.rs::read_usage` | Rereads the whole file, deduplicates assistant message ids, fills absent counters with zero and calls the message count `turns`. It cannot supply the new coverage-aware metrics. |
| `AgentRuntime::usage`, `supervisor/signals.rs::refresh_usage` | Only Claude implements this summary on the inspected base. The detached read updates `Agent.usage`; the new collector needs binding/generation fences and all supported runtime adapters. |
| `crew-protocol::Usage` | Four non-null `u64` counters and `turns`. Preserve this legacy shape; do not redefine its fields as a precise new time series. |
| `agents/files.rs::AgentFiles` | Private files are opened beneath an agent descriptor. The Operator path currently uses an ordinary file open; metrics must restrict it to the captured CLI home and confirmed session as well. |
| `scripts/cache/baseline.mjs` | Proven normalization, streamed-message replacement, cumulative-delta checks, identity conservatism, explicit coverage and diagnostic associations. Its full historical discovery and in-memory cohort are offline behavior, not a daemon loop to copy literally. |
| `scripts/cache/usage.py` | A small bounded benchmark watchdog with API-equivalent cost intervals. Its supported-model assumptions are not a general production price book. |
| `persistence/{mod,sqlite}.rs` | Current store traits, one SQLite connection mutex, WAL, and `clear_session` which removes current entities/meta but retains events. New metrics need dedicated tables and short transactional batches. |
| `notes/epics.md` | Task/epic membership at observation time is authoritative for historical attribution. Current focus/current membership cannot reconstruct it later. |

The new service becomes the source for new metric views. Retire the repeated
whole-file refresh when the replacement is ready; do not add its totals to
new totals. A compatibility `Agent.usage` projection is allowed only when its
non-null fields can be supplied without inventing zero, and only for the
same current agent incarnation/runtime/native session. Its `turns` field
retains its old request-summary meaning. New clients use the new records for
Requests and Turns instead.

## Counter meaning and coverage

Use these canonical names throughout the new records:

| Symbol | Counter | Meaning |
| --- | --- | --- |
| `I` | `input_total` | All input reported for this request/observation, including reads and writes. |
| `N` | `input_uncached` | Ordinary input that is neither a cache read nor a cache write. |
| `R` | `cache_read` | Input read from cache. |
| `W` | `cache_write` | Input written to cache. |
| `W5`, `W1` | `cache_write_5m`, `cache_write_1h` | Explicit native write buckets, when reported. |
| `O` | `output` | Output, including reasoning where the runtime includes it. |

Every primitive is nullable. Accept only nonnegative integral counters within
the adapter's supported integer range; the initial adapters use the baseline's
safe-integer limit, `2^53 - 1`. Reject negative, fractional,
non-finite or overflowing values. Keep a separate validity mask and reason
codes. Unknown is not zero; an explicitly reported zero is evidence. Validate
relations such as `R + W <= I` and bucket sums. An invalid input relation
excludes the affected input metrics; an independently valid output counter
can still contribute to Output with its own coverage.

### Runtime normalization

- **Claude:** top-level `message.usage.input_tokens` is `N`, not `I`.
  `I = N + R + W` only when all three are known. Preserve the native 5m/1h
  split and aggregate `W` separately. The API defines this disjoint input
  decomposition. [Anthropic cache usage](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
- **Codex:** `input_tokens` already includes cached input. Preserve native
  `cache_write_input_tokens` as reported, including zero; absence stays null.
  Derive `N = I - R - W` only when all are known and consistent. `I - R`
  is **input not read from cache**, not ordinary input when writes are
  unknown. OpenAI's read/write categories are alternatives to ordinary input,
  not additive fees on top of it. [OpenAI cache accounting](https://developers.openai.com/api/docs/guides/prompt-caching)
- **pi:** the candidate adapter maps native `usage.input/cacheRead/cacheWrite`
  to `N/R/W`, with `I` derived when all are known, and `usage.output` to `O`.
  The existing corpus has no measured supported-pi run. A fixture parser is
  not a native compatibility result: validate the supported pi version and
  provider adapter before advertising this mapping as supported.
- Never add a reasoning-token breakout to `O` again. Preserve an optional
  breakout only as a subset/detail, with its provenance. OpenAI also
  documents that reasoning is included in output. Its Agents API fields
  are supporting accounting documentation, not a replacement for the Codex
  rollout contract. [OpenAI usage semantics](https://developers.openai.com/api/docs/guides/agents-api/observability)

Keep derivations distinct from reported fields. For example, a known total
write equal to the known 5m bucket can resolve the *cost* of the writes
without changing an absent 1h counter into a reported zero. For providers
with one write price, absent TTL does not itself make that write price
unknown. Do not assume a configured TTL was applied to a native request.

### Observations are not always requests or turns

An observation has an explicit unit:

- `request`: a stable native request/message identity or a validated single
  request increment;
- `counter_delta`: a cumulative increment not proven to represent one
  request, including a batch or partial accounting increment;
- `unusable`: safe diagnostic metadata retained, counters excluded from
  additive totals because identity or semantics are insufficient.

A correction is a newer revision of an identified observation, not a new
unit or another request. It preserves the canonical observation's unit and
identity while replacing its counters and eligibility.

**Requests observed** counts proved distinct requests. If other increments
have unknown request cardinality, return the known count plus an unknown
coverage flag; do not turn every token-count event into one request.
**Turns observed** counts correlated native/daemon turns separately. One
turn can contain many requests. Legacy `Usage.turns` is not its source.
Calls with no usage record, retries that never report usage and unsupported
internal child sessions are not silently counted as zero-cost requests.

### Each metric has its own denominator

Return `eligible_observations`, `observations_seen`, excluded reason counts,
known/unknown request cardinality, and source coverage alongside each metric.
Source coverage separately records unread backlog, unsupported schema,
missing files, skipped records, pre-tracking history and native-child gaps.
A ratio can cover every observed row while the source itself is incomplete.
Expected agents means model-backed member incarnations registered in the
selected run/range, including unsupported or missing sources; shell-only
members are not applicable. Historical expected membership without evidence
is unknown, never reconstructed from the current roster. Reporting agents is
metric-specific and does not imply complete source coverage.

- **Hit rate:** `sum(R) / sum(I)` on the *same rows* with valid known `I/R`.
  Never average request percentages or divide a partial read sum by a larger
  independently covered input sum.
- **Input not read from cache:** `I - R` where both are valid; it includes
  writes and ordinary new input. It is not “avoidable misses”.
- **Category totals:** sum each known valid category with its own coverage.
  No eligible rows produces null, not a displayed zero.
- **Total tokens:** `I + O` on rows with both known, with its own coverage.
- **Write share:** write *list cost* divided by total input-side list cost,
  on a common eligible set. It is not `W/I` and excludes output cost.
  Unknown write counts make the row ineligible even when hit rate is known.
  TTL-only price uncertainty can produce the bounded share described below.
- **Zero denominator:** unavailable with a reason. Distinguish “no covered
  input”, “zero input”, “no tasks first completed” and “no observed cost”.

A Codex row `I=100, R=80, W=null` contributes an 80% hit rate, but neither a
known ordinary-input counter nor a write share. A stacked four-category
chart must use a complete-category cohort or show a gap/partial-coverage
state; it must not draw the unknown input split as zero.

## Native adapters and incremental collection

### Source registration and ownership

Register only native sessions belonging to managed member launches or to an
explicit aside execution. Capture runtime, native session id, agent
incarnation, launch id, CLI version when already known, selected CLI-home
identity, and public model/provider/account assignment metadata. A missing
or ambiguous native session id yields unavailable ownership. Never match
null ids or attach historical usage to whichever member currently shares a
cwd. No global scan of unrelated operator sessions runs in the daemon.

Use a model/provider on the native record or a validated stable native model
context. The requested launch model and current member settings are not proof
of a historical request's effective model. An increment spanning an unresolved
model/provider change remains unpriced or bounded only by proven possibilities.

An incarnation changes when a removed agent id is reused, not on an ordinary
restart of that member. A logical native stream can survive a process
restart or proven relocation. Its identities span run bindings, so replaying
old history into a new run does not charge it again. Two concurrent owners
of the same stream are an integrity/coverage problem, not two copies of usage.

For private homes, open via `AgentFiles` beneath the member's retained root.
For operator/account homes, capture the authorized CLI root at launch and
open only the exact validated session file beneath it using the safe opener.
Do not treat a hook-provided absolute path as read authority. Require a
regular, nonblocking-opened file; refuse symlinks, special files and root
escapes. Persist the private locator for recovery, but do not export absolute
native paths in the public metrics DTO.

### Reader and checkpoint transaction

Each stream has a versioned decoder and a durable checkpoint:

```text
stream_id, file_generation, device/inode, complete_line_offset,
line_number, continuity_witness, decoder_version,
native_session_id, counter_epoch, previous_cumulative?,
current_native_model_context?, current_turn_context?, source_state
```

State contains only selected metadata and counters. It does not contain a
partial raw line, prompt, tool result, credential or reasoning text.
An incomplete line stays in a bounded memory buffer; after restart it is
read again from the last committed complete-line boundary.

Read in fixed batches, bounded by the file size captured when a batch starts.
Appends beyond that boundary belong to the next batch. Deserialize through
an allowlist which discards body fields at the reader boundary; do not retain
whole JSON `Value` trees containing tool results. Recognize only documented
record shapes, not a `usage` substring inside arbitrary text.

Normalize and validate outside SQLite/model locks. One short transaction:

1. looks up the old canonical observation, if any;
2. inserts/replaces the new revision, subtracting the old aggregate
   contribution before adding the new one;
3. updates affected rollups and coverage, including loss of formerly known
   fields;
4. advances the checkpoint and the metrics revision.

Commit before publication. A database failure advances neither cursor nor
visible metrics; replay is idempotent. A lower corrected counter is a
revision, not negative usage. Late data updates the original run/turn/bucket
and captured attribution, never the currently selected task or account.

### Adapter rules

**Claude.** Key by logical native stream and assistant `message.id`, with
provider `requestId` retained for diagnostic joins. Later appended top-level
usage for that id replaces the earlier vector; do not sum streaming copies
or merge absent final fields with stale values. `<synthetic>` records do not
count. Sidechains remain excluded until a separately verified child-session
contract exists; expose that coverage gap. Missing stable identity is not
permission to charge every line as another request. Conflicting ids/models
are diagnosed instead of silently merging different work.

**Codex.** Deduplicate unchanged cumulative vectors. For a continuous source,
subtract the previous known cumulative counters, preserving missing fields.
Compare the increment with direct `last_token_usage`. Only a validated
single-request increment earns a request count. A mismatching/batch increment
retains token evidence as `counter_delta`, with unknown cardinality and
possibly unknown model/context band. Do not price its aggregate input as if
it were one long-context request. Counter decreases start a new counter
epoch: use only direct last-request evidence where its new occurrence is
established, never a negative delta or a re-added lifetime total. Without
that evidence, mark a gap and establish a new baseline. Preserve explicit
write zero versus absent write fields. Output already includes reasoning.

**pi.** Support is gated by a native offline capture from supported pi,
including streaming identity, provider normalization, request count and
compaction/resume. The rejected GSD binary and the existing synthetic parser
fixture cannot establish support. Once verified, stable message identities
get the same replace-not-add treatment; providers without known cache
semantics remain partial/unsupported rather than getting invented zeros.

**Loss of continuity.** Inode replacement, shrinkage, header mismatch or a
failed continuity witness closes that file generation. Keep previously
committed observations; absence from a rewritten file does not undo prior
usage. Reconcile stable ids only with evidence, and quarantine conflicting
replayed values. For unidentified cumulative streams, reseed without charging
an ambiguous prefix, then resume verified new increments with a coverage gap.
Do not claim tamper-proof source history from inode/size/anchor checks.

**Starting on existing files.** New, confirmed fresh native sessions can be
read from offset zero. On upgrade, a preexisting session without a trustworthy
owned-run boundary is not silently billed to the new run. Its initial prefix
seeds parser/cumulative state; optional sanitized pre-tracking observations
are labelled legacy/unattributed separately. Current-run metrics state their
tracking start. A request already identified before that boundary keeps its
old scope when its streamed correction arrives later. Deep historical import
and uncertain ownership remain the offline collector's job in v1.

### Scheduling and bounds

Collection is independent of UI visibility, so session history survives a
closed dashboard. Existing native signals can request a read; a bounded
filesystem/poll fallback catches missed appends. It never submits a runtime
prompt or starts a CLI. Use one serialized reader per stream, at most two
blocking reader jobs per workspace, and at most 64 registered active streams.

- At most 4 MiB or 1,000 lines of input per reader batch; yield between batches.
- At most 8 MiB per JSONL record and 128 nesting levels. Drain an oversized
  line without retaining it and record incomplete source coverage.
- At most 128 sanitized observation changes per database transaction.
- At most 64 KiB of persisted parser state per stream; use indexed storage,
  not an unbounded in-memory map of every message ever seen.
- Recheck growing/active sources at most once a second; idle sources at most
  once per five seconds. Drain a stopped source for a bounded grace period;
  retain its cursor and report whether its end was observed completely.
- Parse errors retain bounded reason codes, counts and source offsets, never
  the rejected raw line. An unavailable source is not zero usage.

Cancellation does not free a worker permit while blocking work still owns it.
No collector scan holds the task planner, delivery, VCS mutation or model
lock. No callback from a stale binding may update the current `Agent.usage`
compatibility projection. Historical ledger corrections can still apply to
their original binding.

Metering must not refresh an agent's activity timestamp or emit a runtime
Activity signal; doing so would hide an idle/stuck worker from health checks.

### Time and late observations

Keep daemon `observed_at`, native timestamp when reported, and an explicit
time basis separately. Native record time is an accounting timestamp, not
necessarily request start. A validated turn boundary can supply a labelled
turn-time basis; ingest time alone cannot place old usage in today's chart.
Unknown-time usage contributes to unfiltered totals but is excluded from
time buckets/ranges with coverage. Keep the original time/binding when a
streamed correction arrives; correct the old bucket rather than billing it
at correction time. File order governs parser/diagnostic joins when native
timestamps regress. Ambiguous ordering across a task's first completion
cannot establish that cost belongs before completion; report it separately
from the cohort's eligible cost.

## Runs, work, accounts and asides

### A durable feature-neutral run

Introduce a `workspace_run_id` shared by metrics and the epic board;
`EpicBoard.generation` uses that identity. Metrics must work with epics
disabled. Create it for a new logical workspace session, retain it through
daemon/process resume, and replace it on `clear_session`, not on every
daemon start. Persist its current pointer outside the old session JSON and
archive run rows in dedicated tables.

A new epic, compaction, retry, model switch or account switch is not a new
workspace run.

On upgrade or after an older binary clears metadata, a missing pointer means
a new tracked generation. Never join orphaned history to reused task/agent
ids. Existing events can support explicitly qualified historical import, not
fabricated exact run attribution. Preserve run start, tracking start, end,
source/decoder coverage and the implementation build when known.

### Event-time work attribution

At an identified native/daemon turn boundary, capture an immutable work
binding: run, agent incarnation, launch, native turn id when available,
task id, explicit epic membership (including known No epic), board revision
and event sequence. The capture and its task/epic evidence must agree under
the model lock, then be persisted without blocking native delivery.

Correlate requests through native turn ids or a verified adapter boundary;
timestamps alone do not establish exact work ownership. Freeze this binding
for its requests, including late streamed usage. A later refile or focus
change never relabels old samples. A reviewed task can receive review work
when the typed opener and reviewer relationship unambiguously identify it.
Several candidate tasks, a mid-turn ambiguous switch, an uncorrelated turn
or historical data without a binding stay **Not attributed**. Do not split
their tokens proportionally or pick the current active task.

Distinguish known task with no epic, known no task, and unknown task/epic.
Orchestration and maintenance work can have no task; keepalive and other
maintenance requests remain in live-session totals with a separate activity
tag so a high hit rate cannot hide extra requests.
A keepalive tag requires an identified execution/turn binding; a nearby
injection event without that evidence is only a possible association.

Capture `account_id` and account assignment generation/interval from the
account-pool binding when available, alongside native session/launch/model/
provider evidence. Do not inspect credential values or join old usage to the
member's current account. Unknown accounts stay unknown. Account-level
figures are workspace-local unless a separate cross-workspace aggregation
contract exists; they are not provider-wide spend or remaining quota.

### First-completion cohort

Persist the first valid transition to Done for `(run_id, task_id)`, after
review approval when required, with completion time, event sequence and epic
membership at that transition. Failed tasks and later recompletions do not
increase the count. Missing historical completion evidence remains unknown.

**Cost per completed task** selects tasks first completed in the query's
range. Sum their observed task-bound live costs from run start through each
first completion, then divide by the number of distinct tasks in that cohort.
The epic selector uses **completion epic for this cohort only**; it does not
rewrite event-time epic token/cost totals. Costs can predate the selected
range. Source/temporal uncertainty and tasks without observed cost remain
explicit in the coverage, not zero-cost tasks.

For a partially observed cohort, divide the observed cost bounds by the full
cohort size, but leave the upper bound unknown when unobserved task cost is
unbounded. Present that result as a partial/lower estimate, never as if the
missing tasks cost zero. Keep the cohort count, tasks with observed cost and
cost-coverage state together in the response.

Use the designer's labels: “Per completed task”, “N tasks first completed in
this period · includes their earlier spend” when applicable, and “cost
observed for K of N”. No completions or no observed cohort cost is unavailable.
All-period spend divided by current done count is a different period ratio
and stays out of v1.

### Asides

Aside executions have `scope_kind=aside`, their own execution/answer id and
source binding. Default live session, agent, epic and benchmark aggregates
exclude them. They have a separate Asides scope/line. An explicit account
view may show live and aside subtotals because both can use that account.

The aside backend must supply a trustworthy billing boundary for a copied
native context: copied historical usage and cumulative counters seed state,
not new aside spend. Ingest only new answer execution usage. If the runtime
cannot distinguish that boundary, aside usage is unknown. Never recover it
by adding the parent's old context totals again.

## Versioned list-price estimates

V1 presents a **standard, non-regional API token-price equivalent**, not a
subscription invoice, provider bill, account quota or enforceable spend cap.
Tool/storage/compute charges, negotiated discounts and subscription charges
are outside this estimate. Capture observed processing/routing metadata when
available, but never infer an account's actual tier or invoice from its model.

Use immutable reviewed price books, never a network fetch during collection
or rendering. Each row records exact provider/model ids and explicit aliases,
currency, category rates, context-band rule, pricing basis, source URL,
checked date, effective dates when known and a content hash. Unknown models
show No price. A family-name prefix or a newer model's rate is not a match.

One price book is selected for a metric query, and its id/date are returned
with the result. This makes historical comparisons reproducible. New books
do not overwrite old ones. If retained data lacks sufficient detail to
reprice an old interval, report that limitation rather than mixing bases.
The baseline's older audit prices remain evidence of that audit, not a
silently updated production price table.

### Reference rows checked 2026-10-08

USD per million tokens, standard base/short-context rates:

| Model id | Ordinary input | Read | Write 5m / flat | Write 1h | Output |
| --- | ---: | ---: | ---: | ---: | ---: |
| `claude-fable-5-1` | 10 | 0.25 | 12.5 | 20 | 50 |
| `claude-opus-5-5` | 4 | 0.20 | 5 | 8 | 20 |
| `claude-sonnet-5-5` | 2 | 0.10 | 2.5 | 4 | 10 |
| `claude-haiku-4-5` | 1 | 0.10 | 1.25 | 2 | 5 |

Source: [Anthropic model pricing](https://platform.claude.com/docs/en/about-claude/pricing).
These are explicit rows, not a universal cached-read multiplier.

| Model id | Ordinary input | Read | Flat write | Output |
| --- | ---: | ---: | ---: | ---: |
| `gpt-6-astra` | 10 | 1 | 12.5 | 50 |
| `gpt-6.1-sol` | 2 | 0.10 | 2.5 | 10 |
| `gpt-6-luna` | 0.10 | 0.01 | 0.125 | 0.50 |

Source: [OpenAI standard pricing](https://developers.openai.com/api/docs/pricing).
For these three model rows, input above 272,000 tokens uses twice the input/
cache rates and 1.5 times the output rate for the entire request; store that
as an explicit band rule, not a prorated surcharge or context-window guess.
[Astra](https://developers.openai.com/api/docs/models/gpt-6-astra),
[Sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol),
[Luna](https://developers.openai.com/api/docs/models/gpt-6-luna).

Implementation must reverify and pin its release price book; this design's
reference rows are not automatic support for every alias, provider route or
historical pricing period. Additional models get sourced rows or remain
unpriced. A source conflict blocks an exact row until resolved.

### Cost and range arithmetic

For known categories, `C = N*pN + R*pR + W5*p5 + W1*p1 + O*pO`, divided by
one million when using the rates above. Flat-write providers use `W*pW`.
Apply a request's context band before aggregation.

If only the Claude aggregate write is known, price its unclassified portion
between the applicable 5m and 1h rates. Do not price every write at one TTL.
If Codex `I/R` are known but `W` is absent, ordinary-plus-write residual
`I-R` gives a bounded token estimate using the minimum/maximum applicable
ordinary/write rates. The write counter and write share remain unknown.
An unknown context band or batch increment uses explicit feasible-rate
bounds, never the batch input as a fictitious request size.

For write share with known ordinary/read cost `A` and TTL-bounded write cost
`[L,H]`, the interval is `[L/(A+L), H/(A+H)]` on a common eligible cohort.
Do not independently combine an upper numerator with a lower denominator
and call it an exact share. More ambiguous pricing can make this metric
unavailable even when a token-cost bound remains possible.

An unknown amount without a finite constraint has no finite upper estimate.
Return a known subtotal/lower bound and `upper=null`, with excluded/missing
coverage; never label the subtotal the whole session's complete cost. Partial
priced scopes name the excluded models/observations. Formatting must distinguish
a finite interval, a partial subtotal and an unavailable estimate.

Use checked integer/fixed-point arithmetic. A practical representation is
rates in micro-USD per million tokens and accumulated cost in pico-USD:
`tokens * rate` is exact before display rounding. Use `u128` internally and
decimal strings for wide persisted/wire integers so SQLite's signed range
and JavaScript's safe-integer limit cannot silently corrupt sums. Ratios
carry their exact numerator/denominator and only their display value is
rounded. Overflow is a reported invalid metric, never saturation into a
plausible total.

The same wide-integer rule applies to aggregate token counters and counts,
not only money. Do not round a counter through a JSON floating-point value.

### Arithmetic examples for implementation fixtures

These tiny examples use the reference book above; amounts are USD before
display rounding. They test arithmetic, not measured efficiency.

| Evidence | Expected result |
| --- | --- |
| Haiku 4.5: `N=10, R=80, W=10, W5=6, W1=4, O=5` | `I=100`, hit rate `80%`, input cost `0.0000335`, write cost `0.0000155`, total cost `0.0000585`, write share `15.5/33.5`. |
| Same counters, absent write-TTL split | Total cost `[0.0000555, 0.000063]`; write share `[12.5/30.5, 20/38]`. Report aggregate writes without inventing either TTL counter. |
| Luna short context: `I=100, R=80, W=null, O=5` | Hit rate `80%`; cost `[0.0000053, 0.0000058]`; ordinary-input counter and write share unavailable. |
| Rows `I/R=100/80`, `1000/900`, `5000/null` | Hit rate `980/1100`, about `89.09%`, covering two of three observed rows; the third row does not enter this denominator. |

## Storage, retention and migration

Add a `UsageStore` seam; do not write counters into message/event bodies.
Suggested dedicated tables (final schema belongs to the implementation card):

| Table | Contents |
| --- | --- |
| `usage_runs` | Durable run identity, start/tracking/end times, coverage/retention metadata and metrics revision. |
| `usage_streams` / `usage_bindings` | Native stream lineage, private source locator, agent incarnation/launch, run/aside scope and event-time account assignment intervals. |
| `usage_cursors` | Decoder/file generation and sanitized atomic checkpoint state. |
| `usage_observations` | Stable observation key, current revision, normalized nullable counters, units, captured attribution/time/model evidence and safe diagnostic flags. |
| `usage_seen_ids` | Compact dedup/correction tombstones and sealed watermarks for retained source generations after detail expiry; bounded by the same storage budget. |
| `usage_turns` | Correlation and immutable work-binding facts; no prompt bodies. |
| `usage_first_completions` | Unique first-Done fact per run/task with completion epic and coverage. |
| `usage_rollups` | Sufficient sums and per-metric coverage for five-minute/hour/session and task/cohort queries. |
| `usage_pricebooks` | Immutable sourced rate rows and their version/hash. |

A rollup stores sums on the metric's eligible intersection, not only four
independent totals. Keep pricing-relevant partitions (model, context band,
write buckets/missing masks) and completion-cohort sufficient statistics.
This lets retained aggregates preserve the same arithmetic after detailed
observations expire. Queries cannot reconstruct lost per-request detail by
inventing rows from a bucket.

Defaults: detailed sanitized observations for 30 days, capped at 250,000 rows;
hourly/run/cohort aggregates for 365 days, capped at 2,000 runs; total metrics
storage target 256 MiB per workspace. Prune oldest eligible detail first,
then old closed runs, in bounded batches. Preserve an active stream's cursor
and the dedup/correction evidence required for its retained horizon. Record
every retention floor and coverage loss. If the hard budget cannot be
maintained without discarding required evidence, pause new collection with
a visible storage-limit state rather than blocking task execution.

After dedup evidence expires, data at/below the sealed source watermark is
not inserted again. An ambiguous rewritten source cannot recover exact
continuity by assumption; reseed with an explicit gap. Late corrections
outside the retained correction horizon are reported, not applied to an
unrelated/new bucket. Retention is a measurement limitation, not permission
to double-count a replay.

New `clear_session` archives the old run and initializes the next pointer in
the same lifecycle transaction while keeping the dedicated history tables.
Older binaries may ignore those tables, but an old metadata clear leaves a
missing current pointer: create a new run on upgrade and leave old history
separate. No broader downgrade or automatic historical restoration claim.
Migrations are additive, repeatable and tested against existing databases.

Native files stay untouched. No raw prompt, reply, tool result, reasoning,
credential, endpoint URL with authentication, environment value or rejected
JSON line is persisted/exported. Diagnostic strings are mapped to bounded
known codes, not copied as arbitrary native text. Metadata and counters are
runtime-reported evidence, not an authenticated billing ledger.

## Diagnostics and what remains offline

Retain cheap safe evidence:

- native request/message/turn ids and source offsets, with identity quality;
- explicit write TTL buckets and model/provider metadata;
- positive `thinking_drop` history diagnostics only when both the known
  history-change kind and `prefix_mismatch` match, joined by request id;
- gaps between successive valid native timestamps, with regressions/unknown
  times marked unknown, not clamped into a false idle gap;
- references to nearby daemon event sequence numbers for restart, compaction,
  delivery, reminder, unanswered follow-up, model/account switch and keepalive.

File order wins for diagnostic/streaming joins. Five-minute/thirty-minute/
one-hour gap flags are thresholds, not proof of applied TTL or eviction.
Nearby events are associations, not causes or avoidable loss. Preserve D1's
queued, accepted and observed-consumed stages separately; `accepted_at` can
be unknown even when a later native marker proves consumption. Missing
diagnostics do not establish unchanged history.

Big-write rows require a proved individual request, known valid `W/R`,
`W >= 10,000` and `W > R`. Batch deltas have a separately labelled detail
state, not an invented giant request. A transcript link is offered only when
the existing authenticated transcript view can locate that native session/
time; otherwise say the evidence is unavailable, never open a newer session
as if it were the historical one.

Keep broad archived-workspace discovery, whole-file fingerprints, joined
historical cohort reconstruction, folded-turn analysis, pricing experiments,
matched before/after controls and savings claims in `scripts/cache`.
The product's basic event associations do not replace those experiments.
`avoidable_miss_tokens`, subscription invoice and realized saving stay
unknown unless a separate measured contract supplies them.

## Proposed protocol and client behavior

Propose configuration-free records in `crew-protocol/src/usage_metrics.rs`:

```text
MetricCoverage = {
  eligible_observations, observations_seen, exclusions_by_reason,
  known_requests, request_cardinality_complete,
  reporting_agents, expected_agents, source_gaps[], retained_from?
}
CounterMetric = { known_sum?, coverage: MetricCoverage }
RatioMetric = { numerator?, denominator?, value?, state, reason?, coverage }
CostEstimate = { price_book_id, basis, lower_pico_usd?, upper_pico_usd?,
                 state, assumptions[], excluded_models[], coverage }
UsageTotals = {
  input_total, input_uncached, cache_read, cache_write,
  cache_write_5m, cache_write_1h, output, total_tokens,
  hit_rate, write_share, estimate, requests_observed, turns_observed
}
UsageScope = { workspace_run_id, kind: live | aside,
               agent_incarnation?, task_id?, epic_selector?, model?, account? }
UsageQuery = { scope, from_ms?, to_ms?, group_by?, bucket?, price_book_id?, cursor?, limit? }
UsageSummary = { revision, scope, range, as_of, tracking_state, totals,
                 attribution_coverage, completion_cohort?, limitations[] }
UsageObservation = { id, revision, unit, binding, time_basis, native_time?,
                     counters, validity, attribution, diagnostic_codes[], event_refs[] }
```

Exact enum/field names, optional feature advertisement and ABI conversions
are coordinated by `feature-protocol-plan`. Distinguish explicit No epic
from Not attributed; distinguish a missing capability from support with no
samples. Do not silently fall back to legacy zero-filled `Agent.usage`.

Proposed read-only calls: `usage_summary`, `usage_series`,
`usage_breakdown`, `usage_observations`, `usage_runs`, `usage_prices`, and
`usage_watch`/`usage_unwatch`. The summary returns a coherent committed
revision; pages/cursors pin that revision or fail stale so a client can
refresh. A watch registers before its initial response and then emits a
small revision/invalidation or bounded summary update, at most once a second.
It is opt-in on that connection, not an agent-facing broadcast.

All methods are workspace-scoped operator reads. Native gateway access uses
Read; watch/unwatch belong on the workspace lane, other reads may use its
read lane. No index/action/terminal-lane access or cross-workspace run ids.
Grant revocation/expiry, disconnect and attempt changes stop watches and
discard stale client results. Existing quota-window operations and their
semantics stay separate. No new agent MCP tool is needed for v1.

Keep serialized replies under 512 KiB and the final frame under 1 MiB. Page
at most 200 detail/breakdown/run rows; at most 720 series buckets per reply.
Default trends use five-minute buckets through six hours, hourly afterwards;
coarsen explicitly for a longer range. At most four active metric watches
per connection, one coalesced pending update each, and bounded query work.
The database/parse workers must not hold native delivery or task locks while
serving a dashboard.

The shared client uses the existing resource-read/state/command seams and
workspace/attempt fences. Web, TUI, GTK and Apple display server-computed
metrics/coverage rather than recalculating from a filtered list. Use the
designer's unavailable/not-reported/no-price labels, show estimate ranges
and their price-book date, and keep quota pressure separate from efficiency.
Full new records/converters and old/new capability fixtures land together;
none of these live values enters standing instructions.

## Acceptance and implementation split

All collector/correctness tests use sanitized native fixtures or supported
CLIs against local stubs. Paid comparative work remains QA's
`cache-acceptance`, with its own budgets and source/session evidence.

| ID | Pass condition |
| --- | --- |
| U01 Native parity | Fresh-source fixture replay matches `baseline.mjs` normalized token totals for Claude/Codex, with explicit documented request-cardinality differences; supported pi requires native evidence. |
| U02 Streaming | Repeated Claude/Pi identities replace counters; late/missing fields and downward corrections update sums and coverage once; synthetic/unsupported sidechain rows do not inflate totals. |
| U03 Codex | Unchanged cumulative events deduplicate; resets use only valid direct evidence or gaps; batches/corrections are not fabricated requests; reasoning is not double-counted. |
| U04 Restart atomicity | Crash before/after every ingest transaction boundary produces the same rows/rollups/cursor as uninterrupted reading; no lost or duplicated increment. |
| U05 File integrity | Partial lines, oversized/malformed records, inode replacement, truncate/rewrite, missing files and ambiguous replay stay bounded and report source coverage. No source file changes. |
| U06 Safe ownership | Exact session/binding wins; null/shared-cwd ambiguity, reused agent ids, changed CLI homes and symlink/special-file escapes cannot read or assign another source. |
| U07 Unknowns | Missing versus explicit zero, negative/fractional/overflow counters, invalid relations and zero denominators produce the stated per-metric eligibility and labels. |
| U08 Metric intersections | `I/R` coverage, ordinary-input coverage, output coverage, cost and complete-category charts each use their own correct rows. Weighted hit rate is not a mean of percentages. |
| U09 Prices | Exact model rows, 272k boundary, write TTL mixtures/unknowns, flat writes, missing write counters, unpriced models and price-book changes yield checked ranges, never a subscription bill. |
| U10 Cohort arithmetic | Immutable first-Done facts, approval, failure/recompletion, refile, earlier spend and missing task cost produce the agreed completed-task cohort and coverage. Zero completions is unavailable. |
| U11 Attribution | Delayed usage/corrections, multi-work turns, uncorrelated native turns, model/account switches and task/epic refile never join to current ownership by accident. |
| U12 Run boundaries | Restart/resume preserves the run; new/old clear-session paths cannot attach old rows to reused ids. Epics disabled still supports metrics. Pre-tracking history stays qualified. |
| U13 Asides | Copied context/cumulative prefixes are not new spend; new answer usage appears only in aside scope; explicit account subtotals can include it without contaminating live benchmarks. |
| U14 Retention | Detail expiry preserves correct sufficient aggregates/coverage; late corrections and lost dedup evidence cannot double-count a replay; hard storage limits do not block tasks. |
| U15 Diagnostics | Strict request-id diagnostic joins and file order work under regressing timestamps; missing diagnostics and long gaps never become causal cache-loss claims. |
| U16 Observer isolation | No model call, terminal scrape, CLI probe, prompt mutation, task/agent scheduling change, credential read for metering, or body/secret persistence/export. |
| U17 Wire/security | Final escaped frames fit; capabilities gate new reads/pushes; pages/revisions/attempts fence stale data; gateway Read lanes/revocation and cross-workspace negatives pass. |
| U18 Presentation | Designer F12–F14/F20 use identical figures, ranges and coverage across clients; Requests/Turns and completed-task cohort copy are truthful; no unsupported native claim. |

Implementation ownership, assigned by the orchestrator after protocol review:

1. **Portable types and pure accounting.** New
   `crew-protocol/src/usage_metrics.rs`; pure normalization/coverage/range
   policy in a core module, exact integer arithmetic and a versioned price
   book. Coordinate central RPC/client changes with native-architect.
   U01/U07–U10, serde/portable/MSRV checks.
2. **Safe native tail adapters.** New `crew/src/usage_metrics/{source,claude,
   codex,pi}.rs`; `agents/files.rs` safe access extensions; bounded parsing,
   source identity and continuity. U01–U06/U15/U16. Supported-pi probe is its
   own prerequisite, not assumed from the GSD binary.
3. **Ledger and lifecycle attribution.** `persistence/{mod,sqlite}.rs`, new
   usage store, run/turn/account binding and first-completion hooks in daemon
   lifecycle. One owner covers atomic checkpoint/aggregate/coverage updates;
   coordinate D1/account-pool/epic hooks before editing shared files.
   U02–U06/U10–U14/U16.
4. **Queries, authenticated updates and bindings.** Daemon metric service,
   bounded query/watch handlers, gateway policy, resource reads, FFI records
   and converters under the feature protocol plan. U08/U14/U17, compatibility
   and native scope tests; no duplicate client aggregation engine.
5. **Views, integration and evidence.** Designer's Usage/Inspector/epic
   metrics slices for web/TUI/GTK/Apple, recorded and partial fixture datasets,
   price/coverage documentation and U18. Reuse existing frontend budgets;
   paid efficiency comparisons stay separate from deterministic meter tests.

Implementation is complete only when normalization, identity, correction,
coverage, lifecycle and client gates agree on the same fixtures. A prettier
percentage computed from legacy counters does not satisfy this contract.
