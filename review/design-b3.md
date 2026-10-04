# Design: closing B3 (operator impersonation)

Author: `lead`. Design only: no code was changed and nothing was committed.

**Status: revision 8.**
- rev-core approved revision 5 for implementation. That revision addressed its change requests on revision 3 (points 1–10) and revision 4 (the gate cycle, toolchain leaves, helper profiles, D2 root acquisition and the Nix probe).
- Revision 6 adds two things after rev-core's review of the sandbox core: §3.3 rule 6 (no writable entry over a read-only mount), and the requirement of Landlock ABI ≥ 9 for `required` mode. rev-core confirmed both (m-661c2a241175). The ABI floor must be the same in the probe, the launch spec and the shim, with no fallback to scanning alone.
- Revision 7 widens §3.4 rule D2 at the request of rev-core and rev-runtime (m-40ad3046fd7e): the daemon writes into an agent's tree only while no sandbox of that agent is alive, which includes its git operations and queue commands. The same exclusion now comes before publishing a new start's identity and configuration files (m-6db21ceb1714).
- Revision 8 (architect, during the jj work; orchestrator m-044d0b27aa92) amends §3.1 step 4: Landlock also handles `LANDLOCK_ACCESS_FS_REFER` and allows it beneath `/`. Without that, every rename or link across directories fails with `EXDEV` inside a sandbox, in the agent's own trees too; the sandbox member found it when jj could not store an object (m-a111740fb9fb).
- B3 stays open until gate G2 for teams that set `required`, and until G3 by default. Operator decisions 1–10 are settled (§6).

**Inputs.**
- Findings B3a–c in [SUMMARY.md](SUMMARY.md) §5, CORE-01 in [core.md](core.md), R4 in [runtime.md](runtime.md), and tui 3 in [core-lib-and-tui.md](core-lib-and-tui.md).
- The current source, read-only. Code is cited by quoted text, because the fix-run `base` commit is reformatted and line numbers differ.

**What the operator decided.**
1. A per-session operator token.
2. The daemon refuses a second `Hello` on a connection.
3. Agents run sandboxed, so that an agent running as the same OS user can neither read the token nor reach operator authority by any other route.

---

## 1. Threat model

**What an agent can do today.** Every agent runs as the operator's uid. Claude Code runs with `--dangerously-skip-permissions`, Codex with `--dangerously-bypass-approvals-and-sandbox` or through its app-server, and pi has no sandbox. So any agent can:

- **Become the operator at the daemon.**
  - It can send an operator `Hello` with no credential (`server.rs` `Role::Operator => Ok(Who::Operator),`).
  - It can re-introduce itself on its own authenticated connection (`handlers.rs` `conn.who = Some(who);`).
  - It can run any `crew` subcommand (`cli/ops.rs` `Target::connect` calls `Client::operator`).
- **Read every secret the user can read.** That includes `crew.db` and every agent's `identity.json`.
- **Reach user services through their sockets**, from which code can run outside any sandbox. On this host `$XDG_RUNTIME_DIR` holds the D-Bus session bus, Hyprland IPC, podman and pipewire sockets, and Xwayland listens on an abstract socket. Another route is the Nix daemon socket: if the operator is a Nix `trusted-user`, Nix settings such as `post-build-hook` make the daemon run code as root.
- **Write files that the operator's own tools later load or execute.** Examples: CLI configuration under `~`, `~/.cargo`, `.envrc` or `.claude/settings.json` in the main checkout, and git's per-worktree `config.worktree`.
- **Have the daemon execute agent-written code.** The daemon runs `git worktree add` itself, so a tracked hook directory (`core.hooksPath=.githooks`) makes `post-checkout` run from the new worktree. rev-core reproduced this. A worker can trigger it with `AgentSpawn`, and the orchestrator can trigger `integrate`.
- **Make the daemon read or write paths of its choice through symlinks** in directories both of them write. Examples: `<A>/briefing.md` is read and injected into Codex, and `write_private_file` writes through a fixed `<name>.tmp`.
- **Write the operator's terminal.** Its output reaches the operator unfiltered through `crew agent attach` and the inspector (H1). OSC 52 writes the operator's clipboard.

**What must hold afterwards.** No process running for an agent may:

- read the operator token;
- authenticate as the operator, or as another agent;
- read `crew.db` or another agent's identity;
- write another agent's worktree, the main checkout, or any file that an unsandboxed process later loads as configuration or executes;
- connect to any socket except its daemon socket (as an agent) and its own sidecar socket;
- signal or ptrace processes outside its sandbox;
- make an unsandboxed crew process execute agent-written code, or follow a path the agent chose;
- act on the operator's terminal beyond rendering text.

"A process running for an agent" means:
- the CLI itself, and every process it starts (tools, MCP servers, hooks, the crew bridge);
- the Codex app-server and the queue command;
- every git command that reads that agent's worktree, branch or admin data.

**Out of scope.**
- Code outside the sandbox: the operator's own tools, herdr panes, and teams run with `sandbox: off`.
- Kernel or bubblewrap escapes.
- An agent reading its own model credentials.
- Network egress, including localhost TCP services.
- Code that agents write and the operator later chooses to build or run.
- **Git integrity.**
  - Agents can write the shared `objects/` and `refs/`. They can corrupt objects, or move branches including the operator's `main`.
  - Moving branches changes no authority, and the operator reviews before landing.
  - A private object store per agent would close this; it is listed as a follow-up.
- **Confidentiality between agents.**
  - Committed and staged work of teammates can be read through the shared object store and `.git/worktrees/*/index`. crew shares this on purpose: `git_cherry_pick` and `git_merge` exchange it.
  - Only other agents' *uncommitted* files are private, because their worktree directories are not mounted.

---

## 2. Operator token and refusing a second `Hello`

### 2.1 Token lifecycle

1. **Lock first.** The daemon takes Area A's B7 single-instance lock: an exclusive `flock` on the state directory, held for the daemon's lifetime. A start that loses the lock exits without touching the token, the socket or the registration.
2. **Generate.** `crew_core::util::new_token()` (2×UUIDv4 from the OS RNG). It stays in daemon memory only: never in `crew.db`, logs, events, pushes or agent environments.
3. **Write.** Write `paths::operator_token_path(socket)`, which is the socket path with extension `.token` (`$XDG_RUNTIME_DIR/crew/<hash>.token`).
   - The file is 0600, inside the 0700 runtime directory.
   - It is written with the hardened `write_private_file`: a random temporary name opened `O_CREAT|O_EXCL|O_NOFOLLOW`, then a rename.
