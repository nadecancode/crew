# Design: one web dashboard for several workspaces

Author: `architect` (task `multi-design`). Design only: no source file was
changed. This file and `notes/multi-spike/` are tracked by force
(`git add -f`), like the rest of `notes/`.

**Request (operator, m-50a884c7d9b1).** "For web server multiple crew
instances should share the same server (but with different workspace that
user can switch between)."

**What this document is.** A revision of `notes/web-design.md` (the design the
dashboard on main, 2c2441e7, implements). Everything there holds unless this
document says otherwise; the decisions it replaces are named where they are
replaced. Section names here carry the prefix `multi-` so that both documents
can be cited side by side.

**Decided with the operator** (asked in m-5119bb45803c, answered in
m-4ca285b69b15): a workspace is shown in the dashboard only after `crew web`
was run for it; `crew web --all` adds every running session at once; the
configuration can make that the rule. The server is a process of its own.

**Revision 2.** After the review of revision 1 (m-014401bad354,
m-083b3108a0dc, `review/web-multi-design.md`; the transport was not objected
to):
- **P1, removal.** Taking a workspace out removed its row and left the rest
  to the page. A membership now has a generation that owns every pending
  open, every connection and the watch; removal revokes it first and answers
  when nothing of it holds the daemon. A rescan under `join = "all"` leaves a
  removed workspace out (M9, §multi-join "Leaving").
- **P2, restarts.** A rescan that skipped a workspace because its old watch
  had not ended yet could lose a restarted daemon for good, and a rescan
  asked for during a scan could be swallowed. A watch belongs to one
  incarnation of a daemon; one task reconciles each membership against the
  registry; a request made during a scan is followed by another scan (M10,
  §multi-server "Reconciling").
- **P2, order.** Listings carry a revision and the page applies only a newer
  one; a summary watch registers and captures in one step (§multi-transport).
- **P2, a connection that failed is not a daemon that stopped.** The page
  takes "not running" from the listing only, and tries again for as long as
  the listing says running (§multi-ui "Two levels").
- **P2, restart and migration.** `crew web --restart` gives the new server
  the old one's memberships, stopped ones included; no server is replaced
  unasked; the step from protocol 5, where there is no shared server to
  replace, is described as what it is (§multi-server "Versions",
  §multi-migration "From protocol 5").
- Two contradictions in wording are gone: which calls no WebSocket has, and
  which `hello` the server answers.

Each fault was replayed in the spike before the rule that replaces it; those
lines of its output say "revision 1, reproduced".

**Revision 3.** After the review of revision 2 (m-da819a0798dc,
`review/web-multi-design-r2.md`; the findings above are closed):
- **P2, an outcome is about its attempt.** Revision 2 made the listing the
  authority and then let a 4002 close, `not_found` and `protocol` set
  `removed` and `incompatible` all the same. Such a report can be about a
  connection older than the newest listing: a workspace taken out and joined
  again within one push interval, a daemon since replaced. No outcome sets a
  state any more: it ends its attempt, the page asks for the listing where
  the outcome says the listing may be stale, and acts on the newest listing
  it then has, an unchanged one included (§multi-ui "Two levels").
- **The forced end of a removal** is spelled out: cancelling a task is a
  request; the removal awaits the cancelled tasks and the generation's count
  before it answers (§multi-join "Leaving", step 4).

Both are replayed in the spike: the page's rules as a model in
`measure.mjs`, in revision 2's version ("revision 2, reproduced") and in
this one; a connection that holds on, and a browser that does not read.

**Verification.** What was uncertain was tried against two real daemons
(built from main, protocol 5) with a prototype of the shared server:
`notes/multi-spike/run.sh` replays it and prints `PASS` (node 24.19.0).
`hub/` is the prototype (Rust, 764 lines: one port, an index connection, one
daemon connection per WebSocket, memberships with generations, watches
reconciled against the registry, listings with revisions), `measure.mjs`
the measurements and the replays, with a model of the page's rules for a
workspace connection (the spike has no page). Findings from those runs are marked
**(observed)**. The spike is evidence, not the implementation: it takes its
label, pairing code and secret as arguments; a rescan joins every running
session it was not told to leave out; its control calls are calls of the
index connection, where the design has a control socket; it has knobs that
bring back revision 1's rules and slow a scan or the end of a watch down so
that a test can get in between. The daemons it talks to speak protocol 5:
they have no summary, and they do not say when they started (the test says
it for them).

