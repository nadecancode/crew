# Review: crew-core (config, rpc, types, client, paths, util) and crew-tui

Reviewer: rev-tui. Review only: no source file was changed.

**Scope.** All of `crates/crew-core/src/` and `crates/crew-tui/` (src, `tests/render.rs`, `examples/preview.rs`). I followed callers into `crates/crew` only to judge impact. Those spots belong to rev-core and rev-runtime and are marked *cross-area*.

**Source state.** I read the main checkout at `/home/nade/workspaces/personal/crew` up to about 00:20 on 2026-10-03.
- Every other file in my area was last modified by 23:54 on 10-02 (the newest is `crew-core/src/rpc.rs`).
- The operator then edited three TUI files at 00:21: `crew-tui/src/lib.rs`, `app.rs` and `input.rs`. The change makes a new question's answer box open by itself, with a bell and an 800 ms Enter grace period.
- At 00:22 I re-located every line reference in those three files, re-ran the probes and the crate tests against the edited source, and got the same results.
- Line numbers below are from 00:22. Each finding quotes the offending line so it can be found again if lines move.

**How findings were confirmed.**
- **Targeted test run.** `cargo test --locked --offline -p crew-core -p crew-tui`, with `CARGO_TARGET_DIR` in my scratchpad, run again at 00:22. crew-core: 20 passed. crew-tui lib: 78 passed, 1 ignored (the live-TTY test). `tests/render.rs`: 35 passed.
- **Probe crates.** I wrote two scratch crates under `/tmp/claude-1000/-home-nade-workspaces-personal-crew/3e2101eb-…/scratchpad/`:
  - `ratexp/` depends on ratatui 0.29.0 only.
  - `probe/` uses `crew-core` and `crew-tui` as path dependencies on the main checkout. It was built between about 00:05 and 00:20, and rebuilt and re-run at 00:22.
  - Both keep their own `Cargo.lock` and `target/`. Nothing was written to the checkout, and no test touches the live daemon's `.crew/` or socket.
- **Labels.** "confirmed (test)" means a probe test reproduced the finding. "confirmed (trace)" means I followed the code path end to end. "plausible" means the reasoning holds but I did not reproduce it.

---

## Findings, most valuable first

### 1. Text from agents and runtimes reaches the operator's terminal raw in the inspector (escape-sequence injection)
- **Kind:** bug (security). **Confidence:** confirmed (test).
- **Root cause.** In ratatui 0.29, rendering a `Span`, a `Line` or a `Paragraph` copies every grapheme into the buffer, control characters included. Only `Buffer::set_string` and `Buffer::set_line` filter them out. `CrosstermBackend` then prints each cell's symbol verbatim. The probe confirmed this: `Span::render`, `Line::render` and `Paragraph` (with and without wrap) left `\u{1b}` and `\u{7}` in cells, while `set_string` and `set_line` left none.
- **Where it happens.** The inspector draws every panel with `Paragraph::new(lines).block(block).render(area, buf);` (`crates/crew-tui/src/views/detail.rs:196`). Several of the strings it is given are never sanitized:
  - **Artifacts.** `detail.rs:409`: `let mut text = format!("   ⬡ {} {}", artifact.kind, artifact.value);`. An agent sets these through `task_complete(artifacts)`. The daemon only trims and truncates them (`crates/crew/src/daemon/orchestrate.rs:500-502`).
  - **Task title.** `detail.rs:242`: `agent.task.clone().unwrap_or_else(|| "—".into()),`. The orchestrator sets it through `task_create`.
  - **Model and effort.** `detail.rs:257`: `runtime.push_str(&part);`. `part` is `model_label()` or `effort`, both reported by the runtime's session.
  - **Fallback note.** `detail.rs:269`: `value.push_str(&fallback.note);`. This is the runtime's own text.
  - **Workdir and session.** `detail.rs:306` (`agent.workdir.clone()`) and `detail.rs:315` (`agent.session_id.clone()…`). The session id arrives through hook payloads, which any process holding the agent's identity can send. `backend_ref` and `branch` take the same path.
