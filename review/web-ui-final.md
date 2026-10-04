# Web dashboard review: approved

Approved frontend tip `98379fba6215df83c2647d0823c1ec033feb8c77`.
All findings from `web-ui-initial.md`, `web-ui-followup.md`, and
`web-ui-resource-identity.md` are closed. The reviewed frontend sources and
committed bundle match that tip in the reviewer worktree.

Application calls require a verified connection, including keyboard answers.
Failed resynchronization restarts the connection; the opening deadline also
bounds stalled handshakes. Stopped pages cannot attach new terminals.
Message and log lists preserve the reader's row identity when their rings
evict entries, and failed prompt submissions preserve their text and mode.

Rejected message and model resources now show their errors without throwing
during rendering. Uncached messages wait for a verified connection and retry
when it returns. The final change checks fetched message identity before
display and associates retained errors with the id that was requested. The
regression that previously displayed message A beneath message B's heading
after stop now passes.

Independent validation:

- On `076f7765`, the full frontend run passed: 98 unit tests, 122 browser and
  performance tests, and two Rust bundle tests. All measured performance
  budgets passed. The task-layout assertion also passed after coordinates
  were captured together following viewport fitting.
- On final tip `98379fba`, the build reproduced the committed bundle exactly
  (554,319 bytes). All 98 unit tests passed. The 15 focused browser tests in
  `reviewer-resources.spec.ts` and `reconnect.spec.ts` passed, including the
  final identity regression. Both Rust bundle tests passed.

The final delta changes only message identity/error selection, its stopped
text, browser coverage, and the generated bundle. The author additionally
reports two complete final browser/performance runs of 124 passing tests and
a successful live daemon check. Those runs are author evidence; the
independent final checks above cover the delta from the full reviewed run.

The server review is recorded separately in `web-server-final.md`.
