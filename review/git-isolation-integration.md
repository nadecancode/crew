# Git fixture and native runner safeguard integration

The combined source starts at approved root `d4e0f979` and retains native runner
`95688127` exactly. Runtime's standalone fixture correction `1122d936` is applied
as `0886ccd0`, without gateway ancestry, conflict resolution or native product
code. The [machine inventory](git-isolation-integration.json) records the exact
source mapping, comparisons, commands and log hashes.

The two layers remove inherited `GIT_*`, `HERDR_*` and Crew agent context before
test/build dispatch. The actual fixture regression then deliberately injects
only its own disposable foreign repository/index overrides into child tests.
Both real fixtures must run and pass while every foreign `.git` byte remains
unchanged. No test selects the operator root as its sentinel or changes
process-global environment.

## Source and scope checks

Fourteen source/guidance/evidence paths differ from `d4e0f979`, attributable only
to the two approved safeguard commits. The fixture import's patch bytes equal
the original after removing only index hashes and hunk range/heading lines.
All runner files and its retained evidence equal `95688127`.

Web source/assets, manifests, lockfile, portable/core/TUI packages, Apple/Linux
product shells and production Git implementation equal the accepted baseline.
Production prefixes in `endpoints.rs` and `daemon/sandbox.rs` also equal it;
the library adds only two private `cfg(test)` module declarations. The four
fixture-source edits, shared test helper, permanent regression and test evidence
contain no production endpoint/VCS behavior change or compiler-floor change.

## Executed combined checks

All invoking test/build processes source the centralized native context filter.
Rust targets and logs stay in QA's ignored `target/native-validation/`.

| Check | Result |
| --- | --- |
| Permanent disposable foreign-repository regression | 1/1 top-level test; six actual child fixture cases preserve HEAD/index/config/refs/objects bytes and pass exactly one test each. |
| Affected sandbox unit module | 25 cases. Initial five self-skipped because the dedicated target did not yet contain the CLI binary. Those five were rerun individually after the integration build produced it; all execute and pass. |
| Affected real sandbox integration | 29 cases. Initial five self-skipped for missing `socat`. Only those five were rerun with the pinned Nix `socat` environment; all execute and pass. No skipped control is counted as executed in its initial run. |
| Actual default CLI floor | Rust/Cargo 1.82.0 locked binary build, version and top-level/run/attach/web/stop help pass in 22.41 seconds. No native GUI/Apple 1.82 claim. |
| Combined native runner regression | Four process-observer paths pass: common, Linux available/re-entry/build; arguments, toolchain/loader/ordinary environment and foreign metadata sentinels preserved. |
| Script syntax | All 11 scripts individually pass `bash -n`. |

The first Rust log and both narrowly targeted completion logs retain all skip
messages and executed reruns. The 55 top-level cases above therefore have
executed coverage; the permanent regression also contains its six child cases.
Accepted broader fixture library/Clippy evidence and the runner's actual GTK
tooling evidence remain valid for their unchanged source. No full workspace,
browser, GTK or Apple matrix was repeated. Apple verification remains deferred
by operator.

Reproduce the primary checks from the QA checkout:

```sh
# Choose a worktree-local target; build the CLI first for sandbox unit controls.
bash -c 'source scripts/native/common.sh; ./scripts/build-env cargo build --locked -p crew --bin crew'
bash -c 'source scripts/native/common.sh; ./scripts/build-env cargo test --locked -p crew --lib git_fixture_tests::inherited_git_overrides_cannot_mutate_a_foreign_fixture_repository -- --exact --nocapture'
bash -c 'source scripts/native/common.sh; ./scripts/build-env cargo test --locked -p crew --lib daemon::sandbox::tests -- --test-threads=1 --nocapture'
nix-shell -I nixpkgs=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source -p socat \
  --run 'bash -c '\''source scripts/native/common.sh; ./scripts/build-env cargo test --locked -p crew --test sandbox -- --test-threads=1 --nocapture'\'''
bash -c 'source scripts/native/common.sh; scripts/check-msrv'
scripts/native/tests/environment.sh
```

Install the actual 1.82.0 toolchain before the floor gate; QA used its existing
isolated rustup home and separate target. Logs live at
`/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/native-validation/git-isolation-*.log`.
The prior [root recovery](root-git-recovery.md) and
[native runner evidence](native/runner-environment-isolation.md) remain intact.
Root refs/index/config, operator services, listener, credentials and live team
were not changed by this integration. Final native application integration must
retain this safeguard baseline; connected application acceptance remains separate.
