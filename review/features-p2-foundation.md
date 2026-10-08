# Daemon feature foundation

The daemon has reusable run, connection, transaction and observer ports. No
domain service or capability is registered by this change. The production
composition is tracked separately by `features-p6-daemon-wiring`; its closure
is required before the integrated feature release can complete.

The source checkpoints are `57110710880506a12c71483520bb5e605d15fe45` and the
explicit host-readiness accessor in `3c0f84b1`. The review candidate also merges
reviewed main `4f24f9c6`; that upstream interval contains docs/probes and the
shared bundle-budget test adjustment, with no production Rust changes. This
foundation changes neither protocol v6 nor native authentication,
grant catalogs, task ordering, execution eligibility or the member MCP catalog.

## Run and connection ownership

The workspace's SQLite database retains a current run and member incarnation
records. Process and store restart preserve them. Session clearing archives the
old run and creates a new one; removing and reusing a member id creates a new
incarnation. A marker in legacy metadata detects an old binary's clear on the
next open, without attaching old feature rows to reused legacy ids.

Hello captures the authenticated caller, a fresh physical connection epoch,
current run and agent incarnation without extending Hello's wire result.
Feature negotiation must precede Snapshot, Subscribe or a feature operation.
Its requested and accepted sets are frozen for that connection; an identical
repeat returns the same epoch and current run metadata. Unknown versions,
unready services and unauthorized observer features remain explicit refusals.

Agent connections cannot gain a reused member's authority. Feature dispatch
checks the captured incarnation, accepted version, actual caller, current
readiness and applicable run before entering a typed handler. Scoped Epic
handlers repeat the identity/run/cancellation check through `commit_tasks_for`
under the model and SQLite writer fence. Historical metrics queries may name
retained runs in this workspace's database; a foreign run is refused before
dispatch and current membership is never substituted for query attribution.

## Transactions and projections

`FeatureDb` shares the daemon's SQLite writer. Domain callbacks receive the
transaction directly and cannot acquire another store lock or commit it.
`durable_transaction` selects FULL synchronization for receipt admission and
removal, restoring the prior setting after commit, error or unwind. Failed
restoration keeps FULL and cannot turn a committed receipt into an apparent
rollback.

`TaskProjection` supplies the authoritative board and hydrated task upserts.
Task creation, mutation and message-plus-task mutation share that transaction.
Messages retain the existing body, recipient, sender and idempotency rules.
An unchanged task can accompany a new message without a synthetic board bump.
Model updates and publication occur only after commit; failed callbacks leave
task, message and model state unchanged. Persisted outbox questions are
published through the existing bus without inserting them again, and durable
publication sequences are acknowledged afterwards. An acknowledgement failure
leaves the domain outbox for replay.

Legacy connections receive the old Task stream and absent-field projections.
Negotiated Epic connections receive grouped changes rather than an additional
legacy Task update. Session, agent, task and board capture share the model
fence. The complete escaped negotiated Snapshot has a 512 KiB page check;
oversized state returns `too_large` without changing or truncating stored data.
Legacy methods retain their response shapes, including load-team metadata.

## Observer lifetime and privacy

Independent forwarders coexist with the legacy stream. The initial reply is
queued before observer metadata. Admission enforces one aside watch, one Docs
watch and four metrics watches for each physical connection. Docs selection
replacement validates and prepares the new stream before aborting and awaiting
the previous forwarder; a refused selection preserves the live previous watch.

Run expiry and stream end/error/identity loss send a final registered resync
while the physical connection remains open. Epoch replacement preserves
bounded terminal selection metadata and then ends. Backpressure or an oversized
update can produce a same-epoch resync while the forwarder stays live. Physical
disconnect emits no terminal frame and releases blocked forwarding. Blocking
domain jobs retain their permits until actual retirement, independent of the
cancellation signal. Shutdown refuses new feature admission and awaits each
service's owned retirement hook before ordinary daemon teardown.

The closed operator audit port accepts only P1 AsideAsked/AsideAnswered metadata.
It takes no model lock and adds no task/member association, body, prompt, bus
message or health signal. Agent event reads exclude the aside event family
before applying their page limit, even for an imported row with a member id.
Snapshot, Subscribe and terminal attach remain operator-only.

## Foundation evidence

Logs and pinned artifacts live under `target/features-p2/`. Provider calls and
the operator's running daemon are not part of these checks.

