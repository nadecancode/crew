# Shared client logic inventory

Source checkpoint: `6049cdf63e36e303327235834f23a798aa5a82c1` (`6049cdf6`),
protocol 6. Every source reference below is a `path:line` at that checkpoint.
The inventory used `jj file list` and `jj file show -r 6049cdf6`; it does not
assume this agent's older working branch contains the reviewed implementation.
This card changes documentation only. No builds or cross-compilation ran here.

The requested clients are iOS, Linux desktop and macOS desktop, with native
controls and shared logic across those clients, web and TUI. This inventory
proposes extraction boundaries; it selects no UI framework or binding tool.
The architecture card owns those decisions; approved N1 follows `a6f70568`,
while the broader client seam below remains proposed. Apple
verification is deferred by the operator (`m-d5c12d4290c9`), not a blocker to
source work.

## Confirmed dependency graph and reusable code

The workspace has three packages (`Cargo.toml:3`), edition 2021
(`Cargo.toml:7`) and Rust 1.82 minimum (`Cargo.toml:9`). These are their actual
direct dependencies, excluding development dependencies:

```text
crew (Rust library and binary)
  crew-core, crew-tui
  anyhow, thiserror, serde, serde_json, chrono, toml, libc, tokio,
  rusqlite, clap, portable-pty, vt100, async-trait, tracing,
  tracing-subscriber, base64, uuid, ratatui, tokio-tungstenite,
  futures-util, httparse, sha2
crew-tui
  crew-core, anyhow, ratatui, unicode-width, serde_json, serde
crew-core
  anyhow, thiserror, serde, serde_json, serde_yaml, toml, libc, chrono, uuid
web (separate TypeScript/Solid build; no Rust client dependency)
  solid-js, @xterm/xterm, @xterm/addon-fit
```

Sources: `crates/crew/Cargo.toml:9`, `crates/crew/Cargo.toml:17`,
`crates/crew-tui/Cargo.toml:9`, `crates/crew-core/Cargo.toml:9`,
`web/package.json:9`, `web/package.json:13`.

| Existing code | Reuse and boundary, confirmed from source |
| --- | --- |
| `crates/crew-core/src/types.rs:3` | Domain records and string enums use `std`, `serde`, `serde_json`: `Agent`, `Task`, `Message`, `Event`, `Artifact`, `Usage`, `ContextUsage`, `UsageLimit`, `ModelFallback`, `TranscriptEntry`, lifecycle/runtime/workspace enums and constants. Includes message classification, operator-question metadata, terminal-state predicates, context percentage and model naming. No socket, filesystem, clock or entropy calls in this file. |
| `crates/crew-core/src/vcs.rs:15` | `VcsAction`, `VcsOutcome`, `VcsEvent` and payload interpretation use the same data dependencies plus `types::Event`. Actual VCS execution remains elsewhere. |
| `crates/crew-core/src/rpc.rs:14` | `Request { id, call }`, `Call`, `ServerFrame`, `RpcError`, `Push`, `Snapshot`, `SessionInfo`, `Quota`, `TaskCard`, `InboxResult`, `GitOutcome`, `ModelsResult`, `TranscriptResult`, `Workspaces`, `WorkspaceEntry`, `DaemonRef`, `WorkspaceSummary` already serve Rust consumers. Whole module imports `AgentSpec`, `TaskSpec`, `TeamConfig`, so it is not independent of configuration. |
| `crates/crew-core/src/lib.rs:5` | One crate exports client/config/paths/rpc/signals/types/util/vcs unconditionally; its manifest has no feature split (`crates/crew-core/Cargo.toml:1`). Domain-only consumption still compiles the host modules. |
| `crates/crew-tui/src/app.rs:812` | Snapshot/push reducers, questions, history, ordering and queries exist, but `App` also contains `ratatui::layout::Rect`, modal/selection state, transcript wrapping and animation state (`crates/crew-tui/src/app.rs:20`, `crates/crew-tui/src/app.rs:332`). Its deterministic clock does not make the entire module toolkit-independent. |
| `web/src/store/store.ts:10` | Parallel reducers exist in a Solid store, with signals, `reconcile`, rings, notices, context samples and `Anims`. `applySnapshot` and `applyPush` are useful semantic references, not a library that native clients can directly consume. |
| `crates/crew/src/lib.rs:6` | Persistence, bus, task mutation, process/runtime adapters, VCS, sandbox, daemon, MCP and web server are host responsibilities. Even the pure scheduler `plan` depends on task occupancy rules (`crates/crew/src/scheduler/mod.rs:10`); clients observe authoritative results rather than re-running scheduling. |

