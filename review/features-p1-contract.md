# P1 feature wire contract

This implements the portable contract from `notes/protocol-features.md`, not
the four feature services. The initial checkpoint is `664d71a2` on `df81cccb`.
Every new daemon method currently returns `unsupported_feature`; native
gateway policy still denies those methods. No feature version, new MCP tool
or observer watch is enabled by this change.

## Shared types

`crew-protocol::{features,epics,asides,docs,usage_metrics}` owns the records.
`crew-core::rpc` reexports those modules and supplies the 33 new `Call`
variants. `features::FEATURE_METHODS` is the fixed method/access inventory,
not a capability grant. `requests-v1.json` gives a complete request example
for each method, including explicit nulls and tagged filters/patches.

The common types enforce canonical decimal-string `Wide` values through
`u128`, exact JSON-integer timestamps, finite display fractions, byte-bounded
UTF-8 `Text`, bounded `List` and nonempty opaque `Id`. Text's Debug output
contains its byte length, not its contents. Identifiers confer no authority.
The final serialized envelope must pass `encode_frame`; result-page and
domain job budgets remain the service's responsibility.

`Task.epic`, `SessionInfo.workspace_run_id` and `Snapshot.epics` are optional
and omitted when absent. Legacy projection helpers strip populated feature
fields without mutating the model; the current legacy Snapshot/Subscribe
paths use them. The shared client does not negotiate these new streams yet.
The two existing FFI records and generated Swift change together; new feature
resources, converters and screens belong to P4/P5.

Legacy `TaskSpec` is unchanged. `TaskCreateScoped` carries membership beside
the boxed legacy spec, so there is only one wire membership authority. P3's
founding-team configuration uses a config-only task declaration wrapper and
extracts the legacy spec plus membership; the earlier `TaskSpec.epic`
proposal in `notes/epics.md` is superseded at this boundary. This preserves
the old TaskCreate encoding and unknown-field rejection.

## Response inventory

All names below are portable records unless noted otherwise. Mutation replies
carry the committed run/revision, not a client-derived projection.

| Methods | Response |
| --- | --- |
| `features_negotiate` | `FeatureSession` when P2 enables negotiation |
| `epics_list` | `EpicBoard`; `generation` is the workspace run id |
| `epic_get`, `epic_create`, `epic_edit` | `EpicDetail`; aliases `EpicCreateResult`, `EpicEditResult` |
| `tasks_query` | `TasksQueryResult` |
| `task_get_scoped`, `task_create_scoped`, `task_revise` | `ScopedTaskCard`; scoped mutation aliases name the same record |
| `epic_complete` | `EpicCompleteResult` |
| `aside_members` | `AsideMembers`, including total membership before paging |
| `aside_list` | `AsideListResult` |
| `aside_get` | `AsideGetResult`, available detail or a scoped/revisioned gone cause |
| `aside_ask` | `AsideAskResult`, accepted summary and duplicate flag, or scoped/revisioned gone cause; a retained conflicting request id is an RPC conflict |
| `aside_cancel`, `aside_remove` | `AsideMutationResult`; an unchanged acknowledgement does not relabel successful answers |
| `aside_watch` | `AsideWatchResult` |
| `docs_index` | `DocIndexPage` |
| `docs_read`, `docs_asset`, `docs_history`, `docs_diff` | `DocReadResult`, `DocAssetResult`, `DocHistoryResult`, `DocDiffResult`; each distinguishes unavailable content from a successful empty body |
| `docs_watch` | `DocsWatchResult` |
| `usage_summary`, `usage_series`, `usage_breakdown`, `usage_observations` | `UsageSummary`, `UsageSeries`, `UsageBreakdown`, `UsageObservations` |
| `usage_runs`, `usage_prices`, `usage_watch` | `UsageRuns`, `PriceBook`, `UsageWatchResult` |
| Every `*_unwatch` | `UnwatchResult`; only the issuing connection's watch can be removed |

`TaskRevise.expected_board_revision` fences a move affecting both epics;
`EpicEdit`/`EpicComplete.expected_revision` refers to the epic's work revision.
`EpicSummary.report` groups report time and covered revision into one optional
record. Report publication does not advance work revision. Decision message
ids are bounded to 64, matching the report decision limit.

Aside availability captures `resets_at` and a distinct `shell_member` reason.
The summary's cost and counts are server aggregates. Its successful-answer
count is separate from metrics `executions_observed`: the latter counts
distinct aside executions with newly observed usage, including explicit zero
and failed/cancelled spend, excluding copied history and duplicate revisions.
Unknown execution membership is unavailable; spend coverage remains separate.
Optional status counts describe that same observed-execution cohort, including
unknown status. The aside receipt remains body-free and rejects extra fields.

