# Web dashboard final review: the architect's pass

Second reviewer of the dual final review (the first is `review/web-final-recheck.md`
and the reports it builds on). Reviewed tree: `agents/qa` at `f76be24`
(qa `7a4dcc8`, web `c7c99d4`); the fixes that followed were re-checked at the
final integration, `agents/qa` at `2c2441e7` (qa `862f344d`, web `e8551391`),
which is identical outside `review/` to this branch.

This pass is independent of the first reviewer's: it re-derives the security
properties of `notes/web-design.md` §security from the running program and a
real browser, and it does not repeat the suites the first reviewer reports
(Rust workspace, clippy, the nine live specs, the mock browser run). Every
check below was made by the architect alone unless it says otherwise.

## Verdict

**Approved at `2c2441e7`.** No finding of this pass is open that blocks: A1 and
A2 were fixed and re-checked, A3 and A4 are accepted as they are.

The first reviewer's verdict, from `review/web-final-recheck.md` (at
`e2fbaa62`): every production finding closed, no production bug open, the
aggregate approval withheld for two test gates with QA. Both gates are fixed
(`c7c99d4`, then `862f344d` after A1) and were re-checked here, as its model
was blocked before it could do so itself; its own confirmation of those two
fixes is the one thing this record cannot supply.

## Findings

