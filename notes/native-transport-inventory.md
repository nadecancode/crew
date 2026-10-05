# Native client transport inventory

Task: `native-transport-inventory`. Source checkpoint:
`6049cdf63e36e303327235834f23a798aa5a82c1`. Targets: native Linux desktop,
macOS desktop and iOS, with shared logic and platform UI. This inventory
changes documentation only. Remote access and native onboarding below are
proposals, not existing product features or authorization to expose a listener.

**Selected direction:** operator decision `m-8b3375948f11`, relayed by the
orchestrator in `m-cfe027e187f8`, approves a scoped Crew gateway with
device-specific credentials, selected workspace access and individual
revocation. A full-account SSH design is rejected. The gateway's design is a
separate approval gate; this inventory does not implement or configure it.

**Existing v6 transport is a local Unix daemon socket or a loopback-only
dashboard WebSocket with pairing, server proof and joined-workspace scope;
native remote access is assigned to a scoped Crew gateway whose design
remains a separate approval gate.**

The phone's `127.0.0.1`, `::1` and `.localhost` refer to the phone. A LAN
address or a VPN does not make the host's loopback listener reachable. The
current daemon, control socket and dashboard offer no direct phone connection.

## Current contracts

Source references below use `path:line` at the checkpoint. The transport,
authentication, DTO and fixture files inspected are identical in the runtime
worktree based on `a700f29e`; the later main changes concern presentation,
TUI work labels and review evidence, not these contracts.

| Surface | Framing and access | Authority |
| --- | --- | --- |
| Daemon Unix socket | NDJSON `Request` / `ServerFrame`; private runtime directory, socket mode `0600`, same-user peer credentials and token-authenticated `hello` | Operator token gives full local operator authority; agent identity token gives that agent's authority |
| Dashboard HTTP / `/ws` | Plain HTTP/WS on `127.0.0.1` and `::1`; one JSON object per WebSocket text message; exact random Host and Origin | `web_pair` exchanges a code for a web session; `hello` authenticates that session; workspace access is limited to joined memberships |
| Dashboard control Unix socket | NDJSON on `web.sock`; same-user peer credentials and distinct `web.token` | Open/pair/join/remove/stop/rescan control; no browser or agent has this credential |

Evidence: `crates/crew-core/src/rpc.rs:1,23,604,672`;
`crates/crew-core/src/types.rs:19`;
`crates/crew/src/daemon/server.rs:239,298`;
`crates/crew/src/web/mod.rs:656`;
`crates/crew/src/web/page.rs:1`;
`crates/crew/src/web/control.rs:39,87`.

### Local identity and discovery

- `paths::runtime_dir()` uses `$XDG_RUNTIME_DIR/crew`, falling back to
  `/tmp/crew-<uid>` when unset or too long. Each workspace socket has a
  16-hex-digit stem derived from its state directory. `.crew/daemon.json`
  and the per-user session registry publish discovery metadata.
- `paths::discover()` considers an explicit workspace, `CREW_SOCKET`,
  `CREW_IDENTITY`, a parent `.crew/`, then the registry, optionally narrowed
  by project. Ambiguity requires selection. `list_sessions()` also prunes
  stale entries; it is not a pure read-only discovery function.
- `Client::operator()` refuses when `CREW_IDENTITY` is present, even if its
  identity file is invalid. It reads the operator token beside the socket;
  token directories must belong to this user and not be writable by others,
  token files must be regular, owned by this user and mode `0600`. It never
  creates or repairs missing credentials. The daemon token rotates on restart.
- `Client::operator()` sends `web: false`. Its authenticated local connection
  has no web capability ceiling and no pairing step. Successful `hello`
  fixes identity for the connection; another `hello` cannot elevate it.
- The dashboard hub validates a discovered socket against the socket derived
  from the registry entry's state directory and checks socket ownership.
  A page cannot supply a filesystem path for the server to dial.

Evidence: `crates/crew-core/src/paths.rs:389,410,426,438,505,576,666,695`;
`crates/crew-core/src/client.rs:227,269`;
`crates/crew/src/web/daemons.rs:56,93`;
`crates/crew/src/daemon/handlers.rs:163`;
`crates/crew/tests/operator_auth.rs:301`.

