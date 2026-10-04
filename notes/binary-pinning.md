# Design note: a team runs the crew binary its daemon runs

Author: `architect`, for task `mismatch-fix` (owner `vcs`; operator
m-ad4e8841ed7e), in answer to vcs's proposal m-ad7e81c5ee1e. Base: main
2f74dc36. Tracked by force like the rest of `notes/`.

**The bug.** A daemon hands its agents the *path* of the crew binary
(`DaemonOptions.crew_bin = current_exe()`): the MCP server command, the
hooks, the API-key helper, the pi extension, `{crew}` of a shell agent, the
sandbox launcher and shim. `cargo build` replaces the file at that path; the
next agent session that starts (after a compaction, a usage fallback, a
restart) runs the new binary against the old daemon, and every crew tool
fails.

**The rule.** For as long as a daemon lives, everything it hands out names a
private copy of the binary the daemon itself is running, and every agent of a
daemon was launched with that copy.

## Decisions

| # | decision |
|---|---|
| B1 | The pin is a **copy**, read from `/proc/self/exe` (the running inode, whatever became of its path), in the workspace's state directory: `<state>/bin/crew-<id>`, mode 0500, in a 0700 directory. Written under a temporary name, fsynced, renamed. No hard link. |
| B2 | `<id>` names the **build**, not the daemon: 16 hex digits of a hash of the running inode's `(dev, ino, size, mtime_ns)`. The same file run again gives the same name. |
| B3 | The pin is **not removed when the daemon exits**. A daemon that starts finds its own pin there (no copy) or makes it; after recovery it deletes every other file in `<state>/bin`. |
| B4 | An agent that outlived the previous daemon is **adopted only if it was launched with this daemon's pin**; otherwise it is restarted. The daemon records the pin name each agent was launched with. |
| B5 | Pinning happens where the crew binary becomes a daemon (`cli/daemonize.rs`, `run_foreground`), not in `daemon::start`: an in-process daemon of a test keeps the `crew_bin` it was given. |
| B6 | In a sandbox the pin is a **generated path of the session**, bound read-only at its own path inside the masked `.crew`, by descriptor and identity like every mount (`sandbox/pin.rs`). It is not a toolchain path under rule 2, and the `target/` exception of `Profile::toolchain` goes. |
| B7 | A pin that cannot be made does not stop the team: the daemon runs unpinned and says so (event `daemon.unpinned`, warn; a line in `crew status`). A pin that disappears is made again from `/proc/self/exe` (checked when an agent is launched and on the housekeeping tick). |
| B8 | A mismatch that still happens is **never silent**: every client's refusal names both sides and says "restart the team"; `crew mcp` starts and answers each tool call with it; a hook reports it without blocking the agent's CLI; the daemon emits a warn event naming the agent. |

## Why these, and not the proposal's variants

**A copy, not `/proc/<daemon pid>/exe`** (how the members got by today): not
visible in a sandbox's pid namespace; gone with the daemon; and a crew
process started through it reads its own path as `… (deleted)`. A real file
is the same path inside and outside a sandbox, on Linux and macOS.

**A copy, not a hard link.** A hard link shares its inode with the build
tree's file: its mode cannot be tightened without changing the original's,
`protected_hardlinks` refuses it for binaries the user does not own (a Nix
store, `/usr/bin`), it does not cross file systems, and whether its content
can change rests on `ETXTBSY` while something executes it. The copy is
cheap (**observed** on this host, ext4: the 15 MB release binary in under
10 ms; the 163 MB debug binary in 50 to 110 ms with its sync) and the rule
is one sentence: nothing outside crew can change the pin. `std::fs::copy`
shares extents where the file system can.

**Named by build, kept across the daemon's exit: because agents outlive
daemons.** With the herdr backend "agents … survive a crew daemon restart"
(`process/herdr.rs`) and are re-adopted (`daemon/supervisor/recovery.rs`).
Their MCP command and hooks hold the path they were launched with. A pin
named `crew-<pid>-<started_at>` and removed at exit would leave every
adopted agent with a path that no longer exists: each hook and each MCP
start failing with ENOENT, silently, in a case that works today. Named by
build and left in place:

- the same build started again: the same pin, no copy, adopted agents go on
  with the binary the new daemon runs;
- another build (an upgrade between two runs): the new daemon has another
  pin. An agent launched with the old one would now speak the old protocol
  to the new daemon, so it is not adopted but restarted with the new pin
  (B4; its session is resumed as for any agent that did not survive). That
  also repairs the herdr upgrade, which fails silently today. Once recovery
  has decided, nothing uses the old pin, and it is deleted (B3).

So the invariant "every agent of a daemon runs the daemon's binary" holds
for pty and herdr, and there is one pin per workspace at rest, two for the
length of one start after an upgrade. An agent recorded without a pin name
(launched before this change) counts as another build.

