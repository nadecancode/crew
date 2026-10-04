# Agent runtime production-readiness review

Reviewer: rev-runtime. Date: 2026-10-03. Verdict for this area: **changes required before production use**, particularly around shared-checkout commits, herdr delivery, and process cleanup. This is a review only; no product code was changed.

## Scope and evidence

Reviewed `agents/`, `process/`, `git/`, `cli/`, `endpoints.rs`, `limits.rs`, `models.rs`, `transcript.rs`, `styles.rs`, `main.rs`, `lib.rs`, `scripts/build-env`, the example teams, and relevant unit/integration tests. Daemon and core-library call sites were read where necessary to establish reachability; those owners were notified.

The checkout has no commits and the operator was editing source during the review. A source-only snapshot was taken at **2026-10-03 00:16:30 -0500** into `/tmp/crew-runtime-source-20261003`. Findings below were re-located in the main checkout at **00:23:53 -0500**. SHA-256 checks of the seven primary finding files (`agents/codex.rs`, `process/herdr.rs`, `process/pty.rs`, `git/mod.rs`, `cli/ops.rs`, `limits.rs`, `endpoints.rs`) still matched the snapshot. Code quotations accompany locations so later line shifts can be resolved. None of these findings had been fixed at that check.

The baseline reviewer reports successful debug/release builds, zero clippy warnings, and three passing runs of 272 workspace tests with isolated environment paths; `cargo fmt --all -- --check` fails. Advisory tooling was unavailable. Those are the baseline reviewer's results, not an independent full baseline rerun; see [baseline.md](baseline.md). I ran focused reproductions myself without invoking a real agent CLI, live daemon, or herdr session.

The external harness is `/tmp/crew-runtime-review-20261003/src/lib.rs`; its Cargo manifest now depends on the timestamped source snapshot. Eight reproductions passed together, two additional edge reproductions passed separately, and a quota/RPC follow-up reproduction also passed: **eleven focused reproductions** in total. **A passing reproduction asserts the defective behavior exists; it is not a passing regression test for correct behavior.** All fixtures use temporary directories and synthetic credentials, and the PTY descendant was explicitly killed after observation. Build artifacts live outside the checkout.

```sh
CARGO_TARGET_DIR=/tmp/crew-runtime-review-target \
  /tmp/crew-runtime-source-20261003/scripts/build-env cargo test --offline \
  --manifest-path /tmp/crew-runtime-review-20261003/Cargo.toml \
  -- --nocapture --test-threads=1
```

## Findings, highest value first

### R1 — P1: shared-checkout commits include private crew state and other agents' work

- **Locations:** `crates/crew/src/git/mod.rs:267`; `crates/crew/src/daemon/team.rs:107`; `crates/crew/src/daemon/gitops.rs:61`; `crates/crew/src/agents/prompt.rs:157` and `:496`.
- **Code:** `out(dir, &["add", "-A", "--", "."])?;`; the state exclusion is conditional: `if result.0 { git::exclude_state_dir(&workspace)?; }`, where `result.0` means worktree isolation. The shared-workspace briefing still says, `When your task is done: git_commit your work`.
- **Failure:** an existing repository with `isolation: shared`, or an unborn repository with `isolation: auto`, receives no `.crew/` exclusion. Its agents can call `git_commit` because the daemon only requires a repository root. `add -A .` stages the daemon database, generated identity/config files, and every teammate's uncommitted files. Private directory/file permissions do not protect content after it enters Git history. Even when `.crew/` is already ignored, a commit made for one task captures other tasks' unfinished changes.
- **Evidence:** `shared_commit_includes_private_crew_state` initialized an unborn temporary repository, created a source file plus synthetic `.crew/agents/a/identity.json` and `.crew/config.toml`, then called the public `commit_all`. `git ls-tree` listed all three in the new commit. This checkout already ignores `.crew/`, so the synthetic reproduction establishes the credential leak; the live review setup independently demonstrates the shared-tree fallback, not a live credential leak.
- **Suggested change:** exclude crew state for every Git-backed workspace before any agent operation; also explicitly reject staged/tracked crew state rather than relying solely on ignore rules. Do not offer an unqualified whole-tree commit as a per-agent operation in shared mode: require a defined shared integration owner or explicit file ownership/path selection, and make task/finishing prompts reflect the available workflow.
- **Kind:** bug. **Confidence:** confirmed by test and daemon call trace.

