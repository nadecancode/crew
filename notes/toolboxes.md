# Team member toolboxes

## Current state

Audit for `toolbox-audit`, 2026-10-07, source baseline `c8052373`.
This section describes the existing implementation and the integration seams for
`toolbox-spec`; it does not define the new configuration contract. No product
code changed during the audit.

Crew injects its coordination tools into each runtime, but does not yet own a
member's complete toolbox. `defaults.inherit_mcp: false` only changes Claude
Code's launch. Codex and pi do not read that setting. Skills, plugins, runtime
subagents, and most additional tool configuration are discovered by the CLI,
not selected by a Crew member specification.

### Evidence and terminology

Repository paths and line numbers below refer to `c8052373`. CLI help was checked
locally for Claude Code `2.1.291` and Codex `0.159.0`. The `pi` on this shell's
PATH is GSD `2.70.1`, not the pi supported by Crew; `agents/pi.rs::check_binary`
explicitly rejects it. Pi capability statements below come from current
upstream documentation, not an installed-pi execution. Documentation was read
on the audit date and can describe capabilities newer than an eventual user's
CLI. No model requests, package installs, or changes to operator configuration
were needed. No operator configuration values were inspected or recorded.

`A` below means `<workspace>/.crew/agents/<member-id>`; the worktree the member
edits is a separate directory. A private generated file in `A` does **not** mean
the CLI has a private configuration home. `daemon/supervisor/launch.rs:165`
only chooses `A/rw/home` when `workspace.sandbox.mode` is `required`. The default
is `off`, independently of `defaults.permissions` and Codex's own sandbox mode.
With Crew's sandbox required, `sandbox/profile.rs` also sets `HOME=A/rw/home`.
Changing only a CLI-specific home outside that sandbox would not have that
effect.

### Per-runtime capability table

| Surface | Claude Code | Codex | pi |
| --- | --- | --- | --- |
| Crew MCP injection today | `A/mcp.json`, containing `mcpServers.crew.command` and `args`, passed with `--mcp-config`. `--strict-mcp-config` added unless `inherit_mcp` is true. | `-c mcp_servers.crew.command=...`, `.args=...`, and `.default_tools_approval_mode="approve"`. Same overrides go to the TUI and its private app-server. Other servers are not disabled. | `A/crew-pi.ts`, passed with `--extension`, calls `registerMcpServer("crew", ..., exposure: "direct")`. Older extension APIs fall back to `crew ctl tools` and individual `registerTool` calls. |
| Crew lifecycle integration today | `A/settings.json` supplies hooks and denies `AskUserQuestion`; `A/mod/crew-bridge` supplies socket delivery and member tone via `--plugin-dir`. | A `notify` override invokes `crew hook codex-notify`; socket delivery uses the app-server. The operator's own `developer_instructions` are explicitly read and prepended to Crew's briefing. | The generated extension reports lifecycle/context/model events, receives bus messages, and handles compaction. |
| Home with Crew sandbox required | `CLAUDE_CONFIG_DIR=A/rw/home/claude`; filtered `settings.json` and initial `.claude.json`. | `CODEX_HOME=A/rw/home/codex`; filtered `config.toml`; same home for TUI and app-server. | `PI_CODING_AGENT_DIR=A/rw/home/pi`; filtered `settings.json`, copied `models.json`; sessions at `A/rw/pi-sessions`. |
| Home without Crew sandbox | Existing operator home, unless endpoint configuration supplies an override. | Existing operator home or endpoint `CODEX_HOME`; operator integrations remain available. | Existing operator agent directory or endpoint override. Sessions still go to `A/pi-sessions`. |
| Session MCP controls available | Explicit config plus strict loading. `--settings` is an additional settings layer, not isolation of all customization. | Per-server `enabled`, `enabled_tools`, `disabled_tools`, transport and approval overrides. CLI `-c` merges configuration; it does not clear inherited tables. | Extension registration; server settings in `mcp.json`; `--no-mcp` disables the built-in MCP engine, including extension-registered servers that depend on it. |
| Session skill/plugin controls available | Repeatable `--plugin-dir`; `--add-dir` discovers that directory's skills too. `--disable-slash-commands` disables skills globally. `--setting-sources` chooses settings sources; no selective Crew skill loading exists. | Per-skill `skills.config` enablement and per-plugin enablement/configuration. `CODEX_HOME` and `--profile` affect configuration/state, but are not a closed discovery allowlist. No Claude-style plugin-directory flag appears in the checked CLI help. | `--no-skills` with explicit `--skill`; `--no-extensions` with explicit `--extension`; analogous prompt-template controls. These are not used by Crew today. |
| Existing `allowed_tools` meaning | Extra permission rules, appended to `mcp__crew`; not a declared-tool allowlist. | Ignored by the adapter. | Ignored by the adapter. |

