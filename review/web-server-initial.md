# Initial web server review

Reviewed `adec5ecb`, `d4c9a553`, `c34fdb24`, and `ea4eb92e` against
`notes/web-design.md` (including the shutdown-order amendment). Changes are
not yet approved: four regressions need fixes.

The existing targeted checks passed independently: 16 web unit tests, three
Session tests (including the adversarial snapshot barrier and lag recovery),
19 real-daemon web tests, two bundle tests, and eight wire fixture tests.

## P1: A stopped server can still deliver new pushes

`web/ws.rs::run` spawns a writer but does not cancel it if the parent task is
aborted or the timeout awaiting the writer expires. `Session` likewise has
no cancellation cleanup: aborting the connection skips `Session::close` and
leaves its subscription task alive.

When a client stops reading, its frame queue fills. A subsequent RPC then
waits in `Session::request` for room to enqueue its reply. `Web::stop` aborts
the connection after one second; dropping `Admitted` clears its registration,
so stop releases the listeners and returns, although the writer and
subscription still exist. Resuming reads on that socket delivers a push
emitted **after** stop returned.

The regression uses a small TCP send buffer, a subscribed connection, 2,200
pushes with 16 KiB payloads, and a ping sent without reading. It then stops
the server, emits a marker, and observes the marker on the old socket.

Fix ownership and cleanup of all connection tasks, and wait for actual
cleanup before releasing listeners. A responsive client alone does not test
this path.

## P2: A failed hello reply permanently consumes authentication capacity

`Registry::hello` increments `authenticated` and the session's `open` count
before queuing the reply. In `web/ws.rs`, a false return from
`Session::request` breaks the loop before `Admitted::authenticated` records
that reservation. Aborting while the reply is blocked has the same problem.
The admission guard then releases only the unauthenticated count.

After enough failures, all 32 authenticated slots are occupied without live
connections. Closing and reopening the web server does not reset this count.
The regression drops a web Session's reply receiver before a valid hello,
then follows the normal false-return cleanup path and observes one phantom
authenticated slot.

Make the reservation belong to the connection before any fallible or
asynchronous reply operation, and release it on cancellation too.

## P2: Backpressure defeats the authentication deadline

The five-second hello deadline is checked while reading a message, but not
while awaiting `Session::request`. An unauthenticated client can send valid
size requests without reading the errors; when the queue fills, the deadline
is no longer checked. The regression floods unknown methods with a bounded
send loop and observes that the unauthenticated connection still occupies
its slot after another eight seconds. Repeating this can occupy all 32 slots.

Enforce connection deadlines through blocked reply handling as well as
reads. Preserve legitimate long authenticated RPCs when changing heartbeat
and cancellation handling.

## P2: Port zero reports a successful but unreachable server

`Web::start` accepts zero, binds the two loopback addresses separately with
port zero, and returns `Serving.port == 0`. The kernel assigns different
ephemeral ports to those listeners, and the returned address is unusable.
The CLI also accepts `--port 0`.

Reject zero at the RPC boundary, or select one actual ephemeral port and use
it for both addresses and the returned URL. Cover the CLI and direct RPC.

## Reproductions

`crates/crew/src/web/review_regressions.rs` contains the four regressions;
`web/tests.rs` includes that module. They deliberately fail on `ea4eb92e`.

```sh
./scripts/build-env cargo test -p crew --lib review_regressions -- --nocapture
```