### R2 — P1: herdr's delivery fallback can execute teammate text in a shell

- **Location:** `crates/crew/src/process/herdr.rs:631`, especially `:638-640`; liveness check at `:612`.
- **Code:** after repeated `agent_not_ready`, it calls `"pane.send_input"` with `json!({ "pane_id": pane.pane_id, "text": line, "keys": ["enter"] })`. Sanitization only replaces newline/control characters; shell metacharacters remain.
- **Failure:** a CLI exits or herdr stops recognizing it while a delivery is retrying. The pane remains at its shell. The method checked `pane.alive` only before the retry loop and never verifies the occupant before the raw fallback. A message containing shell examples, `;`, command substitution, or redirection can therefore be executed with the user's privileges. Updating `pane.alive` during the retries does not stop this fallback.
- **Evidence:** `herdr_sends_shell_syntax_after_agent_not_ready` used a temporary fake herdr socket. After 12 `agent_not_ready` answers, it recorded the original string `[crew] message: please inspect this; printf fixture-only > marker` and `keys: ["enter"]` in `pane.send_input`, with no `pane.get` check. The test did not execute the payload in a real shell; the unsafe sink is confirmed. The source itself documents that the pane returns to its shell when the CLI exits (`:368-370`).
- **Suggested change:** fail/requeue delivery when herdr cannot verify a recognized live agent. Remove the raw fallback for agent runtimes; recheck cancellation/liveness during retries. Any future fallback must atomically validate the intended occupant with submission, not merely check and then type into a reusable shell pane.
- **Kind:** bug. **Confidence:** confirmed by fake-server test and trace.

### R3 — P1: killing a PTY agent leaves live descendants once its leader exits

- **Location:** `crates/crew/src/process/pty.rs:373-385`; waiter at `:288-290`.
- **Code:** `if !proc.alive.load(Ordering::SeqCst) { return Ok(()); }` both before signalling and inside the TERM grace period. `proc.alive` becomes false when `child.wait()` returns for the direct child only.
- **Failure:** a compiler, shell helper, MCP server, or other descendant ignores TERM/HUP while the agent leader exits. `kill` returns before SIGKILL escalation because the leader is gone, although the process group is still occupied. The remaining process can keep changing the worktree after an agent is marked stopped or restarted; inherited PTY fds also keep reader threads/resources alive. A leader that exits naturally produces the same early-return problem on later cleanup.
- **Evidence:** `pty_kill_returns_with_live_descendant` spawned a shell with a same-process-group child ignoring TERM/HUP. After `PtyManager::kill`, the manager reported `alive=false`, while `/proc/<child>/status` showed a non-zombie child. The fixture group was then explicitly killed.
- **Suggested change:** distinguish leader exit from process-tree cleanup; retain a supervised group/session or cgroup and complete bounded TERM/KILL cleanup even after the leader exits. Account for descendants that form new process groups, and reap everything the daemon owns. Add a descendant-survival regression, not only a single-process kill test.
- **Kind:** bug. **Confidence:** confirmed by process test.

### R4 — P2: Codex's app-server lacks the agent identity that protects CLI use

