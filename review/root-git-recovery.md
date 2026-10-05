# Root Git fixture contamination and bounded recovery

Root `main` moved from the last approved `f3e9f234` to the test commit
`e147418ab90a1d7f58d5735e182bca88e317187d`. The commit adds only a two-byte
`tracked.txt` fixture. A second fixture staged `.crew/config.toml`; neither file
exists in the root working tree. Both changes came from the reviewer's exported
`GIT_DIR` during the first native-foundation test run, not host contention or a
candidate source change. The repair below preserves the stray commit and removes
only these two attributable index entries. QA performed read-only root inspection
and wrote backups/proposed artifacts only inside its own worktree.

## Exact attribution

Reviewer admission `m-cc4900056fa8` supplies the invocation. The shell exported
`GIT_DIR=/home/nade/workspaces/personal/crew/.git` to archive `9da898dd`, changed
into the staged source, then ran `cargo test --locked` through `nix-shell`.
Its `env -u` cleared the three Crew variables but left the repository override
exported. For the raw fixture commands, `git -C <temporary-directory>` changed
the working directory while the inherited `GIT_DIR` still selected root metadata.

| Writer | Source and operation | Root evidence |
| --- | --- | --- |
| `daemon::sandbox::tests::a_new_worktree_is_filled_through_the_helper_and_made_without_it` | `crates/crew/src/daemon/sandbox.rs:1247`; raw Git constructor at 1252, init 1262, add 1264, commit 1265. The closure sets author identity with per-command `-c`. | Root HEAD and main reflogs record `f3e9f234` to `e147418`, author/committer `t <t@t>`, message `first`, at 2026-10-05 17:28:23 -0500. Commit/tree comparison adds only `tracked.txt`, mode 100644, blob `587be6b4c3f93f93c489c0111bba5596147a26cb`, exactly the fixture's two bytes. |
| `endpoints::tests::files_others_can_write_and_files_from_the_repository_are_rejected` | `crates/crew/src/endpoints.rs:1132`; synthetic configuration at 1137–1140, raw Git constructor 1169, init 1186, forced add 1191. | The staged 76-byte blob `f0289d50958bdc54b88af0b0187f62085756dec2` is byte-equal to this fixture constant. Its value is redacted from this report. This test passed while staging into the wrong repository. |

The first log has 476 passed / two failed. The two failures are the worktree
fixture above and
`daemon::sandbox::tests::an_agents_jj_does_all_its_work_in_its_sandbox_and_the_helper_integrates`.
The latter's failed assertion at sandbox.rs:2256 also reads Git state through a
raw inherited command; it is not evidence of a second root commit. The root
reflogs contain only the one matching fixture commit. Sanitized reviewer reruns
passed 478/0 and sandbox 25/25 twice; approval of `9da898dd` stands.
The earlier `m-31c4e1c6e451` attribution and QA's copied task artifact note saying
"sandbox contention" are superseded by `m-e97a49004775` and this exact audit.
The accepted foundation files themselves never made that causal claim.

## Preserved state and scope

Private backups are under
`/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/root-git-recovery/2026-10-05T22-43-22.875Z/`,
with directory/file permissions 0700/0600. They include HEAD, packed and loose
refs, index, local Git config, reflogs, existing worktree administration, hooks,
exclude/description metadata, the stray commit/tree and two fixture blobs, and
the original failing log/background output. The failing log SHA-256 is
`8492b934fcf7d2c9134ec0c20c2f739a0d0dceb63e00fc7becccbca165db523e`.
Its timestamp is 17:29:28 -0500. Raw configuration values stay in private backups.
The [redacted machine record](root-git-recovery.json) contains the comparison
facts and hashes.

Read-only object/index decoding and content/mode comparison establish:

* Parent `f3e9f234` has 619 tracked files. Every one of those index entries and
  root working-tree bytes/executable modes is unchanged.
* `e147418` is exactly one commit after that parent and adds only `tracked.txt`.
* The current version-2 index has 621 stage-zero entries: those 619 plus the
  two fixture paths. No unmerged entry or other staged difference exists.
* Both fixture paths are absent from the root filesystem. No root source or
  working-tree configuration write is indicated by either fixture.
* No `agents/a` ref or matching reflog exists; existing linked-worktree
  administration has no modification at/after the fixture time. Other team
  refs may continue changing through their ordinary source work.
* Local `.git/config` was rewritten at the fixture time. Its current 92 bytes
  contain only the four standard core settings and no local user-name/email
  override. Its prior bytes are unavailable, so preserve the current file;
  do not infer a reason to replace it. Per-command `-c` identities are not
  persistent Git configuration writes.

Unknown untracked/ignored files, operator state and all working-tree content
are preserved. This audit does not classify them as disposable. No root ref,
index, config, worktree file or service was modified by QA. HEAD/index/config/
packed-ref hashes remained unchanged during the read-only audit.

## Concrete repair proposal for the orchestrator

