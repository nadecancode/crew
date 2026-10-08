# Captured output views

`crew output` is an explicit member-side view of an already captured tool
result. It never runs the command, changes its approval, installs a hook,
rewrites history or replaces a file read. Toolbox packaging is a separate
step; nothing is enabled automatically.

The rules are compiled into crew (`format_version: 1`, `cargo_test_v1`),
with no project-local rule discovery or separate mutable rules file. A
toolbox must fingerprint its pinned crew binary, these format/parser ids and
the exact frozen skill bytes; the binary digest covers parser and store code.

Pass the outcome recorded by the original caller:

```sh
crew output view target/test.stdout --stderr target/test.stderr --exit-code 0
crew output view target/failed.log --exit-code 101
crew output view target/killed.log --signal 9 --timed-out
crew output view target/records.jsonl --exit-code 0 --input-format machine
```

`CREW_IDENTITY` identifies the active member; `--identity FILE` is an explicit
alternative. There is no operator, member-selector or store-path override.
Every invocation authenticates the identity against the daemon before
reading captured input or the store. The daemon must be reachable, including
for raw recovery. Helper exit status reports whether this view/recovery
worked; the original exit code, signal and timeout are separate structured
`original_status` data and are never inferred from the log's wording.

The JSON response has `format_version: 1`, a `kind` of `summary`, `raw` or
`unavailable`, exact half-open source byte ranges, and a stable content
reference. A cargo success summary includes suites, passed, failed, ignored,
measured and filtered-out counts. It explicitly omits test names, ignored
reasons, build paths and timing. Get the raw evidence before relying on an
individual test name or ignored reason.

Only complete recognized libtest output with at least one passing test and
a reported successful exit can become a summary. The parser checks suite
and per-test counts, treats the two streams independently, and rejects every
unknown line. Warnings, failures, partial/unknown output, zero executed tests,
machine formats, binary data and terminal controls stay raw. Empty suites
within an otherwise exercised run remain in the suite count. ANSI stripping
and quiet/nextest grammars are deliberately unsupported in format 1.

A raw view includes at most 16 KiB from each stream, with exact byte ranges,
an explicit `truncated` flag and UTF-8 or base64 encoding. Those previews
are not full output when truncated. Both full streams are retained before a
summary can be emitted. Recover a stream without running anything again:

```sh
crew output raw sha256:<digest> --stream stdout --offset 0 --length 65536 > recovered.stdout
crew output raw sha256:<digest> --stream stderr --offset 0 --length 65536 > recovered.stderr
```

Raw recovery writes only the requested exact bytes to stdout. JSON metadata
on stderr records original status, source stream, returned range and
`next_offset`; repeat at that offset while it is non-null. Chunks are at
most 1 MiB. Invalid, expired, evicted, corrupted or foreign references fail
explicitly and never rerun the producer. A source/store failure retains the
original status in the view error response when source processing has begun;
authentication/argument failures admit no source and produce no view.
The view's `recovery` array contains arguments for the same crew executable,
not a shell command or a request to look up a different binary on PATH.
Toolbox packaging must prepend its pinned crew executable to those arguments.

## Storage and access

The private store is under the validated member identity directory at
`rw/output-v1`. Its namespace binds daemon socket, member id and credential
epoch. It survives member/daemon restart with that identity; a removed or
revoked identity cannot recover output, and a replacement credential does
not resolve the old namespace. Content references do not grant access and
contain no member id, private namespace, source path or capture timestamp.
Identical bytes/status produce identical view bytes across processes,
private directories and capture times.

Directory-descriptor-relative opens reject symlink traversal, special files,
wrong owners, nonprivate modes and artifact hard links. Directories use
0700 and files 0600. Raw files are immutable, atomically published, bounded
and integrity checked before recovery. A nonblocking store lock coordinates
concurrent helpers. Interrupted pending files are cleaned on the next
operation; a busy or unsafe store produces no compact success summary.

Limits: 4 MiB total captured stdout plus stderr; 128 artifacts and 32 MiB
across the member's retained credential namespaces; seven days from artifact
creation. Expiry/oldest-first eviction runs on access. Inactive stores do
not run a background timer. Oversized source files are refused unchanged,
not silently truncated or deleted; use the original file through its normal
authorized read. A source must be a regular captured file, not stdin, a pipe
or a device. Store eviction deletes only this helper's managed artifacts.

Supply the bytes **after the original tool's redaction policy**. The helper
preserves them exactly, including existing redaction placeholders. It does
not discover credentials, reverse redaction, fetch another tool's private
raw output, or promise secret detection. It runs with the caller's existing
sandbox and environment rights. Files may still contain secrets the caller
was already allowed to see. 0600 is not process isolation between users of
the same Unix UID: unsandboxed processes retain their ambient filesystem
authority. Member isolation still depends on the existing sandbox mounts.

## Validation

`crew-core/tests/output_view.rs` uses decoded fixtures from
`notes/token-savers/corpus.json.gz` and adversarial grammar/status cases.
Store tests exercise identity namespaces, byte integrity, truncation,
expiry/eviction, links, path replacement and interrupted publication.
`crew/tests/output_cli.rs` exercises the real CLI with authenticated daemon
identities, exact chunk recovery, restart, revocation and failure fallback.
These are deterministic correctness checks, not native model savings data.
