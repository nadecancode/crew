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
explicitly rejects it. The initial audit used upstream pi documentation; the
capability addendum below subsequently verifies a disposable pi installation.
Documentation was read
on the audit date and can describe capabilities newer than an eventual user's
CLI. The initial audit needed no model requests, package installs, or changes
to operator configuration. No operator configuration values were inspected or
recorded.

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
`A/briefing.md` on every launch (`agents/claude.rs:164`). Its generated bridge
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
`config.toml` even for a sandboxed member (`agents/codex.rs:38,106`). This is
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

The follow-up below resolves native pi availability, Claude plugin MCP under
strict mode, and several discovery controls. Managed/account-provided
components, native login continuity under a changed configuration home, and
actual wire-level ordering under deferred discovery remain implementation
checks. An empty map or home seeding is not proof of those properties.

### Capability probes addendum

`toolbox-capability-probes`, 2026-10-07 America/Chicago. Source baseline remains
`c8052373`; no Crew implementation changed. The native observations here
supersede the earlier unverified capability cells and the corresponding
assumptions in the first `toolbox-spec` draft (`efd169eb`).

Versions: Claude Code `2.1.291`, Codex `0.159.0`, upstream
`@earendil-works/pi-coding-agent` `1.1.0`. No supported pi was installed on this
host, so the probe installed that exact package only under the ignored
`target/toolbox-audit/pi-runtime`, using a separate npm home/config/cache and
`--ignore-scripts`. No global install or login was used.

The [retained probe runners](toolbox-probes/README.md) create private homes,
bound fixture repositories, and an allowlisted environment. They use no real
credential and do not read or change operator configuration. Codex's discovery
APIs and pi run without a turn. Where discovery needed inspection of the
model-visible request, Claude and Codex sent it to a **localhost stub** that
recorded its body and returned HTTP 400. Nothing was forwarded to a provider;
there was no inference, account login, token refresh, or subagent execution.
For those request probes, CLI exit 1 is expected; it records the deliberate
stub rejection, not a successful model turn.

#### Claude: source selection also controls skills, agents, and memory

Command: `node notes/toolbox-probes/claude.mjs`.
The runner uses `claude --print --model claude-opus-5-5
--dangerously-skip-permissions --mcp-config <fixture.json>`, normally with
`--strict-mcp-config`. Each case sends one `/v1/messages?beta=true` request to
the stub and exits 1. A valid local marketplace contains an enabled user plugin
with a skill and an MCP server. User skill, native-agent, memory, and
`SessionStart` hook sentinels are separate fixtures; a project skill is the
positive control.

| Case | User skill / native agent / memory / plugin skill | User hook ran | Project skill | Explicit selected skill / agent / hook |
| --- | --- | --- | --- | --- |
| Default sources, strict MCP | All present | Yes | Present | Not supplied |
| `--setting-sources project,local` | All absent | No | Present | Not supplied |
| Empty private `CLAUDE_CONFIG_DIR` | All absent | No | Present | Not supplied |
| `--setting-sources project,local --plugin-dir <selected>` | All absent | No | Present | All present; hook ran |
| Private `CLAUDE_CONFIG_DIR --plugin-dir <selected>` | All absent | No | Present | All present; hook ran |

Output excerpt for the selected plugin with project/local settings:

```json
{"hook":false,"selectedHook":true,"user_skill_canary":false,
 "user_agent_canary":false,"USER_MEMORY_CANARY":false,
 "plugin_skill_canary":false,"project_skill_canary":true,
 "selected_skill_canary":true,"selected_agent_canary":true}
```

The same installed plugin's MCP process **does not start under strict MCP**,
although its skill loads. Removing `--strict-mcp-config` starts it. This closes
the plugin-MCP question for this release and this plugin format.

The two selected-plugin cases use different absolute plugin directories. Their
selected skill and agent catalog lines are byte-identical; neither request
body contains the plugin root path. The containing context block does include
the working directory separately, so this proves equality of those catalog
entries, not of the whole input. It also does not inspect paths returned when
a skill is invoked later.

Consequently the draft statement that user skills, agents, and memory cannot
be excluded with `--setting-sources` is false for the tested CLI. Ordinary user
sources can be excluded while explicit Crew plugin components remain. This
does not prove that enterprise-managed, account-synced, or built-in components
are excluded, nor that multiple hook handlers execute in a requested order.
Evidence: `target/toolbox-audit/claude-discovery/*/result.json`, `requests.json`,
and `debug.log`.

#### Codex: exact skill paths, additive roots, hooks, and native agents

Commands:

```sh
node notes/toolbox-probes/codex-config.mjs
node notes/toolbox-probes/codex-server.mjs
node notes/toolbox-probes/codex-request.mjs
```

The first runner repeats the MCP merge probe and invokes `codex debug
prompt-input` with synthetic skills. Observed skill catalogs:

| Configuration | `$HOME/.agents/skills` sentinel | `$CODEX_HOME/skills` sentinel | Repository `.agents/skills` sentinel | Unrelated external folder |
| --- | --- | --- | --- | --- |
| Baseline | Present | Present | Present | Absent |
| `skills.config` disables the two **folders** | Present | Present | Present | Absent |
| `skills.config` disables their exact **`SKILL.md` files** | Absent | Absent | Present | Absent |
| `skills.config` enables the external folder | Present | Present | Present | Absent |

This release matches disablement against the `SKILL.md` path. A folder-level
override silently failed to suppress either sentinel. An enablement entry did
not add a new discovery root. The older documentation-derived uncertainty
about `$CODEX_HOME/skills` is now closed: it is scanned by `0.159.0`.

The second runner starts `codex app-server --stdio`, negotiates
`experimentalApi`, and calls `skills/list`, `hooks/list`, and `config/read`.
`skills/extraRoots/set {"extraRoots":["<external-root>"]}` followed by
`skills/list` adds an external skill while retaining the other roots. This is
a verified app-server discovery API, **not** a verified CLI root-selection
flag or an exclusion policy. Bundled system skills stay present. The
under-development `skip_host_skill_discovery` feature did not change these
`skills/list` results and is not used as an exclusion mechanism here.

Hook fixture output:

```text
baseline       user SessionStart enabled/untrusted; project SessionStart enabled/untrusted
-c hooks={}    user SessionStart enabled/untrusted; project SessionStart enabled/untrusted
private home   project SessionStart enabled/untrusted
-c features.hooks=false   []
```