Sections: [§multi-scope](#multi-scope) · [§multi-server](#multi-server) ·
[§multi-join](#multi-join) · [§multi-security](#multi-security) ·
[§multi-transport](#multi-transport) · [§multi-ui](#multi-ui) ·
[§multi-migration](#multi-migration) · [§multi-perf](#multi-perf) ·
[§multi-tests](#multi-tests) · [§multi-split](#multi-split)

---

<a id="multi-scope"></a>
## §multi-scope

### What changes for the operator

```
$ cd ~/work/payments && crew web
web        http://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417/#/w/946d4617aa702870/
open       http://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417/#pair=3f9a…e1&w=946d4617aa702870   (works once, for 2 minutes)

$ cd ~/work/search && crew web
web        http://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417/#/w/0b54c1d2e3f4a596/
open       …
```

The second command starts nothing: it adds `search` to the server that is
already running and prints the same address, pointing at the other workspace.
A tab that was open on `payments` shows `search` in its workspace switcher at
that moment; no reload, no new pairing. Stopping and starting the `payments`
team no longer ends the tab: the workspace is shown as stopped and comes back
when its daemon does.

### Decisions

| # | decision | replaces | where |
|---|---|---|---|
| M1 | The dashboard is served by **one process per user, of its own** (`crew web-server`, started by the first `crew web`, detached like the daemon). It belongs to no daemon and outlives each of them. No daemon listens on TCP any more. | W1 (the server inside the daemon) | §multi-server |
| M2 | A workspace is in the dashboard only after the operator **joined** it: `crew web` in it, `crew web --all`, or `[web] join = "all"` in the configuration. It stays joined for the server's life, across restarts of its team; `crew web --remove` takes it out. | "only after `crew web` asked for it", kept per workspace | §multi-join |
| M3 | A browser is **paired once, with the server**; its session reaches every joined workspace. The server reaches a daemon as the operator, with that daemon's token file, read when it connects, and marks the connection as a web session so that the daemon keeps enforcing W7. | W6 moves to the server unchanged; W7 gains what a web session cannot do to the server | §multi-security |
| M4 | **One WebSocket per workspace**, exactly today's: one WebSocket is one daemon connection, frames unchanged, now passed through the server. Beside it **one index connection** per page, to the server itself, for the list of workspaces and their badges. Nothing is multiplexed. | W2 kept; adds the index | §multi-transport |
| M5 | What a badge needs comes from the daemon as a **summary** (a call and a coalesced push). The server passes summaries on; it holds no copy of any workspace's state and interprets no push. | new | §multi-transport |
| M6 | The page has **two levels of state**: the page (its server: live, reconnecting, stopped, with today's rules) and the workspace on screen (live, not running, incompatible). A daemon that stops no longer stops the page. | "on `shutdown` the page stops" now holds for the server's `shutdown` only | §multi-ui |
| M7 | Addresses name the workspace: `#/w/<id>/…`, with `<id>` the 16 hex digits that already name the daemon's socket and its registry entry. | routes of §ui | §multi-ui |
| M8 | `PROTOCOL_VERSION` 5 → 6. | W4 | §multi-migration |
| M9 | A membership has a **generation**. Every pending open, every workspace connection and the watch belong to one. Taking a workspace out revokes its generation first and answers only when nothing of it holds the daemon; joining again is another generation. | new | §multi-join |
| M10 | What the server believes of a workspace is **reconciled against the registry, by daemon incarnation, in one task**. The listing it sends carries a **revision**. The page takes a workspace's state from the listing, never from what happened to a connection. | new | §multi-server, §multi-transport, §multi-ui |

W3 (no polling), W5 (loopback, an origin per server start), W8–W13 are
unchanged; "the server" in W5 now means the shared one.

### Not built

- Workspaces of other users or other machines; a server shared between users.
- Acting on several workspaces at once (a message to agents of two teams, a
  combined log). The switcher shows counts for the others, nothing more.
- Adding or removing a workspace from the page: that stays with the command
  line (§multi-security "What a web session may not do").
- Starting a team from the page (as before, W7).
- A second state connection kept warm for the workspace just left: a switch
  takes a new snapshot, which costs about 12 ms on the wire (§multi-perf).

---

<a id="multi-server"></a>
## §multi-server

### Why a process of its own (M1)

W1 put the server in the daemon and gave three reasons against a separate
process. The request changes the first one and leaves the others where they
were:

- **Lifetime.** W1: "a browser tab should stay live for as long as the team
  runs". With several teams there is no single daemon whose life is the
  right one. Hosted by the first daemon, the server would die with that
  team and take every other workspace's tabs with it (the origin is new at
  every server start, W5, so they could not come back); handing the listeners
  and the sessions from one daemon to another does not survive a crash and
  would be the most delicate code in the program. A process that belongs to
  no team has exactly the life that is wanted, and it gives back something
  the single-workspace design had to give up: **a tab survives a team's
  restart**.
- **Authority.** Unchanged: whatever serves the browser can act as the
  operator. The server reads the operator token of each joined daemon, as
  the CLI does. It is the operator's process, started by the operator's
  command, in no agent's environment.
- **One hop more.** Pushes now cross a Unix socket and the server before the
  WebSocket. Measured (**observed**, four runs, 300 to 600 pushes each): from
  a `send` on the daemon's socket to the `message` push at a subscriber,
  431–557 µs at the median through the daemon's own server and 438–572 µs
  through the prototype; the difference per run was −62, −23, +15 and
  +25 µs, which is the noise of the measurement. §perf is unaffected.

What the separate process costs is what W1 said it would: its own
registration, single-instance lock and cleanup. They are specified below and
mirror the daemon's. It also removes something: the daemon no longer parses
HTTP or WebSocket input from the network at all, so a fault in that code is
no longer in the process that owns the agents and the database.

### The process

`crew web-server [--port N]` is a hidden subcommand of the same binary (the
bundle is embedded in it, W8), run in the foreground by tests and detached by
`crew web`, with the daemon's way of detaching and the same environment
scrubbing (`cli/daemonize.rs`, `SESSION_MARKERS`). It refuses to start in an
agent's environment (`refuse_in_agent_context`).

Its files are in the runtime directory (`paths::runtime_dir()`, where the
daemons' sockets are; mode 0700, and hidden from sandboxed agents, which see
only their own daemon's socket and their own sidecar directory there):

| file | what |
|---|---|
| `web.lock` | the single-instance lock (`flock`), held for the process's life |
| `web.sock` | the control socket, mode 0600 |
| `web.token` | the token of the control socket, mode 0600, written before the socket is bound: `paths::operator_token_path("web.sock")`, read with `read_operator_token` and its checks |
| `web.json` | `{pid, pid_start, boot_id, port, url, protocol, version, build}`, for `crew web` and for telling a stale file from a live server |

Its log is `web.log` in the registry's parent (`$XDG_STATE_HOME/crew/`),
written as `daemon.log` is. No code, token, secret or verifier is ever
written to it (as before).

**The port** is per user, not per workspace: `23000 + fnv1a64(runtime_dir)
% 1000`, sixteen tries upwards, or exactly `--port`. Two users of a machine,
and a test with a runtime directory of its own, get servers of their own.
`paths::web_port` takes the runtime directory instead of the state directory.

**The listeners, the origin, pairing, sessions, the proof, the limits, the
stop order**: `crates/crew/src/web/` as it is on main, moved out of the
daemon into this process. Nothing in §security "The listener" to "Limits"
changes, except the two numbers in §multi-security "Limits".

### The control socket

It speaks `crew_core::rpc` frames, one JSON object per line, like a daemon's
socket, so `Client::operator(web.sock)` works on it as it is: `hello` with
the role `operator` and the token of `web.token`. It serves:

| call | does |
|---|---|
| `ping`, `hello` | as on a daemon |
| `web_open {workspace?, all?, restore?, port?}` | joins (§multi-join), makes a pairing code, answers `WebInfo` |
| `web_close {workspace?}` | with `workspace`: takes it out of the dashboard (§multi-join "Leaving"); without: stops the server |
| `workspaces` | the listing of §multi-join (for `crew web --list` and tests) |
| `web_rescan` | asks for a rescan (a daemon says it started) |

Everything else answers `invalid`. No browser reaches this socket.
`web_open`, `web_close` and `web_rescan` are calls of this socket only: no
WebSocket has them, the index connection no more than a workspace's.
(`ping`, `hello` and `workspaces` exist on the index connection as well,
§multi-transport.)

### Its life

**Start.** `crew web` (after `refuse_in_agent_context`, and after it found
the daemon it was run for, as today): connect to `web.sock`; if nobody
answers, take the stale files away when `web.lock` is free, spawn `crew
web-server`, wait up to 5 s for `ping`. Then `web_open`. Two `crew web` at
the same moment: the lock decides, the loser connects to the winner. A
server whose first `web_open` leaves its table empty stops at once instead
of lingering.

**While it runs** it holds, for each joined workspace whose daemon runs, one
connection to that daemon (the *watch*, §multi-transport). Nothing is read
on a timer (W3 applies to the server too), and no timer recurs faster than
the 5 s housekeeping tick except the WebSocket pings; the two coalescing
delays of §multi-transport and the retries named below are armed only while
something is wrong or waiting to be sent. It learns that a daemon is gone
from the end of its watch, and that a daemon started from that daemon. What
either means is decided by reconciling.

### Reconciling (M10)

What the server believes of a joined workspace is a state (`running`,
`stopped`, `incompatible`, `unreachable`) and a watch or none. It is
changed in one place, by one task, one event after another, and always by
comparing it with what the registry publishes at that moment. `stopped`
means that the registry publishes no live daemon for the workspace; nothing
sets it because a connection ended or could not be made.

- **An incarnation** of a daemon is `(pid, pid_start, started_at)` of its
  registry entry (`DaemonInfo`). A restarted daemon is another incarnation
  of the same workspace id.
- **A watch belongs to one incarnation**, and has a number of its own (its
  *epoch*, from a counter of the server).
- **Reconciling a workspace** reads its registry entry, then:

  | the registry | the watch | the server |
  |---|---|---|
  | a live entry | on that incarnation | nothing |
  | a live entry | none, or on another incarnation | that watch is *superseded*: dropped, and whatever it still reports is ignored. Dials the published daemon (§multi-join, step 4). Answered: the watch is on the new incarnation, the state `running`. Answered `protocol` or `unauthorized`: `incompatible`, `unreachable`. Not answered (no connection, or nothing within 2 s: the daemon is going, or coming, or hangs): `unreachable`, and reconciled again after 250 ms, doubling to 5 s, for as long as the registry still publishes a live entry |
  | no live entry | any | no watch, `stopped` |

- **The end of a watch** is an event that carries the watch's epoch. If that
  is not the workspace's current watch, the event is ignored: a superseded
  watch ending late is not news. If it is, the watch is cleared and the
  workspace is reconciled; the registry says whether the daemon is gone (no
  entry), was restarted (another incarnation: dialled), or only dropped the
  connection (the same incarnation: dialled again).
- **A rescan** reconciles every joined workspace against one reading of the
  registry, and under `join = "all"` first joins what runs and was not taken
  out. It is asked for by `web_rescan`, by every `web_open`, and by a page's
  `workspaces` call (at most one a second). A request sets a flag. The task
  clears the flag *before* it reads the registry, and scans again if the
  flag is set when a scan ends. So every request is followed by a reading of
  the registry that began after the request was made; requests are folded
  together, and none is lost to a scan that was already under way.
  `web_rescan` answers once the flag is set.
- **A workspace connection whose dial fails, or whose daemon's side ends,**
  asks for that workspace to be reconciled. The server then checks its own
  picture against the registry, instead of leaving a page to guess what a
  closed connection means (§multi-ui).

The retry in the table exists only while the registry and the socket
disagree: a daemon that exits withdraws its entry, and one that died fails
`is_live` (its pid, its start time), and either way the next reconcile says
`stopped` and arms nothing. An idle server with healthy watches arms no
timer for any of this.

**A daemon stops** (orderly, killed, crashed): its watch ends, the reconcile
finds no live entry, the workspace is `stopped` with its last summary. Each
of its WebSockets ends with 4001 as the daemon's side of it ends
(§multi-transport). Nothing else happens: no port was freed, the origin
still has its owner, other workspaces do not notice (**observed**: the
listing said so 408 ms after `crew stop` was run, which is how long the
daemon took to exit; no scan ran).

**A daemon starts**: once its socket listens and its registry entry is
written it calls `web_rescan` on `web.sock` if that exists (as the operator,
with `web.token`; in the background; a daemon is not an agent's
environment). If the call fails although `web.sock` is there, it tries again
after 1, 2, 4, 8 and 16 s and then leaves it: a server that does not answer
for that long is found again by the next `crew web` or by a page asking for
the listing, both of which rescan. The reconcile that follows dials the new
incarnation whether or not the end of the old watch was handled yet
(**observed** in both orders: with the old watch's end held back for 1.5 s
the new daemon was shown at once and stayed shown when the old end arrived;
under revision 1's rule, "dial only what has no watch", it stayed `stopped`
for good). A workspace that was shown as stopped is live again within the
daemon's start plus one dial, with the same browser session (**observed**:
the same id, a new pid, a new operator token).

**It stops** when
- `crew web --stop` asks it to (`web_close` without a workspace);
- no joined workspace has been `running` for the *linger* time, 600 s
  unless `[web] linger_secs` says otherwise (0: at once). The time is there
  so that `crew stop` followed by `crew run` keeps the tab, and so that
  nothing is left listening on a machine where every team was stopped;
- it gets `SIGTERM` or `SIGINT`;
- it loses a listener it cannot get back, as today (`web.tick`);
- the path of its control socket no longer names its socket (the runtime
  directory was removed under it): checked on the 5 s housekeeping tick, so
  that a test's server does not outlive the test's directory.

Stopping is today's order with the new parts in their places: revoke codes,
sessions and every membership's generation (nothing new is admitted); every
connection lets go of its daemon and is closed, index connections after
their `shutdown` push; the listeners are released; the watches are closed;
`web.sock`, `web.token` and `web.json` are removed. A page that receives
the `shutdown` push stops for good, as a page does today.

**It is killed.** Every tab stops (no proof from whoever answers next, or
nobody for 30 s): §security "A document whose server is gone", unchanged. The
daemons and their teams are not affected; each notices its watch end and
clears its `web` line. The next `crew web` finds the lock free, starts a new
server with a new origin, and joins the workspace it was run in; the others
are joined again by running `crew web` in them, or all at once with `--all`.

**Versions.** The server and a daemon must speak the same protocol, as every
client must. A daemon that answers the server's `hello` with `protocol` is
joined in the state `incompatible` ("started by another version of crew:
restart the team") and dialled again when it starts again.

`crew web` compares itself with the server it finds (`web.json`: `protocol`,
`build`). **It never replaces a server unasked**: replacing one stops every
open tab and, when the protocols differ, takes the dashboard away from the
teams that still speak the old one.
- Another protocol: refused. "The dashboard is served by another version of
  crew (protocol 6, this one speaks 7). Teams started by that version keep
  their dashboard until they are restarted. `crew web --restart` replaces the
  server: open tabs stop, and teams of the other version are shown as
  incompatible until they are restarted."
- The same protocol, another build (`build`: the crate version and the
  bundle's manifest hash): served as it is, with a `note` line naming
  `crew web --restart`.

`crew web --restart` stops the server (`web_close`; or, when it cannot speak
to it, `SIGTERM` to the process `web.json` names, checked by its start time
and boot as the registry checks a daemon, then the lock) and starts this
build's. It stops no team. When both speak the same protocol it first asks
the old server for its listing and gives the new one the same memberships,
stopped ones included (`web_open {restore}`, §multi-join "Restoring"). When
they do not, it cannot ask: it joins the workspace it was run in and says
that the others are joined again with `crew web` or `crew web --all`.

This is about shared servers, from protocol 6 on. The step from protocol 5,
where each daemon serves by itself and there is no `web.json`, is
§multi-migration "From protocol 5".

---

<a id="multi-join"></a>
## §multi-join

### A workspace's name on the wire

The id of a workspace is the 16 hex digits of `session_hash(state_dir)`: the
stem of its daemon's socket and of its registry file. It is the same for
every daemon ever started in that workspace, it is not a secret, and it is
never used as a path: the server looks it up in its table of joined
workspaces and nowhere else.

### Joining (M2)

`web_open {workspace: <state dir>}` on the control socket joins one
workspace; `web_open {all: true}` joins every running session; a server
started with `[web] join = "all"` (user or global `config.toml`; the project
scope is not read for `[web]`: a workspace does not opt itself in) joins
every running session at each rescan, except those the operator took out
("Leaving", below). Joining a workspace means, in order:

1. Its registry entry is read (`registry_file(state_dir)`), and must be
   live (`is_live`: the pid, its start time, this boot).
2. Its socket must be the path the server itself derives for that state
   directory in its own runtime directory (`socket_path_in`), and a socket
   owned by this user. An entry naming any other path is refused
   (`invalid`), whoever wrote it.
3. A **membership** is made: `{id, generation, state_dir, project, state,
   summary, joined_at}`. The **generation** comes from a counter of the
   server and is never given twice. At most 32 memberships (`conflict`
   beyond). A workspace that is already joined keeps the membership it has.
4. It is reconciled (§multi-server): the server dials the daemon and says
   `hello` as the operator with the token of that daemon's token file, read
   at that moment with `read_operator_token` (owner, mode, size checks as
   for the CLI); then `web_attach` and `summary {watch: true}`
   (§multi-transport). This connection is the watch.

A daemon that cannot be reached in step 4 is joined all the same, so that
the page can say why it shows nothing of it: as `incompatible` when it
answered `protocol`, as `unreachable` when its token file could not be read,
it answered `unauthorized`, or it did not answer. One that fails step 1 or 2
is not joined, and
`web_open` answers the reason.

The table is in memory only: consent is given to a server, not for ever.

**Restoring.** `web_open {restore: [<state dir>, …]}` makes memberships for
the workspaces named whether or not their daemons run: steps 1 and 2 for
those that do, and `stopped` for the others, whose daemons are checked by
those steps when they start. It exists for `crew web --restart`, which
passes what the server it replaces had joined: the operator's consent,
carried from one server to the next by the operator's own command. Like
every call of the control socket it takes the token that no browser and no
agent has.

### Leaving (M9)

A table that is consulted at `hello` and never again does not take anything
back: a connection that was let in forwards frames for as long as it lives,
and a `hello` that looked the workspace up a moment before the removal
finishes its dial a moment after (**observed**, with the removal done the
way revision 1 described it: a client that ignored the listing had 59 of its
59 later messages delivered, and a `hello` that was dialling during the
removal was answered and worked).

So every pending open, every workspace connection and the watch **belong to
the generation** of the membership they were admitted under, and
`web_close {workspace}` (`crew web --remove`) does this, in this order:

1. **Revokes the generation**, under the table's lock. The membership leaves
   the table and its id enters the server's *removed* set. From this moment
   a `hello` for it answers `not_found`, and a rescan does not join it.
2. **Pending opens** of that generation are cancelled: a `hello` that is
   dialling its daemon is answered `not_found` and its dial is dropped, not
   waited for. One whose dial completed in the same instant fails its commit
   (§multi-transport "A workspace connection") and is answered the same.
3. **Every connection of that generation lets go of its daemon**: it stops
   forwarding in both directions and drops the daemon's side, and only then
   closes its WebSocket, with 4002 ("taken out of this dashboard"). The
   watch is closed.
4. **Waits for the generation to drain.** Every pending open and every
   connection holds a guard of its generation for exactly as long as it
   owns a connection to the daemon, or may still get one; the count of
   those guards is what is waited for.
   - A unit that has not let go within 1 s is **cancelled, and the
     cancellation is awaited**. `abort()` on a task is a request: the task's
     future, and the daemon's side that future owns, is dropped when the
     runtime next gets to it, not when `abort()` returns. So the server
     aborts the task; awaits its join handle, which resolves once the future
     was dropped; awaits likewise every task that one owns and that could
     hold the daemon's side (a connection's writer task, as `ws.rs` has
     today: `AbortOnDrop` ends such a child when its owner is dropped, and
     the child is awaited too, or is built so that it never holds the
     daemon's side); and then waits for the count again.
   - Only a count of zero lets the removal go on. Nothing on a connection's
     path may block a thread: what blocks cannot be cancelled.
   - The goodbye to a browser is not part of the count. A browser that does
     not read leaves its connection stuck in a write, where it never looks
     at the revocation; that connection is one of those cancelled after the
     second (**observed**: a connection made to hold on for 8 s, and one
     stuck writing to a client that had stopped reading, were each aborted
     after 1 s; the removal answered 2 ms later with the count at zero, and
     nothing sent afterwards was delivered).
5. **Answers** `{"removed": true}`, and the listing is pushed without it. If
   the count could not be brought to zero (no test may reach this), the
   answer is `internal`, the log says so, and the membership stays revoked.

What this guarantees: when `crew web --remove` returns, nothing a browser
sends reaches that workspace's daemon through the server, whatever the page
does (**observed**: the same stubborn client had none of its 58 to 60 later
messages delivered, and both of its connections were closed with 4002; a `hello` held
in a 1.5 s dial was refused within a millisecond of the removal, and the
removal answered right after it).
What it does not undo: a request the server had already written to the
daemon's socket may still be carried out; its reply goes nowhere.

Joining the workspace again makes a new membership with a new generation
(**observed**). Nothing admitted under the old one can come back under it.

**The removed set** is the server's, for its life. A rescan under `join =
"all"` skips what is in it (**observed**: a rescan did not bring the
workspace back). An explicit command takes a workspace out of it again:
`crew web` for that workspace, or `crew web --all`. A workspace also leaves
the table when the server stops, by the same steps for every membership.

### What the listing holds

```rust
// crates/crew-core/src/rpc.rs
/// What the `workspaces` call answers and the `workspaces` push carries: all of it, each time.
pub struct Workspaces {
    /// The revision of this listing: greater than that of every listing the
    /// server made before it (§multi-transport "Order on the index").
    pub rev: u64,
    /// Joined, in the order they were joined.
    pub joined: Vec<WorkspaceEntry>,
    /// Running sessions of this user that are not joined: names only.
    pub others: Vec<OtherSession>,
}
pub struct WorkspaceEntry {
    pub id: String,
    /// The membership's generation: greater after the workspace was taken
    /// out and joined again.
    pub generation: u64,
    /// The daemon the server's watch is on. Absent unless `running`.
    pub daemon: Option<DaemonRef>,
    pub project: Option<String>,
    /// The workspace directory.
    pub workspace: String,
    pub state: WorkspaceState,
    /// Why, for `incompatible` and `unreachable`; else empty.
    pub detail: String,
    /// The last summary its daemon gave; kept while it is stopped.
    pub summary: Option<WorkspaceSummary>,
    pub joined_at: i64,
}
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    /// Its daemon runs and the server has its watch.
    Running,
    /// No daemon runs for it.
    Stopped,
    /// Its daemon speaks another protocol version.
    Incompatible,
    /// The registry publishes its daemon and the server has no watch on it:
    /// it refused the server (the token file is missing or unreadable) or
    /// does not answer. `detail` says which.
    Unreachable,
}
/// One run of a workspace's daemon, as the registry publishes it.
pub struct DaemonRef { pub pid: u32, pub started_at: i64 }
pub struct OtherSession { pub id: String, pub project: Option<String>, pub workspace: String }
```

`others` exists so that the page can say what the operator would otherwise
have to guess: "search is running; `crew web -C ~/work/search` adds it". It
is read from the registry at each rescan and when a page asks for the
listing (at most once a second), not on a timer. It names sessions; it gives
no way to reach them.

### Stale entries, and sessions the operator did not start

- **A registry entry whose daemon is gone**: `list_sessions` removes it; the
  server never dials it (step 1).
- **A joined workspace whose directory was deleted**: stays `stopped` until
  it is removed or the server stops.
- **Sessions of other users**: not in this user's registry (a private
  directory under this user's state home), and their sockets are not in this
  user's runtime directory (step 2).
- **Sessions this user's programs started** (a test harness, a script, an
  unsandboxed agent that cleared its environment): in the registry like any
  other. With the default they are named in `others` and reachable by
  nobody; they are joined only if the operator runs `crew web` for them or
  chose `--all` or `join = "all"`. That is the reason for the default, and
  the README says so next to the setting.
- **A registry entry written by hand** to make the server dial something:
  writing there takes this user's file access, which is outside what crew
  defends against (design-b3.md §1); and step 2 still keeps the operator
  token from being sent to any socket but the one that workspace's daemon
  binds.

---

<a id="multi-security"></a>
## §multi-security

### What is kept, and where it is enforced now

| property of §security | now |
|---|---|
| Loopback only, both addresses, no `SO_REUSEPORT`, one port | the server, same code |
| An origin per server start, a label never reused, exact `Host`, exact `Origin` on `/ws`, `GET` only, files by table lookup, the CSP | the server, same code; `connect-src` is unchanged (one host) |
| A one-time pairing code in the fragment, a session token and a verifier, the proof in every `hello` reply, the session read once at the document's start, no code after the start | unchanged; codes are made on the control socket, sessions are the server's |
| Before `hello` nothing but `ping`, `web_pair`, `hello`; 5 s; 4 KiB | the server |
| A web session is the operator minus `web_open`, `web_close`, `load_team` (W7) | `load_team`: the daemon, on connections whose `hello` said `web` (below). `web_open`, `web_close`: not calls of any WebSocket any more |
| An agent's token is not a web session; a web session's token is nothing on a Unix socket | the server knows no agent tokens at all; the daemons know no web sessions at all |
| Stop order: credentials, then connections, then listeners | the server |
| No secret in logs, events or the database | the server's log and the daemons' events |

What an attacker who is not the operator can reach is what it was: the
port. A sandboxed agent has the host's loopback network and so the port; it
does not have `web.sock`, `web.token` or any token file (of the runtime
directory the sandbox profile mounts its daemon's socket and the agent's own
sidecar directory, nothing else; `a_worker_never_sees_the_token…` gains the
`web.*` names).

### The server to a daemon (M3)

The server holds no credential of its own for any daemon. It reads a
daemon's operator token from that daemon's token file each time it dials
(a restarted daemon has a new one: **observed**), uses it for one `hello`,
and does not keep it.

For a browser's workspace connection it sends

```json
{"id":0,"method":"hello","params":{"role":"operator","token":"…","client":"web","protocol":6,"web":true}}
```

`Hello.web` is new. A daemon that accepted an operator `hello` with `web:
true` treats the connection as today's `Transport::Web`: `load_team`,
`web_attach` and `web_event` answer `forbidden` (`not_on_the_web`). The
restriction is enforced where it always was, in the daemon, per connection;
the server only has to say truthfully which connections are a browser's,
and a unit test holds it to that (§multi-tests). `Transport::Web` and
`Session::web` go; `Conn` gets `web: bool`.

The watch is the server's own connection (`web: false`).

### A browser to the server

Pairing, the session and the proof are W6. What is new is their reach:

- **One pairing reaches every joined workspace**, those joined later
  included. A second pairing per workspace would add nothing: the page is
  one origin, so its documents share their storage and their script realm,
  and whatever holds the session for one workspace holds the others. The
  boundary that means something is which workspaces the *server* will dial,
  and that is the join table, which only the command line changes.
- `crew web` in any workspace makes a pairing code (the operator ran it).
  The code is the server's, not the workspace's; `&w=<id>` in the address
  only says which workspace the page opens first.
- Every WebSocket starts with `hello` and gets the proof in the reply, the
  index and each workspace connection alike, and the page uses nothing a
  connection carries before it checked it. A failed proof on any of them
  stops the page.

### What a web session may not do

It cannot join or remove a workspace, start or stop the server, or make a
pairing code: those are calls of the control socket, which is not reachable
from a browser and needs a token the page never sees. It cannot name a
workspace that is not joined: `hello` with an id that is not in the table
answers `not_found`, and the id is never turned into a path. It cannot load
a team (the daemon refuses). It cannot keep a workspace the operator took
out: the server ends its connections to it, whether the page lets go or not
(§multi-join "Leaving"). Everything else an operator can do in a joined
workspace, it can.

### One request per message

On a daemon's socket a request is one line. The server passes a browser's
text message to the daemon as one line without parsing it (§multi-transport
"What the server does to a frame"), so a message with a line break in it
would be two requests to the daemon (**observed**: two replies for one
message). Since the daemon enforces W7 per connection, a second request
gains nothing by itself; it would still make every later check that looks
at a message meaningless. A text message containing a line feed or a
carriage return closes the connection with 1008. `JSON.stringify` never
produces one.

### Limits

As §security "Limits", with: 64 authenticated connections instead of 32 (a
page has an index connection, a workspace connection and a terminal's), 32
joined workspaces, and 2 s for the server to reach a daemon when a browser
says `hello` (inside the 5 s the browser has). Rescan requests are folded
into a flag, so there is nothing of them to limit (§multi-server
"Reconciling").

### What remains, beyond §security "What remains"

- **A session's reach is every joined workspace.** A stolen session (which
  takes this user's browser profile or this user's files) reaches all of
  them, where it reached one. That is the cost of one dashboard, and the
  reason joining is explicit by default and `crew web --remove` exists.
- **Text from one workspace is rendered in a document that can act on the
  others.** The page still renders agents' text as text under the same CSP;
  a flaw there would now cross workspaces. No new mechanism is added for
  it: separate origins per workspace would end "one server one can switch
  in" (a switch would be a navigation and a pairing, and no page could show
  the others' badges).
- **The server can act as the operator of every running session of this
  user**, joined or not, in the sense that it runs as this user and could
  read any token file. So can every program this user runs. What the design
  guarantees is that it *does* not: it dials joined workspaces only.

---

<a id="multi-transport"></a>
## §multi-transport

### Two kinds of connection

Every WebSocket is `GET /ws` on the one origin, and begins as today
(`web_pair` once per browser, then `hello`). `Hello` gains two fields:

```rust
pub struct Hello {
    // …
    /// Set by the dashboard server on a connection it opens for a browser.
    #[serde(default)] pub web: bool,
    /// From a browser to the dashboard server: the workspace this connection
    /// is for. Absent: the index connection.
    #[serde(default)] pub workspace: Option<String>,
}
```

### The index connection

`hello` without `workspace` is answered by the server itself: `HelloInfo
{role: operator, project: None, version, protocol, pid: <the server's>,
web_proof}`. A page has one for its whole life; it is the page's link to its
server (§multi-ui "Two levels"). On it:

| call | answers |
|---|---|
| `ping` | `{"pong": true}` |
| `workspaces` | `Workspaces` (§multi-join); also asks for a rescan, at most one a second |
| `web_prefs {set?}` | the dashboard's own preferences (below) |

and two pushes, `workspaces` and `shutdown`. There is no `subscribe`. Any
other call answers `invalid`.

**Order on the index.** A reply and a push both carry a whole listing, and
"the one that arrives last wins" is not a rule: a reply made at one moment
and a push made after it reach the page's code in either order, and a page
that takes the later arrival can be left with the older listing for good.
The rule:

- The server keeps one listing and one **revision** of it: a number that
  grows by one with every change (the table, a state, a summary, `others`)
  and is never given twice in the server's life. `Workspaces.rev` is the
  revision of the listing it is sent with, in a reply as in a push. Listing
  and revision are read together, under the table's lock.
- A push is sent on a connection when the revision is greater than that of
  the last push sent on it. It carries the listing as it is when it is sent,
  not as it was when something changed. At most one per 100 ms per
  connection, the first at once. So on one connection the revisions of
  pushes only grow (**observed**), and a reply is never older than a push
  sent before it (**observed**).
- **The page applies a listing only if its `rev` is greater than that of
  the last listing it applied**, however each arrived, and keeps that number
  for the document's life. (Its server is the same process for the
  document's life: no other can show the proof.)
- A page applies nothing a connection carries before that connection's proof
  is checked. It then calls `workspaces` once. A push that came earlier and
  was dropped is covered by that reply, whose revision is at least the
  push's. A push that overtakes the reply is newer and is applied; the reply
  that arrives after it is older and is dropped.

### A workspace connection

`hello` with `workspace` makes today's connection to a daemon, through the
server:

1. The server checks the protocol version and the session token itself, in
   the daemon's order (`protocol`, `unauthorized`).
2. **Admission**, under the table's lock: the id must be joined
   (`not_found`) and its state `running` (`unavailable` for `stopped` and
   `unreachable`, with the reason; `protocol` for `incompatible`). The open
   is counted for that membership's generation from here on, before
   anything is dialled.
3. It dials the daemon and says `hello` to it (§multi-security), within
   2 s. A failure answers `unavailable` and asks for that workspace to be
   reconciled (§multi-server).
4. **Commit**, under the lock again: if the generation was revoked in the
   meantime, the daemon's side is dropped and the answer is `not_found`.
   Otherwise the browser gets the daemon's `HelloInfo`, with the session's
   `web_proof` put into it.
5. From then on it passes frames through, both ways.

It ends when
- the browser closes it;
- the daemon's side ends: closed with **4001**, "the workspace's connection
  ended", and the server reconciles that workspace. 4001 says what happened
  to this connection and nothing about the daemon: whether the workspace
  still runs is the listing's to say (§multi-ui);
- its generation is revoked: **4002**, "taken out of this dashboard"
  (§multi-join "Leaving");
- its browser session is revoked, or the server stops: as every connection
  ends today.

In each case the daemon's side is dropped before the WebSocket's goodbye is
sent.

The state connection and each terminal's connection are workspace
connections, as today. `subscribe`, `snapshot`, the barrier (W13), `attach`,
`resync`, every call and every push are the daemon's, unchanged, because the
daemon cannot tell this connection from any other operator's on its socket
except by `web`. One WebSocket is still one daemon connection (W2).

**Which `hello` the server answers.** The first one of a connection, and
only that: it answers it itself (the index) or sends one of its own to the
daemon in its place (a workspace). Every frame after it is passed through, a
second `hello` included. The daemon refuses that one as it does today
(`forbidden`: a connection is who it first said it was), so neither the
identity nor the `web` mark of a connection can be changed from the browser
(**observed**). A daemon ignores `workspace` in a `hello`.

Why not one WebSocket carrying several workspaces: frames would need an
envelope (W2 has none), a terminal's output would wait behind another
workspace's snapshot on the same stream, and the barrier would have to be
defined per channel inside a stream. Three connections per page cost three
handshakes at the start, in parallel.

### What the server does to a frame

After the two `hello`s, nothing: a line from the daemon is one text message
to the browser, a text message from the browser is one line to the daemon.
The server does not parse either (it checks that a browser's message has no
line break, §multi-security, and that a daemon's line is UTF-8). So the hot
path is a read and a write, and no meaning of the protocol is written down
a second time. A daemon line is at most `MAX_FRAME_BYTES`, the WebSocket
limit is the same constant.

A slow browser: the server stops reading that connection's daemon socket
while a WebSocket write is pending; the daemon then sees a slow reader on
its socket and does what it does for any (`resync`, §transport "Reconnect
and resync"). The server buffers one frame per connection, not a queue.

A daemon that stops in order sends its `shutdown` push first, as to every
subscriber; the connection then ends with 4001 like any whose daemon's side
ended.

### Summaries (M5)

The switcher shows, for every joined workspace, whether someone is waiting
for the operator. The page has a state connection to one workspace only, so
that has to come from elsewhere, and it should not come from the server
keeping a model of every workspace: that would be the page's store written
again in Rust, fed by every push of every team.

```rust
pub enum Call {
    // …
    /// Operator: what a workspace switcher shows of this session. With
    /// `watch`, the connection then gets a `summary` push whenever the
    /// summary changed: the first change at once, then at most one per
    /// 250 ms.
    Summary { #[serde(default)] watch: bool },
    /// Operator on the socket, not web: this connection is the dashboard
    /// server's watch. `SessionInfo.web` is `url` until it ends.
    WebAttach { url: String },
    /// On the watch: something about the dashboard the session's log should hold.
    WebEvent { kind: WebEventKind, #[serde(default)] count: u32 },
}
#[serde(rename_all = "snake_case")]
pub enum WebEventKind { Paired, AuthFailed }
pub struct WorkspaceSummary {
    /// Grows with every change of the summary, for the daemon's life.
    pub rev: u64,
    /// The team's name once one is loaded; the listing shows it in place of
    /// the registry's.
    pub project: Option<String>,
    pub agents: u32,
    /// Agents whose status is `working`.
    pub working: u32,
    /// Agents that cannot go on by themselves: status `blocked` (whatever
    /// the kind) or `failed`.
    pub attention: u32,
    /// Questions put to the operator that nobody answered yet.
    pub questions: u32,
    pub tasks: u32,
    pub tasks_done: u32,
}
pub enum Push { /* … */ Summary(WorkspaceSummary), Workspaces(Workspaces) }
```

The daemon computes the summary from its model when an agent, a task or an
open question changed and at least one connection watches. It holds no
times and no progress: it changes when an agent changes status, a task is
added or done, or a question opens or closes. An idle workspace sends none;
a busy one a few a minute.

**Order.** `summary {watch: true}` registers the watcher and captures the
summary in one step with respect to changes: a change is either in the reply
or followed by a push, never neither. (A `tokio::sync::watch` channel that
is written where the model changes, and subscribed and read in one call, has
exactly this property; a capture followed by a registration does not.) A
push carries the summary as it is when it is sent. `WorkspaceSummary.rev`
grows with every change, and a watcher applies a summary only if its `rev`
is greater than the last it applied *from that connection*: a new watch, on
a new incarnation, starts from its own reply.

The server's **watch** on each joined, running workspace is one operator
connection: `hello`, `web_attach {url}`, `summary {watch: true}`. It never
subscribes. Each summary it applies replaces the entry's and changes the
listing's revision. The end of the watch is an event for the reconciler
(§multi-server); it is how the server learns that something happened to the
daemon, and the registry says what.

`web_attach` gives the daemon what it lost with its own server: the `web`
line of `crew status` (`SessionInfo.web`, with a `session` push when it
changes) and two events in the session's log, `web.joined` ("shown in the
web dashboard at http://c7f3….localhost:23417/") when it is called and
`web.left` when that connection ends. `web_event` adds `web.paired` ("a
browser was paired with the dashboard (3 sessions)") and `web.auth_failed`
("7 refused web logins in the last minute", at most one a minute) to the
log of every joined workspace, since a new browser session is something
each of them should have on record. The texts are the daemon's; the server
sends a kind and a count.

### Preferences

`web_prefs` on a workspace connection is the daemon's, as today, and holds
what belongs to the workspace: `layout` (where agents were dragged).
`web_prefs` on the index connection is the server's and holds what belongs
to the operator: `keys`, `panel`, and `last` (the id of the workspace last
on screen). The server keeps it in `web-prefs.json` beside its log (mode
0600, an object of at most 16 KiB, else `invalid`), so it outlasts the
server. The page validates every field of both, as today. A workspace's
stored `keys` and `panel` from before are read once as defaults when the
server's file has none.

---

<a id="multi-ui"></a>
## §multi-ui

### Two levels (M6)

| | the page | the workspace on screen |
|---|---|---|
| connection | the index connection | its state connection (and terminals) |
| states | `connecting`, `live`, `reconnecting`, `signed-out`, `stopped`, as today (`[data-conn]`) | `connecting`, `live`, `reconnecting`, `not-running`, `incompatible`, `removed`, `none` (`[data-workspace-state]`) |
| stops for good on | the server's `shutdown`, a failed proof on any connection, `unauthorized`, 30 s without the server | never |
| removes the stored session | when it stops, as today | never |

`net/conn.ts` stays the connection; `net/state.ts` splits into `net/hub.ts`
(the index: the listing and its revision, the dashboard's preferences, the
page's state) and `net/workspace.ts` (today's link, for one workspace id).

**The listing says what a workspace is. A connection says only what happened
to that connection**, and by the time the page hears of it the listing may
already describe a later membership or a later daemon. So the state of the
workspace on screen is **computed**, from the newest listing and from
whether the page has a live connection, and nothing a connection reports
sets it:

| the newest listing says | the page's connection | shown | the page |
|---|---|---|---|
| the workspace is not joined | any | `removed` | closes it; no tries |
| `stopped`, `unreachable` | any | `not-running` | closes it; no tries |
| `incompatible` | any | `incompatible` | closes it; no tries |
| `running` | live | `live` | |
| `running` | none | `connecting`, `reconnecting` | tries |

It is evaluated when a newer listing is applied, when the current attempt
ends, and when the index connection becomes live. What was shown stays,
read-only and dimmed, whenever the state is not `live`.

**An attempt** is one try to connect: a socket, its `hello`, and what
follows on it. It has a number, and it records the `generation` and the
`daemon` of the workspace's entry in the listing it was started under. Only
the current attempt's reports are acted on; whatever the socket of an
earlier attempt still delivers is dropped, as `conn.ts` drops the events of
a socket it has replaced.

**What an outcome does.** Every way the current attempt can end (a close
with any code, an error to its `hello`, the daemon's `shutdown` push, a
timeout, a snapshot after `resync` that fails) ends that attempt and nothing
else: its connection is closed, and the state is evaluated again from the
listing.

- 4002, `not_found` and `protocol` say that the listing the attempt was
  started under may be out of date. Before it evaluates, the page calls
  `workspaces`, and then evaluates with the newest listing it has: the reply
  if its `rev` is greater, the one it already had otherwise. **A reply with
  an equal `rev` settles it just the same**: it says that the listing the
  page has is the current one, and the page acts on that. It is not applied
  a second time, and nothing waits for a push that need not come.
- Any other outcome: evaluated at once. The server reconciles the workspace
  when a dial fails or a daemon's side ends (§multi-server), so if the
  daemon did stop, a listing says so within that reconcile.

**Tries.** While the evaluation says so: a new attempt after the backoff
delay (250 ms doubling to 5 s, ±20 %), for as long as the listing says
`running`. There is no giving up into a state that only a change of the
listing would end, because nothing promises that change. The delay starts
over, and the next attempt is made at once, when the entry's `generation` or
`daemon` differ from those the last attempt recorded, and when the
workspace turns to `running` in the listing.

Why no outcome may set a state (**observed**; the last two with a model of
these rules run against the real server, in revision 2's version and in
this one):

- One dial fails while the daemon and the server's watch are healthy. The
  listing does not change and an idle workspace says nothing more: the
  revision did not move and no push came. A page that waited for a listing
  would wait for ever; the next try got through.
- A workspace is taken out and joined again within one push interval. The
  page gets one listing, `running`, with the new generation, and then the
  old connection's 4002. Revision 2's page said `removed` and stayed there,
  with the listing saying `running` and nothing more to come. This page ends
  the old attempt, asks for the listing, finds `running` under a generation
  it has not tried, and is live on its second attempt.
- A `protocol` answer that is about a daemon since replaced. Revision 2's
  page said `incompatible`, for good. This one asks, finds the listing
  unchanged and `running`, and its next attempt is live.

The rest:

- A workspace connection is opened only while the page is `live`. Its
  `hello` carries the id.
- When the index connection is lost the page is `reconnecting` and
  read-only as today, whatever the workspace connection does; no attempt is
  started and no listing is acted on until the index is live again. When the
  page stops, every connection is closed and nothing is requested again.
- A failed proof on a workspace or terminal connection stops the page
  (`link.stop("proof")` as the terminal does today). `unauthorized` on one
  does too: the session is the server's, whichever connection learns that
  it is gone. These two are about the page's server, not about a workspace,
  and are the only reports of a workspace connection that reach beyond its
  attempt.

### Which workspace

In this order: the id in the address (`#/w/<id>/…`), the `w` of the pairing
fragment, `last` of the dashboard's preferences, the first joined workspace
that is running, the first joined. An id that is not joined: the page says
so ("that workspace is not in this dashboard: run `crew web` in it") and
offers the switcher; it does not fall through silently to another one.
Nothing joined (every workspace was removed): "No workspace is shown here.
Run `crew web` in a workspace."

### Addresses (M7)

`#/w/<id>/`, `#/w/<id>/tasks`, `#/w/<id>/logs`, `#/w/<id>/usage`, each with
today's one selection (`?agent=`, `?task=`, `?message=`, `?event=`). A route
without `/w/<id>` is rewritten in place (`replaceState`) to the chosen
workspace's. Switching pushes a history entry, so the browser's back button
returns to the workspace left. `#pair=<code>&w=<id>` is taken out of the
address before anything else, as `#pair=` is today, and leaves `#/w/<id>/`.
`route.ts` gains `workspace: string | null` in `Route`.

### The switcher

The project name at the left of the status bar becomes a button, `<project>
▾`, with the others' badges beside it when there are any: `? 2` (questions
waiting in other workspaces, summed) and `! 1` (agents needing attention in
other workspaces). It opens a list:

```
  ● payments      ~/work/payments     6 agents · 3 working    tasks 4/6            1
  ● search        ~/work/search       4 agents · 1 working    tasks 1/9   ? 2      2
  ○ billing       ~/work/billing      stopped                                      3
  ⚠ infra         ~/work/infra        started by another version of crew: restart the team
  ─ running, not in this dashboard ─
    docs          ~/work/docs         crew web -C ~/work/docs                    [copy]
```

- A listbox: one tab stop, arrows move, `enter` switches, `esc` closes and
  returns focus to the button, typing filters by project and path, `1`–`9`
  switch directly while it is open. `w` opens it from anywhere single-key
  shortcuts work (the key is free in §ui's table; the help overlay lists
  it). The current workspace is marked `aria-current`.
- A row shows the state dot, project, directory (home as `~`), the
  summary's counts, and its badges. `stopped` rows are dimmed and can be
  chosen (the page then shows why nothing is live). `others` rows are text
  with a button that copies the command; they cannot be chosen.
- The document title is `<project> · crew`, with `(n)` in front when `n`
  questions wait in any joined workspace.
- When a question appears in another workspace (its `questions` went up),
  one notice: "search: 2 questions are waiting", with a "switch" action.
  The summary holds counts only, so no text of one workspace is shown
  inside another.

### A switch

1. The state connection and the terminals of the workspace left are closed;
   its store, animations and inspector are disposed. Its route (view and
   selection) is remembered for this document, so coming back shows the
   same view.
2. The address changes; `last` is written (500 ms debounce, as preferences
   are).
3. If the new workspace is `running`: connect, `hello`, `subscribe`,
   `snapshot`, `web_prefs`; the page shows the frame of the workspace (status
   bar with the new name, "connecting…") at once and the content when the
   snapshot is applied. **Observed** on the wire: 10.7–12.8 ms at the median,
   12–15 ms at p95, for a 250–294 KiB snapshot.
4. Nothing of the old workspace remains on screen once the new name is
   shown: no agent, message or terminal of one is ever drawn under the name
   of the other. The store is a new one, not the old one emptied.

Operator actions, dialogs and the message box act on the workspace on
screen and are disabled while it is not `live`. A dialog that is open when
the workspace changes (by a `workspaces` push removing it, or by the back
button) is closed, not carried over.

### Not running, incompatible

- `not-running`, with a last state: that state, dimmed and read-only, under
  "payments is not running. Start it (`crew run` in ~/work/payments) and it
  comes back here." Without one (the page was opened on it): the same
  sentence on an empty main area.
- `incompatible`: "payments was started by another version of crew (0.1.0,
  protocol 5). Restart the team to see it here."
- `removed` (the listing no longer has the workspace: it was taken out
  with `crew web --remove` while on screen): "payments was taken out of this
  dashboard", and the switcher. The page does not have to cooperate for
  that to be true: the server has ended its connections (§multi-join
  "Leaving"). If it is joined again, the listing has it again and the page
  connects.

---

<a id="multi-migration"></a>
## §multi-migration

### The command

```
crew web [-C DIR] [--port N] [--no-open] [--json]   join this workspace, print the address, open the browser
crew web --all                                       join every running session
crew web --remove [-C DIR]                           take this workspace out of the dashboard
crew web --list                                      what the dashboard shows (and what runs beside it)
crew web --restart                                   replace the server with this build's, keeping its workspaces (open tabs stop; no team does)
crew web --stop                                      stop the server: every browser session ends, for every workspace
```

- What a script written against today's `crew web --json` reads keeps its
  meaning: `url` is the origin, `port`, `pair_url`, `expires_in_secs`,
  `started` (this call started the server). New in `WebInfo`: `workspace`
  (the id), `workspace_url` (`url` + `#/w/<id>/`), `joined` (the ids this
  call joined). `pair_url` ends in `&w=<id>`.
- `--stop` now ends the dashboard of every workspace, which is what "stop
  serving" means when there is one server; it says how many workspaces it
  was showing. `--remove` is the narrow one.
- `--port` with a server already on another port: `conflict`, as today.
- `--all` needs no workspace of its own; without any running session it
  says so and starts nothing.
- `crew status` prints the `web` line while the workspace is joined (from
  `SessionInfo.web`, set by `web_attach`).

### Protocol 6 (M8)

| change | why the version moves |
|---|---|
| `Hello.web`, `Hello.workspace` | a daemon that ignored `web` would give a browser `load_team` |
| `summary`, `web_attach`, `web_event`, `workspaces`, `web_rescan`; `web_close` takes `workspace`; `web_open` takes `workspace`, `all` and `restore` | new calls, changed parameters |
| pushes `summary`, `workspaces` | new push kinds |
| a daemon answers `web_open`, `web_close`, `web_pair` with `invalid` ("served by the dashboard server: run `crew web`") | changed meaning |
| `WebInfo` gains three fields; `SessionInfo.web` is set by the watch | changed meaning |

The manifest test in `rpc.rs` moves to `(6, version_6)` with the two push
kinds added. A team started before the upgrade answers `protocol` to the new
CLI as always ("restart the team after upgrading"); to the new server it is
`incompatible`.

### What moves, what goes

- `crates/crew/src/web/` (listener, HTTP, WebSocket, registry of codes and
  sessions, assets) becomes the server process's; `Web::open` and
  `Web::stop` lose their `&Daemon`. `ws.rs` drives a pipe to a Unix socket
  instead of a `Session`.
- The daemon loses `Daemon.web`, `Session::web`, `Transport::Web`, the
  `web.tick` housekeeping and `web.stop` on shutdown; it gains `Conn.web`,
  the summary, `web_attach`, `web_event` and the rescan call at start.
- `web_prefs` stays in the daemon for `layout`.
- There is one way to serve the dashboard, not two: no flag keeps a daemon
  serving by itself.

### From protocol 5

Protocol 5 has no shared server: each daemon that was asked serves its own
dashboard on a port of its own, and there is no `web.sock` and no
`web.json`. So the first upgrade finds nothing to replace, and replaces
nothing:

- **A team started before the upgrade keeps running as it is**, its own
  dashboard included: its listener, its address, its open tabs. Nothing of
  the new crew stops or changes it. The new `crew web` in its workspace is
  refused the way every command of a new crew is refused by an old daemon
  ("restart the team after upgrading"), before it starts or joins anything.
- **When that team is restarted** by the new crew, its old dashboard ends
  with its old daemon (its tabs stop, as tabs did at any restart), and
  `crew web` joins the new daemon to the shared server.
- **The shared server meets a protocol-5 daemon** only through `--all` or
  `join = "all"`. It is joined as `incompatible` and shown with that
  sentence; it becomes `running` when its team is restarted and the new
  daemon says that it started.
- The two kinds of server do not get in each other's way: the shared one
  takes the first free port of its sixteen, as every server has.
- No team is stopped, by any command here, to make room for a dashboard.
  `crew web --restart` and the version rules of §multi-server are about
  shared servers, which exist from protocol 6 on.

---

<a id="multi-perf"></a>
## §multi-perf

The budgets of §perf stand. P5 and P6 are measured with the index and one
workspace connection, as the page now starts. Added:

| # | what | budget | spike (**observed**) |
|---|---|---|---|
| P16 | a push is on the wire, now measured at the browser's end of the server | ≤ 1 ms at p95 (unchanged) | the hop costs −62 to +25 µs at the median: noise |
| P17 | a switch: from `enter` in the switcher to the first frame of the other workspace with its snapshot applied, 10 agents, 300 messages, 1000 events | ≤ 150 ms at p95 | the wire part (connect, `hello`, `subscribe`, `snapshot`): 12–15 ms at p95 |
| P18 | a question asked in a workspace not on screen: from the daemon's `message` push to the badge | ≤ 500 ms at p95 | not spiked: 250 ms (daemon) + 100 ms (server) of coalescing at most |
| P19 | an idle page with five joined workspaces: index frames received and calls made in 60 s, pings aside | none | by construction (summaries hold no times) |
| P20 | the server with five joined workspaces and three pages, idle: resident memory; wake-ups | ≤ 40 MB; none under 5 s but pings | not spiked |

How the server meets P16: a frame is read and written, not parsed
(§multi-transport); `TCP_NODELAY` on every accepted socket, as today; one
task per direction per connection.

---

<a id="multi-tests"></a>
## §multi-tests

### Wire fixtures (owner: vcs; W12)

Added under `crates/crew/tests/fixtures/web/`: `hello` with `workspace` and
its four refusals; the index `hello` reply; `workspaces` reply and push with
every `WorkspaceState` and a non-empty `others`; `summary` reply and push;
`web_open` reply with the new fields; the daemon's `invalid` for the three
calls it no longer serves. The frontend's decoders and its mock read them.

### Rust (owner: vcs)

The server process, started in the foreground with a runtime directory of
the test's own and real daemons beside it:

- **Single instance and files**: a second server exits; stale `web.sock`,
  `web.token`, `web.json` of a dead one are replaced; modes 0600; the
  server refuses to start in an agent's environment.
- **Control socket**: no token, an agent's token, a daemon's operator token:
  `unauthorized`; the calls of a WebSocket (`snapshot`, `send`) are
  `invalid` there. The control-only calls (`web_open`, `web_close`,
  `web_rescan`) do nothing on any WebSocket: `unauthorized` before `hello`,
  `invalid` from the server on the index connection, `invalid` from the
  daemon on a workspace connection (it no longer has them); in none of the
  three is a workspace joined, removed or rescanned. `workspaces` is
  answered on the control socket and on the index connection, with the same
  listing and revision.
- **Join**: one workspace; `all`; a registry entry whose socket is not the
  path derived for its state directory (refused, and no connection is made
  to it: a listener there sees none); a dead entry; the 33rd workspace;
  `join = "all"` picks up a daemon started later; the project scope's
  `[web]` is ignored; joining twice keeps the generation.
- **Leaving** (the stand-in daemon records every line it receives and the
  end of every connection, with times):
  - a client that ignores the listing and every close keeps sending from
    before `web_close {workspace}` until after its answer: the daemon
    received nothing that was sent after the answer, and had seen the end of
    each of that generation's connections, the watch included, before the
    answer was sent;
  - a `hello` held in its dial (the daemon delays its reply) while the
    workspace is removed: answered `not_found` at once, the dial's
    connection is closed, and the removal's answer comes after it;
  - a dial that completes in the instant of the removal fails its commit:
    `not_found`, and the daemon's side is closed (the test holds the commit
    with a hook and removes then);
  - a connection that does not let go (a hook holds it in an await after
    the revocation) is cancelled after 1 s, and the answer is sent only
    after its future was dropped: the hook's guard records its own drop,
    and the test compares that time with the answer's, not with the time
    `abort()` was called. The same with the daemon's side moved into the
    connection's writer task: that child is ended and awaited too;
  - a browser that does not read its socket, while the daemon sends more
    than the socket buffers hold: the connection is stuck in a write and
    never sees the revocation; it is cancelled after 1 s, the answer
    follows the drop, and the daemon receives nothing afterwards;
  - the count that cannot reach zero (a guard leaked by a hook): `internal`,
    logged, and `hello` for the workspace stays `not_found`;
  - joining again: a greater generation, a new `hello` works, the old
    connections stay closed;
  - under `join = "all"`, rescans do not join a removed workspace; `crew web`
    for it and `crew web --all` do;
  - a stopped workspace is removed; removal of one that is not joined is
    `not_found`.
- **Restoring**: `web_open {restore}` with one running and one stopped
  workspace: both joined, the stopped one `stopped`; it turns `running` when
  its daemon starts; a restored path whose daemon then publishes another
  socket path is refused at that point.
- **The daemon's side**: `hello` with `web: true` and the operator token:
  `load_team`, `web_attach`, `web_event` are `forbidden`, everything of W7's
  rest works; `web: true` with an agent's token is an agent's `hello` as
  before (the flag grants nothing); `web_attach` sets `SessionInfo.web`,
  pushes `session`, emits `web.joined`, and the end of that connection
  clears it and emits `web.left`; `summary` counts agents, attention,
  questions and tasks from a scripted team, a watch gets a push for each
  change and at most one per 250 ms, an idle team gets none in 2 s.
- **A workspace connection**: the server's `hello` to the daemon has `web:
  true` (asserted on a stand-in daemon that records it) and the token that
  is in the file at that moment; the reply to the browser is the daemon's
  with the proof; frames pass unchanged both ways (the fixture frames, byte
  for byte); a message with `\n` or `\r` closes with 1008 and the daemon
  receives nothing; the barrier test of §tests passes through the server
  unchanged; `not_found`, `unavailable`, `protocol`.
- **A daemon stops and returns**: its connections close with 4001; the
  index gets `stopped` with the last summary; other workspaces' connections
  are untouched; after the daemon starts again the index says `running`
  without any timer in the server having fired (the test freezes time), and
  the same session reaches it with the new token.
- **Reconciling** (time frozen; the registry reader and the dialler behind a
  seam the test controls):
  - the end of the old watch is handled, then the rescan: `stopped`, then
    `running` on the new incarnation;
  - the rescan is handled while the old watch has not ended: the new
    incarnation is dialled and shown at once; the old watch's end, delivered
    afterwards, changes nothing;
  - a rescan is asked for while a scan is held after its reading of the
    registry, and a daemon is published in between: when the scan is
    released a second reading happens and finds it; ten requests during one
    scan cause one more scan, not ten;
  - a watch ends while the registry still publishes the same incarnation:
    dialled again; while it cannot be dialled, again after 250 ms, 500 ms …
    5 s, and no longer once the entry is gone;
  - a workspace connection's failed dial and a workspace connection's end
    each cause one reconcile of that workspace and no scan;
  - the daemon's side: `web_rescan` is called once at start when `web.sock`
    exists, not at all when it does not, and again after 1, 2, 4, 8, 16 s
    while the call fails;
  - an idle server with healthy watches arms no timer but housekeeping and
    pings (asserted on the frozen clock's pending timers).
- **Order**:
  - on one index connection, revisions of pushes strictly increase, and a
    reply's revision is not below that of any push sent before it;
  - a thousand changes of the listing while a client calls `workspaces` a
    hundred times: the page's rule (apply only a greater `rev`) applied to
    the frames in the order received ends at the server's final listing, and
    so does the same run with replies delayed by a hook;
  - `summary {watch: true}` racing a change of the model, a thousand times:
    the watcher's last summary equals the model's, its revisions increase,
    and no change is in neither the reply nor a push. A hook between capture
    and registration (if an implementation has that gap) makes it fail;
  - a summary from a superseded watch is not applied.
- **One failed dial, healthy watch**: `hello` for a running workspace whose
  daemon refuses one connection: `unavailable`; the listing's revision does
  not change and nothing is pushed; the next `hello` succeeds.
- **The server's life**: linger (with `linger_secs = 0`: exits when the last
  joined daemon stops; with 600 and frozen time: at 600 s; a daemon back at
  599 s cancels it); `web_close` without a workspace runs the stop order
  (the existing stop-order tests, moved); the control socket's path removed:
  exits within two ticks; `SIGTERM`.
- **Versions**: a daemon speaking another protocol is `incompatible`, and
  `running` after it restarts with this one; `crew web` refuses a server of
  another protocol, older or newer, and stops nothing; it notes another
  build; `crew web --restart` with the same protocol carries every
  membership over, a stopped one included, and with another protocol joins
  the invoking workspace and says so; no command here ends a daemon (the
  daemons' pids are the same before and after each).
- **From protocol 5**: a stand-in daemon that answers `protocol`: `crew web`
  for it is refused with the usual sentence and starts no server; under
  `--all` it is listed `incompatible`.
- **Everything of §tests "Rust: the bridge"** that is about the listener,
  HTTP, pairing, sessions, the proof and the limits runs against the server
  process instead of a daemon.
- **Sandbox**: a sandboxed agent cannot see `web.sock`, `web.token`,
  `web.json` (profile test), and its prober (the live spec's) is still
  `unauthorized` on the port.
- **P16** through the server.

### Frontend (owner: web)

- vitest: `hub.ts` and `workspace.ts` state machines (every row of
  §multi-ui "Two levels", including: `shutdown` on a workspace connection
  does not stop the page or remove the session; `shutdown` on the index
  does; a failed proof on a workspace connection stops the page; no
  workspace connection is opened before the index is live or for a
  workspace that is not `running`; no timer while the listing says it is
  not `running`);
  **the listing's order**: a reply older than a push that arrived before it
  is dropped; an older push after a newer reply is dropped; pushes before
  the proof are dropped and the reply that follows covers them; equal
  revisions are not applied twice;
  **a connection is not the daemon**: with the listing saying `running` and
  never changing, a workspace connection that ends with 4001, with the
  daemon's `shutdown` push, with `unavailable`, with a timeout, is tried
  again at 250 ms, 500 ms … 5 s without limit and is live again when a try
  succeeds; the tries stop when a listing says `stopped` and a connection is
  made at once when one says `running`;
  **an outcome is about its attempt** (fake sockets and timers, every step
  in a fixed order):
  - the delayed old close: a listing at rev N (`running`, generation G) and
    a live connection; a listing at rev N+1 (`running`, generation G+1: the
    removal and the rejoin, coalesced); then the first connection closes
    with 4002. The page calls `workspaces`; the reply has rev N+1; a new
    attempt is made at once and is live. `[data-workspace-state]` never was
    `removed`;
  - the same with the reply's `rev` greater and the workspace absent from
    it: `removed`, no attempt; and greater with the workspace `running`:
    live;
  - the late `protocol`: under a listing that says `running`, the attempt's
    `hello` is answered `protocol`; the page asks; the reply has the same
    `rev`; the next attempt is live. It never was `incompatible`. With a
    reply that says `incompatible`: that, and no attempt;
  - `not_found` to a `hello`, the same three replies;
  - a 4002, a `protocol` and a close that arrive from an attempt already
    replaced: dropped; no call, no change of state;
  - a reply with an equal `rev` is not applied to the store a second time
    (no reconcile, no render), and still settles the state;
  - the backoff starts over when `generation` or `daemon` of the entry
    differ from the last attempt's, and not otherwise;
  route parsing and rewriting with `/w/<id>`; the pairing fragment with
  `&w=`; the choice of workspace; the switcher's rows from fixtures; title
  and badges from summaries; preferences split in two.
- Playwright, `mock`, for the same race in a real browser: the mock takes a
  workspace out and joins it again within 100 ms, sends one listing, and
  holds the old socket's close frame back for 300 ms; the tab ends `live`
  and shows the workspace's agents. The same with a held `protocol` answer.
- Playwright, `mock` (the mock server gains an index and two workspaces):
  the switcher by keyboard and by mouse; a switch disposes the old store
  (no element of the old workspace remains, checked while the new snapshot
  is delayed); the back button; a deep link to each workspace; a question
  in the other workspace raises the badge and the notice; a workspace stops
  and returns; a workspace is removed while on screen; an id that is not
  joined; `others`; a dialog open during a switch is closed.
- Playwright, `perf`: P17, P19.

### Live (owner: qa)

Two real daemons, the real server, Chromium:

- `crew web` in A, then in B: the same origin, the tab opened for A shows B
  in its switcher without a reload; switching shows B's agents; an operator
  action in B is confirmed by B's events and absent from A's.
- A question asked by an agent of B while A is on screen: the badge, then
  the banner after the switch.
- `crew stop` for A: the tab stays live, A is `not-running`; `crew run`
  again: A is live again in the same tab, with the same session.
- `SIGKILL` for A's daemon: the same.
- The lifecycle spec, with the server in the daemon's place: `SIGKILL` the
  server, a squatter on both loopback addresses, a hostile second document:
  the open tab refuses the forged proof and stops; a new `crew web` gives a
  new label.
- `crew web --remove` for B while it is on screen and a script in the page
  keeps sending a message every 50 ms through the page's own connection: no
  message sent after the command returned is in B's events; the page says
  B was taken out. `crew web` in B again: B is back, in the same tab.
- `crew web --restart` with A running and B stopped: the new address shows
  both, B as not running; the daemons' pids did not change.
- `crew web --stop`.
- A third session that was never joined: named in `others`, and `hello`
  with its id is `not_found`.
- The outside spec unchanged (another origin, the numeric addresses, an
  agent's token), plus the sandboxed prober.
- P18 measured; P5–P11, P14 measured again through the server.

`scripts/web-e2e` and the harness give each test its own runtime directory,
as now; the harness's teardown runs `crew web --stop`, and the server's
exit when its directory is removed catches what a failed test leaves.

---

<a id="multi-split"></a>
## §multi-split

### Member **vcs** (task `multi-server`)

In this order, each step a commit that builds and passes its tests:

0. **The contract**: `rpc.rs` at protocol 6 (the types above, the manifest
   test), the wire fixtures. Nothing uses them yet. The frontend starts from
   this commit.
1. **The daemon's side**: `Conn.web` from `Hello.web`; `summary` and its
   push; `web_attach`, `web_event`; the rescan call at start. The in-daemon
   server still runs.
2. **The server process**: `crew web-server`, its files, the control socket,
   the join table, the watches, the index connection, workspace connections
   as pipes. `crates/crew/src/web/` moves under it.
3. **The switch**: `crew web` talks to the server (`--all`, `--remove`,
   `--list`, `--restart`, `--stop`); the daemon's server, `Transport::Web`
   and `Session::web` are deleted; `web_open`, `web_close`, `web_pair` are
   `invalid` on a daemon. The Rust tests of §tests move.

### Member **web** (task `multi-ui`)

From step 0: `net/hub.ts`, `net/workspace.ts`, the route, the switcher, the
workspace states and banners, preferences split, the mock server with an
index and two workspaces, the tests. Rebuilt bundle with every commit (W8).

### Member **qa** (task `multi-qa`)

Integration; the live specs of §multi-tests; `README` "Web dashboard" (one
server, joining, `--all`, the setting and why the default is what it is,
`--remove`, `--stop`, tabs and restarts); `justfile` unchanged in its
recipes.

### The contract between vcs and web

`crates/crew/tests/fixtures/web/` at step 0, and these sentences:

- A page opens `ws://<host>/ws` for everything. `hello` without `workspace`
  is the index; with it, a workspace.
- Every `hello` reply carries `web_proof`.
- On the index: `workspaces`, `web_prefs`, `ping`; pushes `workspaces`,
  `shutdown`.
- On a workspace connection: protocol 5's calls and pushes with protocol 6's
  number; closed with 4001 when the daemon's side of it ended (which does
  not say the daemon stopped) and 4002 when the workspace was taken out;
  `hello` errors `not_found`, `unavailable`, `protocol`, `unauthorized`.
- A listing has a `rev`; the page applies only a greater one. What a
  workspace *is* comes from the listing, never from a connection: a close
  code or an error ends an attempt and, at most, makes the page ask for the
  listing. An entry has `generation` and `daemon` for the page's attempts
  to record.
- A text message has no line break.

### Risks to watch

- **Two servers for a while** (steps 1–2): the daemon's own must keep
  passing its tests until step 3 deletes it; no step leaves the tree with a
  dashboard that does not open.
- **The page's two levels**: the easy mistake is to keep one "connection
  state" and let a workspace's `shutdown` stop the page, or let a page-level
  stop leave a workspace connection open. The vitest table is the gate.
- **Timers in the server**: the design has none for discovery. A "just in
  case" registry poll would hide a missing `web_rescan` or a lost reconcile
  and break P20; the frozen-time tests are the gate. The two retries that
  exist (a published daemon that cannot be dialled; the daemon's own
  `web_rescan`) are armed by a failure and end with it.
- **State set from a connection**: anywhere the server writes `stopped`, or
  the page shows `not-running`, `removed` or `incompatible`, because a
  connection ended or answered an error rather than because the registry,
  or the listing, said so, the races of revisions 1 and 2 are back.
- **Removal that trusts the page**: closing the WebSocket is not the
  guarantee; the daemon's side being dropped before the answer is.
- **Removal that trusts `abort()`**: answering when the cancellation was
  asked for, not when the cancelled tasks and their children were awaited
  and the count is zero.
- **The watch is not a subscriber**: if it subscribed, the server would
  receive every push of every team and P20 would depend on the teams.
- **Tests that leave servers behind**: every harness stops its server; the
  directory check is the net, not the plan.