## Duplication worth removing

These are existing parallel implementations. Extraction should preserve their
intentional differences rather than treating either complete UI as the core.
Later-client boundaries in this table are proposals; approved N1 and the
narrow initial web-adoption step are specified below.

| Shared behavior | Rust source | Web source | Extraction boundary |
| --- | --- | --- | --- |
| Wire shape, defaults, enums, message/question helpers | `crates/crew-core/src/types.rs:26`, `crates/crew-core/src/rpc.rs:604` | `web/src/protocol.ts:1`, `web/src/protocol.ts:305` | N1 extracts Rust DTO/VCS definitions. Existing TypeScript projection, validation and unknown-push handling remain; generating wire bindings is a later proposal beyond the approved web step. |
| Natural agent ordering, subagent parents/orphans | `crates/crew-tui/src/app.rs:397`, `crates/crew-tui/src/app.rs:889` | `web/src/store/order.ts:10` | Pure ordering policy, including u64 suffix handling and orchestrator placement. |
| Executing work before parked work | `crates/crew-tui/src/app.rs:520` | `web/src/format.ts:97` | Return chosen task identity, completion flag and optional duration. Keep every Waiting card available independently. Preserve active board-order ties, unknown versus zero start, started Blocked work and original resume timestamps. |
| Atomic replacement, entity upserts, questions, duplicate/history handling | `crates/crew-tui/src/app.rs:812`, `crates/crew-tui/src/app.rs:965` | `web/src/store/store.ts:114`, `web/src/store/store.ts:162` | Domain reducer with changed IDs and explicit notice/effect outputs; selection/focus and reactive subscriptions stay in hosts. |
| Task layers | `crates/crew-tui/src/app.rs:1292` | `web/src/graph/layers.ts:18` | Dependency layers with existing unknown-dependency/cycle behavior; pixel/cell geometry and crossing reduction stay separate. |
| VCS/message animation state | `crates/crew-tui/src/anim.rs:6` | `web/src/anim/anims.ts:9` | Time-stamped semantic links/marks and expiry policy can share; rendering, reduced motion and frame scheduling remain platform work. |
| Slash commands, aliases, hints | `crates/crew-core/src/util.rs:166`, `crates/crew-tui/src/slash.rs:45` | `web/src/commands.ts:9`, `web/src/commands.ts:36` | Shared parser/catalog and semantic intents. `/exit`, `/mouse`, graph layout and single-key settings have client-specific outcomes. |
| RPC timeout classes | `crates/crew-tui/src/net.rs:45` | `web/src/net/conn.ts:57` | Shared method policy: ordinary 3 s, lifecycle 30 s, resume/shutdown 90 s, models 20 s; adapter schedules deadlines. |
| Model names, durations, whitespace | `crates/crew-core/src/types.rs:349`, `crates/crew-core/src/util.rs:48`, `crates/crew-core/src/util.rs:141` | `web/src/format.ts:29`, `web/src/format.ts:155` | Pure identifiers/numeric duration facts and agreed canonical strings. Verify Unicode behavior before replacing mirrored helpers. |
| Quota grouping, paused reasons, staleness | `crates/crew-tui/src/views/usage.rs:32`, `crates/crew-tui/src/views/usage.rs:87` | `web/src/views/usage.ts:9`, `web/src/views/usage.ts:21` | Values, coverage, runtime groups and semantic severity can share. Locale dates, glyphs, gauge geometry and number formatting remain platform-owned. |

History limits already differ: TUI 500 messages/2,000 events
(`crates/crew-tui/src/app.rs:27`), web 1,000/5,000 and 600 context samples
(`web/src/store/store.ts:32`). A proposed core must accept history policy rather
than silently changing either client's bounds.

