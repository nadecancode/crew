# Initial web dashboard review

Reviewed `86b64869`, `06837801`, `3e95afb7`, and `f473e222` against the
approved web design. Six findings need fixes before approval.

The existing frontend build and checks passed independently: 95 unit tests
in 14 files, 107 Playwright mock/performance tests, and two Rust bundle tests.
Rebuilding produced the same committed bundle and manifest. The seven additional regressions
below fail on that bundle/source: six browser tests and one connection unit
test. The backend has separate findings in `web-server-initial.md`.

## P1: Application RPCs bypass the server proof check

`net/conn.ts::call` permits calls whenever the WebSocket is open. It does not
require the server to have proved itself. Although most callers check
`link.live`, the question shortcut and message panel do not.

Two independently reproduced browser paths:

1. Open a question, disconnect, and let the replacement socket open without
   answering hello. Focus the dialog's enabled "later" button and press `1`.
   The disabled option's answer is sent before any proof arrived.
   `QuestionDialog`'s key handler bypasses its disabled buttons, and
   `app.actions.answer` lacks a live check.
2. During that same unproven reconnect, navigate to a message absent from
   the local cache. `MessagePanel` sends `message_get`; a fabricated reply
   from the replacement server renders as the message body. The proof gate
   on pushes does not protect RPC results.

This reopens the impersonation problem the design's proof mechanism was
intended to close. Gate application RPCs and their results centrally, with
only the necessary handshake calls allowed before verification. Fix the
question action's disconnected handling as well.

## P2: A failed resync leaves stale state marked live

`net/state.ts` swallows a resync snapshot's rejection and clears `inFlight`.
Its late reply is ignored by `Conn`; no replacement snapshot or reconnect is
requested. Changes missed during the lag are then absent indefinitely.

The browser regression changes an agent while simulating a missed push,
requests resync, and delays its snapshot reply for 3.3 seconds. The page
still shows the old agent status eight seconds later and remains live.
Recover or retry before declaring the state synchronized.

## P2: A stopped page can start another terminal connection

`TerminalTab.tsx`'s agent-id effect calls `attach` without checking that the
page is live. The asynchronous attach function also checks only component
disposal after awaiting the terminal module.

Reproduction: open a pty terminal for one agent, shut down, then change the
fragment/history selection to another agent while the terminal tab remains
selected. A new WebSocket upgrade reaches the stopped origin. Check page
lifetime both before and after the await; final stop must prevent new
terminal connections.

## P2: Full rings shift the text under a reader

`VirtualList` anchors numeric scroll offsets while the bounded rings evict
their first entries. After 1,050 messages fill the 1,000-entry bus, scroll to
row 400: the first visible message is `review-450`. One more message changes
it to `review-451` without operator input. Cursor indices can shift too.

Preserve visible and selected item identities when old entries leave, until
the anchored item itself is evicted. Cover both messages and events; the
existing scroll-preservation test appends below capacity.

## P2: Failed prompt submission discards the draft

`app.actions.prompt` hides rejection with `quietly`; the prompt branch of
`operate.ts::submit` always returns `keep: false`. A refused `agent_input`
therefore clears the operator's text and returns the box to message mode.

The browser regression receives `unavailable`, observes the error status,
and finds the draft empty. Preserve the draft and prompt mode on failure;
clear them only on confirmed success.

## P2: An unfinished upgrade escapes the reconnect deadline

`Conn.open` has no opening timer. The 30-second ceiling is checked only
after a socket closes or in the callback that starts another attempt. If a
listener accepts TCP but does not complete the HTTP upgrade, no subsequent
check runs.

The fake-clock regression drops the first socket, starts the next attempt,
and leaves it connecting. After another 60 seconds, the connection has not
stopped. Keep the overall deadline active during opening and handshake,
clearing it on verified live state or final stop.

## Reproductions

```sh
cd web
bun run test test/reviewer-conn.test.ts
PLAYWRIGHT_EXECUTABLE_PATH=/path/to/chromium \
  bunx playwright test e2e/reviewer.spec.ts --project=mock
```

These tests deliberately fail on `f473e222`. Browser traces and snapshots
were retained locally under `notes/reviewer-web-ui-*`.
