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
   file asks for it. Anything a runtime still loads that crew cannot exclude is
   listed as inherited, never hidden.
3. One configuration serves every runtime. An item a runtime cannot load is
   reported per member, and a member that needs the item can refuse to start
   instead.
4. Secrets never appear in the team file, the roster, the event log, generated
   files, command lines, prompts or dashboards. The team file names
   environment variables, and values are resolved at launch, in memory.
5. A toolbox is fixed for the life of a session. Changing it means restarting
   the member.
6. The same toolbox renders the same bytes on every spawn, restart and resume,
   in a deterministic order, so it does not break a cached prompt prefix by
   itself.

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
| `plugins` | `path` | A Claude Code plugin directory (`.claude-plugin/plugin.json`). It is implicitly `runtimes: [claude]`. A plugin's bundled MCP servers and hooks are part of the plugin. Whether they survive strict MCP loading is *verify*. Until confirmed, a plugin with a bundled `.mcp.json` is shown with the detail "bundled MCP servers: declare them under mcp". Codex's own plugin format is not supported in this version. |
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

One pure function in `crew-core` computes a member's effective toolbox, in
this order:

1. **Base.** Start from `defaults.toolbox`, or from nothing when the member's
   `toolbox.reset` is true. The orchestrator is a member like the others: its
   level is `orchestrator.toolbox`. The conversion of `OrchestratorConfig`
   into a spec (`daemon/team.rs:231`) must carry that level.
2. **Expand.** Expand a level's `include` list depth-first, in the order
   listed: a bundle's own includes come before its own items. Then add the
   level's own items. A later definition with the same `<kind>.<name>`
   replaces the earlier one whole. There is no field-by-field merge.
3. **Levels.** Apply `defaults.toolbox` (unless reset), then the member's
   level, each expanded as above.
4. **Exclude.** Apply the member level's `exclude`, then `defaults.toolbox`'s
   `exclude`, which removes items from bundles that defaults included. An
   exclusion that matches nothing is a validation error (it catches typos), and
   so is an item defined and excluded at the same level.
5. **Inherit.** `inherit` is taken whole from the most specific level that sets
   it (the member's, then the defaults'), never merged key by key.
6. **Strict.** `strict` is true if any applied level sets it.
7. **Map.** Map every item onto the member's runtime (see Runtime mapping) and
   give it a status.

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
- **Lookup.** References resolve when the member starts. They are looked up
  first in the member's crew configuration, using the same `env` and
  `env_files` lookup as endpoint variables in `endpoints.rs`: `[agents.<id>]`,
  then the parent's section for a subagent, then `[claude]` or `[codex]`.
  Failing that, they come from the daemon's environment. Values live only in
  the daemon's memory, as endpoint secrets do today.
- **Delivery.** Every toolbox MCP server is launched by
  `crew toolbox mcp <name> --identity <identity file>`. Like
  `crew credential` for `apiKeyHelper` today, it asks the daemon over the
  member's identity for that server's resolved environment and headers. A
  local server is then exec'ed with that environment. A remote server is
  bridged by crew with those headers (see Prompt cache rules, rule 5). This
  also satisfies the sandbox, which passes only the variables named in
  `LaunchSpec.env`. The secrets never need to be among them.
- **Never written.** A resolved value never appears in:
  - a generated file (`mcp.json`, settings, plugin directories, Codex `-c`
    arguments, pi registration);
  - a command line;
  - the member session's own environment;
  - the roster, the event log, a prompt or a dashboard.
- **Not an isolation boundary.** This is not one. Any process holding the
  member's identity can ask for the same values, and a process of the same
  user can read another process's environment. What the rule does is keep
  secrets out of files, argv, transcripts and prompts. Those are the places
  where agents, logs and caches would otherwise copy them.
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
| `skills` | `skills/` of the member's generated `crew-toolbox` plugin, passed with `--plugin-dir`. Skills appear as `crew-toolbox:<name>`. | Through crew's app-server: `skills/extraRoots/set` with the store paths of the member's skills, in name order, before the first thread. It adds roots without changing configuration or home (capability probe, Codex 0.159.0 app-server). That the TUI session sees roots set on its app-server is *verify*. Without crew's app-server (terminal delivery): sandboxed, the private home's `.agents/skills/<name>`; unsandboxed, **Not available on Codex without crew's app-server**. | `--no-skills` plus one `--skill <store path>` per item, in name order. | Not applicable. |
| `plugins` | One `--plugin-dir` per plugin, in name order, after crew's own. | **Not available on Codex (Claude Code plugin format).** | **Not available on pi (Claude Code plugin format).** | Not applicable. |
| `subagents` | `agents/` of the `crew-toolbox` plugin. | Sandboxed: the private `CODEX_HOME/agents/<name>.toml`, translated. Unsandboxed: **Not available on Codex outside the sandbox**. | **Not available on pi.** | Not applicable. |
| `hooks` | `hooks/hooks.json` of the `crew-toolbox` plugin. | **Not available on Codex**: hooks from every layer accumulate, run concurrently and need hash-based trust. crew's `notify` integration is unaffected. | **Not available on pi** (pi extends through code extensions, which are out of scope). | Not applicable. |

