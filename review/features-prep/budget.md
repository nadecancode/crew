# Shared feature bundle allowance

The operator approved `m-9a6de4b0d5cc`, recorded by the orchestrator in the
`features-acceptance-prep` and `features-p6-integration` cards. Epics, Docs,
Usage and Asides share one allowance; each slice does not get a separate one.

| Gate | Previous ceiling | Approved ceiling | Baseline bytes |
| --- | ---: | ---: | ---: |
| P1 nonterminal JavaScript | 230,500 | 310,500 | 230,387 |
| P2 all CSS | 34,000 | 42,000 | 33,999 |
| P3 terminal JavaScript | 350,000 | 350,000 | 330,812 |
| P4 every manifest-listed file except manifest.json | 600,000 | 688,000 | 595,884 |

The baseline is manifest source `fnv1a64:fa14dc56661a2b82`. The manifest and
bundle are unchanged. Counting still uses the same terminal-file predicate,
the same all-CSS predicate, and the same complete manifest file list. The
three active assertion sites now agree: Vitest, Playwright and Rust's bundle
integrity test. No runtime behavior, dependency, feature readiness or code
loading policy changed. All chunks must still load at document start.

Historical design results keep their measured values and original limits.
The active budget sections in `notes/web-design.md`, `notes/web-revamp.md` and
`notes/dashboard-features.md` identify the new approval and shared baseline.
Each P5 author records a measured delta against a named base; the combined
build remains authoritative.

Validation: five budget/dependency Vitest tests, the single static P1–P4
Playwright test, and both Rust web-bundle tests pass. The Rust job used the
standing H slot, `CARGO_INCREMENTAL=0`, and a preflight free-space measurement
above 100 GiB. [The evidence manifest](budget.json) hashes the logs and bundle
manifest. Timing, frame, heap, a11y and final combined performance are not
remeasured or relaxed by this checkpoint; they remain P6 gates.
