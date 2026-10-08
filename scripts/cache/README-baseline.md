# Native cache measurements

`baseline.mjs` needs Node 24 (built-in read-only SQLite). It reads native JSONL
usage and Crew metadata, never terminal output. It does not start runtimes,
make API calls, change settings, or write native session files.

```sh
node --test scripts/cache/baseline.test.mjs
node scripts/cache/baseline.mjs --workspace /path/to/project \
  --output /path/to/reports/baseline.json --markdown /path/to/reports/baseline.md
```

The daemon's usage-metrics parity tests use allowlisted extracts of the
lifecycle sessions named in `notes/prompt-cache/lifecycle-before-0*.json`
(ids, models, counters, timestamps and turn markers only). `usage-fixtures.mjs`
regenerates them under `crates/crew/tests/fixtures/usage/lifecycle`, bounded
to each report's finish time; `--check` compares without writing, and
`usage-fixtures.test.mjs` checks that this collector reads the same totals.

Default discovery covers the specified workspace's operator and private native
homes. `--all-runs` also reads workspaces in the local Crew session registry.
Historical files in those native directories are included; arbitrary temporary
projects and unregistered archived workspaces are not searched. Pass an archived
workspace explicitly. A nested registered run is disambiguated by session id,
not just its shared working directory.

For a benchmark or another narrowly scoped measurement, use exact files. This
disables automatic native-file discovery, including the global Codex metadata
scan. Repeat the option for each session:

```sh
node scripts/cache/baseline.mjs --workspace /path/to/fixture \
  --session-file claude=/path/to/session.jsonl \
  --session-file codex=/path/to/rollout.jsonl \
  --output /path/to/reports/baseline.json
```

`--since ISO` and `--until ISO` select requests by their native usage timestamps.
The default upper bound is the start of collection. Earlier requests still
supply gap and event context. Each source is read only to its initial byte size;
its SHA-256, byte count and change-during-read flag are retained. A final streamed
update supplies an assistant response's usage; native timestamps do not identify
the provider's exact request start or cache expiration. A live database is read
in a read-only transaction and recorded with its maximum event sequence. This
is a collection window, not a global atomic snapshot of all runtime files.

## Schema version 1

`sessions[].requests` is the timeline. Each observation includes native source
line/id, model, timestamp, token counts, hit rate, preceding gap, optional native
diagnostic and Crew event references. Claude streamed responses are deduplicated
by message id, with the last top-level usage winning. Sidechains and synthetic
responses are excluded. Codex repeated cumulative counters are ignored; deltas
are checked against last-request usage. Counter decreases are flagged, with
only direct last-request usage retained. A batch delta is marked rather than
claimed to be one request. Output tokens already include reasoning tokens.

The hit rate is the sum of cache reads divided by the sum of total input on
requests with valid, known counters. Claude total input is ordinary input plus
cache writes plus cache reads. Codex input already includes its cached input.
The collector preserves a native Codex write counter where available; missing
write fields remain `null`. These denominators follow the
[Anthropic usage definition](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
and [OpenAI caching usage fields](https://developers.openai.com/api/docs/guides/prompt-caching).

`totals`, `by_runtime`, `by_agent`, `by_role` and each session include metric
coverage counts. Partial known sums are never described as complete coverage.
No observed usage produces a `null` hit rate, not zero percent. Invalid counters
are excluded and counted. Roles come from the current roster; unknown historical
identities stay unknown instead of being assigned to an arbitrary member sharing
the workspace. Ownership uncertainty also prevents joining that member's events.

`miss_associations` partitions requests by the documented priority, while
`event_associations` allows overlap. Thinking-drop history classification
requires both `messagesHistoryChanged` and `prefix_mismatch`, joined by request
id. Fold cohorts connect a completed Claude turn to the next turn's first
request. File order wins when attachment timestamps regress. An absent
diagnostic is not proof that history stayed unchanged. Five-minute, thirty-minute
and one-hour gaps are thresholds, not observations of configured TTL. Explicit
Claude write buckets are recorded separately.

The exported JSON contains no prompt, tool-result, message-body or credential
fields. It contains local paths, public session/request ids and roster roles;
review these before sharing outside the team. Prices, subscription invoices and
avoidable loss are not inferred; `avoidable_miss_tokens` remains `null`.
Outputs cannot replace native inputs, database/WAL/SHM files, registry records or aliases of them,
nor be placed in native session or agent-home directories. Reports inside Crew
worktrees are allowed. Keep outputs outside native sources.
