# Baseline and test health (agent: baseline)

Review only; no code, Cargo files or tests were changed. Checkout state: `main` has **no commits**; whole tree untracked.

## How it was run (isolation)
- Every cargo call went through `./scripts/build-env`, with `CARGO_TARGET_DIR=<scratchpad>/target` (outside the checkout; `target/release/crew` of the checkout untouched). Built binaries: `/tmp/claude-1000/-home-nade-workspaces-personal-crew/db314645-3354-4c24-9753-fbcfd49537bb/scratchpad/target/{debug,release}/crew`.
- Tests: `HOME`, `XDG_RUNTIME_DIR`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `CREW_GLOBAL_CONFIG` pointed at `/tmp/bl/*`; `CREW_IDENTITY`/`CREW_SOCKET` unset. (Integration tests already isolate themselves: `tests/common/mod.rs:63-118` uses a tempdir in /tmp and sets XDG_*, CREW_GLOBAL_CONFIG, CODEX_HOME. Unit tests that touch state use `tempfile`.) After the runs `/tmp/bl/run` and `/tmp/bl/state` were empty: the suite leaves nothing behind. `.crew/` in the checkout was not touched.
- `--ignored` tests were not run (listed below). herdr/tmux not involved.
- One slip of mine: `crew "style list"` (zsh did not word-split) was run from the checkout and sent that text to the live orchestrator as an objective. Reported to orchestrator. See finding 6 for the product-side lesson.

## Results
| Command | Result | Time |
|---|---|---|
| `cargo fmt --all --check` | **FAIL**: diffs in ~70 files (see 1) | 0.7s |
| `cargo clippy --workspace --all-targets` | **pass, 0 warnings** | 9s (warm) |
| `cargo build --workspace` | pass, 0 warnings | 10s |
| `cargo build --release --workspace` | pass, 0 warnings | 47s |
| `cargo test --workspace` (3 runs, isolated env, short XDG path) | **pass x3**, identical counts | 28s each |

Counts: crew lib 106, crew `team` integration 32 (≈26s, the bulk of wall time), crew-core 20, crew-tui lib 77 (+1 ignored), crew-tui render 35, `real_cli` 1 ignored. 272 tests listed; 0 failures, 0 flaky across 3 runs (plus 3 earlier runs, see finding 2).

## Findings, most valuable first