### Dashboard handshake and capability boundaries

The only accepted Host is `<random-label>.localhost:<port>`. `/ws` requires
Origin `http://<random-label>.localhost:<port>`; absent or different Origin
fails. The native transport must send both unchanged even when its TCP
destination is a literal loopback address or a tunnel stream. The existing
non-browser integration client does exactly this, avoiding DNS entirely
(`crates/crew/tests/web_server.rs:248`). Host and Origin are compatibility
checks for a native client, **not native client authentication**: a native
process can choose either header. Origin still protects browser connections.

1. On a fresh socket, `web_pair {code}` returns `WebPaired {token, verifier}`.
   Codes expire after 120 s and work once; at most 8 codes are outstanding.
2. `hello {role: "operator", token, client, protocol: 6}` opens an index
   connection. Adding `workspace: <16-hex-id>` opens a workspace connection.
3. The client checks `SHA-256(hello.web_proof) == verifier` before accepting
   application state, applying pushes or sending application calls. The
   proof secret is kept by the server; browser storage holds only token and
   verifier. Keep the proof check on every index, workspace and terminal socket.
4. The index supports `workspaces`, dashboard `web_prefs` and `ping`.
   Workspace connections are admitted under a membership generation and
   forwarded to that daemon with `web: true`, regardless of fields a page
   tries to supply. The operator daemon token stays on the host.

There are at most 16 sessions and 64 authenticated sockets. Pairing at the
session ceiling evicts the oldest idle session; if all sessions have open
connections, pairing fails. Sessions and proof secrets exist only in server
memory. Before authentication, connections have 5 s and a 4 KiB message
limit; authenticated frames are bounded by 1 MiB. A WebSocket text message
containing a literal line break closes with 1008 so it cannot become two
Unix requests. JSON string escapes for message bodies remain valid. Server
pings run every 20 s and a connection silent for 40 s closes.

A web session can inspect and mutate **every joined workspace**, including
terminal input and stopping a team. Joining one workspace does not issue a
credential restricted to it. No read-only role or per-device workspace grant
exists. `load_team`, `web_attach` and `web_event` are excluded for
`web: true`. `web_open`, `web_close` and `web_rescan` belong to the control
socket, not to any WebSocket. Stopping a team (`shutdown`) differs from
stopping the shared dashboard (`web_close`).

Evidence: `crates/crew/src/web/http.rs:142`;
`crates/crew/src/web/mod.rs:41,178,246`;
`crates/crew/src/web/ws.rs:19`;
`crates/crew/src/web/page.rs:211,323,401,437`;
`crates/crew/src/web/daemons.rs:131`;
`crates/crew/src/daemon/handlers.rs:53,241,638`;
`web/src/net/conn.ts:330` and `web/src/net/session.ts:113`.

### Version skew and information before authentication

Both transports require an exact protocol number, currently 6. Additive
optional fields can be compatible; changed enum values, removed fields or
changed meanings require a protocol bump. An app must expose an actionable
incompatibility state, not reinterpret another protocol or silently restart a
team/server. The daemon's mismatch error is produced before token validation
and includes version, protocol and absolute workspace path. On the current
Unix transport it can reach only an accepted same-user peer; a future raw
Unix relay would extend that disclosure to its clients. The dashboard's
pre-authentication mismatch names protocol numbers without that workspace
data. Its static bundle and `ping` are also available before authentication
to a caller satisfying Host/Origin checks. Random labels are not secrets.

Evidence: `crates/crew-core/src/rpc.rs:20`;
`crates/crew/src/daemon/handlers.rs:151,174`;
`crates/crew/src/web/page.rs:238`;
`crates/crew/src/web/http.rs:142`.

### State ordering, terminal and lifecycle

- A hub listing is authoritative about membership and availability. Apply
  only listings with greater `rev`; preserve membership `generation` and
  `DaemonRef {pid, started_at}`. A late attempt must not change a newer
  workspace's state. Refusals or 4002 trigger a listing refresh; they do not
  prove what the latest membership is.
