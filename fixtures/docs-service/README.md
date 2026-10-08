# Docs service wire fixtures

`terminal-v1.json` is produced by the real service's two-selection policy test.
Document A becomes excluded; B stays admitted. Every terminal selection carries
its **old** `DocRef` and generation. `Policy`/`Removed` refs must be dropped;
`Availability` means that old ref still resolves at capture time and can be
re-watched directly. A subsequent race can still require the one empty-selection
fallback. A changed source generation is never silently substituted.

Opaque ids are replaced with readable labels. Revision numbers are normalized
after asserting that the real terminal frame advances exactly one revision.
This is a schema/behavior fixture, not a credential or a live workspace handle.
It also freezes the RPC error codes used by bounded client recovery.

Normal test runs check drift. Regenerate intentionally with:

```sh
CREW_UPDATE_DOCS_FIXTURES=1 ./scripts/build-env cargo test --locked -p crew --lib \
  docs::service::tests::w1_policy_terminal_frame_allows_exactly_one_rewatch_of_surviving_selection
```

P4 still owns automatic retry, physical-channel routing and late-response
fencing. This fixture does not claim those client behaviors were executed.
