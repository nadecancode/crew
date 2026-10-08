# Docs backend implementation evidence

The card delivers the tested documentation observer service and its explicit
host contract: registered-source indexing, descriptor-safe reads, a closed
Markdown AST, bounded static raster assets, connection-scoped captures/chunks,
metadata watches, published Git history and line comparisons. P2/P6 owns daemon
registration, lifecycle/config/source refresh and capability enablement. P4 owns
physical-channel leases and the separately assigned automatic watch-recovery
follow-up. No unfinished consumer is enabled by this card.

## Source and integration

The tested implementation is `ebcf95d7`, including P2 `c9657e28` and reviewed
main `201e7fdc`. Replayed P2 files were resolved to the author's exact bytes;
no competing central source edits were made. A final conflict-free merge takes
main `4f24f9c6`: notes/probes and shared frontend budget tests only. Product Rust
source and Cargo dependencies remain byte-equal to `ebcf95d7`; the affected Rust
bundle pair was rerun and passed. No RPC method or native-policy inventory entry
is added here, so the separate native-gateway-method-change gate does not apply.

The implementation and trusted host preconditions are documented in
`crates/crew/src/docs/README.md`. Root registration opens only daemon-validated
workspace/member directories; published tips and the Git store are host inputs,
not paths/revsets supplied by a reader. Configuration arrives in global, user,
workspace order without team reload. These ownership boundaries were confirmed
in `m-c0079c86f228`, `m-425d1997213b` and `m-c4c835da952c`.

The service adds no agent MCP tool, prompt content, model request, delivery hook,
planner mutation or repository snapshot. AST parsing never invokes an HTML
renderer. History uses a private fixed bare view, descriptor-pinned read-only
objects, no source config/alternates/helpers/network, and bounded subprocesses.
Recorded images use the captured commit; seen images use captured bytes.

## Review fixes

The preliminary source pass `1d542742` was reviewed in `m-655bb013c56e`.
`e0144236` addressed H1–H4: log/content/reachability batching, merged history
invalidations, an explicit missing `objects/info` refusal, and documented global
LRU eviction. A synthetic 320-commit, three-head fixture returned two full
50-row pages in 1,555 ms and 1,124 ms under the five-second deadline. This is a
fixture measurement, not a production-latency or savings claim. Review
`m-1b9e5f9f893f` accepted these changes as a source pass. The divergence bound
and remaining per-candidate `ls-tree` work are documented.

W1 fixes emit one final new-epoch Policy/Removed/Availability metadata frame
before a policy/source watch ends. Logical document ids stay stable within the
run; captures and cursors still expire. A late failed replacement restores the
old watch unchanged. A successful replacement leaves its old queue quiet while
P2 aborts and awaits the old forwarder, avoiding an EOF-resync race. Scope loss
and physical disconnect remain distinct. Direct service consumers must retire
the old stream after success.

`fixtures/docs-service/terminal-v1.json` comes from the real two-selection policy
fixture, with explicit id/revision normalization and a normal-run drift check.
It freezes old selected refs/generations and `forbidden_path`, `source_gone`,
`disabled` recovery errors. P4 at `201e7fdc` invalidates/fences stale values but
has no automatic re-watch yet; this card proves the server contract and explicit
re-registration, not end-to-end automatic client recovery.

## Validation

| Gate | Evidence | Result |
| --- | --- | --- |
| Workspace, locked, sandbox require mode | `target/docs-workspace-final.log` | 1,438 passed, 0 failed, 4 ignored; 77 result blocks |
| Docs service and hostile inputs | `target/docs-service-tests.log` | 28 passed |
| Confined history integration | `target/docs-history.log`; final workspace `docs_history` target | 5 passed; no skip |
| Strict workspace Clippy, all targets | `target/docs-clippy-final.log` | Passed |
| Rust 1.82 default CLI build/version/help | `target/docs-msrv-final.log` | Passed |
| Production opener code/tests, Apple ARM type check | `target/docs-macos-arm-final.log` | Passed |
| Production opener code/tests, Apple x86 type check | `target/docs-macos-x86-final.log` | Passed |
| Workspace formatting | `target/docs-fmt-final.log` | Passed |
| Bundle pair after final main budget merge | `target/docs-web-bundle-final.log` | 2 passed |

