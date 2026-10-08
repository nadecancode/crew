# Design: the web dashboard

Author: `architect` (task `web-design`). Design only: no source file was
changed. `notes/` is ignored by `.gitignore`; this file and `notes/web-spike/`
are tracked by force (`git add -f`).

**Objective (operator, m-a363288aa209).** After the jj integration, a web app
in SolidJS: maximize usability, minimize latency, and make the graphs more
interactive than the TUI can be.

**Scope assumed (orchestrator; the operator may correct it).** The browser
counterpart of the TUI: served locally by crew, live state and operator
actions, with the TUI's views, keys and commands, and the VCS animations of
`notes/jj-design.md` §anim.

**Implementation waits for the jj work** (operator ordering): it starts on the
commit `jj-final-review` ends with.

**Revision 2.** After the review of revision 1 (m-f40b1a7f31f0; the direction
was agreed in m-a97923a7e48c):
- **P1, origin.** The page's origin outlived the server: whoever held the port
  while crew was stopped could leave a page behind that read the next
  server's token. Each server start now has an origin of its own,
  `http://<label>.localhost:<port>/`, on both loopback addresses, never
  reused; tabs do not survive a restart; the server proves itself to the
  page (W5, §security "One origin per server start").
- **P2, snapshot order.** A snapshot on a subscribed connection is a barrier
  in the push stream, given by the server; the page discards pushes while one
  is in flight (W13, §transport "A snapshot is a barrier").
- **P2, keys.** `tab` is the browser's again; the graph is one tab stop with
  arrow keys inside (§ui "Focus and keys").
Both findings were reproduced before they were fixed in the design; the
replays are in the spike.

