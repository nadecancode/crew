# Web dashboard follow-up review

Reviewed `9b4c2735` (`c09b77f1`, `7e7a0deb`, and the already approved server
merge). The six original findings are closed: all seven original reviewer
regressions pass. The central proof gate correctly blocks application RPCs
before verification; recovery, terminal lifetime, list anchoring, prompt
retention and the overall connection deadline match the requested fixes.

One remaining P2 needs a fix before approval: rejected Solid resources are
read before their errors are checked.

## P2: Failed resources interrupt rendering and do not recover

In `MessagePanel.tsx`, the `Show` condition reads `fetched()` through
`message()` before the fallback checks `fetched.error`. Solid throws when a
rejected resource's value is read. The intended error UI is therefore never
reached. Two browser reproductions:

- Open an uncached message during reconnect, before hello is answered. The
  new proof gate correctly rejects the call locally, but the resource throws
  an uncaught `not connected` error and the panel remains at `loading…`.
  After verified reconnect, the source has the same message id and does not
  request it again.
- Have `message_get` return `not_found`. The same uncaught exception occurs
  instead of the intended error text.

`ModelDialog` has the same problem: its `rows` memo reads `models()` before
the view checks `models.error`. A rejected `models_list` raises an uncaught
exception rather than rendering its error cleanly.

Check error state before reading resource values (or use an appropriate
error boundary). The message resource must wait for a live connection and
retry when a verified connection returns.

Three failing browser regressions are in
`web/e2e/reviewer-resources.spec.ts`:

```sh
cd web
PLAYWRIGHT_EXECUTABLE_PATH=/path/to/chromium \
  bunx playwright test e2e/reviewer-resources.spec.ts --project=mock
```

## Validation

The build reproduces the committed bundle. All 98 unit tests pass. The full
browser run passed 110 tests and failed one existing task-layout assertion;
the six dependent performance tests were skipped. All six task tests passed
on the immediate isolated run. Its `boxes()` helper reads node coordinates
in separate browser round trips while initial viewport fitting can occur;
the failed comparison saw `db.x == -0` and `queue.x == 23`. Capture all boxes
together or wait for the view to settle. This does not establish a product
layout defect.

The three new resource tests fail consistently with the uncaught errors
described above.
