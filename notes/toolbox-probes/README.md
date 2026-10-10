# Native toolbox capability probes

These are audit fixtures, not production launch code. Run from the repository
root with Node 22.19 or newer. The audited versions were Claude Code 2.1.291,
Codex 0.159.0, and pi 1.1.0. `TOOLBOX_CLAUDE` and `TOOLBOX_CODEX` may name
specific binaries; otherwise the probes use PATH.

```sh
node notes/toolbox-probes/codex-config.mjs
node notes/toolbox-probes/codex-trust.mjs
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

`codex-trust.mjs` additionally requires Linux bubblewrap. It runs native MCP
inventory in a network namespace before and after project trust, without
starting a server or a model. It retains the four results under
`target/toolbox-t12/codex-trust` (`TOOLBOX_OUT` can select another directory).
On Codex 0.159.0 the untrusted project server is absent, then enabled after
trust. A disabled-only override fails before trust because it lacks a
transport, but works once the project table loads. This is why a prelaunch
inventory cannot promise to exclude project servers across a later trust
decision. T12's code-mode child regression lives in
`scripts/toolboxes/discovery.py --runtime codex --case sources --child`
from the toolbox QA runner at `378d0b43` and uses its isolated local provider.

Each runner prints its observations and retains JSON/text evidence. Pi's
observation extension reads the native system prompt and tool registry after
server startup; closing stdin shuts the RPC process down. The subagent fixture
only tests discovery and available tool definitions; it never delegates work.
See the capability and exclusion addenda in [toolboxes.md](../toolboxes.md) for
expected observations and limitations. `codex-memory.mjs` uses `codex debug
prompt-input`, which renders the model-visible input without a request.
These fixtures do not test real account login, managed policies, provider
caching, or the Crew OS sandbox.

The launch integration has two additional gates. After `install-pi.mjs`
supplies the isolated `ws` dependency, run
`node notes/toolbox-probes/codex-root-propagation.mjs` in a PTY. It sets roots
through a setup WebSocket, closes it, checks a second connection, then attaches
the real Codex TUI and asserts that its localhost provider request contains the
selected skill. It deliberately kills the TUI and app-server after capturing
the request; success is the assertion, not a model response or an exit code
from either child. It retains evidence under
`target/toolbox-audit/codex-root-propagation`.

`CREW_TEST_CODEX_BINARY=/absolute/path/to/codex ./scripts/build-env cargo test
-p crew --test toolbox_launch` sends only MCP inventory commands to native
Codex. The session and app-server remain stand-ins. This exercises Crew's
closing overrides and effective-configuration verification, both outside and
inside its real sandbox. Other tests cover secret scrubbing, immutable skills,
reloads, required failures, runtime switches and sandbox exclusions. No model
request is made by that Rust suite.
