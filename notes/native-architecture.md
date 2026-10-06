# Native application architecture

Status: N1 foundation and the N2/platform contract approved; gateway security
remains a separate gate, 2026-10-05. This document defines
new implementation work; it does not claim that native applications already
exist or have been built. Baseline: `6049cdf6`, protocol 6. The operator requested
iOS, Linux, and macOS with maximum useful sharing across native, web, and TUI,
without React Native. Native platform controls are the implementation target;
there is no webview application shell.

## Stable contract before implementation

The following boundaries do not depend on the final layout or packaging choice.
N1 starts after its own reviewer approval. Detailed transport, FFI, UI, web
adoption, packaging, and acceptance work continues on `native-platform-contract`
and does not block the independent extraction seed. Broader principles below
constrain that later work; they do not authorize unreviewed implementation.

1. One portable Rust client library owns domain state, snapshot/push reduction,
   bounded history, question reconciliation, stable agent/task ordering,
   executing-work selection, action eligibility and planning, and semantic
   presentation. It is consumed directly by the TUI and Linux app and through
   generated Swift bindings by the Apple apps. Sharing only daemon RPC types is
   insufficient.
2. The portable library has no filesystem, process, socket, UI toolkit, platform
   credential store, ambient clock, or random-number access. Host implementations
   supply received frames, monotonic time, wall time, random values, connection
   events, and operation results. Network and credential ownership never leaks
   into view components.
3. Existing `crew-core` public paths remain available through reexports during
   extraction. Protocol 6 bytes, defaults, identities, validation, and existing
   authorization remain compatible. Platform configuration loading and the Unix
   operator client remain outside the portable dependency graph.
4. State is scoped by endpoint, workspace ID, workspace generation, daemon
   incarnation, and local connection attempt. Replies and pushes from obsolete
   attempts cannot mutate the current workspace, complete its draft, or trigger
   an action. Index availability and workspace availability are distinct.
