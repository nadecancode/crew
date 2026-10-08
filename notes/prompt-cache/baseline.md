# Prompt cache baseline

Native usage through **2026-10-08 04:43:53.093 UTC** has a token-weighted cache
read rate of **97.90%**: 7,306,333,982 cached tokens out of 7,463,282,088 input
tokens across 24,129 observations. The 156,948,106 input tokens not read from
cache include 117,271,674 Claude cache-write tokens. A high aggregate hit rate
therefore still leaves substantial cache-fill volume.

This is an observational history, spanning October 2–8, not a controlled test of
one Crew revision. All eleven current members have measured sessions. Discovery
also covers earlier member sessions and the two other registered local runs
(the native-architect worktree and demo-workspace). It excludes nineteen unrelated
root-project sessions. Unregistered archived workspaces and temporary benchmark
projects require explicit selection.

The [JSON summary](baseline.json) contains per-session, per-agent, per-runtime,
per-role and event aggregates, source fingerprints and coverage. The complete
sanitized per-request timeline is [baseline.timeline.json.gz](baseline.timeline.json.gz).
It expands to the original schema-version-1 collector JSON; its compressed and
uncompressed SHA-256 values are in the summary. No prompt or credential bodies
are included.

## Runtime and member measurements

| Runtime | Observations | Total input | Cache reads | Cache writes | Token hit rate |
|---|---:|---:|---:|---:|---:|
| Claude Code | 16,072 | 5,623,886,457 | 5,506,578,462 | 117,271,674 | 97.9141% |
| Codex | 8,057 | 1,839,395,631 | 1,799,755,520 | 0 | 97.8449% |
| pi | No native usage observed | Unknown | Unknown | Unknown | Unknown |

Codex's native write field is present on **8,057/8,057** measured observations;
the zero above is observed, not an assumption about absent fields. Every Claude
write has an explicit one-hour bucket; five-minute writes total zero. Neither
fact establishes a runtime's subscription invoice or the retention of a later
request.

The following rows are the main team's current member names, across their
identified historical sessions. They can contain multiple models or runtimes;
they are not comparisons of interchangeable workloads. Removed members and
unknown historical owners remain separate in the JSON.

| Member | Observations | Input | Cache reads | Token hit rate |
|---|---:|---:|---:|---:|
| orchestrator | 326 | 68,230,100 | 66,866,337 | 98.00% |
| designer | 1,556 | 434,648,980 | 425,231,490 | 97.83% |
| reviewer | 3,092 | 1,008,528,729 | 979,110,028 | 97.08% |
| ui | 1,106 | 275,386,968 | 266,632,147 | 96.82% |
| graph | 1,547 | 389,566,647 | 380,676,321 | 97.72% |
| content | 2,149 | 649,172,696 | 640,688,843 | 98.69% |
| qa | 2,155 | 581,440,654 | 571,683,261 | 98.32% |
| scheduler | 1,387 | 448,291,522 | 439,352,245 | 98.01% |
| runtime | 688 | 182,078,459 | 177,824,768 | 97.66% |
| native-architect | 32 | 2,171,946 | 2,018,432 | 92.93% |
| cache-reviewer | 104 | 16,612,712 | 16,121,472 | 97.04% |

`by_role` records exact roster role text and metric coverage. The known role
groups include orchestration (359 observations, 97.95%), review (3,092, 97.08%),
design (1,556, 97.83%), graph implementation (1,547, 97.72%), content
implementation (2,149, 98.69%) and runtime lifecycle (688, 97.66%). There are
10,299 observations with unknown roles. The labels are current roster labels,
not evidence that a member had that role throughout an older session. Six
historical sessions have unknown owners; their usage stays in runtime totals
but is not joined to an arbitrary member's events.

## Input not read from cache

These primary classes partition observations using the collector's documented
priority. They describe diagnostic or temporal associations, **not proven
causes, avoidable loss, or incremental cost of an event**. Ordinary new input
also contributes to every row. Amounts are token volumes; no dollar conversion
or fixed cache-price ratio is used.

| Primary association | Observations | Input not read from cache | Explicit cache writes |
|---|---:|---:|---:|
| Native history-change diagnostic | 183 | 44,114,589 | 44,113,859 |
| Ordinary append or unobserved reason | 18,740 | 43,530,998 | 22,872,873 |
| Gap over one hour | 71 | 26,349,326 | 23,253,702 |
| Fold associated, without that diagnostic | 1,107 | 19,778,784 | 19,776,027 |
| Crew delivery associated | 1,720 | 5,820,060 | 294,591 |
| Nudge associated | 152 | 3,869,786 | 2,610 |
| Reminder associated | 1,436 | 3,368,136 | 2,580,461 |
| Gap five to thirty minutes | 488 | 2,838,500 | 829,049 |
| Gap thirty minutes to one hour | 71 | 1,757,052 | 1,054,624 |
| Session's first request | 87 | 1,629,184 | 1,054,803 |
| Crew model/runtime change associated | 23 | 1,616,017 | 172,875 |
| Compaction associated | 47 | 1,426,278 | 420,175 |
| Observed native model change | 2 | 846,029 | 846,025 |
| Resume associated | 2 | 3,367 | 0 |