Current-work fallback has a confirmed difference: TUI's `max_by_key` takes the
last Done card with an equal optional completion time
(`crates/crew-tui/src/app.rs:551`); web's strict `>` takes the first and compares
missing completion as zero (`web/src/format.ts:103`). Approved N1 preserves
the TUI behavior exactly: optional completion ordering, last equal entry,
no policy parameter. Vectors retain the legacy web difference; convergence
or a cross-client policy needs a separate reviewed consumer. Active-card
precedence agrees; do not claim whole-query parity.

The web types are a checked projection, not an exact Rust binding. For example,
Rust `RpcError.data` exists (`crates/crew-core/src/rpc.rs:612`) while web keeps
code/message (`web/src/protocol.ts:197`); Rust reports `binary_warning`
(`crates/crew-core/src/rpc.rs:834`) but the web `SessionInfo` projection omits it
(`web/src/protocol.ts:148`). Web decoders validate the fields they use and fill
defaults in place (`web/src/protocol.ts:258`). Generating records alone cannot
replace that behavior or count as sharing reducer/action implementations.

## Transport and proposed client seams

Existing v6 transport is a local Unix daemon socket or a loopback-only dashboard WebSocket with pairing, server proof and joined-workspace scope; native remote access is assigned to a scoped Crew gateway whose design remains a separate approval gate.

Current source anchors: blocking `Client::operator(socket, client)` and token
loading (`crates/crew-core/src/client.rs:258`); loopback binding
(`crates/crew/src/web/mod.rs:656`); exact Host/Origin checks
(`crates/crew/src/web/http.rs:143`); pairing/session proof
(`web/src/net/session.ts:72`, `web/src/net/session.ts:113`); separate index
listing (`web/src/net/hub.ts:32`) and workspace attempt
(`web/src/net/workspace.ts:59`). Transport/auth details belong to runtime's
inventory; extracting client code grants no new reachability or authority.

Proposal, agreed ownership sentence with runtime:

Shared client core owns protocol 6 Request/ServerFrame handling and connection state; platform adapters own discovery, credential storage, authenticated byte/message channels, clocks and lifecycle signals.

The architecture's proposed `ClientScope`, `ClockSample`, and
`ClientCore::apply/view/plan` are the public boundary, not existing APIs. Map
them to existing behavior as follows:

| Proposed seam | Existing source and required behavior |
| --- | --- |
| `ClientScope` on every asynchronous input, query result and action | Workspace ID plus generation and `DaemonRef` come from `Workspaces` (`crates/crew-core/src/rpc.rs:938`); web's attempt guard prevents old completions from applying (`web/src/net/workspace.ts:169`). Add endpoint and attempt identity explicitly; entity IDs alone cannot identify another host/workspace. |
| `apply(input, ClockSample)` | Keep listing revision, connection state and domain reduction separate. Web drops pushes during subscribe/snapshot, replaces state, then applies later pushes (`web/src/net/workspace.ts:194`). TUI subscribes before a separate snapshot read and deduplicates overlaps (`crates/crew-tui/src/net.rs:84`). Preserve each transport's ordering guarantee; v6 has no global replay cursor across all push kinds. |
| `view(query, ClockSample)` | Bounded agent/task/message/event/usage pages; selected current task; open questions; nullable measurements; evidence origin/read time and scope. Existing result records include `TaskCard`, `InboxResult` and `TranscriptResult` (`crates/crew-core/src/rpc.rs:1545`, `crates/crew-core/src/rpc.rs:1625`). Freshness/coverage are semantic data; platform decides how to present them. |
| `plan(intent, ClockSample)` | Capture immutable scope/target; return unavailable reason, confirmation or typed command effect. Keep pending/result identity in core, OS dispatch in adapter. Recheck scope/capability before dispatch; eligibility is not server authorization. Existing client error docs warn that a failed read may follow execution (`crates/crew-core/src/client.rs:47`); the proposed planner must not automatically replay such mutations. Existing web retries only its explicitly identified message after a closed connection (`web/src/app.ts:289`); `SendParams.id` is the server idempotency seam (`crates/crew-core/src/rpc.rs:79`). |
| Adapter event/effect loop | Separate ordinary requests, slow actions and terminal keys: TUI has independent workers (`crates/crew-tui/src/net.rs:139`). Existing web `Env` injects socket/timers/clock/random (`web/src/net/conn.ts:86`). Host executes effects and returns scoped results; no UI or transport callbacks from the pure reducer. |

