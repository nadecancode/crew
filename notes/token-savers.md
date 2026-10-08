# Token-saving output filters

Evaluation for `token-savers-research`, 2026-10-08. Bundle a small,
deterministic crew output-view helper with a member-scoped, opt-in skill.
Start with recognized successful test summaries, retaining raw output and
the real exit status. Keep ordinary file reads, patches, warnings, failures
and unknown formats raw. No upstream tool examined here is suitable for
unconditional installation into every member.

Use RTK's pinned binary as an optional comparison target while implementing
the helper. Its compact successful cargo output is promising, but warning
and unknown-format probes below prevent making that adapter the default.
Do not vendor an entire filter application or enable its automatic hooks.
This card changes documentation and offline research artifacts only.

## Findings

Do not install an output filter's hooks automatically. lowfat 0.8.1's Bash
hook returns `permissionDecision: "allow"` when it rewrites a recognized
command. In an isolated Claude Code 2.1.291 probe, `dontAsk` refused
`ls … && touch <fixture>`. Installing the lowfat hook made the fixture write
succeed. The hook changed authorization as well as output. The native probe
used a localhost model stub, disposable configuration and a fixture file;
it contacted no provider. [lowfat's release hook](https://github.com/zdk/lowfat/blob/a17b3e0602c954a8160abc9086085083ad2405d5/crates/lowfat/src/commands/hook.rs)

Automatic replacement of native file reads is also unsuitable for edits.
lowfat's `post-read` uses the supported `updatedToolOutput` interface, so it
does replace what Claude receives. Reading 150 lines of this repository's
Cargo.lock changed the model-visible result from 4,713 to 272 JSON-encoded
bytes. The replacement summarized 15 packages and rewrote both `numLines`
and `totalLines` to 3; the source has 3,024 lines. Package versions and
checksums were absent. This is a summary, not a faithful file read.
[lowfat's release Read hook](https://github.com/zdk/lowfat/blob/a17b3e0602c954a8160abc9086085083ad2405d5/crates/lowfat/src/commands/post_read.rs),
[Claude Code hook output contract](https://code.claude.com/docs/en/hooks#posttooluse-decision-control)

tokf 0.2.54 masks child failure by default. A replay of an actual failing
crew test exited 101; `tokf run` returned 0 and put `Error: Exit code 101`
in its text. `--no-mask-exit-code` restored 101. Any integration must retain
exit status as data, not merely describe failure in prose.
[tokf CLI documentation](https://github.com/mpecan/tokf/blob/87c93d9ff3340bdbb0503b2addd0e7e8e8753cad/README.md)

## Pins and scope

The source survey pins all eight repositories. Release binaries can have
different source commits from the default branch; both are recorded here.
No installer, setup command or global hook was run.

| Project | Examined default-branch commit | Initial mechanism / scope |
| --- | --- | --- |
| [lowfat](https://github.com/zdk/lowfat/tree/660bc936cad22a3e71be40da269213640548bcc5) | `660bc936cad22a3e71be40da269213640548bcc5` | CLI filters, Bash rewrite, lossy Read replacement; Rust/Apache-2.0. |
| [RTK](https://github.com/rtk-ai/rtk/tree/8533612180c60efbcb5827c7db4e910aba096705) | `8533612180c60efbcb5827c7db4e910aba096705` | Command-specific CLI adapters and explicit file read levels; Rust/Apache-2.0. |
| [context-mode](https://github.com/mksglu/context-mode/tree/2a7a92ab00496a4848decdd7b651daf9ccd90d96) | `2a7a92ab00496a4848decdd7b651daf9ccd90d96` | Indexed context and retrieval through tools; ELv2, not the same scope as a CLI filter. |
| [LeanCTX](https://github.com/yvgude/lean-ctx/tree/32d72de1f503646deefdb009227333c8c97bba5d) | `32d72de1f503646deefdb009227333c8c97bba5d` | Context gateway, reads/recovery, hooks and an optional request proxy; public-host paths Apache-2.0 under its license matrix. |
| [tokf](https://github.com/mpecan/tokf/tree/87c93d9ff3340bdbb0503b2addd0e7e8e8753cad) | `87c93d9ff3340bdbb0503b2addd0e7e8e8753cad` | TOML command filters and raw-output history; Rust/MIT. |
| [Tamp](https://github.com/sliday/tamp/tree/a50aeb7cb5599f92cba333b9ee10b1fbb1d6f5fa) | `a50aeb7cb5599f92cba333b9ee10b1fbb1d6f5fa` | API request proxy, including stateful history transformations; package declares MIT, but no separate license text found in the pinned tree. |
| [ecotokens](https://github.com/hansipie/ecotokens/tree/62679e627fe9c527851ee59093d839c0b6f019b3) | `62679e627fe9c527851ee59093d839c0b6f019b3` | Shell/native-tool compression with heuristic and model-assisted paths; Rust/MIT. |
| [Token Enhancer](https://github.com/xelektron/token-enhancer/tree/7291cfe4cbd36d61eb0ae7736cb793686a692a3f) | `7291cfe4cbd36d61eb0ae7736cb793686a692a3f` | Web HTML extraction through a proxy/MCP, not a general command filter; Python/MIT. |

Measured binaries, downloaded into private scratch and checked against the
release API's SHA-256 digests:

| Binary | Source release commit | Archive SHA-256 |
| --- | --- | --- |
| lowfat 0.8.1, Linux x86_64 GNU | `a17b3e0602c954a8160abc9086085083ad2405d5` | `78f8e715646db15e41e807dccad0676f2826ab6ddecc85e6930c7b98e317ecad` |
| RTK 0.51.0, Linux x86_64 musl | `e001f773f80b22b7dc4c7a79521b30e35aaef026` | `5028d3b19a8f0990d30fec9fbb07e32782bc5698e618fb1861aad8a9ccba4eb5` |
| tokf 0.2.54, Linux x86_64 GNU | `87c93d9ff3340bdbb0503b2addd0e7e8e8753cad` | `50484ad571ef4a7b36386469bb2d07b7f019db1cc22e3895cc9ae8410f8c01f4` |

## Offline measurements

Counts use `js-tiktoken@1.0.21`, `cl100k_base`. They are a common output-token
proxy, not Claude/Codex/pi billing counters. Commands ran without network
access under bubblewrap, with writable storage confined to private scratch.
Each filter ran twice in separate processes on identical input. The 39
primary observations and two cargo adapters matched across those repetitions;
variable wrapper footers are tested separately below.

Inputs are repository files, the real D4 TUI fix diff, directory output and
actual local crew test logs. Command adapters replay the captured output and
exit status. The replay driver checks that its raw stdout equals the fixture;
it drains stdout before exiting. These are filter measurements, not agent
task-completion or end-to-end savings results.

| Input / mode | Raw tokens | Filtered tokens | Reduction | Information check |
| --- | ---: | ---: | ---: | --- |
| Cargo.lock / lowfat, every level | 28,061 | 67 | 99.76% | Versions and checksums absent. |
| Cargo.lock / RTK read, every level | 28,061 | 28,061 | 0% | Full content retained. |
| toolbox.rs / lowfat full | 17,193 | 17,193 | 0% | Below the hook's reduction threshold; passed through. |
| toolbox.rs / lowfat ultra | 17,193 | 2,849 | 83.43% | Summary; unsuitable as exact edit input. |
| toolbox.rs / RTK aggressive read | 17,193 | 3,467 | 79.83% | Summary; raw read still required for edits. |
| toolboxes.md / lowfat ultra | 20,395 | 4,233 | 79.24% | Lossy document view. |
| TUI fix diff / lowfat lite | 1,017 | 927 | 8.85% | Guard and regression code retained. |
| TUI fix diff / lowfat full | 1,017 | 759 | 25.37% | Guard and regression code retained; context removed. |
| TUI fix diff / lowfat ultra | 1,017 | 72 | 92.92% | Guard and regression code absent. |
| TUI fix diff / RTK diff | 1,017 | 718 | 29.40% | Guard and regression code retained. |
| TUI fix diff / tokf git-diff | 1,017 | 47 | 95.38% | Per-file counts replace the patch. |
| Long ls / lowfat full | 79 | 24 | 69.62% | Executable permission bits absent. |
| Successful cargo tests / tokf cargo-test | 3,955 | 76 | 98.08% | 198 passed, 2 ignored, 5 suites; individual test names omitted. |
| Successful cargo tests / RTK cargo test | 3,955 | 20 | 99.49% | Same totals; individual test names omitted. |
| Failing cargo test / tokf cargo-test | 282 | 169 | 40.07% | Panic cause and failing test name retained. |
| Failing cargo test / RTK cargo test | 282 | 149 | 47.16% | Panic cause retained; exit 101 preserved. |

The RTK cargo adapter and generic `rtk test` are different paths. The generic
adapter must not stand in for the cargo adapter in a comparison. Likewise,
`tokf apply --exit-code` selects a filter branch; it does not measure the
status of `tokf run`, which has the masking behavior described above.

Scratch evidence is in the reviewer's `target/token-savers-research/`:
`results.json`, `results.tsv`, `rtk-cargo-results.json`, `exit-probe.json`,
`native/report.json`, `native-read/report.json`, the pinned inputs and filter
outputs. Native request captures remain ignored and are not publication
artifacts.

## Comparison and maintenance

Runtime entries below describe examined upstream wiring, not crew launch
acceptance. Only lowfat's Claude hook paths were exercised natively here.
An explicit CLI invocation can run under any member that has a shell;
that does not prove native hook support or preserve that runtime's approval
rules. None of the eight repositories was archived in the recorded metadata.
Latest pinned commit dates show activity, not compatibility or maintenance
quality; exact metadata is in [pins.json](token-savers/pins.json).

| Project / last commit UTC | Runtime wiring and mechanism | Decision for crew |
| --- | --- | --- |
| lowfat / Oct 5 | Rust CLI and rule files; Claude Bash rewrite and Read replacement; shell integration can expose the CLI to Codex/pi. Rule files can invoke shell programs. | Do not install hooks. The native authorization change and misleading Read line metadata are blockers. Selected pure transformations would need their own contract and fixtures. |
| RTK / Oct 7 | Command adapters, explicit read levels and raw recall. Upstream documents Claude/Codex hooks and pi extensions; the benchmark uses only the CLI. | Best optional binary comparison target. Preserve its Apache notices; pin executable hash and rules. No default cargo adapter until warning/unknown-format failures are fixed. |
| context-mode / Oct 7 | Node MCP execution/index/search tools, local SQLite retrieval and session hooks. Source contains Codex capability detection and a pi extension entry. | Treat as a separate retrieval toolbox experiment. It changes how agents obtain data and adds tool/schema/instruction cost, so output-filter percentages do not compare directly. ELv2 has hosted-service limitations; retain it as an optional external integration rather than redistributing it inside crew. |
| LeanCTX / Oct 8 | Rust CLI/MCP, source references and expansion, hooks, pi extension and optional provider proxy. Stateful read caches can replace re-reads with references. | Useful recovery design reference; too broad for the initial helper. The public-host matrix identifies Apache paths and excludes separately delivered private components. Do not infer rights from the README's “Mixed” badge. |
| tokf / Sep 6 | Rust CLI and TOML filters, project/global overrides, raw history; Claude and Codex hook code. Explicit CLI works for pi; pi automatic integration was not verified. | Useful declarative-rule reference. Require `--no-mask-exit-code` for a binary experiment, disable telemetry, freeze rules and keep raw artifacts. No upstream default filter set in crew. |
| Tamp / Sep 1 | Node API proxy with Anthropic/OpenAI adapters, including Codex Responses routing; pi could use a compatible configured endpoint, but no native pi test here. Optional history, semantic and image transforms. | Do not bundle in this slice. It owns credentials and request history, a much larger boundary than filtering a new tool result. Resolve missing distribution license text before any packaging. |
| ecotokens / Oct 6 | Rust command/native-tool filters, Claude/Codex hooks and a generated pi extension. Optional Ollama summaries and other model-backed capabilities. | Keep external and opt-in. Source also emits `permissionDecision: allow`; that path was not independently tested natively. Disable model summarization in any deterministic-output experiment. |
| Token Enhancer / Apr 3 | Python HTML cleanup, URL fetching/cache and MCP tools; source also has an optional prompt optimizer. Any compatible MCP client could load it. | A web-retrieval toolbox candidate, not a command-output library. HTML cleanup removes forms, navigation and other potentially relevant material; retain source and provenance. No automatic prompt rewriting. |

The implementation sources behind these distinctions are
[context-mode's server](https://github.com/mksglu/context-mode/blob/2a7a92ab00496a4848decdd7b651daf9ccd90d96/src/server.ts),
[Codex capability detection](https://github.com/mksglu/context-mode/blob/2a7a92ab00496a4848decdd7b651daf9ccd90d96/hooks/core/codex-caps.mjs),
[LeanCTX's path-level license matrix](https://github.com/yvgude/lean-ctx/blob/32d72de1f503646deefdb009227333c8c97bba5d/LICENSE_MATRIX.toml),
[tokf's filter crate](https://github.com/mpecan/tokf/blob/87c93d9ff3340bdbb0503b2addd0e7e8e8753cad/crates/tokf-filter/Cargo.toml),
[Tamp's cache](https://github.com/sliday/tamp/blob/a50aeb7cb5599f92cba333b9ee10b1fbb1d6f5fa/lib/read-cache.js),
[ecotokens' hook](https://github.com/hansipie/ecotokens/blob/62679e627fe9c527851ee59093d839c0b6f019b3/src/hook/handler.rs), and
[Token Enhancer's HTML path](https://github.com/xelektron/token-enhancer/blob/7291cfe4cbd36d61eb0ae7736cb793686a692a3f/data_proxy.py).
The [ELv2 text](https://github.com/mksglu/context-mode/blob/2a7a92ab00496a4848decdd7b651daf9ccd90d96/LICENSE),
[LeanCTX public-host explanation](https://github.com/yvgude/lean-ctx/blob/32d72de1f503646deefdb009227333c8c97bba5d/LICENSE.md) and
[Tamp's package declaration](https://github.com/sliday/tamp/blob/a50aeb7cb5599f92cba333b9ee10b1fbb1d6f5fa/package.json)
are the license evidence; this survey does not grant rights to other versions
or bundled third-party dependencies.

Vendoring the whole applications would expand crew's dependency and compiler
boundary. RTK's measured release requires Rust 1.91 and enables
`serde_json/preserve_order`; tokf's workspace uses edition 2024 and its filter
crate enables Lua by default. LeanCTX's unpublished embedding facade requires
Rust 1.85 and pulls its engine with embeddings, HTTP and secure-update
features. crew's default CLI gate remains Rust 1.82. A pinned external binary
isolates that compiler boundary, but still needs platform builds, notices,
sandbox compatibility and upgrade tests.
[RTK release manifest](https://github.com/rtk-ai/rtk/blob/e001f773f80b22b7dc4c7a79521b30e35aaef026/Cargo.toml),
[tokf workspace](https://github.com/mpecan/tokf/blob/87c93d9ff3340bdbb0503b2addd0e7e8e8753cad/Cargo.toml),
[LeanCTX embedding manifest](https://github.com/yvgude/lean-ctx/blob/32d72de1f503646deefdb009227333c8c97bba5d/rust/crates/lean-ctx-embed/Cargo.toml).

## Edge cases and limits

Six small synthetic fixtures supplement the real corpus. Their inputs and
outputs are committed, clearly separated from the real command logs. They
check information preservation rather than representative average savings.

| Case | Observed result | Integration requirement |
| --- | --- | --- |
| A warning prepended to the successful cargo log | RTK and tokf produce their usual success summaries and omit the warning. | Preserve every warning or return raw. A successful exit alone cannot select a lossy filter. |
| 0 tests run, 198 filtered out | RTK retains “198 filtered out”; tokf omits that count. | Distinguish zero execution from a successful exercised test suite. |
| Unknown test-runner output, exit 0 | RTK emits empty stdout; tokf returns the input. | Unknown grammars return raw, never an empty success view. |
| Cargo machine-format record | RTK `cargo test --message-format=json` emits empty stdout; tokf's direct filter returns it unchanged. | Explicit machine formats bypass summarization. The tokf result is an `apply` test, not proof of its shell adapter's argument handling. |
| Mode-only diff | lowfat full retains only the diff header; lite retains the fixture. RTK says mode changed without the exact modes; tokf reports +0/-0. | Preserve exact modes in review views; a change must not look empty. |
| Rename-only diff | lowfat full drops rename metadata; lite retains it. RTK retains old/new names; tokf shows only the destination and +0/-0. | Preserve old/new paths and rename/copy meaning. |

RTK's failure recall returned the exact 1,025 raw bytes, without rerunning the
command. Its failure reference was stable for identical bytes. lowfat's
failure path preserved exit 101 but emitted a timestamp-bearing raw-file
path on stderr: repeated identical input produced different model-visible
stderr. Append-once history can still cache that result, but the varying
footer defeats identical-output comparisons and must not be regenerated in
old history. Use stable content references in crew's view.

The measured 98–99% reductions for successful cargo logs are reductions of
one output stream. The model may need omitted test names, ignored-test
reasons or source details later. Recovering the whole raw log adds a request
and can consume more total tokens than sending it once. The other five
projects were source-audited, not run through artificial “equivalent” filter
benchmarks: retrieval, HTML extraction and request proxies need different
correctness tasks. No provider model calls, global installs, shell init or
native user configuration changes were made here.

## Packaging and defaults

The first implementation should be a small crew-owned parser and renderer,
with no new heavyweight crate. Package it as a CLI output view and a fixed
skill in a `compact-cli` toolbox bundle. An explicit invocation reads an
already captured artifact; later execution wrappers require separate
approval-equivalence tests. Do not transparently rewrite Bash or replace
native Read. Exact file bytes remain available to the agent doing an edit.

| Command/data class | Default for a member without the bundle | Behavior inside an opted-in bundle |
| --- | --- | --- |
| Recognized successful cargo test output, no warnings | Raw | Compact counts including suites, ignored and filtered-out tests; raw artifact reference. |
| Failure, warning, malformed/unknown output, no tests executed | Raw | Raw, with original exit/signal and bounded explicit retrieval for large output. |
| JSON, JSONL, NUL-delimited records, patches or binary output | Raw | Raw; no normalization of a machine format. |
| Edit-oriented file Read, lockfile versions/checksums | Exact native read | Exact native read. A separately named outline is optional and cannot claim original line metadata. |
| Diff, search and directory inspection | Raw | Explicit summary only; retain modes, paths, match locations and truncation counts; use raw for review/patch application. |
| HTML/retrieval or model-generated summaries | Existing tool | Separate opt-in toolbox experiment; no claim of equivalence with command filters. |

Keep binaries, rules and instruction bytes immutable for each member's
active toolbox. Include their hashes and filter format version in its
fingerprint; restart/reload applies a change. No project-local filter
discovery, dynamic tool schemas or per-turn savings counters in the standing
prompt. MCP exposure, if later added, has a fixed tool set and uses crew's
existing pinned catalog; do not add a server per command or mutate the global
crew tool list at runtime. Claude hooks, Codex hooks and pi extensions have
different native contracts: only enable an adapter after its installed
version passes the corresponding acceptance probe.

Run any helper inside the member's existing sandbox and environment policy.
Give it no model credentials or broader filesystem/network rights. Scope
raw output storage and retrieval to the originating run/member, with bounded
size and retention. A raw artifact can contain secrets; retain the same
redaction and access boundaries as the original tool result. Stable raw
references must not expose another member's artifacts, absolute operator
home paths or timestamps in model-visible summaries. A content digest and
explicit source ranges distinguish a summary from exact source evidence.

For external experiments, pin RTK 0.51.0 by executable hash as an optional
helper with its license. tokf needs an isolated configuration and
`--no-mask-exit-code`; disable Lua/custom rule execution unless separately
selected. Neither becomes a default dependency in this card. Exclude
lowfat's automatic hooks, request-history proxies, model summarizers and
mutable runtime installers from the first bundle.

## Acceptance and cache benchmark

Offline correctness gates precede any model benchmark:

1. Preserve exit codes, signals, timeout status, stderr warnings and all
   unrecognized lines. A filter failure returns raw data and the child's
   original status. No success summary for a zero-test or unknown format.
2. Reproduce the lowfat permission fixture with an allowed listing followed
   by a disallowed write. Enabling a crew view must not change permission
   decisions, run an extra command, or bypass a native denial.
3. Keep exact Read bytes/line metadata for edits, source modes and both
   rename paths. A summary must name itself and provide exact raw recovery.
4. Repeat identical raw bytes/config in separate processes, after a clock
   change, after member restart and in different private directories.
   Summary bytes and content references must match; previously appended
   transcript bytes must never be rewritten.
5. Test raw retrieval after restart, expiry, truncation and rejected
   cross-member access. Do not rerun a side-effecting command to recover its
   output. Exercise timeout/oversize/malformed input and unavailable helper.
6. Check compiler, binary size and each supported sandbox/runtime. An
   unsupported adapter is explicitly unavailable; it cannot silently claim
   filtering. No global configuration or provider-endpoint changes.

Extend `scripts/cache/bench.py` with a paired output-view scenario after the
helper exists. Use the same crew build, native runtime/model, member toolbox,
standing prompt, workload and native cache policy in both arms; vary only
filter enablement. Seed identical warm prefixes, alternate arm order and
run at least three pairs, including follow-up requests that require a raw
recovery and an exact edit. Include successful/failed tests, warnings,
metadata-only diffs, machine output and a source-version/checksum lookup.
Count task correctness independently from compressed output size.

Collect native request/source ids and usage with the existing collector:
input total, cache reads, cache writes with TTL coverage, output tokens,
request count and latency. Add helper time, raw-recovery requests and repair
turns. Use per-metric coverage: absent write fields remain unknown, not zero;
unknown write TTL produces a price range. Compare tokens by runtime/model
and input-side/full priced costs only where prices and coverage support it.
Codex subscription usage is not an invoice; tokenizer estimates and a
filter's advertised “saved tokens” are not native usage.

Pass only when all correctness cases succeed and the enabled arm reduces
end-to-end measured usage/cost including recoveries and repairs. A pricing
range that overlaps the control is inconclusive. Report spread across pairs
and cold/warm requests separately. Cache-hit rate alone cannot pass or fail
the experiment: reducing input can change its denominator, while one extra
reasoning turn can erase an output-size saving. This research has not run
that paid/native model comparison and makes no end-to-end savings claim.

## Implementation split

| Slice | Files / boundary | Deliverable |
| --- | --- | --- |
| Output-view contract | Proposed `crates/crew-core/src/output_view.rs` | Pure parsing/rendering, explicit summary/raw provenance, format version, warning and unknown-line fallback; corpus regressions. |
| Scoped artifacts and CLI | Proposed `crates/crew/src/output/`, CLI dispatch and tests | Bounded per-member raw store, exact recovery, `crew output view`; later execution wrapper separately proves exit and approval equivalence. |
| Toolbox packaging | `agents/toolbox/`, existing skill materialization and launcher tests | Optional `compact-cli` skill, immutable helper/rules, visible availability; no default hook installation. Coordinate with toolbox-launch owner. |
| Native acceptance | Offline runtime probes alongside existing `notes/*-probes/` conventions | Claude/Codex/pi-specific permission, Read and history stability checks, pinned versions, no provider calls. |
| Net-savings decision | `scripts/cache/bench.py`, native usage collector, `notes/prompt-cache/` | Paired measured scenario, correctness review, raw-recovery costs and coverage before any default rollout. |

## Reproducing this research

[Reproducer instructions](token-savers/README.md) describe the Linux offline
runner. [Evidence](token-savers/evidence/results.json) includes 39 primary
observations, [cargo adapter results](token-savers/evidence/rtk-cargo-results.json)
add two, and [edge cases](token-savers/evidence/edge-results.json) add 16.
Inputs, hashes, exact upstream rule files and their license notices are
committed. The native fixture uses a localhost model stub and fresh private
configuration; only its compact [result](token-savers/evidence/native-summary.json)
is committed, not complete native request captures.

The packaged runner independently reproduced all 57 observations and the
native permission/Read findings on Node 24.19.0 and Claude Code 2.1.291.
Evidence confirms the specific pinned builds on this Linux host; it does not
certify other operating systems, all upstream command adapters, or future
releases.
