# Message resource identity follow-up

On `076f7765`, the prior resource error findings are fixed. Independent
validation passed: build, 98 unit tests, 122 browser/performance tests, and
two Rust bundle tests. The task-layout test also passed with the revised
coordinate capture.

One remaining P2 affects the same message resource's lifetime. Solid retains
a resource's previous value when its source becomes null. `MessagePanel`
now disables fetching while disconnected but still displays that old value,
without checking that its message id matches the panel's selected id.

Reproduction in Chromium:

1. Fetch message `review-first`, which is absent from the local message ring.
2. Stop the dashboard.
3. Change the fragment/history selection to uncached message `review-second`.

The heading changes to `message review-second`, while the first message's
body and sender remain displayed indefinitely. The fourth test in
`web/e2e/reviewer-resources.spec.ts` reproduces this mismatch.

Use a fetched value only when its identity matches the selection, or clear
it when that identity changes. Keeping the previous value for the same
message while disconnected is fine.
