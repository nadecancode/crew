# Codex member and native child identity

Card: codex-child-session-id. The implementation is based on reviewed main
394a189e and forward-merged with reviewed client work 201e7fdc.

A native Codex child inherits its Crew member's notify command. The old
adapter treated every agent-turn-complete notification as that member's
Session and TurnEnded, so a child could overwrite the resume target and end
its parent's turn. The native 0.159.0 before-capture reproduces this for two
members. The child's rollout payload.session_id also inherits the parent's
id; its payload.id and notification thread-id identify the child.

The daemon now admits the whole completion hook only for a proven user
thread. Initial confirmation reads at most the first 256 KiB of the exact
thread's rollout within the existing member-file policy. The record must
be session_meta, payload.id must match, and the source must identify a root,
with no subagent source. The current native format declares thread_source
user; the older root-string source format without thread_source is also
accepted. The client label is informational and never supplies proof.
UUIDv7 creation time is only an additional bounded lookup date for sessions
older than the latest launch, not an identity or parenthood check.

A private parent id is persisted with the ordinary session confirmation.
Confirmed native children produce no Session or TurnEnded. Missing or
partial metadata gets six bounded reads with five 400 ms waits. A proven
child never retries. An unprovable mismatched notification cannot invalidate
an already-proven parent. An unbound member whose metadata remains
unavailable gets visible activity, stays unconfirmed, and holds native
queue and transcript/usage reads; its next launch starts fresh. Other hooks
can run during the retry; a replacement launch waits for the hook lease.

A different header-proven user root can rebind the member in the same
launch, preserving native /new and in-TUI resume. The daemon records
agent.session_changed and clears cached rollout, turn, usage and context
state. Parent verification also fences old unsealed resume records: a saved
child or unavailable record cannot become a resume target. Fresh launches
and runtime changes clear the parent pin. Queue execution rechecks the
current parent after obtaining the launch lease.

The committed fixture contains the real parent and child notification
payloads plus the rollout metadata. Its only transformations are the fresh
fixture root placeholder and omission of rollout instruction/tool payloads.
Metadata identity/source fields remain intact. The notify instrumentation
logs the native callback then executes the original authenticated Crew hook
unchanged. The provider is a local synthetic Responses endpoint, inside a
network namespace, with dummy auth and fresh homes. No operator login or
provider traffic is used.

Stand-in runtime fixtures now write root session metadata before their
existing hooks. Existing transcript, counter and workflow assertions are
preserved. The tests also cover child-first, a forged informational client
label, absent/partial/oversized metadata, same-id validation, own Crew child
members, nested native children, old UUIDv7 lookup, legacy root metadata,
delayed header flush, visible permanent uncertainty, root rebind, persisted
resume, and a missing child rollout that cannot revoke its parent.

Native after-capture passes: both actual default children leave their member
parent ids unchanged. Two subsequent bus messages produce native replies in
the parent transcripts; the recorded queue --thread arguments contain only
those parent ids. The committed reproducer repeats this full journey with
the same native binary and immutable QA recipe. Every run reports zero
provider calls and zero remaining owned processes.

The initial complete workspace run passed 1,383 tests, failed five, and
ignored four. The only failures were the Codex empty-turn policy fixture,
which supplied a cached session but omitted the newly required parent
confirmation. That test-only fixture now explicitly represents its known
parent; the entire affected filter passes 10/10 with one ignored count
probe. Its original workflow oracles are unchanged. No production source
changed after the complete workspace run, and no unchanged broad suite was
repeated.

Identity fixtures pass 12/12. Real native gateway 21/21, policy/inventory
15/15, strict workspace and optional native-gateway Clippy, whole-tree fmt,
and the actual Rust 1.82.0 default CLI build/help gate pass. Exact commands,
counts, times, native and log hashes are in codex-child-session-id.json.
The measured default CLI binary is retained at
target/codex-child/pinned/crew-190-default.

Accepted residual: a mismatched new user root whose header remains
unprovable is ignored while a previously proven binding remains intact.
A later proven user-root completion or an explicit fresh launch settles it.

To repeat the native regression from this checkout, build the default CLI,
then run the committed driver with explicit executable paths:

    node scripts/runtime/codex-child-proof.mjs --crew /absolute/path/to/crew --codex /absolute/path/to/codex-0.159.0 --python /absolute/path/to/python3 --out "$PWD/target/new-codex-child-proof"

The driver reads only the immutable recipe files at 378d0b43 through jj,
creates a new synthetic output tree, and runs the provider and all fixture
processes inside a network namespace. Raw synthetic callback payloads and
rollouts remain in that output tree; the portable report omits instruction
content and auth data.
