# Read acknowledgements after full-body delivery

Implementation card: delivery-mark-read. Base: cdbd2303, forward-merged with
reviewed main a9979a4e, then ff9c9d28 before final validation. The separate D1
focused bundle follow-up is recorded in abb1c3d6; no D1 cache-benefit claim
is added here.

Read acknowledgement requires body coverage in addition to D1 consumption.
The daemon persists only an opaque-batch-bound SHA-256 digest, eligibility
and verification outcome with its private receipt. No body is stored in
receipt metadata. A queued bridge reports a digest of the actual native
turn-start segment beginning at that exact batch marker. Before a following
marker, remove exactly one LF inserted by the SDK between adjacent prompts;
otherwise preserve every byte. The rendered batch ends in fixed notice text
without LF, and unclipped bodies retain their trailing whitespace. The
daemon compares exactly one observed digest with the immutable render captured
at offer time. Missing or ambiguous proof does not mark a batch read.

Coverage is all ids in a batch or none. Consecutive clipped and unclipped
messages are offered separately. A clipped render is never read-eligible,
even when its digest matches. Truncation names the actual message id for
an exact agent_inbox lookup. Argument clipping uses render-time byte spans;
its suffix names only ids crossed or omitted by the reserved-suffix cut.

Immediate Claude proof uses the single text block returned as the stored
SessionAppendResult.message, domain-separated by the opaque batch marker.
The handshake advertises this receipt support; old bridges/daemons preserve
legacy consumption without inventing a read proof. No native Codex echo is
verified in this card. Codex, pi, terminal, shell, legacy and unknown routes
therefore remain unread. A process exiting zero is not body coverage.
An authenticated recipient agent's explicit id-scoped full-body fetch marks
that one message read. Operator, outbox and foreign views do not acknowledge
another inbox.

The shared acknowledgement path records parked-card cues before it marks
read, retaining the inbox path's bookkeeping order. A durable consumed
receipt repairs delivery and its proved read acknowledgement after a crash
before the live row update. The unread badge remains acknowledgement-based:
clipped or unproved messages stay unread despite a delivery timestamp.

The forwarded source preserves the reviewed toolbox pause implementation on
ff9c9d28 and native policy inventory fix on 646a0b30. The latter touches only
README and optional native/policy.rs, outside this delivery implementation.

Required workspace run: 1,333 passed, one failed pi launch wording oracle,
four ignored. All 18 delivery cases passed. The pi test's exact final task
body assertion now includes the intentionally appended fixed notice; its
focused rerun passed. Two cloned test-id slices were replaced with from_ref
for Clippy. These changes affect tests only; no unchanged broad suite was
repeated. Strict workspace Clippy/all-targets, whole-tree rustfmt, and the
real Rust 1.82.0 default CLI build/help gate passed. Node tests passed 12/12.
Original failure logs remain recorded rather than relabelled green.

Four isolated native Claude Code 2.1.291 cases completed with zero provider
calls: ordinary queued delivery (three local requests), immediate stored-row
append (two), immediate stored-row rewrite (two), and queued session.append
rewrite (three). Both rewrite cases report the changed stored text's digest,
not the original body's digest. The later queued append hook changes the
actual API row as well, matching turn.start's observed segment. Native
probes use a fixture relay, not a real daemon; the separate daemon/socket
tests exercise digest validation, clipping, read counts and crash repair.
All four generated bridge assets equal the committed asset after fixture
substitution. The merged A+LF+B known-answer fixture comes from the retained
D1 native capture, and both Rust and Node assert its hashes.

The required pipeline paused before starting when storage dropped below
100 GiB. Cleanup removed only authorized disposable caches in this
worktree, preserving executable pins, logs and native captures; validation
resumed above the floor with CARGO_INCREMENTAL=0 and repository overrides
removed from child environments. No operator service, provider credential
or installed binary was changed.

Exact commands, log/native hashes, scope and retained failures are recorded
in delivery-mark-read.json. This card does not claim cache savings.