- Subscribe before the initial snapshot. While a snapshot request is in
  flight, drop pushes older than that snapshot; replace state and bounded
  history rings on its reply, then apply ordered pushes. `resync` repeats
  the barrier. There is no durable global push cursor or resume offset.
  History reads are bounded recent-history queries, not general cursor paging.
- A workspace shutdown or close 4001 ends that workspace attempt; the
  listing determines whether to wait or reconnect. Removal revokes pending
  dials, workspace/terminal connections and the watch before returning;
  close 4002 identifies the ended membership. Registry presence alone never
  joins a workspace. Rejoining creates a new generation.
- Browser index retry starts at 250 ms, doubles to 5 s with 20% jitter and
  stops after 30 s without its server. Workspace retry continues while a
  live index says `running`. Reconnect never replays a mutation: an action
  may have executed before its reply was lost. Only operations with explicit
  deduplication, such as `send` with a caller-chosen message id, support that
  kind of safe retry. Request ids correlate replies, not deduplicate actions.
- A terminal uses a separate proven workspace connection: `attach` then
  base64 `pty` pushes; input/resize use `attach_input`/`attach_resize` with
  no reply. `attach` replaces that connection's state subscription. Do not
  multiplex it onto the shared state socket. Terminal output remains untrusted
  bytes; native emulators must preserve clipboard/title/link restrictions.
- Server stop revokes codes and sessions before closing connections and
  releasing listeners. A restart gets a new label and invalidates all web
  credentials. An unasked-for server stops after 60 s. Default idle linger
  is 600 s without a running joined daemon;
  configurable by the user/global web settings. Closing a client must not
  stop the daemon. There is no operator RPC to revoke one web session.
  Local credential deletion is not host-side revocation.

Evidence: `web/src/net/hub.ts:1`;
`web/src/net/workspace.ts:1`;
`web/src/net/conn.ts:62,367`;
`crates/crew/src/daemon/server.rs:120,333,343`;
`crates/crew-core/src/rpc.rs:92`;
`crates/crew/src/web/hub/tests.rs:818,897,942,1601`;
`web/src/term/Terminal.ts:1`;
`crates/crew/src/daemon/handlers.rs:604`;
`crates/crew/src/web/mod.rs:724`;
`crates/crew/src/web/process.rs:40,42`.

Current client request deadlines agree across `web/src/net/conn.ts:62` and
`crates/crew-tui/src/net.rs:29`: default 3,000 ms; `agent_kill`,
`agent_restart`, `agent_restart_fresh`, `agent_resume` and `agent_set_model`
30,000 ms; `resume` and `shutdown` 90,000 ms; `models_list` 20,000 ms.
These are request deadlines, separate from the web authentication deadline,
SSH connection deadline and retry policy. Preserve them in shared policy;
slower operations and terminal input must not block ordinary reads.

The web state socket fetches `snapshot {messages: 300, events: 1000}` on
the subscribed connection and uses the barrier above. TUI subscribes on one
connection and fetches `snapshot {messages: 300, events: 400}` on a separate
one, relying on idempotent overlapping reducers. Do not transplant that
separate-socket topology into a reducer assuming the web barrier. A native
state adapter should specify its mode explicitly; terminal attachment uses
another connection in either mode. Evidence: `web/src/net/workspace.ts:43,167`;
`crates/crew-tui/src/net.rs:29,87`.

## Reuse and desktop paths

Reusable wire values are `Call`, `Request`, `ServerFrame`, `RpcError`,
`HelloInfo`, `Snapshot`, `Push`, `WebInfo`, `WebPaired`, `Workspaces`,
`WorkspaceEntry`, `DaemonRef`, `WorkspaceSummary` and the agent/task/message/
event/usage/transcript/VCS types in `crew-core`. Preserve enum tags, optional
field defaults, timestamps, 64-bit ids/revisions and unknown additive push
handling. The checked fixtures in `crates/crew/tests/fixtures/web/` are
language-neutral examples, validated by Rust serialization and TypeScript
decoders. State/action fixture work is owned by `native-state-fixtures` and
`native-action-fixtures`; this document does not define another schema.

