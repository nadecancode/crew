# Repeatable cache workload

The runner is `scripts/cache/bench`. Its fixed three-member team reads six
invoice rows, exchanges a question and answer, commits a summary, reviews it,
hands it to the other runtime for an independent audit, waits, and reviews a
follow-up after the idle gap. The fixture has an exact output oracle. Its money
amounts are synthetic integer-cent test data; no financial service is involved.

The first live baseline is **not available yet**. On 2026-10-07, the baseline
binary started the isolated team, but Claude's private home could not
authenticate. No billable usage record was produced. Missing measurements are
unknown, not a 0% hit rate or a measured zero-dollar total. See
[`bench-baseline-attempt.json`](bench-baseline-attempt.json).

## Running it

Run from the operator's normal shell with the authentication already configured
for sandboxed Crew members. This script never logs in, refreshes tokens, reads
credential files, or copies them. It keeps the real `HOME` for Crew's normal
configuration discovery. Crew itself creates each member's private home. A
run started inside another sandboxed member may have no usable credentials;
the authentication failure is reported and the private team is stopped.

Requirements: Linux `/proc`, Python 3.11+, Git, both provider CLIs, bubblewrap,
and socat. The wrapper uses Nix for Python when necessary. On this Nix host:

```sh
nix shell nixpkgs#python3 nixpkgs#bubblewrap nixpkgs#socat --command \
  scripts/cache/bench \
  --crew /home/nade/workspaces/personal/crew/target/release/crew \
  --expected-sha256 ea4e6f127328cd9d679c62c9a1cc7bd6f72c6cdecd2703892962c5a7ea95ed19 \
  --source-revision c8052373c0d2f8b7a993773e5cda3a79e1b834d6 \
  --label baseline-c8052373 \
  --out target/cache-bench/operator-baseline-c8052373
```

The binary is the existing release build from `release-rebuild-2`; this command
does not rebuild it. The runner verifies the supplied SHA-256 before launch.
`source_revision` is recorded provenance supplied by the caller; the runner
does not infer a source revision from the executable. For a candidate run,
change the executable, its verified hash, revision, label and output directory.
Use the same template, models, CLI versions, idle gap, timeout and authentication
route for before/after comparisons. Every output directory must be new.
The runner refuses to launch when either CLI fails to report a nonempty version.
Every observed request must name a priced model; a missing model name also stops
the run. `report.json` records both CLI versions, each request's model and each
member's distinct observed model names.

Add `--prepare-only` to create the fixture and team YAML without a daemon or
provider call. Defaults are a 20-second idle gap and a 600-second total deadline.
`--idle-seconds` can select longer gaps up to 3,600 seconds; choose a compatible
`--timeout-seconds` (maximum 7,200). A gap is measured only while all three
members are idle/done/waiting and their native usage counters remain unchanged.
The standard 20-second gap tests reuse between turns; it is not a TTL-expiry
experiment. A longer gap alone does not establish which provider TTL applied.

## Workload and isolation

`scripts/cache/team.yaml` fixes a Claude orchestrator and Claude worker on
Haiku 4.5, plus a Codex worker on GPT-6 Luna at low reasoning effort. Crew's
catalog canonicalizes the Claude model name to `haiku`; the actual model
reported by the transcript must still match the Haiku 4.5 rate family. The
watchdog stops on an unknown model. Automatic fallback/restart and subagents
are disabled. The initial two tasks and the later `followup` task are fixed.

The fixture is a fresh Git repository beneath the selected output directory.
Crew uses worktrees, the PTY backend and required OS sandboxes. Claude uses
`acceptEdits` plus explicit local-tool permissions to avoid the first-use
Bypass Permissions dialog. Codex's permission policy is bypass within the
outer Crew sandbox. These settings are part of the versioned workload.

Inherited `CREW_*` and `GIT_*` variables and parent CLI session identifiers are
removed. Git system/global configuration is disabled for the fixture. A private,
short `/tmp/cb.*` directory holds daemon sockets and discovery state. Every
Crew operation addresses the fixture with `-C`. Cleanup validates the daemon's
workspace, socket directory and process start time, then stops only that team.
Any remaining descendants are identified by PID and start time before signals
are sent. No global daemon, listener, operator session or native gateway command
is selected. SIGINT, SIGTERM, timeouts and budget stops run the same cleanup.

## Usage and cost accounting

`scripts/cache/usage.py` is the old-binary-compatible spend watchdog. It reads
only this run's native session files beneath the private member homes:

- Claude assistant messages are deduplicated by message ID, retaining the last
  usage. Total input is uncached input plus cache reads plus cache writes.
