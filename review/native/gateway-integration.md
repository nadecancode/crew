# Scoped gateway and terminal lifecycle integration

This checkpoint composes the reviewed opt-in device gateway, Rust connector and
terminal closure correction on landed `e27cb12d70f49d5252a42b0a1164b057634a5cc0`.
The default CLI still builds with Rust/Cargo 1.82.0. Gateway-enabled checks use
Rust/Cargo 1.93.1; device enrollment and all live tests use isolated fixtures.

Final product source is `26bf2ebeaa5270e2b085fb8697e91134e44d6784`; this report
adds evidence only. The [inventory](gateway-integration.json) records 65 exact
approved source blobs, 583 unchanged protected files, the additive lock union,
fixture binary, commands and retained log hashes. Core, local FFI/generated
Swift, desktop/TUI/Apple shells, web assets and Git safeguards remain unchanged.
The single existing-daemon production exception is reviewed `process/pty.rs`.

## Reviewed composition

| Authority | Import |
| --- | --- |
| Gateway implementation `8651c18d`; route proof `759aea95`; membership generation `589dca0c`/`17d071cb`; listing coherence `19c62f52` | `baeb7b50` imports the final server and portable protocol; `dcb8e63c`/`a633ee1e` retain exact final gateway notes and reports. CLI touches outside new modules are only native command registration and the feature-gated library export. |
| Connector `1eaefd36` | `dcb8e63c`: entire 32-file transport crate exact, including four identified-attachment consumer cases. Private Core/server/workspace/lock ancestry excluded. |
| Optional package union | `e7b1625d`, then serialization-only `65d8fe31`; no package membership or default-member change. |
| PTY lifecycle `f94d0d8b` | `26bf2ebe`: exact production file, real handler regression and [producer evidence](../native-pty-lifecycle.md). Formal approval `m-ef15b7d2a17b`. |

Eight workspace members retain the same five CLI/portable default members.
The lock has 302 records: all 280 baseline versions/checksums plus 22 exact
reviewed gateway additions. Existing edge changes affect only Crew and its
transport's optional gateway dependencies, plus five getrandom/rand_core name
disambiguations. Rustls 0.23.45, tokio-rustls 0.26.6, ed25519-dalek 2.2.0,
rustls-pemfile 2.2.0 and zeroize 1.8.2 match the reviewed server. Earlier CLI
compatibility pins, GTK/FFI additions and serde/windows-sys selections remain.
Root Cargo.toml is byte-equal to the baseline.

The gateway remains default-off. Default CLI and pure transport normal graphs
exclude the optional gateway TLS/crypto dependencies. Actual Cargo 1.82 rejects
`native-gateway` at `base64ct 1.8.3` with `feature edition2024 is required`.
That retained failure establishes the feature's reviewed 1.93 toolchain boundary;
it is not a passing gateway MSRV check. The default 1.82 build remains green.

## Verification

| Scope | Result |
| --- | --- |
| Earlier composed gateway, unchanged by the PTY pick | Native unit 12/12; real TLS/daemon/security 19/19. Default members 1114 passed, zero failed, two explicitly ignored; no sandbox self-skip lines. |
| Connector owner on exact earlier `e7b1625d` archive | Full gateway/real-core 56/56 and original local 31/31, with no product overlay. Owner logs/checkpoints retained separately in the inventory. |
| Final `26bf2ebe` PTY and handler | 16/16 units, actual Kill/PtyClosed/retained late replay 1/1, existing sandboxed PTY cases 2/2. EOF, final queued bytes, two receivers, bounded descendant drain and restart isolation covered. |
| Final native fixture binary | Feature build, version and `native --help` passed; SHA-256 `eaadd984a683ae1d920a4c97800235fc4de21245b4f6a9f6c5aac1615517be45`. |
| Final locked metadata, formatting, Clippy | Eight members/five defaults; formatting clean; all-target native-gateway Clippy with `-D warnings` passed. Earlier default/transport feature lint retained. |
| Final actual Rust/Cargo 1.82.0 | Locked default CLI build; version, top-level/run/attach/web/stop help passed. |
| Unchanged FFI/portable source | Both FFI selections 20/20, including both-library unformatted generated freshness; crew-client/crew-protocol WASM check passed. |
| Final real Core/local/gateway journeys | Native-transport owner: 2/2 actual journeys, 1.62 s, on exact `26bf2ebe` product with only three named `d06db944` test-harness paths. Confirmed Kill now ends the real PTY; scoped Read/Control/Terminal, task/question, two attachments and revocation passed. |

K5 uses the exact QA native-gateway fixture binary above. Its harness adds only
`journey-tests` in the transport manifest, `tests/gateway_daemon.rs` and
`tests/journey/mod.rs`; the Core, server, connector, root manifest and lock remain
exactly the composed product. This test-only overlay is recorded separately from
the unchanged 32-file connector delivery. Owner logs remain in its worktree at
`target/native-validation/k5-26bf-journeys.log` and `k5-26bf-binary.json`;
message `m-8ec1689d3959` records source, binary identity and pass counts.

The two earlier ignored tests require token-spending real CLI access and an
interactive terminal. The first earlier default library run retained two
intermittent failures: `a_lagging_subscriber_resyncs_and_converges` and
`the_runtimes_own_refusals_are_still_refusals`. Both focused controls passed,
then the complete serial default-member run passed 1114/0/2. Those test and
production sources equal the baseline; no source workaround or unproven
host-contention explanation is claimed.

The PTY sender closes after EOF or the unchanged 500 ms drain, without retaining
a strong sender across blocked reads. Existing receivers drain queued bytes,
late attach replays the retained screen through a closed receiver, and dead
resize preserves that replay. Existing supervision, wire messages and authority
checks remain intact. AgentStatus is not used to infer terminal EOF.

All fixture runners clear inherited `GIT_*`, `HERDR_*` and agent context before
launching private daemons/workers. Runtime/state/configuration paths and TLS
credentials belong to disposable fixtures. QA final commands clear `CREW_*`,
then supply only the explicit local MSRV target override. The shared storage
pause was honored; only QA's two reproducible incremental caches were removed.
Source, logs, binaries, captures and recovery artifacts were preserved. Final
checks ran serially after coordinated release with `CARGO_INCREMENTAL=0`, a
100 GiB free-space guard and at least 166 GiB available before each command.
The runtime producer's initial ENOSPC fixture failures and recovered 19/19
result remain in its linked evidence; QA's final runs had no ENOSPC failure.

QA logs remain under
`/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/native-validation/gateway-integration/`.
Full unchanged matrices and browser performance were reused from the accepted
[core integration](core-integration.md); no web source, asset or budget changed.

No root checkout, active operator listener, enrollment, device credential,
team process or installed binary changed. Operator activation still requires
the rebuilt binary/dashboard reload. Availability/gateway bindings, TUI and
connected platform UI remain separate acceptance gates. Apple compilation,
linking, simulator/device and native platform accessibility verification stay
deferred or pending as recorded by their owners.
