# Repeatable cache workload

The runner is `scripts/cache/bench`. Its fixed three-member team reads six
invoice rows, exchanges a question and answer, commits a summary, reviews it,
hands it to the other runtime for an independent audit, waits, and reviews a
follow-up after the idle gap. The fixture has an exact output oracle. Its money
amounts are synthetic integer-cent test data; no financial service is involved.

The first live baseline completed on 2026-10-07 using the c8052373 release
binary and runner a1cd0cf5. The operator authorized `--sandbox off` with normal
CLI logins (`m-ff8aa3691883`, `m-de66f70cfa51`). The daemon, repository and session
identities remained separate; native session logs live in the normal CLI homes.
The full numeric report is [`bench-baseline.json`](bench-baseline.json).

| Member | Observed model | Requests | Input tokens | Cache-read tokens | Token hit rate | API-equivalent USD |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| orchestrator | claude-haiku-4-5-20251001 | 10 | 423,132 | 379,177 | 89.6120% | 0.135763 |
| claude-worker | claude-haiku-4-5-20251001 | 29 | 1,295,160 | 1,230,666 | 95.0204% | 0.279513 |
| codex-worker | gpt-6-luna | 27 | 678,036 | 645,376 | 95.1831% | 0.010290–0.022213 |

Across 66 requests, 2,255,219 of 2,396,328 input tokens were cache reads
(94.1114%). The cost estimate was $0.425565–$0.437488. Claude Code was 2.1.291;
Codex CLI was 0.159.0. The run took 149.46 seconds and measured a 20.4535-second
idle gap. All three cards finished, one question/answer exchange and two
reviews were observed, and all fixture output checks passed. The private
daemon stopped with exit code 0 and no tracked process remained.

Earlier required-sandbox attempts could not authenticate; their missing usage
remains unknown, not a measured zero hit rate or cost. Their separate record is
[`bench-baseline-attempt.json`](bench-baseline-attempt.json). This single
normal-home sample is not a before/after result or a sandboxed-production result.

## Running it

Run with the operator's authorized authentication route. This script never logs in, refreshes tokens, reads
credential files, or copies them. It keeps the real `HOME` for Crew's normal
configuration discovery. By default, Crew creates each member's private home. A
run started inside another sandboxed member may have no usable credentials;
the authentication failure is reported and the private team is stopped.
`--sandbox off` explicitly permits fresh sessions using the normal CLI homes
and logins, as authorized for this baseline. It never resumes an existing
session. The watchdog reads only the session IDs created by this fixture.

Requirements: Linux `/proc`, Python 3.11+, Git and both provider CLIs. Required
OS sandboxes also need bubblewrap and socat. The wrapper uses Nix for Python
when necessary. The authorized normal-login run on this Nix host:

```sh
nix shell nixpkgs#python3 --command \
  scripts/cache/bench \
  --crew /home/nade/workspaces/personal/crew/target/release/crew \
  --expected-sha256 ea4e6f127328cd9d679c62c9a1cc7bd6f72c6cdecd2703892962c5a7ea95ed19 \
  --source-revision c8052373c0d2f8b7a993773e5cda3a79e1b834d6 \
  --label baseline-c8052373 \
  --sandbox off \
  --out target/cache-bench/operator-baseline-c8052373
```