- Codex cumulative `token_count` records are differenced and identical updates
  are ignored. Cached input is already a subset of input. Reasoning output is
  not added to output again. A decreasing or incomplete counter stops the run
  rather than inventing a delta.
- Per-agent hit rate is total cached input divided by total input, weighted by
  tokens across requests. Missing sessions or counters remain unknown.
- Native per-request counters, timestamps and observed model names are exported;
  prompt and response bodies are not included in the metrics report.

When `scripts/cache/baseline.mjs` is available, the runner also invokes the
`cache-baseline` collector after shutdown, with the private workspace and
`baseline.json` output. That collector owns event correlation and broader
session attribution. Its absence is explicit in `report.json`; the watchdog's
direct native measurements still work on the original binary.

Rates checked on 2026-10-07, in USD per million tokens:

| Model | Uncached input | Cache read | Cache write | Output |
| --- | ---: | ---: | ---: | ---: |
| Haiku 4.5 | 1.00 | 0.10 | 1.25 for 5 min; 2.00 for 1 h | 5.00 |
| GPT-6 Luna | 0.10 | 0.01 | 0.125 | 0.50 |

Sources: [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
and [official OpenAI documentation for GPT-6 Luna](https://developers.openai.com/api/docs/models/gpt-6-luna).
These are API-equivalent estimates, not subscription invoices. Recheck the
versioned rates before a future benchmark campaign. Claude's reported 5-minute
and 1-hour write buckets determine its write cost when present; otherwise the
two prices bound the estimate. Codex does not expose the write/miss split here,
so its estimate spans input to cache-write pricing and allows Fast processing
in the upper estimate. The long-context multipliers apply above 272,000 input
tokens. Account-specific discounts or routing are not measured.

The runner polls native counters each second and stops at an observed upper
estimate of **$2**, leaving **$1** beneath the requested approximately $3 cap
for in-flight requests and shutdown. It also stops on unknown pricing, agent
failure, a 90-second unexplained lack of new usage, or the deadline. This is a
local watchdog, not a provider-enforced hard spending limit: native usage can
arrive after a request finishes. `observed_cost_*` covers records actually seen;
`cost_usd_*` stays null if any member lacks usage coverage. An estimate above
the run budget is reported as `budget-exceeded`.

## Outputs and acceptance

The output directory is private. It contains the reproducible fixture and
YAML, manifest and binary/template hashes, snapshots, event JSONL, native
session files, Crew's database, cleanup results, and `report.json`. It may also
contain `baseline.json`. Native sessions and the database remain private;
share the numeric report rather than copying the whole directory into source
control. Cleanup removes the runtime socket directory, retaining the evidence.

A successful report requires all three cards to finish, an observed review
result and question/answer exchange, the expected fixture outputs, a measured idle gap,
three members with real usage records, and successful owned-process cleanup.
Both per-agent rates and request counts matter: a smaller workload is not an
equivalent improvement. One run is a baseline sample, not a statistically
established change in cache performance. Before claiming a hit-rate delta,
collect at least three successful runs per side with the same 20-second idle
gap, template, versions, model identities and authentication route. Record run
order and timing because provider cache state can carry across runs. Compare
all samples, with their request counts and token-weighted rates; do not select
only the best run. This card's first baseline does not satisfy that comparison
plan, and no before/after claim is made yet.

## Checks and current live boundary

```sh
nix shell nixpkgs#python3 --command \
  python3 -B -m unittest discover -s scripts/cache -p 'test_*.py' -v
```

The tests cover Claude streaming deduplication and write TTL accounting, Codex
cumulative updates and reasoning output, missing metrics, parent-environment
isolation, PID reuse, rejection of incomplete completion evidence, and the
full runner's idle/review/cost-stop paths using a fake CLI and a real disposable
process. These tests never call a model provider.

Live startup on the c8052373 binary, Claude Code 2.1.291 and Codex CLI 0.159.0
reached the required sandboxes. Attempt 01 caught the YAML spelling `require`
(corrected to `required`). Attempt 02 exposed Claude's first-use bypass dialog
(the workload now uses guarded permissions). Attempt 03 then reported an
authentication failure; attempt 04 repeated it while retaining the real HOME
and all normal configuration locations, with only the runtime directory and
repository isolated. Both authentication attempts stopped their daemon with
exit code 0 and left no tracked process behind.

The final native error at `2026-10-08T04:00:47.095Z` was
`authentication_failed`, with text `Not logged in · Please run /login`.
Crew activity was `sign-in needed · waiting for \`crew resume\``. The
orchestrator was notified and is arranging an operator-run baseline. No
credential workaround or further live launch is attempted by this agent.