| # | severity | owner | what | state |
|---|---|---|---|---|
| A1 | blocking (test gate) | qa | The live P10/P11 gate of `7a4dcc8` asserts a floor on the rate at which the daemon answers `send` calls (`web/e2e/live/perf.spec.ts`, "accepted per second": 0.9 × rate for the steady windows, 240/s for the bursts). On this host it failed five runs of five (jj; sandboxes required three times, off twice): 149–184/s against 240 in the burst log window, 158–179/s against 180 in the steady log window. The rate depends on the machine and on the backlog the scripted agents still work off from the earlier burst. The product was within every budget in the same runs: the page applied 2009–20110 pushes per second (the design's P10 is 200, P11 is 2000), frame p95 16.7–16.8 ms, no long task, heap 13.7 MB. Wanted: gate the load the design names, the pushes the page applies per second, and report the acceptance rate without a floor. | fixed by `862f344d`; re-checked: four runs of four green (see below) |
| A2 | P3 | web | With no orchestrator agent (`orchestrator: agent: false`) the graph has two tab stops in Chromium: the empty hub (`<g class="node hub empty">`, no role, no `tabindex`) takes focus before the roving node, because an SVG element with a focus listener is tabbable there. §ui says the graph is one tab stop. `web/src/graph/AgentNode.tsx`: `tabindex` should be `-1`, not absent. | fixed by `e8551391`; re-checked in the browser |
| A3 | P3, noted | vcs | Before `hello` a WebSocket message is refused above 4 KiB only once it has been read: the WebSocket layer accepts up to 1 MiB from the start, so 32 unauthenticated connections can make the daemon buffer 32 MiB for at most 5 s each. Bounded; no change asked. | open, accepted |
| A4 | P3, noted | vcs | `Web::start` records the server as serving before the accept tasks are spawned; if converting a listener failed in between, the record would stay without a listener. Not reachable in practice; no change asked. | open, accepted |
| A5 | note | web | After `crew web --stop` an open tab says "reconnecting" (read-only) until the 30 s give-up and only then "stopped"; the close reason the server sends ("the dashboard was stopped") is not used. This is what §transport specifies for a lost connection and it is safe (the token is already revoked; whoever answers must show the proof). After a daemon shutdown the tab stops at once. | as designed |
| A6 | note | qa | The README's example of `crew web` shows the `web` and `open` lines; the command also prints a `note` line (browsers, "paste it nowhere else"). | nit |

## The first reviewer's open gates, re-checked

The first reviewer's report closes every production finding and leaves two
test gates with QA. Its model was blocked before it could re-check the fixes,
so they were re-checked here.

- **The `perf` project discovered `e2e/live/perf.spec.ts`** (`m-df59db07f583`).
  Fixed by `c7c99d4`: `playwright test --project=perf --list` lists the six
  tests of `e2e/perf.spec.ts` and none under `live/`; the `mock` project lists
  none either; `vitest` passes 99 tests, the new project test among them.
- **The live load test did not assert the load it achieved** (`m-14fdfd2a3f42`).
  `7a4dcc8` asserts the preload (the daemon's log reaches 5000 events), the
  page's ring (5000 before anything is timed), that no call was refused, that
  every call was answered, and that the page applied what the daemon emitted.
  Those assertions hold. Its acceptance-rate floor was finding A1; `862f344d`
  gates the pushes the page applies per second instead (at least 200 in the
  steady windows, 2000 in the bursts) and keeps the acceptance rate as a
  reported number with a floor of 50.

### Re-check of the fixes at `2c2441e7`

- A1: the live P10/P11 test, four runs in a row on jj (three without
  sandboxes, one with them required), all green. Pushes applied by the page
  per second: 1739–2031 in the steady windows, 17205–20090 in the bursts;
  frame p95 16.7–16.8 ms in all sixteen windows; no call refused. The
  acceptance rate in the log windows was 164–190/s with the host lightly
  loaded too (load 2–4 on 32 cores), so the old floor measured the daemon's backlog, not the load on the
  page. QA reports one red run in six of its own (a frame p95 of 33.3 ms in
  the first steady window under host load): the timing limits remain
  sensitive to what else the machine does, which `CREW_PERF_SCALE` is for.
- A2: `vitest` 99 passed; `e2e/graph.spec.ts` 10 passed, the new test among
  them; in Chromium against a real dry-run team the empty hub has
  `tabindex="-1"` and `tab` goes from "stop team" to the one graph node and
  on to the bus.

## What was checked

### Code read against the design

`crates/crew/src/web/{mod,http,ws,assets}.rs`, `daemon/server.rs`, the web
paths of `daemon/handlers.rs` and `daemon/mod.rs`, `cli/ops/web.rs`;
`web/src/net/{session,conn,state}.ts`, `main.tsx`, `text.tsx`, the terminal
tab. All consistent with §transport and §security:

- two listeners, `127.0.0.1` and `[::1]`, bound with `std::net::TcpListener`
  (no `SO_REUSEPORT`), the label drawn after both are held; stop revokes
  codes and sessions, closes connections, then releases the listeners;
- exact `Host`, `GET` only, exact `Origin` on `/ws`; files by table lookup;
- one `Session` for both transports; on the web `hello` accepts only the
  operator role with a live web session token, the proof is returned only on
  success, and `web_open`, `web_close`, `load_team` are refused;
- the page reads its session once at the document's start, has no `storage`
  listener, makes no call and applies no push before the server's proof,
  requests the terminal's code at the start and never calls `import()` again,
  and removes its session when it stops; agent text is DOM text, links are
  `http:`/`https:` only with `noopener noreferrer`; no `innerHTML`, `eval`,
  `fetch`, cookie or service worker anywhere in `web/src`.

### Black box, a real daemon (dry-run team, built from this tree)

- Listeners: only `127.0.0.1:<port>` and `[::1]:<port>`. The label is 32 hex
  digits, the pairing code 64.
- `GET /` with the right `Host`: 200 on both addresses, with the designed
  Content-Security-Policy, `nosniff`, `no-referrer`, `Cross-Origin-Resource-Policy:
  same-origin`, `Cross-Origin-Opener-Policy: same-origin`, `X-Frame-Options: DENY`,
  `Cache-Control: no-store`, and no `Access-Control-*` header (also not on a
  preflight or with a foreign `Origin`).
- Eleven other `Host` values (`127.0.0.1`, `localhost`, `[::1]`, another
  label, the label without or with another port, a trailing dot, upper case,
  a sub-name, userinfo) answer 421; no `Host` answers 400.
- `POST`, `PUT`, `DELETE`, `OPTIONS`, `HEAD`, `PATCH` answer 405. Path
  traversal (`/../`, `%2e%2e`, `//`, `.git/config`, a source path) answers 404.
  A second `Host`, a body, `Transfer-Encoding`, garbage: 400.
- `/ws`: no `Origin`, `null`, another site, `https:`, a trailing slash, the
  numeric and `localhost` origins, a sub-name and a suffix all answer 403;
  two `Origin` headers 400; the exact origin 101.
- Before `hello`, twenty-two calls with valid parameters (`snapshot`,
  `subscribe`, `send`, `attach`, `events`, `agent_kill`, `agent_pause`,
  `agent_restart`, `agent_remove`, `agent_input`, `agent_output`,
  `agent_set_model`, `load_team`, `shutdown`, `agents_list`, `listen`,
  `web_open`, `web_close`, `web_prefs` and others) answer `unauthorized` and
  change nothing; only `ping` is answered.
- `hello` on the web with the operator's token file, an agent's token (as
  agent and as operator), no token, an empty one and a random one:
  `unauthorized`, no `web_proof` in the reply, and the connection stays
  unauthenticated. A wrong protocol version: `protocol`, no proof.
- A pairing code works once; the same code again and a random code are
  `unauthorized`. The reply holds exactly `token` and `verifier`; the SHA-256
  of the `web_proof` that `hello` returns equals the verifier, and the proof
  is neither the token nor the verifier. Ten codes in a row leave the eight
  newest valid.
- A signed-in web session: `web_open`, `web_close` and `load_team` (with a
  valid team) are `forbidden`; a second `hello` is `forbidden`; the snapshot
  holds no token, verifier or proof.
- The web session token and the proof are `unauthorized` on the Unix socket.
  An agent on the Unix socket gets `forbidden` for `web_open`, `web_close`,
  `web_pair` and `web_prefs`. `crew web` and `crew web --stop` refuse to run
  in an agent's environment.
- No `hello`: closed with 1008 after 5.0 s, also when pings are sent all the
  while. 5 KiB before `hello`: 1009. A binary message: 1003.
- Binding the port while the daemon serves, with and without `SO_REUSEPORT`,
  on `127.0.0.1`, `::1`, `0.0.0.0` and `::`: `EADDRINUSE` every time.
- `crew web --stop`: the open connection ends, the port refuses connections,
  `crew status` loses its `web` line. `crew web` again: the same port, a new
  label; the old label, the old session token and the old pairing code are
  refused.
- 34 codes, tokens, verifiers and proofs made during these checks were
  searched for in the 21 files of the state, runtime and workspace `.crew`
  directories (database, write-ahead log, daemon log), and in the event log:
  none found. The events `web.started`, `web.paired`, `web.auth_failed` and
  `web.stopped` carry counts and the address only.

### A real browser (Chromium 145, the same daemon)

- Opening the pairing address: live; the fragment is gone from the address and
  no history entry holds it; `localStorage` has the one key `crew.web.session`
  with exactly `token` and `verifier`; no cookie, no `sessionStorage`, no
  IndexedDB database, no service worker; neither value appears in the DOM, the
  address, `window.name`, the history state or the resource timings.
- Requests: the document, four scripts (the terminal's two among them) and one
  stylesheet, all before the page was live; none afterwards through opening the
  inspector's four tabs and the four views. One WebSocket for state, one per
  terminal. No console error, page error or Content-Security-Policy violation.
- Seven hostile message bodies (an `img` with `onerror`, a `script`, an `svg`
  with `onload`, an `iframe`, `javascript:` and `data:` addresses, a URL with
  an attribute break-out, a Markdown and an OSC 8 `javascript:` link, `style`,
  `base` and `meta refresh`) appear as text in the bus, the log and the message
  panel. Nothing ran, no element was created, no inline handler exists in the
  document; the only links made are `http://example.com/a` and
  `https://example.org/ok`, with `noopener noreferrer`.
- A second tab of the address is live without a code. A fresh browser without
  a code, and with the spent one, shows the signed-out page with what to do
  and stores nothing. A WebSocket from another origin is refused and a
  cross-origin `fetch` is blocked.
- `crew web --stop`: both tabs go read-only at once, try again for 30 s
  (with the server restarted on the port in the meantime: refused, another
  label), then stop for good and make no request; reloading the old address
  answers 421. A daemon shutdown: the tab is stopped at once, its session is
  removed from storage, and it makes no further connection attempt.
- Keys: arrows, `home` and `end` walk the agents with exactly one node in the
  tab order; `enter` opens the inspector with focus inside it and `esc` gives
  focus back to the node. Tab order: status bar, graph, bus, message box
  (finding A2 for the hub).
- Looked at: graph, inspector (overview, terminal, transcript, diff), task
  graph, log, usage, the signed-out, reconnecting and stopped pages. Nothing
  to report beyond A2 and A5.

### Documents

README "Web dashboard" and `crew web --help` say what the program does
(commands, the two addresses, 421 on the numeric addresses, what a restart
does to open tabs, Safari), apart from nit A6.

## Not checked here

Reported done by the first reviewer and not repeated: the Rust workspace
(822 passed, 2 ignored) and clippy, the nine live specs on jj with and
without required sandboxes (pairing, version control animations, operator
actions, terminal, the killed daemon with a squatter on both loopback
addresses and a hostile second document, the prober inside an agent's
sandbox), the 131 mock and perf browser tests, the bundle reproduction, P16,
and its manual drive of pan, zoom, drag, the task graph and the terminal.
Firefox was not run by either reviewer; Chromium is the gate.
