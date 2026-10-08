# Native toolbox capability probes

These are audit fixtures, not production launch code. Run from the repository
root with Node 22.19 or newer. The audited versions were Claude Code 2.1.291,
Codex 0.159.0, and pi 1.1.0. `TOOLBOX_CLAUDE` and `TOOLBOX_CODEX` may name
specific binaries; otherwise the probes use PATH.

```sh
node notes/toolbox-probes/codex-config.mjs
node notes/toolbox-probes/codex-server.mjs
node notes/toolbox-probes/codex-request.mjs
node notes/toolbox-probes/claude.mjs
node notes/toolbox-probes/claude-sources.mjs
node notes/toolbox-probes/codex-memory.mjs
node notes/toolbox-probes/install-pi.mjs
node notes/toolbox-probes/pi.mjs
node notes/toolbox-probes/pi-context.mjs
```

The install step fetches the pinned upstream pi package under
`target/toolbox-audit/pi-runtime`, with an isolated npm configuration and cache
and install scripts disabled. It changes no global installation. It is the
only step that requires downloading dependencies.

The probes generate throwaway homes and repositories under
`target/toolbox-audit`. They pass an allowlisted process environment and no real
credentials. Codex config/app-server and pi discovery make no inference request.
Claude and the Codex request probe use a localhost HTTP server: it records the
request body, immediately returns a deliberate HTTP 400, and never forwards the
request. Those captured CLI invocations should exit 1; a timeout or signal is
not success. Header values are not recorded. Native system prompt captures stay
in the ignored evidence directory, not in source control.

Each runner prints its observations and retains JSON/text evidence. Pi's
observation extension reads the native system prompt and tool registry after
server startup; closing stdin shuts the RPC process down. The subagent fixture
only tests discovery and available tool definitions; it never delegates work.
See the capability and exclusion addenda in [toolboxes.md](../toolboxes.md) for
expected observations and limitations. `codex-memory.mjs` uses `codex debug
prompt-input`, which renders the model-visible input without a request.
These fixtures do not test real account login, managed policies, provider
caching, or the Crew OS sandbox.
