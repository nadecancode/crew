# Native runner environment isolation

Native build/test subprocesses now discard inherited Git and legacy agent
context, plus `CREW_IDENTITY`, `CREW_SOCKET` and `CREW_LAUNCH`. This prevents the
repository/index redirection that caused the [root fixture incident](../root-git-recovery.md).
`git -C` alone does not override an inherited `GIT_DIR`. Rust/Nix selection,
GTK/pkg-config/loader paths, working-directory behavior, caller arguments and
ordinary environment retain their existing behavior.

`scripts/native/common.sh` owns the boundary and invokes it when sourced and
when preparing the Cargo target. Every existing native runner sources that
library directly or delegates to one that does. The standalone `linux-env.sh`
now sources it too, including after Nix re-entry. The policy clears all `GIT_*`
and `HERDR_*` names, covering namespace, common/object/index paths, discovery,
injected configuration and keys beyond its active count. It matches the separate
Rust fixture isolation policy. No Rust/product, manifest, toolkit pin or
previously retained preparation evidence changes.

## Executed checks

The new `scripts/native/tests/environment.sh` supplies disposable foreign
metadata sentinels and active inherited variables. It observes child processes
at four actual script paths: the sourced common boundary, Linux with a selected
toolkit, Linux fallback/Nix re-entry, and Linux build dispatch. Its fake compiler,
package and bootstrap tools validate environment/arguments; their success is
explicitly not a Cargo build or application result. The fake Nix hook introduces
Git, legacy and Crew context after initial cleanup, requiring another cleanup
before the requested command.

The same regression against exact `9da898dd` common source fails with
`inherited context reached child: GIT_ALTERNATE_OBJECT_DIRECTORIES` and exit 1.
The corrected source passes all four paths, including spaced and literal
arguments, Rust/Nix selection, Cargo target behavior, loader prefix/suffix,
working directory and unrelated environment. Foreign index/ref/object sentinel
checksums remain unchanged.

The existing real `tests/tooling.sh` also passes with inherited disposable Git
repository/common/index/object/namespace/config overrides, legacy context and
Crew agent context. It compiles the GTK toolkit probe, observes and captures its
mapped window, rejects early exit and a live process without a window, and
verifies cleanup of its own app/Xvfb children. This is runner/environment proof,
not connected Crew UI acceptance. GTK 4.22.4, VTE 0.84.1, GLib 2.88.3 and the
existing X11/Cairo/private-display settings are preserved.

All 11 native shell scripts pass `bash -n`. Existing named rejection gates remain
unchanged: Apple build on Linux exits 2 with deferred-verification text; iOS test
without a destination exits 64; absent FFI/bindgen exits 2; output outside
`target/native-validation/**` exits 64. **Apple verification deferred by operator**:
no Xcode, compilation, simulator/device or native Apple execution was performed.
No full unchanged Rust/web matrix was repeated.

Logs and disposable artifacts remain under QA's
`/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/native-validation/`.
The [machine record](runner-environment-isolation.json) retains commands, results,
log hashes and the real toolkit run's geometry/settings/versions. Replay with:

```sh
scripts/native/tests/environment.sh
NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  scripts/native/tests/tooling.sh
for script in scripts/native/*.sh scripts/native/tests/*.sh; do
  bash -n "$script" || exit
done
```

Runtime's separate `fixture-git-isolation` owns the raw Rust test constructors
and disposable foreign-repository regression. This card owns only the native
shell runner boundary, its process-level regression and validation guidance.
