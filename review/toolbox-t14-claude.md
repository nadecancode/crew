# Claude toolbox refresh after resume

Claude Code now receives refreshed managed MCP schemas after an explicit
`crew resume --restart <member>`, retaining its conversation and history. The
native regression also crashes that process and verifies that recovery keeps
the refreshed schemas rather than restoring the old ones.

## Diagnosis and implementation

The earlier dispatch fix (`4adfa536`) refreshed Crew's pin, definition digest and
event correctly. Claude Code 2.1.291 nevertheless reused tool definitions from
the transcript's `prompt_snapshot` attachment. The native request still carried
the old descriptions. Disabling the MCP discovery cache did not change this.

Claude's supported `--system-prompt-snapshot off` switch bypasses the recorded
prompt, including its tool schemas. It works when applied to a previously
started conversation. Applying it just once is insufficient: the next ordinary
launch without the flag restores the original recorded schemas. The CLI
documents the switch from version 2.1.257. See the
[resumed-prompt reference](https://code.claude.com/docs/en/cli-reference#system-prompt-flags-in-resumed-conversations).

Production change `de7156fe` therefore:

- Records the invalidated Claude session id in daemon-private `AgentLocal` only
  when an explicit resumed refresh changes the managed fingerprint or digest.
- Supplies the switch on that launch and every later launch of that same SID,
  including crash recovery. A new SID clears the marker. Unchanged refreshes
  retain the default snapshot policy.
- Checks the native CLI version, bounded and in the member's environment and
  sandbox, before an explicit managed refresh can change the old MCP pins.
  Old or unknown versions show unavailable toolbox items and stop that refresh.
  No unsupported flag is latched. Ordinary recovery is independent of this
  capability probe, including when a version query would return `unknown`.
- Preserves the inherited-catalog guard. It does not edit transcripts, change
  the proxy, alter MCP tool names or create a replacement native conversation.

The compatibility guard is conservative: an old or unrecognized CLI cannot
request an explicit managed refresh even if its upstream catalog would happen
to be unchanged. It can still resume normally or start a fresh conversation.

## Native regression and evidence

The base fixture files under `scripts/toolboxes/` are copied exactly from QA's
`6ee49a08` (`native.py`, `provider.py`, `test_native.py`, `test_provider.py`). The
new `claude_refresh.py` extends that fixture with an unchanged explicit restart,
a real SIGKILL of the identity-checked native process, automatic recovery and a
later unchanged request. Every phase waits for an actual assistant reply in
the exact owned native transcript. The original transcript prefix must remain
byte-identical.

The same runner was executed against the old dispatch-only binary and the
candidate. Both use Claude Code 2.1.291, executable SHA-256
`078fad28d0297c9a25d306b635b2d8816c6839347520f29eb54ffea5d56142fb`.
The portable result is [native.json](toolbox-t14-claude/native.json).

| Check | Before (`4adfa536`) | After (`de7156fe`) |
| --- | --- | --- |
| T14: explicit refresh reaches native catalog | Failed: old descriptions | Passed: changed descriptions |
| T18: removal and historical call retained | Passed | Passed |
| Automatic recovery uses refreshed schemas | Failed | Passed |
| Snapshot bypass appears in refresh and recovery argv | Absent | Present |
| Initial and unchanged-restart snapshot policy | Unchanged | Unchanged |
| Conversation SID and original transcript prefix | Retained | Retained |
| Repeated post-refresh system/tool bytes | Stable | Stable |
| Cleanup | Stop 0, no remaining owned processes | Stop 0, no remaining owned processes |

The observed stable bytes are a property of this controlled fixture. Snapshot
bypass regenerates the system prompt per request on that SID; other workloads
can change its bytes and cache reuse. It does not disable provider prompt
caching. These localhost replies use dummy credentials and synthetic usage;
they make no cache-hit, pricing or savings claim. `cache-acceptance` owns those
measurements. The behavior and cost boundary are also documented in
[the launch notes](../notes/toolbox-launch.md).

## Validation

Logs remain in graph's `target/toolbox-t14-claude/`.

| Gate | Result | Evidence |
| --- | --- | --- |
| Toolbox launch, three runtimes and compatibility negative | 12 passed | `toolbox-launch-policy.log` |
| Team integration | 89 passed | `team.log` |
| Toolbox library tests, including version boundary and inherited pins | 24 passed | `toolbox-lib.log` |
| Python fixture receipt/provider checks | 4 passed | `python-tests.log` |
| Strict workspace Clippy, all targets | Passed | `clippy.log` |
| Formatting | Passed | `fmt.log` |
| Rust 1.82 default CLI and help | Passed | `msrv.log` |
| Native before/after | Expected failure / pass | `native-before-final/`, `native-fixed-final/` |

The launch suite checks actual resumed arguments for Claude, Codex and pi. Its
Claude case forces the version probe to return `unknown` before automatic
recovery and still recovers with the latched flag. A separate old-version case
refuses explicit refresh, then resumes with the old pin and can start fresh.

## Reproduction

Use the repository's short absolute target path, `socat` and bubblewrap for the
sandbox tests. No provider call is needed.

```sh
export CARGO_TARGET_DIR="$PWD/target/t"
export CARGO_BUILD_JOBS=4
nix shell nixpkgs#socat nixpkgs#gcc nixpkgs#pkg-config --command \
  env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH -u CREW_PI \
  CREW_TEST_SANDBOX=require cargo test --locked -p crew --test toolbox_launch
nix shell nixpkgs#socat nixpkgs#gcc nixpkgs#pkg-config --command \
  env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH -u CREW_PI \
  CREW_TEST_SANDBOX=require cargo test --locked -p crew --test team
./scripts/build-env cargo test --locked -p crew --lib toolbox
./scripts/build-env cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
CREW_MSRV_TARGET_DIR="$PWD/target/msrv" ./scripts/check-msrv
nix shell nixpkgs#python3 --command python3 -B -m unittest discover \
  -s scripts/toolboxes -p 'test_*.py'
nix shell nixpkgs#python3 --command python3 -B scripts/toolboxes/claude_refresh.py \
  --crew target/toolbox-t14-claude/crew-de7156fe \
  --sha256 b42a54585a67dc312c3efe0a50ea18ff7ffd057e59655fa67ad183f1d5c5e5d0 \
  --out target/toolbox-t14-claude/reproduction
```

For the negative control, substitute `target/toolbox-t14/crew-4adfa536` and its
SHA-256 `5bfdffac042c41a8c4252f1cc23fa2404979903853a70fdc94d404d1687c50b1`, using a
different new output directory. The runner refuses an existing output directory
or a mismatched binary hash.