| Gate | Foundation evidence | Integrated proof still required |
| --- | --- | --- |
| P01 | Existing legacy byte fixtures and actual daemon Session projections; concurrent legacy/grouped stream test. | Frozen `b4f9a442` client/new-daemon matrix with all enabled services. |
| P02 | Host client fake-peer tests distinguish old negotiation refusal from authentication/transport failure and validate exact accepted subsets. Unnegotiated feature calls enter no handler. | Actual frozen old-daemon/new-client matrix and every public intent adapter. |
| P03 | Authenticated/frozen/partial/unsupported negotiation; physical epochs; role/readiness dispatch; server-side 1/1/4 bounds. Native policy and real gateway suites cover the landed P4 lanes. | Positive production registration for each reviewed capability. |
| P04 | Restart, explicit and old-binary clear, incarnation reuse, stale pre-callback writes, scoped member reuse and own archived-run queries. | Full domain lifecycle/source/launch composition and historical collection. |
| P05 | Shared commit/projection ports, rollback and duplicate controls, no-op delivery, coherent model capture, separate streams and escaped frame bound. | Real Epic factories, bootstrap, captured subagent membership, closure notice coalescing and empty-epic AllDone behavior. |
| P07 | Initial reply barrier; live refused Docs replacement; epoch/end/error/run-cancel terminal frames; slow-reader bounds and physical-close cancellation. | Actual Docs policy/source refresh, aside/metrics observers and the reviewed shared-client recovery follow-up. |
| U17 | Handler spies prove auth, negotiation, readiness and foreign-run refusal before metrics dispatch, with explicit own historical scope preserved. | Production metrics registration/source lifecycle plus P4/P6 transport and revocation composition. |

Focused foundation tests: 22 passed in
`foundation-private-and-bounds-02.log`; persistence identity/durability and host
client discovery suites are also included in the required broad checks.

The first broad native run passed the 617 library tests but one gateway fixture
failed during `native start`, before the tested route opened
(`workspace-native-01.log`). The unchanged gateway suite passed all 21 cases
with one test thread (`native-gateway-replay-01.log`). The final broad run uses
four test threads and `--no-fail-fast`, preserving the original failure log.
Final workspace, strict Clippy, format and Rust 1.82 results follow.

On the source checkpoint, the final broad run passed 1,440 tests with four
ignored (`workspace-native-02.log`, required sandbox mode, four test threads).
Strict workspace/all-target Clippy including `crew/native-gateway` passed
with `-D warnings` (`clippy-01.log`); format passed (`fmt-01.log`). The locked
default CLI built with Rust 1.82.0 and parsed version/help without installing
or starting a daemon (`msrv-01.log`).

Stable native CLI, native library-test and Rust 1.82 CLI executables are tagged
with source `57110710` in `target/features-p2/binaries.json`; that file records
their SHA-256 values and byte counts. The first port checkpoint's separately
cited artifact remains intact.

After the reviewed main merge and readiness-accessor addition, the final
feature suite passed all 22 cases (`final-main-focused.log`) and the changed
bundle-budget suite passed both cases (`final-main-budget.log`). Strict
workspace/all-target native Clippy, format and the Rust 1.82 default CLI gate
passed again (`clippy-final-main.log`, `fmt-final-main.log`,
`msrv-final-main.log`). The unchanged production source needs no additional
broad rerun for the upstream doc/probe and test-budget merge.

## Deferred integration ledger

`features-p6-daemon-wiring` owns actual registration and readiness of the
reviewed services, not a registry of test stubs. Its required inputs are the
approved P3 services, metrics-aside-streams, the service host contracts and the
P4 client recovery follow-up. Credential-specific aside paths remain explicitly
unavailable until their separate proof lands.

Docs needs validated workspace/member roots and published tips, three-layer
configuration and refresh independent of team reload. Asides needs immutable
private launch/session/toolbox/role capture, scoped executor retirement, durable
receipts, audit injection and proven usage binding. Metrics needs source owner
registration, work/account/cohort binding, collection and corrections. Epics
needs one founding bootstrap, all real factory/transition hooks, captured parent
membership and closed/global notice coalescing. Persisted `epic.closed` events
carry the closure information through the shared publication port.

The integration gate must include production construction paths, actual
daemon/client recovery, old/new peers, authorization, slow readers, run/source
replacement and final frame budgets. A test-only Ready service in this report
is evidence for the foundation port, not an enabled domain backend.
