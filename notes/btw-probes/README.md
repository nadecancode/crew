# Aside (`/btw`) probes

Offline probes for [btw.md](../btw.md). Run from the repository root with Node
22.19 or newer. Audited versions: Claude Code 2.1.291, Codex 0.159.0, pi 1.1.0.
`BTW_CLAUDE`, `BTW_CODEX` and `BTW_PI` may name specific binaries; otherwise
Claude Code and Codex come from PATH, and pi from the install below.

```sh
node notes/toolbox-probes/install-pi.mjs   # pi 1.1.0 under target/, once
node notes/btw-probes/claude.mjs
node notes/btw-probes/claude-member.mjs
node notes/btw-probes/claude-deny.mjs
node notes/btw-probes/claude-hooks.mjs
node notes/btw-probes/codex.mjs
node notes/btw-probes/codex-deny.mjs
node notes/btw-probes/pi.mjs
```

Every request goes to [stub.mjs](stub.mjs), a localhost endpoint that records
each request body and answers in the provider's streaming format: Anthropic
Messages for Claude Code and pi, OpenAI Responses for Codex. A probe can hold a
request (a turn in flight) or script tool calls. Nothing is forwarded, so no
provider is called and no cost is incurred. The CLIs complete real turns and
write real session files in throwaway homes under `target/btw-audit/`, with
placeholder keys and an allowlisted environment. Each probe prints its report
and keeps `report.json` and `requests.json` there.

| Probe | Question |
| --- | --- |
| `claude.mjs` | Does `--resume --fork-session --no-session-persistence` leave the member's files alone? Which flags keep the member's tools, system prompt and messages? Does a fork of a copied transcript in another config directory send the same request? What happens mid-turn? |
| `claude-member.mjs` | With a crew-like member (an MCP server named `crew`, hooks in flag settings), which aside settings keep the request identical, with tool search on and off? Do hooks fire, and is the member's MCP server started? |
| `claude-deny.mjs` | Under `dontAsk`, are Write, Bash and an MCP call refused while Read works? What does the default mode do with `--permission-prompts none`? |
| `claude-hooks.mjs` | Does `disableAllHooks` stop project hooks without changing the request? |
| `codex.mjs` | Does `exec fork --ephemeral` leave the rollout and thread records alone? Is the prefix and cache key kept, and what do `--sandbox read-only` and `web_search="live"` change? Does it work from a copied rollout and mid-turn? |
| `codex-deny.mjs` | In an observer home with the read-only sandbox and approvals off, are a write and an MCP call refused while a read works? |
| `pi.mjs` | Does `--fork <file> --session-dir <aside dir>` leave the member's session and agent directory alone? What do `--tools` and `--no-tools` change? Does a `tool_call` guard extension refuse acting tools while keeping the request? |

These probes do not measure provider caching, real logins, crew's OS sandbox
or crew's own launch. Those are acceptance cases B02, B10 and B11 in
[btw.md](../btw.md).
