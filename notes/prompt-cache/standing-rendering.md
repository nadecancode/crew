# D3 fresh-session rendering gate

This checkpoint measures native request structure, not provider cache savings.
No product prompt or adapter was changed. The accepted D3 revision remains the
scope: fresh sessions, no resume snapshot store or extra roster-update turns.

## Reproduction and provenance

The probe used Crew binary SHA-256
`c8ce44acb63a0ab9651ae513600f19dd098d8c9dd5adab19826498c53f6557db`
(the P4 build, with launch code from main), Claude Code **2.1.291** and Codex
**0.159.0**. Each case starts two fresh, identical-role workers in a new Git
fixture, with empty selected toolboxes, synthetic homes and dummy credentials.
Their native session ids differ. Worktree and shared-workspace modes are separate
controls. Every successful pair has byte-identical captured tool definitions.

`scripts/cache/standing_probe.py` reuses the reviewed toolbox loopback provider
and process cleanup. A Linux network namespace has only loopback. The
`anthropic-host` control sets the native endpoint hostname to
`http://api.anthropic.com` and routes HTTP through the same local synthetic
provider via a proxy. This exercises the CLI's hostname-dependent rendering;
it never reaches Anthropic, does not emulate a subscription entitlement, and
cannot establish provider-side cache eligibility or billing.

There are seven accepted pairs (14 fresh native sessions), zero paid provider
calls, and zero remaining owned processes. One candidate trial was rejected
because its extra hook duplicated `prompt.compose`; native validation rejected
the module and its policy was absent. A separate port-80 setup failed before any
native launch. Both remain in the attempt ledger and are not results.

`standing-rendering.json` contains request and catalog hashes, exact JSON
pointers and UTF-8 offsets inside native text, cache boundaries, CLI hashes and
cleanup results. It contains no prompt bodies or request headers. Raw synthetic
requests stay under `target/cache-standing-probe/`. Offsets describe the actual
text values the CLI emitted; JSON key order is not treated as model token order.

For one ordinary capture, from the repository checkout:

```sh
nix shell nixpkgs#python3 nixpkgs#bubblewrap --command \
  bwrap --unshare-user --unshare-net --bind / / --proc /proc --dev-bind /dev /dev \
  python3 scripts/cache/standing_probe.py \
  --crew target/features-p4/crew \
  --sha256 c8ce44acb63a0ab9651ae513600f19dd098d8c9dd5adab19826498c53f6557db \
  --runtime claude --isolation worktree --out target/cache-standing-probe/new-case
```

Use a new output directory for every capture. `--compose observe` records native
section ids/scopes/hashes while retaining the briefing. `--compose shared` is a
fixture-only placement experiment; **it is not a safe product fallback**. Add
`--endpoint-shape anthropic-host` for the hostname/proxy control. The shim validates
the generated plugin before exec, and the capture requires hook evidence.
`scripts/cache/standing_summary.py` derives the tracked, body-free report from
the named raw cases. Its text/UTF-8 evidence tests pass (2 cases).

## What the native requests show

| Capture | First differing Crew identity | Common coordination starts | Native cwd |
| --- | --- | --- | --- |
| Claude, worktrees, custom endpoint | `/system/2/text`, identity starts at byte 28,514; pair differs at 28,523 | Same text, byte 28,932 | First user context block, after system |
| Claude, shared workspace, custom endpoint | `/system/2/text`, identity starts at 28,510; pair differs at 28,519 | Same text, byte 28,928 | First user context block, after system |
| Codex, worktrees | `/input/2/content/0/text`, identity at 0; pair differs at 9 | Same text, byte 418 | `/input/2/content/2/text`, after Crew text but within the same developer message |
| Codex, shared workspace | Same pointer/offsets as worktree control | Byte 418 | Same later developer content block; identical across this pair |

The originally suspected cwd-before-Crew obstruction is **not observed** in these
cases. That does not by itself make reordering profitable: a byte-identical
substring needs a reusable cache boundary.

On the custom Claude endpoint, native system block 2 contains both the native
instructions and Crew's member-specific briefing. Its cache marker is at the
end of that whole block. Moving words within it does not create another marked
prefix. Anthropic's documented lookup works at content-block boundaries and
finds prior writes; it does not create a cache entry at an arbitrary common
substring. [Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)

Codex emits Crew text, permissions and cwd as several text parts in one developer
message. No explicit cache breakpoint was observed in that request. Current
OpenAI guidance for GPT-5.6 and later describes implicit lookups at message ends,
including the initial developer block. Therefore rearranging the Crew string
alone does not prove another reusable boundary in this emitted message. The
session-specific `prompt_cache_key` was recorded as a hash, not treated as proof
that reuse is impossible. [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)

## Claude's qualified shared-section seam

The CLI-generated **2.1.291** SDK (`c05419d7…c07ce`) declares
`PromptComposeSection.scope` as `shared` or `session`. It permits composition
hooks to place public, invariant sections before the shared/session boundary.
The declarations label this API early access. Their full hash is in the report.

The native hostname control confirms the qualification:

| First-party hostname control | Globally scoped system block | Session block |
| --- | --- | --- |
| Original briefing, observe-only hook | 11,570 bytes, identical across the pair | 24,625 bytes, includes Crew identity and coordination |
| Fixture moves public worker coordination into `shared` | 16,979 bytes, identical across the pair | 19,216 bytes, retains member identity |

The candidate's 5,407-byte policy segment contributes 5,409 bytes including its
separator. The tool catalog also matches across these two controls. The native
billing-header text differs between members before these blocks; the report
retains that difference rather than claiming the whole wire prefix is identical.
This is evidence of a larger identical, globally scoped block, **not measured
cache-read gain**. On the custom endpoint the same hook moves the text earlier
but the CLI still sends one variable block.

The experiment removes the policy from the append file before the native hook
loads. The rejected trial demonstrates why that cannot ship: hook failure can
silently omit the policy. Retaining the original append text avoids omission but
duplicates the policy and does not remove the original uncached portion. No such
fallback is claimed to pass the 10% non-read reduction or cost gate.

An additional bundled internal, `CLAUDE_CODE_APPEND_PROMPT_HEAD`, accepts a
`v1:<UTF-8 length>:<SHA-256>` designator, verifies the append prefix and otherwise
leaves it intact. The 2.1.291 verifier first requires `CLAUDE_CODE_REMOTE`.
Ordinary Crew CLI members do not meet that requirement. This probe did **not**
set the remote-ownership flag, modify vendor code, or use the hint. It is not a
supported ordinary-launch solution. Depending on a private hint would separately
need operator approval and the exact-version, invariant-content/fallback and
measured-benefit gates recorded in reviewer message m-451fa3031174.

## Decision requested before implementation

Do not implement a text-only reorder on these results and claim cache savings.
The probe establishes a qualified Claude shared-block opportunity, but no
approved ordinary-launch mechanism that both avoids duplication and preserves
all policy if a composition hook is unavailable. Codex's string-only seam does
not establish a new boundary either.

Recommend a documented D3 no-op for the present ordinary CLI launch paths,
pending a public append-prefix/boundary facility or a separately reviewed
required-module failure contract. This is narrower than saying common-prefix
reuse is impossible. The existing resume controls and their no-miss result stay
unchanged. If a viable candidate is chosen instead, freeze the three matched
fresh-session pairs and peer priming before paid runs, then apply the already
approved native usage and +5% whole-run cost gates. The structural captures above
cannot replace them.
