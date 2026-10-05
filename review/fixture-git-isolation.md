# Git fixture isolation

Task `fixture-git-isolation`. This is a test-source correction, not a change
to production Git behavior, endpoint tracking, native authentication or the
default CLI compiler requirement.

The root incident was an archive shell exporting `GIT_DIR` into a fixture
run. Git's `-C <fixture>` does not override that repository selector. The raw
sandbox fixture committed its `tracked.txt` into the selected foreign
repository; the endpoint fixture staged `.crew/config.toml` there. Identity
was supplied with per-command `-c` arguments, so this did not change the
operator's local user configuration. Root metadata recovery belongs to the
separate controlled recovery; this task performs no root repair or Git writes.

## Correction

The shared test-only helper constructs Git commands with an explicit fixture
directory and per-command `user.name=t`, `user.email=t@t` and
`commit.gpgsign=false`. It removes every inherited `GIT_*` variable, including
repository, worktree, index, common/object/alternate-object, namespace,
configuration and discovery overrides. It also removes `HERDR_*` and
`CREW_IDENTITY`, `CREW_SOCKET`, `CREW_LAUNCH`, preserving other environment
entries. No process-global environment is modified; no `safe.directory=*`
exception or new dependency is needed.

Affected raw unit/integration fixture commands use that helper. The shared
integration `Session::command_of` clears the same inherited context before
launching a fixture CLI; deliberate overrides added by a test afterwards
still work. Structured sandbox launcher inputs that are never executed retain
their original argument behavior.

The two affected unit fixtures re-execute exactly themselves when Git
variables are present. The child strips the context and must run and pass
exactly one test. This also protects their calls into production code that
intentionally honors Git's environment, without changing that production
code. There is no inherited child-marker bypass.

## Disposable before/after evidence

Baseline binary was saved from `759aea95` before editing. A subprocess probe
created its own temporary foreign repository and injected its `.git` path as
`GIT_DIR` into each actual fixture. It never selected the operator repository.

| Actual fixture | Before | After |
| --- | --- | --- |
| Sandbox worktree setup | Exit 101; foreign main ref, index, reflogs and objects changed. | Exit 0; all foreign repository bytes unchanged. |
| Endpoint tracked-file setup | Exit 0 despite foreign index/object changes. | Exit 0; all foreign repository bytes unchanged. |

Foreign config bytes were unchanged before and after. A successful fixture
exit alone was therefore insufficient evidence of isolation.

The permanent regression runs both actual fixtures with three override sets:
`GIT_DIR` alone, foreign `GIT_INDEX_FILE` alone, and combined repository/
worktree/common/object/alternate/namespace/config/discovery overrides. All
six child executions must pass exactly one test while every foreign `.git`
file remains byte-equal, including HEAD, refs, index, config and objects.

Ignored local evidence lives in `target/fixture-git-isolation/`: saved baseline
binary and metadata, `before.json`, `after.json`, the reusable probe and the
focused test log. The permanent regression passed, including all six child
cases. The default crew library passed 479/479 and real sandbox integration
passed 29/29 with the parent Git/agent environment cleared. Default all-target
clippy with `-D warnings` and formatting passed. The actual Rust/Cargo 1.82.0
locked default CLI build and help gate passed using the previously authorized
read-only compiler and this workspace's own target directory.

```sh
./scripts/build-env cargo test --locked -p crew --lib --test sandbox -- --test-threads=1
./scripts/build-env cargo clippy --locked -p crew --all-targets -- -D warnings
./scripts/build-env cargo fmt --all -- --check
./scripts/check-msrv
```

## Safe archive and fixture invocation

Read an archive with a repository argument on that command only:

```sh
git --git-dir="$crew_archive_git" show "$crew_review_commit:path/to/file"
```

Do not export repository overrides across a test run. Start tests with Git
repository/index/object/ref/config selectors and Crew agent context removed;
the native runner environment boundary is owned by QA. Production VCS
semantics, Rust package manifests and lockfile remain unchanged.