Routing/selection identities can share; native navigation, accessibility,
clipboard, focus, row measurement, terminal emulation and preferences tied to
pixel/cell layouts stay adapters. Existing browser routing and preference
storage are UI-specific (`web/src/route.ts:8`, `web/src/prefs.ts:14`); existing
terminal wrapping and controls are TUI-specific (`crates/crew-tui/src/app.rs:251`).

## Portability, bindings and reuse by target

Confirmed blockers in the existing dependency graph:

- `client.rs` imports `std::os::unix::net::UnixStream` unconditionally
  (`crates/crew-core/src/client.rs:9`). `paths.rs` imports Unix filesystem/fd
  APIs and uses libc descriptor-relative reads (`crates/crew-core/src/paths.rs:6`,
  `crates/crew-core/src/paths.rs:308`); `signals.rs` installs process handlers
  (`crates/crew-core/src/signals.rs:20`). Linux/non-Linux cfg branches exist,
  but they do not remove unconditional Unix dependencies or create a WASM core.
- RPC pulls in configuration; `SandboxConfig::validate` uses Unix `OsStrExt`
  (`crates/crew-core/src/config.rs:222`), and `TeamConfig::load` reads files
  (`crates/crew-core/src/config.rs:637`). Separate declarative config records
  from host path validation/loading before moving all of `Call` to a pure crate.
  These are inherent methods: moving their type into another crate requires
  host free functions/extension traits, wrappers with conversions, or explicitly
  gated methods inside the defining crate. Reexports alone cannot preserve the
  original host impls.
- `util.rs` mixes pure helpers with `SystemTime`, `chrono::Local` and UUID v4
  entropy (`crates/crew-core/src/util.rs:4`, `crates/crew-core/src/util.rs:77`;
  enabled features at `Cargo.toml:19`). Portable code should receive time,
  timezone-sensitive formatting and generated IDs from adapters.
- The manifests declare no Apple/WASM binding package or `staticlib`/`cdylib`
  client artifact (`Cargo.toml:3`, `crates/crew-core/Cargo.toml:1`,
  `crates/crew/Cargo.toml:9`). Existing DTOs expose Rust collections,
  `serde_json::Value`, tagged enums and `Option`; that is serialization, not an
  established native ABI (`crates/crew-core/src/types.rs:232`,
  `crates/crew-core/src/rpc.rs:672`). Binding design must define ownership,
  errors, nullability, integer width and bounded copies. `Usage`/request IDs
  use u64 while web uses numbers (`crates/crew-core/src/types.rs:156`,
  `crates/crew-core/src/rpc.rs:604`, `web/src/protocol.ts:38`).

| Target | Proposed actual reuse | Boundary and verification |
| --- | --- | --- |
| Linux desktop | Portable protocol/domain crate and client reducer/action/presentation library consumed directly in Rust; same library consumed by TUI. Existing Unix operator client is a host adapter candidate. | GUI rendering, event loop and credentials remain platform code. Current Linux native app/shared extraction do not exist at this checkpoint; build and connected workflow need local command evidence after implementation. |
| macOS desktop | Same portable Rust logic through a separately defined binding surface, with native records/controls. | Local socket or gateway adapter is a candidate, not a verified macOS daemon/security/package claim. Apple verification deferred by operator; future portable checks below do not verify signing, sandbox or GUI behavior. |
| iOS | Same portable Rust logic and semantic views/actions through bindings. | Host-access transport, device credentials and lifecycle are adapters; desktop socket/path/process helpers cannot supply phone connectivity. Apple verification deferred by operator; no simulator, device or entitlement result claimed. |
| Web | Approved first step: one committed generated policy data file actively imported from the same typed descriptors used by Rust. TypeScript `workOf`, ordering and reducer remain ports checked against Rust vectors. | W12 exception `m-331755b19fb4` requires a separate web card, named file/generator, Rust stale-output check, production imports and original budgets. Generated order/reducer implementations or a full WASM core remain unapproved later proposals. |
| TUI | Consume extracted Rust data and client library directly. | Keep ratatui, crossterm, terminal input/output, view-local scroll/wrapping and signal cleanup outside. Migrate reducers incrementally with behavior comparisons. |

