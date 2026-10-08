---
name: compact-cli
description: View already captured cargo test output as a compact, versioned summary with exact raw recovery. Use for long successful test logs, not source reads or running commands.
---

# Captured test output

Use this skill's `scripts/output` to invoke the pinned `crew output` helper.
It reads an existing captured file; it never runs the producing command.

```sh
scripts/output view target/tests.stdout --stderr target/tests.stderr --exit-code 0
```

Resolve the script relative to this skill's directory. Supply the exit code
actually recorded by the original caller, or `--signal N` / `--timed-out`.
Do not infer success from log text. The helper's exit code describes the view
operation; `original_status` describes the test run. Input must already have
passed the original tool's redaction policy.

Only complete recognized successful cargo test output can become a summary.
Warnings, failures, zero executed tests and unknown formats remain raw.
Use `--input-format machine` for JSON, JSONL or other machine output; use
`--input-format raw` when an exact view is needed. Source files, lockfiles,
patches and edit evidence continue through native Read and ordinary tools.

Summary counts omit individual test names, ignored reasons, paths and timing.
Before relying on omitted detail, recover the existing artifact:

```sh
scripts/output raw sha256:<digest> --stream stdout --offset 0 --length 65536
```

Raw recovery writes exact bytes to stdout and range/status metadata to stderr.
Follow `next_offset` until it is null for the full stream; recover stderr
separately. A raw preview with `truncated: true` is not the whole log. The
view's `recovery` array starts with `output`; pass its remaining arguments to
this script. Never rerun a command to recover output.

Artifacts are private to the authenticated member and expire or may be
evicted. If the helper or artifact is unavailable, use the original captured
file through its normal authorized read. A changed helper requires toolbox
restart/reload; do not fall back to another `crew` on PATH. No extra permission,
hook, native Read replacement, history rewrite or model call is provided.
