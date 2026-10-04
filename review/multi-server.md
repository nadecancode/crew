# Shared-server review: initial findings

Review in progress on `4a5b1b2d`. The independent baseline workspace suite
passed 879 tests, with two ignored. The two new regressions below fail on
that revision; the remaining source review is not yet complete.

## P2: Cancelling a partial daemon read discards the frame prefix

`web::daemons::read_line` clears its output buffer each time it is called.
Both `page::Conn::committed` and `hub::watching` call it from a
`tokio::select!` loop. When another branch becomes ready after a partial
line was read, cancellation leaves the consumed bytes in the buffer. The
next invocation then discards them.

A reply split after `{"id":7,` is subsequently forwarded as only
`"result":{"pong":true}}`. Browser messages or control frames can trigger
this while the daemon is sending a reply or push. The browser drops the
malformed frame, losing the response or update; a summary watch can lose
an update the same way when a `web_event` is sent.

`reviewer_a_daemon_frame_survives_an_interrupted_read` reproduces the
problem with a duplex stream and a cancelled partial read. Keep incomplete
frames across cancellation, clear the buffer only after consuming a whole
frame, and apply the size limit to the accumulated frame.

## P2: Removal does not own or cancel a pending watch dial

`Hub::reconcile_one` awaits `watch_dial` before acquiring a generation
guard, and the dial does not listen for revocation. `Hub::remove` can thus
find the generation drained and answer while this operator connection to
the daemon remains open. After removal answers, the dial can finish and
send `web_attach` and `summary` before noticing that its membership is
gone. The M9 guarantee explicitly includes the watch and pending opens.

`reviewer_removal_drains_a_watch_still_waiting_for_hello` holds the watch's
hello for 500 ms, removes the membership, then lets the hello finish. It
observes both requests after the successful removal reply. The dial must
belong to the generation before it starts, end on revocation, and be
included in draining. Also prevent a watch from committing after the
server closes the hub.

Run the focused checks with:

```
./scripts/build-env cargo test -p crew --lib reviewer_
```

This includes two older passing reviewer regressions and the two new
failing cases. The tests added in this review do not change production
behavior.