Implementation: [Claude adapter](../crates/crew/src/agents/claude.rs),
[Codex adapter](../crates/crew/src/agents/codex.rs),
[pi adapter](../crates/crew/src/agents/pi.rs), and
[pi extension](../crates/crew/assets/crew-pi.ts). Runtime capabilities:
[Claude settings](https://code.claude.com/docs/en/settings),
[Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp?surface=cli),
[Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference),
[pi CLI](https://pi.dev/docs/latest/cli), and
[pi MCP](https://pi.dev/docs/latest/mcp).

### Discovery and inheritance paths

**Claude Code.** Crew writes `A/mcp.json`, `A/settings.json`, and
`A/briefing.md` on every launch (`agents/claude.rs:162`). Its generated bridge
plugin contains `.claude-plugin/plugin.json`, `hooks/hooks.json`, and
`hooks/register.ts`. It is loaded for socket delivery, or for member tone even
when delivery uses the terminal. Extra arguments follow Crew's arguments.
Crew supplies neither `--setting-sources` nor a skill-discovery policy.

The ambient settings files are `~/.claude/settings.json`,
`<project>/.claude/settings.json`, and `.claude/settings.local.json`. Current
Claude documentation says a Git worktree can share the main checkout's local
settings file. User/local MCP definitions are in `~/.claude.json`; project
servers use `.mcp.json`. Strict MCP loading excludes these ambient server
configurations, but does not disable skills, agent definitions, plugins, or
their other hooks. Managed configuration remains a separate layer.
[Settings locations](https://code.claude.com/docs/en/settings),
[MCP scopes](https://code.claude.com/docs/en/mcp).

Skills come from `~/.claude/skills/<name>/SKILL.md`, project/nested
`.claude/skills`, added directories, plugins' `skills/`, and managed/account
sources. `.claude/commands/*.md` remains a legacy skill form. Runtime subagent
definitions live in `~/.claude/agents/`, project `.claude/agents/`, or plugins;
`--agent`/`--agents` can supply session choices. These are Claude's agents, not
Crew's `agent_spawn` members.
[Skill discovery](https://code.claude.com/docs/en/skills),
[subagent scopes](https://code.claude.com/docs/en/sub-agents).

An enabled Claude plugin can carry skills, agent definitions, hooks, MCP, and
LSP configuration. Its conventional manifest is
`<plugin>/.claude-plugin/plugin.json`; other component paths are rooted at the
plugin directory, such as `skills/`, `agents/`, `hooks/hooks.json`, and
`.mcp.json`. Consequently a plugin selection needs a defined relationship to
an explicit MCP selection; loading a plugin directory is not proof its bundled
server survives strict MCP loading. That combination was not exercised here.
[Plugin manifest](https://code.claude.com/docs/en/plugins-reference).

**Codex.** Crew overrides only the `crew` MCP entry; it does not replace the
MCP catalog, disable plugins/apps, filter skills, or set project trust. The
adapter's `own_developer_instructions` deliberately reads the operator's
`config.toml` even for a sandboxed member (`agents/codex.rs:32,106`). This is
standing-instruction inheritance, separate from toolbox inheritance. Each
app-server receives the same overrides and environment as its TUI; a toolbox
implemented only on TUI arguments would miss the process that runs tools.

Codex uses `$CODEX_HOME/config.toml` (normally `~/.codex/config.toml`), trusted
project `.codex/config.toml` files, and system/cloud-managed layers. The
checked `--profile` loads `$CODEX_HOME/<name>.config.toml` as another layer;
`--strict-config` rejects unknown keys, rather than excluding other layers.
`CODEX_HOME` also moves authentication and session state, so repointing it is
not a tools-only change.
[Configuration layers](https://learn.chatgpt.com/docs/config-file/config-basic),
[environment variables](https://learn.chatgpt.com/docs/config-file/environment-variables).

Documented skill discovery includes `.agents/skills` between the working
directory and repository root, `$HOME/.agents/skills`, `/etc/codex/skills`,
and bundled system skills. Codex home also contains skill state. Legacy
`$CODEX_HOME/skills` and `skills/.system` paths exist in this environment, but
their discovery rules need version-specific verification. Do not treat copying
selected skills into `CODEX_HOME/skills` as an exclusive toolbox: the other
discovery roots still matter. Per-skill enablement entries refer to paths;
they are not documented here as arbitrary extra discovery roots.
[Skills](https://learn.chatgpt.com/docs/build-skills),
[configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).

Codex plugins can bundle skills and MCP configuration. Current packaging
supports a root `plugin.json` with `skills/` and `mcp.json`, with optional
`.codex-plugin/plugin.json` compatibility metadata. Local-marketplace plugin
keys use `name@marketplace`; their bundled MCP entries have a separate
`plugins.<plugin>.mcp_servers.<server>` namespace. Crew has no plugin install,
enablement, or selection path. A standalone `mcp_servers` policy therefore
does not cover every source of tools.
[Plugin packaging](https://developers.openai.com/plugins/build/plugins),
[plugin MCP controls](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).

**pi.** User configuration is under `PI_CODING_AGENT_DIR`, normally
`~/.pi/agent`: `settings.json`, `mcp.json`, `models.json`, `extensions/`,
`skills/`, `prompts/`, and instruction files. Trusted projects add the analogous
`.pi/` paths. Crew passes `--approve` only for `defaults.auto_trust: true`;
otherwise it passes `--no-approve`. This controls project resources, not the
operator's user resources. Skills also load from `~/.agents/skills` and
repository `.agents/skills` roots.
[Configuration paths](https://pi.dev/docs/latest/configuration),
[skills](https://pi.dev/docs/latest/skills).

`settings.json` can select extension/resource paths and packages. Pi's native
package/extension model is not interchangeable with a Claude plugin directory.
Crew uses only its explicit extension and does not restrict ambient packages.
The upstream API supports temporary MCP registration, but a same-name server
from `mcp.json` takes precedence; an extension can also replace the native MCP
implementation. Crew's `crew` name is not reserved against those runtime
sources today. The fallback `registerTool` path only bridges Crew tools; it
does not translate arbitrary MCP servers for an older pi.
[Settings](https://pi.dev/docs/latest/settings),
[extension MCP registration](https://pi.dev/docs/latest/extensions),
[MCP implementation replacement](https://pi.dev/docs/latest/mcp).

### Private-home behavior is not a toolbox policy

`agents/home.rs::Operator::detect` resolves the operator's configuration roots
from `HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, and `PI_CODING_AGENT_DIR`.
Codex's endpoint `CODEX_HOME` is respected as the source. `prepare` runs only
after prior sandboxes retire, under the member launch lease, using descriptor
relative writes that do not follow agent-controlled links.

The seed is deliberately narrow:

- Claude copies selected model/display preferences, not operator hooks,
  plugins, MCP configuration, permission rules, or login files. A new
  `.claude.json` records completed onboarding; existing state is preserved.
- Codex copies selected model configuration, including `model_providers`, not
  `mcp_servers`, hooks, project trust, `notify`, or `auth.json`.
- pi copies selected preferences and `models.json`, not package/extension/skill
  settings or `auth.json`.

No runtime's skill, plugin, command, agent, or extension directories are copied.
The agent's existing private home is not scrubbed on restart; session data and
files outside the seed's own outputs survive. Project files still exist in the
mounted worktree. A future toolbox cannot claim exclusivity from this seed
alone. Source/tests: [home.rs](../crates/crew/src/agents/home.rs), especially
`seeding_keeps_session_data_and_replaces_only_its_own_files`.

The sandbox also constrains executable paths, mounts, environment, and Unix
sockets. A toolbox declaration would not automatically make a host program or
its files available inside it. Credential handling already has a separate
launch-time path in [endpoints.rs](../crates/crew/src/endpoints.rs); resolved
secrets are not persisted in the roster or settings state. Toolbox support
should preserve that boundary rather than put secret values in `AgentSpec`.

### Team, roster, style, and launch attachment points

| Location at this baseline | Existing behavior | Toolbox seam to account for |
| --- | --- | --- |
| `crates/crew-core/src/config.rs`: `TeamConfig`, `Style` | Both carry defaults, orchestrator config, and worker `AgentSpec`s; strict unknown-field rejection. `Style::team` copies those into a team. | New declarations must be admitted, validated, and carried through both types. A new top-level catalog would also need explicit copying in `Style::team`. |
| `AgentDefaults` (`config.rs:319`) | `allowed_tools`, `extra_args`, `inherit_mcp`, `auto_trust`; the latter two default false. | No named toolbox, MCP entry, skill, plugin, or extension field. |
| `AgentSpec` (`config.rs:517`) | Per-member permissions plus additional `allowed_tools`/`extra_args`; no per-member `inherit_mcp` or `auto_trust`. | Requires an explicit member selection/override shape, including omitted versus empty semantics. |
| `OrchestratorConfig` (`config.rs:481`), `daemon/team.rs:231` | Narrow orchestrator schema becomes a fresh `AgentSpec`; team defaults still apply. | A worker-only field would leave the orchestrator without a corresponding override unless this conversion is updated. |
| `agents/mod.rs::AgentSettings::resolve` | Concatenates default then member `allowed_tools`/`extra_args`; other launch settings largely come from defaults. | Central place to resolve intent. Existing lists cannot express subtraction. |
| `daemon/supervisor/mod.rs::agent_settings`, `creation.rs` | Creates per-agent `AgentLocal` with original spec and resolved settings. `daemon/mod.rs::save_local` persists that state in SQLite metadata. | New resolved selection must survive daemon recovery, runtime switch, and model fallback; those paths re-resolve settings. |
| `daemon/roster.rs` | Saves standing members' specs in `.crew/roster.yaml` and `.crew/rosters/<style>.yaml`; omits tasks, subagents, temporary members. | Preserve toolbox references through save/load without expanding credentials or runtime-generated paths. |
| `crates/crew/src/styles.rs` | Built-ins, `/etc/crew/styles`, user config's `crew/styles`, then project `.crew/styles`; later same-name style replaces earlier. | Definition path resolution must survive a style becoming a team and later a roster. Today style origin is known during loading but is not a generic resource base in `AgentSpec`. |
| `daemon/subagents.rs`, `mcp/tools.rs`, `crew-core/src/rpc.rs` | `agent_spawn` accepts id/role/task/runtime/model, constructs a mostly default spec; settings come from team defaults, not a clone of parent's settings. `agent_add` also exposes only a narrow spec subset through MCP. | Decide parent inheritance and expose selection through the actual creation paths, not only team YAML. |
| `daemon/supervisor/launch.rs` and three adapters | Resolves endpoint fresh, assembles briefing, launches runtime; `LaunchContext` carries settings and optional private home. | Shared resolution before runtime translation; coordinate both Codex processes and keep generated files inside the member's directory. |

Crew endpoint files are separate from team files: global
`/etc/crew/config.toml` (or `CREW_GLOBAL_CONFIG`), user
`$XDG_CONFIG_HOME/crew/config.toml` (normally `~/.config/crew/config.toml`), and
project `.crew/config.toml`. `endpoints.rs::FileConfig` currently accepts
`claude`, `codex`, `agents`, and `web`, not toolbox declarations. Runtime
sections are merged before parent-member and then member sections. Pi has no
top-level runtime section there; it receives member environment overrides.
Reusing these files for toolboxes requires changing their strict parser too.

### Offline Codex inheritance probe

With Codex `0.159.0`, an isolated synthetic `CODEX_HOME/config.toml` contained
only `[mcp_servers.ambient]` with `command = "/bin/false"`. An empty working
directory and separate `HOME`/`XDG_CONFIG_HOME` were used. `codex mcp list --json`
returned the following; all invocations exited 0 without a model request:

| Added CLI override | Servers reported |
| --- | --- |
| None | `ambient`, enabled |
| `-c 'mcp_servers.crew.command="/bin/false"'` | `ambient` and `crew`, enabled |
| `-c 'mcp_servers={}'` | `ambient`, enabled |
| `-c 'mcp_servers={ crew = { command = "/bin/false" } }'` | `ambient` and `crew`, enabled |
| `-c 'mcp_servers.ambient.enabled=false'` | `ambient`, disabled |

This proves configuration merge behavior, not which tools reach an inference
request. A table containing only selected servers does not erase inherited
servers. Explicit disablement or a verified discovery boundary is necessary
for a closed toolbox. Evidence is retained in this worktree under
`target/toolbox-audit/codex-{baseline,crew-leaf,empty-table,replacement-table,disable-leaf}.json`,
alongside CLI help. The synthetic fixtures contain no credentials.

### Cache effects and remaining gaps

Tool definitions and available-skill catalogs are reusable model input, so
changing them can invalidate a prefix well before a member's task text.
Anthropic documents the tools/system/messages cache hierarchy; OpenAI documents
prefix matching and the importance of stable tools and instructions. A stable
toolbox is necessary, but is not by itself proof of a cache hit: model,
runtime rendering, cache scope, breakpoints, and compaction also matter.
[Anthropic prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching),
[OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching).

Crew's own MCP definitions are already a fixed `Vec<ToolDef>` in
`mcp/tools.rs::definitions`. `mcp/mod.rs::tool_list` filters that vector by
orchestrator role without reordering it; `crew ctl tools` uses the same filter.
`tools/list` contains neither member identities nor live task state, and MCP
advertises `listChanged: false`. The member-specific command and identity-file
path configure the transport; they are not part of Crew's tool schemas.

Design consequences, not yet implemented:

1. Resolve one explicit toolbox before launch. Canonicalize set-like selections
   and map keys; preserve order where it has meaning, such as process arguments
   or extension execution. Do not let filesystem enumeration or server
   connection completion choose model-visible ordering.
2. Preserve the resolved catalog across turns and ordinary resumes. Define what
   happens when a requested toolbox changes: restarting a process does not
   necessarily replace the runtime's saved system prompt or tool history.
   Record a nonsecret selection/version identity outside the prompt for
   diagnostics; do not add changing task IDs or timestamps to descriptions.
3. Define inheritance separately for ambient MCP, skills, plugins, hooks, and
   runtime subagents. None of the three adapters currently implements that
   combined boundary. Retain Crew's coordination/lifecycle components while
   applying selection, and define how name collisions and unsupported runtime
   components fail.
4. Treat deferred tools separately from tools declared on every request. Pi
   supports direct and deferred exposure; Claude supports MCP tool search.
   Smaller initial declarations and unchanged total available tools are
   different measurements. Crew cannot promise the runtime's final tool order
   merely by sorting generated config.
5. Validate with two members using different selections plus an ambient sentinel:
   observe native tool/skill discovery, restart/resume, subagent creation,
   fallback, private-home operation, and unchanged operator files. Measure
   cache reads from runtime-native usage after repeated equivalent turns;
   configuration-byte equality alone is not a cache benchmark.

Open verification items for implementation: native pi availability; Claude
plugin-bundled MCP under strict mode; Codex's full skill/plugin discovery
boundary on the supported release; managed or account-provided components;
and actual wire-level ordering under deferred discovery. The current audit
does not claim those are solved by a flag, by an empty map, or by home seeding.