4. **Order of publication.** Write the token, then bind the socket, then write the registration (`daemon.json`), then start recovery spawns and accept connections. A client therefore never finds a socket whose token is missing.
5. **Rotation.** A new token is written on every start that wins the lock. Reconnecting clients re-read the file.
6. **Cleanup.**
   - At clean shutdown, and on a startup error after step 3, the daemon removes only what this start created: the token (if the file still holds its own token), the socket and the registration. It does this while still holding the lock.
   - A start that lost the lock removes nothing.

**Clients.** `Client::operator(socket, client)` in crew-core reads the token file and sends the token in `Hello.token`.
- Every operator path already goes through `Client::operator`: the TUI, `Target::connect`, `quick` and `attach`.
- `CREW_SOCKET` keeps working, because the token path is derived from the socket path.
- If the file is missing, the client fails with: `no operator token at <path>: operator commands must run as the user who started the daemon, outside agent sandboxes`.

### 2.2 Protocol

**Version.** `PROTOCOL_VERSION` goes **from 2 to 3**.
- fix-c's H8 commit (`area-c` `f863e54`) already took version 2. It added exact matching in the client (`check_hello`) and the manifest test `(PROTOCOL_VERSION, actual), (2, version_2)`. Area A adds exact matching in the daemon.
- The manifest test's expected version becomes 3; the enum lists don't change.

**Wire format.** `Hello` is unchanged; the operator token travels in `token`. `authenticate(Role::Operator)` answers `unauthorized` in two cases: `operator token required`, and `operator token does not match this daemon (it may have restarted; reconnect)`. The comparison uses `ct_eq`.

**Mixed versions.**
- A v2 client gets H8's mismatch error before authentication runs. No special case is needed.
- As with H8: after upgrading the linked binary, restart running teams.

### 2.3 Refusing a second `Hello`

The check is the first statement inside `if let Call::Hello(hello) = call {` in `daemon/handlers.rs`:

```rust
if let Some(who) = &conn.who {
    return Err(RpcError::forbidden(format!(
        "this connection is already authenticated as {}; open a new connection to change identity",
        who.label()
    )));
}
```

- A failed first `Hello` leaves `conn.who = None`, so a retry still works.
- No in-tree client sends two hellos on one connection.

### 2.4 The CLI side (B3b, B3c)

- **B3b.** `Client::operator` refuses before connecting when `CREW_IDENTITY` is set:

  > `this process runs as agent '<id>' (CREW_IDENTITY is set): operator commands are not available to agents; use your crew tools or crew ctl`

  - One check covers every subcommand, `attach` and the TUI.
  - This only gives honest mistakes a clear message. The token and the sandbox are the actual boundary.
- **B3c.** In `agents/codex.rs`, build the environment once with `CREW_IDENTITY` included, and use it for both the sidecar (`env: env.clone(),`) and the TUI launch.

---

## 3. Sandbox mechanism

**Recommendation.** Use **bubblewrap** for mount, PID, IPC, UTS and cgroup namespaces. Inside it, run a shim, **`crew sandbox-exec`**, before the real program. On a Linux build host, bwrap is the only practical way to *hide* paths and sockets that sit under directories the agent otherwise needs.

**Why bubblewrap, and not Landlock alone.**
- **Landlock cannot carve an exception out of a granted tree.** Hiding one file or folder inside a directory the agent otherwise needs takes a mount namespace.
  - Mount hiding also covers `/proc`, whole-tree process cleanup, and the paths the design exposes as single files.
- **Landlock is still needed for sockets, in two ways** (§3.3 rule 5):
  - Abstract sockets need the scoping added in ABI 6.
  - Named sockets need `LANDLOCK_ACCESS_FS_RESOLVE_UNIX`, added in ABI 9. Mounts are live views of host trees, so a mount namespace alone cannot stop a socket that appears in a mounted tree after launch. **`required` mode therefore needs Landlock ABI ≥ 9.**
- **Raw user namespaces written inside crew** would mean maintaining and auditing the mount setup ourselves.
- **The CLIs' own sandbox modes** confine only the commands the model runs, not the CLI itself, its file tools, MCP servers or hooks; pi has none. They can stay as an inner layer.

### 3.1 What the shim does

The shim runs single-threaded, and **fails closed**: any failed step exits 125 with a message.

1. `close_range(3, ~0, 0)`. No inherited descriptors survive except stdio and the PTY.
2. `prctl(PR_SET_NO_NEW_PRIVS, 1)`.
3. A seccomp filter that denies `ioctl(TIOCSTI)` and `ioctl(TIOCLINUX)` (as Flatpak does).
4. Landlock `restrict_self` with three parts:
   - `LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET | LANDLOCK_SCOPE_SIGNAL` (ABI ≥ 6). Abstract sockets are scoped by network namespace, and agents keep the host network, so this is the only thing that closes them.
   - `LANDLOCK_ACCESS_FS_RESOLVE_UNIX` (ABI ≥ 9), denied everywhere except the exact socket exceptions of §3.3 rule 5.
   - `LANDLOCK_ACCESS_FS_REFER` (ABI ≥ 2), handled and **allowed beneath `/`** (revision 8).
     - Why it is needed. REFER (rename or link into another directory) is the one right Landlock denies in every domain, whether the ruleset handles it or not. With the ruleset of revisions 6–7 a sandboxed process could not `rename` or `link` across directories anywhere: `git mv a d/a` failed with `EXDEV`, tools without a copy fallback failed, and jj could not write a commit (its git library renames `objects/.tmp…` into `objects/xx/`).
     - Why the root. Landlock is used here for sockets and signals only; what an agent may write is decided by the mounts of §3.2. A rule on `/` gives rename and link back the meaning they have without Landlock, and nothing more: both directories must still be writable (a read-only bind answers `EROFS`), and on one mount (the kernel refuses a move between two binds with `EXDEV`, so nothing moves between the worktree, `<A>/rw`, the object store and the sidecar directory).
     - What Landlock still refuses. A file may not be moved or linked to where it would gain a handled right: a socket made in the worktree cannot be moved or linked into a directory that carries the `RESOLVE_UNIX` exception.
     - Observed on Linux 7.1.8 (ABI 9) with `notes/landlock-refer-check/`: sockets-only ruleset: rename and link across directories `EXDEV`; with REFER beneath `/`: both succeed, a move into the socket directory is still `EXDEV`.
     - The rule is added by the shim itself, for every role and both VCS backends; it is not in the launch spec. Failing to add it exits 125.
5. `execve` of the program.