**Revision 3.** After the review of revision 2 (m-1533ef7585bf; the three
points above are resolved):
- **P2, the server proof.** Revision 2 stored the proof beside the token, so
  a second document of a dead origin could read it and pass it on. The page
  now stores a hash of it (the verifier); the server reveals the preimage;
  the session is read from storage once, at the document's start. Also: the
  page requests all of its code at the start, because whoever holds the port
  of a dead origin answers script requests too; and what a stopped page is,
  and what no page can be protected from, is spelled out (§security "A
  document whose server is gone").
- The sentence that TLS "would protect nothing" is gone: a trusted
  certificate would give the origin an owner; it is not chosen for its cost.
The replayed gap and the two fixes are in the spike (`origin.spec.ts`).

**Approved** by the reviewer through revision 3 (m-f95c7cb48e47, commit
0b2a79e1). Added afterwards, at the reviewer's request (m-8f47734a8c0c), as a
gate for the implementation: the order in which a server stops (§security
"The listener", "Stopping").

**Budget P1 raised during implementation** (web, m-2f19582ef628): 150 KB was
an estimate from the spike, which had none of the inspector, dialogs, DAG,
usage view or hover cards; the whole app of §ui and §graphs measures 191 KB.
P1 is now 220 KB. No feature is cut for it, and P4 (the whole bundle, 600 KB)
is unchanged (§perf "Budgets").

**Budgets P1 and P4 raised for member toolboxes** (designer, m-42430a6280f1;
review finding D2 of cache-reviewer, m-4638ef2212b4). The web revamp had left
P1 at 230,500 bytes and P4 at 600,000 (`notes/web-revamp.md`), and its build
used both to within 3 and 1,996 bytes. Card `toolbox-dashboards` adds each
member's toolbox to the inspector, the team List and the graph
(`notes/toolboxes.md` §Display). That adds 5,452 bytes of dashboard script and
5,545 bytes to the whole bundle. The stylesheet stays within P2 because its
rules are folded into the inspector's existing list and details rules
(33,999 of 34,000 bytes). Measured on that card's build: P1 235,949 bytes,
P2 33,999, P3 332,915, P4 603,549. P1 is now 236,500 bytes and P4 604,100,
each the measured size plus about 0.5 KB. P2 and P3 are unchanged. As above,
P1 is a ceiling against growth nobody decided on, and this feature was
decided.

**Revised by `notes/web-multi.md`** (operator, m-50a884c7d9b1: one server
for several workspaces). It replaces W1: the server becomes a process of its
own, shared by every workspace the operator joined, and the page gains a
workspace switcher. What this document says about the listener, the origin,
pairing, the proof, the frames, the views and the budgets stands, with "the
server" read as that process.

**Verification.** The transport, the security checks, the build and the
performance figures below were tried in a spike against a real daemon (this
branch, protocol 4) running the scripted team of `examples/dry-run.yaml`:
- a Rust relay (`tokio-tungstenite`) that serves a bundle and a WebSocket on
  one loopback port and passes `crew_core::rpc` frames through unchanged;
- a SolidJS prototype (store, radial SVG graph with pan, zoom, drag and
  pulses, a virtual log, an xterm.js terminal over `attach`);
- vitest unit tests and Playwright browser tests, including the measurements
  §perf quotes and the attacks of revisions 2 and 3 with their fixes;
- `snapshot-race.mjs`, which shows the snapshot race on the daemon as it is.

`notes/web-spike/run.sh` replays all of it and prints `PASS` (crew built from
this branch, bun 1.3.9, node 24.19.0, Chromium 145 headless): `relay/` is the
Rust relay, `web/` the prototype with its tests. Findings from those runs are
marked **(observed)**.

The spike is evidence, not the implementation. It differs from the design on
purpose: it is a separate process relaying to the socket (the design puts the
server in the daemon, §transport); it accepts a fixed token from the URL
fragment and takes its host label and its proof secret as arguments (pairing
codes, session tokens and the label made at start, §security, are designed
here and covered by the tests of §tests, not spiked; the proof check is
spiked); and the daemon it talks to has no snapshot barrier yet.

Sections: [§scope](#scope) · [§transport](#transport) · [§security](#security) ·
[§build](#build) · [§ui](#ui) · [§graphs](#graphs) · [§perf](#perf) ·
[§tests](#tests) · [§split](#split)

---

<a id="scope"></a>
## §scope

### What is built

- `crew web`: asks the running daemon to serve the dashboard on a loopback
  port, prints its address and opens it in the browser.
- A server inside the daemon (`crates/crew/src/web/`): static files embedded
  in the `crew` binary, and a WebSocket that speaks the socket protocol of
  `crew_core::rpc`.
- A SolidJS single-page app (`web/`): everything the TUI dashboard shows and
  does, plus what a browser adds: a graph that pans, zooms and drags, hover
  and click-through on every node, link and mark, an interactive task DAG, a
  real terminal, and usage charts.

### Decisions

| # | decision | where |
|---|---|---|
| W1 | The server runs **inside the daemon**, and only after `crew web` asked for it. Not a separate bridge process. | §transport |
| W2 | **WebSocket**, carrying `crew_core::rpc` frames as they are: one WebSocket is one daemon connection. Not SSE. | §transport |
| W3 | **No polling**: every fetch is caused by a push or by something the operator did. The terminal is a stream (`attach`), not a screen read on a timer. | §transport |
| W4 | `PROTOCOL_VERSION` 4 → 5: four calls (`web_open`, `web_close`, `web_pair`, `web_prefs`), `SessionInfo.web`, `HelloInfo.web_proof`, and the barrier. | §transport |
| W5 | Loopback only, both addresses. **Every server start has an origin of its own**, `http://<label>.localhost:<port>/` with a random label that is never reused; the server answers to no other name. Tabs do not survive a restart. No TLS, no remote access. | §security |
| W6 | A **one-time pairing code** in the URL fragment is traded for a **session token** and a **verifier**; the token travels in `hello`, never in a cookie, a URL or a header; the reply carries the proof whose hash the verifier is. | §security |
| W7 | A web session is an operator session without `web_open`, `web_close` and `load_team`. | §security |
| W8 | Sources in `web/`; the built bundle is **committed** in `crates/crew/web-dist/` and embedded by `crates/crew/build.rs`. `cargo install` needs no node. A test fails when the bundle is older than the sources. | §build |
| W9 | SolidJS 1.9, Vite 7, TypeScript, bun. No router, no component kit, no state library. xterm.js is the one large dependency, in a chunk of its own beside the first paint. | §build |
| W10 | Graphs and charts are **hand-written SVG**: no graph, layout or chart library. | §graphs |
| W11 | The animation state machine is a port of `crew_tui::anim::Anims`; parity with the TUI is tested with cases generated from the Rust code. | §graphs |
| W12 | The Rust side publishes **wire fixtures** generated from the real types; the frontend's decoders, its mock server and its tests read them. The sole frontend-generation exception is `web/src/generated/client-policy.ts`, emitted from live shared Rust policy by `crates/crew-client/examples/generate_web_policy.rs` and checked byte-for-byte by Rust. | §tests |
| W13 | On a subscribed connection a **snapshot is a barrier**: no push before its reply is newer than it, every push after its marker follows it. The server guarantees it; the page relies on it. | §transport |

Native-sharing amendment to W12, 2026-10-05: the orchestrator approved one
committed generated production-policy file,
`web/src/generated/client-policy.ts`, emitted by
`crates/crew-client/examples/generate_web_policy.rs` from the typed descriptors
used by the Rust client itself. A Rust freshness test compares exact output;
production imports replace existing policy without changing behavior. This
exception does not move generated wire fixtures into the frontend, add a
runtime dependency, introduce WASM, change CSP, or increase any byte/performance
ceiling. Web still builds without Cargo and `cargo install` without Node. The
separate adoption card follows the reviewed baseline handoff and must rebuild
and review the embedded bundle. See [native-architecture.md](native-architecture.md),
"Reuse, web amendment and acceptance evidence". Authorization:
`m-331755b19fb4`; review conditions: `m-acb7e01564a8`.

### Not built (v1)

- Launching a team from the browser (the launcher, `crew run`): the dashboard
  is for a session that runs. `load_team` is refused on web connections (W7).
- Access from another machine, TLS, accounts. The listener binds the loopback
  addresses and there is no flag to change that.
- One page for several sessions. Each daemon serves its own workspace on its
  own port, as each `crew attach` shows one.
- A phone layout. The page works down to 900 px; below that it scrolls.
- Safari: it does not resolve the dashboard's address (§security).
- Tabs that live through a restart of the server: a restart has a new
  address by design (§security).
- History the daemon does not keep: usage over time is charted from what the
  page saw since it was opened (§graphs).
- Changes to the TUI. It may show `SessionInfo.web` later; nothing here needs
  it.

The first three are assumptions of this design that the operator may overrule;
none of them is built in so deep that adding it later means a redesign.

### Parity with the TUI

Everything in `crates/crew-tui` has a place; "TUI" names where it is today.

| TUI | web | data |
|---|---|---|
| dashboard: agents around the orchestrator (`views/dashboard.rs`, `layout.rs`) | agent graph (§graphs) | `Snapshot.agents`, `agent`, `agent_removed` pushes |
| message pulses and links (`anim.rs`) | the same, on the graph | `message` pushes |
| VCS links and marks (`anim.rs`, jj-design §anim) | the same, on the graph; hover and click-through | `event` pushes with `source: "vcs"` |
| bus log (`render_bus`) | message list, virtual | `Snapshot.messages`, `message` pushes |
| message view (`views/message.rs`) | message panel | store; `message_get` for one not in memory |
| question band and box (`views/question.rs`) | question banner and dialog, suggested answers as buttons | `Snapshot.questions`, `message` pushes; `answer` |
| compose, slash commands (`views/compose.rs`, `slash.rs`) | message box and command palette | `send`, and the calls of §ui |
| inspector (`views/detail.rs`): facts, tasks, messages, problems, terminal, transcript, diff | inspector panel with the same parts as tabs | store; `inbox`, `agent_transcript`, `git_diff`; terminal: `attach` |
| task graph (`views/tasks.rs`) | task DAG (§graphs) | `Snapshot.tasks`, `task` pushes; `task_get` |
| event log (`views/logs.rs`) | log view, virtual, filters | `Snapshot.events`, `event` pushes; `events` for older ones |
| usage strip (`views/usage.rs`) | usage strip and usage view (§graphs) | `SessionInfo.quotas`, `Agent.limit`, `Agent.usage`, `Agent.context` |
| model picker (`views/models.rs`) | model dialog | `models_list`, `agent_set_model` |
| confirmations (kill, restart fresh, stop, restart paused) | the same dialogs | – |
| help overlay (`views/help.rs`) | help overlay, generated from the same key table the handler uses | – |
| notices: provider errors, socket warning, stale daemon | toasts and a status-bar note | `agent.error`, `agent.refused` events; `SessionInfo.socket_warning`, `vcs_note` |
| `q`, `/exit`, `/mouse` | not applicable in a browser (§ui) | – |
| launcher (`launcher.rs`) | not built | – |

---

<a id="transport"></a>
## §transport

### Where the server runs (W1)

Inside the daemon, as tasks on its runtime, started by the first `web_open`
and stopped by `web_close` or with the daemon. Before `crew web` is run there
is no TCP listener at all.

Why not a bridge process (`crew web` staying in the foreground, relaying to
the socket, as the spike's relay does):
- **Lifetime.** The daemon "owns the agents, so closing the dashboard never
  stops them"; a browser tab should stay live for as long as the team runs,
  not for as long as a terminal stays open. A second detached process would
  need its own registration, single-instance lock and cleanup.
- **No more authority either way.** Whatever serves the browser must be able
  to make operator calls: a bridge would hold the operator token. Putting it
  in another process moves the same authority, it does not shrink it.
- **One hop less.** Pushes go from `Daemon::push` to the WebSocket without a
  second serialization.

What the daemon pays for it, and how that is bounded: a listener that code
running as an agent can reach (sandboxed agents keep the host network,
design-b3.md §1). Everything it accepts before authentication is bounded in
size, time and number (§security, "limits"), a panic in a connection's task
ends that task only (the workspace builds with unwinding panics), and nothing
is served but the bundle until `hello` succeeded.

### WebSocket, not SSE (W2)

The browser needs what the TUI has: an ordered, two-way stream where a
response precedes the pushes that follow it. A WebSocket is that, and the
existing protocol goes over it without translation.

SSE with `POST` for calls was rejected:
- `EventSource` cannot set a header, so the credential would have to be a
  cookie or sit in the URL (both excluded in §security);
- a long-lived HTTP/1.1 response per tab counts against the browser's six
  connections per host, shared by all tabs; a seventh tab would hang;
- calls and pushes would arrive on different connections, and the "subscribe
  first, then snapshot" rule that makes the TUI gap-free would need sequence
  numbers on every push to restore;
- typing into a terminal needs ordered input with no per-key request.

`fetch` with a streamed response has the same connection limit and ordering
problem. WebTransport needs HTTP/3 and TLS.

### The bridge is the protocol itself

`crew_core::rpc` says: "The same calls back the MCP tools, `crew ctl` and the
dashboard, so adding another front end never needs a protocol change." The web
keeps that: **one WebSocket is one daemon connection**, with the connection
state the socket server already keeps (`Conn`: who, subscription, attachment).

| socket (today) | WebSocket |
|---|---|
| one JSON object per line, `\n`-terminated | one JSON object per **text message**, no newline |
| client sends `Request` (`{"id":1,"method":"hello","params":{..}}`) | the same object |
| server sends `ServerFrame` (`{"id":1,"result":..}`, `{"id":1,"error":{..}}`, `{"push":{"kind":..,"data":..}}`) | the same object |
| `MAX_FRAME_BYTES` (1 MiB) per request line | 1 MiB per message from the browser |
| peer uid check, then `hello` with the operator token | Host and Origin checks at the upgrade, then `hello` with a web session token (§security) |

Binary messages are not used: a browser that sends one is disconnected (close
code 1003). `pty` data stays base64 inside JSON, as on the socket.

**(observed)** The spike's relay forwards socket lines as text messages and
back without parsing them (it rewrites only the token in `hello`); the
prototype's `hello` → `subscribe` → `snapshot`, its pushes, and a second
WebSocket turned into a terminal with `attach` all worked against the
unmodified daemon. Note for the frontend: a push's payload is under `data`,
and for a struct variant that is an object, so terminal output is
`frame.push.data.data` (the spike got this wrong once).

### In the daemon: `Session`

`daemon/server.rs::handle` today mixes the socket with the protocol. The
protocol part moves into a type both transports use, so that there is one
implementation of request handling:

```rust
// crates/crew/src/daemon/server.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transport { Socket, Web }

pub(crate) struct Conn {
    // … today's fields …
    pub transport: Transport,
}

/// One connection's protocol state, whatever carries its frames.
pub(crate) struct Session { /* daemon, conn */ }

impl Session {
    /// A new connection, and the frames for it (responses and pushes) in the
    /// order they must be written.
    pub fn new(daemon: &Daemon, transport: Transport) -> (Self, mpsc::Receiver<ServerFrame>);
    /// Handle one request as received (one line, one text message): parse,
    /// dispatch, queue the response, start the subscription after the reply.
    /// A `snapshot` on a subscribed connection is a barrier in the push
    /// stream (see "A snapshot is a barrier"). Malformed JSON is answered
    /// with `invalid`, as today. `false`: the writer is gone, stop reading.
    pub async fn request(&mut self, text: &str) -> bool;
    pub fn authenticated(&self) -> bool;
    /// The connection ended: stop its stream task, release its listener slot.
    pub fn close(self);
}
```

- `handle` (socket) keeps the peer-uid check, the line reader with its size
  limit and the writer task, and calls `Session`.
- `web/ws.rs` calls the same `Session` from its message loop; its writer task
  turns each `ServerFrame` into one text message.
- `Daemon::authenticate` gains the transport: on `Socket` nothing changes; on
  `Web` only `Role::Operator` with a live web session token is accepted
  (§security). `dispatch` refuses `web_open`, `web_close` and `load_team` on
  `Transport::Web` with `forbidden`, and accepts `web_pair` only there.
- Backpressure is the socket's: the connection's queue holds 1024 frames; a
  browser that stops reading makes its subscription lag, and the daemon sends
  `resync` when it catches up. No other connection waits for it.

### What the page does on the wire

**The state socket** (one per tab):

```
→ {"id":1,"method":"hello","params":{"role":"operator","token":"<session>","client":"web","protocol":5}}
← {"id":1,"result":{"role":"operator","agent_id":null,"project":"demo","version":"0.1.0","protocol":5,"pid":4242,"web_proof":"<S>"}}
→ {"id":2,"method":"subscribe"}
← {"id":2,"result":{"subscribed":true}}
→ {"id":3,"method":"snapshot","params":{"messages":300,"events":1000}}
← {"id":3,"result":{ …Snapshot… }}
← {"push":{"kind":"agent","data":{ …Agent… }}}
…
```

The page checks `web_proof` against its verifier before it does anything else
with the connection (§security). Subscribing comes **before** the snapshot, as in
`crew-tui/src/net.rs`. What makes the pair gap-free on one connection is the
next section.

Pushes the page handles: `event`, `message`, `agent`, `agent_removed`, `task`,
`session`, `resync`, `shutdown`; on a terminal socket `pty` and `pty_closed`.
Everything else, and any kind it does not know, is ignored (the daemon may add
kinds; `Push::Unknown` is the Rust client's version of this rule).

**Terminal sockets** (one per open terminal, pty backend): `hello`, then
`attach {agent, rows, cols}`; from then on the connection carries `pty` pushes
and takes `attach_input {data}` and `attach_resize {rows, cols}`, which are
not answered. Closing the WebSocket detaches. `attach` replaces the
connection's subscription, which is why the terminal has a socket of its own.

**Calls the page makes.** Result shapes are the Rust types; the fixtures of
§tests are the normative examples.

| method | params | result | when |
|---|---|---|---|
| `web_pair` | `{code}` | `WebPaired` | first visit, before `hello` |
| `hello` | `Hello` | `HelloInfo` | every connection |
| `subscribe` | – | `{subscribed}` | state socket |
| `snapshot` | `{messages: 300, events: 1000}` | `Snapshot` | after `subscribe`; on `resync` |
| `send` | `SendParams` with a client-chosen `id` | `SendResult` | message, broadcast |
| `answer` | `{id, body}` | `SendResult` | question dialog |
| `agent_pause`, `agent_resume`, `agent_restart`, `agent_restart_fresh`, `agent_kill` | `{id}` | `Agent` or null | actions |
| `agent_compact` | `{id}` | `Agent` | action |
| `models_list` | – | `ModelsResult` | model dialog opens |
| `agent_set_model` | `{id, model, runtime}` | `Agent` | model dialog |
| `resume` | `{agents: [], restart}` | `{resumed: [id]}` | usage strip |
| `shutdown` | `{stop_agents: true}` | `{stopping}` | stop the team |
| `agent_focus` | `{id}` | `{ok}` | herdr backend |
| `agent_input` | `{id, text, submit}` | `{ok}` | prompt box; raw keys on herdr |
| `agent_output` | `{id, lines: 300, screen: true, ansi: true}` | `OutputResult` | herdr backend only, see below |
| `agent_transcript` | `{id, limit: 400}` | `TranscriptResult` | transcript tab |
| `inbox` | `{agent, unread_only: false, ack: false, limit: 100}` | `InboxResult` | inspector |
| `events` | `{agent?, limit}` | `[Event]` | "older" in the log |
| `history` | `{agent?, limit}` | `[Message]` | "older" in the message list |
| `message_get` | `{id}` | `Message` | a message not in memory |
| `task_get` | `{id}` | `TaskCard` | task panel |
| `git_diff` | `{agent}` | `GitOutcome` | diff tab |
| `web_prefs` | `{set?}` | `{prefs}` | after `hello`; when a preference changes |
| `attach`, `attach_input`, `attach_resize` | – | – | terminal socket |

Timeouts are the TUI's (`net.rs`): 3 s; 30 s for kill, restart, resume of one
agent and `agent_set_model`; 90 s for `resume` and `shutdown`; 20 s for
`models_list`. A call that times out is reported as failed and its late answer
is ignored; the connection stays.

### A snapshot is a barrier

**The problem (reviewer, m-97031d916788; reproduced).** The TUI subscribes on
one connection and fetches the snapshot on another, and applies the snapshot
before the pushes it buffered meanwhile. On one connection the snapshot's
reply and the pushes share a queue, and two tasks fill it: the request
handler (which captures the state and then queues the reply) and the
subscription's forwarder. A change made after the capture can have its push
queued **before** the reply. A page that applies the push and then replaces
its state with the snapshot has lost that change, and nothing brings it back
until the same agent or task changes again. Replacing by id is not a fence.
**(observed)** `notes/web-spike/snapshot-race.mjs`, against the daemon as it
is: one subscribed connection asking for snapshots while 400 messages are
sent to one agent; 45 of 56 snapshot replies were older than a push that had
arrived before them.

**The guarantee (server).** On a subscribed connection, `Session` handles
`snapshot` as a barrier in that connection's push stream:

1. it takes a **new** receiver on the daemon's push channel: the marker `M`.
   A receiver gets what is sent after it was made;
2. it stops the connection's forwarder **and waits until that task has
   ended** (so nothing of the old stream can be queued later);
3. it captures the snapshot;
4. it queues the reply;
5. it starts a forwarder on the receiver of step 1.

So on the wire:

```
… pushes sent before M …   ← {"id":3,"result":{Snapshot}}   … every push sent after M …
```

- **Before the reply: only pushes older than the snapshot.** Each was queued
  by the old forwarder, which had ended before the capture; a push is sent
  after its change is made, so the change is in the snapshot. Overwriting
  them loses nothing. Pushes the old forwarder had not queued when it was
  stopped are dropped from this position; those sent before `M` are in the
  snapshot (events and messages in its lists), those sent after `M` come
  again after the reply.
- **After the reply: every push sent after `M`, in order, none missing.** A
  change made after the capture is among them. A change made between `M` and
  the capture is in the snapshot *and* comes again as a push.
- A lagging forwarder still sends `resync`; the snapshot the page then asks
  for is a barrier like any other.
- A connection that is not subscribed (the TUI's snapshot connection, `crew
  status`) is answered as today.

```rust
// crates/crew/src/daemon/server.rs
pub(crate) struct Conn {
    // …
    /// A subscription to start once the pending response has been queued:
    /// from now (`subscribe`), or from a receiver taken before a snapshot was
    /// captured. Replaces `subscribe_after_reply`.
    pub subscribe_after_reply: Option<broadcast::Receiver<Push>>,
}
```

**The rule (page).**
1. From the moment it sends `snapshot` until the reply arrives, the page
   **discards** pushes (they are older than the snapshot that is coming).
   This includes the time between `subscribe` and the first snapshot, and it
   includes `resync`: a lag reported before the reply is covered by that
   snapshot. So there is never more than one snapshot in flight on a
   connection; a `resync` after the reply starts the next one.
2. The reply replaces agents, tasks, session, questions and both rings.
3. From then on every push is applied in the order received:
   - `agent`, `task`, `session`: replace by id; `agent_removed`: remove;
   - `event`: dropped when `seq != 0 && seq <= last.seq` (the snapshot's last
     event is the watermark);
   - `message`: dropped when its `id` is among the newest 64.

**Why this converges without versions on agents and tasks.** After the reply
the page replays the daemon's own stream from a point not later than the
capture. The last push for an entity always carries its latest state, so the
page ends equal to the daemon. A push for a change that is already in the
snapshot may show an earlier state of that entity for as long as it takes
the next push to arrive (two changes of one agent between `M` and the
capture: a window of well under a millisecond, and the two pushes are
adjacent in the stream). The TUI has the same property today. A revision
counter on every agent and task, stamped under the model lock, would remove
even that; it would touch every mutation in the daemon and two wire types,
and is not needed for correctness.

### No polling (W3)

The page has no timer that asks the daemon for anything. Each fetch and what
causes it:

| fetch | caused by |
|---|---|
| `snapshot` | a connection opened; a `resync` push |
| `inbox` (inspector) | the inspector opening on an agent; an `agent` push for it whose `inbox_total` or `inbox_unread` changed |
| `agent_transcript` | the transcript tab opening; a refresh key; an `agent` push for it whose `status` changed (a turn ended), at most one request in flight |
| `git_diff` | the diff tab opening; a refresh key; a `vcs.commit` event of that agent |
| `task_get` | the task panel opening; a `task` push for that task |
| `models_list` | the model dialog opening |
| `events`, `history` | "load older" |

The TUI reads the terminal every 2 s (200 ms while typing). The web does not:
- **pty backend**: `attach`. Output is pushed as the process writes it, keys
  go in as they are typed. **(observed)** key to echoed output in the page:
  4.1 ms median, 4.7 ms at most over 30 keys.
- **herdr backend**: there is no attachment (`ProcessManager::attach` fails:
  "focus the agent's pane instead"). The tab shows the last screen with a
  "jump to pane" button (`agent_focus`) and reads the screen once (`agent_output`) when the
  tab opens, on the refresh key, after input the operator sent, and on an
  `agent` push for that agent whose `last_activity_at` changed, with at least
  1 s between two reads. No read happens while nothing changes.

### Reconnect and resync

| what happens | what the page does |
|---|---|
| the state socket closes or errors | status "reconnecting"; every call in flight fails with `closed`; retry after 250 ms, doubling to 5 s, ±20 % jitter; retry at once when the tab becomes visible or the browser reports `online` |
| a connection opens | `hello`; check `web_proof`; `subscribe`; `snapshot`, applied as the barrier rules say; VCS marks and links are rebuilt with `Anims.resume` (jj-design §anim: history starts no pulse) |
| `resync` push | `snapshot` again on the same connection (one in flight at most) |
| `shutdown` push | the page stops (§security "What a stopped page is"): session removed, no retry, ever |
| `hello` is answered without a `web_proof` that hashes to the page's verifier | the same: whoever answers is not this page's server (§security) |
| `hello` answers `unauthorized` | the same, with "signed out: run `crew web`" (the session was dropped to make room for a seventeenth) |
| `hello` answers `protocol` | cannot happen with the embedded bundle (page and daemon are one binary); shown as an error, no retry, **no reload** |
| reconnecting fails for 30 s | the page gives up as for `shutdown`. A server that is alive answers within a retry or two; one that is gone never comes back under this address (a new start has a new name, and answers the old name with `421`), so there is nothing to wait for |
| a terminal socket closes | that terminal shows "disconnected" and re-attaches when the state socket is live again (the attachment starts with a repaint of the screen) |

**Calls are not replayed.** A request may have run before the connection
failed (`client.rs`: "This does not make replaying a mutation safe"). The one
exception is `send`: the page always sets `SendParams.id`, re-sending the same
id is a no-op in the daemon, so a message whose reply was lost is sent again
once after the reconnect. Every other failed action is reported and left to
the operator.

**A restart ends every tab.** Reconnecting is for a connection that dropped
while the server lives (the laptop slept, the tab was frozen). A server that
was restarted (the daemon, or `crew web --stop` and `crew web`) has a new
address and new tokens (§security): `crew web` opens it, and the tabs of the
old address stay stopped. The page never navigates or reloads itself to
follow a server.

### `crew web` and the new calls (W4)

```
crew web [--port N] [--no-open] [--json]     start if needed, print the address, open the browser
crew web --stop                               stop serving; every browser session ends
```

```
$ crew web
web        http://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417/
open       http://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417/#pair=3f9a…e1   (works once, for 2 minutes)
note       opens in Chromium-based browsers and Firefox; Safari cannot resolve this address
```

- Run again while the dashboard is served, it prints the same address with a
  new code (`started: false`): that is how another browser, or a browser that
  lost its session, gets in. `--port` naming another port than the one in use
  answers `conflict` and names `crew web --stop`.
- A daemon without a team (`crew start`) serves too; the page shows that no
  team is loaded.
- `--json` prints `WebInfo` as JSON (tests and scripts read `pair_url`) and
  opens nothing.
- The browser is opened with `xdg-open` (Linux) or `open` (macOS) unless
  `--no-open`, stdout is not a terminal, or the opener is missing; failing to
  open is not an error, the address is printed either way.
- Like every operator command it goes through `Client::operator`, so it is
  refused inside an agent (`CREW_IDENTITY`) and needs the operator token.
- `crew status` prints the `web` line while it serves.

```rust
// crates/crew-core/src/rpc.rs
pub const PROTOCOL_VERSION: u32 = 5;

pub enum Call {
    // …
    /// Operator, on the daemon socket: serve the web dashboard if it is not
    /// served yet, and make a one-time pairing code for one browser.
    WebOpen { #[serde(default)] port: Option<u16> },
    /// Operator, on the daemon socket: stop serving; every web session ends.
    #[serde(deserialize_with = "empty_params")]
    WebClose,
    /// A browser, before `hello`: trade a pairing code for a session token.
    WebPair { code: String },
    /// Operator, either transport: the dashboard's preferences for this
    /// workspace. With `set` they are replaced first. Answers `{"prefs": {…}}`.
    WebPrefs { #[serde(default)] set: Option<Value> },
}

/// What `web_open` answers. `Debug` leaves `pair_url` out.
pub struct WebInfo {
    /// `http://<label>.localhost:<port>/`: this server's origin (§security).
    pub url: String,
    pub port: u16,
    /// `url` with `#pair=<code>`: opens a browser session once.
    pub pair_url: String,
    pub expires_in_secs: u32,
    /// This call started the server (it was not running).
    pub started: bool,
}

/// What `web_pair` answers. `Debug` leaves the token out.
pub struct WebPaired {
    /// What the page presents in `hello`.
    pub token: String,
    /// SHA-256, in hex, of the secret the server shows in every `hello`
    /// reply of this session. Not a secret itself.
    pub verifier: String,
}

pub struct HelloInfo {
    // …
    /// On the web transport: the secret of the session whose token was
    /// presented; its SHA-256 is that session's verifier. Absent on the
    /// socket, and on any `hello` that failed. `Debug` leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_proof: Option<String>,
}

pub struct SessionInfo {
    // …
    /// The dashboard's address while it is served, else empty. Never a code
    /// or a token.
    #[serde(default)]
    pub web: String,
}
```

`web_close` answers `{"stopped": true}` (`false` when nothing was served).
`Call::method()` gains the four names.

**Preferences live in the daemon, not in the browser.** The page's origin is
new at every start (§security), so nothing kept in the browser outlasts a
restart. What should (where the operator dragged the agents, whether
single-key shortcuts are off) is one JSON object per workspace, kept by the
daemon in `crew.db` (`meta` key `web_prefs`): at most 16 KiB, an object, else
`invalid`. The page reads it after `hello`, validates every field it uses
(unknown keys and wrong types are dropped), and writes the whole object back
500 ms after the last change. Two tabs: the last write wins.

**Why the version moves.** The rule in `rpc.rs` is to bump for new enum values
and changed meanings. Without a bump a new `crew web` against a daemon started
before the upgrade would fail with `malformed request: unknown variant`; with
it the operator gets the existing message, "restart the team after upgrading".
The barrier is a changed meaning of `snapshot` on a subscribed connection as
well. The manifest test in `rpc.rs` moves
to `(5, version_5)` with unchanged lists (no new push kind, role or status).

**Events** (through `Daemon::emit`, so they are in the log like any other):

| event_type | level | message |
|---|---|---|
| `web.started` | info | `web dashboard on http://c7f3….localhost:23417/` |
| `web.stopped` | info; warn when the server stopped itself | `web dashboard stopped`; `web dashboard stopped: [::1]:23417 was taken by another process` |
| `web.paired` | info | `a browser was paired (3 sessions)` |
| `web.auth_failed` | warn | `7 refused web logins in the last minute`: at most one event a minute |

No event, log line or push ever contains a code or a token.

---

<a id="security"></a>
## §security

### Who is kept out

The web server gives operator authority over TCP, so it must hold the line
design-b3.md drew for the socket: *no process running for an agent may
authenticate as the operator*.

1. **Code running for an agent on this host.** Sandboxed agents keep the host
   network (design-b3.md §3.2), so they can connect to any loopback port, and
   bind any loopback port that is free. They cannot read the daemon's memory,
   the runtime directory, the operator's home (a tmpfs in the sandbox) or the
   host's `/proc` (own PID namespace).
2. **Other local users.** Loopback TCP has no file permissions: unlike the
   socket, any user of the machine can connect, and bind.
3. **A page of another origin in the operator's browser**: any web site, and
   in particular a page an agent serves on another local port, which the
   operator is likely to open (a dev server the agent just started).
4. **Whoever holds the dashboard's port at another time.** A loopback port
   belongs to nobody: before the server starts and after it stops, 1 or 2 can
   bind it and serve pages to a browser that still has a tab, a bookmark, a
   cache entry or a service worker for that address.
5. **Text and terminal output produced by agents**, rendered in the page.

Out of scope, as in design-b3.md: processes of the operator outside a sandbox
(they can read the operator token file), teams run with `sandbox: off`, the
browser and its extensions, and denial of service by a local process.

### The listener (W5)

- Bound to the loopback addresses only, **both of them, on the same port**:
  `127.0.0.1` and `[::1]`. There is no option for another address. The
  dashboard's host name (below) resolves to both, and a browser tries `::1`
  first: **(observed)** with the relay on `127.0.0.1` alone and another
  program on `[::1]` at the same port, Chromium loaded the other program's
  page for the dashboard's address. A port is used only if both addresses
  could be bound; `EADDRINUSE` on either means the port is taken. A host
  without IPv6 loopback (`EADDRNOTAVAIL` or `EAFNOSUPPORT` for `::1`) is
  served on `127.0.0.1` alone: nobody can bind there what does not exist.
  Should it come to exist later, the server must not leave it to others:
  while it serves on one address, the daemon's housekeeping tick (5 s) tries
  the bind again. Success completes the pair; `EADDRINUSE` means someone else
  answers for the name now, and the server stops itself as `web_close` does,
  with a `web.stopped` warning that says why.
- Port: `23000 + (h mod 1000)`, where `h` is the hash of the state directory
  that already names the socket (`paths::session_hash`, FNV-1a), exposed as
  `crew_core::paths::web_port(state_dir)`. If that port is taken the next 15
  are tried; if all are taken `web_open` fails with `unavailable` and names
  `--port`. `--port N` means exactly `N` or an error. The port is a
  convenience (the same workspace, the same number, below the kernel's
  ephemeral range 32768–60999); nothing in the security of the page rests on
  it, and the address as a whole is new at every start (next section).
- **Never `SO_REUSEPORT`.** With that option on the listener, another process
  of the same user (an agent) can bind the same port and the kernel hands it a
  share of the connections: the operator's browser would send its token to the
  agent. **(observed)** against a listener with the option, a second process
  bound the port and received 11 of 12 connections; against one without it
  (what `tokio::net::TcpListener::bind` makes), binding the port again is
  refused with `EADDRINUSE`, with the option, without it, and on `0.0.0.0`.
  The listeners are made with `TcpListener::bind` and nothing else; a test
  binds the port again with the option set and must be refused.
- **Stopping, in this order.** The two listeners cannot be released in the
  same instant. If one address were free while the other still answered,
  someone could bind the free one, receive a tab's reconnect there (the tab
  sends its token), and present that token to the listener that still
  answers: a live session, and the secret that proves the server. So:
  1. the server is marked stopping: `web_pair` and `hello` on the web
     transport answer `unavailable` (with no `web_proof`), and every code,
     token and secret is dropped;
  2. every web connection is closed, state and terminal sockets alike;
  3. only then are the listeners closed, both.
  This is the path of `web_close`, of the daemon's shutdown (before the
  socket and the registration are withdrawn) and of the self-stop of the
  one-address case. A daemon that is killed releases both addresses with the
  process, and its tokens with its memory. The same in reverse at the start:
  no label, code or session exists before both binds succeeded.
- Plain HTTP. A certificate the browser trusts would give the origin an
  owner: a squatter could not present it, and that would be the stronger
  answer to the problem of the next section. It needs a certificate authority
  of crew's own in the trust store of the operator's browser, on every
  machine, and a private key no agent can read; crew does not ask that of a
  local dashboard. Without a trusted certificate TLS adds a warning and no
  owner, and loopback traffic needs no secrecy (reading it takes privileges
  that already give everything). So the origin is given an owner another
  way.

### One origin per server start

**The problem (reviewer, m-f0921e2e2bcf; reproduced).** A browser keeps state
per origin: `localStorage`, the HTTP cache, service workers, and open
documents that go on running. With an address like `http://127.0.0.1:<port>/`
the origin outlives the server, and the port can be bound by someone else
while crew is stopped. A page that someone served there (to a tab left open
and reloaded, to a bookmark) keeps running in the dashboard's origin after
the port is released; when the next daemon starts on the same port and a new
tab signs in, the old page reads the new token out of the shared storage, or
is handed it by the `storage` event. A service worker installed in between
would take over every later load. "The old token is dead" does not help: it
is the *new* token that leaks. **(observed)** `notes/web-spike/web/e2e/origin.spec.ts`
replays it: with the same origin for two server lifetimes, the page left open
by the squatter receives the second server's token.

A stable port, a random port and a port never used twice all fail the same
way, because an attacker can prepare origins in advance: one visit to an
agent's page can load frames from thousands of loopback ports the agent
serves at that moment, each leaving a service worker or a cached page behind
for whoever binds that port later.

**The rule.** The server's origin is made at its start, cannot be known or
prepared before, and is never used again:

```
http://<label>.localhost:<port>/          e.g. http://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417/
```

- `<label>`: 128 random bits as 32 hex digits, generated **after** both
  listeners are bound, new at every start of the server (`web_open` after
  `web_close`, or a new daemon), kept in memory only. A label is never
  written down and never used twice.
- Names under `.localhost` resolve to the loopback addresses inside the
  browser, without a DNS query (Chromium-based browsers and Firefox;
  RFC 6761 §6.3). **(observed)** Chromium loads the dashboard from
  `http://<label>.localhost:<port>/` and gives it that origin.
- The server answers **only** to its own name: `Host` must be exactly
  `<label>.localhost:<port>`. `127.0.0.1:<port>`, `localhost:<port>` and a
  name with another label get `421`. So no document of this server is ever
  delivered into an origin that outlives it.
- What this gives: while the server runs, its origin is served by it alone
  (it holds the port on both addresses, unshared). Before it started the
  origin did not exist, so nothing could be left in it. After it stops the
  origin is dead: no server of crew will ever answer for that name again, so
  nothing new is ever stored there. Whoever takes the port afterwards can
  serve the dead name (an old tab's requests tell it the label) and read
  what the origin holds: a token of a server that no longer exists.
  **(observed)** with a new label for the second server, the page left open
  by the squatter sees no `storage` event and reads only the first server's
  token.
- The label is not a secret and need not be: it appears in `SessionInfo.web`,
  in `crew status` and in the `web.started` event. Knowing it while the
  server runs gives nothing (the port cannot be shared; requests still need
  a token); the protection is that it is new and not reused.
- **Rules for the page**, which keep the origin's state bounded by the
  server's life:
  - it registers no service worker, and uses no browser storage but
    `localStorage` under the one key `crew.web.session` (the token and the
    verifier, below); preferences are kept by the daemon (`web_prefs`);
  - it reads that key **once, when the document starts**, and from then on
    trusts only the copy in its memory; it does not listen for `storage`
    events and never takes a credential from anywhere but its own pairing;
  - it never reloads or navigates by itself on anything a server says;
  - it fetches all of its code while its server is known to be there, and
    asks the origin for no script, style or document afterwards ("A document
    whose server is gone", below);
  - on `shutdown`, on a failed server proof and on `unauthorized` it removes
    `crew.web.session` and stops; it does not retry.
- **Tabs do not survive a restart.** After the daemon (or the web server) was
  restarted, `crew web` prints a new address and opens it; a tab from before
  shows "the dashboard was stopped: run `crew web`" and stays that way. This
  is the price of the rule, and the operator's one command.
- **Safari** does not resolve names under `.localhost` (WebKit bug 160504,
  open since 2016): the page does not load there. Chromium-based browsers
  and Firefox do. There is no fallback to an address by number: that address
  would be an origin that outlives the server. `crew web` says which browsers
  work when it prints the address.

### A document whose server is gone

The fresh origin settles what a *later* server's pages share with anyone:
nothing. It leaves one situation open. A server can go without a word (the
daemon crashed or was killed), a tab of it is still open and running crew's
own code, and from that moment whoever takes the port answers for the dead
origin. That party can:

- accept the open tab's reconnect and answer it;
- serve its own documents into the dead origin (a second tab, a reload, a
  bookmark), which read and write everything the origin stores;
- answer any request the open tab still makes to its origin, scripts
  included: `script-src 'self'` allows what the origin serves.

What it must not get from this: the open tab showing state it made up and
taking the operator's replies as if the team were there, or its code running
in that tab.

**The server proves itself, with something storage does not hold**
(revision 3; the reviewer's m-b89b12d0fa53). Each session has a secret `S`
(`new_token()`), made at pairing and kept in the daemon's memory.

- `web_pair` answers the token and a **verifier**, `SHA-256(S)` in hex. The
  page stores both.
- Every `hello` on the web transport is answered with `S` itself
  (`HelloInfo.web_proof`). The page hashes it (`crypto.subtle.digest`; a name
  under `.localhost` is a secure context **(observed)**), compares the result
  with the verifier **in its memory**, and forgets `S`. No match, no
  `web_proof`, or an error: the page closes the connection, uses nothing the
  server sent, removes its session and stops for good.
- Why a second document cannot help the squatter. What the origin's storage
  holds is the token (of a server that is gone) and the verifier (a hash).
  `S` was only ever sent by the real server, in reply to a `hello` with a
  live token, to documents of an origin that only it was serving at the
  time; it is in no storage. The squatter would have to invert SHA-256.
- Why rewriting the storage does not help either. The open tab read the
  session when it started, which was while its server was alive and the
  origin held nothing but crew's own documents. It never reads it again.

Revision 2 stored the proof itself beside the token; then a second document
of the dead origin could read it and hand it to the squatter, and the open
tab accepted a forged snapshot. **(observed)** `origin.spec.ts` replays
exactly that: server killed, the port taken on both addresses, a second
document reads the storage and rewrites it, the first tab reconnects. With
the proof stored it shows the squatter's made-up agent as live; with the
verifier stored it ends "stopped" with what it last knew and makes no
further attempt.

**No code after the start.** **(observed)** in the same spec: a tab that
first imports its terminal chunk after the server is gone gets the
squatter's script, and runs it. So the page does not load code "when
needed". Its entry module requests every chunk when the document starts
(`const terminal = import("./term/Terminal")` at the top level, the promise
kept), from the server that has just served `index.html`; opening a terminal
awaits that promise and never calls `import()` again. A chunk whose request
failed stays unavailable in that document. After its start the page makes no
request to its origin except WebSocket upgrades. The chunk is still separate
from the entry script, so it is fetched and compiled beside the first paint,
not before it. With the code requested at the start the spec's second half
passes: the terminal opens from memory and the squatter sees no script
request.

**What a stopped page is.** "Stopped" is final for the document:

| it | does |
|---|---|
| shows | what it last knew, dimmed, under a bar: "The dashboard was stopped at 14:03. Run `crew web` for the new address. This tab cannot come back; reloading it asks whoever holds the port now." |
| allows | reading and scrolling, selecting and copying text, switching views |
| does not allow | sending, answering, any action, typing into a terminal: every control is disabled |
| network | none: no reconnect, no request of any kind |
| storage | its session was removed when it stopped |

While "reconnecting" (at most 30 s) the page looks the same but says so, and
nothing it shows changes until a server has proven itself.

**What this does not cover, and cannot.** A tab that is *reloaded* (by the
operator, or by the browser after it discarded the tab to save memory), or a
bookmark opened, while someone else holds the port, shows that party's page
in the old address: it can imitate the dashboard, with made-up state and a
message box that sends what is typed to it. It has no authority over the
team and no live credential. Nothing in the browser can mark an address as
dead; the defences are that the address of a restarted dashboard is always
a new one that `crew web` opens, and the stopped bar's words. This is listed
under "What remains".

### Pairing code, session token (W6)

```
crew web ──web_open──▶ daemon: makes code C (120 s, once)   prints http://<label>.localhost:P/#pair=C
browser  ──GET /──────▶ bundle (no credential needed, no state in it)
page:      takes C out of the URL (history.replaceState) before anything else
page     ──WebSocket /ws──▶ Host and Origin checked
page     ──web_pair{C}────▶ C is spent; daemon makes token T and secret S, answers T and V = SHA-256(S)
page:      stores T and V (localStorage "crew.web.session"), keeps them in memory
page     ──hello{token: T}─▶ operator session; the reply carries S; the page checks SHA-256(S) = V
```

- **Code.** `crew_core::util::new_token()` (244 random bits). Kept in the
  daemon's memory with its expiry. At most 8 outstanding (a ninth drops the
  oldest). Spent by the first `web_pair` that presents it; a wrong, spent or
  expired code answers `unauthorized` and changes nothing.
- **Session token and secret.** Two values from `new_token()`, memory only:
  not in `crew.db`, the state directory, events, pushes, logs or
  `SessionInfo`. Valid until `web_close` or the daemon exits. Compared with
  `ct_eq`.
- **Sessions.** At most 16. Pairing a seventeenth drops the session with no
  open connection whose last `hello` is oldest; when all sixteen have open
  connections `web_pair` fails with `unavailable`.
- **The page** always pairs when the URL carries a code, even if it already
  holds a token (so a code that was opened is always spent), and adopts the
  new session. Without a code it uses the stored one; without either it
  shows "signed out: run `crew web`".
- **On the web transport** `hello` accepts only `role: operator` with a live
  session token. The operator token is not accepted there, and a web session
  token is not accepted on the socket: each credential works on one transport,
  so ending the web sessions never touches the TUI or the CLI. `role: agent`
  is refused on the web transport whatever the token.

Why a code and not the token in the URL: the URL is printed on a terminal, is
an argument of `xdg-open`, and may be pasted somewhere. A code that works
once, for two minutes, makes what is left in scrollback, a process list or a
chat harmless. Why the fragment: it is never sent to a server and never in a
`Referer`.

**No cookie.** Cookies are scoped by host, not by port: a cookie would be
sent to whatever answers for the same name on another port. The credential is
never ambient: the page presents it explicitly, in `hello`, on a connection it
opened to its own origin.

**Why `localStorage`, not `sessionStorage`.** Every tab of the origin shares
the session, so a second tab on the same address needs no new pairing. The
origin lives exactly as long as the server, so does the token, and nothing
else ever runs in that origin while the token is good. What it costs: the
token is on disk in the browser profile, which the sandbox does not mount.

### What each request must pass

Before any routing:
- the head arrives within 5 s, is at most 8 KiB with at most 48 headers,
  parses as HTTP/1.1 and announces no body (no `Content-Length` other than 0,
  no `Transfer-Encoding`). If not: `400`, and the connection is closed;
- `Host` is exactly `<label>.localhost:<port>`. Otherwise `421`. This is the
  origin rule above, and it is also the defence against DNS rebinding: a
  foreign name that resolves to loopback sends its own `Host`;
- the method is `GET`. Otherwise `405` with `Allow: GET` (so no preflight is
  ever answered and no cross-origin request with side effects exists).

| request | answer |
|---|---|
| `GET /` | `index.html` |
| `GET /<path>` of a file in the bundle | that file |
| `GET /ws` with `Upgrade: websocket` | the checks below, then `101` |
| anything else | `404` |

Files are looked up in the table `build.rs` embedded (§build); no path ever
reaches the file system. Query strings are ignored. Every response closes the
connection (`Connection: close`): a page load is four requests, and the
server needs no keep-alive state.

The upgrade additionally needs:
- `Origin` exactly `http://<label>.localhost:<port>`. Missing or different:
  `403`. WebSockets are not covered by the same-origin policy; this is what
  stops another site's page (or an agent's page on another port, or a page
  in a dead origin of an earlier server) from opening the socket in the
  operator's browser. A program that is not a browser can send any `Origin`:
  against those the token is the defence, and the Origin check is not counted
  on;
- `Sec-WebSocket-Version: 13` and a key; otherwise `400`.

**(observed)** with the relay, started with a label: its own name → 200 on
`127.0.0.1` and on `[::1]`; `Host: 127.0.0.1:<port>`, `localhost:<port>`,
another label and a foreign name → 421; `POST` → 405; upgrade without
`Origin`, from `http://evil.example`, from `http://127.0.0.1:5173` and from
`http://127.0.0.1:<port>` → 403; a `data:` page in the browser could not open
the socket; before `hello` a `snapshot` is answered `unauthorized: say hello
first`, a wrong token `unauthorized`, a wrong protocol number `protocol`.

Headers on every response:

```
X-Content-Type-Options: nosniff
Referrer-Policy: no-referrer
Cross-Origin-Resource-Policy: same-origin
```

and on `index.html`:

```
Cache-Control: no-store
X-Frame-Options: DENY
Cross-Origin-Opener-Policy: same-origin
Content-Security-Policy: default-src 'none'; script-src 'self'; style-src-elem 'self' 'unsafe-inline'; style-src-attr 'none'; img-src 'self' data:; font-src 'self'; connect-src 'self' ws://<label>.localhost:<port>; base-uri 'none'; form-action 'none'; frame-ancestors 'none'
```

Other files: `Cache-Control: public, max-age=31536000, immutable` (their
names carry a content hash, and the origin they are cached for dies with the
server). No `Access-Control-*` header is ever sent.

- `script-src 'self'`: no inline script, no `eval`. Markup that got into the
  page could not run code.
- `frame-ancestors 'none'`: an agent's page cannot frame the dashboard and
  steer clicks. `Cross-Origin-Opener-Policy`: a page that opened the
  dashboard keeps no handle on its window.
- **`style-src-elem 'unsafe-inline'` is there for xterm.js (observed).**
  xterm.js 6.0 creates `<style>` elements; under `style-src 'self'` the
  browser blocks them (a browser test of the spike shows the violations, and
  that the app without the terminal is clean under the strict policy). What
  the relaxation allows is a `<style>` element, which needs an HTML injection
  first; it cannot run script, and with `img-src`/`font-src 'self'` it cannot
  send anything elsewhere. `style-src-attr 'none'` stays: no `style="…"`
  attribute works. Solid's object form (`style={{…}}`) sets properties
  through the CSSOM and is not affected **(observed)**; a string `style` in
  JSX would be blocked, so the frontend does not use one.

### Limits

| what | limit |
|---|---|
| connections not yet authenticated (a head being read, a WebSocket before `hello`) | 32; more are closed at accept |
| WebSocket before `hello` succeeded | 5 s; messages up to 4 KiB; only `ping`, `web_pair` and `hello` do anything (others: `unauthorized`, as on the socket) |
| authenticated WebSockets | 32; a `hello` that would make a 33rd answers `unavailable` and that connection is closed |
| message from the browser | 1 MiB (`MAX_FRAME_BYTES`); larger closes the connection (1009) |
| keep-alive | the server pings every 20 s and closes a connection silent for 40 s |
| failed `web_pair` and `hello` | counted; one `web.auth_failed` event a minute. No lock-out: a lock-out would let an agent shut the operator out, and a 244-bit token is not guessed |

### What a web session may do (W7)

Everything the operator may do on the socket, except:
- `web_open` and `web_close`: making codes and ending sessions stays with
  whoever holds the operator token file;
- `load_team`: it carries a whole team definition with commands to run; the
  browser does not launch teams (§scope).

"No control action without auth" follows from the transport: there is no
state-changing HTTP request, and on a WebSocket nothing but `ping` and
`web_pair` is dispatched before `hello` succeeded.

### Agent text in the page

The page shows strings agents control: ids, roles, activity, messages, task
text, event messages, transcripts, diffs, file names, model names, error
text, terminal output. An injection there would be an agent acting as the
operator through the operator's own browser. Rules for the frontend:

1. Such strings reach the DOM only as text (JSX expressions) or as attribute
   values set by Solid. Never `innerHTML`, `outerHTML`, `insertAdjacentHTML`,
   `document.write`, `eval`, `new Function`, or a string given to
   `setTimeout`. A unit test fails when `web/src` contains one (§tests).
2. No Markdown or HTML rendering of message bodies. Text is shown as written;
   a URL becomes a link only when it parses as `http:` or `https:`, with
   `target="_blank" rel="noopener noreferrer"`.
3. Colour in a captured screen (herdr): SGR sequences become spans with
   classes from a fixed palette; every other escape sequence is dropped.
4. The terminal is xterm.js with the `fit` addon only. No clipboard addon, so
   OSC 52 cannot write the operator's clipboard; title changes are ignored;
   hyperlinks open only as rule 2 says. The daemon keeps stripping terminal
   queries from the attached stream (`strip_queries`), so the browser's
   terminal never answers one into the agent's input.
5. The session is read from and written to `localStorage` in one module, is
   never logged, never put in a URL, and is not in any store a component can
   render.

### What remains

- A local process can occupy the 32 unauthenticated slots and keep new tabs
  from connecting while it does. It gains nothing, sockets already
  authenticated are counted apart, and the TUI and CLI do not use this
  listener.
- **A host whose IPv6 loopback appears while the dashboard is served on
  `127.0.0.1` alone.** Until the next housekeeping tick (5 s) someone else
  can hold `[::1]:<port>` and answer for the name. It takes an administrator
  enabling IPv6 on the loopback interface at that moment; an agent cannot.
- **A dead address, loaded again.** A tab of a server that is gone, reloaded
  by the operator or by the browser (a discarded tab), or a bookmark of it,
  shows whatever the port's holder serves: possibly an imitation of the
  dashboard, with made-up state, taking what is typed into it. It holds no
  credential that works and has no authority over the team. A tab that was
  *not* reloaded is protected ("A document whose server is gone"); a reload
  cannot be. The stopped page says not to reload; `crew web` gives the
  address that is alive.
- **Asking for the address.** A page the operator trusts for the wrong
  reason (an agent's page on another port made to look like crew; a tab from
  an earlier server, reloaded while someone else holds the port) can ask the
  operator to paste the output of `crew web` into it. A pairing code is
  whoever presents it first. `crew web` opens the browser itself; the
  dashboard never asks for an address or a code, and the README and the
  "stopped" screen say: run `crew web`, paste its output nowhere.
- A script injection in the page would have the session. The rules above and
  the policy are the defence; there is no second credential behind them.
- Safari users need another browser (above).

---

<a id="build"></a>
## §build

### Stack (W9)

| piece | choice | note |
|---|---|---|
| UI | SolidJS 1.9 (`solid-js`, `vite-plugin-solid`) | the operator's choice; fine-grained updates are what §perf relies on |
| bundler | Vite 7 (7.3.6 in the spike) | Vite 8 builds the same project to within 2 KB a file **(observed)**; `web` may take it, pinned |
| language | TypeScript, `strict` | `tsc --noEmit` is the type check |
| package manager and script runner | bun 1.3 (`bun.lock` committed, `bun install --frozen-lockfile`) | node 24 runs Vite, vitest and Playwright |
| terminal | `@xterm/xterm` 6 with `@xterm/addon-fit` | a chunk of its own, requested when the document starts, used when a terminal first opens (§security: no code after the start) |
| tests | vitest 5 with jsdom and `@solidjs/testing-library`; `@playwright/test` | §tests |
| not used | a router (four hash routes are parsed by hand), a component kit, a CSS framework, a state library, a graph, layout or chart library (§graphs), web fonts, icon fonts | each would cost more bytes than the code it replaces |

Runtime dependencies are exactly three packages: `solid-js`, `@xterm/xterm`,
`@xterm/addon-fit`. Adding a fourth is a design change (ask `architect`).

### Where things live

```
web/                         the frontend project (owner: web)
  package.json  bun.lock  tsconfig.json  vite.config.ts  vitest.config.ts  playwright.config.ts
  index.html
  src/                       the app (§ui has the module list)
  test/                      vitest
  e2e/                       Playwright
  dev/mock.ts                a mock daemon for development, fed by the fixtures
  scripts/stamp.ts           writes web-dist/manifest.json after a build
crates/crew/web-dist/        the built bundle, committed (written only by web's build)
  index.html
  assets/<name>-<hash>.js|css
  manifest.json
crates/crew/build.rs         embeds web-dist (owner: vcs)
crates/crew/src/web/         the server (owner: vcs)
crates/crew/tests/fixtures/web/   wire fixtures (owner: vcs; §tests)
```

The bundle sits inside the `crew` crate so that everything `cargo install
--path crates/crew` compiles is under that path.

### The bundle is committed (W8)

`cargo install --path crates/crew` must work on a host without node. The
choices were:

| option | verdict |
|---|---|
| commit the built bundle, embed it at compile time | **chosen**: no node, no network, nothing to install beside the binary |
| `build.rs` runs `bun run build` | needs node or bun at `cargo install`; a build script that downloads packages |
| fetch a prebuilt bundle at build or run time | needs the network and somewhere to publish; crew is local-first |
| ship the files next to the binary | `cargo install` installs one file |

What a committed bundle costs, and the answers:
- **It can go stale** (sources changed, bundle not rebuilt). A Rust test
  (below) fails `cargo test` when that happens, without needing node.
- **It grows the repository.** The bundle is about 370 KB in the spike, of
  which 331 KB is xterm.js, which changes only when its version does. Vite is
  configured with `manualChunks` so that vendor code (`solid-js`, xterm) and
  the app are separate files: an ordinary frontend commit rewrites one app
  chunk and `index.html`. Source maps are not committed
  (`build.sourcemap: false`).
- **It must be reproducible**, or two members building the same sources would
  commit different bytes. **(observed)** two builds from the same lock file
  give identical files (`run.sh` checks it).

Rule: **every commit that changes `web/` sources carries the rebuilt bundle**
(`bun run build` takes about half a second). There is never a commit on the
branch whose bundle does not match its sources.

Vite: `build.outDir = "../crates/crew/web-dist"`, `emptyOutDir: true`,
`build.target = "es2022"`, `modulePreload.polyfill = false`,
`assetsInlineLimit: 0` (the policy allows `data:` only for images). File names
keep Vite's content hash, so they can be cached forever and a new
`index.html` always names the new files.

### `manifest.json`

Written by `web/scripts/stamp.ts` as the last step of `bun run build`:

```json
{
  "version": 1,
  "source": "fnv1a64:9f3c0a11d2e4b877",
  "files": [
    { "path": "index.html", "bytes": 412, "hash": "fnv1a64:…" },
    { "path": "assets/app-B7xk2f9a.js", "bytes": 98211, "hash": "fnv1a64:…" }
  ]
}
```

- `hash`: FNV-1a 64 of the file's bytes, as 16 hex digits (the function is
  `crew_core::util::fnv1a64`; in TypeScript a `BigInt` loop). A fingerprint
  against forgetfulness, not a security boundary.
- `files`: every file of the bundle except `manifest.json`, sorted by path.
- `source`: FNV-1a 64 over the inputs of the build, in this order: `index.html`,
  `package.json`, `bun.lock`, `tsconfig.json`, `vite.config.ts`, then every
  file under `src/` sorted by path (bytewise). Each contributes its path
  relative to `web/` (with `/`), a zero byte, its length in decimal, a zero
  byte, and its bytes.

### Embedding (`crates/crew/build.rs`)

- Walks `web-dist/` and writes `$OUT_DIR/web_assets.rs`: one
  `(path, content_type, &[u8])` entry per file with `include_bytes!`, sorted
  by path. `web/assets.rs` includes it and looks a request path up with a
  binary search.
- `cargo:rerun-if-changed=web-dist`.
- Content types by extension: `html`, `js`, `css`, `svg`, `json`, `png`,
  `ico`, `woff2`, `txt`; anything else is `application/octet-stream`.
- **No bundle** (`web-dist/index.html` missing, as on the server's branch
  before the frontend lands): one built-in page is embedded as `index.html`
  that says the dashboard is not part of this build. The server and its tests
  do not depend on the frontend existing.
- No compression. On loopback the transfer is not what costs (§perf), and
  the server stays without a compression dependency.
- No new crate for this: `build.rs` uses `std` only.

### Commands

`web/package.json` scripts (owner: web):

| script | does |
|---|---|
| `bun run dev` | Vite dev server with hot reload; `/ws` is proxied to `dev/mock.ts`, or to a real daemon when `CREW_WEB` is set to the address `crew web --no-open` printed (the proxy sends that name as `Host` and `Origin`; open the dev server with the `#pair=` fragment). For development with a scratch team only: the dev server's own origin (`localhost:5173`) has none of the protections of §security |
| `bun run build` | `tsc --noEmit`, `vite build`, `scripts/stamp.ts` |
| `bun run test` | vitest |
| `bun run e2e` | Playwright against the mock and the built bundle |

`justfile` (owner: qa): `web-build`, `web-test`, `web-e2e` call those in
`web/`; `web-check` rebuilds into a temporary directory and compares it byte
for byte with `crates/crew/web-dist/` (the bundle was built from these
sources with this lock file). `just test` stays `cargo test --workspace` and
needs no node.

### Dependencies added to the Rust workspace

| crate | for |
|---|---|
| `tokio-tungstenite` 0.28, `default-features = false`, `features = ["handshake"]` | the WebSocket protocol (framing, masking, ping, close, the accept key) |
| `futures-util`, `default-features = false`, `features = ["sink"]` | `SinkExt`/`StreamExt` on the WebSocket |
| `httparse` 1 | the request head |
| `sha2` 0.10 | the verifier (`SHA-256` of a session's secret) |

**(observed)** these add 23 packages to the 183 in `Cargo.lock` (22 for the
first three, among them `tungstenite`, `http`, `sha1`, `rand`; `sha2` is one
more, its own dependencies are already there); `tokio` needs no new feature. The
relay built with them is 1.0 MB stripped, all of tokio included.

Why not write the WebSocket by hand, as the sandbox code avoids crates:
frames are parsed before authentication (`web_pair` and `hello` arrive as
frames), by the process that holds every authority. That parser should be the
one that is fuzzed and passes the Autobahn suite, not a new one. Why not
`axum` or `hyper`: the HTTP crew needs is "read one `GET` head, answer one
embedded file or upgrade"; `axum` with only its `ws`, `http1` and `tokio`
features adds 37 packages **(observed)**, for a router and a body pipeline
that have nothing to do here.

The upgrade is done by hand around the library, which is what lets one port
serve both files and the socket: read and check the head, answer `101` with
`tungstenite::handshake::derive_accept_key`, then
`WebSocketStream::from_raw_socket(stream, Role::Server, config)` with
`max_message_size` and `max_frame_size` set. `TCP_NODELAY` is set on every
accepted socket. **(observed)** this is the relay's code path.

---

<a id="ui"></a>
## §ui

### Layout

One page. What the TUI shows as separate full-screen views can be side by
side on a browser window, so the graph never has to leave the screen:

```
┌ status bar: project · objective · elapsed · tasks 4/6 · jj · sandbox · ● live ───────────────┐
├ usage strip (quota bars, paused agents; hidden when there is nothing to show) ───────────────┤
├ question banner (when the team waits for an answer) ─────────────────────────────────────────┤
│                                                     │ side panel                              │
│  main area: one of                                  │  nothing selected: the bus (messages)  │
│    graph   agents, links, marks        #/           │  an agent selected: its inspector      │
│    tasks   the task DAG                #/tasks      │    facts · tasks · messages · problems │
│    logs    the event log               #/logs       │    terminal · transcript · diff        │
│    usage   charts                      #/usage      │  a task, message or event selected:    │
│                                                     │    its panel                           │
├ message box / command line (opens with m, M or /) ───────────────────────────────────────────┤
└ footer: key hints for what has focus ────────────────────────────────────────────────────────┘
```

- The side panel is resizable and can take the whole main area (`f`, as the
  inspector's terminal does in the TUI). Under 1100 px it overlays the main
  area instead of sharing it.
- Routes are in the fragment, parsed by hand: `#/`, `#/tasks`, `#/logs`,
  `#/usage`, and a selection as `?agent=<id>`, `?task=<id>`, `?message=<id>`
  after the route (`#/tasks?task=db`). Back and forward work; an address
  can be opened in another tab for as long as the server runs; the server
  only ever serves `/`. `#pair=<code>` is not a
  route: it is consumed before the router starts (§security).
- Dialogs (compose, question, model, confirm, help) are modal, trap focus,
  close with `esc`, and return focus to where it was.
- A page that is reconnecting or stopped is read-only: what it last knew,
  dimmed, every control disabled, and a bar that says which and what to do
  (§security "What a stopped page is").
- Dark palette from `crates/crew-tui/src/theme.rs` (`Theme::truecolor`), as
  CSS custom properties with the same names: `--text`, `--muted`, `--dim`,
  `--faint`, `--accent`, `--codex`, `--pi`, `--shell`, `--orch`, `--ok`,
  `--err`, `--warn`, `--p2p`, `--gauge`, `--gauge-track`. Runtime glyphs as in
  the TUI (`✱ ❂ π $`). System fonts: the UI stack for labels, the monospace
  stack for ids, logs and the terminal. No font file is shipped.
- `prefers-reduced-motion`: pulses and sweeps do not move; links and labels
  still light up and expire.

### Views

**Graph** (`#/`): §graphs. The default.

**Bus** (side panel when nothing is selected): the message list, newest at
the bottom, virtual (§perf). A row: time, from → to, type, first line. Colour
by kind as in the TUI (orchestrator, peer-to-peer, broadcast). Click or
`enter`: the message panel with the whole text, `reply_to` and the replies as
links. Filter by agent pair (set by clicking a link in the graph).

**Inspector** (an agent selected). Header: glyph, id, role, status, model,
branch, elapsed. Tabs, each remembering its scroll position:
- *overview*: the facts of `views/detail.rs::info_lines` (process, session,
  delivery, context window, usage, limit, fallback, restarts), its tasks, its
  last messages with delivery state (`inbox`), its problems (error-level
  events and `Agent.error`);
- *terminal*: pty backend: xterm.js attached (§transport); herdr: the last
  screen and "jump to pane";
- *transcript*: `agent_transcript`, entries by kind (prompt, reply, tool,
  note), virtual;
- *diff*: `git_diff`, with added and removed lines coloured.

Actions are buttons in the header and the keys below.

**Tasks** (`#/tasks`): the DAG of §graphs. A task selected: its card in the
side panel (`task_get`): description, status, assignee, reviewer, progress
note, result, artifacts (a commit id copies on click), and the tasks it builds
on as links.

**Logs** (`#/logs`): the event log, virtual. Columns: time, level, source,
type, agent, message. Filters: agent (`f` toggles the selected agent, as in
the TUI), level, and a text filter; `vcs` is one click. Follows the tail
while scrolled to the bottom; scrolled up it stays where the reader is.
"Load older" asks `events` with a larger limit (the daemon caps it at 5000).
A `vcs.*` row selected shows the `VcsEvent` fields (commit, change, source,
branch, files).

**Usage** (`#/usage`): §graphs. The strip under the status bar is the TUI's
strip: one row per runtime, one bar per window, reset times, and for a
runtime with paused agents what happened and the two actions (continue now,
restart on the current login).

**Questions.** A question for the operator shows the banner, a `?` on the
asking agent's node, the count in the page title (`(1) crew · demo`), and
opens its dialog by itself once nothing else holds the keyboard (a dialog, a
focused text field, the terminal), exactly as `App::pop_question` decides.
The dialog: who asks (and for whom), the whole text, the suggested answers as
buttons (`1`–`6` pick one), a text field for the operator's own words. `esc`
puts it off; `a` reopens the oldest.

**Model dialog.** `models_list` when it opens (a spinner while pi lists its
own). Tabs per CLI (`1` `2` `3`, ←→), rows per model with tier, price,
summary and why one is unavailable; the current one marked. The same warning
as the TUI when the choice moves the agent to another CLI (a fresh session).

### Focus and keys

**`tab` belongs to the browser.** `tab` and `shift-tab` move focus through
the page in document order and are never taken over outside a dialog (a
dialog keeps focus inside itself, as modal dialogs do). The order is: status
bar actions, usage strip actions, question banner, the main area, the side
panel (its tabs, its action buttons, its content), the message box. Every
action of §ui's table is a button that `tab` reaches and `enter` or `space`
presses, so the page can be operated without any single-key shortcut.

**The graph is one tab stop** (a roving tabindex): exactly one node is in the
tab order, the selected agent, or the first when none is selected. Inside the
graph:

| key | does |
|---|---|
| `→` `↓` / `←` `↑` | next / previous agent, in the TUI's order, wrapping; selects it (the TUI's `tab` / `shift-tab`) |
| `home` / `end` | first / last agent |
| `enter` | opens the inspector and moves focus to the side panel |
| `tab` | leaves the graph for the next region |

`esc` in the side panel closes it and gives focus back to the node it was
opened from. The task DAG is one tab stop in the same way (arrows along and
across layers). Lists (bus, log, transcript) are one tab stop each, with the
arrow keys moving the selected row.

**Single-key shortcuts** are the TUI's keys. They act on the selected agent
whichever region has focus, and are inactive while a text field, a dialog or
the terminal has focus. `/keys off` turns them off (kept in the
preferences, `web_prefs`) for those who type by voice or switch; the command line
(`ctrl-k`) and the buttons remain. The table is one data structure in
`web/src/keys.ts`; the handler and the help overlay both read it.

| key | action | TUI |
|---|---|---|
| arrows, in the graph | next / previous agent | `tab` / `shift-tab`, arrows |
| `enter` | open the selected agent's inspector | same |
| `o` | select the orchestrator | same |
| `m` / `M` | message the selected agent / broadcast | same |
| `/` or `ctrl-k` | command line | `/` |
| `t` / `l` | tasks / logs (again: back to the graph) | same |
| `p` | pause / resume the selected agent | same |
| `r` (`R` in the inspector) | restart the selected agent | same |
| `k` | kill the selected agent (asks first) | same |
| `g` | jump to the agent's herdr pane | same |
| `O` / `s` | model and CLI of the orchestrator / of the selected agent | same |
| `u` / `U` | continue the agents paused at a limit / restart them on the current login (asks first) | same |
| `a` | reopen a question that was put off | same |
| `S` | stop the team (asks first) | same |
| `?` | keys and commands | same |
| `esc` | close the dialog, else the side panel, else clear the status line | same |
| inspector: `i` | prompt box: text typed is submitted to the agent as one prompt | same |
| inspector: `d`, `f` | diff tab; side panel fills the main area | same |
| inspector: click the terminal | keys go to the agent; `ctrl-]` gives the keyboard back | same |
| lists: `j`/`k`, arrows, `PageUp`/`PageDown`, `Home`/`End` | scroll (`k` scrolls in tasks and logs, kills on the graph, as in the TUI) | same |
| logs: `f` | filter to the selected agent | same |
| graph: `+` `-` `0`, drag, wheel | zoom in, out, fit; pan; zoom | new |
| `v` | the usage view | new |
| `q` | nothing: closing the tab leaves the team running | quits the TUI |

Keys that a browser keeps for itself are not rebound (`ctrl-w`, `ctrl-t`,
`ctrl-r`, `F5`); `ctrl-k` is taken only when the command line would open.
While the terminal has focus every key but `ctrl-]` goes to the agent.

### Commands

The command line is the message box opened with `/` typed (as
`input::open_command`). The commands are `slash::DASHBOARD` with the same
names, aliases, arguments, completion with `tab`, the hint line under the
box, and the same rule: a line starting with `/` is never sent as a message.

| command | web |
|---|---|
| `/stop`, `/kill [agent]`, `/restart [agent]`, `/restart-fresh [agent]`, `/pause [agent]`, `/unpause [agent]`, `/compact [agent]`, `/model [agent]`, `/resume`, `/answer`, `/tasks`, `/logs`, `/help` | as the TUI (`input/commands.rs::run_slash`): the agent named, else the one the box was addressed to, else the selected one |
| `/exit`, `/mouse` | answer "not needed in the browser" in the hint line |
| `/usage`, `/graph [reset]`, `/keys on`, `/keys off` | new: the usage view; the graph (`reset` forgets dragged positions); single-key shortcuts |

The fixtures list the TUI's commands (§tests); a unit test fails when the web
neither implements nor explicitly declines one.

### Operator actions

| action | call | confirm | feedback |
|---|---|---|---|
| message, broadcast | `send` (`to` an id or `*`, `type: message`, client `id`) | – | the message appears in the bus through its push; a failure stays in the box with the text kept |
| answer a question | `answer` | – | the question leaves the banner when the operator's `answer` message arrives |
| pause / resume | `agent_pause` / `agent_resume` | – | the node shows "…" until the `agent` push |
| restart | `agent_restart` | – | same |
| restart fresh | `agent_restart_fresh` | yes | same |
| kill | `agent_kill` | yes | same |
| compact | `agent_compact` | – | status line "compaction of X's context queued" |
| model and CLI | `agent_set_model` | the dialog is the confirmation | status line |
| continue paused agents | `resume {restart: false}` | – | status line |
| restart paused agents | `resume {restart: true}` | yes | status line |
| stop the team | `shutdown {stop_agents: true}` | yes | "the team was stopped" when `shutdown` is pushed |
| jump to pane | `agent_focus` | – | herdr only; on pty the terminal tab is offered instead |
| prompt | `agent_input {submit: true}` | – | the terminal shows it |
| type | terminal socket `attach_input`; herdr: `agent_input {submit: false}`, keys that pile up while a call is out go in the next one | – | the terminal shows it |

- A confirmation names what will happen and to whom; focus starts on
  "cancel".
- No optimistic state: the page shows a pending mark on the control and
  changes state only when the daemon says so (the reply or the push). On
  loopback that is milliseconds, and the page never shows what did not happen.
- An error answer is shown where the action was started, with the daemon's
  message as text.
- Labels and messages are the TUI's wording (`net.rs` labels, `App` status
  texts) so that both read the same.

### Frontend modules (owner: web; names may change, the split of concerns should not)

```
src/protocol.ts        wire types mirroring crew_core (rpc, types, vcs); PROTOCOL = 5
src/net/session.ts     pairing, the token in localStorage, the storage event
src/net/conn.ts        one WebSocket: hello, calls with timeouts, reconnect
src/net/state.ts       the state socket: subscribe, snapshot, resync
src/store/…            reducers (applySnapshot, applyPush), rings, selectors
src/anim/anims.ts      port of crew_tui::anim::Anims (§graphs)
src/graph/…            Viewport (pan, zoom), AgentGraph, TaskDag, charts, layouts
src/views/…            Bus, Inspector, Tasks, Logs, Usage, dialogs, Help
src/term/Terminal.tsx  its own chunk; xterm.js on a terminal socket
src/keys.ts            the key table and its dispatcher
src/commands.ts        the slash commands
src/theme.css
```

---

<a id="graphs"></a>
## §graphs

### SVG, written by hand (W10)

What has to be drawn is small: up to about 30 agent nodes (10 members and
their subagents), 48 links (`MAX_LINKS`), 48 marks (`MAX_MARKS`), a task DAG
of up to a few hundred nodes, a handful of bars and lines. At that size:

- **SVG, not canvas.** Every node, link and mark is a DOM element: hover,
  click, focus, tooltips and keyboard order come from the browser; colours
  and states are CSS classes on the theme's variables; Playwright and screen
  readers can find a node by its role and name. Solid changes exactly the
  attributes whose signals changed. A canvas would need its own hit testing,
  text layout and accessibility tree, and pays off above thousands of moving
  elements, which the caps above rule out.
- **No library.** What a library would give is pan and zoom, a layered
  layout, and charts; each is a few hundred lines here, with behaviour the
  design fixes anyway (the TUI's radial topology, jj-design's animation
  timing). Measured, minified **(observed)**:

  | library | bytes | gzip |
  |---|---|---|
  | the whole spike app (store, graph with pan, zoom, drag, pulses, virtual log, connection) | 31.8 KB | 12.9 KB |
  | `d3-zoom` + `d3-selection` + `d3-drag` | 51.7 KB | 17.1 KB |
  | `d3-force` | 12.5 KB | 4.9 KB |
  | `@dagrejs/dagre` | 48.7 KB | 17.1 KB |
  | `elkjs` | 1474 KB | 446 KB |
  | `cytoscape` | 453 KB | 143 KB |
  | `uplot` | 53.2 KB | 23.3 KB |
  | `chart.js` (bar and line) | 151 KB | 53.2 KB |

  Libraries also inject styles or build markup in ways a strict content
  policy refuses (xterm.js did, §security); code written here is known to
  pass.

### `Viewport`: pan and zoom, shared by the agent graph and the task DAG

One component owning a transform `{x, y, k}` applied to a single `<g>`:

| input | effect |
|---|---|
| wheel | zoom about the pointer, `k` in [0.25, 4]; `ctrl`-wheel the same (trackpad pinch) |
| two pointers | pinch zoom about their midpoint |
| drag on the background | pan (pointer capture) |
| double click on the background, `0` | fit the content with a margin |
| `+` / `-` | zoom about the centre |
| arrow keys with the viewport focused | pan |

The math is pure functions (`zoomAt(view, point, factor)`, `fit(bounds,
size)`), unit-tested without a DOM. A drag shorter than 3 px is a click
(the spike's rule).

### Agent graph

**Layout.** The TUI's topology, so that both dashboards read the same: the
orchestrator in the centre, members on an ellipse, the first at the top left
and the rest clockwise in the TUI's order (`App::sort_agents`: natural id
order, each member followed by its subagents), subagents as satellites beside
their parent with a tether. A team without an orchestrator agent has an empty
hub that still receives operator and integration traffic, as in the TUI.
Positions are computed from the list of agents and the viewport size by a
pure function (`layout/radial.ts`).

**Drag.** A node can be dragged anywhere; the position is kept in the
dashboard's preferences (`web_prefs`, `layout: {id: {x, y}}`) and wins over
the computed one. "Reset layout" (`/graph reset`, or the button) forgets them.
An agent that joins takes its computed place; one that leaves frees it.

**A node** shows what the TUI's box shows (`widgets/agent_box.rs`): runtime
glyph and colour, id, role, status (a spinner while working), the task's
title, the activity line, progress, the context gauge, model label, and the
badges: unread inbox, `?` (waits for the operator), paused, usage limit,
`temp`, in-session subagents `⑂N`.

**Interaction** (what the TUI cannot do):

| on | hover | click | other |
|---|---|---|---|
| node | card: status and since when, task, model and effort, context %, tokens, last activity, branch | select (the inspector opens in the side panel); double click or `enter`: inspector on its terminal tab | context menu: message, pause/resume, restart, model, compact, kill |
| the task title on a node | the task: status, progress note, what it waits for | that task's card in the side panel (`#/?task=<id>`: the graph stays) | – |
| the model label | – | model dialog (the TUI's "click a model") | – |
| message link | the last three messages between the two, newest first | the bus filtered to that pair | – |
| pulse in flight | the message's first line | that message's panel | – |
| VCS link | the operation: action, outcome, source, commit, files | the log filtered to `vcs` with that event selected | – |
| mark label (`◆ 1a2b3c4d`, `✗ 2 file(s)`, `· status`) | the `VcsEvent` in full | the same event in the log; the commit id copies | – |
| `?` badge | the question's first line | the question dialog | – |
| background | – | clear the selection | pan, zoom |

**Links, pulses, marks: the TUI's animation, unchanged in meaning (W11).**
`src/anim/anims.ts` is a port of `crates/crew-tui/src/anim.rs` as it stands
after the jj work (`Anims`: `on_message`, `on_vcs`, `end_op`, `resume`, `gc`,
`animating`, `running`, `mark_for`, `remove_agent`, `next_expiry`), with the
same constants under the same names:

| constant | value |
|---|---|
| `PULSE_MS` | 900 |
| `LINK_MS` | 3000 |
| `MARK_SWEEP_MS` | 700 |
| `MARK_MS` | 2400 |
| `GLANCE_SWEEP_MS` | 300 |
| `GLANCE_MS` | 900 |
| `RUNNING_MAX_MS` | 600 000 |
| `MAX_LINKS`, `MAX_MARKS` | 48 |

The rules are jj-design §anim and are not restated here: which event starts
which link and mark, held links, "on arrival", which mark a box shows, what a
snapshot restores, the 10 s freshness rule, unknown actions ignored. The
module is plain TypeScript with the clock passed in, so the same cases run in
vitest; the cases themselves are generated from the Rust implementation
(§tests), which makes "the web animates what the TUI animates" a test and not
a reading of two files.

How each piece is drawn:

| piece | TUI | web |
|---|---|---|
| spoke (hub to member) | faint line | `<line>` in `--faint`, always there |
| lit link | coloured cells along `layout::route` | `<path>` from box edge to box edge, in the kind's colour (`--orch`, `--accent` for peer-to-peer, `--p2p` banner rule unchanged); a held link in `--muted`, dashed |
| message pulse | `●` with `•` behind | a dot with a short fading tail, eased as `Link::pulse` |
| VCS pulse | `◆` with `◇` behind | a diamond with the same tail, in the tone's colour (`--ok`, `--err`, `--warn`) |
| done mark | a bright segment once around the border, label on the bottom border | the node's outline redrawn once with `stroke-dashoffset` over `MARK_SWEEP_MS`, and a label chip under the node until `MARK_MS` |
| running mark | spinner and label | spinner and label chip; no sweep |
| glance | a short segment along the bottom border, muted label | a short sweep along the bottom edge over `GLANCE_SWEEP_MS`, muted chip until `GLANCE_MS` |
| compact layout (small terminal) | the label replaces the activity text | not needed: the viewport zooms |

Peer-to-peer links curve around the hub (a quadratic curve whose control
point is pushed away from the centre), so that a link between two members is
never mistaken for two spokes. Integration and operator traffic end at the
hub, as `endpoint()` maps them.

**Frames.** Position along a link is a function of the time, as in the TUI.
A `requestAnimationFrame` loop runs **only while `anims.animating(now)`**,
writing one `now` signal that the moving elements read; when nothing moves
the loop is not scheduled. Expiries (a link going dark, a label leaving) are
one `setTimeout` at `anims.next_expiry(now)`. Spinners are a CSS animation
and cost no script. **(observed)** an idle page requests no animation frame;
with a pulse starting twenty times a second every frame is on time (§perf).
When the tab is hidden the browser stops the frames by itself; on return
`gc(now)` drops what expired meanwhile.

### Task DAG

- **Layout**: layers by longest path from the roots, the function the TUI
  uses (`app::task_layers`), drawn left to right; inside a layer the order
  that minimises crossings by two barycentre sweeps; a pure function of the
  task list (`layout/layers.ts`). Edges are cubic curves from a task's right
  edge to its dependant's left edge.
- **Node**: id, title, status colour (the TUI's), assignee with its glyph,
  progress bar, reviewer when in review, `timeout` when set.
- **Interaction**: hover a task: everything it builds on and everything that
  waits for it stays lit, the rest dims; click: its card in the side panel;
  click the assignee: that agent; `j`/`k` or arrows move the selection along
  the layer, `h`/`l` across layers; `enter` opens the card. Pan and zoom by
  `Viewport`.
- A task that changes state flashes its outline once (the done mark's sweep,
  in the status colour).

### Message flow

Besides the pulses on the graph, the bus list is tied to it: hovering a row
lights the link between its two agents, selecting a link filters the list
(above). That covers "who talks to whom, now".

A time view (lanes per agent, messages as arrows, VCS operations as spans
from `started` to their terminal event, time on the horizontal axis, zoomable
with the same `Viewport`) is a later addition, not part of v1: it needs
nothing from the daemon that the rings do not already hold, so it can be
added without touching the contract.

### Usage

- **Strip** (always visible when there is something to show): the TUI's grid.
  One row per runtime, one bar per window (`session`, `week`, others), percent
  used, time to reset, stale figures muted (older than 20 minutes), and the
  row above a runtime that has paused agents.
- **Usage view** (`#/usage`), all from state the page already has:
  - per agent: context window gauge (`Agent.context`), tokens as one stacked
    bar (input, output, cache read, cache creation), turns, compactions;
    sorted by tokens; click: the agent;
  - per runtime: the windows as in the strip, larger, with the reset as a
    time and a countdown;
  - a line per agent of its context percentage **since the page was opened**:
    the page samples each `agent` push into a ring of 600 points per agent.
    The daemon keeps no usage history, and v1 adds none: after a reload the
    lines start again, and the view says so.
- Bars, gauges and lines are SVG elements with the theme's `--gauge` and
  `--gauge-track`; hover gives the exact figures.

---

<a id="perf"></a>
## §perf

### Where the time goes on loopback

Bytes on the wire cost nothing here: the server is on the same machine. What
the operator waits for is, at load, the browser parsing and running the
script and the first snapshot; afterwards, the next frame. So the budgets are
in **uncompressed bytes** (what has to be parsed) and in **frames**, and the
server does not compress.

### Budgets

Measured in headless Chromium on the development host, production bundle,
the `team(10)` fixture or the scripted dry-run team. "Spike" is what
`notes/web-spike/run.sh` printed over five runs for the prototype (which has the store, the
graph, the virtual log and the terminal, and not yet the dialogs, the DAG or
the charts).

| # | budget | limit | spike **(observed)** |
|---|---|---|---|
| P1 | script needed for the dashboard (everything but the terminal chunk), minified | ≤ 236.5 KB (raised for member toolboxes, above) | 31.8 KB; the whole app: 191 KB (172 KB app, 19 KB solid-js) |
| P2 | stylesheet | ≤ 34 KB | 5.0 KB (3.6 KB of it xterm's) |
| P3 | terminal chunk (xterm.js), requested at the start beside the first paint, used when a terminal first opens | ≤ 350 KB | 331 KB |
| P4 | the whole of `web-dist/` | ≤ 604.1 KB | 369 KB |
| P5 | first contentful paint, cold cache | ≤ 150 ms | 28–40 ms |
| P6 | live (snapshot of 10 agents, 300 messages, 1000 events applied and drawn), cold cache | ≤ 300 ms | 44–55 ms median, 82 ms worst (6 agents; 300 messages and about 1000 events in the last run) |
| P7 | daemon emits → the page's handler runs, p95 | ≤ 10 ms | 1 ms median, 2–4 ms p95 (real pushes, through the relay) |
| P8 | handling one push (reduce and update the DOM), p95 | ≤ 1 ms | 0.2–0.3 ms |
| P9 | a push is on screen: handler → the frame after it is presented, p95 | ≤ 2 frames + 2 ms (35 ms at 60 Hz) | 31–32 ms |
| P10 | frame interval with 10 agents, 5000 events in the log, 200 pushes/s of which 20/s start a pulse, p95; tasks over 50 ms | ≤ 18 ms; none | 16.9–17.2 ms; none |
| P11 | the same during a burst of 2000 pushes/s for 3 s | ≤ 20 ms; none | 17.3–17.4 ms; none (17.5–17.7 ms at 10 000/s) |
| P12 | log rows in the DOM, whatever the log's length | visible rows + 20 | 22 for a 12-row pane |
| P13 | idle page: animation frames requested, timers under 1 s | none | 0 frames in 3 s |
| P14 | terminal: key → echoed output in the page, pty backend, p95 | ≤ 20 ms | 4.2–4.4 ms |
| P15 | JS heap with both rings full | ≤ 60 MB | 7–11 MB (5000 events, few messages) |
| P16 | server: `Daemon::push` → bytes written to the WebSocket, p95 | ≤ 1 ms | not separable in the relay (P7 includes it); the bridge test prints it |

**What P1 is for.** On loopback the size of the script is not what the
operator waits for; P5 and P6 are. P1 is a ceiling against growth nobody
decided on (a library pulled in, code duplicated), about 15 % above the app
as specified. It is not a target to cut features for. If P5 or P6 is missed,
the remedy is to take what the first screen does not draw (inspector,
dialogs, DAG, usage view, hover cards) out of the entry script into a second
chunk that the entry requests when the document starts, as the terminal chunk
is (§security allows no later request); the sum still counts against P1.

P7 and P9 together are the push-to-paint latency: under 45 ms at p95, of
which all but a few milliseconds is waiting for the display's next frame.
Against the TUI the state views gain nothing in latency (it also redraws when
a push arrives); the terminal does: from a read every 2 s (200 ms while
typing) to a stream.

### How the page meets them

1. **A push is applied in the `message` handler, synchronously.** No queue,
   no debounce, no "next tick": the DOM is updated before the browser's next
   frame. Several pushes in one frame cost several small updates and one
   paint.
2. **Agents, tasks and session are a Solid store reconciled by id.** An
   `agent` push that changes `activity` rewrites one text node of one box.
3. **Messages and events are rings outside the reactive graph** (plain
   arrays capped at 1000 messages and 5000 events) with one version signal
   each. A push appends in O(1); only the list that is on screen recomputes,
   and it computes a window.
4. **Lists are virtual**: fixed row height, rows for the visible range plus
   10 above and below, the rest is one spacer's height. The bus, the log and
   the transcript use the same component. Scrolled to the bottom it follows
   the tail; scrolled up, appends do not move what is being read.
5. **Frames only while something moves** (§graphs), and the moving elements
   read one time signal. Animated properties are `transform`, `opacity` and
   `stroke-dashoffset`; nothing animated causes layout.
6. **One viewport transform** for pan and zoom: a drag changes one attribute.
7. **The terminal chunk is not on the first paint's path.** It is a separate
   file that the entry script requests when it starts (§security forbids
   requesting code later); the browser fetches, compiles and evaluates it
   while the dashboard draws, and a terminal is made from it when one first
   opens. **(observed)** with the chunk requested at the start the spike's
   first paint (28–36 ms) and live time (54 ms median) are what they were
   with the chunk loaded on demand.
8. **One second clock** for elapsed times, a single interval that runs only
   while the tab is visible and something on screen shows a duration.
9. **The snapshot asks for 300 messages and 1000 events**: enough for the
   bus and the first screens of the log; older ones on demand.

### How the server meets them

- A push is serialized once and written; `TCP_NODELAY` is set, so a small
  frame is not held back for the next one.
- No compression (no `permessage-deflate`): on loopback it would add work at
  both ends and save nothing.
- The static files are slices of the binary; a request is one table lookup
  and two writes.

### Measuring

`web/e2e/perf.spec.ts` (from the spike's) prints a report with every figure
above and fails when a limit is exceeded. It runs against the built bundle
with the mock (synthetic load through the real reducers) and, in `web-qa`,
against a real daemon with the dry-run team. The member writes the report
into the task result. Limits are for the development host; on a slow CI
machine the timing limits may be scaled by `CREW_PERF_SCALE`, the counts
(P1–P4, P12, P13) never.

---

<a id="tests"></a>
## §tests

Three layers, each runnable on its own: Rust (no node), vitest (no daemon, no
browser), Playwright (a browser; with the mock, or with a real daemon).

### Shared client policy (W12 exception; owner: content)

The narrow client-policy exception shares the current method timeouts and slash
command metadata with `crew-client/src/policy.rs::web_policy()`. The Rust action
runtime and the emitter read the same typed descriptors. Regenerate explicitly:

```sh
cargo run --locked -p crew-client --example generate_web_policy > web/src/generated/client-policy.ts
```

`crates/crew-client/tests/web_policy.rs` rejects stale output. Normal web builds
read the committed TypeScript module and need no Cargo; Rust installation needs
no Node. The exception adds no runtime dependency, CSP permission or WASM.
Connection retry logic, confirmations, reducers, ranking and retention remain
their existing TypeScript implementations. They are not a shared Rust browser
engine. Generated declarations that have no production consumer do not count
as runtime reuse. No other source under `web/` is generated.

### Wire fixtures (W12; owner: vcs)

`crates/crew/tests/web_wire.rs` builds values with the real types and writes
them as JSON under `crates/crew/tests/fixtures/web/`. Run normally, it fails
when a file differs from what the types serialize now; with
`CREW_UPDATE_FIXTURES=1` it rewrites the files. So a change to a wire type
shows up as a diff in a fixture, and the frontend's tests then say whether
its decoders still read it. Timestamps are fixed, so the files are stable.

| file | content |
|---|---|
| `snapshot.json`, `subagents.json`, `team10.json` | `crew_tui::fixture::snapshot`, `with_subagents`, `team(10, …)` as `Snapshot` |
| `pushes.jsonl` | one `ServerFrame` a line: every push kind; a `vcs.*` event for every row of jj-design §events (each `started` with its terminal event, a conflict, an error, both reads); `agent.error` and `agent.refused` events; an operator question and its answer; `pty` and `pty_closed`; one push of a kind that does not exist (must be ignored) |
| `results.json` | by method: an example result of every call in §transport's table (`HelloInfo`, `WebPaired`, `SendResult`, `ModelsResult` from `fixture::models()`, `TranscriptResult`, `InboxResult`, `OutputResult`, `GitOutcome` clean and conflicted, `TaskCard`, lists) |
| `errors.json` | one `ServerFrame` per `RpcError` code, including `protocol` and `unauthorized` |
| `enums.json` | `protocol`, and the value lists of the manifest test (statuses, message types, runtimes, levels, transcript kinds, push kinds) plus VCS actions and outcomes |
| `commands.json` | `crew_tui::slash::DASHBOARD`: name, aliases, args, about |
| `anim.json` | animation cases, below |

**Animation cases.** The generator drives `crew_tui::anim::Anims` with
scripted inputs and records what it holds at chosen times:

```json
{ "name": "merge: started, then ok",
  "workers": ["db", "ledger"],
  "steps": [
    { "at": 0,    "vcs": { "action": "merge", "outcome": "started", "op": "v1", "agent": "ledger", "peer": "db", … } },
    { "at": 4000, "vcs": { "action": "merge", "outcome": "ok",      "op": "v1", "agent": "ledger", "peer": "db", "commit": "5e6f7a8b…", … } } ],
  "checks": [
    { "at": 100,  "links": [{ "from": "db", "to": "ledger", "kind": "vcs", "tone": "ok", "held": true, "pulse": null }],
                  "marks": [{ "agent": "ledger", "kind": "running", "label": "merge db" }], "animating": false, "running": true },
    { "at": 4450, "links": [{ "from": "db", "to": "ledger", "kind": "vcs", "tone": "ok", "held": false, "pulse": 0.5 }], … } ] }
```

One case per test listed in jj-design §anim ("Tests (tui)", `anim.rs`), plus
the message cases (peer-to-peer, orchestrator, broadcast fan-out, operator
traffic entering at the hub). `steps` may also hold `message` (a `Message`),
`snapshot` (events to `resume` from) and `remove` (an agent leaving).
`pulse` is rounded to three decimals. The TypeScript port must produce the
same for every check.

### Rust: the bridge (owner: vcs)

Unit tests beside the code (`crates/crew/src/web/`, `daemon/server.rs`):
- the request head: routing by path, `Host` and method; the upgrade's
  `Origin`, version and key; bodies and oversized or malformed heads refused;
- the asset table: every embedded path answers with its type; unknown paths
  and anything with `..` or `%` do not;
- codes and sessions with an injected clock: a code works once; it expires
  after 120 s; the ninth outstanding code drops the oldest; the seventeenth
  session drops the idle one with the oldest `hello`, and is refused when all
  have connections; `web_close` clears both; two starts never give the same
  label;
- the one-address case, with the bind injected: serving on `127.0.0.1` alone,
  a later successful bind of `[::1]` is kept; `EADDRINUSE` there stops the
  server and revokes every session;
- stopping, with a hook between the steps: after step 1 a `hello` with a
  token that was live a moment ago is `unavailable` and carries no
  `web_proof`, on a connection already open and on a new one, on both
  addresses; the listeners are still bound at that point and are closed
  last; the same order on daemon shutdown and on the self-stop;
- `Session`: the socket's existing behaviours hold through the refactor
  (malformed request answered with the salvaged id, `attach_input` not
  answered, the subscription starts after the reply);
- lag: a subscriber that does not read while more pushes are emitted than the
  channel holds receives `resync`;
- **the barrier, adversarially** (`snapshot_is_a_barrier`). A task changes one
  agent's `activity` to `n=1`, `n=2`, … as fast as it can (through
  `Daemon::update_agent`, so every change pushes) while a subscribed
  `Session` asks for snapshots in a loop. A test hook
  (`#[cfg(test)] Conn::after_capture`) sleeps between the capture and the
  queueing of the reply, which is the window the bug lived in. The frames of
  the connection are read in order and checked:
  1. no `agent` push between two snapshot replies carries an `n` greater than
     the `n` of the reply that follows it (nothing newer is overwritten);
  2. between two replies the pushes are in increasing `n` (pushes sent after
     the marker may come once before the reply and again after it, never out
     of order within either part), and the last push of the run carries the
     final `n`;
  3. a model that applies the frames by the page's rule (discard while a
     snapshot is in flight, replace on the reply, apply after) holds the
     final `n` at the end, in every one of 200 runs;
  4. the same with the forwarder forced to lag (`resync` in the stream).
  Without the barrier check 1 fails within the first run (the spike's probe:
  45 of 56 replies).

Integration tests (`crates/crew/tests/web.rs`), a real daemon with scripted
agents through the existing harness, a WebSocket client
(`tokio-tungstenite` with `connect` as a dev-dependency) and plain TCP. The
client connects to `127.0.0.1:<port>` and sends the `Host` and `Origin` of
the address `crew web --json` printed:

| test | asserts |
|---|---|
| `off_until_asked` | nothing listens on the workspace's port before `web_open`; after it `SessionInfo.web` is the address and `web.started` is logged; after `web_close` open sockets are closed, the port is free on both addresses, `SessionInfo.web` is empty |
| `owns_both_loopback_addresses` | while serving, the page is answered on `127.0.0.1` and on `[::1]`; binding `[::1]:<port>` from the test fails with `EADDRINUSE`; a port whose `[::1]` side is taken by the test beforehand is skipped for the next one (skipped on a host without IPv6 loopback) |
| `port_is_not_shared` | while serving, binding the same port from the test with `SO_REUSEPORT` set fails with `EADDRINUSE` |
| `answers_only_to_its_name` | `Host: 127.0.0.1:<port>`, `localhost:<port>`, another label and a foreign name → 421; upgrade without `Origin`, with a foreign one, with another loopback port, with `http://127.0.0.1:<port>` → 403 |
| `a_new_start_is_a_new_origin` | `web_close` then `web_open`, and stop then start of the daemon: the label differs each time, the port may be the same; a request with the old name → 421; the old token → `unauthorized`; the old pairing code → `unauthorized` |
| `serves_the_bundle` | `GET /` is `index.html` with the policy and headers of §security; a hashed file is `immutable`; `/nope` is 404; `POST`, `OPTIONS`, `HEAD` are 405; no response has an `Access-Control-` header |
| `nothing_before_hello` | `snapshot` before `hello` → `unauthorized`; a wrong token → `unauthorized`; the operator token of the socket → `unauthorized`; `role: agent` with that agent's real token → `unauthorized`; no `hello` within 5 s → closed |
| `pairing_works_once` | `crew web --json` gives a `pair_url`; `web_pair` answers a token and a verifier; `hello` with the token gives an operator session whose `web_proof` hashes (SHA-256, hex) to that verifier; the same code again → `unauthorized` |
| `proof_is_per_session` | two pairings have different verifiers and proofs; `web_pair` never returns the proof; `hello` on the socket has no `web_proof`; a `hello` that fails (wrong token, wrong protocol, wrong role) carries none, so the proof is shown only to a live token |
| `web_token_stays_on_the_web` | a `hello` on the socket with a web session token → `unauthorized` |
| `web_sessions_cannot` | `load_team`, `web_open`, `web_close` on a web session → `forbidden` |
| `prefs_are_kept` | `web_prefs` set on a web session reads back on the socket and after a daemon restart; more than 16 KiB, or a value that is not an object → `invalid`; an agent → `forbidden` |
| `pushes_reach_the_browser` | after `subscribe`: a `crew send` arrives as `message` and `event` pushes; the `subscribe` reply precedes every push; prints the time from `emit` to receipt (P16) and fails above 10 ms median |
| `snapshot_under_load` | the integration form of the barrier test: scripted agents report progress in a tight loop while the client asks for snapshots; checks 1 and 3 of the unit test on the frames received |
| `same_answers_as_the_socket` | `snapshot`, `tasks_list`, `history`, `events` give the same result on both transports |
| `reconnect_sees_what_it_missed` | close the socket, act, connect again with the same token: the snapshot has it |
| `terminal_over_the_web` | a second connection, `attach`: a `pty` push with the screen; `attach_input` is echoed back in `pty` pushes; closing detaches |
| `limits` | a message over 1 MiB closes with 1009; one over 4 KiB before `hello` closes; the 33rd unauthenticated connection is closed at accept |
| `stop_tells_the_browser` | `crew stop`: a `shutdown` push, then the socket closes and the port is free |
| `no_secret_is_written` | after pairing and a session: neither code, token nor secret appears in `crew logs --json`, the daemon's log file, `crew.db` or `daemon.json` |
| `web_needs_the_operator` | `crew web` with `CREW_IDENTITY` set is refused with the existing agent-context error |

`crates/crew/tests/web_bundle.rs` (no node needed): when
`crates/crew/web-dist/manifest.json` exists, every listed file exists with
its size and hash, nothing unlisted is in the directory, `index.html` names
only listed files, the total is within P4, and `source` equals the
fingerprint recomputed over `web/` (§build). Without a manifest it checks
that the built-in page is what `/` serves.

`rpc.rs`: the manifest test at version 5; `web_open`, `web_close`,
`web_pair` round-trip; `WebInfo`, `WebPaired` and `HelloInfo` do not print
their secrets with `{:?}`; a `SessionInfo` without `web` and a `HelloInfo`
without `web_proof` still read.

### Frontend unit tests (vitest; owner: web)

`vitest` with jsdom and `@solidjs/testing-library`; `include` is
`test/**/*.test.{ts,tsx}` so it leaves the Playwright files alone
**(observed)**: without it vitest tries to run them and fails.

| file | asserts |
|---|---|
| `protocol.test.ts` | every fixture file is read by the decoders; `PROTOCOL` and every enum list equal `enums.json`; the unknown push kind is ignored |
| `store.test.ts` | snapshot then pushes: an event with a known `seq` and a message with a known `id` are dropped; agent order equals the fixture's (the TUI's sort); a removed agent leaves and its inspector closes; a question joins on its message and leaves on the operator's answer; the rings cap |
| `order.test.ts` | the page's half of the barrier. Pushes received while a snapshot is in flight are discarded, `resync` among them, and no second snapshot is asked for; pushes after the reply are applied in order. Property test: for random histories of one agent and random positions of marker, capture and reply that obey the server's guarantee, the page ends in the daemon's final state; for a stream that breaks the guarantee (a newer push before the reply) the test's own naive reducer ends stale, which shows the property test can fail |
| `anims.test.ts` | every case of `anim.json` |
| `conn.test.ts` | with a fake WebSocket: `hello`, proof check, `subscribe`, `snapshot` in that order; nothing is rendered and no call is made before the proof matched; the proof must hash to the verifier: a missing one, a wrong one and **the verifier itself sent back as the proof** are refused; a missing or wrong proof, `shutdown`, `unauthorized` and 30 s of failed reconnects each remove the session and end in the stopped state without another attempt, with every control disabled; `protocol` does not reload; the backoff schedule; a lost `send` is sent once more with the same id and nothing else is replayed; timeouts per call |
| `session.test.ts` | the `#pair=` fragment is removed before any connection; a code is always traded, even with a stored session; token and verifier are stored under `crew.web.session` and nowhere else, the proof nowhere; the key is read once: after the document started, changing or removing it in `localStorage` changes nothing the page does (the reconnect still checks against the first verifier); no `storage` listener and no service worker registration exist in `src/`; `import(` appears only at the top level of the entry module |
| `prefs.test.ts` | preferences are read after `hello`; fields of the wrong type and unknown keys are dropped; a change is written once, 500 ms later; a dragged node keeps its place after a new snapshot |
| `layout.test.ts` | radial: order and no overlap for 1 to 12 agents with subagents; layers: every dependency points left, the fixture's task graph gives the expected layers; `zoomAt` keeps the point under the pointer; `fit` |
| `keys.test.ts` | every key of §ui does its action; none fires while a text field, a dialog or the terminal has focus, nor after `/keys off`; `tab` is never handled outside a dialog; in the graph exactly one node has `tabindex="0"` and the arrows move it; the help overlay lists exactly the table |
| `commands.test.ts` | every command of `commands.json` is implemented or declined by name; completion and hints as `slash.rs` |
| `virtual.test.tsx` | 5000 rows put fewer than 80 in the DOM; it follows the tail at the bottom and stays put when scrolled up |
| `text.test.ts` | only `http:` and `https:` become links (`javascript:`, `data:`, `file:` do not); SGR becomes spans, other escape sequences vanish |
| `no-html-sinks.test.ts` | no file under `src/` contains `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write`, `eval(`, `new Function`, or a string `style=` in JSX |
| `budget.test.ts` | P1–P4 from `web-dist/manifest.json`; runtime dependencies in `package.json` are exactly the three of §build |

### Browser tests (Playwright)

Playwright Test is the runner: assertions that wait, a fresh profile per
test, traces on failure. `agent-browser` is what a member uses to look at the
page while working; it is not the suite.

**(observed)** On this host Playwright's downloaded Chromium does not start
(exit 127, NixOS); with `launchOptions.executablePath` taken from
`PLAYWRIGHT_EXECUTABLE_PATH` (set here, a Chromium-based browser from the Nix
store) the spike's sixteen tests pass. `playwright.config.ts` does that, and
defines two projects:

- **`mock`** (owner: web; `web/e2e/*.spec.ts`): the built bundle served by
  `dev/mock.ts` under a name of the form `<label>.localhost`, with the
  headers of §security; it replays the fixtures and records calls. No daemon.

  | spec | asserts |
  |---|---|
  | `graph.spec.ts` | ten agents drawn; wheel zooms about the pointer; dragging the background pans; dragging a node moves it, and a reload keeps it (the mock stores `web_prefs`); hover shows the card; click selects; the arrows walk the agents in order; `enter` opens the inspector |
  | `keyboard.spec.ts` | with the keyboard alone: `tab` from the status bar reaches the graph (one stop), the side panel's tabs and action buttons, the message box, and `shift-tab` goes back the same way; focus is never stuck in the graph; every action of §ui's table can be reached and pressed; `esc` in the side panel returns focus to the node; a dialog keeps focus inside and gives it back; after `/keys off` a `k` does nothing and the kill button still works |
  | `anim.spec.ts` | a `vcs.merge` `started` push shows the running label and the held link; its terminal event the pulse, then the done label, which is gone after `MARK_MS`; a read shows the glance; a message push shows the pulse; with reduced motion nothing moves and the labels still show |
  | `actions.spec.ts` | each row of §ui's action table sends its call with its parameters; the four confirmations ask first and `esc` sends nothing; an error answer is shown as text |
  | `question.spec.ts` | a question push shows banner, badge and title count and opens the dialog; a number picks a suggested answer; `esc` puts it off, `a` reopens |
  | `lists.spec.ts` | bus and log follow the tail, stay when scrolled up, filter; "load older" asks `events` |
  | `tasks.spec.ts` | the DAG's layers; hover dims what is unrelated; click opens the card |
  | `auth.spec.ts` | the fragment is gone from the address after load; no cookie exists; no service worker is registered; without a session the signed-out screen; a second tab of the same address is live without pairing; **after the document started the page makes no HTTP request at all** through every flow of the other specs, the terminal included (a fixture fails any request that is not the WebSocket upgrade) |
  | `reconnect.spec.ts` | the mock drops the socket: "reconnecting", then live with the new snapshot; `resync`; a push the mock sends between the page's `snapshot` and its reply is not shown, one after it is; `shutdown` ends without retry and empties the storage; a mock that answers `hello` with a wrong proof, or with the verifier as the proof, is refused and nothing it sends is drawn; the stopped page keeps what it showed, disables every control and makes no further request |
  | `policy.spec.ts` | no Content-Security-Policy violation is logged in any of the above (a fixture collects them, as the spike does); an agent named `<img src=x onerror=…>` and a message with markup are shown as text |
  | `perf.spec.ts` | the report and limits of §perf |

- **`live`** (owner: qa; `web/e2e/live/*.spec.ts`, run by `scripts/web-e2e`):
  a real daemon, a scripted team that commits, cherry-picks, merges,
  conflicts and asks the operator a question, `crew web --json` for the
  address.

  | spec | asserts |
  |---|---|
  | `open.spec.ts` | pairing through the printed address; the page goes live; agents, tasks and messages change as the script runs; the same address opened again is refused (the code is spent) and the stored session still works |
  | `vcs.spec.ts` | on the script's commit, cherry-pick, merge and conflict the marks and links of jj-design §anim appear on the right nodes; under jj and under git |
  | `operate.spec.ts` | message an agent (it reacts), answer the question, pause and resume, kill; the daemon's events confirm each |
  | `terminal.spec.ts` | the terminal shows the agent's screen; typed keys are echoed; P14 |
  | `outside.spec.ts` | a page from another origin cannot open the socket; the bare addresses `http://127.0.0.1:<port>/` and `http://localhost:<port>/` show nothing of the dashboard (421); from inside an agent's sandbox (`CREW_TEST_SANDBOX=require`) the port answers but a `hello` with the agent's own token is `unauthorized` |
  | `lifecycle.spec.ts` | **the attacks of revisions 2 and 3, as regressions.** A tab is live, a terminal never opened in it. The daemon is killed (`SIGKILL`: no `shutdown`). The test binds the old port on both addresses and serves, for any `Host`: a page that records `storage` events, reads `localStorage` and reports it to the test; a script for any `.js` path that sets a mark; and a WebSocket that answers `hello` with whatever proof the test gives it and `snapshot` with a made-up team. (a) **A second tab of the old address is opened first**: it shows the test's page, which reads the token and the verifier out of the storage, hands them to the fake server as its proof, and overwrites the storage with values of its own. (b) Only then is the first tab allowed to reconnect: it refuses the server, draws nothing of the made-up team, ends "stopped" with every control disabled, and makes no further request; opening its terminal tab requests no script and the mark is not set. (c) The test releases the port; the daemon starts again; `crew web --json` gives an address with another label, on the same port; a new tab pairs there and goes live. Then: the page of (a) has seen no `storage` event since, reads no session of the new server, and a `hello` with anything it holds is `unauthorized`; the two origins differ |
  | `perf.spec.ts` | the report against the real daemon (P5–P11, P14) |

  Chromium is the gate. Firefox, when Playwright has one on the host, runs
  the same projects; what the page uses is in every current browser except
  that Safari does not resolve the address (`style-src-elem` and
  `style-src-attr`: Firefox 108).

### Install without node (owner: qa)

`scripts/web-install-check`: `cargo install --path crates/crew --locked
--root <tmp>` with a `PATH` that has neither `node` nor `bun`; then, with
that binary, a daemon, `crew web --json`, and `GET /` returns the committed
`index.html` (its script name is in `manifest.json`). `just web-check`
proves the committed bundle is the one these sources build.

---

<a id="split"></a>
## §split

Three members. No file is edited by two of them.

### Member **vcs** (task `web-server`): the daemon and server side

| file | change |
|---|---|
| `Cargo.toml`, `Cargo.lock`, `crates/crew/Cargo.toml` | `tokio-tungstenite`, `futures-util`, `httparse`; the client feature as a dev-dependency |
| `crates/crew-core/src/rpc.rs` | `WebOpen`, `WebClose`, `WebPair`, `WebInfo`, `WebPaired`, `SessionInfo.web`, `HelloInfo.web_proof`, `PROTOCOL_VERSION = 5`, tests |
| `crates/crew-core/src/paths.rs` | `web_port(state_dir)` |
| `crates/crew/build.rs` (new) | embeds `web-dist/` |
| `crates/crew/src/web/` (new): `mod.rs` (start on both loopback addresses, the label, stop, codes, sessions and proofs, limits), `http.rs` (head, routing, headers), `ws.rs` (upgrade, message loop, ping), `assets.rs` (the table) | the server |
| `crates/crew/src/lib.rs` | `mod web` |
| `crates/crew/src/daemon/server.rs` | `Transport`, `Session`, the snapshot barrier; `authenticate` by transport |
| `crates/crew/src/daemon/handlers.rs` | the four calls; what a web session may not call |
| `crates/crew/src/daemon/mod.rs` | the web state in `Inner`; `session_info().web`; the listener stops in `serve`'s shutdown path |
| `crates/crew/src/cli/mod.rs`, `cli/ops/mod.rs`, `cli/ops/web.rs` (new), `cli/ops/session.rs` | `crew web`; the `web` line of `crew status` |
| `crates/crew/tests/web.rs`, `web_wire.rs`, `web_bundle.rs`, `tests/fixtures/web/` (new) | §tests |
| `crates/crew-tui/src/fixture.rs` | only if `SessionInfo` is built there field by field: the new field |

### Member **web** (task `web-ui`): the frontend

| file | change |
|---|---|
| `web/` (new), except `web/e2e/live/` | the app, its unit tests, the mock, the `mock` browser project, the build scripts |
| `crates/crew/web-dist/` (new) | the built bundle, with every commit that changes `web/` sources |
| `.gitignore` | `web/node_modules/`, `web/test-results/`, `web/playwright-report/` |

web reads `crates/crew/tests/fixtures/web/` and never writes there.

### Member **qa** (task `web-qa`): integration, live tests, docs

| file | change |
|---|---|
| `web/e2e/live/` (new), `scripts/web-e2e`, `scripts/web-install-check` (new) | §tests |
| `justfile` | `web-build`, `web-test`, `web-e2e`, `web-check` |
| `shell.nix` | bun and node for those who build the frontend |
| `README.md` | the web dashboard: `crew web`, pairing, what the address is, rebuilding the bundle, "open what `crew web` prints, paste it nowhere" |

### The contract between vcs and web

Pinned by this document; a change goes through `architect`.

| what | where it is fixed | who provides |
|---|---|---|
| frames: `Request`, `ServerFrame`, every type in them, protocol 5 | `crew_core::rpc`, `types`, `vcs`; the fixtures are the examples | vcs |
| one text message per frame, both ways; `/ws`; state and terminal sockets | §transport | vcs |
| `web_pair` (token and verifier), then `hello {role: operator, token, client: "web", protocol: 5}` answered with a `web_proof` whose SHA-256 is the verifier, and with none when `hello` fails; the error codes `unauthorized`, `protocol`, `forbidden`, `unavailable` | §transport, §security | vcs |
| on a subscribed connection a `snapshot` reply is a barrier: nothing before it is newer than it, everything sent after its marker follows it, in order | §transport | vcs |
| the page discards pushes while a snapshot is in flight, keeps one in flight at most, and applies what follows in order | §transport | web |
| which calls a web session has, with parameters and results | §transport's table, `results.json` | vcs |
| the address: `http://<label>.localhost:<port>/`, new at every start, answered on both loopback addresses and under no other name; `#pair=<code>`; `WebInfo` from `crew web --json` | §transport, §security | vcs |
| files served at their path under `/`, `index.html` at `/`, the headers and the policy | §security, §build | vcs |
| limits (sizes, counts, times, ping) | §security | vcs |
| the fixtures and their regeneration | §tests | vcs |
| `crates/crew/web-dist/`: any files, `index.html` at its root, `manifest.json` as specified, names with content hashes | §build | web |
| the page obeys the policy (no inline script, no string styles); keeps the session only under `crew.web.session`; reads it once, at the document's start; registers no service worker and no `storage` listener; never reloads or navigates on a server's word; requests all its code at the start and nothing but WebSocket upgrades afterwards; refuses a server whose proof does not hash to its verifier; once stopped, stays stopped; makes no call outside the table | §security, §transport | web |
| selectors the live tests use: a node is `[data-agent="<id>"]` with role `button` and name `<id>, <status>`; a task `[data-task="<id>"]`; connection state `[data-conn]` on the status bar, one of `live`, `reconnecting`, `signed-out`, `stopped`; a mark's label `[data-mark="<agent>"]`; a lit link `[data-link="<from>:<to>"]` | here | web, for qa |

### Order

1. **vcs, step 0** (first, one commit, announced to web and qa): the
   `crew-core` changes (calls, types, version 5, `web_port`) with the daemon
   answering the new calls as "not implemented yet" (`unavailable`), and
   the fixtures with their generator. From here web has the wire types and
   every example it needs.
2. In parallel:
   - vcs: `Session` refactor with the snapshot barrier and its adversarial
     test (socket behaviour otherwise unchanged, tests green), then the
     server, pairing, `crew web`, the bridge tests.
   - web: `protocol.ts`, store and connection against the fixtures and the
     mock; then graph, animation port, views, keys, terminal; unit and `mock`
     browser tests; budgets.
   - qa: the scripted team for the live tests, scripts, justfile.
3. vcs announces the server commit; web runs the app against it (`bun run
   dev` with `CREW_WEB`), then qa merges both, runs the `live` project, the
   install check and `just web-check`, and writes the README section.

### Risks to watch

- **`Session` refactor.** It touches the path every client uses. It lands
  first and alone, with the socket's existing tests unchanged and green,
  before any web code depends on it.
- **A stale or foreign bundle.** Covered by `web_bundle.rs` (stale against
  sources) and `just web-check` (not what these sources build). A merge
  conflict in `web-dist/` is never resolved by hand: rebuild.
- **xterm.js and the policy.** A new xterm version may need more than
  `<style>` elements; `policy.spec.ts` with the terminal open is the check.
  Never widen `script-src`.
- **Resizing an agent's terminal.** `attach` fits the agent's pty to the
  browser's pane (as `crew agent attach` does to the operator's terminal).
  The pane never asks for less than 80×24 and scrolls instead; two tabs on
  one agent: the last resize wins.
- **Protocol 5 on running teams.** As with every bump: teams started before
  the upgrade must be restarted; the CLI already says so.
- **Port collisions.** Sixteen ports are tried, each on both addresses; a
  machine with that many crew sessions hashing together needs `--port`. The
  error must say so.
- **Names under `.localhost`.** The design rests on the browser resolving
  them to loopback by itself. Chromium-based browsers and Firefox do; Safari
  does not, and a browser behind an unusual proxy setting may not. `crew web`
  says so next to the address, and the README names the browsers. The answer
  to a browser that cannot is another browser, never an address by number.
- **Stopping in the wrong order.** Closing a listener before the sessions
  are revoked leaves an address free while tokens are still good (§security
  "Stopping"). The unit test with the hook between the steps is the guard.
- **The origin rule is easy to undo by accident.** Accepting `127.0.0.1` as
  `Host` "for convenience", persisting the label, adding a `storage`
  listener or a service worker, reloading on a server's word, storing the
  proof instead of its hash, reading the session from storage a second time,
  or an `import()` that runs later than the start each reopens a hole of
  revision 2 or 3. `answers_only_to_its_name`,
  `a_new_start_is_a_new_origin`, `session.test.ts` and `lifecycle.spec.ts`
  are the guards; none of them may be weakened to make something else pass.
- **Frontend dependencies creeping in.** `budget.test.ts` pins the three
  runtime packages and the sizes.
