# Compact CLI helper

Card: `compact-cli-helper`. Implementation checkpoint `962b21da`, integrated
with main `df81cccb` at `3486e90d`. This implements the first two slices of
`notes/token-savers.md`; toolbox packaging and native execution wrappers are
separate work.

`crew output view` consumes captured files and a required original outcome.
Recognized complete successful cargo test output becomes a versioned JSON
summary with explicit omissions and exact raw references. Every unknown
line, warning, failure, timeout, machine format and unexercised test run
stays raw. Raw previews are bounded and labelled; `crew output raw` recovers
exact byte ranges without executing the producer again. A view/storage
failure cannot become a success summary, and the helper's process status
never replaces the original command status stored as data.

The member must authenticate against the daemon on each invocation. The
local store is private, descriptor-relative, scoped by credential epoch,
bounded by bytes/count/age, and integrity checked. Public references contain
no private scope, path or timestamp. Existing upstream redaction is preserved
exactly; this is not a secret detector or a new isolation boundary for
unsandboxed processes sharing a Unix UID. See
[`crates/crew/src/output/README.md`](../crates/crew/src/output/README.md)
for the interface, limits and known raw-only grammars.

## Validation

All commands ran offline with respect to model providers, in content's slot
D. Native model CLIs were not invoked. Logs remain in `target/output-*.log`.

| Gate | Result / evidence |
| --- | --- |
| Corpus source equality | Five decoded entries byte-equal to the committed gzip; reconstructed warning entry byte-equal; archive SHA-256 checked against the fixture README. |
| Core corpus/parser | `cargo test --locked -p crew-core --test output_view`: 7 passed, including zero execution, invalid/unknown/control/machine data, inconsistent counts, distinct streams and stable references. `output-core.log`. |
| Workspace | `cargo test --locked --workspace --no-fail-fast -- --test-threads=1`: 1320 passed, 0 failed, 4 ignored across 72 harness results. `output-workspace.log`. |
| Final store tests | `cargo test --locked -p crew --lib output::store::tests`: 7 passed. Namespace denial, integrity, truncation, links, descriptor replacement, expiry/eviction and interrupted publication. `output-store-final.log`. |
| Final CLI and required sandbox | `CREW_TEST_SANDBOX=require cargo test --locked -p crew --test output_cli`: 4 passed, 0 ignored. Actual member sandbox view/recovery, authenticated cross-member and revoked-identity denial, daemon restart, binary chunks, status variants and refusal cases. `output-cli-final.log`. |
| Strict Clippy | `cargo clippy --locked --workspace --all-targets -- -D warnings`: passed. `output-clippy.log`. |
| Formatting | `cargo fmt --all -- --check`: passed. `output-fmt.log`. |
| Rust 1.82 | `scripts/check-msrv`, target `target/cw-msrv`: default CLI build/help passed with the committed lockfile. Additional `output view --help` and `output raw --help` passed. `output-msrv.log`, `output-msrv-*-help.txt`. |
| Existing portable crates | `cargo check --locked -p crew-protocol -p crew-client --lib --target wasm32-unknown-unknown`: passed. `output-wasm.log`. |
| New pure module portability | The unchanged `output_view.rs` compiled for `wasm32-unknown-unknown` through an isolated crate with only serde 1.0.229 (derive) and sha2 0.10.9. `output-wasm-module.log`. |

The full workspace run precedes one Clippy-only equivalent rewrite of the
store's not-found predicate (`!is_some_and(equal)` to `is_none_or(not_equal)`).
Final store and CLI/sandbox suites, strict Clippy, formatting and MSRV cover
that rewrite. Other subsequent changes are README clarifications and this
evidence note; parser behavior and the CLI contract did not change.

The portable-module check uses `target/output-portable/Cargo.toml` with an
independent `[workspace]`, the two dependencies above, and a `src/lib.rs`
containing `#[path = "../../../crates/crew-core/src/output_view.rs"] pub mod output_view;`.
It runs `cargo check --offline --manifest-path target/output-portable/Cargo.toml
--lib --target wasm32-unknown-unknown`. The whole `crew-core` crate remains
host-only because of its existing Unix client/paths modules; this check does
not claim otherwise. No dependency or lockfile change was needed in crew.

These gates establish deterministic output and recovery behavior. They do
not establish native provider token/cost savings, automatic hook support or
execution/approval equivalence. Those remain the later packaging, native
acceptance and paired benchmark slices.