**The identity is the inode's, not the content's.** Hashing 163 MB at every
start of a debug daemon costs more than the copy it would save. `(dev, ino,
size, mtime_ns)` of the running inode is read in one `fstat`; a file that is
replaced has another inode, and one written in place another mtime. Two
identical rebuilds get two names: one needless copy and one round of
restarts, which is the safe direction.

**In the workspace, not shared per user.** A shared store
(`$XDG_STATE_HOME/crew/bin`) would save 15 MB per further workspace and
need reference counting between daemons to delete anything. The workspace's
daemon holds the workspace's lock, so it alone manages `<state>/bin`.

## What uses the pin

Everything the daemon writes down or starts with `crew_bin`: the agents'
MCP command, hooks, credential helper and extension (`agents/claude.rs`,
`codex.rs`, `pi.rs`, `shell.rs`); `crew sandbox-run` and the shim inside
(`Spec.crew_bin`); the probe; helpers and sidecars. Not: what the operator's
own shell runs (`crew attach`, `crew web` and the dashboard server it
starts), which is B8's case. A crew process started from the pin finds
itself through `current_exe()` as before, since the pin is an ordinary file.

## The sandbox (B6)

`Profile::toolchain` checks `crew_bin` against the protected paths, with an
exception for a binary in the workspace's `target/`. The pin is under
`<workspace>/.crew`, which is the session's own and masked for the agent by
a tmpfs; it is bound read-only on top of that mask, as the agent's
directory is. It has to be exactly the file the daemon made: a regular file
directly in `<state>/bin`, owned by this user, not writable by group or
others, with the `(dev, ino)` recorded when it was pinned (the mount is made
from a descriptor opened without following links and compared with that
identity, as for every mount). A workspace under `/tmp` therefore works
sandboxed whatever the protected set says about `/tmp`, and a binary in
`target/` no longer needs its exception: no agent is ever handed a path
outside `<state>/bin`.

## When there is no pin (B7)

The state directory is read-only, full, or on a file system mounted
`noexec` (`statvfs`, since a file's mode does not show it): the daemon
starts with the path it was started from, emits `daemon.unpinned` ("could
not keep a copy of the crew binary (<reason>): replacing <path> while this
team runs will break its agents' tools"), and `crew status` shows the same
line. B8 then says what happened if it does break.

A pin deleted under a running daemon (someone cleaned `.crew/bin`): the
daemon checks it (existence, identity) before every launch and on its 5 s
housekeeping tick, and writes it again from `/proc/self/exe`, with a warn
event. The running inode is always there to copy.

## Mismatch is said, not suffered (B8)

- **Any client** (`Client::operator`, `crew ctl`, the TUI): "crew <version>
  (protocol N) cannot talk to the daemon of <workspace>, which is crew
  <version> (protocol M): restart the team (`crew stop`, then `crew run`)".
- **`crew mcp`** completes its MCP handshake whatever the daemon answered
  and returns that sentence as the error of every tool call, so the model
  reads it and can say so.
- **`crew hook`** prints it and exits with the code its CLI treats as a
  failure that does not block the tool (for Claude Code: not 2).
- **The daemon**, on an agent's `hello` with another protocol: a warn event,
  at most one a minute per agent: "<agent>'s crew tools speak protocol N,
  this daemon speaks M: restart the agent". The operator sees it in the log
  of the TUI and the dashboard even when the agent says nothing. With the
  pin in place, restarting that agent gives it the right binary.
- **No client re-executes a binary named by a daemon.** It is tempting to
  let a newer `crew status` run the old daemon's pin by itself. A path taken
  from `<workspace>/.crew/daemon.json` is taken from a directory a cloned
  repository can supply; executing it would turn `cd` and `crew status` into
  running someone else's program.

## Tests (beyond the card's)

- The file at the daemon's path is replaced (renamed over) and, separately,
  removed, under a running daemon; an agent is restarted: its `crew mcp` is
  the pin (`/proc/<pid>/exe` of the MCP process is the pin's inode) and its
  tools work. The same sandboxed, with the workspace under `/tmp`.
- A second start from the same file makes no copy (the pin's inode and mtime
  are unchanged); a start from another file makes a second pin and, after
  recovery, the first is gone.
- herdr (or the backend stand-in with `adopt`): an agent launched with this
  build's pin is adopted; one launched with another pin, or with none
  recorded, is restarted and its config names the new pin.
- The pin is removed under a running daemon: it is back within a tick, with
  the same name, and a warn event; an agent launched in between gets it.
- `<state>/bin` cannot be written, and `noexec`: the team starts,
  `daemon.unpinned` is emitted, `crew status` says so.
- A file planted in `<state>/bin` under the pin's name with another inode is
  not used by a sandbox (the identity check) and is replaced.
- B8: the sentence from the CLI; `crew mcp` against a daemon of another
  protocol answers `tools/call` with it; the hook's exit code; the daemon's
  event, once a minute at most.
- An in-process daemon in a test makes no pin (B5).