User hooks came from `$CODEX_HOME/hooks.json`; project hooks from the trusted
repository's `.codex/hooks.json`. An empty hooks table does not remove those
sources. `features.hooks=false` suppresses the entire hook inventory; it is not
selective loading. This probe enumerates hooks and trust state, without running
them. Codex documents cumulative hook sources, concurrent matching handlers,
and trust of the exact definition; native support therefore does not establish
the spec's proposed sequential execution after Crew hooks.
[Native hook semantics](https://learn.chatgpt.com/docs/hooks).

For native subagents the third runner uses `codex --no-daemon exec
--skip-git-repo-check --json` with a custom provider pointing to the stub. It
captures the `/v1/responses` tool definitions. Standalone TOML files under
`$CODEX_HOME/agents/` and trusted `.codex/agents/`, with `name`, `description`,
and `developer_instructions`, both appear in the `spawn_agent` definition:

```text
baseline                 userRole=true  projectRole=true  spawnTool=true
private CODEX_HOME       userRole=false projectRole=true  spawnTool=true
features.multi_agent=false  userRole=false projectRole=false spawnTool=false
```

Codex can load native agents, but a Claude Markdown definition is not that
format. Translation or an explicit runtime-specific format would be needed.
Disabling multi-agent functionality removes the whole native spawn surface.

The private-home control changes only `CODEX_HOME`, retaining the synthetic
`HOME`: old Codex-home skills/hooks/agents disappear, while
`HOME/.agents/skills` and trusted project sources remain. Repointing
`CODEX_HOME` also changes native auth/session storage. These results do not
prove an unsandboxed `operator: none` route that preserves an existing login.
Explicit per-server disablement, exact per-skill disablement, and whole-feature
toggles are observed controls, not a complete policy for future discoveries.
Evidence: `target/toolbox-audit/codex-config`, `codex-server/*-summary.json`,
and `codex-request/*/result.json` plus request captures.

#### pi: explicit skills work; MCP and extension controls interact

Commands: `node notes/toolbox-probes/install-pi.mjs`, then
`node notes/toolbox-probes/pi.mjs`. The actual pi process uses
`--mode rpc --offline --no-session` and an explicit observation extension.
That extension registers a Crew-shaped MCP server and reads the native system
prompt, commands, and tool registry after startup. No user turn is submitted;
all seven cases exit 0. Fixture MCP processes expose one canary tool each.

| Added controls | Native MCP tools | Skills | Ambient user extension |
| --- | --- | --- | --- |
| Baseline `--approve` | User, project, Crew | User, `HOME/.agents`, project | Loaded |
| `--no-skills --skill <selected>` | User, project, Crew | Selected only | Loaded |
| `--no-extensions`, retaining explicit observation extension | None | User, `HOME/.agents`, project | Excluded |
| `--no-extensions --extension builtin:mcp`, retaining observation extension | User, project, Crew | User, `HOME/.agents`, project | Excluded |
| `--no-mcp` | None, including Crew | User, `HOME/.agents`, project | Loaded |
| `--no-approve` | User, Crew | User, `HOME/.agents` | Loaded |
| Private `PI_CODING_AGENT_DIR`, `--no-approve`, `--no-extensions -e builtin:mcp -e <selected-extension>`, `--no-skills --skill <selected>` | Crew only | Selected only | Excluded |

The last case's native registry contains `mcp__crew__canary` and neither
ambient MCP canary. This is a demonstrated closed **fixture** selection route.
It also suppresses project discovery and changes pi's auth/provider directory;
it does not satisfy independent per-kind inheritance or unchanged login by
itself. Re-enabling `builtin:mcp` without controlling its config sources simply
restores ambient servers. `--no-mcp` cannot retain Crew's native registration.

User `<agent-dir>/agents/*.md` and project `.pi/agents/*.md` sentinel definitions
do not enter the prompt or command catalog, and no native subagent tool appears
in the baseline registry. Loading those Claude-style definitions would require
an extension or adapter; this probe does not claim that third-party pi
extensions cannot implement delegation.

Pi's observed MCP tool order differs between cases with the same three
servers: `ambient, crew, project` versus `ambient, project, crew`. The installed
`dist/extensions/mcp/index.js` registers definitions from each connection's
`onTools` callback. Therefore stable config/registration order is insufficient
to prove final model tool-array order. Evidence:
`target/toolbox-audit/pi-discovery/*/{result,observed}.json`.

#### Boundaries for the spec

These probes establish native discovery and launch controls on the named
versions. They do not validate the Crew sandbox, real login continuity,
enterprise/account policies, hook ordering, resumed model state, or provider
cache hits. In particular, keep separate: a runtime supports a component; Crew
has an adapter for its format; unwanted sources are excluded; the session
actually loaded it; and its model-visible bytes remain stable. The spec must
not mark all five as established by a configuration flag.

## Spec

Specification for `toolbox-spec`, 2026-10-07, against the current state above
(baseline `c8052373`). It defines the configuration, how it resolves and maps
onto each runtime, how it is shown and how it is accepted. It follows the
operator's request for team-member scoped toolboxes (w-9599bbf059d8f7a6) and
the prompt cache objective (w-44a18622b211c078): a toolbox must render the
same bytes every time it is applied. No product code changes with this note.

Capabilities come from two sources:

- the current state above;
- graph's offline capability probes, which ran after that audit. They used
  Claude Code 2.1.291 against a local request-capture stub, and Codex 0.159.0
  through `codex mcp list`, `codex debug prompt-input` and its app-server, all
  with synthetic homes and no model request. The evidence is under
  `target/toolbox-audit/` in graph's worktree.

The text cites the second source as "capability probe". A mechanism still
marked *verify* has not been exercised. It must be confirmed by a probe before
the slice that depends on it is accepted.

### Goals

1. Each member runs with exactly the extensions configured for it: the team's
   defaults, the bundles it includes and its own additions, minus its
   exclusions. crew's own coordination and lifecycle components are always
   kept.
2. Nothing comes from the operator's own runtime configuration unless the team
   file asks for it, wherever the runtime can exclude it. Anything a runtime
   still loads that crew cannot exclude is listed as Cannot be excluded, never
   hidden. With `strict`, the exclusion is exact: such a member does not start.
3. One configuration serves every runtime. An item a runtime cannot load is
   reported per member, and a member that needs the item can refuse to start
   instead.
4. Secrets never appear in the team file, the roster, the event log, generated
   files, command lines, prompts or dashboards. The team file names
   environment variables, and values are resolved at launch, in memory.
5. A toolbox is fixed for the life of a session. Changing it means restarting
   the member.
6. The same toolbox renders the same bytes, in a deterministic order, on every
   spawn, restart and resume of the same member. Its model-visible definitions
   are the same for every member with the same fingerprint and role. A toolbox
   therefore does not break a cached prompt prefix by itself.

### Non-goals

- **Instructions, beyond one point.** The content and order of the crew
  briefing, project `CLAUDE.md` and `AGENTS.md` belong to the cache work. The
  one point here is the operator's personal instruction files: user
  `CLAUDE.md`, `~/.codex/AGENTS.md`, and the operator's Codex
  `developer_instructions`, which crew deliberately prepends today. They are
  the `memory` kind of `inherit.operator`. On Claude Code, the switch that
  excludes the operator's skills, subagents and plugins excludes them as well
  (capability probe, Claude Code 2.1.291).
- **Built-in tool restriction, permissions and sandbox policy.** `--tools`,
  `allowed_tools`, `permissions` and `workspace.sandbox` keep their own
  fields.
- **Installing anything.** A toolbox points at programs, URLs, files and
  directories that already exist. crew does not fetch packages, install
  marketplace plugins or change the operator's configuration files.
- **Changing a toolbox inside a running session.**

### Terms

| Term | Meaning |
| --- | --- |
| Item | One extension: an MCP server, skill, plugin, subagent or hook, named within its kind. It is referred to as `<kind>.<name>`, for example `mcp.github`. |
| Kind | `mcp`, `skills`, `plugins`, `subagents`, `hooks`. |
| Toolbox body | Item definitions plus `include`, `exclude`, `inherit`, `reset` and `strict`. It appears at four levels: `toolboxes.<name>`, `defaults.toolbox`, `orchestrator.toolbox` and `agents[].toolbox`. |
| Bundle | A named toolbox body under the top-level `toolboxes`. This is the per-role unit: members of one role include the same bundle. |
| Source | Configuration the runtime discovers by itself. The operator's sources are the user home and its local settings. Project sources are the files checked into the member's worktree. System and managed sources are machine-wide. |
| Effective toolbox | The resolved items of one member on one runtime, with each item's origin and status, plus every source and what it contributes. |
| Fingerprint | SHA-256 of the canonical form of the effective toolbox (see Prompt cache rules). It is shown shortened to 12 hex characters. |
| Definition digest | SHA-256 of the MCP tool definitions crew's proxy serves to the session (see Prompt cache rules). It changes only when a snapshot is refreshed. |

### Team file

#### Example

```yaml
project: shop
toolboxes:                         # bundles, by name
  base:
    mcp:
      context7:
        command: [npx, -y, "@upstash/context7-mcp"]
  web:
    include: [base]
    mcp:
      playwright:
        command: [npx, "@playwright/mcp", --headless]
        required: true             # a member that includes web will not start without it
    skills:
      frontend-qa:
        path: toolbox/skills/frontend-qa        # a directory holding SKILL.md
  github:
    mcp:
      github:
        command: [github-mcp-server, stdio]
        env:
          GITHUB_TOKEN: { env: SHOP_GITHUB_TOKEN }  # a reference, never a value
          GITHUB_TOOLSETS: { value: "repos,issues,pull_requests" }
      docs:
        url: https://docs.example.com/mcp
        headers:
          Authorization: { env: DOCS_TOKEN, format: "Bearer {}" }
defaults:
  runtime: claude
  toolbox:
    include: [base]
    inherit:
      operator: none               # the default: none of your own configuration
orchestrator:
  toolbox:
    reset: true                    # the orchestrator gets crew's tools only
agents:
  - id: qa
    role: frontend QA
    toolbox:
      include: [web]
  - id: release
    role: release notes
    runtime: codex
    toolbox:
      include: [github]
      exclude: [mcp.context7]
      skills:
        changelog: { path: toolbox/skills/changelog }
      plugins:
        lint-kit: { path: ~/plugins/lint-kit }  # a Claude Code plugin: reported on Codex
```

#### Fields

Every new struct keeps `#[serde(deny_unknown_fields)]`. A binary older than
this feature therefore rejects a team file that uses it. That is intended, and
the README says so.

| Field | Where | Type | Meaning |
| --- | --- | --- | --- |
| `toolboxes` | top level of a team file and of a style | map of name to toolbox body | Bundles. Names follow agent ids: `[a-z][a-z0-9_-]{0,31}`. |
| `toolbox` | `defaults`, `orchestrator`, `agents[]` | toolbox body | This level's contribution. Omitted and `{}` mean the same thing: no change at this level. |
| `include` | toolbox body | list of bundle names | Bundles merged in, in the order listed. A bundle may include bundles; a cycle is an error. |
| `mcp`, `skills`, `plugins`, `subagents`, `hooks` | toolbox body | map of name to item | Items defined at this level. |
| `exclude` | toolbox body except bundles | list of `<kind>.<name>` or `<kind>.*` | Removes items that came from earlier levels or from this level's includes. |
| `inherit` | toolbox body except bundles | `{ operator, project }` | Which runtime sources may load (see Sources and inheritance). |
| `reset` | `orchestrator.toolbox`, `agents[].toolbox` | bool, default `false` | Start from an empty toolbox instead of `defaults.toolbox`. |
| `strict` | toolbox body except bundles | bool, default `false` | Treats every item as `required`. A source the runtime cannot exclude then stops the launch. |
| `required` | any item | bool, default `false` | The member does not start if this item cannot be loaded. |
| `runtimes` | any item | list of `claude`, `codex`, `pi` | Restricts where the item applies. Elsewhere it is Not applicable: shown, but not a fault. |

Items per kind:

| Kind | Fields | Notes |
| --- | --- | --- |
| `mcp`, local | `command` (literal argument array), `env` (map of name to reference), `cwd` (optional, relative to the member's worktree) | No secrets in argv or `cwd`; they go in `env`. |
| `mcp`, remote | `url` (literal), `transport` (`http`, the default, or `sse`), `headers` (map of name to reference) | Secrets go in `headers`. |
| `skills` | `path` | A directory containing `SKILL.md`. Claude Code, Codex and pi share this format. |
| `plugins` | `path` | A Claude Code plugin directory (`.claude-plugin/plugin.json`). On Codex and pi it is Not available (Claude Code plugin format), which counts for `required` and `strict`; set `runtimes: [claude]` to make it Not applicable there instead. A plugin's skills, subagents and hooks load with it. Its bundled MCP servers never start under strict MCP loading, while its skills stay (capability probe, Claude Code 2.1.291), so a plugin with a bundled `.mcp.json` shows the detail "bundled MCP servers not loaded: declare them under mcp". Codex's own plugin format is not supported in this version. |
| `subagents` | `path` | A Markdown definition with front matter (`name`, `description`, optional `model`, `tools`) and the instructions as its body. crew renders it for Claude Code unchanged and for Codex as an agent TOML file (`name`, `description`, `developer_instructions`, and `model` when it is a Codex model). A field that does not translate is named in the item's detail. |
| `hooks` | `event`, `matcher` (optional), `command` (literal argument array) | Runs alongside crew's own hooks for the same event, never instead of them. Runtimes run matching hooks concurrently, so no order relative to crew's hooks is promised. |

A reference is either `{ env: NAME }`, optionally with `format` (a string
containing one `{}`), or `{ value: "literal" }` for a value that is not
secret. A plain string where a reference is expected is a validation error.
Every literal is therefore deliberate and visible: dashboards print a
`{ value }` in full and an `{ env }` as `$NAME`, marked set or unset.

**Paths.** A relative `path` resolves against the directory of the file that
defines it: the team file's directory, or the style file's directory. That
need not be the workspace root. `~/` expands to the operator's home. crew
stores every path as absolute when the file is loaded, so the daemon's team
copy, a roster and a style-made team all carry the same location. Built-in
styles define no file items.

**Reserved names.** `mcp.crew`, and any item named `crew-bridge` or
`crew-toolbox`, are reserved for crew. Defining one is a validation error.

### Resolution

One pure function in `crew-core` computes a member's effective toolbox. It
processes levels one at a time, each level completely before the next:

1. **Levels.** The applied levels are `defaults.toolbox`, then the member's
   own level. With `reset: true` on the member's level, `defaults.toolbox` is
   not applied at all: none of its items, its `exclude`, its `inherit` or its
   `strict`. The member then states its own policy, or gets the built-in
   defaults. The orchestrator is a member like the others; its level is
   `orchestrator.toolbox`. The conversion of `OrchestratorConfig` into a spec
   (`daemon/team.rs:231`) must carry that level.
2. **Processing one level.** Start from the result of the previous level, or
   from nothing for the first. Then:
   1. Expand the level's `include` list depth-first, in the order listed: a
      bundle's own includes come before its own items.
   2. Add the level's own items. A later definition with the same
      `<kind>.<name>` replaces the earlier one whole. There is no
      field-by-field merge.
   3. Apply the level's `exclude` to the result so far. Defaults' exclusions
      therefore act on defaults' bundles, and a member's exclusions act on
      everything from defaults and from its own bundles. Each exclusion must
      match at least one item at that point. Otherwise it is a validation
      error (it catches typos and duplicates). An exclusion naming an item the
      same level defines is also an error.
3. **Inherit.** `inherit` is taken whole from the most specific applied level
   that sets it: the member's, then defaults' (unless reset), then the
   built-in default (`operator: none`, `project` all except `mcp`). It is
   never merged key by key.
4. **Strict.** `strict` is true if any applied level sets it.
5. **Map.** Map every item onto the member's runtime (see Runtime mapping) and
   give it a status.

A member's later level can always re-add an item that defaults excluded, by
including a bundle with it or defining it. A defaults exclusion never removes
a member's own item.

Origins are recorded per item:

- `default`: from `defaults.toolbox`.
- `bundle:<name>`: from that bundle. The innermost bundle that defines the
  item wins.
- `member`: defined at the member's own level.

Sources carry `operator`, `project`, `system` or `managed`.

Validation runs in `crew-core` when a team file or style is loaded, so
`crew run` refuses a bad file before anything starts. Errors name the field
path, for example `agents[2].toolbox.exclude[0]: mcp.contex7 matches no item
(did you mean mcp.context7?)`.

### Sources and inheritance

`inherit.operator` and `inherit.project` each take `none`, `all` or a list of
kinds. The kinds are `mcp`, `skills`, `plugins`, `subagents` and `hooks`,
plus two more: `settings`, for the runtime's remaining settings files, and
`memory`, for instruction files (see Non-goals).

- **`operator`, default `none`.** These are the operator's user home
  (`~/.claude` or `$CLAUDE_CONFIG_DIR` and `~/.claude.json`; `$CODEX_HOME`
  and `~/.agents`; pi's agent directory and `~/.agents`) and the operator's
  local, uncommitted project settings (`.claude/settings.local.json`, which a
  Git worktree can share with the main checkout). With `none`, none of their
  MCP servers, skills, plugins, subagents, hooks, settings or personal
  instructions load. Model and
  display preferences are the exception. crew passes the same allowlist the
  sandbox seeds today (`agents/home.rs`: model, effort, theme, output style,
  language, and the Codex and pi equivalents) through its own generated
  settings, so a member keeps the operator's model choices. Login is not part
  of a toolbox and works as it does today.
- **`project`, default `all` except `mcp`.** This is the repository's
  checked-in runtime configuration in the member's worktree (`.claude/`,
  `.codex/` when the project is trusted, `.pi/`, `.agents/skills`). It is part
  of the work, versioned and reviewable, and a person working in the
  repository would get it too. Project MCP servers stay excluded, as
  `--strict-mcp-config` excludes them today: an unattended session cannot
  answer Claude Code's approval prompt. To use one, declare it under `mcp`.
  pi's project resources keep following `auto_trust`.
- **System and managed sources** (`/etc/codex/skills`, bundled system skills,
  managed policy and account-provided components) are not the operator's
  personal configuration and are not controlled here. They are listed when
  crew can enumerate them.

**Exact exclusion and reporting.** For each member, crew works out per kind
what every source will contribute, and lists the entries it can enumerate by
name (skill and subagent directories, hook entries, MCP server names).

When a runtime cannot exclude something that `inherit` excludes, the kind is
listed as **Cannot be excluded on <Runtime>**, with the reason. A member under
`strict` then does not start.

A sandboxed member (`workspace.sandbox.mode: required`) runs in a private home.
The operator's home sources are therefore absent by construction, but that is
not a complete boundary:

- **Project and system sources remain.** They are mapped like any other source.
- **The private home is not scrubbed.** Files the member's earlier sessions
  left in the private home survive restarts (current state, home.rs). crew
  owns the directories it renders the toolbox into and replaces them whole at
  each start, using descriptor-relative writes that never follow links. Other
  discovery roots inside the private home are listed as an `operator` source
  named "earlier sessions" when they are not empty.

**Migration.** `defaults.inherit_mcp: true` keeps working: it means
`inherit.operator: [mcp]` at the defaults level. Setting both `inherit_mcp`
and `defaults.toolbox.inherit` is a validation error, and
`crew toolbox check` suggests the replacement. Today only Claude Code reads
`inherit_mcp`. From this version it applies to every runtime, which closes
the Codex and pi gaps in the current state.

### Secrets

- **The team file holds only references** (`{ env: NAME }`). Literals are
  `{ value: … }` and are treated as public.
- **A separate lookup.** Toolbox references do not use the endpoint
  variables. Endpoint `env` and `env_files` values are meant for the runtime:
  they go to the session's `LaunchSpec.env`, and Claude Code's `apply_endpoint`
  writes `env` into `A/settings.json` (`agents/claude.rs:141`). References
  resolve when the member starts, from two places, in this order:
  1. A new `toolbox_env` table in the crew configuration files (same three
     scopes as `endpoints.rs`): `[toolbox_env]` for every member, and
     `[agents.<id>.toolbox_env]` for one member, which wins. Each entry is
     `NAME = { file = "~/.secrets/github" }`, a file only the operator can
     read with the same ownership and mode checks as `api_key_file`, or
     `NAME = { env = "DAEMON_VAR" }`, a variable of the daemon's environment.
     A subagent uses its parent's section before its own.
  2. A variable of the same name in the daemon's environment.

  Values live only in the daemon's memory, as endpoint secrets do today.
- **Secret names.** For each reference, crew tracks two names:
  - the **reference name** (`SHOP_GITHUB_TOKEN`);
  - the **source name**, which is the daemon variable a `toolbox_env` entry
    points at (`{ env = "DAEMON_TOKEN" }`), or the reference name itself when
    the value comes straight from the daemon's environment.

  A file source has no source name. The member's **secret names** are every
  reference name and every source name of its effective toolbox.
- **No collision with model credentials.** A secret name that the member's
  runtime or endpoint also uses is a validation error. This applies to source
  names as much as reference names, so `SHOP_TOKEN = { env =
  "OPENAI_API_KEY" }` is refused. The protected names are:
  - `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`;
  - `OPENAI_API_KEY`, `CREW_CODEX_API_KEY`;
  - `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `PI_CODING_AGENT_DIR`, `CREW_IDENTITY`;
  - every name in the member's endpoint `env`, `env_files` and `secret_env`;
  - the variables the endpoint's `api_key_env` and `auth_token_env` name.

  Scrubbing such a name would break the member's login, and passing it would
  hand the model credential to a third-party server.
- **Scrubbed from the session.** The member session's process environment has
  every secret name of the member's effective toolbox removed, on both launch
  paths:
  - The PTY launcher (`process/pty.rs`) adds the names to `SCRUBBED_ENV` for
    this launch.
  - The sandbox launcher (`daemon/sandbox.rs` and `sandbox::spec::launcher_env`)
    never passes them by name.
  - The Codex app-server sidecar follows the same rule.

  An unsandboxed session therefore inherits neither `SHOP_GITHUB_TOKEN` nor
  the `DAEMON_TOKEN` it aliases, even when the operator started the daemon
  with both exported. A server process gets the resolved value under its
  reference name only, never under the source name.
- **Delivery.** Every toolbox MCP server, local or remote, is reached through
  `crew toolbox mcp <name> --identity <identity file>`, the server entry crew
  renders for the runtime. Like `crew credential` for `apiKeyHelper` today, it
  asks the daemon over the member's identity for that server's resolved
  environment and headers. A local server is spawned as its child, with
  exactly that environment added to the scrubbed one, and every message is
  forwarded through it. A remote server is bridged with those headers. The
  launcher is the forwarding proxy of Prompt cache rules, rule 5, in both
  cases. This also satisfies the sandbox, which passes only the variables
  named in `LaunchSpec.env`; the secrets never need to be among them.
- **Never written by crew.** A resolved value never appears in:
  - a generated file (`mcp.json`, settings, plugin directories, Codex `-c`
    arguments, pi registration);
  - a command line;
  - the member session's own environment;
  - the roster, the event log, a prompt or a dashboard.
- **Scope of the guarantee.** It covers what crew writes and passes. A server
  can still put its own credential in an error message or a tool result, and
  those reach the model. As a best-effort defence, the proxy replaces exact
  occurrences of that server's resolved values in responses with
  `[redacted $NAME]`. It does not catch encoded or partial copies, so it is not
  a guarantee.
- **Not an isolation boundary.** Any process holding the member's identity can
  ask for the same values, and a process of the same user can read another
  process's environment. What the rule does is keep secrets out of files,
  argv, transcripts and prompts. Those are the places where agents, logs and
  caches would otherwise copy them.
- **Missing variables.** A missing variable makes the item unavailable, with
  the status Needs $NAME. A required item, or any item under `strict`, stops
  the launch with the same text.
- **Rotation.** Rotating a secret changes no generated byte and no
  fingerprint.

### Runtime mapping

Items are mapped per runtime each time a member starts. Generated files live
in the member's agent directory `A`, as today.

File items (skills, plugins, subagents) are snapshotted into a
content-addressed store, `.crew/toolbox/store/<sha256>/`, which members with
the same content share. The copy is a recursive copy that never follows links
and has a size limit (16 MiB per item; a larger item is refused with its
size). A sandbox mounts the store read-only. A file edited during a session
therefore changes nothing until the member restarts.

Toolbox MCP servers are always auto-approved, like `crew`. This covers Claude
Code (`mcp__<name>` added to `--allowedTools`), Codex
(`default_tools_approval_mode`) and pi. Otherwise a guarded member would stall
on a prompt nobody answers.

| Kind | Claude Code | Codex | pi | shell |
| --- | --- | --- | --- | --- |
| `mcp` | Entries in `A/mcp.json` after `crew`, in name order. Always `--strict-mcp-config`, unless `inherit.operator` includes `mcp`. | `-c mcp_servers.<name>.*` per server, in name order, given to both the TUI and its app-server. | Registered by crew's pi extension after `crew` (the `pi.rs` extension template gains the server list). | Not applicable. |
| `skills` | `skills/` of the member's generated `crew-toolbox` plugin, passed with `--plugin-dir`. Skills appear as `crew-toolbox:<name>`. With `--setting-sources project,local`, a selected skill, subagent and `SessionStart` hook loaded through `--plugin-dir` while every operator sentinel stayed absent (capability probe). | Through crew's app-server: `skills/extraRoots/set` with the store paths of the member's skills, in name order, before the first thread. It adds roots without changing configuration or home (capability probe, Codex 0.159.0 app-server). That the TUI session sees roots set on its app-server is *verify*. Without crew's app-server (terminal delivery): sandboxed, the private home's `.agents/skills/<name>`; unsandboxed, **Not available on Codex without crew's app-server**. | `--no-skills` plus one `--skill <store path>` per item, in name order. | Not applicable. |
| `plugins` | One `--plugin-dir` per plugin, in name order, after crew's own. | **Not available on Codex (Claude Code plugin format).** | **Not available on pi (Claude Code plugin format).** | Not applicable. |
| `subagents` | `agents/` of the `crew-toolbox` plugin. | Sandboxed: the private `CODEX_HOME/agents/<name>.toml`, translated; a role there appears in `spawn_agent`'s definition (capability probe, captured request). Unsandboxed: **Not available on Codex outside the sandbox**. | **Not available on pi.** | Not applicable. |
| `hooks` | `hooks/hooks.json` of the `crew-toolbox` plugin. | **Not available on Codex**: hooks from every layer accumulate, run concurrently and need hash-based trust. crew's `notify` integration is not a hook and is unaffected. | **Not available on pi** (pi extends through code extensions, which are out of scope). | Not applicable. |

How `inherit` exclusions are enforced, per runtime:

| Source kind excluded | Claude Code | Codex | pi |
| --- | --- | --- | --- |
| Operator MCP | `--strict-mcp-config` (also excludes project `.mcp.json`). | Closing step. Run `codex mcp list --json` with the member's overrides, home and worktree (no model request), then add `-c mcp_servers.<name>.enabled=false` for every listed server that is not in the toolbox, to the TUI and the app-server alike. Listed servers are named in the sources line. A table override is not enough, because it merges (current state, Codex inheritance probe). Plugin-bundled servers: *verify* that `mcp list` reports them; otherwise they are listed as cannot be excluded. | Sandboxed: excluded by the private agent directory. Unsandboxed: **Cannot be excluded on pi**. `--no-mcp` removes crew's server too. `--no-extensions` also removes native MCP, and restoring it with `-e builtin:mcp` brings back the operator's and the project's servers with crew's (capability probe, pi 1.1.0). In addition, a same-name server from the operator's `mcp.json` wins over crew's registration, so such a name is shown as Shadowed. |
| Operator skills, subagents, hooks, settings and memory | `--setting-sources project`, dropping `user` and `local`, with preferences passed through crew's `--settings`. On Claude Code 2.1.291, `--setting-sources project,local` removed a user skill, user subagent, user `CLAUDE.md` and user `SessionStart` hook, and kept the project skill (capability probe). Dropping `local` as well is *verify*. All five Claude Code kinds come from the one `user` source, so they are inherited together or not at all: a list naming only some of them loads none and reports the named ones as Not available separately on Claude Code (use all). Sandboxed: also excluded by the private home. | Skills: one `skills.config` entry `{ path = "<…>/SKILL.md", enabled = false }` for each operator skill under `HOME/.agents/skills` and `CODEX_HOME/skills`. Only exact `SKILL.md` paths work; folder paths do nothing (capability probe, Codex 0.159.0). Hooks: `-c features.hooks=false`, which empties `hooks/list`; `-c hooks={}` removes neither user nor trusted project hooks (capability probe). Subagents: `-c features.multi_agent=false`, which removes `spawn_agent` with every role (capability probe); sandboxed, the private `CODEX_HOME` removes only the user roles. Both switches also drop the project's hooks or roles, which are then reported as Not available separately on Codex. `~/.codex/AGENTS.md`: **Cannot be excluded on Codex** unsandboxed (*verify*). Bundled system skills always remain and are listed as a system source. Operator `developer_instructions`: crew stops prepending them. Sandboxed: excluded by the private home, except the instructions, which crew reads from the operator's configuration and drops under `none`. | Extensions: `--no-extensions -e builtin:mcp -e A/crew-pi.ts`. Native MCP must be re-added, or crew's own server goes with it. Skills: `--no-skills` plus `--skill` for the selected and inherited project skills, which removes every ambient skill (capability probe, pi 1.1.0). Instruction files: *verify*. |
| Operator plugins | Excluded by the same `--setting-sources project`: an enabled user plugin's skill was removed (capability probe, Claude Code 2.1.291). Plugin-bundled MCP servers never start under `--strict-mcp-config` (capability probe). | Plugin-bundled MCP servers follow the closing step, if `mcp list` reports them (*verify*). Plugin skills follow the exact-path rule. | Packages: excluded by `--no-extensions` (*verify*). Sandboxed: the closed route below. |
| Project kinds | Default: loaded, except MCP. Excluding them drops `project` from `--setting-sources`; that this also removes project skills, subagents and memory is *verify*. | Excluding project skills uses `skills.config` with exact paths, as for the operator. Project configuration needs trust, which crew does not grant. | `--no-approve` (the existing `auto_trust: false`) removes project MCP servers and skills and leaves the user ones (capability probe). With `--no-skills`, inherited project skills are passed explicitly, in name order. |

**pi, sandboxed.** The closed route on pi 1.1.0 (capability probe, seven
cases) is:

- the private `PI_CODING_AGENT_DIR`;
- `--no-approve`;
- `--no-extensions -e builtin:mcp -e A/crew-pi.ts`;
- `--no-skills` plus one `--skill` per selected skill.

It loaded only crew's MCP server and the selected skill, with no ambient
extension. Toolbox MCP servers are added by crew's extension, so the route
stays closed with them. Unsandboxed, the operator's pi MCP servers are the
one kind that cannot be excluded.

**Readiness.** Every start runs a preflight before the runtime is launched:

- **MCP servers.** The daemon runs `crew toolbox mcp <name> --preflight` for
  every MCP item. It resolves the item's references, starts or connects to the
  server, completes `initialize` and `tools/list` within 30 seconds and stops
  it. A server is therefore started twice per member start: once by the
  preflight and once behind the session's proxy.
- **File items.** Every file item is validated when it is snapshotted:
  `SKILL.md` and its front matter, `plugin.json`, the subagent front matter.

A preflight failure gives the item the status Failed preflight, with the
reason. `required` and `strict` promise exactly this: the member does not
start unless every required item passed preflight. They promise nothing about
what the runtime does afterwards.

**Item statuses**, shown in the same words everywhere:

| Status | When | Stops launch? |
| --- | --- | --- |
| Planned | `crew toolbox check`, or a member not started yet: the item maps onto the runtime, nothing has been run. | Not a launch status |
| Active | Mapped and passed preflight at the last start. | No |
| Not available on <Runtime> | The runtime cannot load this item; the detail gives the reason (format, outside the sandbox). | If `required` or `strict` |
| Not applicable | The item's `runtimes` excludes this runtime, or the member is a shell member. | Never |
| Needs $NAME | A reference did not resolve. | If `required` or `strict` |
| Blocked by the sandbox | The program is outside the bound trees, or the server needs a Unix socket the sandbox does not pass (scheduler's survey: `docker.sock`, `ssh-agent`). | If `required` or `strict` |
| Shadowed by <source> | A runtime source defines the same name and wins (pi). | If `required` or `strict` |
| Failed preflight | The preflight above failed. | If `required` or `strict` |
| Failed in session | After launch, the proxy saw the server exit, or the runtime reported a load error (when it surfaces one). | No. The session keeps running, the orchestrator is told, and the reason is shown. |

**Source statuses:**

- **Not used**: excluded exactly.
- **Used: <kinds>**: inherited by choice.
- **Disabled: <names>**: excluded entry by entry (Codex closing step, skill
  paths).
- **Not available separately on <Runtime>**: the runtime can load the kind
  only together with others that are excluded, so it is not loaded.
- **Cannot be excluded on <Runtime>**: the runtime loads it although `inherit`
  excludes it. It stops the launch under `strict`.

**The contract has two parts:**

- **Toolbox items are exact.** crew renders no item that is not in the
  effective toolbox, and every configured item that is not Active is shown
  with its status. If a runtime cannot load exactly the requested subset of a
  kind, crew loads less and reports the difference.
- **Exclusion of runtime sources is best effort by default and exact under
  `strict`.** crew excludes every source the runtime lets it exclude. What
  remains is listed by name as Cannot be excluded, never hidden. With `strict`,
  a member with any such remainder does not start. The default stays best
  effort because an exact default would stop unsandboxed Codex and pi members
  outright today. A team that needs the guarantee sets `strict` or uses the
  sandbox.

### Lifecycle

- **When a toolbox is applied.** The effective toolbox is resolved and
  rendered every time a member's process starts: spawn, crash restart,
  `crew resume`, `crew resume --restart`, and a runtime switch through a model
  fallback or `agent_set_model`.
- **Where the definitions come from.** The definitions are the daemon's team
  configuration (`session.team`, kept since `crew run`), not the team file on
  disk; `launch.rs` reads endpoints afresh, but not the team file.
  `crew toolbox reload` rereads the team file's `toolboxes` and `toolbox`
  fields, validates them and stores them in the daemon's copy. It never
  touches other fields and says so if they differ. Members whose fingerprint
  would change show **Toolbox changed: restart to apply**, and
  `toolbox.pending` records both fingerprints. A running session never changes
  toolbox.
- **Active and configured.** Each member has an **active toolbox** and a
  **configured toolbox**:
  - The active toolbox is what its running session was started with: full
    item definitions, file content hashes, MCP definition snapshots and the
    fingerprint. It is stored in the member's local state (`AgentLocal`).
  - The configured toolbox is what the daemon's current definitions resolve
    to.

  Every process start makes the configured toolbox active. This covers
  crash restarts and runtime switches too, so after a reload the next start of
  any kind applies the change. File sources are re-read at each start, and
  their content is part of the configured definition. MCP definition
  snapshots are server output, not configuration, so they stay pinned across
  starts (Prompt cache rules, rule 5).
- **Events at each start.** Each start records `toolbox.applied`, with the
  fingerprint, the definition digest, the item counts per kind and every
  status that is not Active. A start whose fingerprint or definition digest
  differs from the previous session's also records `toolbox.changed`, so cache
  measurements can attribute the expected reset.
- **Resume after a change.** A restart after a change resumes the
  conversation; it is not a fresh session. A transcript can then mention tools
  that are no longer configured. Each runtime must keep working in that case
  (T18). Where one does not, the change applies with a fresh session that is
  handed the work, as a cross-runtime fallback is today.
- **Runtime switch.** `daemon/fallback.rs` clears runtime-specific fields on a
  runtime switch (`permission_mode`, `allowed_tools`, `extra_args`). It must
  keep the toolbox, which is runtime-neutral and mapped again for the new
  runtime. Daemon recovery and model fallback re-resolve settings
  (`creation.rs`), and they resolve the toolbox the same way.

### Prompt cache rules

These rules are inputs to scheduler's `cache-prompt-audit` and
native-architect's `cache-design`. They make the toolbox a constant of a
session. Sorting generated configuration is necessary, but it does not decide
the runtime's final tool order (current state). The benchmark measures that.

1. **Canonical form.** The resolved toolbox is serialized with kinds in the
   fixed order `mcp`, `skills`, `plugins`, `subagents`, `hooks`, items sorted
   by name (byte order) and object keys sorted. File items carry their content
   hash instead of their source path. Nothing carries a time, a process id or
   a random value. The fingerprint is
   `sha256(canonical form ‖ runtime ‖ mapping version)`. Order is preserved
   only where it means something: argument arrays and the order of crew's own
   components.
2. **Byte-stable rendering, per member.** Generated artifacts are
   byte-identical across spawn, restart and resume of the same member, given
   the same fingerprint and the same launch prerequisites (crew binary path,
   agent directory, identity file path, endpoint). That covers:
   - `A/mcp.json`, with servers in name order;
   - the `crew-toolbox` plugin and the `--plugin-dir` arguments;
   - Codex `-c` arguments in name order, including the closing step's
     `enabled=false` entries;
   - pi registration and `--skill` order;
   - environment lists in name order.

   These files hold member-specific paths (the identity file in every server
   entry), so they are not compared across members. Paths are stable (the
   agent directory and the content store), never temporary. Filesystem
   enumeration order and server connection order never choose model-visible
   order.
2a. **Model-visible definitions, across members.** crew adds nothing
   member-specific to what the model sees from a toolbox. It never adds a
   member id, identity path, agent directory or worktree path to the proxy's
   tool definitions, and it renders file items from content-addressed store
   paths. Two members' toolbox definitions are equal only when the evidence
   shows it, not because their fingerprints and roles match. The evidence is:
   - the same fingerprint and role;
   - the same definition digest, meaning byte-equal pinned snapshots;
   - equal native catalog listings from the runtime.

   References with the same names can resolve to different accounts, and an
   upstream server can serve different definitions to each. Those
   definitions stay distinct; the proxy sorts them but never rewrites them to
   match. The inspector and `crew toolbox` show the definition digest so the
   comparison is visible.

   **Gap to probe.** Claude Code's skill and plugin catalog may name the
   plugin directory crew passes with `--plugin-dir`. Today that directory is
   `A/crew-toolbox`, a member path. Until a native-listing probe shows which
   paths the catalog renders, the `crew-toolbox` plugin is not claimed free of
   member paths. If the catalog renders the path, the plugin moves into the
   content store under a digest of its rendered contents. Codex skill roots
   and pi `--skill` paths point into the store already, and the same probe
   confirms what their catalogs show. QA compares served definitions and
   native listings, never `A/mcp.json`.
3. **No secrets in rendered bytes**, so rotating a credential does not
   invalidate a cached prefix (see Secrets).
4. **crew's own tools first and constant.** This is confirmed by the current
   state and by scheduler's audit: `mcp/tools.rs::definitions` is a fixed
   vector, filtered by role without reordering, with no identities or live
   state in `tools/list`, and `listChanged: false`. Toolbox servers come after
   `crew` and change nothing in it.

   One gap remains for `cache-prompt-audit` to fix. `mcp/mod.rs:45-64` looks
   the role up from the daemon at `tools/list` time and falls back to the
   worker list when the daemon is unreachable, so an orchestrator can get a
   different list on one launch. The role must come from the launch identity,
   not a live call.
5. **Stable MCP tool definitions.** Third-party servers can reorder tools,
   change descriptions or connect late. When tools are declared on every
   request, that changes the definitions mid-session and breaks the cached
   prefix. Deferral cannot be assumed. Claude Code defers only under
   conditions: `ENABLE_TOOL_SEARCH`, a first-party base URL, and no server
   `alwaysLoad` or tool `_meta` `anthropic/alwaysLoad`. A gateway endpoint, or
   a server that asks for `alwaysLoad`, puts the definitions in the tools
   prefix. crew's MCP launcher (`crew toolbox mcp <name>`) therefore proxies
   every toolbox server, local and remote alike. A local server runs as the
   launcher's child, never in its place. The launcher:
   - It returns `tools/list` sorted by tool name.
   - It never advertises `listChanged` to the runtime.
   - It answers `initialize` and `tools/list` from a snapshot pinned for the
     session and stored in `A`. A slow or reordered upstream therefore cannot
     change what the runtime sees.

   The definition digest is the SHA-256 of the pinned snapshots, in server
   order. The first snapshot comes from the preflight. A snapshot is refreshed
   only when the item's definition changes or on `crew resume --restart`.
   Otherwise a later preflight listing that differs is logged and does not
   replace it. A refresh that changes the digest records
   `toolbox.changed` even when the fingerprint is unchanged. Tools that
   appear upstream later are logged and are not exposed until a refresh. A
   call to a tool that disappeared returns the upstream error. Deferred
   loading (Claude Code's MCP tool search, pi's deferred exposure) keeps
   toolbox definitions out of the declared prefix; loaded schemas land in
   messages instead. Whether toolbox servers should use deferred exposure is
   decided by `cache-design` after the baseline and audit. The proxy remains
   for secrets and stable ordering either way. The requirement stays: no
   definition changes within a session.

   Operator servers inherited with `inherit.operator: [mcp]` (or the legacy
   `inherit_mcp`) do not go through the proxy. crew does not control their
   definitions, and the member's inspector lists them as **outside the
   cache rules**.
6. **Size is visible.** Per member, crew reports the number of tools and the
   bytes of tool definitions its toolbox adds (from the pinned snapshots). The
   inspector, `crew toolbox` and the cache benchmark show them, so a large
   toolbox is a measured choice.
7. **A toolbox change is a deliberate cache reset**, recorded as
   `toolbox.changed` (see Lifecycle).

### Orchestrator tools

The orchestrator chooses bundles; it cannot define items. Defining an MCP
command or a hook from inside a model session would let a model run arbitrary
programs as the operator.

- **`agent_add`** gains `toolbox`: an optional list of bundle names from the
  team's `toolboxes`.
  - The new member gets `defaults.toolbox` plus those bundles. The selection is
    saved with its roster entry (`AgentSpec.toolbox.include`).
  - An unknown name is refused: `Unknown toolbox "webb". Toolboxes in this
    team: base, github, web.`
  - The result carries the summary, for example
    `toolbox 3f9a0c21e7b4: mcp context7, playwright; skills frontend-qa; not available on Codex: plugins.lint-kit`.
- **`agent_spawn`** gains the same optional `toolbox`. Today
  `daemon/subagents.rs` starts a subagent from team defaults only.
  - **Default.** A subagent gets a copy of its parent's **active** toolbox:
    the full item definitions, the same content-store entries and the same
    MCP definition snapshots. It does not get the parent's bundle names
    resolved again. A `toolbox reload` that is pending for the parent
    therefore does not reach the subagent until the parent restarts.
  - **Requested bundles.** These resolve against the daemon's current
    definitions. Every resulting item must equal an item in the parent's
    active toolbox: the same definition, the same content hash and the same
    snapshot digest. Otherwise the request is refused, naming the first item
    that differs. A worker cannot grant more than it has, including a bundle
    that a reload has since widened.
- **`agent_set_model`.** A model on the same runtime keeps the toolbox and its
  fingerprint. A model on another runtime maps the toolbox again at the
  switch, and the result lists every item that is not Active there. If a
  `required` item would be unavailable, the switch is refused with that
  item's reason. Fallbacks crew makes by itself follow the same rule and tell
  the orchestrator.
- **`agents_list`** shows each member's fingerprint and its count of statuses
  that are not Active, for example `toolbox 3f9a0c21e7b4 (1 not available)`.
- **The input schemas** of these tools accept no item definitions.

### Display

There is one shape everywhere. The protocol `Agent` (`crew-protocol`
`types.rs`) gains an optional `toolbox`, and native clients get it through the
FFI records. It has these fields:

- `fingerprint`
- `definition_digest`
- `pending_fingerprint`, when a restart would change the toolbox
- `items`, each with `kind`, `name`, `origin`, `status` and an optional
  `detail`
- `sources`, each with `kind`, `source`, `status` and an optional `names`
- `tool_count` and `definition_bytes`

Secret values are never in it. References appear as `$NAME` or
`$NAME (not set)`.

**CLI.**

- `crew toolbox [<agent>] [--runtime <kind>] [--json]` prints the effective
  toolbox, for every member when no agent is named. `--runtime` shows how the
  toolbox would map on another runtime.
- `crew toolbox check` validates the team file and resolves every reference
  and source without starting anything. It exits non-zero on an error or on
  any status that would stop a launch.
- `crew toolbox reload` is described under Lifecycle.

The JSON output is the protocol structure, which is what QA asserts on.

**Inspector (web, TUI, Linux and Apple native).** The agent inspector gets a
**Toolbox** section after the runtime and model facts.

- A summary line: `Toolbox 3f9a0c21e7b4 · 4 items · 1 not available`. The
  fingerprint is monospaced and selectable.
- When a change is pending, the notice **Toolbox changed: restart to apply**,
  offering **Restart…** through the existing restart confirmation.
- Groups headed **MCP servers**, **Skills**, **Plugins**, **Subagents** and
  **Hooks**, each shown only when it has entries. Each row shows:
  - the name;
  - the origin: **Team default**, **Bundle web** or **Member**;
  - the status, in words.

  An MCP row's detail adds its command or URL, its references and its tool
  count.
- A **Sources** group, one row per runtime source, for example:
  - **Your Claude Code configuration: not used**
  - **Your Claude Code configuration: MCP servers**
  - **Your subagents: cannot be excluded on Codex without the sandbox**
  - **Your Codex MCP servers: disabled (ambient, docs-local)**
  - **Project: skills, hooks, instructions**
- The definition digest and tool-definition bytes sit in the detail, not in
  the summary.

**Accessibility and layout:**

- Status is always text; colour, if used, only repeats it.
- Group headings are headings to assistive technology.
- Long commands and URLs wrap or are middle-ellipsized, with the full value in
  the tooltip and accessible description, like workspace paths (R2 in the
  native sign-off).

**TUI.** The detail view shows the summary line and the pending notice. The
existing detail expansion shows the groups, so there is no new key.

**Agent graph and team list.** No new chrome. A member with a required item
that is not Active, or with a pending change, gets the existing attention
marker, with the reason in its tooltip and accessible description.

### Acceptance cases

QA runs these against a fixture team. The fixture's stub MCP servers report
their environment's variable names (never values) and serve a fixed tool list.
There is a synthetic operator home with a sentinel of every kind: MCP server,
skill, subagent, hook and enabled plugin. Shell stubs stand in for `claude`,
`codex` and `pi` where only the generated launch is asserted. Native
discovery is asserted with the runtimes' own listing commands where they have
one (`codex mcp list --json`, `codex debug prompt-input`). pi results must
come from the supported pi, not the GSD binary on this host.

| ID | Case | Pass when |
| --- | --- | --- |
| T01 | Team file without any `toolbox` key, Claude member, unsandboxed | `A/mcp.json` holds only `crew`; `--strict-mcp-config` and `--setting-sources project` are present; `crew toolbox --json` shows no items and lists the operator source as Not used. The sentinel user skill, subagent, enabled plugin, `CLAUDE.md` and hook do not reach the session; a project skill does. |
| T02 | The example team above | `crew toolbox --json` for `qa` lists `mcp.context7` (bundle base via default), `mcp.playwright` (bundle web, required) and `skills.frontend-qa`. For `release` on Codex, unsandboxed: `mcp.github` and `mcp.docs` are Active; `skills.changelog` is Active through crew's app-server (with `delivery: terminal`, Not available on Codex without crew's app-server); `plugins.lint-kit` is Not available on Codex (Claude Code plugin format); `mcp.context7` is excluded. |
| T03 | Reorder keys, items and bundles without changing meaning | Same fingerprint, byte-identical generated files for the same member,. A second member with the same selection, role and stub account has the same definition digest and an equal native catalog listing (rule 2a). A stub that serves member-specific tools gives a different digest, and the two definitions stay distinct. Until the Claude catalog probe passes, the `crew-toolbox` plugin path is excluded from this comparison and listed as the open gap. |
| T04 | Validation | Each of these stops `crew run` and `crew toolbox check` with its field path: a bundle cycle; an unknown bundle; an exclusion matching nothing at its level; the same exclusion listed twice at one level (the second matches nothing); an item defined and excluded at one level; a reserved name; a plain string where a reference belongs; `inherit_mcp` together with `inherit`; a reference or source (alias) name that collides with a model credential or an endpoint variable. Accepted without error: defaults exclude `mcp.context7` from bundle base, and the member includes bundle base again, so `mcp.context7` is back with origin `bundle:base`. Accepted: defaults define `mcp.docs`, and the member defines `mcp.docs` differently, so the member's definition wins whole. Accepted: the member has `reset: true`, and defaults' `exclude`, `inherit` and `strict` do not apply to it. |
| T05 | Secrets | Four sentinels are used. `SHOP_GITHUB_TOKEN` comes from `[agents.release.toolbox_env]` as a file. `DOCS_TOKEN` is exported in the daemon's environment. `SHOP_TOKEN = { env = "DAEMON_TOKEN" }` aliases a daemon variable, with `DAEMON_TOKEN` exported. `ENDPOINT_SENTINEL` is in the member's endpoint `env_files`. The first three values reach only their stub server's process, through the proxy (`DOCS_TOKEN` through the remote bridge). The aliased value arrives there as `SHOP_TOKEN`, and neither `SHOP_TOKEN` nor `DAEMON_TOKEN` is in the member session's environment. They are absent from: `A`, `A/settings.json`, the store, the roster, the event log, `crew toolbox` output, every dashboard payload, every argv (`/proc/*/cmdline`), and the member session's own environment, both unsandboxed (PTY) and sandboxed, including the Codex app-server. A toolbox reference to `ENDPOINT_SENTINEL`, and an alias whose source is `ENDPOINT_SENTINEL` or `OPENAI_API_KEY`, are validation errors. A stub that echoes its token in an error shows `[redacted $SHOP_GITHUB_TOKEN]` to the model. |
| T06 | Missing variable | For an item that is not required, the member starts and the item shows Needs $SHOP_GITHUB_TOKEN. For a required item, or under `strict`, the member does not start, and the inspector and the orchestrator's result give the same text. |
| T07 | Stability over the lifecycle | Spawn, crash restart and `crew resume` give the same fingerprint and definition digest, and byte-identical `A/mcp.json`, plugin directory hashes, Codex `-c` arguments, pi arguments and pinned snapshots. |
| T08 | Reload while running | After the team file is edited, `crew toolbox reload` leaves running sessions untouched. Affected members show the pending notice and `toolbox.pending` appears. A restart applies the change, and `toolbox.changed` carries both fingerprints. An edit to a field other than a toolbox field is reported and not applied. |
| T09 | Snapshots | A `SKILL.md` edited mid-session changes nothing in the session. After a restart, the new content hash is in the fingerprint. An item over 16 MiB is refused with its size. |
| T10 | Runtime switch | `agent_set_model` from a Claude Code model to a Codex model lists the items that are not Active on Codex. A `required` one refuses the switch with its reason. A model change within the runtime keeps the fingerprint. |
| T11 | `agent_add` and `agent_spawn` | A bundle selection is applied and saved to the roster. An unknown bundle is refused with the copy above. The input schemas reject item definitions. A subagent gets a copy of its parent's active toolbox: equal definitions, content hashes and snapshot digest. After a reload that widens bundle `web` while the parent still runs the old one: a default subagent gets the old items, and a request for `web` is refused, naming the first new item. |
| T12 | Operator sources, two members | Two members with different selections, with the sentinel operator home. Under `none`, unsandboxed, without `strict`: on each runtime, the only sentinels that reach a member are the ones its sources list by name as Cannot be excluded. That is best effort, visible. Codex sentinel servers appear as Disabled, and the native listing shows them disabled. Codex sentinel hooks and roles are absent from `hooks/list` and `spawn_agent`. Sandboxed (pi: the closed route): no operator sentinel reaches either member, and no source is listed as Cannot be excluded. With `inherit.operator: [mcp]`, only the operator's MCP sentinel reaches the member, listed as Used and outside the cache rules. Operator files are byte-unchanged afterwards. |
| T13 | Strict | With `strict: true` and an unsandboxed source that cannot be excluded, the member does not start, and the reason is shown. |
| T14 | Proxy stability (rule 5) | The stub upstream returns tools in a random order and changes one description after the first listing. The runtime sees sorted tools and the pinned descriptions for the whole session, and `listChanged` is never advertised. After `crew resume --restart` the new description appears, the definition digest changes and `toolbox.changed` is recorded with the fingerprint unchanged. |
| T15 | Dashboards | For the T02 fixture, the web inspector, TUI detail, GTK inspector (AT-SPI names) and Apple detail (source or unit level, while Apple execution stays deferred) show the same groups, origins, statuses and sources. Status is text, group headings are headings, and long commands do not widen the pane. |
| T16 | Cache attribution | Precondition: the pinned tool-definition snapshots and generated artifacts are byte-identical before and after a restart (T07). Then measure, in `cache-bench`, paired warm sessions for the same member, model and endpoint, using runtime-native usage over repeated equivalent turns within the cache lifetime. Report cache read and write per turn, with the cache TTL, model and routing (endpoint, region) stated. Aggregate token counts alone do not pass this case. A deliberate toolbox change shows one `toolbox.changed` and the expected reset. |
| T17 | Shell members | A toolbox on a shell member shows every item as Not applicable and does not stop the launch, even under `strict`. |
| T18 | Resume after removal | On each runtime, remove an MCP server that was used earlier in a session, reload and restart. The resumed session continues, and the history's earlier calls to the removed tool cause no error. If one runtime fails, that runtime's change path uses a fresh session that is handed the work, and the case asserts that path instead. |
| T19 | Sandbox | A server whose program is outside the bound trees shows Blocked by the sandbox. Store items are readable and read-only inside the sandbox. Files left in the private home's toolbox directories by an earlier session are gone after a restart. |
| T20 | Readiness | `crew toolbox check` shows mapped items as Planned, never Active. Four stub behaviours are checked. A required stub that exits during `initialize` gives Failed preflight, and the member does not start. A required stub that does not answer `tools/list` within 30 seconds gives the same result. The same stub without `required` gives Failed preflight, and the member starts. A stub that passes preflight and exits later in the session gives Failed in session; the session keeps running and the orchestrator is told. A required skill without `SKILL.md` gives Failed preflight, and the member does not start. |
| T21 | Cache rule gaps | The orchestrator's crew tool list is identical when the daemon is briefly unreachable at `tools/list` time, because the role comes from the launch identity. A server that sets `alwaysLoad`, or a member on a gateway endpoint, still sees pinned, sorted definitions. An operator server inherited with `[mcp]` is listed as outside the cache rules. |

### Implementation split

One card per slice. Owners are assigned by the orchestrator. Slices 2 to 4
need slice 1. Probes for the *verify* items are their own card, ahead of the
slice they unblock.

1. **Schema and resolution (`crates/crew-core`).**
   - `config.rs`: `Toolbox`, the item types, `Reference`, `Inherit`;
     `toolboxes` on `TeamConfig` and `Style`, carried by `Style::team`;
     `toolbox` on `AgentDefaults`, `OrchestratorConfig` and `AgentSpec`.
   - Path absolutization at load, including the style origin from
     `crates/crew/src/styles.rs`.
   - Validation beside `validate_agent_spec`.
   - A new `toolbox.rs` with the pure resolution, canonical form and
     fingerprint, and unit tests for T03 and T04.
   - The `inherit_mcp` mapping.
2. **Launch per runtime (`crates/crew/src/agents`, `daemon`).**
   - `agents/toolbox.rs`: content store (descriptor-relative copy that never
     follows links, size limit), `crew-toolbox` plugin rendering, statuses and
     source enumeration.
   - The `claude.rs`, `codex.rs` (TUI and app-server, closing step, skill
     paths), `pi.rs` (extension server list, `--no-skills`/`--skill`,
     `--no-extensions`) and `shell.rs` mappings.
   - The resolved toolbox in `AgentSettings` (`agents/mod.rs`) through
     `daemon/supervisor/mod.rs::agent_settings`.
   - Reference resolution through `endpoints.rs`.
   - The orchestrator conversion (`daemon/team.rs`); the parent's selection
     for subagents (`daemon/subagents.rs`); `fallback.rs` and `creation.rs`
     keeping the toolbox.
   - The roster saving `toolboxes` and selections (`daemon/roster.rs`);
     `crew toolbox reload` against `session.team`.
   - The events `toolbox.applied`, `toolbox.pending` and `toolbox.changed`.
   - Launch-spec tests for T01, T05–T10, T12, T13, T17 and T19.
3. **MCP and CLI (`crates/crew/src/mcp`, `cli`).**
   - The `toolbox` parameters on `agent_add` and `agent_spawn`, and the
     `agent_set_model` result (`mcp/tools.rs`, `daemon/handlers.rs`).
   - The `agents_list` summary.
   - `crew toolbox`, `crew toolbox check` and `crew toolbox reload`.
   - `crew toolbox mcp <name>`, covering:
     - the secret fetch over the identity;
     - local servers as forwarded children;
     - a stdio-to-HTTP bridge for remote servers;
     - sorting, pinned snapshots and the definition digest;
     - best-effort redaction;
     - `--preflight`.
   - Tests for T11, T14, T18, T20 and T21.
4. **Protocol and dashboards.**
   - `crew-protocol` `Agent.toolbox`.
   - Web `views/Inspector.tsx` and `protocol.ts`.
   - TUI `views/detail.rs`.
   - Desktop `views/agent.rs`.
   - FFI `records/shared.rs` and Apple `CrewAgentDetail.swift`.
   - T15.
5. **Docs and QA.**
   - The README team file sample, a Toolboxes section (including the
     old-binary note) and an update to the sandbox credentials section.
   - The fixture team, stubs and sentinel home.
   - T16 in `cache-bench`.
   - The screenshot gallery entries.

Slice 3's proxy may land after slices 1, 2 and 4. Until it lands:

- Remote servers with `headers` references show **Not available (crew bridge
  pending)**.
- Local servers get their environment through an interim exec path of
  `crew toolbox mcp`, which replaces itself with the server, without
  forwarding, sorting, pinning or redaction.
- Preflight covers file items only, so `required` on an MCP item checks
  references and the sandbox, not readiness.
- The definition digest is reported as unavailable.

**The cache guarantee is incomplete in that state.** Rules 1 to 4 hold, but
rules 2a and 5 do not hold for MCP definitions, and rule 6 has no tool bytes.
T14, T16 and T20 cannot pass. The inspector and `crew toolbox` say **MCP
definitions not pinned (proxy pending)** for every member with a toolbox MCP
server.

### Open items

**Probes to run** (graph's `capability-probes` card). Each probe either
confirms its cell or turns it into a Cannot be excluded or Not available
entry. No cell may be implemented as Active or Not used on an unconfirmed
mechanism.

- Claude Code:
  - `project` alone (without `local`);
  - project exclusion;
  - which paths the skill and plugin catalog renders for a `--plugin-dir` plugin (rule 2a gap).
- Codex:
  - whether `codex mcp list --json` reports plugin-bundled servers;
  - whether the TUI session sees app-server skill roots;
  - `~/.codex/AGENTS.md` unsandboxed.
- pi 1.1.0:
  - instruction files;
  - packages.

Already settled by graph's probes and folded in above:

- Claude Code: user sources, user plugins, plugin MCP under strict loading,
  selected skill, subagent and hook through `--plugin-dir`.
- Codex: MCP merge, skill paths, app-server skill roots, `features.hooks`,
  `features.multi_agent`.
- pi 1.1.0: skills, extensions and native MCP, `--no-approve`, the closed
  sandbox route, no native subagents.

**Decided: personal instructions under `none`** (orchestrator, m-dd4748765038).
`inherit.operator: none` keeps the operator's personal instructions (user
`CLAUDE.md`, `~/.codex/AGENTS.md`, the operator's Codex
`developer_instructions`) away from members. This matches what Claude Code
members already get with a private `CLAUDE_CONFIG_DIR`. The operator has none
of these files or settings today, so the default changes nothing in
practice. The opt-in is `inherit.operator: [memory]`:

- Codex and pi: keeps the personal instructions alone.
- Claude Code: personal instructions come only with all user kinds, that is
  `inherit.operator: all`. `--setting-sources` loads or drops the whole `user`
  source. Following the rule of never more than configured, a partial list
  such as `[memory]` loads none of the user kinds. It reports the named ones as
  **Not available separately on Claude Code (use all)**. The README says so
  beside the field.