This proposed plan requires independent checking; QA has not executed it. Do not
perform a broad reset, checkout, restore, clean or worktree rewrite.

The private `proposed-index` contains the original encoded entries minus only
`.crew/config.toml` and `tracked.txt`; the stale optional TREE cache is dropped
and the index SHA-1 footer is recomputed. All other encoded entries, including
their stat metadata, are unchanged. It has 619 entries, expected to describe
exactly the parent's tree `baec4c7d4d4c3870b7029e3ba506697913650563`.
Before application, the independent reviewer must read it through Git's own
index parser using an explicit **private** `GIT_INDEX_FILE`, verify its paths,
modes and blob IDs against `f3e9f234`, and leave the root index untouched.

Reviewer `m-2f54c25af464` completed that check with a clean environment and
explicit private index. Git computed the exact parent tree
`baec4c7d4d4c3870b7029e3ba506697913650563`; it found 619 entries and neither
fixture path. Root index hash remained unchanged. `write-tree` may write a
content-addressed tree object; this check changed no ref, root index or
working-tree/config content. The reviewer also approved the guarded repair
proposal. Root application and subsequent verification remain unperformed
at this report checkpoint.

| Guard | Expected value |
| --- | --- |
| Root HEAD | `ref: refs/heads/main` |
| Root main | `e147418ab90a1d7f58d5735e182bca88e317187d` |
| Parent/recovery main | `f3e9f2343934c665ec9a557688c3bcf4b5eaac4d` |
| Current root index SHA-256 | `d4b4ff6e5ff647f3cad58aa0efaaedf24016612cade227035a0bdf43729f9619` |
| Proposed index SHA-256 | `54928bc6fdcdc8db459ef36b0e599e06f6d0c424ea06b2a4cee09f32b6a2f396` |
| Current local Git config SHA-256 | `cfe7ba1238c9a78be7535d7c63bcaf5a4d5011d46b07c9b45d3bbf7d6c312dfe` |

1. Preserve the original snapshots above and independently validate the private
   proposed index. Clear `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
   `GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
   `CREW_IDENTITY`, `CREW_SOCKET` and `CREW_LAUNCH` before every repair command;
   name the root explicitly. Disable hooks and filesystem-monitor commands for
   the local reference operation. Do not run any Git-mutating fixture test.
2. Acquire root `.git/index.lock` exclusively, failing if it already exists.
   Hold that owned lock while rechecking HEAD/main, original index/config hashes,
   the two absent paths and the 619 unchanged baseline entries/files. Stop on
   any mismatch; preserve new or unknown changes.
3. Fill only that owned lock file with the validated proposed-index bytes,
   preserving the original index mode. Keep the original root index in place
   until the reference transaction succeeds.
4. Use one guarded reference transaction to create a fresh retention ref and
   restore main. The backup ref must be absent; never overwrite an existing
   ref. Only these two refs participate:

   ```text
   start
   create refs/crew-recovery/20261005-leaked-git-dir-e147418 e147418ab90a1d7f58d5735e182bca88e317187d
   update refs/heads/main f3e9f2343934c665ec9a557688c3bcf4b5eaac4d e147418ab90a1d7f58d5735e182bca88e317187d
   prepare
   commit
   ```

   Feed this text to root `git update-ref --stdin` through the orchestrator's
   recorded command path. The expected old main supplies the compare-and-swap
   guard. On transaction failure, leave main/index unchanged and remove only
   the index lock this invocation owns.
5. After the successful transaction, atomically rename the owned index lock
   onto `.git/index`. No working-tree file is opened for writing. If this last
   installation fails, stop with the backup/ref/index records preserved;
   do not attempt an automatic broad rollback or landing.
6. Verify main `f3e9f234`, retention ref `e147418`, index hash/619 entries/tree,
   an empty tracked/index diff, unchanged config, absent fixture files and all
   619 unchanged working-tree bytes/modes. Other refs and working-tree content
   receive no repair mutation. Release the fixture pause only after the
   orchestrator verifies isolation. The already reviewed foundation fast-forward
   to exact `9da898dd` is a separate subsequent landing.

## Regression prevention boundary

Future fixture commands must clear the Git repository/common/object/index
overrides and Crew agent variables, as required by `m-942f4c8fd757`. Use an explicit
per-command `--git-dir` for archive reads; never export it across a test run.
The production Git command builder already removes the three
Git overrides at `crates/crew/src/git/mod.rs:51–53`; the two raw unit-test
constructors above bypass it. A focused fixture-isolation follow-up should
sanitize those constructors and protect native test runners. Its regression
should use a disposable foreign repository/index and prove fixture changes
cannot affect that repository when overrides are inherited. Do not reproduce
against the operator root or expand this recovery into a production rewrite.

QA owns `scripts/native/**` runner changes; `fixture-git-isolation` tracks the
bounded raw test-constructor hardening. The present card changes no production
or test source. Native source work
can continue independently while unsafe Git-fixture runs remain paused.
