# Compact CLI native acceptance

These offline probes check the opt-in `compact-cli` skill on Claude Code
2.1.291, Codex 0.159.0 and pi 1.1.0. They are correctness and isolation
fixtures, not model-usage or token-savings measurements.

Build crew, install the pinned disposable pi package if needed, then run:

```sh
node notes/toolbox-probes/install-pi.mjs
node notes/compact-cli-probes/run.mjs \
  --crew target/cw/debug/crew --scratch target/compact-native
# Optional: --runtime claude, --runtime codex, or --runtime pi
```

Each run requires a new direct child of this checkout's `target/`. Nothing
overwrites an earlier run. Node 22.19 or newer and Linux bubblewrap are
required; the recorded run uses Node 24.19.0. `COMPACT_CLAUDE`,
`COMPACT_CODEX` and `COMPACT_PI` can select exact native executable paths;
their reported versions must match the pins. The report records the actual
binary hashes. Only the pi install step downloads packages.

The runner starts a private network and PID namespace. Native requests go
only to the localhost streaming stub in `notes/btw-probes/stub.mjs`; there
is no route to a provider. All child environments are allowlisted, with
fresh homes and placeholder keys. The host filesystem is read-only except
the new scratch directory and its private `/tmp`. The fixture daemon and
every native child live in that namespace and are retired at the end.
No operator credential or native configuration is copied or modified.

A real Crew daemon first materializes the built-in skill and its pinned
helper using a recorded member CLI. The three real native runtimes then
load those same skill bytes through their supported skill mechanisms.
Codex CLI discovery uses a copy under its private `.agents/skills`; Crew's
actual app-server root setup and sandbox mounts are separately covered by
`crates/crew/tests/toolbox_launch.rs`.

Each runtime has baseline and selected arms with the same permission policy:

- Claude `dontAsk` refuses an allowed listing followed by a disallowed write.
  Native Read returns all 150 requested lines, with original `numLines`,
  `startLine`, `totalLines` and exact file content. The selected skill adds no
  hook or permission rule.
- Codex `workspace-write` refuses the outside write and also refuses the
  helper's daemon socket. Explicit helper invocation remains denied; product
  availability reports this combination as unavailable. A separate pair
  with existing `danger-full-access` settings verifies successful helper use.
  That pair's write canary is on the fixture's read-only mount, not a claim
  that Codex's unrestricted mode supplies write protection. Codex exposes
  file reads through its shell, so the selected 150 lines are compared to
  the exact expected bytes after removing the shell's execution envelope.
- Pi has no Claude-style permission mode. Both arms install the same test
  guard extension; its denial survives skill loading. Native `read` stays
  byte-identical. The product bundle installs no extension or guard.

Successful arms call the real output helper and recover the original full
log exactly, without executing a producer. Every native session resumes
with its earlier tool results unchanged and its existing transcript bytes
preserved as a prefix. The helper's output and frozen skill bytes also remain
stable across a clock tick and member restart. Ordinary source reads remain
native in every arm.

`report.json` contains assertions, versions and hashes. `requests.json`,
native transcripts and per-command stdout/stderr stay in ignored scratch;
they contain synthetic inputs plus native standing instructions and are not
publication artifacts. An assertion failure, wrong version, timeout or
signal fails the probe. The compact committed evidence identifies the exact
successful runs and any earlier failed attempts separately.