5. A snapshot is an atomic replacement. Dropping pushes while it is in flight is
   safe only when snapshot and subscription share the connection on which the
   daemon guarantees its barrier. Web uses that mode: replace state on the
   reply, then apply subsequent pushes in received order. Today's TUI subscribes
   on one connection and fetches its snapshot on another; it must retain queued
   pushes, apply the snapshot, then consume the overlap. N1 does not change
   either mode. A later reducer must explicitly support both modes or obtain
   separate review for a TUI transport migration. Protocol 6 has no global replay
   cursor across all push kinds. See `crates/crew-tui/src/net.rs:67-120` and
   [the server barrier](web-design.md#transport).
6. Mutating actions are scoped to the workspace and target captured when the
   user starts them. Validate scope and capability again on confirmation and
   dispatch. A transport timeout after sending means the outcome may be unknown;
   do not automatically replay a mutation. Preserve existing idempotent message
   IDs and explicit retry semantics.
7. Shared presentation returns semantic values: IDs, counts with coverage,
   status/tone, action availability and reasons, selected work, durations,
   warning facts, and evidence references. Native hosts own controls, layout,
   focus, accessibility, local date/number formatting, navigation, clipboard,
   haptics, and rendering. Terminal emulation is a separate platform renderer.
8. Web must adopt actual shared policy/code in its production path, with
   differential fixtures against Rust. A full Rust/WASM reducer is a measured
   integration choice, not permission to exceed existing byte, CSP, dependency,
   startup, or idle-work budgets. Generated declarations alone do not satisfy
   client-logic reuse.
9. The existing browser gateway remains loopback-only. The operator selected a
   dedicated scoped Crew gateway (relayed in `m-8b3375948f11`): device credentials, selected
   workspace grants, and individual revocation. Full-account SSH was rejected.
   `native-gateway-contract` owns the separate security/server gate. Native
   core, bindings, local desktop transport, and presentation can proceed without
   claiming a working remote path. Never forward the local operator token to a
   phone or weaken browser Host/Origin checks.
10. Linux must have a real connected daemon workflow, beyond a fixture or static
    scaffold. Apple source completion, Rust cross-compilation, Xcode builds,
    simulator runs, and signed device testing are separately reported gates.

### State/action boundary

The complete N2 records and adapter rules below define this boundary. N1's
already approved extraction remains a separate slice; its APIs are unchanged.

`ClientInput` carries typed connection/listing changes, a snapshot, a push,
operation result, timer expiration, or lifecycle change. Each asynchronous input
includes its scope/attempt. `ClientUpdate` carries a monotonically increasing
local revision, changed entity IDs/regions, and explicit effects for the host.
For a dashboard connection, workspace ID, generation, and daemon incarnation
come from `WorkspaceEntry`; `DaemonRef` supplies PID/start time. A direct Unix
connection knows a local endpoint/socket identity and the `HelloInfo` PID, but
has no dashboard generation. Snapshot `SessionInfo` can supply workspace/start
time only after loading. Those unavailable values remain `None`; they are not
fabricated from PID or path. Every new local connection gets a new attempt,
including same-PID reconnects. This boundary does not change the TUI transport.

Effects describe RPC requests, requested wake-up deadlines, connection changes,
and user-facing outcomes. They are not arbitrary native callbacks or executable
commands. `ViewQuery` supports bounded history pages rather than copying entire
history through FFI per frame. History capacities are host-supplied: preserve
TUI 500 messages/2,000 events and web 1,000 messages/5,000 events. Native limits
are defined for the new app below, not an increase to the existing clients.

`ActionPlan` is either unavailable with a stable reason, a confirmation request
bound to an immutable target/scope, or a typed command effect. The core owns
pending-operation identity and outcome classification; the host owns transport
request IDs, cancellation, and dispatch. Credentials are absent from the public
domain state, view projections, diagnostics, and action result records.

## Framework direction under verification

Use SwiftUI with targeted AppKit/UIKit integration on Apple platforms, and
GTK4 widgets with Rust bindings on Linux. Keep platform shells separate so the
shared library remains reusable by the existing TUI and web. UniFFI is the Swift
binding candidate; it supports Swift generation but does not package libraries
for Apple targets. GTK4 development requires both Rust and GTK system libraries.
These are verified framework facts, not a successful application build.
([UniFFI](https://mozilla.github.io/uniffi-rs/latest/),
[GTK installation](https://gtk-rs.org/gtk4-rs/stable/latest/book/installation.html))

## N1: independently reviewable extraction seed

Owner: `scheduler`. Reviewer approved this seed in `m-77242b75959b`, conditioned
on the concrete requirements below being checked at code review. It does not
wait for remote connectivity or Apple packaging decisions.

- Add `crates/crew-protocol/{Cargo.toml,src/lib.rs,src/types.rs,src/vcs.rs}`.
  Move `crew-core/src/types.rs` and `vcs.rs`, including their tests, without
  changing wire representations. Dependencies are `serde` and `serde_json`.
  Keep `crew_core::types`, `crew_core::vcs`, and root domain exports working by
  reexport from `crew-core/src/lib.rs`; remove the original files so both moves
  show 100% similarity, including tests. Leave `rpc.rs`, `client.rs`, `config.rs`,
  `paths.rs`, and `signals.rs` in `crew-core` in this slice.
- Add `crates/crew-client/{Cargo.toml,src/lib.rs,src/order.rs,src/work.rs}`.
  Expose the following pure functions; no UI toolkit or network dependencies.
- Migrate `crew-tui/src/app.rs` ordering and work selection to these functions.
  Keep rendering and selection-repair behavior. Preserve `App::work_of`,
  `WorkOn.title`, `App::satellite_parent`, and public `natural_cmp` compatibility
  wrappers. `tests/render.rs` must pass without edits. Use
  `crew-tui/Cargo.toml` for the direct dependency. The `crew-client` crate docs
  distinguish it from the existing Unix socket client at `crew_core::client`.
- `scheduler` owns workspace membership and the lockfile for this seed. Later
  slices coordinate membership/dependency changes through one integrator.

```rust
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering;
fn is_orchestrator(agent: &Agent) -> bool;
fn satellite_parent_id<'a>(agents: &'a [Agent], agent: &Agent)
    -> Option<&'a str>;
fn sort_agents(agents: &mut [Agent]);

struct CurrentWork {
    task_id: String,
    done: bool,
    elapsed_ms: Option<i64>,
}
fn current_work(tasks: &[Task], agent_id: &str, server_now_ms: i64)
    -> Option<CurrentWork>;
```

Current work ranks executing (`working`, or started `blocked`) before `waiting`,
then other started nonterminal work; a newer start wins within a rank and an
equal start keeps board order. Only then fall back to completed work. Failed and
unstarted queued work never displace executing work. Elapsed values clamp at
zero. Preserve the existing TUI completed choice exactly: maximum
`Option<completed_at>`, `None` below `Some`, last entry among equal values.
Sorting remains stable. No unused completed-tie policy parameter is introduced.
Web selects the first of equal completion times and coalesces missing time to
zero; vectors must expose those differences. A cross-client policy parameter
requires a real consumer, and convergence requires separately reviewed behavior.

N1 validation on this host (coordinate with `qa` before using its quiet testing
window):

- Moved domain/VCS tests pass unchanged. Add `crew-client` tests covering every
  case in `web/test/work.test.ts`, natural ordering, satellite/orphan behavior,
  stable sort ties, equal completed timestamps, and missing completion time.
  Expected results use the stated Rust semantics where existing web differs.
- `cargo test --workspace` and unchanged `crew-tui/tests/render.rs` pass.
- `cargo check --target wasm32-unknown-unknown -p crew-protocol -p crew-client`
  passes. `cargo tree -p crew-protocol -e normal` and `cargo tree -p crew-client
  -e normal` show only `serde`, `serde_json`, their transitive dependencies, and
  the internal `crew-protocol` edge. The new client adds no other dependency.
- `crates/crew/tests/fixtures/web/**`, protocol 6, `web/**`, and
  `crates/crew/web-dist/**` are unchanged; manifest source stays
  `fnv1a64:f22f3cf5ad74d25f`. Daemon `roster.rs::work_of` remains unchanged.

`graph` owns the separate `native-state-fixtures` preparation card at
`fixtures/native/state/**`; it is not an N1 dependency while unpublished and
unreviewed. Its eventual vectors supplement the named existing test sources.
A WASM check verifies portability, not browser integration. No daemon scheduler
changes, protocol bump, transport, generated bindings, or shell code are in N1.

## Subsequent gates

The GTK direction requires a reproducible native development environment:
`shell.nix` currently includes `pkg-config` but no GTK. The feasibility owner
verified GTK system metadata through `nix-shell -p gtk4 pkg-config gcc`; the
shell implementation must add its explicit environment and prove build/run on
this host. GTK is not presumed available from the base PATH.

The orchestrator approved a narrow W12 exception (`m-331755b19fb4`) for one
committed generated web policy file, from the same typed Rust descriptors used
by the Rust client at runtime. A separate web-adoption card must amend W12 in
`notes/web-design.md`, name the file/generator, add a Rust stale-output test,
replace existing production policy imports without behavior changes, and
rebuild/review the bundle within existing budgets after the baseline handoff.
Web still has TypeScript reducer, ordering, and work-selection ports checked
against Rust behavior vectors; it does not yet share the Rust client engine.
There is no extra runtime dependency, CSP change, or WASM in that first step.
Web builds must continue to work without Cargo; `cargo install` without Node.

## Baseline budgets and evidence

The reviewed manifest is `fnv1a64:f22f3cf5ad74d25f`. It contains 230,415 bytes of
nonterminal JavaScript, 33,906 bytes of CSS, 332,915 bytes of terminal JavaScript,
and 597,922 total bytes. Existing ceilings remain P1 230,500, P2 34,000, P3
350,000, P4 600,000; the three web runtime dependencies remain unchanged. There
is no assumed space for another engine. Any web integration must replace code
and measure its complete shipped cost, including WASM when applicable.

The current host is Linux x86_64. Initial PATH inspection found Rust, Node, Bun,
Nix, and Jujutsu; it found no Swift, `xcodebuild`, or `xcrun`. Apple build and
simulator verification is not available merely because Rust lists an Apple
target. The platform feasibility inventory will record exact installed versions
and build prerequisites. The operator explicitly deferred Apple builds and
tests and said an Apple runner is available for later use. Complete Apple
source/projects and reproducible runner commands remain in scope; unperformed
Xcode/device/simulator checks are reported as deferred, not as implementation
blockers or successful verification.

## N2: portable engine and concrete consumer contract

Reviewer approved this platform contract at `6eefc480` (`m-b55ebdda14d2`),
releasing separate engine, projection, action, binding, local-transport, and
shell cards. This does not approve the remote gateway design. The supporting source
inventory is [native-core-inventory.md](native-core-inventory.md); the experience
contract is [native-apps.md](native-apps.md). This section resolves their earlier
API/path proposals. Native clients initially follow the TUI's completed-work
selection from N1. Existing web differences remain explicit compatibility cases.

### Crates and dependency direction

| Package | Owns | Must not depend on |
| --- | --- | --- |
| `crew-protocol` | N1 domain types; N2 config-free RPC records/constants, `Push`, `ServerFrame`, `Snapshot`, workspace listings and read results | `crew-core`, native UI, network runtimes, platform APIs |
| `crew-client` | Domain reduction, requests/actions, stable query projections, policy descriptors, pure session/timeout/retry decisions | `crew-core`, filesystem/process/clock/entropy APIs, UI, FFI or network libraries |
| `crew-core` | Existing configuration, local operator/agent authorization, Unix client, paths and signals; compatibility reexports | Native UI/bindings |
| `crew-client-transport` | Native connection driver, bounded framing, request correlation and local IO; gateway adapter added only under its own reviewed contract | UI widgets |
| `crew-client-ffi` | Thin UniFFI conversion facade; reference ownership and error conversion; optional local transport binding | UI frameworks; a second state reducer |
| `crew-desktop` | GTK application, platform input/storage/navigation/accessibility, native terminal widget | Apple source/frameworks |
| `apps/apple` | SwiftUI application/shared views, UIKit/AppKit integration, OS storage/lifecycle and host effect execution | Web frontend/runtime |

N2 moves the config-free definitions from `crew-core/src/rpc.rs` to
`crew-protocol/src/rpc.rs`, retaining their original public paths by reexport.
`Call` and `Request` stay in `crew-core`: they pull in declarative team config
with host-bound inherent methods. Do not feature-gate or duplicate all of that
configuration merely to compile an app. The protocol crate may add the existing
`thiserror` dependency for `RpcError`; the dependency tree must remain portable.

`crew-client::Command` is a closed enum of the existing client read/control
operations, with typed parameters. Its encoder writes the existing protocol 6
`{id,method,params}` envelope. Every variant is round-tripped through the real
`crew_core::rpc::Request` in compatibility tests. No arbitrary method string,
configuration loader, shell command or new daemon action is exposed to a view.
Server errors and unknown-push behavior retain their existing wire meanings.
Known malformed frames fail validation; an unknown push is safely ignored.

The exact `Command` variants for this delivery are `Ping`, `Snapshot`,
`Subscribe`, `Workspaces`, `AgentsList`, `TasksList`, `TaskGet`, `History`,
`MessageGet`, `Events`, `Inbox`, `AgentOutput`, `AgentTranscript`, `GitDiff`,
`ModelsList`, `Send`, `Answer`, `AgentPause`, `AgentResume`, `AgentCompact`,
`AgentRestart`, `AgentRestartFresh`, `AgentKill`, `AgentSetModel`, `AgentFocus`,
`AgentInput`, `Resume`, `Shutdown`, `Attach`, `AttachInput`, `AttachResize`.
Parameters, defaults and serde names match the same-named `Call` at protocol 6;
only bounded counts use `u32` instead of host `usize`, with checked conversion.
`Inbox` for a native read always uses `unread_only: false, ack: false`; mutation through an inbox
read is not offered. `Hello`/enrollment belong to connectors, not user intents.
No `WebOpen`, `WebClose`, `WebPair`, `LoadTeam`, agent/team/task administration,
credential retrieval, hook or VCS mutation enters this app enum.

The deliberate small application enum excludes far more than the three
configuration-bearing `Call` variants. Extracting the whole enum while splitting
those three would still expose agent-only/admin operations, require changing
every full-Call match, and disturb host configuration method ownership. N2 keeps
the host enum intact and tests the bounded duplicate wire encoder explicitly.
Use `crates/crew-client/tests/command_wire.rs`, with `crew-core` as a
**dev-dependency only**: normal portable dependency trees remain clean. One
macro/catalog defines the variant set and exhaustive test constructors. Every
variant checks emitted bytes against `serde_json::to_string(&Request)`, parses
through the real request type, and checks its method against `Call::method()`.
Adding a variant without a case must fail compilation/exhaustiveness, not merely
lower test coverage. Compare boundary/default/optional values as well as a
single happy-path sample.

### Identity and state records

All fields below are part of the implementation contract. Domain record fields
reuse their definitions in `crew-protocol`, rather than a second handwritten
model. Integer milliseconds are not floating-point dates. Host times are passed
in; arithmetic is checked or saturating and never panics on a timestamp.

| Record | Fields and meaning |
| --- | --- |
| `ClockSample` | `wall_ms: i64`, `monotonic_ms: u64`. Timeouts use monotonic time; snapshot `server_time` provides the offset for display durations. |
| `ClientScope` | `endpoint_id: String`, `workspace_id: String`, `generation: Option<u64>`, `daemon_pid: Option<u32>`, `daemon_started_at_ms: Option<i64>`, `attempt: u64`. Full value accompanies asynchronous data; attempt is process-local and never reused during a core instance's lifetime. Index-server PID is never a workspace daemon PID. |
| `EndpointStatus` | `Disconnected`, `Connecting`, `Authenticated`, `Reconnecting`, `SignedOut`, `Incompatible`, `Stopped`; safe display reason separately optional. Authentication alone does not make a workspace live. |
| `WorkspaceStatus` | `NoneSelected`, `Connecting`, `Live`, `Reconnecting`, `Stopped`, `Unreachable`, `Incompatible`, `Removed`, `NeverJoined`, `AccessRevoked`; loaded/stale state and last synchronization time separately represented. Safe source detail remains separate. |
| `ConnectionMode` | `LocalOperator` or `ScopedGateway`. This describes established authority, not a user-set assertion. The transport supplies it only after successful authentication. |
| `SnapshotMode` | `OrderedConnection` or `BufferedOverlap`. Native drivers use the former when snapshot and subscribe share a connection. TUI retains the latter and its existing subscriber/read arrangement. |
| `ClientLimits` | `messages: u32`, `events: u32`, `context_points: u32`, `buffered_pushes: u32`, `pending_operations: u32`, `page_size: u32`. Zero or values above the native preset maximums below are rejected. |
| `ClientConfig` | Limits and snapshot mode. N1's Rust work-selection semantics remain the initial native behavior; no unused cross-client policy knob. |
| `EntityKey` | Scope plus enum `Agent(id)`, `Task(id)`, `Message(id)`, `Event(seq)`. A local connection's provisional workspace key cannot alias a gateway ID. |
| `ClientRevision` | Monotonic local `u64`, advanced only when visible state, resource state or pending-operation state changes. Revision overflow is an explicit terminal error, not wraparound. |
| `ChangeSet` | `revision: u64`, `agents: Vec<String>`, `tasks: Vec<String>`, `removed: Vec<EntityKey>`; Boolean `session`, `questions`, `messages`, `events`, `usage`, `availability`, `pending`, `reset`. No rendering instructions. |
| `Grant` | Opaque `grant_id: String`, `tiers: Vec<GrantTier>`, `workspace_ids: Vec<String>`, `expires_at_ms: Option<i64>`. `GrantTier` is `Read`, `Control`, `Terminal`; gateway Read is default, Control and Terminal independent explicit host grants. Local grants come from the trusted local driver. Exact gateway method mapping belongs to `notes/native-gateway.md`, not a guessed client allowlist. |

On direct Unix access, `workspace_id` begins as a host-local profile/socket key.
The adapter records the canonical workspace from the authenticated snapshot;
changing that key invalidates outstanding requests. Listing-only fields remain
absent until supplied by their actual source. Compare the *whole* scope on every
incoming result and push. A reconnect creates a fresh attempt even if PID and
directory are unchanged. Do not infer a fresh daemon from a dropped socket.

The native preset is 1,000 messages, 5,000 events, 600 context samples per agent,
256 queued pushes, 64 pending operations and at most 200 records per query
page. These are independent bounds, not a guarantee that all collections are
constant size: agents/tasks/questions remain the daemon's authoritative set.
Preserve TUI's 500/2,000 history preset. Snapshot request defaults remain 300
messages/1,000 events where the current web behavior is adopted. The 1 MiB
maximum frame and 64 KiB message body limits still apply before decoding or
submission. Additional per-resource limits follow the existing read RPCs.

`ClientInput` is a closed enum with these variants:

- `Bind { scope, mode, grant, snapshot_mode }` and `Unbind { scope, reason }`.
- `EndpointChanged { endpoint_id, attempt, status }` and
  `Listing { endpoint_id, attempt, listing: Workspaces }`.
- `SnapshotStarted { scope, operation_id }`,
  `Snapshot { scope, operation_id, snapshot: Snapshot }`,
  `Push { scope, push: Push }`.
- `OperationFinished { scope, operation_id, outcome: OperationOutcome }`.
- `AttachmentEnded { scope, attachment_id, reason: TerminalEnd }` for a
  terminal channel ending independently of an action reply.
- `Wake { timer_id, monotonic_ms }` and `Lifecycle { active: bool }`.

`ClientCore::new(ClientConfig)`, `apply(ClientInput, ClockSample)`,
`view(ViewQuery, ClockSample)`, `plan(ActionIntent, ClockSample)`, and
`confirm(confirmation_id, ClockSample)` return typed `Result` values. Core calls
are serialized; there are no core-created threads or callbacks. `apply` and
`confirm` return `ClientUpdate { revision, changes, effects, outcomes }`, where
`outcomes: Vec<OperationNotice>` carries finalized operations. A stale scope or
canceled operation yields a no-op update rather than a failure on the new page.
`plan` returns `PlanUpdate { plan: ActionPlan, update: ClientUpdate }`, so pending
state and the decision are observed together. Request effects are the only
dispatch mechanism; a `Dispatch` plan describes the same operation, not a
second command to execute.
`cancel_confirmation(confirmation_id)` returns a no-op or changed update and
never dispatches an RPC. Confirmation records share the pending-operation cap;
cancel, confirm, unbind and disconnect remove them. Confirming a missing/canceled
token returns `StaleConfirmation`. There is no unbounded abandoned-dialog cache.

Final outcome ownership (reviewer `m-f565209487b2`): only Core appends operation
notices to `ClientUpdate.outcomes`, exactly once per logical operation ID. The
driver applies wire outcomes through `OperationFinished`; it never publishes a
second inferred completion. A held identified-message retry emits no final
notice until completion or expiry, including expiry from a Core wake-up.
SentUnacknowledged and ClosedLocally also reach Core as inputs before Core emits
their final notices. Driver mapping to `HostUpdate.outcomes` and
`ClientNotice.outcomes` is field-for-field. `ClientEffect::Notice` remains a
non-operation notice, not a hidden completion protocol. Ordinary state inputs
have an empty outcomes vector.

The trait has the following exact method boundary; construction remains an
inherent method on the eventual engine, not a placeholder N2A implementation:

```rust
pub trait ClientApi {
    fn apply(&mut self, input: ClientInput, now: ClockSample)
        -> Result<ClientUpdate, ClientError>;
    fn view(&self, query: ViewQuery, now: ClockSample)
        -> Result<ViewData, ClientError>;
    fn plan(&mut self, intent: ActionIntent, now: ClockSample)
        -> Result<PlanUpdate, ClientError>;
    fn confirm(&mut self, confirmation_id: u64, now: ClockSample)
        -> Result<ClientUpdate, ClientError>;
    fn cancel_confirmation(&mut self, confirmation_id: u64)
        -> Result<ClientUpdate, ClientError>;
}
```

In `OrderedConnection`, pushes received between `SnapshotStarted` and its
matching snapshot response are discarded because that exact connection supplies
the daemon barrier. In `BufferedOverlap`, they are queued in received order,
then applied after atomic snapshot replacement with existing message/event
deduplication. Exceeding the queue bound must force a fresh synchronization;
never mark the stream live after silently dropping data. TUI's existing network
layer can continue delivering the snapshot first and queued pushes afterward;
it must not introduce a discard interval. No migration of TUI transport is
required to share its reducer.

Availability mapping is explicit: gateway `running` plus synchronized live
channel becomes `Live`, otherwise `Connecting`/`Reconnecting`; `stopped`,
`unreachable`, and `incompatible` map to their distinct counterparts with source
detail. A missing listing ID is `Removed` if previously seen and `NeverJoined`
otherwise; explicit revoked authority becomes `AccessRevoked`. A direct local
profile can show `Stopped` only from affirmative process/registry evidence;
missing socket, refused connection or missing metadata without that evidence is
`Unreachable`. Exact protocol mismatch is `Incompatible`. A local discovery
list is host profile metadata, not a fabricated `Workspaces` listing with
invented generations. Selecting it establishes a real scope through auth and
snapshot. Endpoint authentication failures are endpoint status/reasons, never
evidence that a team stopped.

Gateway endpoint/device/grant generation changes retire the old context before
binding a fresh attempt. The adapter maps those values to opaque `grant_id`
and scope identity; portable core need not embed the gateway handshake schema.
The grant is a presentation/preflight input only. The daemon and gateway enforce
authorization for every method/channel; modifying a client grant gains no power.
Revocation terminates affected requests/terminal channels, disables actions and
purges that scope's native evidence/drafts when the client learns of it. This
does not promise erasure of data a disconnected device already received.

Snapshot replacement resets current entities, questions and bounded histories.
Message overlap deduplicates the newest 64 message IDs, matching both current
clients; event overlap ignores nonzero sequence numbers no greater than the
last accepted event, while sequence zero retains its existing meaning. Open
questions reconcile independently of the history ring. Questions pending an
answer are hidden as today, restored on failure, and retired by the authoritative
answer or a snapshot that omits them. A client never changes task/agent lifecycle
optimistically. Task scheduling, parking and resumption remain daemon work.

### Queries and semantic presentation

`ViewQuery` has variants `Overview`, `Team`, `Tasks`, `AgentDetail { id }`,
`TaskDetail { id }`, `Messages { before, limit, filter }`,
`Events { before, limit, filter }`, `Usage`, `Questions`, and
`Evidence { key, kind }`. History cursors include scope and revision; a cursor
whose bounded data has expired returns `CursorExpired`, never an unrelated row.
Querying retained local data sends no RPC. `ActionIntent::Read` requests data
missing from a query and records the entity, resource kind and request sequence.

Every `ViewData` result carries optional scope (absent before binding), revision,
optional synchronization time and
freshness (`Loading`, `Fresh`, `Stale`, `Unavailable`). Its payload matches the
query variant. The data comprises protocol records plus these shared derived
records:

- `AgentPresentation`: agent ID, semantic status/tone, current `CurrentWork`,
  separately retained Waiting task IDs, parent/satellite IDs, warning reasons,
  model/runtime labels, optional progress/context, and available actions.
- `TaskPresentation`: task ID, dependency IDs with present/missing status,
  dependency layer/cycle facts, assignee/reviewer and semantic state.
- `UsagePresentation`: optional reported totals, reporter count/eligible count,
  runtime quota groups and freshness, context samples, paused-at-limit IDs.
  Missing reports remain missing; zero stays zero. No cost estimate is derived.
- `ResourceState`: resource key, `Idle`/`Loading`/`Ready`/`Empty`/`Unsupported`/
  `Failed`/`Stale`, optional safe error, and the existing typed RPC result.
  Each read completion must still match its resource request sequence.

The core returns semantic tones/labels and raw timestamps/durations, not colors,
SwiftUI views, GTK widgets, pixel positions, locale strings or terminal glyphs.
Graph relations, task layers and event-mark expiry can be shared; geometry,
hit-testing and reduced-motion rendering remain platform-owned. New clients
read one immutable projection per update, with incremental changed IDs and
bounded history pages. Do not serialize a complete history through FFI on every
agent update. Lists provide a complete independent navigation/accessibility
path; native Team graphs and task DAGs remain final requirements, not deferred
features.

### N2A projection record closure

The following fields close the query/projection contract. All domain names
(`Agent`, `Task`, `Message`, `Event`, `SessionInfo`, result DTOs and their nested
types) refer to the exact moved protocol definitions at the baseline, with all
fields retained. These are Rust records first, with the ABI conversion below.

```rust
enum Tone { Neutral, Info, Success, Warning, Danger }
struct WarningFact { code: String, text: String, entity: Option<EntityKey> }
struct ActionAvailability { action: ActionKind, available: bool,
                            reason: Option<ClientReason> }
struct ClientReason { code: String, message: String }
struct AgentPresentation {
    agent: Agent, status_label: String, tone: Tone,
    current_work: Option<CurrentWork>, waiting_task_ids: Vec<String>,
    satellite_parent_id: Option<String>, subagent_ids: Vec<String>,
    warnings: Vec<WarningFact>, actions: Vec<ActionAvailability>,
}
struct DependencyState { id: String, status: Option<TaskStatus> }
struct TaskPresentation {
    task: Task, dependencies: Vec<DependencyState>, layer: u32, in_cycle: bool,
}
struct AgentRelation { parent_id: String, child_id: String }
struct TaskLayer { index: u32, task_ids: Vec<String> }
struct OverviewData {
    session: Option<SessionInfo>, agent_count: u32, working_count: u32,
    task_count: u32, done_count: u32, question_count: u32,
    warnings: Vec<WarningFact>,
}
struct TeamData { agents: Vec<AgentPresentation>, relations: Vec<AgentRelation> }
struct TasksData { tasks: Vec<TaskPresentation>, layers: Vec<TaskLayer> }
struct AgentDetailData {
    agent: Option<AgentPresentation>, assigned_tasks: Vec<TaskPresentation>,
    review_tasks: Vec<TaskPresentation>, recent_messages: MessagePage,
    resources: Vec<ResourceState>,
}
struct TaskDetailData { task: Option<TaskPresentation>, card: ResourceState }
enum MessageEndpoints { All, Agent { id: String }, Pair { first: String, second: String } }
struct MessageFilter { endpoints: MessageEndpoints, kind: Option<MessageType>, text: String }
struct EventFilter { agent_id: Option<String>, level: Option<EventLevel>, text: String,
                     vcs_only: bool }
enum HistoryAnchor { Message { id: String, timestamp_ms: i64, ordinal: u64 },
                     Event { seq: u64, ordinal: u64 } }
struct HistoryCursor { scope: ClientScope, revision: u64, anchor: HistoryAnchor }
struct MessagePage { items: Vec<Message>, before: Option<HistoryCursor>,
                     has_older_local: bool, may_have_older_remote: bool }
struct EventPage { items: Vec<Event>, before: Option<HistoryCursor>,
                   has_older_local: bool, may_have_older_remote: bool }
struct ContextPoint { at_ms: i64, percent: u8 }
struct AgentUsage { agent_id: String, usage: Option<Usage>, context: Option<ContextUsage>,
                    samples: Vec<ContextPoint>, compactions: u32 }
struct QuotaRow { quota: Quota, stale: bool }
struct RuntimeQuotaGroup { runtime: RuntimeKind, quotas: Vec<QuotaRow>,
                           paused_agent_ids: Vec<String> }
struct UsagePresentation {
    total: Option<Usage>, reporters: u32, eligible: u32, overflowed: bool,
    agents: Vec<AgentUsage>, groups: Vec<RuntimeQuotaGroup>,
}
enum ResourceKind { TaskCard, Message, Event, Transcript, Diff, Output, Inbox,
                    Models, History, Events }
struct ResourceKey { entity: EntityKey, kind: ResourceKind, options: ReadOptions }
enum ResourceStatus { Idle, Loading, Ready, Empty, Unsupported, Failed, Stale }
enum ResourceValue {
    TaskCard(TaskCard), Message(Option<Message>), Event(Option<Event>),
    Transcript(TranscriptResult), Diff(GitOutcome), Output(OutputResult),
    Inbox(InboxResult), Models(ModelsResult), History(Vec<Message>), Events(Vec<Event>),
}
struct ResourceState { key: ResourceKey, status: ResourceStatus, request_sequence: u64,
                       error: Option<ClientReason>, value: Option<ResourceValue> }
enum ReadOptions { Default, Bounded { limit: u32 },
                   Output { lines: u32, screen: bool, ansi: bool },
                   Diff { against: Option<String> } }
struct ResourceRequest { key: ResourceKey }
enum ViewPayload {
    Overview(OverviewData), Team(TeamData), Tasks(TasksData),
    AgentDetail(AgentDetailData), TaskDetail(TaskDetailData),
    Messages(MessagePage), Events(EventPage), Usage(UsagePresentation),
    Questions(Vec<Message>), Evidence(ResourceState),
}
struct ViewData { scope: Option<ClientScope>, revision: u64,
                  synchronized_at_ms: Option<i64>, freshness: Freshness,
                  payload: ViewPayload }
```

`EntityKey` additionally permits `Workspace` within its existing scope, for
aggregate history/events/model queries. `ActionKind` is the discriminant of
the closed `ActionIntent`/`AgentAction` set below, without parameters. Every
unavailable action has a reason; available actions have none. A missing detail
returns its optional entity empty with `Unavailable` freshness, rather than
substituting a neighbour. `CurrentWork` retains N1's three fields.

`ViewQuery` arguments are concrete: Team/Tasks/Overview/Usage/Questions have no
arguments; AgentDetail/TaskDetail take `id: String`; Messages/Events take
`before: Option<HistoryCursor>`, `limit: u32` and the matching filter; Evidence
takes `key: ResourceKey`. Query limit is 1–200. Pair endpoints are unordered.
Text filters are case-insensitive literal substring matches over retained
message body, or event message/type/agent ID as in the current event view, never
regex or a promise of server-wide search. VCS-only uses the current event view's
VCS event predicate.
Results stay in history arrival order, oldest first within a page. An unrelated
agent revision does not expire a retained history anchor; wrong scope/kind,
evicted anchor or snapshot history replacement does. Snapshot replacement
records a history-generation watermark against the cursor revision.
The per-history `ordinal` disambiguates repeated sequence-zero events and
duplicate IDs; it is a client arrival identity, not a new daemon sequence.
`has_older_local` states retained data availability; `may_have_older_remote`
states uncertainty, not known server contents. Bounded snapshots never prove
server exhaustion. Only a corresponding scoped read refines that uncertainty.

Resource kind, entity and options must agree. TaskCard requires Task; Message
requires Message; Event requires Event and is local-only; transcript/diff/output/
inbox require Agent; models/history/events accept Workspace, with history/events
also allowing Agent. Native read sizes, sent explicitly, default to transcript
400, output 300, inbox 100, events 200 and history 300. These are native client
choices; daemon omitted-parameter defaults are output 60, inbox 50, history 100,
events 200 and transcript 400. Maximums follow current daemon limits:
transcript/output/history 2,000, inbox 500, events 5,000. Invalid options fail
before dispatch. History/event reads merge scoped returned data through the
same dedupe path; query filters do not add unsupported daemon parameters.
Canonicalized effective read options are part of the resource key, so two
transcript/history limits or diff revisions cannot share an accidental cache
entry. Request sequence still guards stale completions for that exact key.
The completed-resource cache is bounded by `ClientLimits.pending_operations`,
evicting least-recently-used nonloading entries. Loading entries are never
evicted and are separately bounded by the pending-operation limit, keeping total
retained resource entries at most twice that limit. An evicted resource queries
as Idle. Scope retirement purges affected entries; no unbounded tombstone map
is added. Duration calculations use saturating subtraction, including the N1
current-work helper, without changing ranks, ties or ordinary timestamp results.

Usage sums use checked `u64` arithmetic: no reporters produces `None`; reported
zero stays `Some(zero)`. Any overflow makes the total unavailable and sets
`overflowed`, while individual reports remain readable. Eligible count includes
all current agents as the web coverage denominator does. Quota staleness is the
existing 20-minute threshold. Shared task layers preserve
`app.rs::task_layers`/`layers.ts::taskLayers`: missing dependencies do not raise
depth, revisiting an active cycle yields zero, equal-layer order is input board
order. `in_cycle` is separately derived without changing those layers. Dependency
status `None` explicitly marks a missing card. Rendering geometry/barycentre
sweeps do not enter these records.

The ABI mirrors every protocol field recursively except `Message.metadata`
becomes `metadata_json: Option<String>` and `Event.data` becomes
`data_json: Option<String>`: JSON null maps to `None`, any other value to compact
JSON with deterministic sorted object keys. Round-trip tests restore semantic
JSON equality, including arrays, Unicode, numbers and empty objects. No arbitrary
`Value` crosses UniFFI. RPC error metadata stays inside the driver; user-facing
errors expose only safe code/message. N2A supplies the complete Rust record
closure; N4 mirrors it with exhaustively checked ABI converters. GTK can use
the N2A records directly; Apple compiles against N4's generated type checkpoint.
Neither requires a fake working reducer while implementation proceeds.

### Actions, confirmations and results

`ActionIntent` captures an explicit scope and target. Its variants are
`SendMessage { to, body, message_id }`, `Answer { question_id, body }`,
`Agent { id, action }`, `SetModel { id, runtime, model, note }`,
`AgentInput { id, text, submit }`, `ResumeLimited { restart }`, `StopTeam`,
`Read { resource }`, `Attach { agent, rows, cols }`,
`AttachInput { attachment_id, data }`, `AttachResize { attachment_id, rows, cols }`,
and `Detach { attachment_id }`. `AgentAction` covers pause, resume, compact,
restart, restart-fresh, kill and local Herdr focus. Terminal bytes are `Vec<u8>`
in action input and base64 only in the wire encoder. Attach dimensions are
`u16`; native hosts normalize rows to 5–500 and columns to 20–1,000 as the
current PTY backend does. `AgentInput.text` is optional,
`submit` Boolean; `SetModel` has optional runtime/note and a String model, with
no writable effort field. Detach closes the channel rather than inventing a
daemon RPC. Message IDs come from host cryptographic entropy; they are immutable
for one submission. A message with `type=task` is not task creation.
Every enum variant additionally has `scope: ClientScope`; the parameter lists
above elide that repeated field, not its requirement. Do not introduce a second
unscoped intent type or select the current workspace during dispatch.

`ActionPlan` is `Unavailable { reason }`,
`Confirm { confirmation_id, kind, scope, target_ids }`, or
`Dispatch { operation_id }`. The command is carried only by the returned
request effect, so a plan cannot dispatch it twice. `ConfirmationKind` has exactly four
values: `KillAgent`, `RestartFresh`, `RestartLimited`, `StopTeam`. Only planning
these four actions creates a confirmation. Resume, ordinary restart, compact,
pause and model selection retain their existing behavior.

The core stores the complete proposed command behind a confirmation ID; UI
cannot edit its target by returning a different ID/body. `confirm` checks that
the binding, grant and relevant target facts remain valid. A changed target or
affected limited-agent set invalidates the confirmation; opening it again shows
the current consequence. Unrelated push traffic does not invalidate a dialog.
Native stricter scope validation does not silently change the current web.
`ResumeLimited` captures explicit agent IDs for both Continue and confirmed
Restart. The dispatched `resume.agents` contains those IDs, never the empty
"all agents at execution time" wildcard. Newly limited agents cannot join an
already confirmed/in-flight command. This is a new native safety rule tested
separately from the current web's `agents: []` behavior.
The daemon continues accepting that empty list from the web; the baseline
`resume-unlimited-no-restart` fixture remains valid for the existing web path.

`OperationOutcome` is `Succeeded { result: CommandResult }`, `Rejected { code, message }`,
`NotSent { reason }`, `Canceled`, `OutcomeUnknown { reason }`,
`SentUnacknowledged { attachment_id }`, or `ClosedLocally { attachment_id }`.
Every `Succeeded` traces to a daemon reply;
`ClosedLocally` is an explicitly different local-operation result. A failure after
write initiation must not claim that the daemon rejected the action. Canceling
an already-sent call only abandons the local wait; it cannot undo daemon work.
There is no offline action queue. Drafts are host-local, keyed by
endpoint/workspace/recipient/question, retained on failure and never redirected
by switching views or workspaces.

Automatic retry is limited to the existing identified-message policy: on
connection closure (not a generic timeout), wait at most 30 seconds for verified
resynchronization, then retry once with the exact same message ID and payload.
Native additionally requires unchanged endpoint, workspace, daemon incarnation
and grant; a new socket attempt alone does not authorize replay into another
daemon. All other mutations, answers and terminal bytes are never replayed.
Read retries are explicit or required for protocol synchronization. Shared
timeouts remain ordinary 3,000 ms, lifecycle 30,000 ms, models 20,000 ms,
resume/shutdown 90,000 ms. Pending operations in different lanes cannot block
state updates or terminal IO.

An identified Send keeps one logical operation ID through its one authorized
replay; transport attempts use distinct `(channel_id, request_id)` pairs. This
does not allocate or reuse a different operation's identity. The exact message
ID, body and recipient remain immutable. The 30-second retry deadline starts at
the first eligible closure and never extends. Only a fresh verified snapshot of
the same endpoint/workspace/generation, proven daemon incarnation and grant can
rebind that pending logical operation to the new connection attempt. Unknown
incarnation facts do not authorize replay. Old-attempt results remain ignored;
the final current-scope notice uses the stable logical ID and scoped draft key.
Timeout, explicit cancellation, authentication failure or grant loss are not
closure-retry triggers.

`ClientEffect` variants are `Request { scope, operation_id, lane, command }`,
`CloseAttachment { scope, operation_id, attachment_id }`,
`Schedule { timer_id, deadline_ms }`, `CancelTimer { timer_id }`,
`Resynchronize { scope }`, and `Notice { code, entity }`. `RequestLane` is
`State`, `Read`, `Action`, or `Terminal { attachment_id: u64 }`. These are bounded work descriptions;
they cannot open arbitrary files, execute programs, or mutate a UI control.

Terminal-routing amendment approved at `a2628e85` (`m-7139012714dd`), following
the gap found in N2A checkpoint `0cad5304`; case names and stale-detach wording
were clarified in `986dcb8c`:
an Attach plan's newly allocated operation ID is also its attachment ID.
Operation IDs are not reused during a client instance's lifetime, so attachment
IDs are never reused within a scope. Its request
effect uses `Terminal { attachment_id: operation_id }`; subsequent input/resize
requests have fresh operation IDs but retain the original attachment ID.
Every terminal effect carries its full captured scope. The core and driver MUST
route only by `(scope, attachment_id)`, never a selected agent or "current
terminal". The driver maps that pair to exactly one channel. Input/resize/detach
of an unknown or closed attachment plans `Unavailable` with a reason, never a
Request or CloseAttachment;
the driver independently rejects an absent/retired route as a final guard.

Detach produces `CloseAttachment`, not a wire `Command`. Its operation finishes
only after local channel closure, using `ClosedLocally { attachment_id }`, never
a daemon acknowledgement. Repeated delivery of the same driver close effect is
idempotent; it does not emit a second outcome or terminal-end notification.
This differs from a new user Detach against a closed ID, which is unavailable.
Native plan results
expose the operation ID; on Attach that same ID identifies later input, resize,
detach and observer chunks. These fields let effects generated by apply/confirm
retain their target without a driver side-channel that remembers UI intents.

An allocated attachment starts pending and becomes live only when that Attach
operation receives `Succeeded { result: Attached { agent } }`. A failed,
canceled or outcome-unknown Attach leaves no attachment and closes any
provisional channel. Terminal bytes received before the acknowledgement remain
bounded in the driver and are not exposed as a live attachment. Unbind, scope
retirement and grant loss close every attachment of that scope. A closing
attachment is marked once; concurrent detach/retirement does not issue duplicate
close effects or outcomes. An unsolicited EOF, `pty_closed` or overflow is
reported as `AttachmentEnded` before subsequent input planning and produces
one terminal end notification, not a fake daemon action response. Explicit
Detach gets one `ClosedLocally` operation outcome and one end notification.

Attach-notice clarification (`m-1dc00f8e11b2`): the successful Attached result
produces `OperationNotice { kind: Success, operation_id,
attachment_id: Some(operation_id), ... }`. All other ordinary successes keep
`attachment_id: None`; failure outcomes are unchanged. This is a meaning for
the existing optional field, not a new record, outcome kind or wire field.
The core's final notice is published through `changed` before any terminal
bytes for that attachment. A platform's continuation of `plan` may run after
those callbacks, even though Rust has already returned the operation ID.

The UI creates a terminal renderer only from that identified Attach success,
keyed by client instance, full scope and attachment ID. It routes subsequent
chunks only to that exact renderer; unknown IDs are dropped and counted, not
buffered before attachment or matched by workspace/selected agent. The later
plan result associates its captured panel/agent by the same operation ID and
does not reset an existing or ended renderer. A canceled panel still cleans up
its actual attachment when the result arrives. Producer regressions verify the
successful Attach identity, unchanged ordinary success, the Core outcome path,
and notice-before-data delivery using the real core.

The `attach`, `attach_input` and `attach_resize` wire methods and parameters are
unchanged; attachment identity is local routing metadata, never an added RPC
parameter. Existing 64 Request-byte goldens remain valid. Before N2A release,
`content` adds `fixtures/native/actions/native/terminal-routing.json`, separately
tagged to this native contract. Case IDs are `native-terminal-two-attach`,
`native-terminal-route-input`, `native-terminal-route-resize`,
`native-terminal-close-one`, `native-terminal-stale-id`,
`native-terminal-unacked-attach`, `native-terminal-retire-scope`, and
`native-terminal-duplicate-close`. Together they cover two live attachments,
independent routing, closing one while the other remains live, unknown/stale
input/resize/detach, failed/canceled/unknown Attach, scope retirement, and one
close outcome/end notification per attachment. N2A checks their
constructor/shape contract; the planner/driver consumers must execute those
same behavior cases against their real implementations before acceptance.

One-way terminal-write correction (reviewer `m-beeac3daa0db`): protocol 6's
`Session::request` suppresses successful `attach_input` and `attach_resize`
replies (`crates/crew/src/daemon/server.rs:130-148`). Their handler's null value
never reaches a client. The driver emits exactly one
`SentUnacknowledged { attachment_id }` outcome when the complete frame has been
written to the authenticated channel and retires the pending wait immediately.
This means transport-written, not accepted/applied by the daemon; there is no
success-response deadline and no fabricated `Succeeded` result. A partial write
remains `OutcomeUnknown` and is never replayed. An authoritative rejection before
completion, or a failure before any write, uses Rejected/NotSent as appropriate.

A late error for one of those writes does not issue a second operation outcome.
The driver ends that fixed attachment once through
`AttachmentEnded { reason: TerminalEnd::Failed { reason }, ... }`, preserving
the full scope/attachment identity and making the failure visible. An unexpected
success frame for a retired one-way request is ignored, not decoded into a new
result. Attach and AgentInput still await their actual replies. No daemon wire
or server behavior changes. `CommandResult::Null` is removed because it has no
client-side producer. Content adds `native-terminal-input-unacked` and
`native-terminal-input-error-ends` to the same tagged terminal corpus; real
driver tests verify successful writes receive no response and late errors end
only their own attachment.

## Host transport and FFI boundary

### Protocol driver and local desktop path

`crew-client-transport::SessionDriver` translates core effects into wire
requests, maps per-channel request IDs to operation IDs, enforces deadlines and
framing bounds, and feeds decoded outcomes back to the core. It is the shared
implementation for native hosts, not copied into Swift. Each opened channel has
`ChannelId`, scope/attempt and purpose (`Index`, `Workspace`, `Read`, `Action`,
`Terminal`). The driver ignores late data for a closed channel. Hosts never use
an entity ID alone to choose a destination socket.

These value-based, event-driven ports are internal Rust transport interfaces.
Both local and future gateway IO remain in Rust; Swift does not shuttle raw
protocol JSON into and out of the engine. The Swift bridge receives typed view
projections, invalidations/outcomes and terminal byte chunks only:

```rust
enum HostEvent {
    Opened { channel_id: u64, context: ConnectionContext, peer: PeerInfo },
    Frame { channel_id: u64, text: String },
    SendProgress { channel_id: u64, operation_id: u64,
                   disposition: SendDisposition },
    Ended { channel_id: u64, reason: TransportEnd },
    Wake { timer_id: u64 },
    Lifecycle { active: bool },
}

enum HostEffect {
    Open { channel_id: u64, profile_id: String, purpose: ChannelPurpose },
    Send { channel_id: u64, operation_id: u64, text: String },
    Close { channel_id: u64 },
    Schedule { timer_id: u64, deadline_ms: u64 },
    CancelTimer { timer_id: u64 },
}
```

Authentication happens in the approved connector before `Opened`. Its verified
authority/grant and identity become the core binding through
`ConnectionContext { mode, grant, snapshot_mode, scope }`. The initial `Open`
effect contains only the configured profile and purpose; it cannot claim a
grant before authentication. `PeerInfo` contains version, protocol, PID and
optional project, with no token or proof secret. Callers cannot self-assert
`LocalOperator` through an ordinary action. Gateway-specific
pairing/TLS/challenge records do not belong in these v6 domain records.

`SendDisposition` distinguishes `NeverWritten`, `WriteStarted` and
`FullyWritten`. The host reports `WriteStarted` before invoking an IO write;
partial writes or an interrupted send are outcome-unknown. `FullyWritten` is
not daemon acknowledgement. `TransportEnd` distinguishes closure, authentication
failure, protocol mismatch, permission failure, TLS trust failure, revocation
and malformed input without embedding credentials. `CommandResult` is a typed
union of the existing command-specific result DTOs; loose JSON is internal to
the protocol decoder, not the native projection API.

The exact `CommandResult` closure is below. These are internal value results,
not extra wire tags; the driver decodes according to the outstanding command.
Boxing a large Rust payload is permitted without changing value semantics.

| Result variant | Command and verified protocol 6 payload |
| --- | --- |
| `Pong`, `Subscribed`, `Stopping` | Ping `{pong:true}`, Subscribe `{subscribed:true}`, Shutdown `{stopping:true}` respectively; validate the named Boolean before construction |
| `Ok` | AgentInput and AgentFocus `{ok:true}` |
| `Attached { agent: String }` | Attach `{attached:<agent>}` |
| `Snapshot(Snapshot)`, `Workspaces(Workspaces)` | Same-named response DTOs |
| `Agents(Vec<Agent>)`, `Tasks(Vec<Task>)` | AgentsList/TasksList return bare arrays |
| `TaskCard(TaskCard)` | TaskGet |
| `Messages(Vec<Message>)`, `Message(Message)` | History's bare array; MessageGet's record (missing message is an RPC error) |
| `Events(Vec<Event>)`, `Inbox(InboxResult)` | Events' bare array; Inbox's existing record |
| `Output(OutputResult)`, `Transcript(TranscriptResult)`, `Diff(GitOutcome)`, `Models(ModelsResult)` | AgentOutput, AgentTranscript, GitDiff and ModelsList respectively |
| `Sent(SendResult)` | Both Send and Answer return the existing SendResult |
| `Agent(Option<Agent>)` | Kill/restart/restart-fresh/set-model/pause/resume return an optional Agent; compact returns an Agent and maps to Some |
| `Resumed { agents: Vec<String> }` | Resume `{resumed:[<agent IDs>]}` |

Grounding: `crates/crew/src/daemon/handlers.rs::dispatch`,
`context.rs::compact_agent`, `questions.rs::answer_question`, and
`limits.rs::resume_limited`. No arbitrary `serde_json::Value` fallback is part of
`CommandResult`, and malformed method-specific success payloads are decode errors.

`SessionDriver::handle(event, clock)` returns
`HostUpdate { revision, changes, effects, outcomes }`;
`query`, `plan`, and `confirm` delegate to the core and return corresponding
typed updates/plans. Driver request encoding/parsing lives in Rust. Public view
records contain no frame JSON, tokens, private keys or credential-store handles.

The driver owns bootstrap subscribe/snapshot, request correlation, channel
deadlines and canonical workspace rebinding. It opens action/read/terminal lanes
only after the first authenticated snapshot establishes the canonical scope;
all lanes share that identity and retire together when it changes. The core owns
action/retry policy timers. Driver timer keys include owner, ID and attempt so
those timers cannot collide; one Rust timer executor dispatches wake-ups. The
native driver supplies all `ClockSample` values from one wall/monotonic source;
Swift cannot supply a second monotonic origin. Test drivers inject a clock.
Queue overflow closes the affected channel, marks already-started mutations
outcome-unknown, and requires a fresh snapshot before live state. Terminal
overflow detaches visibly instead of dropping bytes or replaying input.

The adapter's exact pre-live normalization reason is `canonical_scope`. An
active, byte-identical Bind is idempotent; a changed Bind cannot reuse a live
attempt. Before that attempt becomes live, the driver may explicitly Unbind
with this reason and Bind a refined context from the same authenticated channel.
Known PID, start time and membership generation cannot be replaced with
different values; absent facts may be filled in. Mode, endpoint, attempt and
grant identity, tiers and expiry remain fixed. For LocalOperator only, the
initial profile workspace key may become the canonical workspace from that
channel's verified snapshot, with the same single-key replacement in
`Grant.workspace_ids`. ScopedGateway workspace IDs and grants do not use this
exception. The core may retain the old context for validation while unbound;
it does not treat retained facts as a live connection.

A held identified-message retry survives same-workspace provisional
normalization. Only the subsequent complete, verified snapshot permits its
retry policy to decide whether to dispatch. An explicit endpoint/workspace
switch retires the retained old scope immediately, including while no active
connection exists. Unknown proof is not proof of a different daemon; known
contradictory proof is not a refinement. These rules prevent both accidental
retirement during bootstrap and replay into a different workspace.

If an Action connection closes with pending work, the native driver retires
that workspace's lanes, retains the independent endpoint Index, and reconnects
the state lane with a fresh attempt and barrier snapshot. This conservative
strategy supplies verified state for Core's retry decision; the driver does
not copy retry eligibility or replay other mutations. Registered Core policy
timers survive this closed-scope reconnect unless Core emits `CancelTimer`.
Their unique timer IDs reach Core even after the active connection attempt has
changed; Core validates the retained operation and immutable scope. Connection
and IO timers retain attempt guards, and canceled/replaced opaque timer handles
are ignored. Explicit stop/dispose performs Core cleanup and then cancels all
timers. A socket loss must not erase the held Send retry's expiration wake-up.

The local connector is `LocalTransport::open(profile, purpose, observer)`. It
owns protected operator-token loading, refuses an agent identity, performs the
existing exact-version `hello`, then returns frames through a bounded observer
queue. Reuse `crew-core` authorization/path checks; do not replace them with a
plain read of the token file. New native framing must limit a line before
allocating/decoding an unbounded string. The connection closes on malformed,
oversized or uncertain framing. Errors use existing safe diagnostics.

Local native state uses one subscribed socket for snapshot and pushes, with
the existing server barrier; requests/actions and terminal attachments have
separate lanes. This is new native behavior, not a TUI migration. Discovery is
the current user's protected Crew registry or an explicit local socket profile;
there is no port scan or automatic workspace join. A disappeared registry entry
is not permission to bind another daemon to the old selection. Linux runs as
the existing operator user. Initial macOS local distribution is an ordinary
non-App-Sandbox application; sandbox/App Store socket access is not claimed.

`LocalTransport` compiles only for Linux/macOS behind feature `local`. iOS
does not compile or offer local Unix discovery. Gateway transport is a separate
implementation of the same driver ports, released only with
`native-gateway-contract`: TLS server identity, device credential storage,
workspace grants, expiry and individual revocation must be enforced there.
Revocation ends all affected live channels and disables actions. No host-layer
fallback to SSH, raw remote Unix/TCP, plaintext credentials or browser-loopback
exceptions is permitted. Remote-disabled fixture mode is a development aid,
never evidence of a connected iOS application.

### Native bindings and event-loop ownership

Use UniFFI `=0.31.0` for generated Swift bindings, pending the pinned-graph build
gate below. It supports Swift records/enums and Rust objects; its generator does
not itself package Apple libraries. UniFFI's own documentation warns about
incomplete Swift 6 support, particularly asynchronous interfaces. This design
uses synchronous state/driver calls plus explicit host events, not exported Rust
futures. ([UniFFI type model](https://mozilla.github.io/uniffi-rs/latest/types/interfaces.html),
[Swift support](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html))

`crew-client-ffi` exports `CrewClient`, an owned reference to a
`SessionDriver`/`ClientCore`, plus public view/action records. `HostEvent`,
`HostEffect`, wire `Command` and raw frames are **not** exported. The native Rust
driver serializes IO/timer/core events and protects synchronous facade access
with a small mutex; no network IO occurs while the state lock is held. Never
call a foreign observer while holding that lock. Rust errors become a finite
error enum with code and safe message;
decode errors, stale requests, poisoned locks and closed objects never unwind
through FFI. UI code cannot obtain mutable Rust references.

FFI projections use owned strings, booleans, explicit-width integers, options,
enums and sequences. `usize`, borrowed lifetimes, raw pointers, `serde_json::Value`
and arbitrary maps do not cross this interface. Opaque metadata needed for an
evidence view is an optional bounded JSON string, not the app's state format.
`u64` remains `UInt64` in Swift; the TypeScript compatibility layer validates
safe integer boundaries and does not round unsupported IDs. Null measurements
remain optionals throughout the bridge.

The Rust driver executes transport effects; there is no periodic "get changes"
FFI polling. `CrewClient` exports `connect(profile_id)`, `select_workspace(id)`,
`query(query)`, `plan(intent)`, `confirm(id)`, `cancel_confirmation(id)`,
`set_active(active)`, `disconnect()`, `observe(observer)` and
`remove_observer(subscription_id)`. Connect/selection return after scheduling
work, with typed progress/outcomes delivered through the observer; they never
block the UI on IO. Query/planning are bounded synchronous work called off the
UI thread. The opaque `profile_id` resolves through the approved connector;
gateway profile/enrollment/credential callbacks are finalized on its own card,
not treated as arbitrary executable configuration here.

The local factory is `CrewClient.local()`. It composes the real shared
`ClientCore` using native policy limits and OrderedConnection with
`NativeSession::local`. Discovery uses only the connector's protected
`discover_profiles()` path, including its agent-context refusal. The factory
does not dial; the caller explicitly selects an opaque `profile_id` afterward.
`localProfiles()` exposes the construction-time discovery snapshot as
`LocalProfileInfo { profile_id, workspace, project: Option<String> }`. These
records contain no socket, token, PID, start time or OS handle. The IDs belong
to that client instance; a fresh discovery snapshot uses a new client.
Discovery metadata never supplies verified ClientScope facts. Unknown IDs and
calls on a disposed client return typed errors before any connection attempt.

The generated ABI includes the same object, methods and local factory on every
platform. The `bindings` feature enables the platform-neutral transport
`runtime`; `local` adds only the Linux/macOS Unix connector. Where that connector
is absent, `local()` returns
`ClientError::InvalidInput { field: "local", reason: "local_unavailable" }`.
The iOS UI never offers local discovery. Freshness tests generate both with and
without the local feature and require identical committed ABI sources; this
does not replace the deferred Apple compiler/linker checks.

Per `m-712aa438f958`, N4 delivers this real Core/local bridge, typed observers,
listing access and deterministic ABI after its producer source is approved.
The separate `native-gateway-bindings` card owns the gateway factory,
DeviceSigner and enrollment/setup bindings against the reviewed connector.
Apple UI and final live integration require that gateway composition; N4
completion alone is not evidence of a connected iOS application.

`ClientNotice` has `scope: Option<ClientScope>`, `revision: u64`,
`changes: ChangeSet`, `endpoint: EndpointStatus`, `workspace: WorkspaceStatus`,
`reason: Option<ClientReason>`, and `outcomes: Vec<OperationNotice>`.
`OperationNotice` has scope, operation ID, outcome kind (success/rejected/
not-sent/canceled/unknown/sent-unacknowledged/closed-locally), optional attachment
ID (Some for successful Attach, SentUnacknowledged and ClosedLocally; None for
other ordinary outcomes),
and optional safe reason; wire result data stays in
state/resource projections. `ClientObserver::changed(ClientNotice)` delivers
that record. `TerminalChunk` has `scope`, `attachment_id: u64`, `data: Vec<u8>`
bounded to 64 KiB, and `ended: Option<TerminalEnd>`; end values are normal end,
detach, disconnect, revocation, overflow, unsupported, or safe failure reason.
`ClientObserver::terminal(TerminalChunk)` delivers it.
One Rust dispatcher delivers callbacks in FIFO order outside the core lock;
coalesce replaceable invalidations, never silently discard operation outcomes
or terminal data. On sustained terminal backlog, detach with a visible overflow
reason. Unregistering an observer prevents new queued deliveries; already
enqueued native callbacks still check instance/scope identity before publishing.
The facade follows NativeSession's single-observer model: a new `observe`
replaces the previous subscription and returns a fresh ID. Removing an obsolete
ID is a no-op and cannot remove its replacement. The adapter deactivates the old
subscription before replacement/removal, including any remaining terminal
chunks in a callback batch. A callback already executing may finish. Callback
invocation and final foreign-reference release occur outside wrapper and driver
state locks; a callback can dispose its client without waiting for its own
dispatcher thread to join.

Linux marshals notifications into its GTK main context. Apple uses a `CrewHost`
actor to serialize facade calls and a `@MainActor` observable model to publish
projections. UI rendering and OS APIs run on the platform UI thread. Native
callbacks enqueue work only; they do not touch widgets or reenter Rust. Platform
credential stores remain host services. Local operator tokens stay connector-only;
the scoped gateway's device signer returns signatures, never its private key.
No credential enters core/view records. Application shutdown cancels timers, closes
channels, unregisters observers, and drops core references without stopping
the Crew team.

The FFI action decision mirrors `ActionPlan`; `Dispatch` contains only the
operation ID. Internal `Command`/effects are never
exposed for a UI to dispatch again. `ClientError` is a finite recoverable enum:
invalid input with field/reason, decode failure, protocol mismatch with available
versions, too-large input, expired cursor, closed client, or internal failure
code. Confirmation cancellation is exported explicitly. N4 may generate these
real records before the engine is linked, but must not export fake successful
control methods. Full binding acceptance requires the working driver facade
and lifetime/observer tests.

Swift bindings, C header and module map are generated deterministically from
the pinned crate, committed under `apps/apple/CrewClient/Sources/Generated`,
and validated against regeneration. The actual Rust static libraries are build
artifacts. Package iOS device and simulator slices separately in an XCFramework;
never combine their arm64 objects as one fat binary. macOS arm64/x86_64 slices
belong to the macOS library. The checked-in Xcode project links the matching
artifact and shared Swift package. [Rust's iOS target guide](https://doc.rust-lang.org/rustc/platform-support/apple-ios.html)
requires the Apple SDK/toolchain; binding generation on Linux is not an Apple
link test. [UniFFI's Xcode guide](https://mozilla.github.io/uniffi-rs/latest/swift/xcode.html)
describes linking the static library and generated bridging interface.

### UI service seam and native terminal

Each shell defines a small platform `ClientService` interface over the generated
or direct Rust API: observe revision/availability changes, query a view, plan an
intent, confirm an ID, select a workspace, and disconnect. UI test doubles
implement that same interface; they do not invent a separate app data model.
Shell work can proceed with these reviewed records while engine/transport work
continues. Final acceptance replaces the test double with the live service.

Five destinations, evidence and the four confirmations follow
[native-apps.md](native-apps.md). SwiftUI can share Apple views and view models
where native patterns agree; macOS menus/windowing and iOS tabs/sheets remain
platform implementations. GTK uses native list, text, menu and dialog widgets.
Both desktop shells collapse panes without dropping identity/focus. No shell
contains HTML, a browser runtime or a React Native layer.

Terminal is required. The driver owns authenticated attach state and raw bytes;
native renderers own VT emulation, fonts, selection, IME and geometry. Keep
terminal bytes out of the domain history reducer. Each attachment is scoped to
its exact connection attempt, rejects input after end/revocation, clamps resize
dimensions, and discards unsent input on reconnect. Streaming output is never
replaced with screen polling. The existing Herdr screen/prompt path is a
separate capability, with same-host focus available only when local integration
exists. Transcript is not a substitute for an unimplemented terminal.

Renderer candidates are VTE's GTK4 terminal widget and SwiftTerm's native
UIKit/AppKit views. VTE supports a native terminal widget; SwiftTerm provides
separate UIKit/AppKit frontends. Their exact compatible pins and build proof
must be recorded by the feasibility owner before the renderer card starts.
Disable terminal-requested clipboard writes and automatic external-link opening;
copy/open require an explicit user action. Native tests cover UTF-8, escape
sequences, paste, resize, detach and no input replay.
([VTE API](https://gnome.pages.gitlab.gnome.org/vte/gtk4/class.Terminal.html),
[SwiftTerm](https://github.com/migueldeicaza/SwiftTerm))

## Source ownership and independently deliverable cards

The orchestrator accepted the ownership below (`m-719bf0acd1d7`). Owners may
compile UI tests against the reviewed API checkpoint before the engine finishes;
no runtime `todo!()`, mock success response or disconnected demo qualifies as
the final application. Every package `lib.rs` and shared manifest has one owner.

| Slice | Exclusive source ownership | Prerequisite and acceptance |
| --- | --- | --- |
| N1 foundation — `scheduler` | Paths in the approved N1 section; README crate rows and N1 Cargo/lock additions | Approved `native-architecture`; N1 test/evidence gates unchanged |
| N2A API checkpoint — `content` | `crates/crew-protocol/src/rpc.rs`, its exports/reexports in `crew-core/src/rpc.rs`; `crates/crew-client/src/api.rs` and `lib.rs` exports | N1 plus this reviewed contract. Compile complete record/enums and `ClientApi` trait; preserve wire fixtures. This is a real type/schema checkpoint, with no stub engine implementation. |
| N2B reducer — `scheduler` | `crates/crew-client/src/{state,history,questions,engine}.rs`; reducer tests; TUI App adaptation in its own commit | N2A. Snapshot/overlap/scope/dedupe/history/query-state cases plus unedited TUI render tests; direct shared reducer use replaces TUI duplicate domain reduction, preserving modal/selection/transport. |
| N2C actions/policy — `content` | `crates/crew-client/src/{actions,commands,policy}.rs`, their tests | N2A. Closed command encoder, eligibility/confirmation/pending/retry logic; current action vectors and separately tagged new native scope tests. Scheduler integrates module exports. |
| N2D projections — `graph` | `crates/crew-client/src/{presentation,topology,usage}.rs`, projection tests | N2A. Real derived records; preserve task-layer semantics and current-work differences; query/filter/cursor/JSON bridge vectors. Scheduler integrates module exports. |
| N3 native driver/local adapter — `native-transport` | `crates/crew-client-transport/**`, local protocol integration tests | N2A plus planned core effects. Driver correlation/frame/deadline tests; protected Unix auth; single-connection snapshot barrier; real isolated daemon tests. Gateway client adapter waits on `runtime`'s separately reviewed gateway contract. |
| N4 bindings — `native-architect` | `crates/crew-client-ffi/**`, `uniffi.toml`, `src/bin/crew-client-bindgen.rs`; generated ABI sources under `apps/apple/CrewClient/Sources/Generated/{Swift,Headers}` | N2A. Linux binding generation and deterministic stale-output tests; typed ownership/error/scope tests. Apple linking deferred. Coordinate package source list with Apple owner; no shell view edits. |
| N5 Linux app — `graph` | `crates/crew-desktop/**`, desktop entry/icon/package metadata; Linux native views and terminal adapter | N2A and native environment proof. Tests may use `ClientService` fixture implementation while N2/N3 arrive. Final binary uses N2/N3, passes live daemon journeys and native accessibility checks. |
| N6 Apple apps — `ui` | `apps/apple/Crew.xcodeproj/**`, `CrewApp/**`, `CrewUI/**`, `CrewHost/**`, `Tests/**`, `CrewClient/Package.swift`, checked package resolution and artifact-integration script | N2A plus N4 bridge shape. Complete native iOS/macOS source, shared schemes, SwiftPM targets, no placeholder runtime actions. Linux can check generated files/project wiring; Xcode/simulator/device gates explicitly deferred by operator. |
| N7 web policy adoption — `content` | `crates/crew-client/examples/generate_web_policy.rs`, `crates/crew-client/tests/web_policy.rs`, `web/src/generated/client-policy.ts`, importing existing web policy files and focused tests | Baseline web handoff plus N2C. Actual production imports replace duplicate data; Rust stale-output check; unchanged web behavior and all existing byte/performance/security gates. QA alone rebuilds/integrates `web-dist`. |
| N8 validation/integration — `qa` | `scripts/native/**`, `notes/native-validation.md`, `review/native/**`; integrated root Cargo/lock changes after N1, README native build instructions, generated bundle | Live Linux build/test/capture evidence and reproducible Apple runner commands; no passing result for a missing target. No remote workflow execution or signing/publishing in this task. |

`ClientApi` declares query/plan/confirm/apply signatures against the concrete
records; N2A does not supply a fake implementation. N2B's `ClientCore` implements
it. Native UI `ClientService` adapters map to this trait/FFI facade and can use a
test-only fixture implementation. Action and projection modules consume the
shared records; their storage-independent functions are integrated by the core
owner, avoiding concurrent edits to `engine.rs` or package exports.

Ownership update `m-be1e6c4a71c3`: `content` owns the N2A protocol/API checkpoint
and its required exports; `scheduler` continues N1 conformance and owns N2B's
later reducer integration. After the API checkpoint, general client module
exports follow the reducer owner's integration sequence. This changes staffing,
not the approved API semantics; QA still coordinates shared manifest writes.

Package manifests inside a slice are prepared by its owner. After N1, QA merges
all workspace membership, version and `Cargo.lock` changes in order. No owner
quietly commits another card's pending edits. Native crates are workspace
members with default features off; `default-members` lists the existing CLI/TUI
and portable crates, excluding GUI/FFI/transport tools. `crew-desktop` binary has
`required-features = ["native-ui"]`; native-only dependencies are optional.
`default-members` alone does not protect `--workspace`: gate GUI/FFI modules and
tests behind their corresponding features, give the bindgen binary
`required-features = ["bindgen"]`, and run the existing `cargo test --workspace`
and `cargo clippy --workspace --all-targets` successfully with no GTK development
environment and no native features. Run actual native tests separately with
the explicit features; a default-feature test run is not GUI/FFI evidence.
`cargo install --path crates/crew` must still require neither GTK, Node, Swift,
Xcode nor native binding generation. The original Rust 1.82 floor is not raised
for the CLI or portable crates. Native packages explicitly declare Rust 1.93;
their reviewed toolchain is 1.93.1. Older-toolchain compatibility is tested rather
than inferred from a dependency's advertised minimum.

Neither `scripts/build-env` nor an unqualified default Rust toolchain pins
1.93.1. Native scripts must select it explicitly or verify/report that exact
compiler/Cargo version before asserting this baseline. Product commands below
assume that selected environment. Core/CLI minimum-version tests remain separate.

## Dependencies, targets and build contract

The new native package graph may add GTK4 `=0.10.3`, VTE4 `=0.9.0`, UniFFI
`=0.31.0`, and Apple SwiftTerm `1.19.0` as the four named platform roots.
Existing workspace serialization/errors/base64/network utilities are reused
where needed. Any further root dependency needs an explicit purpose and graph
review on its implementation card. There is no libadwaita, chart/component kit,
embedded browser, shipped font or remote asset dependency in this design.
WASM is not a native or web runtime dependency in this phase.

GTK4 `0.10.3` and VTE4 `0.9.0` belong to the same gtk/glib binding generation;
the latest releases are not interchangeable with that pair. The feasibility
probe compiled them using Rust 1.93.1 and Nix GTK 4.22.4/VTE 0.84.1. Transitive
minimums can exceed GTK's declared minimum, so native Rust 1.93 is a deliberate
package policy. SwiftTerm's tagged package uses Swift tools 6.0, Swift 5 language
mode, and UIKit/AppKit frontends. Set the Crew apps to Swift 5 language mode on
a compatible Swift 6 compiler; choose iOS 17 and macOS 14 deployment floors
for Crew's SwiftUI API use, not as claims about library minimums.
([GTK prerequisites](https://gtk-rs.org/gtk4-rs/stable/latest/book/installation.html),
[VTE4 0.9 source](https://docs.rs/crate/vte4/0.9.0/source/Cargo.toml),
[SwiftTerm 1.19 package](https://raw.githubusercontent.com/migueldeicaza/SwiftTerm/v1.19.0/Package.swift))

Linux environment: Nix packages `gtk4`, `vte-gtk4`, `pkg-config`, `gcc`, required
fonts, and a test display/session bus. QA provides `scripts/native/linux-env.sh`
using `nix-shell` or an equivalent installed development environment; a runtime
GTK package alone does not provide development `.pc` files. Use an isolated
Xvfb/session-bus display for repeatable captures; do not drive the operator's
active desktop. Wayland compatibility is an additional real-session check.

The required product commands, after their packages exist, are:

```sh
cargo test --locked -p crew-protocol -p crew-client
cargo check --locked -p crew-client --target wasm32-unknown-unknown
cargo build --locked -p crew-desktop --features native-ui,local
cargo build --locked -p crew-client-ffi --features bindings,local
cargo run --locked -p crew-client-ffi --features bindgen,local \
  --bin crew-client-bindgen -- generate --library <host-ffi-library> \
  --language swift --no-format --out-dir target/native-validation/apple-bindings/generated
```

`bindings` enables the pinned UniFFI facade, `bindgen` additionally enables its
generator binary, and `local` enables Unix IO only on Linux/macOS. The exported
ABI is stable across these targets: the thin local-connector entry point returns
`UnsupportedTransport` on iOS, where Unix discovery/credentials are not compiled
and no local option is offered. This is an explicit platform capability, not an
unfinished implementation. The source owner tests the exact generator CLI when
it exists; the feasibility probe is not a product build.

Run product binding generation from `crates/crew-client-ffi`, using absolute
library and staging output paths: UniFFI's Cargo metadata lookup follows the
working directory. Replaying the independent feasibility probe instead uses
`notes/native-platform-feasibility/probe` as its working directory. This is the
replay clarification to the immutable `fc87ae36` evidence, not a new Apple build
claim.

Use `--no-format` for deterministic product generation and stale-output checks.
UniFFI 0.31's CLI otherwise enables formatting, and the Swift generator searches
for available `xcrun swift-format`, `swift-format`, `swift format`, or
`swiftformat` tools. Their presence can change output between Linux and an
Apple runner. The pinned generator's own output is the canonical committed
source; do not add an unpinned formatter or hand-edit generated files.

QA's scripts write only below `target/native-validation/**`. Binding generation
stages Swift, headers and `CrewClientFFI.xcframework` under
`target/native-validation/apple-bindings/`. The FFI owner reviews/updates
committed generated sources separately. The Apple owner's
`apps/apple/scripts/install-artifacts.sh --framework <path>` places it at
`apps/apple/CrewClient/Artifacts/CrewClientFFI.xcframework` (ignored build output).
The package has a `CrewBindings` Swift wrapper target and a `CrewClientFFI`
binary target with its own headers/module map; do not put Swift and C sources in
one SwiftPM target. Generator configuration, module names, feature set and Rust
library come from the same pinned build. Stale bindings fail validation.
For validation, QA copies the Apple source tree to
`target/native-validation/apple-workspace`, installs staged binary artifacts in
that copy, verifies the generated-source diff, and runs Xcode there. QA scripts
never modify the source `apps/apple` tree. The explicit developer helper remains
owned by the Apple implementer.

| Target | Rust artifact | Required verification |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu`, `crew-desktop` and host FFI library | Build/link/run here, real local daemon read/send/action/terminal workflow, isolated display capture and keyboard/accessibility checks |
| Portable browser feasibility | `wasm32-unknown-unknown`, protocol/client only | Compile gate here; no web integration or browser performance claim |
| macOS Apple silicon | `aarch64-apple-darwin`, FFI static library | `CrewMac` Xcode build, native tests, local transport and UI on Apple runner; deferred |
| macOS Intel | `x86_64-apple-darwin`, FFI static library | Link/test the Mac slice on a supported runner or compatible build destination; do not infer from arm64; deferred |
| iOS device | `aarch64-apple-ios`, FFI static library | `CrewIOS` device build plus later signed device connection/lifecycle tests; deferred |
| iOS simulator | `aarch64-apple-ios-sim`, FFI static library | Simulator build/test on Apple-silicon runner; distinct from device library; deferred |

The source-controlled project is `apps/apple/Crew.xcodeproj`, with shared
schemes `CrewMac` and `CrewIOS`. `scripts/native/apple-build.sh` first records
`xcodebuild -version`, selected developer directory, available SDKs/destinations,
Rust version/targets and lockfile identity, then builds the Rust slices and
matching Xcode target. `apple-test.sh` runs the corresponding test scheme on an
actually listed Mac/simulator destination. `CODE_SIGNING_ALLOWED=NO` may be used
for source/simulator build checks; it is not device-install or distribution
evidence. Signing identities, profiles, App Store/TestFlight submission,
notarization and publication remain outside this implementation authorization.

Rust's Apple target availability does not supply Xcode, SDKs, linker, Simulator
or entitlements. The operator explicitly deferred those builds/tests to their
Apple runner. Deliver complete source/projects/scripts and label each unrun
gate **deferred by operator**. Do not mark Apple source defects as accepted
because testing is deferred, and do not mark Apple compilation as passed based
on Linux binding generation. [Rust target requirements](https://doc.rust-lang.org/rustc/platform-support/apple-ios.html)
and [UniFFI integration](https://mozilla.github.io/uniffi-rs/latest/swift/xcode.html)
are the underlying toolchain references.

## Reuse, web amendment and acceptance evidence

After N2, the TUI, Linux app and Apple bindings consume the same Rust domain,
reducer, action and projection code. Each platform keeps native rendering and
host services. The TUI migration preserves its two-connection transport,
caps/selection/scrolling and status wording; native driver improvements do not
silently alter it. The CLI continues to consume `crew-core` compatibility paths.

Web N7 is a bounded, actual production reuse step. The one generated file is
`web/src/generated/client-policy.ts`; its header names
`crates/crew-client/examples/generate_web_policy.rs`. The source is
`crates/crew-client/src/policy.rs`, whose typed static descriptors are also read by the
Rust client at runtime. Generate only used timeout, command/confirmation,
status/rank and validation policy. First adoption must emit exactly the current
web values and replace existing duplicated definitions/imports. The generator
is not a second handwritten implementation of those policies.

`crates/crew-client/tests/web_policy.rs` compares the committed output byte-for-byte
with current Rust emission, following the existing wire-fixture freshness
pattern. Web builds need no Cargo; Rust installation needs no Node. Existing
web reducers, ordering and work-selection algorithms remain TypeScript ports
tested with differential vectors, including their known completed-work
differences. Do not describe web as running the Rust engine. A later full WASM
replacement requires measured savings, a separate CSP/build review and unchanged
budgets or explicit authorization; none is approved now.

The W12 amendment in `notes/web-design.md` records this specific policy-file
exception. N7 begins only after the existing baseline handoff. Generated wire
fixtures are unchanged, and their source remains Rust. N7's new manifest must
include all shipped bytes and pass P1–P20, with the original three runtime
dependencies and no CSP expansion. With only 85 bytes of P1 headroom, acceptance
depends on replacing bytes; a generated file's existence is not sufficient.
P1–P4 are the manifest assertions in `web/test/budget.test.ts`.
`web/e2e/perf.spec.ts` covers browser startup, push/frame/terminal latency,
bounded rows/heap and idle behavior, plus mock switching/index checks. P16's
server-wire check is in `crates/crew/tests/web.rs`; P17–P20's live workspace
checks are in `web/e2e/live/perf-multi.spec.ts`, with the P20 schedule checker
tested in `web/e2e/live/wakeups.spec.ts`. Passing a byte check does not establish
these runtime measures.

| Gate | Required evidence before that slice is accepted |
| --- | --- |
| API/DTO | Every current wire fixture decodes unchanged; each app command round-trips through `crew_core::Request`; all public records construct and exhaustively match; no unresolved placeholder method. Negative frames/unknown pushes/numeric/null cases covered. |
| Reducer | Snapshot replacement, both overlap modes, overflow/resync, duplicate histories, question answer success/failure/elsewhere, stale attempts, same-ID entities on different hosts, expiry/cursor reset, executing/Waiting and completed-tie vectors. Existing TUI render tests unchanged. |
| Actions | Four confirmations only, immutable scope/target, changed limited-agent set, permission denial, timeout/partial write unknown outcome, same-ID message retry only on closure, no answer/input replay, concurrent slow action and responsive read/terminal lanes. |
| FFI | Deterministic Swift/header output, option/integer/Unicode/JSON conversion corpus, object lifetime/dispose tests, no reentrant callback or panic across ABI, bounded query copies. Linux generation is recorded separately from deferred Apple compilation. |
| Local transport | Real isolated daemon and operator credentials; agent-context refusal, missing/bad token, exact protocol mismatch, frame cap/malformed data, subscription barrier, reconnect/new PID/same PID new attempt, close and pending teardown. Never use the active team's daemon for destructive test actions. |
| Linux app | Live workspace read, named message send, question answer, harmless reversible control, then destructive confirmation/terminal tests against a disposable fake-CLI team. Native window, five destinations, graphs/list/detail, full evidence, keyboard/accessibility and appearance matrix; screenshot plus RPC assertions. |
| Apple source | Complete target/project/package graph, feature/slice/ABI consistency, UI/action/host and transport seams, deterministic fixtures and exact build/test scripts. Record Xcode/simulator/device results as deferred until run. |
| Gateway integration | Separate approved gateway tests for trusted enrollment, selected-workspace visibility, every command/read/terminal permission, per-device revocation and live channel teardown. No remote-ready claim from local/core checks. |
| Web adoption | Baseline closed; deterministic generation, true production imports, all web unit/e2e/bundle freshness and performance suites; measured P1/P2/P3/P4 and unchanged dependency/CSP policy. |

Native performance gates preserve event-driven operation: no polling of state,
no idle animation frames, no subsecond decorative timers, and no full-history
copy on an unrelated entity update. Lifecycle inactive closes/suspends channels
as platform policy requires, discards pending terminal input, retains only
explicitly scoped in-memory evidence, and requires fresh authentication/snapshot
before mutations resume. Background notifications and durable offline history
are outside this release. Profiles/preferences may persist; credentials use the
approved platform store; evidence and drafts are memory-only unless a separate
storage policy is reviewed.

QA records measured cold/warm first-live latency, push-to-visible latency,
long-stream memory stability, queued-operation bounds and idle activity on the
same fixed fixture/local-daemon setup. Use the new app's initial measurements
to set native numeric release budgets in its evidence report before performance
sign-off; do not invent a passing benchmark. Web numerical budgets remain
unchanged. Final app acceptance additionally satisfies N01–N20 in the reviewed
experience contract, with only the operator's explicit Apple execution deferment
and the separately tracked gateway integration gate remaining distinguishable.
