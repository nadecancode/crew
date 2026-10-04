# Shared-server review: changes requested

Reviewed `4a5b1b2d` against revision 3 of `notes/web-multi.md`. Changes
requested for the six P2 findings below. The independent baseline workspace
suite passed 879 tests, with two ignored. Each new regression below fails
on the reviewed revision.

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

This includes two older passing reviewer regressions and the three new
failing library cases. The tests and test-only scheduling hook added in
this review do not change production behavior.

## P2: Restart requires a running daemon before replacing the server

`cli::ops::web::web` resolves and connects to the current workspace before
it calls `replace_server`; `--all` also refuses when no daemon runs. Thus
`crew web --restart` cannot replace a dashboard whose joined workspaces
are all stopped, although the command operates on the shared server and
the design requires restoration of stopped memberships. It can similarly
require an unnecessary workspace selection when multiple daemons run.

`reviewer_restart_preserves_a_dashboard_whose_workspaces_are_stopped`
starts and joins a daemon, stops it while its server lingers, and invokes
the documented restart command. It fails with `no crew daemon is running`
before touching the server. A replacement that can read the old server's
listing should restore that listing without requiring a live target.

## P2: Cross-protocol restart produces invalid JSON output

The incompatible-server branch of `replace_server` prints its note to
stdout even when the caller requested `--json`. The successful replacement
then prints the JSON document after that note, breaking clients that parse
the command's output.

`reviewer_another_protocol_server_is_kept_until_explicit_restart` first
verifies that an incompatible server is refused and remains running, then
requests replacement with `--json`. The latter succeeds, but JSON parsing
fails at the note. Keep diagnostics off JSON stdout.

## P2: A missing explicit project removes the current workspace

`state_dir_of` discards errors from `paths::discover`, including errors for
an explicitly supplied `--project`, and falls back to the current
directory. In joined workspace A, `crew web --remove --project
not-a-running-project` therefore succeeds and removes A instead of
reporting that the requested project could not be found.

`reviewer_remove_does_not_fall_back_from_an_explicit_missing_project`
reproduces this against the real server. Preserve explicit target errors;
use the stopped-workspace fallback only when its target is unambiguous.

Run these three process regressions with
`./scripts/build-env cargo test -p crew --test web_server reviewer_`.
The remaining protocol-5 migration gate in the design also needs coverage
through the CLI; the existing stand-in protocol test covers the hub only.

## P2: Concurrent summary refreshes can publish stale counts permanently

`Daemon::refresh_summary` captures the model under its lock, releases that
lock, and later publishes the counts under the watch channel's lock. The
background refresh and a connection's forced refresh can run concurrently.
A later capture may publish first; the older capture then overwrites it
and receives an even higher revision. With the dirty flag already clear,
nothing makes the counts converge until another relevant change occurs.

`reviewer_concurrent_refreshes_do_not_publish_old_counts_with_a_new_revision`
uses a test-only scheduling hook after capture. It holds a capture with
zero working agents, changes the model to one working agent, lets the newer
refresh publish, and releases the older capture. The final summary is
`rev: 3, working: 0`, while the agent remains `Working` and the dirty flag
is false. Serialize capture and publication, including updates to the
question-count cache, or reject captures that became obsolete.

## Review coverage

Reviewed the new process lifecycle, control socket, browser pipe, membership
admission and removal, reconciliation and watch ordering, daemon summary and
web restrictions, CLI migration, sandbox profile assertions, and protocol
contract. Existing process, authentication, origin, stop-order and hub
tests passed in the independent workspace run. The focused new library and
process tests expose gaps not covered by that baseline. No approval is
given until the findings are resolved and their regressions pass.
