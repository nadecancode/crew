# Read acknowledgements after full-body delivery

Implementation card: delivery-mark-read. Base: cdbd2303. The separate D1
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

Validation and source checkpoints will be recorded after the native and
daemon regression checks.