How `inherit` exclusions are enforced, per runtime:

| Source kind excluded | Claude Code | Codex | pi |
| --- | --- | --- | --- |
| Operator MCP | `--strict-mcp-config` (also excludes project `.mcp.json`). | Closing step. Run `codex mcp list --json` with the member's overrides, home and worktree (no model request), then add `-c mcp_servers.<name>.enabled=false` for every listed server that is not in the toolbox, to the TUI and the app-server alike. Listed servers are named in the sources line. A table override is not enough, because it merges (current state, Codex inheritance probe). Plugin-bundled servers: *verify* that `mcp list` reports them; otherwise they are listed as cannot be excluded. | Sandboxed: excluded by the private agent directory. Unsandboxed: **Cannot be excluded on pi**. `--no-mcp` also removes crew's server, and a same-name server from the operator's `mcp.json` wins over crew's registration, so such a name is shown as Shadowed. |
| Operator skills, subagents, hooks, settings and memory | `--setting-sources project`, dropping `user` and `local`, with preferences passed through crew's `--settings`. On Claude Code 2.1.291, `--setting-sources project,local` removed a user skill, user subagent, user `CLAUDE.md` and user `SessionStart` hook, and kept the project skill (capability probe). Dropping `local` as well is *verify*. All five Claude Code kinds come from the one `user` source, so they are inherited together or not at all: a list naming only some of them loads none and reports the named ones as Not available separately on Claude Code (use all). Sandboxed: also excluded by the private home. | Skills: one `skills.config` entry `{ path = "<…>/SKILL.md", enabled = false }` for each operator skill under `HOME/.agents/skills` and `CODEX_HOME/skills`. Only exact `SKILL.md` paths work; folder paths do nothing (capability probe, Codex 0.159.0). Subagents (`~/.codex/agents`), hooks (`-c hooks={}` removes neither user nor trusted project hooks) and `~/.codex/AGENTS.md`: **Cannot be excluded on Codex** unsandboxed. Bundled system skills always remain and are listed as a system source. Operator `developer_instructions`: crew stops prepending them. Sandboxed: excluded by the private home, except the instructions, which crew reads from the operator's configuration and drops under `none`. | Extensions: `--no-extensions` plus crew's explicit `--extension`. Skills: `--no-skills`, with toolbox and project skills passed explicitly. Instruction files: *verify*. |
| Operator plugins | Excluded by the same `--setting-sources project`: an enabled user plugin's skill was removed (capability probe, Claude Code 2.1.291). Plugin-bundled MCP servers are covered by `--strict-mcp-config`; that is *verify* only for plugins the toolbox itself loads. | Plugin-bundled MCP servers follow the closing step, if `mcp list` reports them (*verify*). Plugin skills follow the exact-path rule. | Packages: excluded by `--no-extensions` (*verify*). |
| Project kinds | Default: loaded, except MCP. Excluding them drops `project` from `--setting-sources`; that this also removes project skills, subagents and memory is *verify*. | Excluding project skills uses `skills.config` with exact paths, as for the operator. Project configuration needs trust, which crew does not grant. | `--no-approve` (the existing `auto_trust: false`) excludes project resources. With `--no-skills`, inherited project skills are passed explicitly, in name order. |

**Item statuses**, shown in the same words everywhere:

