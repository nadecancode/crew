# Member toolbox launch implementation

The runtime launch slice applies the resolved toolbox before a member starts.
It snapshots selected files into the immutable content store, renders the
runtime's discovery inputs, preflights and pins MCP definitions, and reports
unsupported or failed items through `Agent.toolbox`. Reload changes the next
launch's selection and leaves the running member's files and definitions alone.
`toolbox.applied` and `toolbox.changed` are emitted after the process starts.

MCP catalogs are also copied into the immutable store when the parent pins
them. `ActiveToolbox.mcp_snapshots` carries those references through default and
selected-bundle subagent inheritance, before agent creation can await. A child
restores its catalog from those immutable bytes and reuses its own identical
pin on resume. Refreshing or removing the parent's mutable pin cannot replace
the child's catalog or invalidate it when the parent's definition changes.

## Codex closing and selected skills

The launcher inventories the effective native MCP configuration in the member's
home, worktree and sandbox. It disables every ambient server except explicitly
inherited operator servers. It then runs a second native inventory with the
final overrides, verifies that crew and selected servers have the intended
transports and that every excluded server is disabled, and only then starts the
app-server and TUI. An unavailable inventory or a failed verification stops the
launch. No excluded server is allowed to remain reachable as a fallback.

Codex 0.159.0 splits `-c` keys on dots without unquoting their components.
For arbitrary native names, the disabling patch is a TOML inline-table value,
for example `mcp_servers={"dot.name"={enabled=false}}`. It precedes crew and
selected server leaves: a later whole-table CLI override would replace earlier
CLI leaves before merging with native configuration. The operator's files are
never edited.

T12 is exercised in `tests/toolbox_launch.rs` by
`codex_closes_ambient_servers_on_both_processes_and_sets_roots_before_tui`, with
native names `ambient`, `dot.name`, `two words`, and `quote"name`. Every one is
disabled and reported by name when unsandboxed; all are absent from the private
home when sandboxed. Set `CREW_TEST_CODEX_BINARY` to the installed Codex to use
its actual inventory rather than the test double. Sessions remain stand-ins,
so this gate cannot make a model request. Same-name selection collisions are
covered separately: the optional item is Shadowed and both definitions are
disabled; a required item refuses the launch. An ambient `crew` name always
refuses the launch because merged fields could alter the coordination server.

That gate creates operator skill directories to force `skills.config`, then
passes every captured TUI and app-server `-c` pair to the real Codex parser.
It covers arrays of TOML inline tables as well as MCP patches, feature flags,
standing instructions, notifier arguments and sidecar permission overrides.
JSON arrays containing objects are not valid substitutes for those TOML arrays.

Selected skills reach the app-server through `skills/extraRoots/set`, acknowledged
over its Unix WebSocket before the TUI starts. The separate native runner
`notes/toolbox-probes/codex-root-propagation.mjs` proves that roots survive the
setup connection closing and reach a later TUI connection's provider request.
That request is captured by a localhost HTTP 400 stub; no provider inference is
performed. These checks establish discovery and launch behavior, not cache hits
or account login.

## Claude prompt snapshots after an explicit refresh

Claude Code 2.1.291 records tool schemas inside a `prompt_snapshot` transcript
attachment. Replacing Crew's MCP pin does not replace that recorded native
catalog. An ordinary resumed process can therefore keep sending the original
descriptions even while Crew serves the refreshed definitions.

For `crew resume --restart <member>`, Crew checks that a resumed Claude member
with managed toolbox items supports `--system-prompt-snapshot off` before
replacing its MCP pins. Old or unrecognized versions report the refresh as
unavailable. Update Claude Code or use `crew agent restart --fresh <member>` to
start a new conversation. Ordinary recovery does not depend on this capability
probe, and a refused explicit refresh leaves the old catalog available to it.
The switch is documented for Claude Code 2.1.257 and newer. See the
[native CLI reference](https://code.claude.com/docs/en/cli-reference#system-prompt-flags-in-resumed-conversations).

If the explicit resumed refresh actually changes the managed fingerprint or
definition digest, Crew marks that native session id in daemon-private member
state and passes the switch on that launch and every later launch of the same
conversation, including crash recovery. A new native session id clears the
marker. An unchanged refresh leaves the snapshot policy alone. Inherited
subagent catalogs remain frozen. The flag is managed by Crew and cannot be
overridden through `extra_args`.

The conversation id and native history are retained; Crew does not rewrite
transcript records. Disabling the native snapshot rebuilds the system prompt
on each request for that conversation. It does not disable provider prompt
caching, but regenerated prompt bytes can affect cache reuse. The localhost
fixture observes stable system and tool bytes after refresh and recovery;
it does not establish cache hits, priced savings or stability for other
workloads. Resume-prefix and usage comparisons remain `cache-acceptance` work.

The native regression is `scripts/toolboxes/claude_refresh.py`. It covers an
unchanged explicit restart, a changed upstream description, resumed refresh,
automatic crash recovery, a later unchanged request and tool removal. Each
phase requires an actual native assistant reply, and the original transcript
prefix must remain byte-identical. Evidence and commands are in
[the Claude follow-up report](../review/toolbox-t14-claude.md).

## Experimental cache keep-alive mechanism

This integration chain also contains scheduler's `cache-retention-policy`
implementation (prompt-cache design D2). Its team YAML controls are experimental:
`limits.keep_cache_warm_secs`, `limits.keep_cache_warm_max`, and
`limits.keep_cache_warm_min_tokens`. The interval defaults to `0` (off). No
shipped default or style enables it.

Scheduler confirmed that D2 (`cache-retention-policy`) owns the decision to
enable that mechanism. It must remain
off in defaults and styles until at least three long-gap comparison pairs show
a net priced saving, including the keep-alive turns themselves, from native
session usage. Passing those pairs keeps the mechanism opt-in; default or style
enablement additionally needs production evidence. The preregistered failure
outcome is removing the setting and closing D2 as a no-op. See
`notes/prompt-cache.md`, “D2 revision: keeping idle Claude members warm” and
“Preregistered experiment: three long-gap pairs”. Landing the bounded, opt-in
mechanism in this code chain is not
evidence that warming saves money and does not authorize switching it on. The
toolbox launch card makes no cache-hit or priced-saving claim.

## Composition and validation

The composed branch includes main `cb95b650`, toolbox MCP controls approved at
`aaea82f4` (with the review-note correction in `99956858`), and pi's registration
barrier approved at `89650283` (with comment clarification `476879df`). Scheduler's
keep-alive stall fix `c1566c70` is also included; its code review is separate from
the D2 policy decision described above.

Final validation uses an absolute, short `CARGO_TARGET_DIR` under the workspace:
the sandbox library tests locate the built binary through that variable, and
Unix socket fixtures need room below the platform path-length limit. The
require-mode workspace run includes `socat` on PATH. The explicit native pi and
Codex suites supplement the workspace tests; neither performs provider inference.
Final gate results and exact commands are recorded with the task's review artifact.
