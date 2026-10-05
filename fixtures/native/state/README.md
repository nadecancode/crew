# Shared client state conformance fixtures

Five small, language-neutral replay cases for protocol 6 at
`6049cdf63e36e303327235834f23a798aa5a82c1`. These are test vectors, not a
proposed shared API. Native, shared-core, web, and TUI tests can read the same
JSON and adapt their own state to the observations below.

| Case | Product behavior |
| --- | --- |
| `parked-resumption` | Execution precedes parked work; Review yields to Waiting; resuming an older card retains its ID, start, progress, artifact, and board position. |
| `work-choice` | Started Blocked and unknown-start Working remain current; equal Waiting starts preserve board order; another agent's work is excluded; recent completion and started transient work supply fallbacks. |
| `optional-values` | Absent/null optional data stays unknown; zero is measured; complete entity replacements clear old optional data. |
| `snapshot-replacement` | Snapshots replace collections and clock offset; replayed history is deduplicated; removed references become invalid while task history remains inspectable. |
| `completed-ties` | Existing completed-card tie differences remain explicit per client, pending a reviewed shared policy. |

Expectations were authored from the behavior described in each step's
`reason`, then checked against existing implementations. They are not
generated from reducer or projection output. Each case names source
provenance. Session/agent fields and unchanged history records come from
Rust-generated `crates/crew/tests/fixtures/web/snapshot.json`; the operator
question comes from `pushes.jsonl`. Other inputs are reduced, synthetic
adaptations of the reviewed parked scenario and projection tests. Original
wire fixtures remain unchanged. The retained draft artifact adds an identity
check to the parked scenario.

Replay each case independently from empty state, applying steps in file
order. `snapshot` is the exact snapshot RPC result. `frame` is an exact push
`ServerFrame`. Neither uses fixture inheritance or patches. `at_ms` is a
fixed local receipt/query time; snapshot `server_time - at_ms` establishes
clock offset, used for later elapsed observations. These vectors start after
the transport has committed a snapshot; connection generations and buffered
push barriers belong to separate transport tests.

`expect` is a sparse observation: recursively compare supplied object fields,
compare arrays in full and in order, and compare scalar values exactly.
Normalize absent optional inspected fields to JSON null, never zero.
`agent_ids` is lexically sorted membership, not visual order; `task_ids` is
board order. `work` queries current/recent work by agent ID and observes task
ID, `done`, and nonnegative elapsed milliseconds or null. `selection_validity`
queries entity existence; it does not change selection or prescribe focus.
The web closes a removed agent's inspector, while the TUI selects a neighbour.
Workspace-qualified selection and stale asynchronous reads are outside this
single-workspace corpus. Every history is shorter than both clients' caps.

Only `completed-ties` supplies `client_expect`. Overlay the appropriate
legacy client's fields on `expect`: equal completion times choose the first
web card and last TUI card; null followed by zero completion also differs.
These observations document existing divergence rather than resolve it.
Keep them separate from shared expectations until a reviewed contract
chooses a policy. Active-work ties already agree.

`schema.json` describes fixture structure; Rust DTOs remain authoritative for
wire input validity. With Rust, Node, and the existing web dependencies
installed, run from the repository root:

```sh
node fixtures/native/state/check.mjs
# Rust DTO validation and existing TUI observations alone:
node fixtures/native/state/check.mjs --rust-only
```

The checker builds existing `crew-core`/`crew-tui`, deserializes every wire
input, and replays all 31 steps through the TUI and web adapters. It changes
no Cargo manifests or product code. Fixture-local adapters observe existing
reducers and presentation functions; future consumers can use their own
adapters and the same five `.case.json` files. Apple compilation/simulator
verification is not claimed by this data-only Linux check.
