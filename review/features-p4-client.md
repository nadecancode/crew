# Shared feature client and native bindings

P4 adds the negotiated epics, asides, Docs and usage contracts to the shared Rust
client, transport and Swift bindings. Daemon service readiness remains P2/P3's
responsibility; defining a client record never advertises a daemon capability.

## Implementation

The app command inventory includes 28 new methods (59 total). Epic administration
is excluded. Native policy separates read, action and workspace observer lanes,
while preserving the existing protocol 6 Hello and signed gateway transcript.
Every eligible physical channel negotiates once before Snapshot/Subscribe or any
feature request. Old-peer `invalid`, intermediary `forbidden` and unsupported
feature responses retain the legacy views. Authentication failures close the
connection. The exact authenticated connection epoch accompanies feature replies.

The shared reducer applies epic task/board transactions atomically, ignores
repeated revisions and resynchronizes on gaps. Typed resources include the query,
workspace attempt and, for Docs, a lease for the issuing physical connection.
Replacing a read channel cannot revive its old cursors, snapshots or assets.

Observer registration uses the workspace lane and independent reply/revision
barriers. Watch state is bounded to one aside watch, one Docs watch and four
metrics watches. Removed asides purge retained bodies and pending ask bytes;
stale replies cannot restore them. Gaps hide retained aside text until an
authoritative read resolves the gone cause. Seen-here expiry invalidates retained
Docs views without silently selecting a newer version. Usage replies must preserve
explicit query filters, including run, range, grouping and price book.

Native projections use checked remote UniFFI records and converters, including
bounded strings/lists, exact decimal counters, nullable coverage, safe links and
the closed recursive document AST. Feature data never crosses the FFI as JSON.
Generated Swift is committed without formatting. New Docs/aside screens and Apple
execution remain outside P4.

## Validation checkpoint

Source chain starts at landed P1 fbe243af. Commits 5cae9dfb, 724d9b95 and 6845aea6
cover policy/catalog, per-history-row source references, and negotiated reads.
The current checkpoint adds observers, aside actions/privacy and typed bindings.

Focused driver cases cover legacy discovery, authentication/version rejection,
atomic epic updates, same-connection Docs continuation, late aside removal,
observer gaps and typed admission failures. Converter cases exercise nullable
large counters, scope/lease identity, bounded input, safe links and AST validation.

Final integration and gates are pending at this checkpoint. In particular, merge
current main, run the native-gateway policy suite, strict Clippy, portable/WASM and
Rust 1.82 checks. Logs under `target/features-p4/` are author evidence, not a claim
that services or Apple applications have been exercised end to end.