- **Locations:** `crates/crew/src/agents/codex.rs:133` and `:180`; `crates/crew/src/cli/ops.rs:268` and `:289-313`; `crates/crew/src/daemon/supervisor.rs:551`.
- **Code:** the sidecar receives `env: env.clone()`, but `IDENTITY_ENV` is added only afterward to the TUI launch: `env: std::iter::once((IDENTITY_ENV.to_string(), identity)).chain(env).collect()`. The plain CLI guard is `if std::env::var_os(paths::IDENTITY_ENV).is_some()`.
- **Failure:** socket delivery runs the model/tool session in the app-server. The daemon normally removes its invoking agent marker at startup, so the sidecar/tool environment has no `CREW_IDENTITY`. Agent-side `crew ctl` cannot discover its identity, and plain `crew "some objective"` bypasses the agent guard, discovers the workspace daemon, and uses `Client::operator` to send a message labelled as human operator input.
- **Evidence:** `codex_sidecar_omits_identity_and_creates_unbindable_socket` asserts the TUI env includes `CREW_IDENTITY` and the sidecar env does not. This review's Codex tool shell also had no marker. The operator-routing consequence is established by the CLI call trace. The baseline reviewer's accidental operator message was a **different** case: it had deliberately unset its otherwise-present Claude identity during test setup. That incident is not evidence of broken Claude propagation.
- **Suggested change:** pass the same identity/agent context to the app-server and its tool environment, and test `crew ctl` plus plain-CLI rejection from a fake Codex tool child. Separately, `Target::connect` (`ops.rs:40-42`) always uses `Client::operator`, so explicit operator subcommands bypass the plain-CLI guard even with a marker. Coordinate a consistent CLI guard and documented same-UID trust boundary with the core reviewer. Environment checks alone are not protection against a deliberately hostile same-UID process.
- **Kind:** bug. **Confidence:** confirmed by launch test and call trace; no new operator message was sent to the live daemon.

### R5 — P2: repository initialization consumes staged user work but omits untracked source

- **Locations:** `crates/crew/src/git/mod.rs:131-138`; caller `crates/crew/src/daemon/team.rs:86-97`.
- **Code:** `cmd.args(["commit", "-q", "--allow-empty", "-m", "Initial commit"]).output()?;` uses the existing index. `--allow-empty` permits an empty commit; it does not force one.
- **Failure:** a team file with `workspace.init: true` targets an unborn repository. If the operator has staged WIP, crew commits it under crew's identity without identifying that side effect. If the code is all untracked, the initial commit contains none of it; worktree isolation becomes usable and workers receive empty checkouts, even though the orchestrator can see the input project in the main checkout. A partially staged repository creates a misleading partial baseline.
- **Evidence:** `initial_commit_consumes_preexisting_index_and_omits_untracked_code` staged `staged.txt`, left `untracked.rs` untracked, called `init_repo`, and verified `staged.txt` entered the initial commit while the resulting agent worktree lacked `untracked.rs`.
- **Suggested change:** detect a nonempty/index-bearing unborn repository before initialization and require a deliberate project baseline. Preserve the existing index if making a genuinely empty commit is intended. Do not silently switch a nonempty source workspace into empty or partial worktrees; clearly describe the required initial commit to the operator.
- **Kind:** bug. **Confidence:** confirmed by Git test.

### R6 — P2: default Codex sockets fail in valid, moderately nested workspace paths

- **Location:** `crates/crew/src/agents/codex.rs:118-124`.
- **Code:** `ctx.agent_dir.join(format!("codex-{}.sock", crew_core::util::now_ms()))` is passed directly as the app-server Unix socket.
- **Failure:** the pathname includes the absolute workspace, `.crew/agents/`, agent id, and timestamp. A valid long project path or long agent id exceeds the Unix-domain socket pathname limit. Socket delivery is the default, so affected Codex members cannot start. The main daemon already hashes its socket name into a runtime directory, but this sidecar does not.
- **Evidence:** the launch/socket test generated a 144-byte path in an ordinary temporary nested directory. `UnixListener::bind` failed with `path must be shorter than SUN_LEN`. No Codex installation is needed to reproduce the OS limit.
- **Suggested change:** allocate short, unique sidecar sockets in a private runtime directory keyed by workspace/agent/incarnation; validate the final byte length, including a long runtime directory, before launch. Clean up socket artifacts during teardown.
- **Kind:** bug. **Confidence:** confirmed by launch and OS socket test.

### R7 — P2: herdr ignores the validated/resolved pi executable

