# Shared client state conformance fixtures

Five small, language-neutral replay cases for protocol 6 at
`6049cdf63e36e303327235834f23a798aa5a82c1`. These are test vectors, not a
proposed shared API. Native, shared-core, web, and TUI tests can read the same
JSON and adapt their own state to the observations below.

Root: `fixtures/native/state/`. `graph` owns this corpus, schema, and permanent
fixture-local checker. The shared policy is the approved N1 contract in
`notes/native-architecture.md` at `a6f70568`; these fixtures introduce no API.

| Case | Product behavior |
| --- | --- |
| `parked-resumption` | Execution precedes parked work; Review yields to Waiting; resuming an older card retains its ID, start, progress, artifact, and board position. |
| `work-choice` | Started Blocked and unknown-start Working remain current; equal Waiting starts preserve board order; another agent's work is excluded; recent completion and started transient work supply fallbacks. |
| `optional-values` | Absent/null optional data stays unknown; zero is measured; complete entity replacements clear old optional data. |
| `snapshot-replacement` | Snapshots replace collections and clock offset; replayed history is deduplicated; removed references become invalid while task history remains inspectable. |
| `completed-ties` | Shared-core completed fallback follows approved N1/TUI semantics; legacy web differences remain separately labelled. |

Expectations were authored from the behavior described in each step's
`reason`, then checked against existing implementations. They are not
generated from reducer or projection output. Each step supplies a product
reason and source reference; each case also names provenance. Session/agent
fields and unchanged history records come from
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
push barriers belong to separate transport tests. Every case names both actual
legacy transport modes: web's one-connection barrier
(`notes/web-design.md:377-424`) and TUI's two-connection overlap
(`crates/crew-tui/src/net.rs:83-95`). The state replay is valid after either
mode has delivered the committed snapshot. Message deduplication covers only
the newest 64 IDs; nonzero event sequences use the last accepted watermark,
while sequence zero retains its unsequenced meaning. These cases do not assert
deduplication across an entire history or change either transport.

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

Common `expect` is what N1 `crew-client` must meet: the approved TUI policy
takes maximum optional completion time, with unknown below known and the last
board entry among equals. Only `completed-ties` supplies
`legacy_client_expect.web`: the existing web picks the first equal completion
and coalesces missing completion to zero for selection. The web checker applies
only that explicitly labelled legacy override. It is not the shared-core
oracle. Neither policy fabricates an elapsed duration for an unknown timestamp.
Active-work ties already agree. No convergence decision remains pending in N1.

`schema.json` describes fixture structure; Rust DTOs remain authoritative for
wire input validity. With Rust, Node, and the existing web dependencies
installed, run from the repository root:

```sh
node fixtures/native/state/check.mjs
# Rust DTO validation and existing TUI observations alone:
node fixtures/native/state/check.mjs --rust-only
```

The checker builds existing `crew-core`/`crew-tui`, deserializes every wire
input, and replays all 31 steps through the TUI and web adapters. Re-serialization
must preserve every supplied field exactly; only omitted serde defaults may
appear. The resulting canonical DTO must then round-trip with full equality.
This rejects misspelled optional fields instead of accepting serde's ignored
fields. Scratch compilation uses the ignored `target/native-validation/` path.
The checker changes no Cargo manifests or product code.

All timestamps are fixed integer i64 milliseconds. This corpus stays within
JavaScript's safe integer range; its runner rejects larger numeric IDs,
counters, or times rather than rounding values above 2^53. String IDs stay
strings. No state observation uses the ambient wall clock. Apple
compilation/simulator verification is not claimed by this data-only Linux check.

`scheduler` owns permanent N1 `crew-client` consumers for `work-choice`,
`parked-resumption`, and `completed-ties`, using common `expect`; the fixture
checker currently observes N1 through the TUI compatibility wrapper. Reducer
consumers for `optional-values` and `snapshot-replacement` belong to the later
`native-client-engine` owner after that interface is approved. `qa` owns the
`scripts/native/**` integration runner. Each consumer reads these same JSON
files; it does not generate expected values from the implementation.

## Coverage map

Step numbers below are zero-based. The named tests remain normative; deliberate
omissions keep this corpus small and remain covered by the N1 unit-test port.

| `web/test/work.test.ts` case | Corpus observation or deliberate omission |
| --- | --- |
| resumed older Working beats newer Waiting | `parked-resumption:4` |
| Working beats newer Review | Left out as a separate positive-start combination; `work-choice:2` covers the unknown-start variant indirectly. Retained in N1's named tests. |
| latest started Waiting wins | `parked-resumption:1` |
| Waiting ties keep first board entry | `work-choice:1` |
| started Blocked beats Waiting | `work-choice:0` |
| unstarted Blocked yields to Waiting | `work-choice:1` |
| unstarted Blocked yields to last Done | `work-choice:6` |
| Working with unknown start beats Waiting | `work-choice:2` |
| Waiting beats newer Review | `work-choice:1` |
| started Ready remains a display fallback | `work-choice:7` |
| unstarted Ready and Queued are not current | `work-choice:8` |
| latest completion wins among Done | `work-choice:6` |
| Failed is ignored | `work-choice:0,3` |
| other agent's work is ignored | `work-choice:0,8` |
| unknown start preserves unknown elapsed | `optional-values:0,4` |
| elapsed measures the chosen executing card | `parked-resumption:0` |
| multiple Working snapshots keep latest start | Left out: the replay models one executing card. N1's named test preserves recovery behavior for an inconsistent multi-Working snapshot. |
| zero is a known Blocked start | `work-choice:0` |
| status priority does not depend on timestamp magnitude | Left out: negative/extreme timestamp stress stays in the N1 named test; this corpus uses ordinary fixed times plus known zero. |
| Waiting without start keeps first board tie | Left out: known-start Waiting ties and unknown-start execution are covered separately; N1's named test retains their combined boundary case. |

| `crates/crew-tui/tests/render.rs:80-179` case | Semantic coverage and limits |
| --- | --- |
| parked_work_never_replaces_the_executing_task_in_the_dashboard | Working/resumed parts: `parked-resumption:0,4`; started Blocked: `work-choice:0`. Actual terminal rendering remains in the unchanged render test. |
| parked_cards_remain_separate_and_inspectable_in_the_task_view | `parked-resumption:0,4,5` retains board membership, individual statuses, and valid task references. Rendering remains in the unchanged render test. |
| waiting_projection_uses_the_same_stable_selection_as_default_task_operations | Known-start tie: `work-choice:1`; unstarted Blocked and unknown-start Working: `work-choice:1,2`. One/both unknown-start Waiting permutations are deliberately left out and remain in the named TUI test. |