No thread is created before `restrict_self` succeeds. Raw `syscall`s go through `libc`, with no new crate.

**Per-launch enforcement.**
- A successful probe at load (bwrap present, user namespaces work, Landlock ABI ≥ 9) is necessary but not sufficient.
- Every launch must succeed through bwrap and the shim. Any other result marks the agent failed and is never retried unsandboxed.
- The launch spec carries the minimum ABI (9). If the kernel offers less at launch, for example after a downgrade, the shim exits 125.

**Requirements.** `bwrap` new enough for `--ro-bind-fd` and `--bind-fd`, unprivileged user namespaces, and a kernel with **Landlock ABI ≥ 9**. This host has all three once `bubblewrap` is installed: kernel 7.1.8 reports ABI 9 (measured by the sandbox member). Add `bubblewrap` to `shell.nix` and the README.

### 3.2 What a sandbox contains

The sandbox starts empty (`bwrap --clearenv … --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup-try --die-with-parent`), with the host network kept. Only what is listed is mounted. Notation: `<ws>` is the workspace, `<A>` is `<ws>/.crew/agents/<id>`, `<R>` is the daemon's runtime directory, and `<C>` is `$XDG_CACHE_HOME/crew/<hash>`.

| Mount | Access | Notes |
|---|---|---|
| System trees: `/usr /bin /sbin /lib*`, `/etc`, `/nix/store`, `/nix/var/nix/profiles`, `/run/current-system`, the resolved `/etc/resolv.conf` target; `/opt` only if listed in `read_only` | RO | Each tree must be root-owned and not writable by the operator (§3.3). **Not `/nix` as a whole**: that would expose `/nix/var/nix/daemon-socket`, and a read-only mount still allows `connect()`. |
| fresh `/proc`, minimal `/dev`, private `/tmp`, `$HOME` as tmpfs | — | `$HOME` is empty apart from the binds below |
| Trusted toolchain leaves (§3.3): `crew_bin` (one file), the agent CLI's resolved install directory, `~/.cargo/bin`, `RUSTUP_HOME`, `~/.gitconfig` (one file), and each PATH entry that passes validation | RO | Pinned descriptors. The parents are not bound, so `~/.cargo/registry`, other files in `~` and the rest of the main checkout stay hidden. |
| `<ws>/.crew/worktrees/<id>` | RW | The agent's worktree |
| `<ws>/.git/{objects,refs,logs,packed-refs}` | RW | Required for commits (integrity risk accepted in §1) |
| `<ws>/.git/worktrees/<id>/` | RW | git needs it for lock files, index and HEAD |
| `<ws>/.git/worktrees/<id>/{commondir,gitdir,config.worktree}` | **RO, bound over the files** | The daemon creates an empty `config.worktree` first. The agent can then change neither the repository this worktree points to, nor per-worktree config such as `core.fsmonitor` and `core.hooksPath`. |
| rest of `<ws>/.git` | RO | Common `config` and `hooks` cannot be changed |
| `<A>` | RO | Daemon-owned: `identity.json`, `settings.json`, `mcp.json`, `briefing.md`, mod, extension, `sidecar.log` |
| `<A>/rw/` | RW | Everything the CLI writes: its private home `<A>/rw/home`, and pi sessions |
| `<C>/cargo` | RW | `CARGO_HOME`, shared by the team's agents and never used by the operator |
| `<R>/<hash>.sock` | RW (socket) | The daemon socket, used as an agent. The `.token` beside it is not mounted. |
| `<R>/<hash>/<id>/` | RW | Sidecar socket directory. Short path (R6). |
| `workspace.sandbox.read_only` / `read_write` | as listed | Validated (§3.3); in `required` mode an invalid entry is a load error |

**Environment, as an allow-list.** The sandbox gets only:
- **Inherited:** `PATH` (filtered to the mounted entries), `LANG`, `LC_*`, `TZ`, `TERM`, `COLORTERM`, `USER`, `LOGNAME`, `SSL_CERT_FILE`, `NIX_SSL_CERT_FILE`.
- **Set by crew:** `HOME=<A>/rw/home`, `XDG_RUNTIME_DIR=/tmp/run`, `CARGO_HOME=<C>/cargo`, `RUSTUP_HOME`, `CREW_IDENTITY`, the per-CLI home variable (§3.5), and the runtime's endpoint variables from `LaunchSpec.env`.

Nothing else is passed: no `DBUS_*`, `DISPLAY`, `WAYLAND_*`, `SSH_AUTH_SOCK` or `HERDR_*`.

**Sessions and terminals.**
- PTY agents keep their own PTY as controlling terminal. TIOCSTI on it would only reach the agent itself, and the shim denies it anyway.
- Helpers with no PTY (git, sidecar, queue command) also get `--new-session` and stdin `/dev/null`.

**Nix.** No Nix daemon socket by default; the toolchain comes from PATH.
- Recommended setup: start crew from `nix-shell`. `shell.nix` already provides `gcc`, `pkg-config`, `git` and `rustup`, and their `/nix/store` paths are mounted RO, so `scripts/build-env` never needs `nix shell`.
- `workspace.sandbox.nix_daemon: true` mounts exactly `/nix/var/nix/daemon-socket/socket`.
  - It is accepted only if that same endpoint reports the user as **not** trusted. The probe is `nix store info --json --store unix:///nix/var/nix/daemon-socket/socket`, run with `NIX_REMOTE` and `NIX_CONFIG` cleared.
  - If the user is trusted, or the probe fails, `required` mode refuses to load.

#### Helper profiles: no agent authority