- **Locations:** `crates/crew/src/process/herdr.rs:534-549`; `crates/crew/src/daemon/supervisor.rs:364-387`.
- **Code:** the supervisor resolves pi and assigns `spec.program = program.to_string_lossy().into_owned()`, but herdr's `agent.start` sends only `name`, `kind`, `pane_id`, `args`, and `timeout_ms`. Only the Shell branch uses `spec.program` (`:575`).
- **Failure:** `CREW_PI` names the correct pi binary, or the first `pi` on PATH is GSD and crew finds the supported pi later/on a user install path. Validation succeeds, PTY mode launches the selected binary, but herdr asks for its generic `kind: pi` without conveying the selection. It can launch the wrong CLI or fail to find the program; the validated executable choice has no effect.
- **Evidence:** `herdr_discards_resolved_program_path` supplied an explicit program path and recorded every fake herdr request; none contained that path.
- **Suggested change:** use a herdr API that accepts the selected program, or a verified launcher/environment arrangement that preserves this exact selection. If the backend cannot honor it, return a clear unsupported-configuration error rather than silently choosing another executable. Cover both backends in pi selection tests.
- **Kind:** bug. **Confidence:** confirmed by fake-server test and trace; exact wrong-binary behavior depends on the herdr installation.

### R8 — P2: ordinary conversation text can be interpreted as an account limit

- **Locations:** `crates/crew/src/limits.rs:499-508`, `:535-538`; caller `crates/crew/src/daemon/limits.rs:377`, at `runtime_for(runtime).detect_limit(&screen, now)`.
- **Code:** `line.contains("usage limit reached")` accepts any line, and the subsequent reply/prompt checks inspect only lines **after** the matching line.
- **Failure:** a working agent is quiet while its last prompt, explanation, or tool output discusses a usage-limit error. The content is mistaken for an actual Codex account error. The daemon calls `agent_limited`, parking otherwise healthy work and potentially triggering configured model fallback. The same content-sensitive design affects the Claude/pi screen fallback matchers; plain numeric substrings in pi error classification are also broad.
- **Evidence:** `ordinary_text_is_misread_as_usage_limit` returned `Some(LimitInfo { kind: Usage, ... })` for both `› Add a test for the message usage limit reached` and `• The app should display usage limit reached.\n›`. This proves the parser false positive; the resulting park/fallback is confirmed by the daemon call trace, not a live-account test.
- **Suggested change:** prefer structured failures and require a recognizable runtime error block for the screen fallback. Exclude user prompt lines and normal assistant/tool content, including the candidate line itself. Add negative fixtures containing quoted error messages and code examples.
- **Kind:** bug. **Confidence:** confirmed by parser test and trace.

### R13 — P2 (follow-up): an overflowing quota poisons typed session messages

- **Locations:** `crates/crew/src/limits.rs:89`, `:138`, and `:197`; `crates/crew/src/daemon/limits.rs:432-445`; `crates/crew-core/src/rpc.rs:557`. Rechecked at 00:26 -0500; `limits.rs` still matched the timestamped snapshot. This follows the quota observation in finding 4 of [core-lib-and-tui.md](core-lib-and-tui.md); existing runtime finding ids are retained.
- **Code:** `percent_used: w.get("percentUsed")?.as_f64()? as f32` (equivalent casts in the cached-Claude and Codex readers); the daemon copies `percent_used: r.percent_used` into `Quota`, whose field is a non-optional `f32`.
- **Failure:** a valid JSON number such as `1e300` is finite as `f64` but becomes infinity as `f32`. It is accepted as a quota, stored in the daemon model, and serialized by serde_json as `"percent_used":null`. Typed `Quota` deserialization rejects null. Session pushes and snapshots containing this quota consequently fail for clients until a valid replacement reading clears it, disrupting dashboard updates and status retrieval.
- **Evidence:** `overflowing_quota_cannot_round_trip_over_rpc` called the actual Claude quota parser with `percentUsed: 1e300`, serialized the resulting `Quota`, and reproduced `invalid type: null, expected f32`. The unchanged daemon copy/push path confirms propagation; no live daemon was involved.
- **Suggested change:** validate finiteness and supported numeric range before narrowing to `f32`, and reject invalid readings at the state-update boundary too. Preserve valid over-100 spend readings where the protocol allows them. Add serialization round-trip tests with extreme positive/negative values for all three parsers.
- **Kind:** bug. **Confidence:** confirmed by parser/RPC test and daemon call trace.

### R9 — P2: endpoint reports reveal credentials supplied through `env`

