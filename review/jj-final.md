# Final review: Jujutsu support

Reviewed integration: `a89f194a81e426a3025b90f50713c29f278c6539` (`agents/qa`).
The review worktree matched that commit's complete tree before this report.

Verdict: approved. No blocking findings remain.

The implementation matches `notes/jj-design.md`: jj 0.44 is the default with
the documented Git fallback; hidden repository state preserves the user's
checkout; agent work is exported as Git branches; conflicts restore the
workspace; and validated revisions are passed as lookups rather than raw
revsets. The sandbox mounts and Landlock REFER rule match the amended design.
VCS events drive the TUI's write, read, conflict and running animations, and
the idle redraw checks pass.

## Findings resolved

- VCS serialization now lasts for the blocking job after a request is
  cancelled.
- All publishers report refused Git exports. Integration publishes completed
  work after both final-export and intermediate-lookup timeouts.
- Compact layouts show hub animations. Removing an agent or receiving a late
  terminal event clears its running animation.
- Runtime handoff resolves the backend before taking the model lock, avoiding
  a deadlock when the backend cache is empty after daemon restoration.
- Real status, diff and failed-commit RPC events are tested through the TUI
  reducer on both backends.

## Independent verification

On integration `e6bcf656`, each complete workspace suite passed 764 tests,
with 2 optional tests ignored:

- Default environment without jj, exercising automatic Git fallback.
- `CREW_TEST_VCS=jj` and `CREW_TEST_SANDBOX=require`, with real jj 0.44.0,
  bubblewrap and socat supplied by Nix.
- `CREW_TEST_VCS=git` and `CREW_TEST_SANDBOX=require`, with real sandboxes.

The final delta in `a89f194a` contains the handoff lock correction and its
regression tests. On that exact tree, the lazy-backend handoff test and the
real RPC-to-TUI event test passed; the latter passed separately on Git and jj.
`cargo clippy --workspace --all-targets -- -D warnings` also passed.

`cargo build --release -p crew` passed on the final tree.

The optional live provider CLI and interactive TUI tests remain ignored.
Scripted-agent, rendering, idle-timing and real sandbox tests supplied the
automated coverage; no claim is made about an additional live provider run.