Crew-event overlaps are retained separately in `event_associations`. For
example, `agent.restart` precedes 25 observations containing 2,462,046 non-read
input tokens and 1,018,900 writes; `agent.limit_lifted` precedes 32 observations
containing 3,126,513 non-read tokens; deliveries precede 2,411 observations with
47,021,775 non-read tokens. These overlap other classes and must not be added
together. There is no dedicated native resume indicator; a process restart,
usage-limit lift, or `task.resumed` event is not proof of a provider-side cache
reset.

The directly joined history diagnostics account for 37.6% of explicit Claude
write volume. This is a narrower measurement than a residual large-write
heuristic. It does not say 37.6% is recoverable. Every observation and aggregate
keeps `avoidable_miss_tokens` null.

## Fold correlation

There are 185 native `thinking_drop` records in 23 sessions. Exactly 183 match
both `clientChange.kinds = messagesHistoryChanged` and
`newlyDropped.reason = prefix_mismatch`. The other two (`none` / `other`) are
retained as diagnostics and excluded from history-change classification.
`requestId` joins each diagnostic to the assistant usage record; source line
numbers survive in the timeline. Attachment timestamps can regress, so the
join and completed-turn boundaries use file order.

Among first requests after completed Claude turns, 183 of 387 requests after a
folded turn have that diagnostic, versus zero of 1,267 after a turn without a
fold. Restricting the cohort to sessions which emit any thinking-drop
diagnostic gives **183/340** versus **0/1,135**. These are positive diagnostic
counts. Missing diagnostics are unknown, not evidence of unchanged history.
The cohort selects completed turns and does not establish causality or the
token savings a fold change would produce. Scheduler's prompt audit and QA's
controlled lifecycle experiments test that mechanism separately.

## Reproduction and coverage

The collector is [scripts/cache/baseline.mjs](../../scripts/cache/baseline.mjs),
with [its contract](../../scripts/cache/README-baseline.md) and 23 focused Node
regressions. Source starts from `cdcbb595`; the append-only source replay is
`34ab90ad`, `9015e0dc`, `a2398529`, then measurement source `ec5de823`.
The final tool also protects registry records against output replacement;
that guard does not change measurement parsing. Both collector hashes are in
the JSON verification metadata. The original source objects
remain at `agents/runtime-cache-initial` (`ac93f892`) and
`agents/runtime-cache-identity` (`e3e9d29f`). There was no history rewrite.

```sh
node --test scripts/cache/baseline.test.mjs
node scripts/cache/baseline.mjs --workspace /home/nade/workspaces/personal/crew \
  --all-runs --until 2026-10-08T04:43:53.093Z \
  --output target/cache-baseline/baseline.full.json
```

Inputs total 708,636,259 bytes from 106 native session files. The database
snapshots have independent event-sequence bounds; file fingerprints cover each
initial byte prefix. This is not an atomic snapshot across live sources. The
selected metrics have zero missing or invalid cache-ratio counters, zero Codex
counter resets, and zero Codex deltas disagreeing with native last-request
usage. The exported source stats cover the entire read prefix, which can
include records later than the request-timestamp cutoff. One live native file
grew during the final scan; its change flag is retained and its initial byte
prefix is the hashed measurement source. The reader does not include bytes
appended beyond that bound.

SQLite opens read-only and queries no token table or message body. Native
content is discarded at the reader boundary. Tests cover streamed deduplication,
missing versus zero counters, counter resets, invalid counters, window context,
strict diagnostics, Pi's streamed header, ambiguous/null identities, nested
registered workspaces, bounded live appends, and output alias protection. No installed binary,
active team, listener, credential, or runtime setting was changed.

As a separate cross-check, replaying the collector over QA's three preserved
[c805 workload sessions](bench-baseline.json) exactly reproduces its 66 request
counts and input/read totals: orchestrator 423,132/379,177; Claude worker
1,295,160/1,230,666; Codex worker 678,036/645,376. This made no new API calls.
These controlled-workload numbers are separate from the observational totals
above. QA's `55851b03` adds a fake runner handoff test with real SQLite and Node,
three exact session selectors, preserved native hashes and owned-process
cleanup. Its 17-test suite passes without provider calls. QA owns the
workload's API-equivalent cost interval.

Claude's input denominator follows the
[Anthropic cache usage definition](https://platform.claude.com/docs/en/build-with-claude/prompt-caching).
Codex input already contains cached input, consistent with
[OpenAI's cache usage fields](https://developers.openai.com/api/docs/guides/prompt-caching).
Native model/provider configuration does not establish the retention applied to
each request. Five-minute, thirty-minute and one-hour gap flags are comparisons;
explicit native write buckets are stronger evidence about Claude writes.
Subscription invoice cost and avoidable loss remain unknown.
