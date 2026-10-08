# Turn-boundary Claude delivery

Delivery validation complete. The independently owned bundle trim closes the
recorded main failure below.
This is not a controlled cache-savings claim.

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

Source integration includes reviewed main `2c7a1c3f`, including D4. The lead's
disposition `m-f24de6e70417` permits submission with the known main overage
recorded below. The raised-cap `web-bundle-budget` replacement was withdrawn;
`web-bundle-trim` fits the original ceilings. After its reviewed landing,
the focused `web_bundle` pair passed on exact main `d9d763cb` (2 passed,
0 failed); no unchanged broad suite was repeated. The original failed log
remains evidence of the base overage. The other workspace suites pass,
including all 87 `team`
tests serially, remaining packages, web-server/wire and doc tests. Whole
workspace Clippy with `-D warnings`, rustfmt and the real Rust 1.82 default CLI
build/help checks pass. The default-parallel Codex limit-event assertion raced;
its exact focused rerun and serial suite pass. Two explicit terminal-mode
regressions were fixed by preserving that opted-in legacy route; it is outside
the queued-mod cache guarantee, and their original oracles pass.

The pre-trim main bundle was 604,322 bytes against the Rust P4 check of
600,000. D1 changes no web source or bundle. `web-bundle-trim` owns that
resolved base failure; the original P1 230,500 and P4 600,000 limits stay in
force. No threshold is changed in this card. The retained checks,
native binary fingerprint and fixture report hashes are in
[cache-delivery.json](cache-delivery.json).

Native raw request prefixes are not byte-identical: the SDK moves cache-control
annotations and uses a string instead of a one-text-block array. After only
those representation changes are normalized, old content is identical across
all six baseline native scenarios. The thinking-stream case retains the same
old thinking/signature block in later requests. This proves the narrower
history-integrity claim, not whole-request equality, provider cache reuse or
the C3 paired cost target. The native relay-reconnect probe makes one native
copy per batch despite repeated frames; separate Rust metadata tests cover
daemon receipt restore and consumption repair, and Node tests cover activation
store restore and unknown tombstones.

No installed
operator binary, running team, provider credential or listener was changed.

Native scenario counts: normal, relay reconnect, slow hook (21 seconds), body
rewrite and thinking-stream each make three local fixture requests; drop and
short-circuit each make two. The raw SDK A+B merge makes two and records an
explicit result only for B. Every scenario makes zero provider calls. Fixture
reconnect repeatedly resends the same identities without a second native copy.
The exact stale-attempt regression is
`daemon::delivery::tests::expired_receipt_restores_as_a_tombstone_and_stale_retirement_cannot_bind_a_new_attempt`:
retire A, offer the same message ids as a fresh B, then A's late observation is
ignored and B stays unbound. Restore, phase expiry, stale launch and crash-before-
bus-update checks are separate from the native relay test.

This change does not fix the existing latest-transcript race in `check_turn`.
Codex `custom_tool_call` records already count as tools. That check sleeps
500 ms before reading `last_turn`, so a newer prompt can hide an earlier
turn's tools. D1 leaves Codex delivery and this transcript check unchanged.
Native Claude receipt tests prove recipient binding to actual native turn ids;
they do not prove that the later transcript read is fenced to the completed
turn. This limitation is separate from code-mode tool visibility.