1. **cargo fmt fails across the workspace** — `cargo fmt --all --check`. ~70 files differ; worst: `crew/tests/team.rs` (211 hunks), `crew-tui/tests/render.rs` (84), `crew-tui/src/input.rs` (53), `crew/src/cli/ops.rs` (48), `crew/src/limits.rs` (51). Also non-test source in nearly every module. Why: no CI gate can pass; every future diff will be noisy. Fix: one standalone `cargo fmt --all` commit before anything else is refactored (so cleaners' diffs stay reviewable), then add fmt to CI/`just lint`. Kind: cleanup (high value for the cleanup phase; schedule it first).

2. **Environment-dependent unit test** — `crates/crew-core/src/paths.rs:274-281` `socket_paths_are_short_and_stable` asserts the socket path is `< 100` bytes but builds it from the live `XDG_RUNTIME_DIR`. With a long runtime dir (my first scratch dir, 3 of 3 runs) it fails: `.../xdg/run/crew/21d58979989559ee.sock` panicked at paths.rs:280. Passes with a short one. Deterministic, not flaky, but wrong under CI/containers/Nix sandboxes with long `XDG_RUNTIME_DIR`. It also tests the host environment rather than the code. Fix: test the hash-file name length with a fixed runtime dir, or temporarily override the env. More importantly: `sun_path` limit (~108) is a real production risk if `XDG_RUNTIME_DIR` is long — `socket_path` (paths.rs:107) has no length check or fallback to `/tmp/crew-<uid>`; rev-tui/rev-core may want to look. Kind: bug (test) / risk (prod).

3. **Test suite depends on wall-clock sleeps** — `tests/team.rs:156,1470,1476,1596,2102,2839` (400–1500 ms fixed sleeps), scripted agents with `sleep 300`/`sleep 1500` (team.rs:329-330), `common/mod.rs:193,207` poll loops (60 ms, bounded by a `TIMEOUT` deadline: fine). Fixed sleeps proving "nothing happened" are the likely future flake source on a loaded CI box; all 3 runs here were green and stable at 25.9–30.4s. Fix: replace with condition polling where an event can be awaited; keep negative-assertion sleeps short and documented. Kind: risk.

4. **Ignored tests (2)** — `crew/tests/real_cli.rs:30` "needs the Claude Code CLI and spends tokens" (`just test-real`); `crew-tui/src/net.rs:650` `live_dashboard_against_a_scripted_daemon` "needs an interactive terminal". Neither run. No other `#[ignore]`. Note `just test` (and README "no Claude Code or Codex needed") is accurate: scripted agents (`crew scripted-agent`) stand in. Tests do need `git` and `/tmp` writable. Kind: info.

5. **Test coverage shape** — about 70% of tests are in-process unit tests; production daemon flow is covered by the 32 end-to-end `team.rs` tests, which run in one 26s serial file. Nothing mocks the network; no tests need network. `unwrap()`/`expect` occur ~700 times, mostly in tests (not audited). Kind: info.

6. **Any process in the workspace gets operator authority on the live daemon (risk, trust boundary)** — exact command, run from the main checkout with HOME/XDG on temp dirs: `crew "style list"`. Output: `a team is already running here; sent the objective to orchestrator`; the orchestrator received it labelled "from operator" (confirmed by orchestrator). Discovery by cwd (`.crew/daemon.json`) with no agent identity (`CREW_IDENTITY` unset) yields operator rights, so an agent or any same-user process running `crew` in the workspace can message agents, answer questions, stop the team. HOME/XDG isolation did not prevent it. Trust question left to rev-core / rev-runtime. **Retracted (per rev-runtime, verified in source):** my claim that a lone mistyped word (`crew statuss`) is sent to a live team is wrong: `crates/crew/src/cli/mod.rs:485` `if let Some(word) = cli.objective.as_deref().filter(|o| o.split_whitespace().count() < 2) {` ... `bail!("{}", unknown_command(word));` rejects it before dispatch. A quoted multi-word `crew "style list"` is intentionally an objective, not a bug. With `CREW_IDENTITY` set, plain `crew` is refused at `crates/crew/src/cli/ops.rs:268` (`if std::env::var_os(paths::IDENTITY_ENV).is_some() {` ... `bail!`). The real gap (rev-runtime, owner rev-core): `Target.connect` (cli/mod.rs ~40-42) picks operator for explicit subcommands (`crew send`, `crew stop`, ...) regardless of the identity marker; not re-verified by me. Kind: risk.

7. **Repo hygiene — nothing is committed** (`git log`: no commits). Untracked, not ignored, unrelated to the product: `findings/` (208K), `notes/` (540K), `report.md` (36K; earlier research run), `.serena/` (36K; `.serena/project.local.yml` already ignored by its own inner .gitignore). Ignored by `.gitignore`: `/target` (7.7G), `/demo-workspace`, `.crew/` (11M live daemon state), `/objective-workspace`, `/mixed-workspace`. Fix before the first commit: decide on findings/notes/report.md/.serena (delete or add to .gitignore); add `.serena/`, `review/` policy as wanted. `Cargo.lock` is present and untracked: commit it (binary crate; `just install` uses `--locked` and fails without it). Kind: risk/cleanup.

8. **Secrets scan** — grep for `sk-…`, `ghp_`, `AKIA`, PEM headers across the tree (excluding target/.git): no hits. README/config docs reference env-var names (`GATEWAY_TOKEN`, `CREW_CODEX_API_KEY`) only. Identity tokens are written 0600 (paths.rs `write_private_file`). Not scanned: contents of `.crew/` (live state, deliberately untouched; 11M, gitignored). Kind: info.

9. **Dependencies** — `cargo tree -d`: only `hashbrown` (0.14/0.15/0.17), `rustix` (0.38/1.1) and `linux-raw-sys` duplicates, all transitive (rusqlite, ratatui/crossterm, tempfile); harmless, not fixable locally. No git deps, no `*` versions, all workspace deps use major-version ranges; `Cargo.lock` pins. **`serde_yaml 0.9.34+deprecated`** is unmaintained (crew-core config parser): risk, consider `serde_yml`/YAML-free later. `cargo audit`, `cargo deny`, `cargo outdated`, `cargo nextest` are **not installed**, so no advisory/licence check was done: run `cargo audit` before release. `rust-version = "1.82"` is declared but only rustc 1.93.1 was tested; MSRV is unverified. Release profile: thin LTO, strip. Kind: risk.

10. **Docs vs reality** — README/justfile verified: `just` recipes' commands match the scripts; `examples/{dry-run,demo,mixed,objective}.yaml` all exist; `--help` command list matches README sections (run, attach, status, stop, logs, messages, questions, integrate, agent, models, style, config); `crew models`, `crew config`, `crew status`, `crew style list` (in a neutral dir) work. Not verified: real-agent flows (demo/mixed), `just link`/`install`/`demo` (would overwrite the live binary / spend tokens), `crew run examples/dry-run.yaml` (needs its own workspace; not run to avoid the live daemon). README says "Requires Rust 1.82+" (unverified, see 9). `justfile` has no `fmt` recipe and `lint` omits `fmt --check`. `shell.nix` includes `rustup`, so the toolchain version is not pinned (no `rust-toolchain.toml`). Kind: nit/cleanup.

11. **Build artifacts** — checkout's `target/` is 7.7G (ignored). Fine, but the live `crew` is `~/.local/bin/crew -> checkout/target/release/crew`: any `just build` mid-run replaces the running team's binary. Kind: risk (operational).

## Suggested order for cleanup phase
1. Commit baseline tree (decide on findings/notes/report.md/.serena), including `Cargo.lock`.
2. `cargo fmt --all` as its own commit. 3. Fix finding 2 test. 4. Add fmt check to `just lint`; run `cargo audit`.
Re-run: `./scripts/build-env cargo test --workspace` with short isolated XDG dirs (≈28s).

## Re-verification against current source (after operator edits)
Re-checked in the main checkout; none of my findings was fixed during review.
- Finding 1: `cargo fmt --all --check` still fails, now 1114 `Diff in` hunks (was ~1105 per lead; 72 files).
- Finding 2: `crates/crew-core/src/paths.rs:274` `fn socket_paths_are_short_and_stable()` and `:280` `assert!(a.to_string_lossy().len() < 100, "{}", a.display());` unchanged; `:109` `pub fn socket_path(state_dir: &Path) -> PathBuf {` (my earlier "107" was off; lead's 109-111 is right).
- Finding 3: sleeps now at `crates/crew/tests/team.rs:156` (`std::thread::sleep(std::time::Duration::from_millis(400));`), `:1470` (1500), `:1476` (800), `:1596` (1500), `:2102` (1200), `:2839` (`from_millis(wait)`).
- Finding 4: `crates/crew/tests/real_cli.rs:30` `#[ignore = "needs the Claude Code CLI and spends tokens"]`; `crates/crew-tui/src/net.rs:650` `#[ignore = "needs an interactive terminal"]`.
- Test counts/timings in the table are from the tree as it was before the operator's edits; not re-run.

**Correction to finding 6.** My `crew "style list"` ran after my env script did `unset CREW_IDENTITY CREW_SOCKET`, so it ran with no agent identity; that is why it was attributed to the operator. It does not show what an agent that has `CREW_IDENTITY` gets. A shell with the identity set was checked later (CREW_IDENTITY present, non-empty). What remains true: a caller with no identity inside the workspace is treated as operator via cwd discovery, and a stray bare word becomes an objective. Whether an identity-bearing agent running plain `crew <word>` is attributed to itself is untested (suggested to rev-runtime).

## Flaky test found later (reported by fix-tui; read by baseline)
`crates/crew/src/process/pty.rs:587` (in `arguments_are_never_interpreted_by_a_shell`): `assert!(screen.contains("$(echo pwned); `id` && true"), "{screen}");` fails intermittently with an EMPTY screen (log: crew-fixes/logs/area-c/M7/test-first-flaky.txt, 106 passed / 1 failed in `crew --lib`). My 6 runs on the original tree and the 2 on `base` did not hit it, so it is rare (load-dependent). Likely cause (from reading the test, not reproduced): the test calls `wait_for_exit(&mut rx).await` and then reads `mgr.screen(..)` once; if the exit event can be delivered before the PTY reader has fed the process's last output into the vt100 screen, a fast `printf` leaves the screen empty. If that is the mechanism it is a race in production code too (exit notice vs. final output), not only in the test; fix in pty.rs by draining the reader before emitting exit, or have the test poll the screen. Kind: bug (flaky test, possible real race). Owner: fix-runtime.