| Status | When | Stops launch? |
| --- | --- | --- |
| Active | Mapped and loaded. | No |
| Not available on <Runtime> | The runtime cannot load this item; the detail gives the reason (format, outside the sandbox). | If `required` or `strict` |
| Not applicable | The item's `runtimes` excludes this runtime, or the member is a shell member. | Never |
| Needs $NAME | A reference did not resolve. | If `required` or `strict` |
| Blocked by the sandbox | The program is outside the bound trees, or the server needs a Unix socket the sandbox does not pass (scheduler's survey: `docker.sock`, `ssh-agent`). | If `required` or `strict` |
| Shadowed by <source> | A runtime source defines the same name and wins (pi). | If `required` or `strict` |
| Failed to start | Reported by the runtime after launch (server exit, plugin load error), when the runtime surfaces it. | No; shown with the runtime's reason |

**Source statuses:**

- **Not used**: excluded exactly.
- **Used: <kinds>**: inherited by choice.
- **Disabled: <names>**: excluded entry by entry (Codex closing step, skill paths).
- **Cannot be excluded on <Runtime>**: stops the launch under `strict`.

The rule for any mismatch: **never more than configured, and every shortfall
visible**. If a runtime cannot load exactly the requested subset of a kind,
crew loads less (never the operator's whole set) and reports the difference.

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
2. **Byte-stable rendering.** For the same fingerprint, every generated
   artifact is byte-identical across spawn, restart and resume. That covers
   `A/mcp.json` with servers in name order, the `crew-toolbox` plugin,
   `--plugin-dir` arguments, Codex `-c` arguments in name order (including
   the closing step's `enabled=false` entries), pi registration and `--skill`
   order, and environment lists in name order. Paths are stable (the agent
   directory and the content store), never temporary. Filesystem enumeration
   order and server connection order never choose model-visible order.
3. **No secrets in rendered bytes**, so rotating a credential does not
   invalidate a cached prefix (see Secrets).
4. **crew's own tools first and constant.** This is confirmed by the current
   state: `mcp/tools.rs::definitions` is a fixed vector, filtered by role
   without reordering, with no identities or live state in `tools/list`, and
   `listChanged: false`. Toolbox servers come after `crew` and change nothing
   in it.
5. **Stable MCP tool definitions.** Third-party servers can reorder tools,
   change descriptions or connect late. When tools are declared on every
   request, that changes the definitions mid-session and breaks the cached
   prefix. crew's MCP launcher (`crew toolbox mcp <name>`) therefore proxies
   every toolbox server:
   - It returns `tools/list` sorted by tool name.
   - It never advertises `listChanged` to the runtime.
   - It answers `initialize` and `tools/list` from a snapshot pinned for the
     session and stored in `A`. A slow or reordered upstream therefore cannot
     change what the runtime sees.

   The definition digest is the SHA-256 of the pinned snapshots, in server
   order. A snapshot is refreshed only when the item's definition changes or
   on `crew resume --restart`. A refresh that changes the digest records
   `toolbox.changed` even when the fingerprint is unchanged. Tools that
   appear upstream later are logged and are not exposed until a refresh. A
   call to a tool that disappeared returns the upstream error. Deferred
   loading (Claude Code's MCP tool search, pi's deferred exposure) keeps
   toolbox definitions out of the declared prefix; loaded schemas land in
   messages instead. Whether toolbox servers should use deferred exposure is
   decided by `cache-design` after the baseline and audit. The proxy remains
   for secrets and stable ordering either way. The requirement stays: no
   definition changes within a session.
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
- **`agent_spawn`** gains the same optional `toolbox`. By default a subagent
  gets its parent's member-level selection; today `daemon/subagents.rs`
  starts from team defaults only. A requested bundle whose items are not all
  in the parent's effective toolbox is refused: a worker cannot grant more
  than it has.
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
| T03 | Reorder keys, items and bundles without changing meaning | Same fingerprint, byte-identical generated files. |
| T04 | Validation | Each of these stops `crew run` and `crew toolbox check` with its field path: bundle cycle, unknown bundle, an exclusion matching nothing, an item defined and excluded at one level, a reserved name, a plain string where a reference belongs, `inherit_mcp` together with `inherit`. |
| T05 | Secrets | The sentinel value of `SHOP_GITHUB_TOKEN` reaches only the stub server's process. It is absent from `A`, the store, the roster, the event log, `crew toolbox` output, every dashboard payload, every argv (`/proc/*/cmdline`) and the member session's own environment. The same holds for `DOCS_TOKEN` through the remote bridge. |
| T06 | Missing variable | For an item that is not required, the member starts and the item shows Needs $SHOP_GITHUB_TOKEN. For a required item, or under `strict`, the member does not start, and the inspector and the orchestrator's result give the same text. |
| T07 | Stability over the lifecycle | Spawn, crash restart and `crew resume` give the same fingerprint and definition digest, and byte-identical `A/mcp.json`, plugin directory hashes, Codex `-c` arguments, pi arguments and pinned snapshots. |
| T08 | Reload while running | After the team file is edited, `crew toolbox reload` leaves running sessions untouched. Affected members show the pending notice and `toolbox.pending` appears. A restart applies the change, and `toolbox.changed` carries both fingerprints. An edit to a field other than a toolbox field is reported and not applied. |
| T09 | Snapshots | A `SKILL.md` edited mid-session changes nothing in the session. After a restart, the new content hash is in the fingerprint. An item over 16 MiB is refused with its size. |
| T10 | Runtime switch | `agent_set_model` from a Claude Code model to a Codex model lists the items that are not Active on Codex. A `required` one refuses the switch with its reason. A model change within the runtime keeps the fingerprint. |
| T11 | `agent_add` and `agent_spawn` | A bundle selection is applied and saved to the roster. An unknown bundle is refused with the copy above. A subagent gets its parent's selection by default, and one asking for a bundle outside its parent's toolbox is refused. The input schemas reject item definitions. |
| T12 | Operator sources, two members | Two members with different selections, with the sentinel operator home. Under `none`, unsandboxed: per runtime, the only sentinels reaching a member are the ones its sources list as cannot be excluded. Codex sentinel servers appear as Disabled, and the native listing shows them disabled. Sandboxed: no operator sentinel reaches either member. With `inherit.operator: [mcp]`, only the operator's MCP sentinel reaches the member, listed as Used. Operator files are byte-unchanged afterwards. |
| T13 | Strict | With `strict: true` and an unsandboxed source that cannot be excluded, the member does not start, and the reason is shown. |
| T14 | Proxy stability (rule 5) | The stub upstream returns tools in a random order and changes one description after the first listing. The runtime sees sorted tools and the pinned descriptions for the whole session, and `listChanged` is never advertised. After `crew resume --restart` the new description appears, the definition digest changes and `toolbox.changed` is recorded with the fingerprint unchanged. |
| T15 | Dashboards | For the T02 fixture, the web inspector, TUI detail, GTK inspector (AT-SPI names) and Apple detail (source or unit level, while Apple execution stays deferred) show the same groups, origins, statuses and sources. Status is text, group headings are headings, and long commands do not widen the pane. |
| T16 | Cache attribution | Precondition: the pinned tool-definition snapshots and generated artifacts are byte-identical before and after a restart (T07). Then measure, in `cache-bench`, paired warm sessions for the same member, model and endpoint, using runtime-native usage over repeated equivalent turns within the cache lifetime. Report cache read and write per turn, with the cache TTL, model and routing (endpoint, region) stated. Aggregate token counts alone do not pass this case. A deliberate toolbox change shows one `toolbox.changed` and the expected reset. |
| T17 | Shell members | A toolbox on a shell member shows every item as Not applicable and does not stop the launch, even under `strict`. |
| T18 | Resume after removal | On each runtime, remove an MCP server that was used earlier in a session, reload and restart. The resumed session continues, and the history's earlier calls to the removed tool cause no error. If one runtime fails, that runtime's change path uses a fresh session that is handed the work, and the case asserts that path instead. |
| T19 | Sandbox | A server whose program is outside the bound trees shows Blocked by the sandbox. Store items are readable and read-only inside the sandbox. Files left in the private home's toolbox directories by an earlier session are gone after a restart. |

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
   - `crew toolbox mcp <name>`: secret fetch over the identity; exec for
     local servers; stdio-to-HTTP bridge for remote ones; sorting, pinned
     snapshots and the definition digest.
   - Tests for T11, T14 and T18.
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
- Local servers get their environment through the exec path of
  `crew toolbox mcp`, without sorting or pinning.
- The definition digest is reported as unavailable.

### Open items

**Probes to run** (graph's `capability-probes` card). Each probe either
confirms its cell or turns it into a Cannot be excluded or Not available
entry. No cell may be implemented as Active or Not used on an unconfirmed
mechanism.

- Claude Code:
  - MCP servers bundled in a toolbox plugin, under strict MCP loading;
  - `project` alone (without `local`);
  - project exclusion.
- Codex:
  - whether `codex mcp list --json` reports plugin-bundled servers;
  - whether the TUI session sees app-server skill roots.
- pi:
  - instruction files;
  - packages;
  - every pi row, on the supported pi.

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
