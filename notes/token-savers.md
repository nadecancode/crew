# Token-saving output filters

Work in progress for `token-savers-research`, 2026-10-08. This is an offline
evaluation, not approval to enable a hook or change a member's launcher.
The remaining work is the alternatives' implementation audit, the final
packaging recommendation and the reproducible measurement artifact.

## Findings established so far

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
| [LeanCTX](https://github.com/yvgude/lean-ctx/tree/32d72de1f503646deefdb009227333c8c97bba5d) | `32d72de1f503646deefdb009227333c8c97bba5d` | Context gateway, reads/recovery, hooks and an optional request proxy; path-level license matrix needs inspection. |
| [tokf](https://github.com/mpecan/tokf/tree/87c93d9ff3340bdbb0503b2addd0e7e8e8753cad) | `87c93d9ff3340bdbb0503b2addd0e7e8e8753cad` | TOML command filters and raw-output history; Rust/MIT. |
| [Tamp](https://github.com/sliday/tamp/tree/a50aeb7cb5599f92cba333b9ee10b1fbb1d6f5fa) | `a50aeb7cb5599f92cba333b9ee10b1fbb1d6f5fa` | API request proxy, including stateful history transformations; package metadata says MIT, notice audit pending. |
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
Each filter ran twice in separate processes on identical input. The measured
streams were deterministic in these repetitions.

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

## Integration constraints to carry into the recommendation

- A filter must not grant permission or broaden the command's authority.
- A failure, warning, unrecognized format or explicitly machine-readable
  request needs a raw escape. A compact diff is not a patch.
- Summaries need explicit labels, source coordinates and bounded raw recovery.
  They must not replace an edit-oriented Read while claiming the original
  tool's line metadata.
- RTK 0.51.0 requires Rust 1.91; crew's default CLI gate is Rust 1.82. An
  external pinned helper avoids moving that compiler gate. Its
  `serde_json/preserve_order` dependency also merits care: linking a whole
  tool can change workspace feature resolution and invalidate assumptions
  about canonical JSON ordering.
- Filter configuration, binary and rules need a stable member-scoped
  snapshot. Automatic discovery of mutable project configuration and plugin
  execution require separate decisions, not an implicit hook install.
- Output-size reduction cannot establish lower spend. The cache benchmark
  must count raw recovery, repairs, extra requests, cache reads/writes and
  task correctness on both arms.

## Remaining work

Finish the source and maintenance comparison for all eight projects; test
warning/format fallbacks and metadata loss; package a bounded reproducer;
settle the bundling/default policy, acceptance cases and implementation split.
