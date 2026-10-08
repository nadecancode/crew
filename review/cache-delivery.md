# Turn-boundary Claude delivery

Validation in progress. This checkpoint is not a shipping or cache-savings claim.

Ordinary Claude traffic stays in the durable bus until a native turn carrying
its daemon-issued batch marker is observed. Operator traffic is classified by
the authenticated bus sender, split from ordinary traffic, and can steer an
active turn. The native bridge never derives this policy from message text.

Queue admission, native observation and consumption have separate timestamps.
Admission does not mark a row delivered. Private versioned receipt metadata
contains batch/message/session/launch identities and times, never bodies.
Unknown admissions retain tombstones; a timer cannot authorize a second copy.
Consumption is persisted before updating the bus, allowing recovery of a crash
between those operations. Native turn identity, not the daemon's active flag,
binds recipients. Optional `immediate` and `batch_id` fields preserve v6 legacy
serialization when absent; behavioral guarantees require an upgraded pair.

The deployed Claude Code 2.1.291 differs from an assumption suggested by the
2.1.290 generated SDK comments. Two submissions during a foreground turn can
merge into one next turn; only the last Promise resolves. Therefore each exact
marker line in the actual native `turn.start` is consumption evidence, and
`accepted_at` stays null without an explicit result. The failed assumption and
the synthetic endpoint traces are retained under `target/cache-delivery-sdk`.

The native probes run against an explicit 127.0.0.1 Anthropic-compatible stub
with a dummy key and isolated config/worktree. They make no provider calls.
Production bridge probes cover normal queueing, relay reconnect, a hook waiting
21 seconds, body rewriting, dropping and short-circuiting. The minimal SDK
probe retains the merged A+B case. Node regressions execute the actual bridge
asset against a fake host; Rust regressions use the real durable bus and
receipt store. Native endpoint captures are fixture data and are not actual
provider usage or a controlled C3 cost series.

Source integration includes reviewed main `2c7a1c3f`, including D4. Remaining
gates: final workspace tests, strict Clippy/rustfmt, Rust 1.82 default CLI,
complete failure/receipt resync coverage, and reviewer verdict. No installed
operator binary, running team, provider credential or listener was changed.
