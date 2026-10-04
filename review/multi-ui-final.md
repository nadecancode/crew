# Multi-workspace frontend: approved

Approved the frontend at `4a69d1167c113ca3007c3b10224e5ae1cbd18354`.
All findings in `review/multi-ui.md` and `review/multi-ui-r2.md` are closed;
no frontend findings remain.

The final changes preserve the switcher trigger's state across the focus
change preceding its click, and suppress duplicate unjoined rows when the
server's listing also names them as joined. Deduplication uses the full
joined listing, independently of the current text filter. The underlying
server listing issue was reported by the author to the server implementer.

Independent checks on the final frontend passed:

- Build reproduced the committed 575,599-byte bundle, source fingerprint
  `fnv1a64:c5dd2a33534c6d7e`.
- All 154 unit tests in 20 files.
- All 22 targeted browser tests in `reviewer-multi.spec.ts`,
  `workspaces.spec.ts` and `keyboard.spec.ts`, including the previously
  failing trigger-toggle case.
- Both Rust `web_bundle` tests.

The initial review independently ran the complete mock and performance
suites. For the final revision the author reports 154 mock and performance
tests passing, P17 switch p95 44.6 ms and P19 zero idle frames and calls.

The author also completed live integration against server `b57724d4`,
merged into the submitted branch, with three dry-run daemons and a headless
browser. That covered pairing with an initial workspace, joining another
workspace without reloading, switching, a message delivered only to the
selected workspace, daemon stop and restart, removal and rejoining,
listing, and stopping the shared server, with no page errors. These live
results are the author's validation, not an independent rerun by this
review. Review of the shared-server implementation itself belongs to
`multi-server`.