`crew-core` currently combines DTOs with Unix paths, filesystem/configuration
and a blocking Unix client; it is not an isolated portable client engine.
`rpc.rs` also imports configuration types. `web/src/net/conn.ts` provides
injectable `Env`, handshake/proof, reply correlation and timeout policy;
`workspace.ts` provides `Sink`, attempts and the snapshot barrier. TUI
`net.rs` uses Unix `Client` and separates subscriber, fast commands, slow
actions and keys into workers. These are extraction seams, not existing
Swift bindings. Detailed source/dependency ownership belongs to
`native-core-inventory` and `native-architecture`.

| Target | Feasible path and setup | Reuse and verification boundary |
| --- | --- | --- |
| Linux desktop, trusted same-user app outside an app sandbox | Discover and select an existing daemon; use `Client::operator()` on its Unix socket. No web server, pairing, extra listener or token copy needed. An optional separately chosen local control operation can use `web.sock` and its own token. | Reuse Rust DTOs and current Unix client directly; put blocking I/O on workers. Full operator authority, including `load_team`; this process is in the existing trusted operator class. Existing Linux daemon/auth tests pass. A native UI/binding has not been built by this card. |
| Linux desktop, confined package | Runtime-directory visibility, environment/path translation, socket access and private token access depend on the selected confinement. | Shared protocol/state can remain identical. Flatpak/Snap or another confined packaging path is unverified; do not assume it can read the host runtime directory. A helper or trusted handoff would need its own reviewed contract. |
| macOS desktop, trusted local app | Same Unix protocol is a candidate if a compatible daemon runs locally and the app can read that daemon's private credential. An authenticated remote channel to an existing Linux host is a separate candidate. | Reuse portable DTO/state logic, then provide a macOS channel/path/binding adapter. Local daemon portability, runtime path discovery, same-user checks and UI binding are **Apple verification deferred by operator**. Linux kernel sandbox support is not a macOS sandbox implementation. |
| macOS desktop, App Sandbox | Direct access to an external daemon's runtime directory/token is not established; a network-client entitlement alone would not grant file access. | Shared DTO/state logic still applies. Socket/token visibility, helper/IPC arrangement, signing and entitlements are **Apple verification deferred by operator**. Apple's [App Sandbox file-access contract](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox) makes confinement a distinct adapter concern. |
| iOS | Selected direction is an authenticated scoped Crew gateway; never try the host's Unix socket or interpret the dashboard's loopback URL as a remote address. | Reuse protocol/state/connection policy; adapt network channel, secure storage and app lifecycle. No current remote/onboarding adapter or Swift binding exists. **Apple verification deferred by operator** for the eventual native implementation. |

The operator explicitly permits later Apple-runner checks; this deferral is
not a blocker to authorized source/project work after the architecture
contract. These are current-source runner commands, not claims of passing
Apple builds:

```sh
# Run in the repository on the macOS runner.
cargo test -p crew-core --lib rpc::compatibility_tests
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
xcrun --sdk iphoneos --show-sdk-path
xcrun --sdk iphonesimulator --show-sdk-path
cargo check -p crew-core --target aarch64-apple-ios
cargo check -p crew-core --target aarch64-apple-ios-sim
```

Once the N1 `crew-protocol` and `crew-client` extraction is integrated, run
its protocol/client tests on macOS and compile those same pure packages for
device and simulator targets. **Apple verification deferred by operator**:

```sh
cargo test -p crew-protocol -p crew-client
cargo check -p crew-protocol -p crew-client --target aarch64-apple-ios
cargo check -p crew-protocol -p crew-client --target aarch64-apple-ios-sim
```

Expected result: protocol/client tests pass and both target checks compile.
These package-specific commands are conditional on N1 landing; the packages
do not exist at the reviewed source checkpoint.

Expected gates: protocol fixtures pass; both SDK paths exist; each selected
core target compiles or yields an explicit portability finding before it is
described as supported. These commands do not verify a native app, sandbox,
signing or physical network behavior. Reproducible app build/simulator/device
commands must name the real projects and tests once architecture supplies
them; there is no native Xcode project at this checkpoint.

## Native remote paths and selected gateway

The scoped gateway is approved as a product direction, with its precise
security and platform design pending a separate gate. SSH is retained below
only as a feasibility comparison; it is not the selected native design. Both
require trusted native distribution and authenticated host identity. Neither
path is implemented or configured here. The daemon and existing dashboard
listeners remain local under either design.

