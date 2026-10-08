# Docs backend implementation evidence

This card implements the workspace documentation observer: registered-source
indexing, safe reads, closed Markdown AST, bounded static raster assets,
connection-scoped captures/chunks/cursors, independent metadata watches,
published Git history (including Git-backed jj stores), and line comparisons.
It does not enable an unfinished frontend or advertise a service merely because
types compile. The P2 owner controls central daemon registration and dispatch;
P4 owns the physical read/watch-channel lease and client invalidation contract.

## Source and integration

The implementation lives in `crates/crew/src/docs/`; its README describes the
trusted host ports, limits, history boundary and resync behavior. Parser and
watcher dependencies are pinned. `[docs]` is recognized separately from endpoint
settings. No agent MCP tool, prompt field, model call, delivery hook, planner
mutation or repository snapshot is introduced.

The working branch forward-merged reviewed main `201e7fdc` after checkpoint
`e0144236`; the merge was conflict-free. It includes the shared P2 ports through
`51136032` as dependencies. P2's remaining central lifecycle integration and its
own review are separate from the Docs source review.

The original source pass was `1d542742` (reviewer message
`m-655bb013c56e`). Follow-up `e0144236` batches history traversal/content, retains
history-source invalidations while coalescing, explicitly rejects an absent
`objects/info` mask mountpoint, and documents global cache eviction. A 320-commit,
three-head fixture returns full 50-row pages in 1,555 ms and 1,124 ms under the
five-second operation deadline. These are synthetic fixture measurements, not a
claim about every repository or production latency.

## Checks so far

| Check | Evidence | Result |
| --- | --- | --- |
| Docs policy/source/parser/assets/service | `target/docs-service-tests.log` | 23 passed |
| Confined history, require mode | `target/docs-history.log` | 5 passed; no skip |
| Actual opener code and tests, Apple ARM type check | `target/docs-macos-arm-check.log` | Passed |
| Actual opener code and tests, Apple x86 type check | `target/docs-macos-x86-check.log` | Passed |
| Workspace / strict Clippy / Rust 1.82 | Final merged-base gates | Pending |

Commands use `CARGO_TARGET_DIR=target/docs CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=4 ./scripts/build-env`. The history integration test additionally
sets `CREW_TEST_SANDBOX=require`. `fixtures/docs-openers` compiles the production
safe opener and source fixtures without unrelated daemon C/PTY dependencies;
its own lockfile is checked in. Cross-compilation is not execution.

## Acceptance ownership

K01–K05 cover root/immediate-child and recursive selection, untracked member
files under separately registered `.crew/worktrees`, shared-root deduplication,
configuration replacement, exclusions, symlinks/hard links/FIFOs, replaced roots,
atomic saves, UTF-8 and size failures. K06–K08 cover the closed AST, literal HTML,
inert schemes, static raster limits, immutable chunks and escaped JSON budgets.
K09–K11 cover capture barriers, a real native save burst with a trailing update,
new directories/deletion, a single pending metadata push and reconciliation.
K12/P08 cover operator-only service admission, wrong run/connection, cancellation,
source removal, omitted expiry ids and permits retained by cancelled workers.
K13–K18 cover cache pressure, observer-only operations, confined history,
recorded assets, captured heads, immutable comparisons and indexed link lookup.

Actual macOS opener execution is an explicit later platform gate, accepted by
orchestrator `m-30e26c0ea7e0`; it is not marked passed here. Actual authenticated
web/gateway lane and revocation framing remain P2/P4/integration tests. K19 client
transport/FFI behavior and K20 renderer, focus, CSP/network and frontend budgets
belong to those client/UI/integration cards. This card supplies service fixtures
and typed DTOs; it does not claim a browser or native screen was exercised.