- **Locations:** `crates/crew/src/endpoints.rs:649-650`, `:207-210`, and `:359`; CLI prints the report at `crates/crew/src/cli/ops.rs:681`.
- **Code:** `row(out, "env", &format!("{name}={value}"), *scope);`; `has_literal_secret` considers only `api_key` and `auth_token` fields.
- **Failure:** supported environment overrides such as `[claude.env] ANTHROPIC_API_KEY = "..."` or a provider token print in full in `crew config`. An operator can copy a diagnostic report into logs/issues believing the documented masking applies. Such a config also misses the warning for a literal secret in a broadly readable file.
- **Evidence:** `config_report_prints_env_credentials` used only a synthetic key in an isolated config. `Layers::describe()` contained the entire key and `warnings()` was empty.
- **Suggested change:** display environment variable names and origins without values by default, or classify sensitive names consistently and redact them. Include env credentials in permission checks; avoid exposing their values via derived debug output as well.
- **Kind:** bug. **Confidence:** confirmed by config test.

### R10 — P2: a valid agent named `integration` prevents integration

- **Locations:** `crates/crew/src/git/mod.rs:22`, `:175-176`, `:194-198`, and `:417`; `crates/crew-core/src/config.rs:684`.
- **Code:** `INTEGRATION_BRANCH` is `"agents/integration"`, while `branch_for(agent)` is `format!("{BRANCH_PREFIX}{agent}")`; validation reserves only `orchestrator` and `operator`.
- **Failure:** a team legitimately names an integration worker `integration`. Its worktree owns `agents/integration`. Later `crew integrate` tries to create `_integration` on that same branch, and Git refuses because it is already checked out. Collection of every agent's work is blocked by an accepted id.
- **Evidence:** `valid_integration_agent_collides_with_collector_branch` created that worker, called `integrate`, and received `fatal: 'agents/integration' is already used by worktree .../integration`.
- **Suggested change:** reserve the colliding id consistently at team and dynamic-agent validation, or place the collector branch in a namespace agent branches cannot occupy. Add a validation/integration regression. The core-library owner was notified.
- **Kind:** bug. **Confidence:** confirmed by Git test and validation trace.

### R11 — P2: pi's advertised probe deadline does not cover reading stdout

- **Location:** `crates/crew/src/agents/pi.rs:126-156`, especially `Ok(Some(_)) => break` and `reader.join().ok()`.
- **Failure:** a pi wrapper exits but leaves a helper holding inherited stdout. `output_within` stops checking the deadline as soon as the direct child exits and then joins an unbounded reader thread. `crew models`, availability checks, or pi startup can wait indefinitely despite the 10/15-second advertised limits. On the timeout path it also kills only the direct child and drops the reader handle, leaving the reader/helper alive until EOF.
- **Evidence:** `pi_probe_timeout_does_not_cover_inherited_stdout` used a fake executable that printed help, spawned `sleep 12` inheriting stdout, and exited immediately. The 10-second `check_binary` probe returned after **12.003 seconds**. An indefinitely running child would leave the join unbounded.
- **Suggested change:** apply one deadline to process exit plus pipe draining, bound output size, and terminate/reap the probe process group on failure. Use cancellable/nonblocking reads rather than an unconditional reader-thread join.
- **Kind:** bug. **Confidence:** confirmed by timed fake-executable test.

### R12 — P2: a hung Git hook blocks every crew Git operation without a deadline

- **Locations:** `crates/crew/src/git/mod.rs:50-54`, `:274-276`, `:380`, and `:396-398`; `crates/crew/src/daemon/gitops.rs:39-42`.
- **Code:** Git operations use blocking `.output()?`; the daemon holds `self.git_lock.lock().await` around `spawn_blocking(f).await`. The comment explicitly retains project commit hooks.
- **Failure:** a repository hook, filter, or external Git helper never exits. Disabling signing/editor/credential prompts does not prevent this. The operation retains the global daemon Git lock indefinitely, stopping status/diff/commit/integration for all agents. A caller timing out does not cancel the blocking operation or prevent a late commit after the caller has given up. Output is collected in full before diff truncation, so a huge helper output also has no memory bound.
- **Suggested change:** supervise Git subprocesses with bounded execution/output and process-group cleanup; represent timeout/uncertain outcomes explicitly, and reconcile Git state before retrying. Preserve legitimate project hooks while bounding their impact. Test a stuck pre-commit hook and client cancellation.
- **Kind:** risk. **Confidence:** confirmed by call trace; an indefinitely hung Git fixture was not run.

