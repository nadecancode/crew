# Intermittent gateway route revocation test

Card: `native-gateway-route-flake`. Test:
`crates/crew/tests/native_gateway.rs`,
`enabled::grant_revocation_cancels_route_verification_without_an_opened_reply`.
It failed once directly after a full build and passed on reruns.

## Race

The test pauses the workspace daemon with `SIGSTOP`, sends a Terminal route
proof so the gateway starts verifying the route, then revokes the device's
workspace grant and expects the channel to end without any reply.

Two gateway paths compete:

- the revocation, which commits the registry durably (file and directory
  `sync_all`) and then cancels the device's connections for that workspace;
- the route verification's dial deadline: `say_hello` waits at most
  `DIAL_WITHIN` (2 s) for the paused daemon's hello, after which the gateway
  refuses the proof with `{"error":{"code":"unavailable"}}`.

The test started a `crew native revoke` process for the revocation. Process
start, control-socket authentication and the durable save all had to finish
inside those 2 s. A slow process start or a stalled save, as when a full build
has just left a large amount of dirty data to write back, lets the deadline
answer first. The gateway is then correct to refuse, but the test treated any
reply as a failure.

The gateway behaves as `notes/native-gateway.md` requires: a held or
unresponsive daemon is refused after a bounded deadline, a revocation cancels
in-flight verification, and no route is ever opened for a revoked grant. No
product source changes.

## Fix (test only)

- The revocation goes over a native control session that the test
  authenticates before pausing the daemon, so the timed window contains one
  request instead of a process start.
- The test records an instant before the route proof is sent. The gateway's
  dial, and so its earliest deadline refusal, starts after it. If the
  revocation's reply arrives less than 2 s after that instant, the revocation
  certainly preceded the deadline and the channel must end with no reply, as
  before. Only when the revocation itself took 2 s or more may the gateway
  have answered; that answer must be the `unavailable` refusal for request 1,
  never a result. The rest of the test (empty listing, refused new proof,
  unchanged daemon) is unchanged.
- The previous assertion that the revoke command returned within 2 s is
  removed. It measured process start and durable save time, not
  cancellation: with a paused daemon, a gateway that failed to cancel would
  still return within about 1 s, when its drain timeout aborts the channel.

## Evidence

Log: `target/native-validation/route-flake.log`. Binaries pinned to cores 0-3
for contention runs, as directed.

- Forced ordering on main `b408ff7a` (revocation delayed 2.1 s past the dial
  deadline): the main test fails 3 of 3 runs with
  `Text({"id":1,"error":{"code":"unavailable",...}})`, the reported failure.
- The fixed test under the same forced ordering passes 3 of 3, accepting only
  the refusal and logging that the deadline answered first.
- Mutation: a gateway whose `send` ignores cancellation answers after a
  revocation that landed first. The fixed test fails 2 of 2 with
  `reply after a revocation that landed first (198 ms)`.
- CPU contention (12 busy loops on cores 0-3, test on the same cores, 105 s
  each): main test 0 of 85 runs failed; fixed test 0 of 80 failed, and none
  needed the late-revocation path. CPU load alone does not reproduce the
  race; slow revocation does.
- On main `fc26357d`: the `native_gateway` suite with `native-gateway` passed
  20 of 20 three times, and without the feature 1 of 1. Strict Clippy for
  `crew` all targets with and without `native-gateway` and
  `cargo fmt --all --check` pass. No `crates/crew` source changed, so the
  Rust 1.82 CLI gate is unaffected.
