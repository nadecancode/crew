# Integrated web dashboard review: findings

Review target: `c0fc1892ebc855e5883ba5d1cd7edc6627205953`.
The production frontend is the approved `98379fba` tree. Final integration
checks found two remaining inspector lifecycle problems and gaps in the
live security/performance coverage.

## P2: Inspector reads retain another agent's content

`Inspector.tsx` keeps its diff and transcript results in signals; the herdr
screen in `TerminalTab.tsx` does the same. Changing selection through browser
history or the URL updates the agent prop without resetting the tab. These
signals are not associated with the agent that was fetched.

After the page disconnects or stops, select another agent through history.
The heading changes, but the previous agent's diff, transcript or screen
remains beneath it indefinitely. This was reproduced in the real daemon:
create `reviewer-only-dev.txt` only in dev's workspace, inspect dev's diff,
stop the web server, then navigate to `/?agent=echo`. Echo's heading displays
dev's diff. Browser captures are retained locally in
`notes/reviewer-web-final-stale-diff.png`.

There is also a live race: keep A's transcript request pending, history-select
B, then return A's response. The `inFlight` guard skipped B's request and
there is no pending refresh. B's panel displays A's transcript until another
trigger occurs. Four failing cases in
`web/e2e/reviewer-inspector-identity.spec.ts` reproduce these paths.

Associate fetched state and pending work with the selected agent, reject
obsolete results, and ensure a changed selection receives its own read.
Resetting state must not reset the proof gate.

## P2: Inspector opened during reconnect never fetches after recovery

Open a transcript while `hello` is pending during reconnect. Its `read()`
correctly refuses to run before `link.live()`, but the reactive source
contains only agent id, status and refresh tick. Once the verified connection
returns with unchanged agent data, nothing retries the read. The tab remains
at "reading the session…" indefinitely despite the page being live.

The fifth test in `reviewer-inspector-identity.spec.ts` reproduces this.
Fetch eligibility needs to react to the verified connection becoming live,
as the message panel already does. Check the same lifecycle for the other
inspector panes.

## Live coverage follow-up

Sent to QA: run the hostile agent-token request inside the required sandbox;
gate the trusted tab's reconnect until the hostile second tab has stolen and
rewritten storage; and cover P10/P11 on the real daemon or record an explicit
design exception. The original lifecycle test could answer hello with an
empty proof before the theft happened and still pass.

P16 was also missing its required gate. VCS supplied `190316c1` with a p95
limit of 1 ms, measuring push to client receipt/parse as an upper bound on
push to write. Its review and integration remain pending at this report.

## Checks already passed

The independent Rust workspace run passed 822 tests, with two optional tests
ignored; clippy with `-D warnings` passed. The nine live browser tests passed
on jj and on jj with required bubblewrap sandboxes. Normal-load budgets
passed: cold FCP at most 64 ms, live at most 109.8 ms, and p95 push-to-handler,
apply and handler-to-frame of 5 ms, 0.6 ms and 31.9 ms on the first run.

The reviewer also drove a real Chromium page: pan, zoom, node drag persisted
across reload, keyboard graph navigation and focus return, task dependencies
and browser history, message round-trip, terminal echo, pause/resume, log
filters and usage. No later HTTP resource requests or idle RPC polling were
observed. Page errors were empty. The independent interactions discovered
the inspector identity issue above.