## Cross-area findings affecting the runtime

- **Buffered bridge delivery is lost:** `cli/agent_side.rs:128` calls `client.call_value(Call::Listen)?`, then `:131` calls `client.next_frame()`. A `Deliver` push received before the Listen reply is buffered by `crew-core/src/client.rs:111` (`self.pushes.push_back(push)`) but `next_frame` at `:102-104` reads only the socket. rev-tui reproduced the loss with a fake server; the client fix and primary finding are in [core-lib-and-tui.md](core-lib-and-tui.md), finding 2. The bridge needs an API that drains pending pushes. Avoid changing `next_frame` in isolation so `call_value` does not repeatedly consume and re-buffer the same push.
- **MCP client reuse after timeout:** `mcp/mod.rs:73` reconnects only when `looks_disconnected(&e)` matches selected strings; `:83-87` excludes timeout. rev-tui traced partial-line consumption in `crew-core/src/client.rs` that makes reuse after timeout unsafe. Keep typed error classification and connection invalidation in the client/bridge contract; see [core-lib-and-tui.md](core-lib-and-tui.md), finding 14, and coordinate with rev-core. Retrying a mutation after a timeout also needs an explicit duplicate/uncertain-outcome policy.

## What is solid

- PTY launches use structured argv; shell command construction quotes program/arguments where necessary. Git revision validation rejects option injection, and root checks prevent accidentally committing a parent repository from a nested directory.
- Generated identity/config files use private-file helpers, and explicit endpoint credential fields use redacted secret wrappers or private/helper-based transport. Endpoint layers validate ownership/write permissions and reject tracked project endpoint files.
- Normal Git conflict paths check for dirty worktrees, abort conflicting merges/cherry-picks, and have targeted tests. Runtime hooks generally tolerate missing or malformed optional data; transcript readers limit displayed content and handle incomplete JSONL records without requiring every row to parse.
- There is meaningful coverage for shell-process startup/failure, direct messages, task dependencies, pause/restart, ordinary worktree exchange, launch argument construction, endpoint layering, and parser examples. `scripts/build-env` passes its argv directly and avoids shell reconstruction.

## Riskiest untested paths

- Real current Claude/Codex/pi launch, resume, tool-child environment, socket transport, and parser compatibility. `real_cli.rs` covers only Claude and is ignored; it was deliberately not run. Green string-construction/fake-CLI tests do not establish compatibility with installed provider CLIs.
- herdr disconnects/reconnects, exit during delivery retries, wrong occupants, failed `pane.close`, startup failure cleanup, adoption after daemon restart, and switching executables. Existing herdr unit tests mostly cover helper parsing/names/unreachable sockets.
- Whole process-tree teardown, descendants with separate process groups, daemon crash during spawn, PTY backpressure, and allocation/thread-start failures after a child exists. PTY query handling also matches per read chunk, so split query escape sequences need explicit tests.
- Shared/nonempty/unborn repositories, staged operator work, state files already in the index, simultaneous agent edits, reserved branch collisions, hung hooks, and failed abort operations. The existing happy-path worktree tests do not cover these cases.
- Sessions resumed days after their original rollout path, partial or reordered external JSON, quota/error phrases quoted inside conversation, and account/model cache staleness. Transcript/model/limit code should use versioned realistic fixtures and negative examples.
- Detached daemon startup timeout/child reaping, shutdown with stopped children, and signal/terminal restoration for raw attachment. These were traced but not exercised against the live team.

A later cleanup can separate CLI setup/launch/discovery from report rendering in `cli/ops.rs`, and split external-format parsing from filesystem lookup in `limits.rs`. Those structural changes should follow the behavioral fixes and preserve the newly established failure tests. No cleanup or product fix was performed by this reviewer.