The binary is the existing release build from `release-rebuild-2`; this command
does not rebuild it. The runner verifies the supplied SHA-256 before launch.
`source_revision` is recorded provenance supplied by the caller; the runner
does not infer a source revision from the executable. For a candidate run,
change the executable, its verified hash, revision, label and output directory.
Use the same template, models, CLI versions, idle gap, timeout and authentication
route and sandbox mode for before/after comparisons. Every output directory must be new.
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
Crew uses worktrees and the PTY backend. OS sandboxes are required by default;
the operator-authorized baseline selects off explicitly. Claude uses
`acceptEdits` plus explicit local-tool permissions to avoid the first-use
Bypass Permissions dialog. Codex's permission policy is bypass. These settings
and the selected sandbox mode are recorded as part of the workload.
Unsandboxed members may load the operator's user agents, skills and plugins.
Sandboxed production teams use filtered private homes, so this normal-login
baseline does not establish their cache behavior. Keep that environment
difference explicit when interpreting or comparing results. Each member's
exact new session ID and native file path are recorded in the report; the
harness never edits or deletes those files.

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
only this run's native session files. They are beneath the private member homes
by default; with `--sandbox off`, the exact newly created session IDs are found
under `CLAUDE_CONFIG_DIR` / `CODEX_HOME` or the usual `~/.claude` / `~/.codex`.
It does not read other sessions' contents or write any native session file.

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
two prices bound the estimate. The watchdog conservatively leaves Codex's
write/miss split out of its cost calculation, so its estimate spans input to
cache-write pricing and allows Fast processing in the upper estimate. This
run's native Codex records do contain `cache_write_input_tokens`, consistently
zero; the conservative interval is retained to describe the runner as executed.
The long-context multipliers apply above 272,000 input
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
YAML, manifest and binary/template/runner hashes, snapshots, event JSONL,
Crew's database, cleanup results, and `report.json`. It may also
contain `baseline.json`. Native sessions are retained where the runtime wrote
them (private homes or normal CLI homes). Native sessions and the database remain private;
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

The 11 passing tests cover Claude streaming deduplication and write TTL accounting, Codex
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
orchestrator was notified. The operator then authorized the successful
unsandboxed run above; no credential extraction, copy or login command was used.
The native session files remain untouched in their original CLI homes. The
runtime's broader `baseline.mjs` collector was not yet present, which the
report records explicitly; all reported measurements came directly from the
fresh sessions through the watchdog's native parsers.

## Extended lifecycle workload

`--scenario lifecycle` selects `scripts/cache/team-lifecycle.yaml`; the short
control above stays unchanged. The fixed sequence warms both workers, resumes
each with an unchanged roster, adds a token-free scripted observer and resumes
both workers, removes the observer and resumes them again. Every resume must
preserve the session ID. The Crew briefing's byte length, SHA-256 and longest
common byte prefix are recorded before and after each restart; this measures
Crew's own file, not the provider's full effective system prompt.

The Claude worker then runs two 30-second shell windows. The control message is
sent after its turn ends. The fold message is sent only when the exact session
JSONL contains the marked pending `tool_use` without its matching `tool_result`.
The injected marker differs from the card's command marker, so the card itself
cannot pass the fold evidence check. The report records native `isMeta` rows,
post-fold thinking-block counts (never thinking text), and `thinking_drop`
metadata joined to the following probe's request IDs. Bounded thinking is
requested with `MAX_THINKING_TOKENS=2048`; actual presence is measured separately.

Next, Claude reads six deterministic synthetic context files. Forced compaction
requires at least 65,000 input tokens in the preceding native request and a
subsequent increment in Crew's compaction counter. A post-compaction message
then exercises the retained session. Finally the team remains quiet for more
than 310 seconds before both workers receive fixed checksum probes. Their
actual last-request-to-next-request intervals must each exceed 300 seconds.
`--long-idle` adds 3,665 quiet seconds and strict native intervals over 3,600
seconds. A five-minute gap is not claimed to expire a one-hour Claude cache or
Codex's longer cache; these are measured gaps, with TTL interpretation kept
separate from the observations.

Example, using the same verified binary and arguments shown above:

```sh
scripts/cache/bench ... --scenario lifecycle --sandbox off \
  --label lifecycle-before-1 --out target/cache-bench/lifecycle-before-1 \
  --timeout-seconds 7200 --long-idle
```

Every step records its action boundary, request offsets, first subsequent
request, input/cache-read/uncached tokens and estimated cost. This is temporal
attribution to a controlled step, not proof of a provider-internal cause.
Native declarations such as `prefix_mismatch` are preserved as declarations.
Runs still stop at the $2 observed upper estimate under the approximately $3
budget, and the long waits do not bypass failure or cost checks. The scripted
observer never calls a provider and is excluded from token totals. All newly
observed model session IDs remain in the spend ledger, even if an unexpected
restart changes identity and makes the scenario fail.

The extended before-run series is in progress. It requires three successful
runs on c8052373; one includes the one-hour interval if the measured cost
allows it. The corresponding after series must use the same mode, versions,
models and per-run scenario. There is no lifecycle before/after result yet.