Reproduction uses an **absolute** `CARGO_TARGET_DIR`, `CARGO_INCREMENTAL=0`,
`CARGO_BUILD_JOBS=4`, and test/dev debug symbols disabled to bound disposable
build storage. This changes debug information, not assertions or test semantics.
The require-mode workspace command supplies gcc, pkg-config and socat through
Nix; bubblewrap and jj were already available. `scripts/check-msrv` uses the
separate absolute `CREW_MSRV_TARGET_DIR=.../target/docs-msrv`.

```sh
CREW_TEST_SANDBOX=require CARGO_PROFILE_TEST_DEBUG=0 \
CARGO_TARGET_DIR="$PWD/target/docs" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4 \
nix shell nixpkgs#gcc nixpkgs#pkg-config nixpkgs#socat --command \
  cargo test --locked --workspace --no-fail-fast
```

The initial broad run is retained honestly in `target/docs-workspace.log`:
1,406/11/4. Five unit failures came from a relative target path, five integration
failures from missing socat, and one from the P1 no-backend negotiation oracle
superseded by P2. Corrected environment plus the P2 oracle yielded 1,425/0/4 in
`target/docs-workspace-corrected.log`; the final candidate then yielded the
1,438/0/4 result above. No failure was relabelled as a pass or hidden by a skip.

Pinned local binaries, retained with the cited logs:

- `target/docs/debug/crew`: SHA-256
  `25b4b9fde60f742e0485cef776e7c1ad894aea1411cd62baf675cbf948ee533f`.
- `target/docs-msrv/debug/crew`: SHA-256
  `2e4af044cb1d7712a8ad5686ddcd8d43b14ee3352f92bc9932dfa48cda32a8fe`.

## Acceptance ownership and limits

K01–K05 exercise selected/untracked/shared/member roots, configuration replacement,
exclusions, symlink/hard-link/FIFO/socket/device rejection, root replacement,
atomic saves, byte/parser expansion limits and visible depth/path cutoffs. K06–K08 exercise
the closed AST, literal HTML, inert schemes, raster/chunk limits and escaped
JSON budgets. K09–K11 exercise the initial capture barrier, a real save burst
with a trailing update, new directories/deletion, bounded pending metadata,
terminal frames and replacement rollback/retirement. K12/P08 exercise operator
admission, wrong run/connection, cancellation, source removal, omitted expiry
ids and blocking permits retained after awaiter cancellation. K13–K18 exercise
cache pressure, observer-only operations, confined Git/Git-backed-store history,
recorded assets, frozen heads, source labels, diffs and indexed links.

Actual macOS opener **execution** remains a named later platform gate, approved
by `m-30e26c0ea7e0`; cross-compilation is not execution. Real web/gateway lane,
authentication/revocation framing and host lifecycle composition remain P2/P4/P6
integration checks. K19 client/FFI behavior and K20 renderer/focus/CSP/network and
frontend budgets belong to those client/UI cards. No browser or native Docs
screen was exercised here. History currently requires the Linux sandbox and
SHA-1 published Git objects; unsupported history remains explicitly unavailable
while safe working-copy reads can still work.

## Inherited artifacts kept separate

The branch preserves approved output-store lock fix `847606a4` (review
`m-1dfa695f621b`; independent QA/graph composed runs recorded separately). It also
preserves the approved-as-evidence-only turn-efficiency notes
`95e605cc`/`fb2cf73e`. Those are two legacy windows, not a treated After or a
savings claim; `turn-efficiency-rerun` remains parked for verified deployment.
Neither is presented as Docs implementation. P3 landing remains gated on P2
approval, as recorded by the orchestrator in `m-e8189e606663`.
