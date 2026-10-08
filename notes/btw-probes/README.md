# Aside (`/btw`) probes

Probes for [btw.md](../btw.md). Run from the repository root with Node 22.19
or newer. The audited versions are Claude Code 2.1.291, Codex 0.159.0 and
pi 1.1.0.

- `BTW_CLAUDE`, `BTW_CODEX` and `BTW_PI` may name specific binaries.
- Otherwise Claude Code and Codex come from PATH, and pi from the install
  below.
- `codex-isolation.mjs` needs `sqlite3` on PATH.

```sh
node notes/toolbox-probes/install-pi.mjs   # pi 1.1.0 under target/, once
node notes/btw-probes/claude.mjs
node notes/btw-probes/claude-member.mjs
node notes/btw-probes/claude-deny.mjs
node notes/btw-probes/claude-allow.mjs
node notes/btw-probes/claude-hooks.mjs
node notes/btw-probes/claude-copy.mjs
node notes/btw-probes/codex.mjs
node notes/btw-probes/codex-deny.mjs
node notes/btw-probes/codex-copy.mjs
node notes/btw-probes/codex-isolation.mjs
node notes/btw-probes/pi.mjs
node notes/btw-probes/pi-isolation.mjs
# Live, with the operator's own logins and a cheap model (a few cents):
node notes/btw-probes/claude-live.mjs
node notes/btw-probes/codex-live.mjs
```

## The offline probes

Every request goes to [stub.mjs](stub.mjs), a localhost endpoint:

- It records each request body.
- It answers in the provider's streaming format: Anthropic Messages for
  Claude Code and pi, OpenAI Responses for Codex.
- A probe can hold a request (a turn in flight) or script tool calls.
- Nothing is forwarded, so no provider is called.

The CLIs complete real turns and write real session files in throwaway homes
under `target/btw-audit/`. They use placeholder keys and an allowlisted
environment. Each probe prints its report and keeps `report.json` (and most
keep `requests.json`) there.

## The live probes

The two live probes (`claude-live.mjs`, `codex-live.mjs`) call the provider
through the operator's own CLI logins, which they use in place and never
copy:

- Claude Haiku 4.5 for Claude Code.
- GPT-6-Luna at low effort for Codex.

They keep the operator's hooks, plugins, configuration and MCP servers out:

- Claude Code: `--setting-sources project` with a throwaway project, and
  `disableAllHooks`.
- Codex: `--ignore-user-config` and `features.hooks=false`.

They remove what they leave behind: the Claude transcript directory, and the
Codex thread (with `codex delete --force`).

## What each probe answers

| Probe | Question |
| --- | --- |
| `claude.mjs` | Does a fork without persistence leave the member's files alone? Which flags keep the request? Does a copied transcript work? What happens mid-turn? |
| `claude-member.mjs` | With a crew-like member (an MCP server named `crew`, hooks), which aside settings keep the request identical, with tool search on and off? Do hooks fire? |
| `claude-deny.mjs` | With no grants, does `dontAsk` refuse Write, Bash and MCP? What does the default mode do? |
| `claude-allow.mjs` | With project and operator grants, which settings still refuse every call and keep the tool definitions? |
| `claude-hooks.mjs` | Does `disableAllHooks` stop project hooks without changing the request? |
| `claude-copy.mjs` | Can the fork read a frozen copy by path while the config directory, and so the login, stays the member's? |
| `codex.mjs` | Does `exec fork --ephemeral` leave the rollout and thread records alone? Are the prefix and cache key kept? What do the read-only sandbox and `web_search="live"` change? Does a copy work, and does a fork mid-turn? |
| `codex-deny.mjs` | Are a write and an MCP call refused while a read works? Does an exec-policy rule that allows `touch` get past the sandbox, and does `--ignore-rules` stop it? |
| `codex-copy.mjs` | Can a fork read a rollout copy by path in the member's home? |
| `codex-isolation.mjs` | In a home with a user hook and an ambient MCP server, does an aside keep both from running? Are the SQLite rows unchanged? Does a half-written last line break an in-place fork? |
| `pi.mjs` | Does `--fork <file> --session-dir <dir>` leave the member alone? What do `--tools` and `--no-tools` change? Does a `tool_call` guard refuse acting tools and keep the request? |
| `pi-isolation.mjs` | With an ambient extension and MCP server in the agent directory, which launch keeps both from starting? |
| `claude-live.mjs`, `codex-live.mjs` | Live: does the aside read the member's cache, and does the member's next turn still read its own? |

These probes do not measure the following, which acceptance cases B02, B10
and B11 in [btw.md](../btw.md) cover:

- crew's own launch;
- crew's OS sandbox;
- cold caches;
- pi live.