- **What the probe showed.** Each of these fields, set to `"\x1b]52;c;…\x07\x1b[2J"`, produced ESC and BEL cells when the inspector was rendered. The dashboard, the bus row, the message box, logs, tasks and the question band produced none, because they go through `put_str` or `one_line`.
- **Impact.**
  - An agent writes an artifact such as `{"kind":"commit","value":"\u001b]52;c;<base64>\u0007"}`.
  - When the operator opens that agent's inspector, the sequence goes straight to the operator's terminal. OSC 52 writes the operator's clipboard, so a later paste into a shell runs the attacker's command.
  - The same route can also retitle the terminal, clear the screen or inject OSC 8 links. On some terminals, query sequences cause answerback.
  - Agents run with `--dangerously-skip-permissions` by default and read untrusted repositories, so this is a realistic prompt-injection-to-operator path.
- **The launcher has the same gap, at lower risk.** `crates/crew-tui/src/launcher.rs:446`, `:453` and `:460` render style `description`, `details` and `instructions` through `Paragraph`. Project-origin styles come from the checked-out repository.
- **Suggested change.**
  1. Add one `printable(&str) -> Cow<str>` helper (in `crew_core::util` or `widgets::text`) that drops C0 and C1 controls and expands tabs.
  2. Apply it at the widget boundary rather than at each call site. In `detail.rs`, map every span through it in `panel_in()` before `Paragraph::new`. Do the same in `launcher::render_preview`.
  3. Fold `detail.rs:532 fn sanitize`, `launcher.rs:290 fn sanitize` and `util::one_line` onto that helper.
  4. Strip control characters at ingestion in the daemon as defence in depth (owners: rev-core and rev-runtime).
  5. Add a render test that salts every string field of the fixture with ESC and BEL, renders every view and modal, and asserts that no buffer cell contains a control character.

### 2. `Client::next_frame` never returns pushes that `call_value` buffered, so socket deliveries race the `listen` reply and stall
- **Kind:** bug. **Confidence:** confirmed (test of the client); daemon ordering confirmed (trace).
- **The client.** `crates/crew-core/src/client.rs:111` (`self.pushes.push_back(push);`) stashes any push that arrives while waiting for a reply. Only `subscribe()` drains that queue (`:136`, `buffered: self.pushes`). `next_frame()` (`:102-104`) reads the socket directly.
- **The caller.** `crew bridge` does `client.call_value(Call::Listen)?;` and then loops on `client.next_frame()` (`crates/crew/src/cli/agent_side.rs:128`, `:131`).
- **The daemon.** `Call::Listen` registers the listener (`daemon/handlers.rs:299`, `self.listener_joined(...)`) before the handler returns the reply (`:301`). `listener_joined` ends with `self.request_delivery(agent);` (`daemon/delivery.rs:99`). That spawns a task which can push `Deliver` onto the same connection's channel before the `{"listening":true}` reply is queued.
- **Effect.**
  1. The waiting messages end up in `Client.pushes` and the bridge never sees them.
  2. They are marked in flight, so after `IN_FLIGHT_TIMEOUT_MS` (20 s, `delivery.rs:32`) the daemon logs "socket delivery was not confirmed; using the terminal".
  3. The daemon drops back to typing into the terminal.
  4. Messages pending at session start are therefore late, and the agent loses socket delivery.
