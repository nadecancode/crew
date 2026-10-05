# Shared client core

`ClientCore` implements `ClientApi` with real state reduction, semantic views,
action planning, confirmations and operation outcomes. Native hosts instantiate
it with `ClientConfig`; `policy::NATIVE_LIMITS` provides the native bounds.
`apply`, `view`, `plan`, `confirm` and `cancel_confirmation` operate on the same
serialized instance. The core performs no IO and invokes no callbacks.

The host authenticates each connection, supplies `ClientInput` and explicit
`ClockSample` values, and executes returned `ClientEffect` values once. Index
endpoint attempts and workspace binding attempts remain separate. A new
endpoint attempt fences the old workspace stream; a new workspace connection
requires its matching snapshot barrier. Every asynchronous result carries its
complete `ClientScope`. Direct local connections may refine their provisional
profile key before the first snapshot through the reviewed `canonical_scope`
unbind/bind sequence; known identity and authority facts remain fixed.

Views read retained local data without dispatching a request. History pages are
bounded and cursors expire on scope changes, replacement or anchor eviction.
Evidence queries canonicalize options through the same helper as action
planning. Event evidence comes from retained events. Backgrounding preserves
stale reads and the bounded identified-message retry deadline; foreground
mutations require fresh authentication and a snapshot. Explicit stop,
revocation, selection changes and changed authority retire captured operations.

`ClientUpdate.outcomes` contains the core's finalized operation notices. Hosts
forward those notices without synthesizing another notice from each wire reply.
A held identified-message retry emits no final notice until its single replay,
retirement or deadline. Its logical operation ID stays fixed while the host
allocates a fresh wire request ID. Terminal input and resize are one-way writes;
their successful outcome is `SentUnacknowledged`, without a reply timeout.

Validation uses `cargo test --locked -p crew-client` and
`cargo clippy --locked -p crew-client --all-targets -- -D warnings`. Portable
normal dependencies also compile with Rust 1.82 and `wasm32-unknown-unknown`.
The permanent public API conformance test replays the reviewed protocol fixtures
through the actual trait object; it does not reproduce the reducer or projection
algorithms. Fixture paths assume this workspace layout.