Docs has a closed AST and static-raster enum. `DocSnapshot`, `DocVersion` and
`DocAssetChunk` are checked wrappers, constructed through their corresponding
`*Data` records and `TryFrom`; JSON decoding uses the same validation. The
checks cover AST count/depth, image references/dimensions/budgets, recorded
commit presence and base64/range/progress consistency. An unavailable image
has typed causes for SVG/animation/format, source/policy refusal and each size
budget. `DocsChanged.expired_seen_versions` also covers retained unselected
views. These types do not replace the source-safe opener or connection leases.

Metric coverage preserves unknown expected historical membership with
`expected_agents: Option<Wide>`. Gap totals/backlog, per-counter reasons and
price-check time are explicit. Ratio endpoints preserve the exact operands
for priced uncertainty. No task and unattributed group keys remain single
rows; activity/reason details belong to the row. Diagnostic `Unusable`
observations are not priced or eligible by default. Price rates are
pico-USD per million: multiply tokens by that wire rate and divide by
1,000,000, with the metric design's checked bound arithmetic.

## Compatibility and remaining integration

`scripts/features/legacy-goldens.mjs` extracts and compiles the actual
`b4f9a4424c53ddf54a6db1d0d82164015221faaa` protocol/core crates. It records
source hashes and the old encoder's result hash in `legacy-v6.json`.
Run this script from the repository checkout (a jj workspace); a plain
source extraction cannot resolve the frozen revision. Tests compare canonical
bytes for legacy requests, Task/Session/Snapshot
and pushes, and retain the old decoder's scoped-method refusal evidence.
The reproduction uses only scratch files and compilation, not a daemon or
provider request.

Recognized malformed feature requests use `invalid_feature_request`, never
the old decoder's `invalid`. Oversized aside question/target payloads retain
typed `question_limit`/`target_limit` data and numeric limits even when bounded
decoding fails before the handler. These errors contain no submitted text.

P2 must still enable negotiation only after the full service is ready, freeze
its accepted set on the authenticated connection, reject first negotiation
after Snapshot/Subscribe, enforce all authority and scope checks, select
legacy versus grouped streams, and preserve independent observer barriers.
P2/P4 must test old web/index and native intermediaries and treat their actual
unsupported `invalid`/`forbidden` responses without an unscoped retry. The
task write path must have one coherent grouped-emission point, with a test
for every transition. No P1 fixture claims those behavioral gates passed.

Domain gates E01–E16, B01–B12, K01–K20 and U01–U18 remain with the respective
services. Apple platform execution remains separate from Linux binding-source
generation; no native feature screen or paid cache gain is claimed here.

## Validation

- Frozen-peer generator reproduced eight requests, three pushes and
  Task/Session/Snapshot bytes; the fixture records its result SHA-256.
- Protocol/core feature-contract suites: 9 and 6 tests passed. Existing
  protocol/core suites also passed at the first checkpoint.
- Two real daemon-session negatives passed: all new methods remain disabled,
  and a legacy Subscribe/Snapshot removes populated feature fields while
  retaining the model's membership. Old/new malformed-method codes differ.
- `scripts/native/check.sh`: 238 tests passed, including TUI rendering;
  portable protocol/client WASM check passed. Log:
  `target/features-native-check.log`.
- `cargo clippy --locked --all-targets -- -D warnings` and
  `cargo fmt --all --check` passed on the default workspace members.
- Rust 1.82 protocol/client check passed with the committed lockfile.
- Rust 1.93 FFI build and both generated-binding tests passed. Generated Swift
  was refreshed for the two optional legacy-record additions; headers and
  module map were unchanged. No Apple platform build was run.

Disposable Cargo caches were removed after verification under the team's disk
budget instruction. Cited logs, the extracted old sources, generator output
and top-level binaries/libraries were retained.

## Native policy review follow-up

The reviewed P1 commits were replayed onto main `646a0b30`, which contains
the scheduler's five-method toolbox inventory repair `59ed91d2`:

| Original | Replay | Stable patch id |
| --- | --- | --- |
| `664d71a2` | `ffd45fd7` | `ebac6848b1cfb279d2be31c626f60c492d8fc989` |
| `99bab9bd` | `6c974de0` | `d3afd890dbda4f4e517df17b22275a19eb715390` |

The P1 follow-up lists all 33 feature methods explicitly as denied in the
native tier/lane test, bringing its pinned method count to 105. Comments
record the intended later lanes; they grant nothing in P1. The test selects
the exact feature request fixtures because their strict parameter records
must not accept the legacy catch-all object. Production policy is unchanged.

On the replayed tip, with bubblewrap and socat available,
`cargo test --locked -p crew --features native-gateway` passed **866 tests,
0 failed, 2 ignored** across 28 suites. The ignored cases are the explicit
obligation-measurement experiment and the paid real-Claude test. The native
method inventory test passes; the library portion passed 570 tests. Evidence:
`target/features-policy/native-gateway.log`.

`cargo clippy --locked -p crew --features native-gateway --all-targets -- -D warnings`
and `cargo fmt --all --check` also passed. Clippy log:
`target/features-policy/native-gateway-clippy.log`.
