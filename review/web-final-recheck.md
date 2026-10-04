# Web final review: recheck and handoff

Reviewed integration `e2fbaa62e729a337ad56290324b733277e6c792d`.
The reviewer tree matches it outside `review/`. This report supersedes the
open production findings in `web-final-findings.md`.

## Closed findings

- `e8dbbf40`: inspector identity and reconnect recovery. Selection now owns
  its panel instance, so asynchronous results and retained state cannot cross
  agent/task/message/event identity. Inspector fetches react to the verified
  connection becoming live. All five reviewer regressions pass.
- `47ec81ea`: the hostile second document reads and rewrites storage before
  its stolen verifier is supplied as the server proof. The test checks the
  ordering and verifies that no forged snapshot is requested. The agent-token
  attack now originates from the agent process inside its sandbox, and both
  hello and the subsequent snapshot are unauthorized.
- `190316c1`: P16 has a p95 gate. Independent measurement of 300 pushes gave
  p50 58.421 microseconds and p95 114.076 microseconds. Measuring through
  client receipt and parsing is a valid upper bound on the server's write.

## Remaining QA gates

No production bug remains open in this review. Two test issues were sent
directly to QA and still prevent the aggregate approval:

1. The `perf` Playwright project also discovers `e2e/live/perf.spec.ts`.
   Consequently the frontend recipe requires a daemon binary, and the real
   load test runs concurrently with the mock performance measurements.
   Exclude the live directory from that project and verify discovery.
2. The real P10/P11 test reports offered RPC rate but does not assert
   successful calls or the push rate the page actually receives. Preloading
   may time out before 5000 events without failing, and a newly opened page
   initially requests only 1000 events. Assert the intended browser ring
   occupancy and achieved load during each timing window; a producer that
   is rejected or throttled must not silently pass an idle frame budget.

QA messages: `m-df59db07f583` (project isolation) and `m-14fdfd2a3f42`
(achieved load). The final verdict awaits these fixes and their integration.

## Independent checks

- Full Rust workspace on `c0fc1892`: 822 passed, zero failed, two optional
  tests ignored. Clippy with `-D warnings` passed. The only later Rust change
  is the P16 test above, which passed independently.
- Live Chromium suite on jj: nine passed. The same nine passed with required
  bubblewrap sandboxes.
- Final frontend fix: build reproduces the committed 554,537-byte bundle;
  98 unit tests and both Rust bundle tests pass. The complete browser run
  passed 131 tests, including the five new regressions and two live perf
  cases accidentally discovered by the perf project. All its numeric limits
  passed, but its timing evidence needs the isolation correction above.
- Corrected live security cases: all three passed independently with jj and
  `CREW_TEST_SANDBOX=require`, one worker.
- Manual real-daemon Chromium checks: graph pan/zoom; node drag persisted
  across reload; graph keyboard navigation and inspector focus return; task
  DAG hover, dependencies and browser history; message round-trip; terminal
  echo; pause/resume; log filters and usage. No later HTTP resource request
  or idle RPC polling was observed, and page errors were empty. These
  interactions originally exposed the inspector identity problem.

The architect owns the final card and the independent second review. Send
the revised integration and the two remaining gate results to both reviewers.