- **The probe.** A fake server sent `push(Deliver)` and then the reply. `call_value(Listen)` succeeded, and the next `next_frame()` timed out instead of returning the `Deliver`.
- **Suggested change.**
  - In `client.rs`, `next_frame` should return `ServerFrame::push(p)` for any buffered push first, or the client should offer a `next_push()` used by both the bridge and `PushStream`. The drain must not feed back into `call_value`: a buffered push handed out by `next_frame` must not be stashed again (rev-runtime's note).
  - Cross-area (rev-core): queue the `listen` reply before starting delivery.
  - Add unit tests for `client.rs`, which has none.

### 3. The operator role is self-declared: any process that can open the socket, every agent included, is the operator
- **Kind:** risk (security). **Confidence:** confirmed (trace). **Primary write-up:** `review/core.md` CORE-01 (rev-core). rev-core reproduced this live in isolation: a worker's `Snapshot` call is refused as forbidden, the same worker sends a second `Hello` with `role=operator`, and `AgentKill` then succeeds. This entry covers the crew-core protocol and CLI side only.
- **Where.**
  - The protocol's `Hello` (`crates/crew-core/src/rpc.rs:31-42`) carries a `role` and an optional `token`.
  - The daemon accepts the operator with no check: `Role::Operator => Ok(Who::Operator),` (`crates/crew/src/daemon/server.rs:178`).
  - In the CLI, only plain `crew` refuses to run inside an agent (`cli/ops.rs:268`, `if std::env::var_os(paths::IDENTITY_ENV).is_some()`). Every other subcommand connects as operator through `Target::connect` (`ops.rs:42`, `Client::operator(...)`).
  - `paths::discover` even routes such a command to the agent's own daemon, through `CREW_IDENTITY` (`paths.rs:220`).
- **Impact.**
  - Agents run as the operator's uid, with shell access and bypassed permissions by default. Any of them can run `crew …` (or speak the socket protocol) to act as the operator.
  - That covers `AgentSetModel` on the orchestrator (an operator-only decision), killing or restarting peers, answering operator questions, typing into other agents' terminals, and `Shutdown`.
  - The daemon's operator-only checks are therefore advisory.
- **Suggested change.**
  - Cheap fix, which catches honest mistakes: CLI operator commands refuse to run when `CREW_IDENTITY` is set.
  - Real fix, in the daemon: authenticate operator hellos by peer credentials. Read `SO_PEERCRED`, then reject a pid that belongs to an agent's process group or tree (the daemon knows them). Alternatively, require an operator token kept out of agents' environments.
  - Owner of the enforcement: rev-core. The protocol type lives here.

### 4. No working version negotiation: an older peer fails hard on any new push kind or enum value
- **Kind:** risk. **Confidence:** confirmed (test).
- **The version number carries no information.**
  - `pub const PROTOCOL_VERSION: u32 = 1;` (`rpc.rs:19`) was never bumped, although the protocol has grown (`ansi`, `AgentTranscript`, `questions`, `Compact`, …).
  - The daemon rejects only newer clients: `if hello.protocol > PROTOCOL_VERSION` (`daemon/handlers.rs:95`).
  - Clients ignore `HelloInfo.protocol` entirely (`client.rs:63`, `:76`).
- **Nothing tolerates unknown values.**
  - `Push` (`rpc.rs:524-525`, `#[serde(tag = "kind", content = "data", …)]`) has no fallback variant.
  - Neither does any `string_enum!` (`types.rs:21-27`): `AgentStatus`, `TaskStatus`, `MessageType`, `RuntimeKind`, `EventLevel`, `TranscriptKind`.
- **What the probe showed.**
  - `{"push":{"kind":"something_new",…}}` gives `unknown variant 'something_new'`.
  - A snapshot whose agent status is `"compacting"` gives `unknown variant 'compacting'`.
  - The whole frame or snapshot is lost in both cases.
- **The same failure from a bad number.** `Quota.percent_used: f32` (`rpc.rs:557`) serializes a non-finite value as `null`, and that does not parse back ("invalid type: null, expected f32", confirmed). A single odd usage figure from a runtime therefore breaks `Session` pushes and snapshots in the same way.
- **What it does to the dashboard.**
  - `net.rs:80` (`match stream.next()? {`) turns the protocol error into a disconnect, and the dashboard reconnects every second, losing pushes each time.
  - If the snapshot itself fails (`net.rs:75`), the dashboard never connects: it shows "connecting to the crew daemon…" forever, and `crew attach` fails at probe.
- **When it happens.** The documented `just link` workflow rebuilds `crew` while a daemon from the previous build keeps running. The CLI already handles one case of this skew (`stale_daemon_note`, `cli/ops.rs:186-189`).
- **Suggested change.**
  1. Add a `#[serde(other)] Unknown` variant to `Push` and to the status enums, and have `PushStream::next` skip unknown pushes.
  2. Bump `PROTOCOL_VERSION` on incompatible changes, and have `Client::operator` and `Client::agent` compare it, with an error like "the daemon is from another crew build; restart it".
  3. Clamp quota floats where they are parsed (`crates/crew/src/limits.rs:89`, `:138`, `:197`), or deserialize them leniently. rev-runtime reproduced this independently through the real parser and owns that fix as `review/runtime.md` R13.
  4. Add a compatibility test that feeds the client a frame "from the future".

### 5. Dashboard actions time out at 3 s while the daemon takes longer: reported as failed, but carried out
- **Kind:** risk. **Confidence:** plausible (trace).
- **Where.** `const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);` (`crates/crew-tui/src/net.rs:29`) applies to every action on the requester connection (`:213`). That includes `Command::Kill`, `Command::Restart` and `Command::SetModel` (`:308`, `:309`, `:354`).
- **How long the daemon takes.**
  - A pty kill waits up to 2 s after SIGTERM and up to 1 s after SIGKILL (`crates/crew/src/process/pty.rs:379`, `:386`, `for _ in 0..20`).
  - A restart is a kill plus a spawn, and setting a model restarts the session.
  - Each herdr call may take up to `CALL_TIMEOUT` = 15 s (`process/herdr.rs:31`).
- **Effect.**
  - On timeout the requester drops the connection (`net.rs:221-225`), so the real answer is discarded.
  - The footer says "restart claude-3: the crew daemon did not answer in time" for a restart that did happen. An operator who retries restarts the agent twice.
  - There is also a single requester thread: a 20 s model listing or a 90 s stop blocks every action queued behind it.
- **Suggested change.**
  - Give actions per-command timeouts, as `STOP_TIMEOUT` and `MODELS_TIMEOUT` already are (about 30 s for kill, restart, set-model and resume).
  - Or report "sent" and let the agent push confirm.
  - Run the long calls (models, stop) off the main requester thread.

### 6. The `/tmp` fallback for the socket directory follows whatever is at `/tmp/crew-<uid>`, including a symlink
- **Kind:** risk (security; affects only shared hosts without `XDG_RUNTIME_DIR`). **Confidence:** plausible (trace).
- **Where.**
  - `runtime_dir()` falls back to `_ => PathBuf::from(format!("/tmp/crew-{}", uid())),` (`crates/crew-core/src/paths.rs:103`).
  - The daemon prepares it with `ensure_private_dir` (`daemon/mod.rs:516`), which uses `if !path.is_dir()` (`paths.rs:67`) and then `fs::set_permissions(path, …0o700)` (`:74`). Both follow symlinks.
- **The attack.**
  1. Another local user pre-creates `/tmp/crew-1000` as a symlink to a directory the victim owns, so the chmod succeeds.
  2. After the daemon binds its socket, that user swaps the symlink for one pointing into their own directory.
  3. Every later connect resolves the path again (`client.rs:47`, `UnixStream::connect(socket)`), so it reaches the attacker's listener.
  4. Agents' tokens arrive there in `Hello`.
- **What already fails closed.** A real directory owned by the other user makes the chmod fail with EPERM.
- **Same issue elsewhere.** `registry_dir()` falls back to the same path when `HOME` is unset (`paths.rs:123`).
- **Suggested change.**
  - Use `symlink_metadata` and refuse symlinks. Require `st_uid == getuid()` and mode 0700, and create the directory with a single non-recursive `mkdir`.
  - Optionally, have clients check the daemon's uid with `SO_PEERCRED`.

### 7. Keyboard scrolling in the message and question boxes counts characters, not lines, so End then ↑ appears stuck
- **Kind:** bug (UX). **Confidence:** confirmed (test).
- **Message box.** `crates/crew-tui/src/input.rs:580`: `let lines = app.messages.iter().find(|m| m.id == view.id).map_or(0, |m| m.body.chars().count());`. End then sets `view.scroll = lines` (`:587`), and ↓ can also push the stored scroll past the end. The render only clamps the view (`views/message.rs:67`).
- **Question box.** Same pattern: `q.body.chars().count()` (`input.rs:752`), then `KeyCode::PageDown => answer.scroll = (answer.scroll + 5).min(*length),` (`:779`). The render only clamps the view (`views/question.rs:185`).
- **The probe.** With a 60-line message at 100×30, End then 5×↑ left `scroll` at 2264 and the frame unchanged. It takes about 2240 presses before the view moves.
- **The mouse path is already right.** It uses `message::line_count` (`input.rs:69`).
- **Suggested change.** Clamp to the wrapped line count minus the visible rows. Use `message::line_count`, with the columns from the last draw (as `detail.cols` does) or passed in with `page`.

### 8. Input-line rendering is quadratic in text length
- **Kind:** risk (performance). **Confidence:** confirmed (measured).
- **Where.** `views/compose.rs:131` and `launcher.rs:496`: `while chars[start..at].iter().map(cell).sum::<usize>() > room { start += 1; }`.
- **The cost.**
  - With the compose box at its 8000-character cap (`MAX_COMPOSE_CHARS`) and the cursor at the end, one 120×40 frame takes about 30 ms (probe, opt-level 2). A typical frame is about 0.4 ms.
  - While an animation pulse runs (33 ms frames) that pins a core and makes typing lag.
  - Plausible trigger: pasting a log into a message.
- **Smaller cost elsewhere.** `insert()` (input.rs and launcher.rs) recounts the characters on every inserted character. A 200 kB paste costs about 50 ms, which is acceptable.
- **Suggested change.** Walk back from the cursor, summing widths until `room` is reached (O(n)). Share one implementation (see 12).

### 9. A bracketed paste into an agent's terminal can be broken out of
- **Kind:** risk. **Confidence:** plausible.
- **Where.** `input.rs:148`: `text: format!("\x1b[200~{text}\x1b[201~"),`. The pasted text is not checked for the end marker.
- **Effect.** Pasted content that contains `ESC[201~` ends the paste early. Everything after it is typed into the agent's TUI as keystrokes, which is the classic paste-injection pattern.
- **Suggested change.** Strip `\x1b[201~` (or every ESC) from `text` before wrapping it.

### 10. Config validation accepts limits that make the daemon misbehave
- **Kind:** risk. **Confidence:** confirmed (trace).
- **Where.** `TeamConfig::validate` checks only `max_message_bytes` among `Limits` (`crates/crew-core/src/config.rs:638`: `if self.limits.max_message_bytes == 0 || …`).
- **What gets through.**
  - `idle_nudge_secs: 0` and `unresponsive_secs: 0` nudge or flag an agent immediately (`daemon/supervisor.rs:1540`, `:1563`).
  - `context_warn_percent` above 100 silently never fires.
  - Very large values overflow `(limits.idle_nudge_secs as i64) * 1000`.
  - `TaskSpec.timeout_secs: Some(0)` is accepted.
  - Agent `model` and `name` strings are not validated, although `model_fallbacks` entries are (`config.rs:651-675`).
- **Suggested change.** Add range checks in `validate()`, with errors that name the key, such as `limits.idle_nudge_secs must be between 10 and 86400`.

### 11. `integration` is a valid agent id but collides with the integration branch (cross-reference)
- **Kind:** bug. **Confidence:** confirmed (trace). The primary write-up is in `review/runtime.md` (rev-runtime).
- **Where.**
  - `validate_agent_spec` reserves only `orchestrator` and `operator` (`config.rs:684`: `if agent.id == ORCHESTRATOR_ID || agent.id == crate::types::OPERATOR_ID {`).
  - Agent branches are `agents/<id>` (`git/mod.rs:175`), and the integration branch is `"agents/integration"` (`git/mod.rs:22`).
- **Suggested change, crew-core side.** Reserve `integration`. Better, keep agent branches and the integration branch in separate namespaces.

### 12. Duplicated text-input and sanitizing code
- **Kind:** cleanup. **Confidence:** confirmed (read).
- **Where.**
  - The launcher re-implements the compose editor. Its `byte_at`, `insert`, `delete_range`, Ctrl-W, Ctrl-U and Ctrl-K handling (`launcher.rs:100-290`) duplicates `input.rs` `byte_at`, `insert` and `edit`.
  - Its `render_objective` (`launcher.rs:470-510`) duplicates `views/compose.rs::input_line` (`:372-405`).
  - There are three sanitizers: `util::one_line`, `views/detail.rs:532 fn sanitize` and `launcher.rs:290 fn sanitize`.
- **Suggested change.** Extract a small line editor (text, cursor, `edit(key)`, `insert`) used by both editors, reuse `input_line` in the launcher, and keep one `printable()` helper (see 1). This also fixes 8 in one place.

### 13. Dead code and misplaced or stale comments
- **Kind:** cleanup. **Confidence:** confirmed (grep across the whole workspace).
- **Dead code.**
  - `pub fn parse_key_value` (`config.rs:744`) has no callers.
  - `pub const AGENT_H_CONTEXT` and `CENTER_H_CONTEXT` (`crew-tui/src/layout.rs:9-10`) are unused.
  - `pub fn put_cell` (`widgets/canvas.rs:36`) is unused.
  - Because they are `pub` in library crates, rustc does not warn about any of them.
- **Misplaced and stale comments.**
  - `/// Validate a single agent declaration (also used by \`crew agent add\`).` (`config.rs:645`) sits on `MAX_FALLBACK_MODELS`; `validate_agent_spec` (`:677`) has no doc comment.
  - `impl Default for MessageType` (`types.rs:76`) is placed before `MessageType` is defined, between `AgentStatus` and its impl.
  - `net.rs:111-115` stacks two doc comments on `Requests`. The first ("Run commands against the daemon on a worker thread…") is left over from `spawn_requester`.
- **Wide public surface.** crew-tui exports almost every module as `pub` (`lib.rs:9-21`) so tests and examples can reach them. Consider `pub(crate)` plus a narrow test API.

### 14. A `Client` is not safe to reuse after a timeout, and callers match on error text
- **Kind:** cleanup/risk. **Confidence:** plausible.
- **Partial lines are lost.** `read_frame` (`client.rs:215-236`, `let n = match reader.read_line(&mut line)` at `:219`) loses any partial line consumed before a read timeout. A `Client` used after `ClientError::Timeout` can therefore misframe.
- **Who is affected.**
  - The TUI drops its client on any non-RPC error, which is correct.
  - The MCP bridge keeps its client after a timeout. It decides whether to reconnect by matching the message text (`crates/crew/src/mcp/mod.rs:83`, `fn looks_disconnected(error: &str)`).
- **No length limit.** Reads are unbounded `String`s, with no line-length cap. That is fine against a trusted daemon, but it matters together with finding 6.
- **Suggested change.** Poison the client after a timeout (any further call returns `Closed`). Expose `ClientError::is_disconnect()` so callers stop matching on text.
- **Retries need care.** A request that timed out or hit a disconnect may still have run on the daemon. A caller that reconnects and retries a mutation, such as kill, restart or send, needs a policy for that uncertain outcome: an idempotency id like `SendParams.id`, or report "outcome unknown" instead of retrying. rev-runtime cross-references this from `review/runtime.md`. It also relates to finding 5.

### 15. Smaller items
- **`Identity::from_env()` hides the real error** (`paths.rs:50-53`, `Self::load(Path::new(&path)).ok()`). An unreadable or invalid identity file is reported as "CREW_IDENTITY is not set", and `discover` silently falls through to the cwd and the registry. Return `Result<Option<Identity>>`. *Nit.*
- **`is_live` trusts pid plus socket file** (`paths.rs:165-167`, `pid_alive(info.pid) && Path::new(&info.socket).exists()`).
  - After a daemon crash, a reused pid keeps the stale registry entry alive.
  - `discover` then returns a dead socket, and the user sees "cannot reach the crew daemon" instead of "no daemon is running".
  - The daemon's own start-up check also tries to connect (`daemon/mod.rs:518-522`). `list_sessions` could do the same.
  - *Risk, low.*
- **Unit-variant RPC methods reject `"params": {}`** (adjacent tagging, `rpc.rs:69`). For example `{"id":1,"method":"agents_list","params":{}}` fails with "invalid type: map, expected unit variant Call::AgentsList" (confirmed), while `"params": null` works. In-tree clients are fine; hand-written ones trip on it. Accept `{}` or document it. *Nit.*
- **Terminal state is restored only on a panic or a normal exit** (`crew-tui/src/lib.rs:193-247`, `fn enter_terminal` / `fn leave_terminal`). *Risk, low.*
  - SIGTERM, SIGHUP and SIGQUIT leave raw mode, the alternate screen and mouse capture on.
  - The panic hook also restores the terminal on a panic in any thread, while the UI thread keeps drawing.
  - The input thread may sit in `term::poll` for up to 200 ms after `leave_terminal` (`net.rs:397`) and swallow one event meant for the shell.
- **`Push::Shutdown` is treated as a disconnect** (`net.rs:87`, `Push::Shutdown => return Err(ClientError::Closed),`). After `crew stop` from another terminal, the dashboard shows "daemon disconnected — reconnecting…" forever. Exit, or say the team was stopped. *Nit.*

---

## What is solid

- **No render panics.** I swept the views at different sizes with the probe crate:
  - Every view (dashboard, inspector, tasks, logs), with every modal (compose, type, confirm-kill, confirm-stop, confirm-restart, question, message, model picker) and help, at every size from 0×0 to 90×40.
  - Teams of 0 to 14 members with subagents and wide, emoji or very long text, at 0–240 × 0–70 (step 3/2).
  - The launcher with 30 styles at every size from 0×0 to 200×60.
  - Nothing panicked.
- **Cheap redraws.** The dashboard with 10 agents at 220×60 costs about 0.4 ms per frame, and the inspector with 300 ANSI lines about 0.5 ms. The loop redraws only when something changed. It wakes once a second when idle, at the 150 ms spinner rate only while an agent works, and at 33 ms only while a message pulse travels (`app.rs` `next_wake`).
- **Safe paths for untrusted text.**
  - The ANSI parser (`widgets/ansi.rs`) keeps only SGR and cursor-forward, and drops OSC, other escape sequences and C0/C1 controls.
  - `put_str` drops characters of zero or undefined width.
  - Everything outside the inspector's panels goes through `put_str` and `one_line`.
- **Robust connection layer.**
  - The subscriber subscribes before fetching the snapshot, and this ordering is tested.
  - It reconnects with backoff.
  - The requester drops a connection in an unknown state.
  - Keys go over their own connection and are batched (tested).
  - The daemon caps request frames at 1 MiB.
- **Config.** `deny_unknown_fields` everywhere, id validation, cycle detection with the cycle path in the error, and messages that name the offending key.
- **Paths.** Private files and directories are created 0600 and 0700, and this is tested. Socket names are hashed so they fit `sun_path`.
- **The new question box that opens by itself (00:21) looks careful.** It never opens over another box, help or the inspector's terminal. An Enter within 800 ms of it opening is ignored. It draws through `put_str`.
- **Pure, well-tested reducers and key handling.** `app.rs` has about 20 tests, `input.rs` 23, `tests/render.rs` 35 assertion-bearing screen tests, and `net.rs` 4 against a fake daemon that speaks the real wire protocol.

## Riskiest untested paths

1. **`crew-core/src/client.rs` has no unit tests.** Framing, push buffering (finding 2), timeouts and partial lines (14), `Closed` and `PushStream` are untested.
2. **No test checks that rendered output is free of control characters** (finding 1).
3. **No compatibility test against a peer from an older or newer build** (finding 4).
4. **`paths::discover` and `list_sessions` are untested.** That covers env precedence, registry pruning, pid reuse and the `/tmp` fallback (findings 6 and 15).
5. **`lib.rs` `run`, `event_loop` and terminal setup and restore** are covered only by an ignored test that needs a real terminal. The same goes for the launcher's `run_launcher` loop.
6. **The `crew bridge` Listen path end to end** against the daemon's real ordering (finding 2).
7. **Input editing and rendering with long text** (finding 8) and the scroll clamping in the message and question boxes (finding 7).