### SSH channel to the host's loopback dashboard

An SSH client can open `direct-tcpip` to **the SSH host's**
`127.0.0.1:<dashboard-port>` and carry the existing WebSocket upgrade and
frames over that byte stream. The destination differs from the logical
dashboard Host/Origin. This follows [RFC 4254 §7.2](https://www.rfc-editor.org/rfc/rfc4254#section-7.2);
it is a feasibility inference from the protocol and the existing Linux
non-browser WS tests, not a tested iOS transport. Open a channel per index,
workspace and active terminal; no phone listener is necessary.

**Rejected full-account SSH model.** An authenticated fixed exec operation,
`crew -C <workspace> web --no-open --json`, could obtain `WebInfo` and a
pairing code without pasting browser output. However, its account key could
also execute other commands and read operator/control token files. The app's
fixed command would not restrict a stolen key. This expands trust to the
operator's whole account and does not meet the selected device/workspace
boundary. Fixed commands and correctly shell-quoted workspace arguments
would be necessary for such a model; they would not make it scoped. It is
not an implementation option for the approved gateway.

**Rejected forwarding-only alternative.** A pinned-host SSH account could
be limited to a fixed loopback destination with `AllowTcpForwarding local`,
`PermitOpen`, `MaxSessions 0` and denied Unix/agent/X11 forwarding. Per-key
`restrict` requires explicit `port-forwarding` to re-enable that one path.
These are operator-admin SSH setup requirements, not settings supplied by
Crew. [OpenSSH server policy](https://man.openbsd.org/sshd_config),
[OpenSSH key restrictions](https://man.openbsd.org/sshd.8#AUTHORIZED_KEYS_FILE_FORMAT).
This could authenticate an SSH key and carry plain WS inside encryption,
but the existing Crew web session is not device-bound, still accesses every
joined workspace and has no individual revocation. Deleting the SSH key
does not terminate an existing channel or revoke its web session. It meets
none of the three required Crew guarantees and is rejected as the native
gateway design. It also lacks a trusted metadata/code handoff today. The
server's label changes at every start; a fixed `--port` and forwarding rule
cannot replace the incarnation proof. No SSH policy or adapter was installed
or tested here. Source for default port selection remains
`crates/crew-core/src/paths.rs:518` and `crates/crew/src/web/mod.rs:657`.

### Selected authenticated scoped gateway

A separate host service could terminate authenticated TLS and forward only
`/ws` to the loopback dashboard, applying the exact upstream Host/Origin
and preserving frames and web capability limits. A transparent TLS relay
alone would still grant all joined workspaces through the existing web
session; it would not meet the selected scoped gateway requirement. The
service must authenticate a device and enforce its selected workspace grants
itself. This adds a network-facing service and a trust boundary. Design,
implementation and deployment are outside this inventory, with a separate
architecture/security approval gate.

| Approved requirement | Current implementation gap | Gateway responsibility |
| --- | --- | --- |
| Device-specific credential | None. `WebSession` contains token, secret and last hello; pairing creates a bearer session without a device identity (`crates/crew/src/web/mod.rs:86,178`). | Authenticate distinct device credentials, bind connections to the device and manage its credential lifecycle. |
| Selected workspace access | None per session. Every web session can access all joined members; `Workspaces` includes all joined workspace paths and `others` names/paths for the user's other running sessions (`crates/crew-core/src/rpc.rs:938,951,983`). | Authorize routes server-side and filter index replies, summaries and listing pushes before delivery. The native client cannot supply or enforce its own grants. |
| Individual revocation | None. Web server stop revokes every session (`crates/crew/src/web/mod.rs:724`); no single-session operator RPC exists. | Revoke one device or grant and drain its pending/active channels while keeping other devices and existing local clients running. |

A `/ws` pass-through using one shared browser session meets none of these
three requirements. TLS alone cannot turn that shared bearer credential into
a device identity, a workspace grant or an individual revocation mechanism.

Inside a granted workspace, current web authority permits terminal input,
`shutdown`, model changes and other operator mutations; there is no read-only
role (`crates/crew/src/daemon/handlers.rs:250,285,604`). The gateway gate must
choose its grant tiers and exact action allowlist. This inventory assumes
neither read-only device authority nor all current operator actions by default.

Operator setup would include a bound interface and firewall scope, trusted
certificate/host identity, per-device authentication (for example client
certificates), a route pinned to this server incarnation, trusted onboarding
and rules denying HTTP assets/control Unix access/arbitrary upstreams. The
app authenticates the gateway's TLS identity; the gateway authenticates the
device; the web session and verifier authenticate the dashboard within it.
Restricting the listener to a configured private network limits exposure but
does not replace device authentication. A VPN alone provides neither the
loopback relay nor application capability separation.

The gateway necessarily sees its upstream session/proof and becomes trusted
with dashboard authority. Origin is not authentication for the external
native connection. Required contract points are:

- Device credentials are distinct from agent identity, daemon operator,
  `web.token` and browser web-session credentials. The device must receive
  neither host operator/control tokens nor a reusable all-workspace web
  token. Host provisioning is an intentional operator action outside agent
  context, with its authentication and trusted native handoff specified.
- Bind every accepted connection to an authenticated immutable device
  identity. Check its grant against an opaque selected workspace id before
  routing; device-supplied paths, ids or headers cannot authorize a socket.
  Preserve the upstream `web: true` ceiling, including no team loading,
  web management or watch/event impersonation.
- Enforce grants for index listings, summaries, state, history, actions and
  terminal channels. Filter unauthorized `others`, workspace paths, summary
  counts and questions server-side; hiding a workspace in the UI is not
  access control. Authorization must survive arbitrary protocol calls from
  a modified native client. Specify whether selected grants must also be
  dashboard-joined; do not silently join every registered daemon.
- Membership removal and device/grant revocation must cancel pending dials,
  reject new requests, close active state/terminal channels and await their
  teardown. A successful revocation reply must mean no later client bytes
  can reach that device's denied upstream. Removing a stored credential or
  an authorization row alone does not end existing channels.
- Device revocation is individual: it must leave other devices and local
  web/TUI access running. Server restart, persistent credential lifetime,
  device rotation, grant generation and gateway stop semantics need explicit
  tests. The existing global web-session invalidation cannot substitute for
  this requirement.
- Rate-limit unauthenticated attempts, bound frames and queues, keep secrets
  out of logs/errors, and define metadata revealed before authentication.
  A gateway relaying a raw Unix mismatch would otherwise disclose the
  workspace path before device authentication. Retain trusted distribution,
  host TLS verification and native secure-storage requirements.

Reading dynamic host metadata, provisioning device credentials, authorizing
workspace routes and revoking a device are new gateway features. They are not
capabilities the current browser token possesses. Availability of this
gateway, certificates and an iOS adapter is unverified; the approved design
direction must not be described as an existing connection path.

## Native entry and threat boundaries

The current rule is explicit: `crew web` opens the browser itself; the
dashboard never asks for an address/code; its output must be pasted nowhere
(`notes/web-design.md:1067`, README "What keeps it yours"). Codes belong to
the first presenter. A code imported into an imitation native app would grant
the attacker every joined workspace. A QR image does not authenticate the
app or make that import safe.

**Native address/code entry, clipboard import, QR pairing and custom-URL
launch are not approved onboarding flows.** A future contract must identify
how the operator knows the installed app is the authentic build, how the
host targets that app, how host/device identity is checked, which joined
workspaces become accessible, and how a lost device is revoked. Verified
distribution/signing and an OS-mediated local handoff are candidates, not
existing guarantees. If manual native import is chosen, architecture must
explicitly replace the no-paste rule with an app-specific grant and its
anti-phishing requirements; the browser rule must remain intact. Until then,
design can specify disconnected/setup-required states but cannot promise a
working phone onboarding screen. The selected gateway needs its own trusted
device provisioning flow and does not inherit a safe pairing handoff from
the browser token scheme. Restricted SSH provides no missing metadata or
pairing-code handoff automatically.

Enrollment ceremony remains an explicit gateway design decision. The native
app must authenticate the host through an operator-verified TLS trust anchor
before submitting a device proof or receiving a credential; the host must
authenticate the intended device through a bounded enrollment proof approved
by its local operator. The ceremony must bind that proof to the actual
device/app and selected grants. Exactly how the operator verifies that binding,
and which native-specific rule replaces the browser no-paste behavior, remain
open for `native-gateway-contract`. Gateway direction approval is not approval
of a code, QR or clipboard enrollment UI.

| Threat | Boundary to preserve |
| --- | --- |
| Sandboxed agents retaining host network | Neither loopback nor a native Origin excludes them. They must not see operator/control files, pairing codes, device keys or native-session storage. An app/helper must not turn agent-originated requests into operator commands. |
| Other local users | Unix credentials and private files protect local operator access; loopback dashboard needs pairing/token/proof. Remote forwarding must not relay the full Unix operator channel. |
| Other browser origins | Keep exact browser Host/Origin and existing CSP/text handling unchanged. A native process supplies Origin freely; its credentials and authenticated channel carry trust. |
| Port reuse or delayed callbacks | Preserve incarnation label, verifier proof, connection attempt identity, membership generation and daemon identity. Never trust a fixed port, workspace id or stale callback as authority. |
| Network attacker or pairing interception | Require authenticated encryption and authenticated host identity before pairing/token exchange. The current verifier is a static server-secret hash, not encryption, a fresh challenge or transport channel binding. Do not expose plain HTTP/WS on a LAN. |
| Lost phone or credential copy | Native credential storage and an unlock policy protect at-rest secrets; disclose only selected workspace grants. Client-side Forget is insufficient. The selected gateway must revoke one device and close its active channels; existing web revocation is global and cannot fulfill that contract. |
| Untrusted state/text/terminal output | Validate frames before reducers; bound messages/queues; render text without evaluating it; gate links and terminal clipboard/title effects. Safe transport alone does not make output safe. |
| Unauthenticated network connection exhaustion | Current 32 unauthenticated slots are for loopback clients (`crates/crew/src/web/mod.rs:49,225`). A new network listener needs its own admission/time/connection budgets and tests before per-device state is allocated. |
| Repeated authentication/enrollment attempts | Current `AUTH_FAILED_EVERY_MS` throttles failure logging, not login attempts (`crates/crew/src/web/mod.rs:60,296`). Gateway must define actual attempt throttling and bounded enrollment state, separately from log suppression. |
| TLS impersonation or downgrade | Gateway must authenticate host TLS identity before device authentication or enrollment, specify pin/trust establishment and rotation, and reject plaintext/downgrade. Enrollment is not trust-on-first-use without operator verification. |
| Public assets or pre-auth metadata | New endpoint must expose no dashboard static assets or workspace/version details before device authentication. Existing browser assets and mismatch errors remain unchanged on their existing local surfaces. |
| Gateway compromise | Gateway holds upstream dashboard authority: compromise reaches every granted workspace and may reach every joined workspace if an all-workspace upstream credential is used. Device grant enforcement is not a new OS sandbox guarantee. |

The existing security model intentionally trusts operator processes outside
agent sandboxes, and excludes teams running unsandboxed
(`notes/web-design.md:624`). A trusted Linux/macOS local operator app belongs
to that class; a phone carrying an unrestricted operator SSH credential would
expand it to a device and must not be presented as preserving web limits.

Apple platform constraints have primary-source support, but actual app
behavior is **Apple verification deferred by operator**:

- Local-network TCP access requires the declared purpose and user permission;
  specific Bonjour service browsing also needs declared service types. A
  denied prompt must produce a setup/error state. No current Crew mDNS
  advertisement exists; adding one would announce host/service availability
  to the LAN and needs its own scope decision.
  [Apple TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)
- Apple offers native WebSocket networking APIs. DNS handling for Crew's
  random `.localhost` label and the ability to preserve logical Host/Origin
  while dialling a different destination need a real adapter test; Safari's
  documented resolver limitation is not proof of Foundation behavior.
  [Apple TN3151](https://developer.apple.com/documentation/technotes/tn3151-choosing-the-right-networking-api)
- ATS local-load exceptions concern particular domain/address categories;
  do not assume one key makes Crew's random qualified name usable. A TLS
  gateway should use normal authenticated `wss`; an SSH stream adapter must
  document its networking policy without a global arbitrary-load exception.
  [Apple local networking policy](https://developer.apple.com/documentation/bundleresources/information-property-list/nsapptransportsecurity/nsallowslocalnetworking)
- Use a platform secure store for native session/device secrets rather than
  ordinary preferences. Apple Keychain supports sensitive login tokens;
  item accessibility, no unwanted sync, unlock and deletion policy still
  need implementation tests. [Apple Keychain data protection](https://support.apple.com/guide/security/keychain-data-protection-secb0694df1a/web)
- iOS normally suspends background apps; continuous socket delivery and
  notifications are not an existing guarantee. On foregrounding, mark cached
  data stale and reconnect/prove/refresh listings and snapshots before actions.
  Do not infer that a background HTTP transfer preserves a WebSocket session.
  [Apple background execution](https://developer.apple.com/documentation/xcode/configuring-background-execution-modes)

## Smallest testable adapter proposal

First prove a read-only Linux client harness against the **existing** local
daemon, using current fixtures and `Client::operator()` on a worker. It should
subscribe before its barrier snapshot, handle `resync`, correlate one history read,
and dispose without sending shutdown. Exercise `CREW_IDENTITY` refusal,
missing/wrong credentials, protocol mismatch and daemon restart with fresh
credentials. This is a local trusted operator test harness with read-only
behavior, not a new server read-only credential. No new listener, credentials,
pairing UI or product renderer is required.

In parallel with that eventual harness, use existing `Daemons` stand-ins
(`crates/crew/src/web/daemons.rs:56`) and `Host`/binder seams
(`crates/crew/src/web/mod.rs:356`) to verify the dashboard session's frame
handling and membership/proof lifecycle without an external listener.
The existing raw-stream WS client demonstrates that logical Host/Origin can
be preserved separately from loopback dialling. A future encrypted-channel
adapter can keep this exact compatibility contract.

Proposed shared ownership, subject to `native-architecture`: shared client
core owns protocol 6 `Request`/`ServerFrame` handling and connection state;
platform adapters own discovery, credential storage, authenticated
byte/message channels, clocks and lifecycle signals. Backoff decisions,
snapshot barriers, attempt invalidation, freshness and no mutation replay
belong to shared policy. The OS provides timer effects and foreground/suspend
events; native views render snapshots and dispatch typed actions. No FFI or
platform API shape is introduced by this inventory.

`native-platform-contract` proposes separate `crew-client-transport`
drivers for local Unix (Linux and a verified non-sandbox macOS path) and
authenticated scoped gateway channels (Linux/macOS/iOS). The pure client core
must remain free of sockets, crypto implementations and platform credential stores. This is
an ownership proposal; exact files, package exports and bindings are owned
by the architecture contract and are not created by this card.

Conformance gates for the eventual adapter: current wire fixtures; failed
proof accepting no application data; wrong Host/Origin; capacity/timeouts;
snapshot/resync under interleaved pushes; stale attempts after remove/rejoin;
server versus daemon restart; no credential leaks; dispose cancelling child
tasks; dropped action replies not replaying actions. Gateway gates additionally
prove device-specific credentials, selected workspace isolation across all
methods and listings, individual revocation cancelling pending and active
channels, and changed/untrusted TLS identity stopping before authentication.
On-device permission, suspension, secure-store and real-channel checks belong to the deferred Apple
runner/device pass, with actual app-specific commands recorded at delivery.

## Verification recorded here

```sh
./scripts/build-env cargo test -p crew --test web_wire --test web --test web_server --test operator_auth
```

Passed on Linux: `operator_auth` 14, `web` 21, `web_server` 16, `web_wire` 9
(60 total; 0 failed). These existing tests exercise the non-browser exact
Host/Origin connection, pairing/proof, credential separation, loopback
ownership, state/terminal transport, server versus daemon lifetime and wire
fixture fidelity. They do not verify an iOS client, an SSH service, a TLS
gateway, Linux confined packaging or macOS app/daemon support. No product
source, listener configuration or credential files were changed.