Git runs on behalf of no particular agent in two places: provisioning (populating a new worktree) and integration (merging every agent's branch). These run in a **helper profile**, not in an agent's sandbox:
- **What it gets:** the system trees and toolchain leaves (RO), the one worktree it works on (RW: the new worktree, or `.crew/worktrees/_integration`), and the `.git` mounts in §3.2.
- **What it gets nothing of:** the daemon socket, `<R>`, any `<A>`, `CREW_IDENTITY`, endpoint or model-credential variables, and the network (`--unshare-net`).
- **Why:** a hook from any agent's branch can run here, and it must not inherit the requester's identity or any other agent's.

An agent's own git operations (commit, status, diff, cherry-pick, merge in its worktree) run in that agent's own profile. Its own hooks then carry only its own identity, which is intended.

### 3.3 Mount validation

**Protected set S** (canonical paths):
- `<ws>`
- `<R>`
- the registry directory
- the operator's crew config directory
- each CLI home (`~/.claude`, `~/.claude.json`, `~/.codex`, `~/.pi`)
- `~/.ssh`, `~/.gnupg`, `~/.config`, `~/.local/share/keyrings`
- `~/.cargo` minus `bin/`
- `/run`, `/tmp`, `/nix/var/nix/daemon-socket`

`$HOME` is not in S, because toolchains live under it. It does count as an ancestor (next point).

**Rules for every bind source** (system trees, toolchain leaves, PATH entries, `read_only`/`read_write`):
1. **Not equal to, and not an ancestor of, any member of S, or of `$HOME`.** This rejects `/`, `/home`, `/home/<u>`, `<ws>` and the like.
2. **Not inside any member of S**, with two exceptions:
   - the daemon-generated paths of §3.2 (inside `<ws>/.crew` and `<R>`);
   - `crew_bin` as one RO file, when it is in the main checkout's `target/` (as on this host today).
3. **Canonicalized** before both checks, so aliases and PATH symlinks are judged by their target.
4. **PATH entries** that fail 1–3 are dropped and listed in a `warn` event. **Explicit `read_only`/`read_write` entries** that fail are a load error in `required` mode, never silently dropped.
5. **System trees** must be root-owned (`st_uid == 0`) and not writable by the operator at their top level. That ownership is a precondition only. It does **not** make a socket harmless, because a root-owned service can still authorize the caller's uid and groups.
   - Sockets are **deny-by-default**. The only sockets reachable in a sandbox are the exact exceptions: the daemon socket and the agent's own sidecar socket, plus the Nix daemon socket when `nix_daemon` is allowed.
   - **What enforces it:** the shim's Landlock `RESOLVE_UNIX` rule (§3.1 step 4). It also refuses a socket that some host process creates inside a mounted tree *after* launch. A scan cannot do that: a one-time scan of live host trees says nothing about later.
   - **Scan and mask, as a second layer.** At launch, every mounted tree except `/nix/store` is scanned for sockets (`S_IFSOCK`, including mount points). Each one found is masked (an empty file bound over it). If masking fails, the launch is rejected. `/nix/store` is not walked: it takes about two minutes on this host, and the Landlock rule already covers it.
   - Sandbox test 2 enforces this with no carve-out for root-owned sockets, and includes a socket created on the host after the agent started.
6. **Explicit `read_write` entries must not overlap any read-only mount, in either direction.** That covers system trees, toolchain leaves, PATH entries, the RO parts of `.git`, and `<A>`. An entry that equals one, sits inside one or contains one is a load error.
   - Without this, `read_write: [~/.rustup/toolchains/x/bin/cargo]` would mount a writable file over the read-only toolchain, and the agent could rewrite a tool the operator runs unsandboxed.
   - Explicit `read_only` entries inside an existing read-only mount are redundant and allowed.

**Pin what was validated.**
- After validation, each source is opened `O_PATH|O_NOFOLLOW` on its canonical path, with `openat2(RESOLVE_NO_SYMLINKS)`, and its identity (`st_dev`/`st_ino`) is checked against the validated path.
- That descriptor is passed to bwrap (`--ro-bind-fd` / `--bind-fd`), so the object mounted is the object validated.

**Mount order.** The `$HOME` tmpfs comes first, then the binds. No accepted bind can be an ancestor of a protected path, and no *explicit* writable bind can overlap a read-only one (rule 6), so a later mount cannot undo an exclusion or a read-only guarantee. The built-in writable mounts inside read-only ones stay as designed: `<A>/rw` inside `<A>`, and `objects/`, `refs/` and the agent's own `worktrees/<id>/` inside `.git`.

### 3.4 Everything the daemon executes or touches for an agent

Two rules apply throughout:
- **D1: execution.** Unsandboxed crew code never runs git or a CLI with its working directory or `-C` in an agent worktree. It never loads that worktree's admin data, and never runs agent-written hooks or config.
- **D2: files.**
  - **Root acquisition.** At start, the daemon opens `<ws>/.crew` as a directory descriptor and keeps it. No sandbox mounts `.crew` or `.crew/agents` writable. Every agent root is reached from that descriptor by `openat` with `O_DIRECTORY|O_NOFOLLOW`, one component at a time: `agents` → `<id>` → `rw` → `home`. An agent-replaceable path is never resolved from the top.
  - **Writes into agent-writable trees** (`<A>/rw`, worktrees) happen only while **no sandbox of that agent is alive**. The guarantee is that the daemon never writes there while any agent process can change the tree under it.
    - **What counts.** Every sandbox launched for the agent, including read-only helpers that can see its identity or settings. Each is launched separately: the PTY session, the sidecar, each of the agent's git operations, each queue command, and a helper that fills its worktree. A git or queue helper can outlive the PTY and sidecar sandboxes. A read-only mount prevents writes but still exposes a replacement token.
    - **Lifetime.** The daemon keeps a record of every such sandbox, from before launch until its outer process and every process in its sandbox are positively known gone.
      - Neither reaping nor an exit code from the outer bubblewrap monitor proves the sandbox is empty. Completion must follow the positively identified namespace init's lifetime, or an equivalent positive proof. If the init was never identified, an ordinary outer exit status does not clear the uncertainty.
      - The reason: bubblewrap's outer monitor returns the sandboxed program's status before the sandbox's init has finished waiting for its remaining descendants (rev-core, from `bubblewrap.c` v0.12.0, `monitor_child` and `do_init`). This holds for a normal exit as well as for a signal.
      - Unknown, unreadable, or incomplete retirement evidence blocks preparation and publication. It is retained across a failed or retried launch, removal and re-addition, and daemon recovery; restarting the daemon is not itself proof of retirement.
      - Any lease remains owned by the actual job or process through teardown, even if its awaiting RPC is cancelled or times out.
    - **Exclusion.** Preparation (seeding a home, placing credential destinations, any other daemon write there) is exclusive per agent. It starts only when no record remains. No sandbox of that agent starts while it runs. Preparations and launches of one agent are serialized.
  - **Publishing into `<A>` falls under the same exclusion.** A read-only mount is a live view: it stops the agent writing, but the agent still sees whatever the daemon writes there later.
    - Before the daemon creates or replaces anything in `<A>` for a new start (the directory itself, `identity.json`, `settings.json`, `mcp.json`, `briefing.md`, the mod or extension), every sandbox of the previous start must have been positively confirmed gone.
    - Otherwise a sandbox that should be gone could read the new token. After a remove and re-add under the same id, the old process's token is revoked, and it must not be handed the replacement.
    - If the previous sandboxes cannot be confirmed gone, nothing is published and the start fails.
  - **Write and read mechanics.** Writes are descriptor-relative beneath that root (`openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS|RESOLVE_NO_MAGICLINKS)`; `unlinkat` then `O_CREAT|O_EXCL|O_NOFOLLOW`), and special files are rejected. Reads go through `open_beneath` from the same root (the same flags, regular files only). Contents are treated as untrusted.
  - **Persistence.** Homes and sessions persist across restarts. Re-seeding replaces only the copied settings files, never session data.

| Operation (today's code) | After |
|---|---|
| Agent git: `gitops.rs` commit, diff, cherry-pick, merge, **status** | Run in the agent's sandbox, through the git argv-prefix hook |
| `ensure_repo` / `git::is_root` probes on an agent directory | Replaced by daemon knowledge (`workdir` is the worktree it provisioned). No git runs. |
| `ensure_worktree`: reuse check `out(&path, &["rev-parse", "--abbrev-ref", "HEAD"])` (runs *in* the worktree) | Read `.git/worktrees/<id>/HEAD` with `open_beneath`, and compare with `ref: refs/heads/agents/<id>` |
| `ensure_worktree`: `worktree add`, `worktree prune`, `show-ref` | Run in the main repo with the hardened invocation: `GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c core.untrackedCache=false`, using `worktree add --no-checkout`. No hook runs, because `post-checkout` does not run with `--no-checkout`. |
| Populating a new worktree | `git checkout` / `reset --hard` in a **helper profile** (no identity, no socket, no network), so smudge filters and the parent branch's hooks run with no authority |
| `integrate` (callable by the orchestrator: `Call::Integrate => { self.privileged(&who)?;`) | Its worktree is set up as above. Its merges run in a **helper profile** on `.crew/worktrees/_integration`, whoever calls it. |
| Codex sidecar (`start_sidecar`) | The agent's sandbox. `sidecar.log` is opened by the daemon in RO `<A>`, which the agent cannot redirect. |
| Queue command (`delivery.rs` `queue`, runs a Codex CLI against the sidecar socket) | The agent's sandbox, with the agent's `CODEX_HOME`. The socket directory is agent-writable, so it must not run unsandboxed. |
| Codex re-briefing (`context.rs` reads `briefing.md`) | Unchanged code: `briefing.md` now lives in RO `<A>`. A "compaction" detected from an agent-writable rollout is untrusted; its only effect is re-sending the real briefing. |
| Transcript, quota and rollout readers (`limits.rs`, transcript) | `open_beneath` under the agent's home. Results are shown to the operator only (`AgentTranscript` is `operator_only`) and are untrusted text (H1 sanitizing). |
| Home seeding and credential destinations | D2, before launch |
| `write_private_file` | Hardened: random temporary name, `O_EXCL|O_NOFOLLOW` |
| `claude -p /usage` (`daemon/limits.rs`), pi probe (`agents/pi.rs` `output_within`) | Operator-side and unsandboxed: operator binaries and configuration, with the working directory in the main checkout, which agents cannot write |

### 3.5 Per runtime

The rule: nothing an agent can write is loaded as configuration, or executed, by an unsandboxed process. Each CLI therefore gets a private home under `<A>/rw/home`. It is seeded by a one-way copy of the operator's settings (no hooks, plugins or extensions) under D2, and it persists across launches so that resume works.

| Runtime | Home | Model credentials (open question 4) | Readers |
|---|---|---|---|
| claude | `CLAUDE_CONFIG_DIR=<A>/rw/home/claude` (verify that `.claude.json` moves with it). `--settings`, `--mcp-config` and the mod stay in RO `<A>`. | `.credentials.json` bound RW at its destination (prepared under D2), or `CLAUDE_CODE_OAUTH_TOKEN` | transcript and quota readers, via `open_beneath` |
| codex (TUI, app-server, queue) | `CODEX_HOME=<A>/rw/home/codex` | `auth.json` bound RW, or `OPENAI_API_KEY` | rollout reader, via `open_beneath`. `models.rs` keeps reading the operator's `CODEX_HOME`. |
| pi | its agent directory moved under `<A>/rw/home` (verify the override variable), and `--session-dir <A>/rw/pi-sessions` | `auth.json` bound RW, or an env key | session reader, via `open_beneath` |
| shell / scripted | `HOME` only | — | — |

If a CLI replaces its credential file by rename, the single-file bind fails with `EBUSY`; such a CLI uses the env-token route instead.

### 3.6 Backends, workspace modes, and when the sandbox is unavailable

- **PTY.**
  - `LaunchSpec.sandbox: Option<SandboxProfile>`; `pty.rs` prefixes the bwrap argv.
  - Killing bwrap tears down its PID namespace, which kills every descendant: B9 for sandboxed agents. This must be verified for **both** the PTY and the sidecar launches, including descendants that started their own session or process group. "Gone" is established by rule D2's evidence (§3.4), not by the outer bwrap process ending.
- **herdr: refused when `sandbox: required`.** herdr's `agent.start` launches its own binary from `kind` (`"kind": kind,`, R7), so crew cannot put bwrap in front of it.
- **Isolation.**
  - Shared isolation is refused in `required` mode: agents would write the operator's checkout (B1, `.envrc`, `.claude/settings.json`).
  - **The orchestrator** gets `<ws>` read-only, with `<ws>/.crew` and `<ws>/.git` masked, its own `<A>` (RO) and `<A>/rw` (RW), and the shared mounts.
- **Configuration.** `workspace.sandbox: { mode: off | required, read_only, read_write, nix_daemon }`.
  - It lives in `WorkspaceConfig`, whose struct literals already use `..WorkspaceConfig::default()`.
  - **The default stays `off`** until the release gate in §5 flips it to `required`.
- **Sandbox unavailable** (no bwrap, user namespaces denied, Landlock ABI < 9). In `required` mode, refuse to start, with the cause and the fix in the error. For an old kernel the error gives the measured Landlock ABI and the fix: a newer kernel, or `sandbox: off`. `off` is an explicit opt-out, recorded as a `warn` event and shown in the dashboard header; B3 stays open under `off`. The operator decided this (decisions 1 and 10).
- **Two other routes to the operator's terminal:**
  - H1 is fixed in Area C (`area-c` `f9ac378`).
  - `crew agent attach` gets a **stateful** filter. It drops OSC except titles, DCS, APC, PM, SOS, terminal queries, and the 8-bit C1 forms. It works across chunk boundaries, and caps unterminated strings at a maximum length.

---

## 4. Tests

**Ground rules:**
- Each test is written failing first.
- Every negative test first runs a **positive control**. A host sentinel file and a host socket are created, and are shown to be reachable from an unsandboxed process, before the test checks that the agent cannot reach them.
- Auth tests use the current `PROTOCOL_VERSION`. Only the compatibility test uses 2.

**Running them:**
- No real CLI is needed. Scripted shell agents cover the general cases. Fake `codex`, `claude` and `pi` programs cover the sidecar, queue command, private homes and readers: small scripts the tests put on PATH, which listen on the sidecar socket and write rollout and transcript files.
- `tests/common` sets `workspace.sandbox.mode = off`.
- `tests/sandbox.rs` calls `require_sandbox()`. With `CREW_TEST_SANDBOX=require` (CI and `just test-sandbox`), a missing bwrap or Landlock fails the test; otherwise it is skipped with a printed reason.
- CI needs `bubblewrap` and unprivileged user namespaces. Ubuntu 24.04 needs the AppArmor userns setting.

| File | Tests |
|---|---|
| `crew-core/src/client.rs` | `operator_hello_carries_the_token_file`; `missing_token_file_is_a_clear_error`; `operator_client_refuses_under_crew_identity` (asserts that no connection was made) |
| `crew-core/src/paths.rs` | `write_private_file_does_not_follow_a_planted_tmp_symlink` (a sentinel outside stays unchanged); `open_beneath_refuses_symlinks_dotdot_and_special_files` |
| `crew/tests/operator_auth.rs`: raw JSON over a known socket path, with no CLI guard | positive control: `operator_with_token_succeeds`; `operator_without_token_is_refused`; `wrong_token_is_refused`; `failed_first_hello_then_valid_retry`; `second_hello_is_refused_for_operator_and_agent_and_authority_is_unchanged` (agent→operator with the correct token is refused, after which `AgentKill` is still forbidden); `agent_calls_succeed_on_the_same_socket`; `token_file_0600_and_rotates_on_restart` (old token refused, reconnect with the new one succeeds); `losing_concurrent_start_keeps_the_live_token`; `startup_failure_removes_only_its_own_token`; `token_never_leaks` (in snapshot, events, or a scripted agent's environment); compatibility: `protocol_2_client_gets_mismatch` |
| `crew/tests/team.rs` | `operator_subcommands_refuse_inside_an_agent` |
| `agents/codex.rs` | `sidecar_env_has_crew_identity` |
| `crate::sandbox` unit tests (no bwrap needed) | env allow-list; mount list; the token, `crew.db`, `<R>`, other `<A>` directories and other worktrees never appear; overlap rejection for an ancestor (`/`, `$HOME`, `<ws>`), a symlink alias, and a PATH entry resolving into S; an explicit `read_only` entry that fails is a load error; a `read_write` entry that equals, sits inside or contains a read-only mount is a load error (rule 6); the launch spec demands Landlock ABI 9; a fd-pinned source survives a symlink swap between validation and mount; helper profiles have no identity, socket, `<A>` or network; `nix_daemon` refused for a trusted user, probed at the exact mounted endpoint; **positive layout test**: a fixture `$HOME` with `~/.cargo/bin`, `~/.rustup`, `~/.local/share/claude/versions/x`, `~/.gitconfig` and `crew_bin` under `<ws>/target/release` yields exactly those leaves, and nothing else from `~` or `<ws>` |
| `git/mod.rs` | the hardened invocation carries the `-c` overrides; `worktree add` uses `--no-checkout` |
| `cli/attach.rs` | OSC 52, DCS and DA are dropped **when split at every byte boundary**; 8-bit C1 forms are dropped; an oversized unterminated OSC is dropped; SGR and cursor sequences are kept |
| `crew/tests/sandbox.rs` (real bwrap) | see the list below |

The `tests/sandbox.rs` cases:

1. `agent_cannot_read_operator_token_or_secrets`: the live token, `crew.db`, another agent's identity, and a sentinel under `$HOME` and `<ws>`.
2. `agent_cannot_reach_host_sockets`: from inside, `find / -type s` lists only the daemon socket and its own sidecar socket. A test-made sentinel socket in `$XDG_RUNTIME_DIR` and a test-made abstract socket are both refused. So is a socket the test creates inside a mounted read-only tree **after** the agent has started.
3. `agent_without_crew_identity_cannot_become_operator`: raw operator `Hello` with no token, and with a guessed token, on the exact socket path, while its agent calls succeed.
4. `agent_cannot_see_host_processes`: `/proc/<host pid>/{root,environ}` and `kill(<host pid>)` fail.
5. `no_inherited_descriptors_or_env_secrets`: a sentinel fd opened in the daemon is absent, and a sentinel environment variable is absent.
6. `agent_spawn_does_not_run_branch_hooks_unsandboxed`: a branch with `.githooks/post-checkout` writing a host sentinel, and the repo with `core.hooksPath=.githooks`. `AgentSpawn` from it leaves the sentinel unwritten outside the sandbox. This inverts rev-core's reproduction.
7. `integrate_hooks_have_no_authority`: a worker's branch carries a `post-merge` hook. During `integrate` called by the orchestrator, the hook tries to write a host sentinel, read any `identity.json` or `CREW_IDENTITY`, connect to the daemon socket (`crew ctl agents_list`, and a send as the orchestrator), and reach the network. All of these fail, and an outside control shows each target is reachable. The same is checked for population during `AgentSpawn`.
8. `git_status_ignores_agent_fsmonitor`: the agent tries to set `config.worktree` (`core.fsmonitor`) and fails (read-only). Daemon-side `GitStatus` and `GitCommit` then run no host-side command.
9. `compaction_does_not_read_through_symlinks`: the agent tries to replace `<A>/briefing.md` with a symlink to a sentinel and fails. A forged compaction makes the daemon re-send only the real briefing (fake codex).
10. `daemon_writes_do_not_follow_planted_links`: before a restart, the agent replaces `<A>/rw/home` itself with a symlink to an outside directory, and plants symlinks at credential destinations and `.tmp` names. Re-seeding leaves the outside sentinels unchanged. It does not run while any sandbox of that agent is alive. The test covers each kind: the PTY session, the sidecar, a git operation whose hook is still running after its caller was cancelled, and a queue command. It also covers a restart issued while the old sidecar is still exiting: the old sidecar's record is kept until it is positively confirmed gone. Session files survive the restart.
10a. `host_toolchain_layout_starts`: a scripted agent runs `cargo --version`, `git --version` and the fake CLI from the host-like layout of the positive layout test.
10b. `nothing_is_published_while_an_old_sandbox_lives`: an agent is removed and re-added under the same id while a sandbox of the old one cannot be retired (a helper that ignores termination, held in the test). The start fails, and `<A>/identity.json`, `settings.json` and `briefing.md` are unchanged, so the old sandbox never sees the new token. Once the old sandbox is positively confirmed gone, the start succeeds. A read-only helper that can only see the identity blocks publication in the same way.
10c. `unknown_retirement_blocks_preparation_and_publication`: four cases, each of which blocks seeding and publication until the sandbox is positively confirmed gone:
    - the outer bubblewrap process was killed by a signal and reaped, while a process inside is still alive;
    - the outer bubblewrap process exited normally with the program's status, while a descendant inside is still alive;
    - the retirement evidence is missing or unreadable;
    - the daemon was restarted while an old sandbox was still running. The restart alone does not release the record: after recovery the start is still refused.
10d. `a_cancelled_caller_does_not_release_its_job`: a git operation and a queue command whose RPC is cancelled or times out keep their record until they finish teardown. The same holds across a failed launch that is retried.
11. `sandboxed_agent_can_do_its_work`: `crew ctl`, `task_complete`, a write in its worktree, and `git_commit` with a `pre-commit` hook, which runs confined and cannot read the token.
12. `orchestrator_reads_but_cannot_write_the_checkout`.
13. `codex_sidecar_and_queue_run_sandboxed` (fake codex): the sidecar's `/proc/self/ns/mnt` differs from the host's; queue delivery works.
14. `kill_tears_down_pty_and_sidecar_trees`: includes a descendant that called `setsid`.
15. `required_refuses_without_bwrap`; `required_refuses_herdr_and_shared`; `off_starts_with_warning_event`.
16. `shim_fails_closed`: a forced Landlock failure means the agent does not start.

**Manual, by the operator:** `#[ignore]` tests in `crew/tests/real_cli.rs`, in which claude, codex and pi each start sandboxed, answer, refresh credentials and resume.

---

## 5. Rollout

**Where:** private clones of `crew-fixes/base.git` (tag `base`, `cargo fmt` applied), never the operator's checkout.

**When:** the patch series lands in the operator's tree **after the operator makes the initial commit**. Until then nothing is committed in the shared checkout; working there would share it, which is B1.

**Rules:**
- Each area's B3 tasks start after its non-B3 task (fix-a, fix-b, fix-c) is reviewed.
- One commit per change; the build and tests stay green at every commit.
- Tests come from §4, written failing first.

**Gates (stated explicitly):**
- **G1, auth active:** tag `b3-auth`. After that, operator authority requires the token; this closes B3a and B3b for honest clients.
- **G2, sandbox usable:** tag `b3-sandbox`. Every route in §3.4 is wrapped, including B-S5's private homes and readers, and all of `tests/sandbox.rs` (with the fake CLIs) passes under `CREW_TEST_SANDBOX=require`.
- **G3, default flipped:** `workspace.sandbox.mode` defaults to `required`. Only after G2 and the operator's manual real-CLI run. B3 is closed only from G3, or for teams that set `required` explicitly after G2.

### Order and tasks

| # | Task | Area / owner (reviewer) | Needs | Commits |
|---|---|---|---|---|
| 1 | **C-T1 Plumbing, no enforcement** | C / fix-tui (rev-tui) | fix-c | `paths::operator_token_path`; hardened `write_private_file`; `open_beneath`; `Client::operator` sends the token **if the file exists**; `WorkspaceConfig.sandbox` (default `off`) with validation |
| 2 | **A-T1 Daemon token and refusing a second `Hello`** | A / fix-core (rev-core) | fix-a (incl. B7 lock), merge C-T1 | token lifecycle §2.1 (written, **not yet enforced**); refuse a second `Hello`; `tests/common` sets `sandbox.mode = off` |
| 3 | **C-T2 Activation (one coordinated commit)** | C / fix-tui, with the daemon hunk agreed with fix-core (rev-tui **and** rev-core) | merge A-T1 | v3 bump plus manifest; the client requires the token; the daemon enforces it (`server.rs`); fake daemons in `crew-tui/src/net.rs` tests create token fixtures; `operator_auth.rs`. Then B3b: refuse under `CREW_IDENTITY`. **Tag `b3-auth` (G1).** |
| 4 | **B-T1 B3c and the attach filter** | B / fix-runtime (rev-runtime) | fix-b (may run alongside 1–3) | sidecar environment; stateful attach filter |
| 5 | **B-S1 Sandbox core** ⚠ | B / **separate strong-model member** (rev-core) | merge `b3-auth` | `crate::sandbox`: profile, allow-list, validation, fd pinning, argv; `crew sandbox-exec` shim (§3.1); probe; `shell.nix`/README |
| 6 | **B-S2 Git execution** | B / fix-runtime (rev-runtime) | B-S1 | hardened unsandboxed invocation; argv-prefix hook; `ensure_worktree` split (`--no-checkout`, HEAD read via `open_beneath`, a hook point for population); helper-profile builder; short sidecar socket directory (R6) |
| 7 | **B-S3 PTY wrapping** | B / fix-runtime (rev-runtime) | B-S1 | `LaunchSpec.sandbox` in `pty.rs`; `--new-session` for helpers. When **B-S2 and B-S3** are both reviewed, tag `area-b` as `b3-sandbox-core`. |
| 8 | **A-S1 Daemon wiring** ⚠ | A / fix-core (rev-core) | merge `b3-sandbox-core` | (1) profiles from daemon paths, `<A>` / `<A>/rw` split, D2 root descriptor, empty `config.worktree`. **Tag `b3-layout` after this commit.** (2) sidecar and queue command sandboxed; (3) gitops sandboxed, `ensure_repo` replaced, population in the helper profile; (4) integration in the helper profile; (5) refusals (unavailable, herdr, shared, Nix trust); (6) orchestrator RO |
| 9 | **B-S5 Private homes** ⚠ | B / fix-runtime, split per CLI if needed (rev-runtime) | merge `b3-layout` (can run in parallel with A-S1 (2)–(6)) | claude, codex, pi homes (one commit each); readers via `open_beneath` from the D2 root; pi sessions under `<A>/rw`; fake CLI programs |
| 10 | **A-S2 `tests/sandbox.rs`** | A / fix-core (rev-core) | written test-first alongside A-S1; **merge B-S5 before the full run** | cases 1–16 and `just test-sandbox`. When everything passes with `CREW_TEST_SANDBOX=require`: **tag `b3-sandbox` (G2).** |
| 11 | **G3 Flip the default** | C / fix-tui (rev-core) | G2, plus the operator's manual real-CLI run | `mode` defaults to `required` |
| 12 | **Final integration** | lead | all | merge branches; full baseline including `CREW_TEST_SANDBOX=require` (bwrap from `nix shell nixpkgs#bubblewrap`); patch series; test-apply on a copy of the operator's current tree |

**Flagged for sizing:**
- **B-S1** is the largest security-critical piece. Staff it separately and give it the strictest review.
- **A-S1** touches every launch path; keep its six commits separate.
- **B-S5** needs the real CLIs for credential refresh and resume. CI covers it only through fake programs.

---

### Implementation deviations approved during B-S1 (by rev-core, m-d066b69105a8)

- **Descriptors are reopened and revalidated in two stages.** portable-pty closes descriptors ≥ 3, so the pinned source descriptors (§3.3) are reopened and revalidated by `sandbox-run` before bwrap runs, with tests for the pinned descriptors.
- **Secrets travel in the environment passed to `execve`**, never in bwrap's argv (which `/proc/<pid>/cmdline` exposes). The launcher's own environment is clean.
- **The `/etc/resolv.conf` target is allowed as one exact file**, an exception to rule 2 of §3.3.
- **Not approved: skipping the socket scan of `/nix/store`** on the grounds that NAR archives cannot contain sockets. The deny-by-default rule (§3.3 rule 5) stands. The store is left unscanned only because the Landlock `RESOLVE_UNIX` rule covers it.

### Design changes after the B-S1 review (rev-core, m-12b85562ae9d)

rev-core reproduced two gaps at `sandbox` `6e434ea`. Both were gaps in this design, not only in the code:

- **A writable entry could overlay a read-only mount.** §3.3 now has rule 6: an explicit `read_write` entry may not overlap any read-only mount, in either direction.
- **A launch-time scan cannot enforce deny-by-default sockets** on live host trees: a socket created after the scan is reachable. Enforcement is now the Landlock `RESOLVE_UNIX` rule, so `required` mode needs Landlock ABI ≥ 9 (§3, §3.1, §3.3 rule 5, §3.6). The scan stays as a second layer.
  - Consequence for the code: nothing below ABI 9 runs, so the ABI 6–8 store walk and its cache (`sandbox` `e91ac6e`) are deleted (decision 10).

### Design change after the daemon-wiring review (rev-core and rev-runtime, m-40ad3046fd7e)

- **Rule D2 named too few writers.** It said the daemon writes into an agent's tree only after the PTY and sidecar sandboxes have exited. An agent's git operations and queue commands are separate sandboxes that also mount `<A>/rw` writable, and they can outlive both, even after their caller was cancelled.
- §3.4 rule D2 now covers every sandbox launched for the agent, read-only helpers included. It requires a record of each until it is positively confirmed gone, and makes preparation exclusive per agent. Sandbox test 10 covers each kind.
- **The exclusion also has to come before publishing into `<A>`** (rev-runtime, relayed in m-6db21ceb1714). The daemon replaced the identity and settings files before the old sandboxes were retired. A read-only mount is a live view, so after a remove and re-add an old sandbox could read the new token. D2 now requires every sandbox of the previous start to be positively confirmed gone before anything in `<A>` is created or replaced; if that cannot be confirmed, nothing is published and the start fails. Sandbox test 10b covers it.
- **"Reaped" was too weak as evidence** (rev-core, m-7a4e54acc076 and m-483d0b30deaf). The outer bubblewrap process ending does not show that every process inside is gone, whether a signal killed it or it exited normally. D2 now asks for positive evidence tied to the sandbox's own init process, treats unknown or unreadable evidence as blocking, keeps it across retries, re-adds and daemon recovery, and keeps a job's lease through teardown even when its caller was cancelled. Sandbox tests 10c and 10d cover the unknown-state, recovery and cancellation cases.
- This is within the guarantee already approved (no daemon write while an agent process can change the tree). It needs no new operator decision.

## 6. Operator decisions

**Decided:** the operator accepted recommendations 1–9 below as written (relayed by the orchestrator, message m-0da627b49d53). Build to them with no further checking. G3 (flipping the default to `required`) is not part of this fix run; it waits for the operator's manual real-CLI run.

Decision 10 was added after the B-S1 review and is settled under decision 1.

1. **No sandbox available.**
   - **Recommend:** refuse to start (fail closed). `sandbox: off` stays an explicit, visible opt-out.
2. **Shared isolation.**
   - **Recommend:** not allowed with `required`. Sandboxed teams in this checkout therefore wait for its first commit.
3. **The orchestrator and the main checkout.**
   - **Recommend:** read-only. If it must write, it gets its own worktree.
4. **Model credentials.**
   - **Recommend:** bind each CLI's credential file RW where refresh works through the bind. Otherwise use env tokens from 0600 files the operator creates (`claude setup-token`, `OPENAI_API_KEY`, pi's key).
5. **Operator configuration in agent homes.**
   - **Recommend:** a one-way copy of settings and model configuration only: no hooks, plugins or extensions unless listed.
6. **herdr.**
   - **Recommend:** refuse sandboxed agents until herdr can launch a given argv. That is the same change R7 needs.
7. **Network.**
   - **Recommend:** keep the host network; localhost TCP is a documented residual risk. A filtered network namespace comes later.
8. **Nix.**
   - **Recommend:** no daemon socket. Start crew from `nix-shell` so the toolchain comes from `/nix/store` on PATH. Allow `nix_daemon: true` only for a non-trusted user.
9. **Git integrity.**
   - **Recommend:** accept shared `objects/` and `refs/` for now, with integration showing which branches moved. A private object store per agent is a follow-up, if moving `main` matters.
10. **Kernels with Landlock ABI 6–8: settled under decision 1.** On these kernels crew cannot stop an agent from connecting to a socket that appears in a mounted folder after the agent started, so `required` cannot give its guarantee there.
    - **Decided:** `required` refuses to start. The question the operator accepted for decision 1 already named an old kernel as a case of "sandbox unavailable" (orchestrator, m-c69c6ced16d0). There is no weaker mode.
    - The error names the cause and the fix: the Landlock ABI is below 9, with the measured value; use a newer kernel, or set `sandbox: off`.
    - The ABI 6–8 code path is deleted, the store walk and its cache included.
    - This host is unaffected: it has ABI 9. At G3, once `required` is the default, crew refuses to start agents on older kernels unless the team sets `sandbox: off`. The orchestrator lists this in the final report, so the operator can still reverse it.