Exact protocol matching is enforced (`crates/crew-core/src/rpc.rs:20`,
`crates/crew-core/src/client.rs:93`, `web/src/protocol.ts:6`). Consequence for
independently released apps: an incompatible host protocol needs a compatible
client release or an explicitly approved compatibility layer; present code
does not negotiate versions. Additive defaulted fields can keep v6; extraction
must not bump it.

## Approved first extraction

First protocol step: move only `crates/crew-core/src/types.rs` and
`crates/crew-core/src/vcs.rs`, unchanged, to the approved `crew-protocol` crate
with `serde`/`serde_json`. Preserve `crew_core::types`, `crew_core::vcs` and
root domain imports through reexports. Move their unit tests with them. Leave
RPC, config, client, paths, signals and mixed utilities in place. This closes
the new crate's dependency graph without the config/inherent-method migration,
and gives every later native/shared client one domain definition immediately.
It does not yet share the full request protocol or client reducer. The
architecture's approved N1 also adds `crew-client` ordering/current-work
functions and migrates TUI callers: `natural_cmp`, `is_orchestrator`,
`satellite_parent_id`, `sort_agents`, and
`current_work(tasks, agent_id, server_now_ms)`. Its
`CurrentWork { task_id, done, elapsed_ms }` result avoids a toolkit or borrowed
view type. Preserve TUI's completed fallback exactly: `Option<completed_at>`
ordering with `None` below `Some`, last entry on equality. There is no
completed-policy parameter or web/TUI convergence in N1. Separate legacy web
expectations remain visible in conformance vectors.

Before landing that source slice, run the new crate tests, the existing
`crew-core` compatibility tests (`crates/crew-core/src/rpc.rs:1030`) and
`./scripts/build-env cargo test -p crew --test web_wire` with fixture updates
disabled. Run TUI tests to check preserved type identity/reexports. Existing
wire fixtures are generated from real Rust values
(`crates/crew/tests/web_wire.rs:30`, `crates/crew/tests/web_wire.rs:61`) and read
by web decoders (`web/test/protocol.test.ts:1`); they must remain unchanged.
Broader workspace checks belong to the implementation card, coordinated with QA.

Then extract client behaviors behind the reviewed architecture contract.
Use graph's `native-state-fixtures` and content's `native-action-fixtures`
corpora rather than authoring a second set here. Existing useful provenance:
`web/test/work.test.ts:13`, `web/test/store.test.ts:1`,
`web/test/order.test.ts:1`, `web/test/commands.test.ts:1`, and the generated
`anim.json` cases (`crates/crew/tests/fixtures/web/README.md:24`). Compare
snapshot replacement, event/message duplicates, answer reconciliation,
removed selection, scope changes, unknown/zero timestamps and executing-work
selection. Client-state extraction must retain configurable bounds.

Approved N1 portable build gates: Linux host
`cargo check -p crew-protocol` and `cargo check -p crew-client`; WASM
`cargo check --target wasm32-unknown-unknown -p crew-protocol -p crew-client`;
`cargo tree -p crew-protocol -e normal` and `cargo tree -p crew-client -e normal`
must show only the internal protocol edge and `serde`/`serde_json` closure.
None ran on this inventory card. Host observation: `rustup target list --installed`
on 2026-10-05 includes `wasm32-unknown-unknown`, `wasm32-wasip2` and
`x86_64-unknown-linux-gnu`; the required WASM target is already installed.
Apple verification deferred by operator:
on the Apple runner, install the appropriate Rust targets with
`rustup target add aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim`,
then run `cargo check -p crew-client --target <target>` for each. Expected
result: portable crate checks without Unix/GUI/ambient-clock dependencies.
These checks do not establish a linked app or replace the platform contract's
Xcode, simulator, signing and entitlement gates.

No web source moves in the first slice. The source fingerprint stays
`fnv1a64:f22f3cf5ad74d25f`; approved P1/P2/P3/P4 ceilings remain
230,500/34,000/350,000/600,000 bytes (`review/runtime-delivery.md:53`). Any later
generated web or WASM integration must rebuild the embedded bundle and pass
those same gates. Existing packaging embeds `web-dist` in Rust
(`crates/crew/build.rs:1`), while web build stamping fingerprints its inputs
(`web/scripts/stamp.ts:1`); adding a sharing mechanism cannot bypass either.
