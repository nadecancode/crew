# Scoped native device gateway contract

Task `native-gateway-contract`. This is a design for review, not a running
service. Existing contracts are verified at
`6049cdf63e36e303327235834f23a798aa5a82c1` in
[native-transport-inventory.md](native-transport-inventory.md), approved at
`51183326`. No listener, credential, session or product source is changed here.
Gateway product edits require reviewer security approval of this contract.

Operator decision `m-8b3375948f11` selects device-specific credentials,
selected workspace access and individual revocation; SSH account authority
is rejected. Relayed orchestrator decision `m-7db8b1b663e8`, with addendum
`m-2b059a4d3afa`, sets **Read by default**,
with separately explicit host-side **Control** and **Terminal** grants.
Kill/Stop team may appear on iOS only with Control and the existing native
confirmations. Terminal requires its own explicit grant and authority notice.
This design does not grant any actual device access.

## Endpoint, local operator and upstream authority

Add an opt-in per-user `crew native-server` process, separate from the
dashboard. Starting a team, `crew web`, opening a native app or listing
devices never starts it. Proposed local commands are `crew native start`,
`status`, `enroll`, `approve`, `devices`, `grant`, `revoke` and `stop`.
Every operation that provisions or changes authority refuses any presence of
`CREW_IDENTITY` before side effects, using the existing operator-context
check. Read-only native administration also requires local operator identity.

Start requires all of: an explicit numeric **unicast** bind address and
nonzero port, explicit public `wss` endpoint, TLS certificate/key paths, and
endpoint profile name. Initial server target is the existing Linux host;
macOS/iOS client verification is deferred, not assumed from server support.
No wildcard/unspecified/multicast address, automatic LAN bind, UPnP, mDNS,
account SSH login or external relay is supplied. Example shape, not a command
executed by this card:

```text
crew native start --listen <unicast-ip>:<port> --endpoint wss://<host>:<public-port>/native/ws --tls-cert <file> --tls-key <file> --name <name>
```

The operator provides routing/firewall access and a certificate valid for the
explicit endpoint's hostname or IP. A different public port is allowed only
through deliberately configured operator routing, never automatic discovery.
The TLS private key must be a regular same-user `0600` file beneath
`paths::runtime_dir()/native/`, without symlinks or external hard-link aliases.
An operator-chosen key outside that protected tree is rejected before loading
or binding; provision it there explicitly. Certificates are public and may
come from another validated file; private key paths cannot be arbitrary.
The native profile contains `wss://<host>:<public-port>/native/ws`,
opaque endpoint UUID, certificate/trust material and expected leaf-certificate
SHA-256. These are public bootstrap metadata, not an enrollment credential.
The operator verifies the fingerprint from the trusted local `crew native
status` output independently of the received profile. Profile import must not
silently trust a fingerprint delivered by the same unverified network peer.

TLS 1.3 only; no plaintext upgrade, TLS early data or automatic pin replacement.
Validate certificate chain against the explicitly approved trust anchor,
validity, configured name and expected leaf hash; verify TLS handshake
signatures. Self-signed operation needs explicit trust material verified by the
operator, not a verifier that returns success for arbitrary certificates.
Leaf rotation requires a deliberate profile trust update through the local
operator; old pins fail before device authentication. Native TLS selection is
independent of the browser's random `.localhost` origin.

The only network HTTP route is an exact WebSocket upgrade at `/native/ws`:
GET, no query or body, expected configured Host, valid upgrade/version/key,
bounded headers. Reject any Origin header on this native-only endpoint to
exclude ordinary browser-origin requests; Origin omission is not native
authentication. A future browser-based native client is out of scope; the
existing dashboard continues using its own unchanged endpoint. No static
assets, redirect, health/version route, cookies,
CORS, remote administration or filesystem route is served. Pre-authentication
errors are generic and contain no daemon/app protocol version, workspace,
device name, public key or host filesystem path. TLS certificate identity and
the opaque connection challenge are intentional public metadata.

Local control uses a new private `native.sock` and startup control credential
`native.token` under `paths::runtime_dir()`, with same-user peer-credential,
`0600` file/socket and private-directory checks. It is separate from
`web.sock`/`web.token`; network device credentials cannot use either control
socket. Native control requests use their own closed enum, not new daemon
`Call` variants. No control token is printed or sent to a device.

The gateway connects directly to selected daemons through their existing
Unix sockets, with the current protected operator-token reader and socket
ownership/derived-path checks (`web/daemons.rs:93,131`). It says `hello` with
`role: operator`, exact protocol 6 and **`web: true`**. Device values never
choose those identity fields. This retains W7 and avoids sharing a global
browser session or depending on dashboard lifetime. Operator tokens remain
in the host process/files; a network response cannot serialize them.

Native memberships are independent explicit local-operator selections. They
do not join/remove workspaces in the browser hub. Each selection pins the
canonical state directory to its existing 16-hex workspace id; mismatched
registry socket, ownership or id collision is refused. No device supplies a
path for discovery, no `--all` enrollment default exists, and registry
presence alone grants nothing. Stopped selected workspaces remain selected;
their registry identity and daemon incarnation are revalidated before dialing.

Gateway compromise exposes its granted Crew resources and its host-side
operator authority; it is not a new OS isolation boundary. Device compromise
provides only that device's granted API access while its public key remains
active. No upstream operator token, browser token, SSH key or agent credential
is ever delivered to the device.

## Device keys, enrollment and trust

The device generates an Ed25519 signing key using OS cryptographic entropy.
Its private half remains on the device in the platform secure store; no
exported private-key DTO, ordinary preferences or profile JSON contains it.
The host stores only the public half, device id/label, expiry, generation and
selected grants. Public keys must decode to 32 bytes and pass strict key/
signature validation. Signatures are 64 bytes. No legacy compatibility or
arbitrary remote signing request is enabled.

The connector's internal `DeviceSigner` port supplies public-key retrieval,
bounded asynchronous signing of **AuthTranscript or EnrollTranscript only**,
and key retirement. It accepts a typed transcript constructed after verified
TLS, never arbitrary server text or a view-supplied byte string. Unlock/read
the key before starting the short authentication deadline. Signer cancellation,
locked storage and unavailable secure storage are explicit connector outcomes;
private-key/store handles never enter the pure core, FFI view records or logs.
Linux may offer explicit memory-only storage when its secure store is absent;
it must not silently fall back to a plaintext key file. Apple Keychain policy
uses device-only, non-synchronizing, unlocked access; actual platform behavior
is **Apple verification deferred by operator**.

Enrollment is a reverse approval ceremony:

1. The local operator opens one 120 s enrollment window with `crew native
   enroll`. It requires the native control credential and rejects agent
   context. It grants nothing and displays no browser pairing code.
2. The authentic installed native app verifies the host's configured TLS
   identity/pin before sending any device key or proof. It opens an enrollment
   channel, receives a fresh 32-byte nonce, generates a 32-byte client nonce
   and signs EnrollTranscript with its own key. The gateway accepts enrollment
   proofs only during the local window. Unrequested enrollment is refused.
3. A valid signed proof creates a pending intent bound to that public key,
   both nonces, endpoint/certificate, exact live channel and window. The app
   and host derive the same 64-bit comparison code, displayed as four groups
   of four hex digits. The device label is untrusted text, not identity.
4. On the trusted local console, the operator compares that code with the
   one displayed in the installed app, selects each workspace and confirms
   Read plus any explicit Control/Terminal powers. Approval supplies the
   intent id, expected public-key hash and comparison code over the local
   control socket. Nothing on the network can approve or supply its own grants.
5. Approval rechecks live channel, deadline, window, proof and selected
   canonical memberships; it atomically consumes the intent and durably
   persists the public device record and grants. Its completion is sent only
   over that intent's existing TLS channel. The window closes after one
   approval. Consumed, canceled or expired intents never issue another grant.

At most 8 pending intents exist globally, at most 1 per public key/window,
and no intent outlives 120 s or its connection. Comparison-code collisions
among live intents are regenerated/refused, not ambiguous. Five incorrect
local approval comparisons consume the intent. A connection gets one signed
proof attempt; failure consumes its nonce and closes it. Rate limits below
apply independently of window/pending capacity. A disconnect before durable
approval cancels the intent. If approval committed but its reply was lost,
the same private key may authenticate afresh and recover its record; it cannot
mint a duplicate device or different grants. Enrollment never auto-retries.

The app displays a comparison code only after the pinned host's enrollment
reply names the accepted intent; it verifies the returned code against its
own transcript. It never displays a pre-verification or unsolicited code.

This replaces the native entry portion of the browser no-paste rule with:
**Verify the host fingerprint in the installed Crew app, then approve that
device and its powers at the trusted host console. Never paste or scan browser
`crew web` output into any app or page.** No QR, clipboard secret import,
custom URL handler or code typed into a network-served page is required.
A comparison code alone is neither a credential nor a remotely redeemable
grant. Another key/channel cannot claim an approved intent; signatures bind
the intended key, and approval occurs on the private local control socket.

Authentic app distribution is still a prerequisite: the host cannot infer
Apple/Linux code-signing identity from an Ed25519 public key. An imitation
page cannot approve, authenticate as the real device or redeem its comparison
code. If the operator deliberately installs an imitation app and approves its
own key at the trusted console, that is a different trust decision the protocol
cannot repair. Native setup must identify the verified distribution; it must
not claim remote code attestation or universal anti-phishing protection.

## Canonical proof and gateway wire

Gateway wire version is **1**, carried outside daemon protocol 6 records.
Gateway-only challenge/auth/enrollment/grant metadata has its own DTOs and
methods. `Call`, `Request`, `Snapshot`, `Push`, `HelloInfo` and existing v6
frames are unchanged. Authentication succeeds before exact native-v1/v6
compatibility is reported; unauthenticated failures reveal neither version.
Authenticated skew returns `protocol` with supported native/daemon numbers
and no workspace path, then closes. An incompatible selected daemon remains a
scoped listing entry; the gateway never restarts it or reinterprets its frames.

The WebSocket's first server message is only:

```json
{"native":{"kind":"challenge","nonce":"<64 lowercase hex>"}}
```

The challenge is generated after TLS/upgrade and belongs to that exact
connection. A normal device submits `native_auth` in the usual id/method/
params envelope, with native/daemon protocol numbers, public key, signature
and requested channel purpose/workspace. No `role`, `token`, `web`, path or
caller-supplied grant is accepted. `native_enroll` is the separate windowed
proof method and carries key, client nonce, label and versions. Unknown
fields, malformed lengths and extra JSON lines are rejected safely.

Proof bytes, without JSON canonicalization or prehash variants:

```text
AuthTranscript:
  ASCII("crew.native.auth.v1\0")
  endpoint_uuid[16] || tls_leaf_sha256[32] || server_nonce[32]
  public_key[32] || native_version_u32_be || daemon_protocol_u32_be
  purpose_u8 || workspace_present_u8 || workspace_id[8 when present]

EnrollTranscript:
  ASCII("crew.native.enroll.v1\0")
  endpoint_uuid[16] || tls_leaf_sha256[32] || server_nonce[32]
  public_key[32] || client_nonce[32]
  native_version_u32_be || daemon_protocol_u32_be

Comparison code:
  first 8 bytes of SHA256(EnrollTranscript), shown as 16 lowercase hex
```

AuthTranscript is at most 150 bytes; EnrollTranscript is exactly 174 bytes.
This is the signer input bound. The label is display-only, never an identity
or grant selector; no arbitrary method/message text reaches the signer.

UUIDs are decoded canonical 32-hex values; workspace ids decode their existing
16 hex digits to exactly 8 bytes. Purposes are Index=0, Workspace=1, Read=2,
Action=3, Terminal=4; only Index omits workspace. The verifier reconstructs
these fixed bytes from validated fields and server-held nonce/endpoint/pin,
checks key activity/expiry and strict Ed25519 signature, then consumes the
connection challenge before opening any upstream. Nonces cannot cross
connections, certificates, endpoints, purposes, workspaces or server starts.
The server also checks the requested purpose against the grant.

After authentication, the gateway returns separate metadata:

| Record | Fields |
| --- | --- |
| `GatewayAuthority` | `endpoint_id`, new-per-start `endpoint_epoch`, `device_id`, `device_generation`, `credential_expires_at` (UTC ms), `server_time` |
| `WorkspaceGrant` | `workspace_id`, `grant_generation`, ordered `capabilities` (`read`, optionally `control`, `terminal`), explicit allowed action identifiers |
| `GatewayOpened` | authority, purpose, optional workspace/grant, current compatible daemon `HelloInfo` when connected |
| `GatewayListing` | authority, grant records and filtered v6 `Workspaces`; `others` always empty |

Endpoint UUID is persistent; endpoint epoch is fresh random 128 bits per
gateway start. Device/grant generations increase on every authority change,
never wrap. Default device expiry is 30 days, returned explicitly; the local
operator may set a shorter positive lifetime. No silent refresh or permanent
default exists. Re-enrollment/rotation requires a newly generated key and an
explicit host approval; a successful existing-key auth after a lost approval
reply is recovery, not renewal. A host-side replacement names the old device,
fences its generation, and approves the new key/device with explicitly
selected grants. Old public keys remain inactive; new enrollment does not
inherit powers silently. Expired/revoked key records
persist as inactive and cannot be reactivated by restarting the gateway.

The server checks expiry on authentication and every request/write lease,
and schedules live-session expiry with its monotonic deadline. Clock rollback
before a persisted issuance time fails closed; restart never extends a
credential beyond its stored UTC expiry or originally issued duration.
The client expiry is display/availability advice; only the server grants access.

The connector verifies this authority and emits the existing normalized
`ConnectionContext {mode: ScopedGateway, grant, snapshot_mode, scope}`.
Its opaque `Grant.grant_id` and new scope attempt bind endpoint epoch,
device/grant generations and daemon incarnation. Authority changes Unbind
old contexts before Bind. The core consumes v6 Snapshot/Push/outcomes and
filtered listings, dispatching closed typed commands. Secrets and gateway
wire/parser/crypto stay in the adapter. Browser connections continue using
their existing verifier proof; no native change bypasses that browser check.

## Workspace data and exhaustive method policy

Read is mandatory on every selected grant. Control and Terminal are separate
powers, not an ordered ladder and never client assertions. Index accepts only
authenticated `ping` and `workspaces`; gateway grant/revocation/stop notices
are server messages, not remote administration calls. Listing
and summary watches expose only this device's selected memberships. A per-device
listing revision changes only when its visible authorized entries/grants
change, with pushes at most once per 100 ms. `others=[]`; neither initial
listing nor updates contain ungranted names, paths, objectives, counts or
questions. Device-owned preferences are local, not shared browser `web_prefs`.

Every workspace request, subscription and terminal write checks active
device/grant generation, expiry, immutable route, purpose and the policy
below at the server. Repeated `hello`, raw unknown methods, path-based
destinations and fields attempting to change identity/scope fail. Socket
authentication is not permission to forward arbitrary JSON unparsed.

The following table assigns **every protocol-6 `Call::method()`** exactly
once. Allowed methods are also constrained by purpose and validated params;
all other methods remain denied even with Control plus Terminal. A raw modified
client receives the same policy decision as a native UI command.

| Required grant / handling | Methods |
| --- | --- |
| Read on a fixed workspace connection | `snapshot`, `subscribe`, `agents_list`, `tasks_list`, `task_get`, `history`, `message_get`, `inbox`, `events`, `agent_output`, `agent_transcript`, `git_status`, `git_diff` |
| Authenticated index or granted workspace; handled without arbitrary forwarding | `ping`, `workspaces` |
| Gateway-internal index watch only; any device call denied | `summary` |
| Control on that workspace | `send`, `answer`, `agent_pause`, `agent_resume`, `agent_compact`, `agent_restart`, `agent_restart_fresh`, `agent_kill`, `agent_set_model`, `resume`, `shutdown`, `models_list` |
| Terminal on that workspace and named agent | `agent_input`, `attach`, `attach_input`, `attach_resize` |
| Denied for every native network grant | `hello`, `load_team`, `agent_add`, `agent_remove`, `agent_focus`, `agent_spawn`, `usage_status`, `listen`, `delivered`, `credential`, `ack`, `status_update`, `decline`, `progress`, `task_create`, `task_edit`, `task_assign`, `task_complete`, `task_fail`, `task_retry`, `review_request`, `review_submit`, `session_complete`, `git_commit`, `git_cherry_pick`, `git_merge`, `integrate`, `hook`, `web_open`, `web_close`, `web_pair`, `web_prefs`, `web_rescan`, `web_attach`, `web_event` |

Additional constraints:

- `task_get` requires an explicit task id; agent/entity targets must belong
  to the connection's one daemon. `inbox` requires a named agent and
  `ack:false`. `git_status` and `git_diff` require a named `agent`; omission
  is refused before forwarding, retaining the operator caller's daemon rule.
  `summary {watch:true}` belongs to the gateway's internal index watcher,
  which feeds only filtered `Workspaces.joined[].summary`. It is never
  forwarded from a device connection; device `summary` calls are forbidden.
  Read never sends/answers messages, changes agent state, performs
  VCS writes or subscribes to agent delivery/credential channels.
- `send` is the existing operator composer operation: `type:message`,
  explicit recipient in this workspace or `*`, bounded body, caller-chosen
  stable message id. `answer` addresses an open question in this workspace.
  They require Control because they change agent behavior. Unknown metadata
  or message kinds are not an alternate route for task creation/impersonation.
- `agent_compact` requires a named agent. `resume` affects only this daemon;
  an empty agent list retains its existing all-limited-agents meaning.
  `shutdown` requires `stop_agents:true`. Model listings require Control;
  broad account-wide `usage_status` is denied. Native Usage uses the selected
  daemon's existing snapshot/push data.
- `agent_input` and PTY bytes are Terminal powers, including submitted
  prompts. Read may fetch rendered output/transcripts but may not use
  `attach`: attachment and resize can alter a shared PTY. A Terminal channel
  attaches once to its named agent and thereafter accepts only ping/input/
  resize; a different agent requires a new authenticated channel.
- State channels support Read/subscribe/snapshot; Read channels accept reads
  without subscriptions; Action channels accept Control or `agent_input`
  with Terminal. Independent lanes preserve responsive state/terminal IO.
- Existing native confirmations remain for Kill, Restart fresh, usage-limit
  restart and Stop team. Server authorization is the explicit Control grant;
  a client confirmation is not a cryptographic security boundary and a
  compromised Control device can exercise its granted powers directly.

Read explicitly exposes selected-workspace task/chat/question contents,
transcripts, diffs and rendered screens, which may contain project secrets
or agent-authored references to other files. It also retains absolute paths
and existing workspace diagnostics in `SessionInfo`/`binary_warning`, agent
records and VCS results. These are accepted selected-workspace disclosures,
explained at host approval; Read is not metadata-only. Structured listing
metadata for ungranted workspaces is filtered server-side. The gateway cannot
promise to redact arbitrary sensitive text agents put into an authorized
workspace's own messages/output. No token/key or raw upstream mismatch data
may appear in gateway errors or normalized auth records.

**Workspace grants limit Crew resources, not OS access.** Control may restart
agents/change models or cause agents to act on messages as that workspace's
configured processes. Terminal is the agent's command input under its actual
shell/backend and sandbox; an unsandboxed agent can access the operator's
files beyond that workspace. The host-side Control and Terminal checkboxes state this
plainly before approval. A selected workspace is not a new filesystem sandbox.

## Revocation, cancellation and lifecycle

Persist device/grant records at `paths::runtime_dir()/native/devices.json`,
beside the already protected operator runtime, with private directories and
regular same-user `0600` files. Native authority never uses `XDG_STATE_HOME`,
including an operator override outside `$HOME`. Never place records in a
workspace or agent `.crew` directory. Startup/control token and TLS key remain
separate protected credentials in the same protected runtime tree. Safe atomic
replacement must fsync the new file and parent;
durable-write failure changes no authority and must not report success.
Serialize local admin changes; device/grant generation changes are committed
before new sessions can observe them. A private lock prevents two services
using the same state concurrently.

One device revoke or one grant removal follows this order:

1. Acquire the admission/write fence for the admin transition; commit inactive
   device or removed grant plus increased generation to the private registry,
   then publish the inactive generation before releasing the fence. No new
   lease for that authority can succeed. Do not hold a registry lock while
   awaiting network teardown; leases expose a separate drain counter.
2. Cancel pending enrollment/auth/dial work and invalidate queued frames,
   results, callbacks and terminal input for that generation.
3. Close its state/read/action/terminal channels and drain their upstream
   writers and child tasks. Existing writes hold a generation lease through
   completion/cancellation; revocation awaits those leases. After 1 s abort
   slow tasks and await them; allow at most 5 s for forced draining.
4. Reply success only once no lease or writer can send another byte to that
   denied upstream. If teardown fails, report an error, keep authority
   inactive and fail closed; never claim a successful revocation early.

Already-forwarded daemon actions may still finish: closing a socket cannot
undo a restart, message, answer, kill or terminal byte accepted before the
fence. Client results after an uncertain write are OutcomeUnknown and never
automatically replayed, except the existing identified-message policy with
unchanged full authority/daemon context. The success guarantee is **no later
client byte forwarded after the revocation reply**, not rollback of earlier work.
No service stop, daemon stop, global web revocation or other-device disconnect
is used to implement individual revocation. Revocation is durable across crashes.

Service stop first denies all new authentication/enrollment and discards
ephemeral challenges/windows, then closes/drains native connections and
upstreams, then releases the TLS listener and control files. Stored device
grants remain; stop is not device revocation. Startup gets a new endpoint
epoch and new control token, drops all old nonces/windows, reads the durable
registry and validates every membership afresh. Revoked/expired keys stay
denied. Its TLS identity remains the configured certificate until explicitly
rotated, so active devices can reauthenticate after restart.

The private runtime registry survives service/dashboard/daemon restart while
the runtime directory exists; routine stop never deletes it. If logout,
reboot or explicit runtime-directory removal loses that state, startup creates
a new endpoint UUID with no devices or grants. Old profiles/keys cannot
authenticate that endpoint; the operator reprovisions its protected TLS key
and enrolls new device keys explicitly. There is no restoration from
`XDG_STATE_HOME` or an automatic reactivation of missing authority. This is
the v1 tradeoff for using the already protected runtime tree.

The native service does not inherit dashboard `UNASKED_FOR` 60 s or `LINGER`
600 s: it runs until explicit stop/signal or a fatal listener/control/store
failure. Dashboard restart/stop has no native authentication effect because
the gateway uses separate direct-daemon channels. Daemon restart changes
incarnation, ends its native channel and requires fresh protected token read,
hello, subscribe and snapshot; it does not revoke the device. Native shutdown
of a team affects only that selected daemon; it does not stop the gateway.

Native close codes are separate from existing browser 4001/4002: 4101 device
revoked/expired/auth lost, 4102 workspace grant changed/removed, 4103 gateway
stopping, 4104 authenticated incompatibility. Missing/running/stopped workspace
state still comes from the filtered listing, not a late close frame. Grant
change invalidates pending confirmations and attempts before any reconnect.
On iOS foregrounding, reauthenticate, refresh grants/listing and take a fresh
barrier snapshot before actions; cached state is stale/read-only while offline.
There is no background delivery promise or offline mutation queue.

Agent sandboxes must not expose the native private registry, control token,
TLS key or operator tokens. The runtime root is already protected by
`protected.add(s.runtime_dir)` in `crates/crew/src/sandbox/profile.rs:262`;
native registry and key paths are constrained beneath it. No private key may
be loaded from an operator-chosen external tree. Required regression attempts
reads/opens of every named native credential/state path from a real sandbox
with `XDG_STATE_HOME` both inside and outside `$HOME`, and confirms failure;
it also confirms that an external TLS-key path is rejected before startup.
it may connect to the host TLS listener but cannot approve/enroll itself or
obtain any authorized workspace. Network reachability is not operator identity.

## Limits, errors and accepted risks

| Resource | v1 limit / behavior |
| --- | --- |
| TLS plus HTTP pre-authentication | 32 global connections, 4 per source address; 5 s TLS/HTTP deadline; HTTP head 8 KiB and 48 headers; no allocation of device state before admission |
| Normal signed authentication | One proof attempt, 5 s after challenge; pre-auth JSON 4 KiB; consumed nonce closes on failure; no JSON ping/version/status before auth |
| Enrollment | Local window 120 s; 8 pending intents; 1 per key/window; 5 wrong local comparisons; per-address 5 begin attempts/minute and global 30/minute, bounded LRU source bucket table (1,024 entries) |
| Repeated auth/handshake failures | Per-address 10/minute and global 100/minute token buckets, separate from log throttling; full bucket/table denies, never grows unbounded |
| Persistent authority | At most 32 device records including inactive keys, 32 selected memberships, 32 grants/device; explicit capacity error at local administration, no automatic tombstone purge or id reuse |
| Authenticated channels | 128 global, 8 per device; purpose and grant checked before each upstream dial; 64 pending requests/device and 16/channel |
| Frames and writes | 1 MiB authenticated frame, 64 KiB message/input body; one JSON object/text frame, no raw line breaks; bounded complete-line buffers and outbound queue of 256 frames or 4 MiB, whichever first |
| Reads | snapshot messages <=2,000/events <=5,000; bounded recent history <=2,000, events <=5,000, transcript <=400, output <=300 lines, inbox <=500; no general paging claim |
| Liveness/deadlines | ping 20 s, silent close 40 s; reads 3,000 ms, lifecycle 30,000 ms, models 20,000 ms, resume/shutdown 90,000 ms; monotonic timers; backpressure triggers resync/disconnect, never silent loss marked live |

Source-address rate limits are DoS budgets, not identities; NAT sharing can
cause visible temporary refusal. Operator-local control is independently
bounded and remains available to revoke/stop under network saturation. Invalid
input returns generic `invalid`/`unauthorized` before auth; scoped policy denial
returns `forbidden`, missing authorized resources `not_found`, unavailable
daemon `unavailable`. Strip upstream error data and sanitize unsafe messages;
do not forward serde diagnostics quoting input or mismatched-workspace paths.

Log only event kind, safe opaque authenticated device/workspace ids and
coalesced counters. Do not log frames, proof/signature/nonces, comparison
codes, private keys, operator/control tokens, imported profiles or upstream
credential-bearing hello. Human-readable device labels are escaped/bounded.
Redact Debug/Display for all credential/proof/enrollment objects. Logs for
unauthenticated attempts contain no attacker-supplied identity or filesystem
data. Sanitized failures must not reveal whether a revoked/unknown key exists.

Read can disclose valuable project data. Control/Terminal intentionally cause
work on the host; confirmations do not reduce a compromised device's granted
authority. Operators outside agent sandboxes and unsandboxed agents remain
trusted as in the existing security model. Gateway compromise and local DoS
remain separate from the device grant guarantee; do not advertise OS isolation
or leak-proof arbitrary agent text.

## Source ownership and delivery slices

| Slice / owner | Proposed files and release gate |
| --- | --- |
| Shared wire/transcript values, coordinated runtime + native-transport | `crates/crew-protocol/src/native.rs`: gateway-v1 DTOs, strict field sizes, canonical transcript builder and fixed vectors; no signing/private keys/platform calls. Approval does not change protocol 6. |
| Gateway server / runtime | `crates/crew/src/native/{mod,http,tls,auth,enroll,store,grants,bridge,control}.rs`; `crates/crew/src/cli/ops/native.rs` plus minimal CLI/lib registrations; private path helpers. Requires gateway security approval first. |
| Native gateway connector / native-transport | `crates/crew-client-transport/src/gateway/**`: verified TLS, DeviceSigner port, signed handshake/enrollment, grant/close conversion, request correlation and cancellation; no independent reducer. Local transport remains a separate N2A slice. |
| Shared core/action/projection | Existing N2A `ClientScope`, `ConnectionContext`, `Grant`, ClientInput/Effect and typed commands; gateway binding is normalized by driver. Scheduler/content/graph ownership remains in native-platform-contract. |
| Platform secure store/setup / native platform owners | Linux GTK/secret-store adapter and Apple Keychain/SwiftUI setup, host pin comparison and device approval instructions. They consume the approved connector and no private-key view DTO. Apple verification deferred by operator. |
| Integration/security QA / qa + reviewer | New gateway/client suites and existing 60 tests; real-daemon matrix below; no active operator service/listener mutation. |

Reuse `Registered` daemon socket checks, typed v6 serialization, snapshot
barriers and membership generation/drain semantics. Do not turn the current
web server into a LAN listener or change its routes/auth. Server-side wrappers
must parse and authorize native requests before encoding a known v6 call;
daemon authorization remains defense in depth. No browser bundle/dependency,
origin, pairing, terminal UI or P1–P4 ceiling change is needed.

Candidate implementation dependencies: existing Tokio/tungstenite/serde/SHA-256
plus a pinned rustls/tokio-rustls backend and strict Ed25519 implementation.
Gate server dependencies behind the new default-off `crew` feature
`native-gateway`; ordinary `cargo install --path crates/crew` does not include
its crypto/TLS graph. Without that feature, native CLI operations report
"not built with the gateway" and never silently fall back to another service.
Gateway-enabled installation is an explicit `--features native-gateway` build.
Implementation evidence includes the lockfile diff and
`cargo tree -e normal -p crew` versus
`cargo tree -e normal -p crew --features native-gateway`.
Primary APIs support certificate and handshake-signature verification and
asynchronous TLS streams; concrete lockfile/MSRV/backend choices and Linux
compilation are implementation gates, not results of this design.
[Rustls verification API](https://docs.rs/rustls/latest/rustls/client/danger/trait.ServerCertVerifier.html),
[Tokio rustls stream contract](https://docs.rs/tokio-rustls/latest/tokio_rustls/),
[Ed25519 strict verification API](https://docs.rs/ed25519-dalek/2.2.0/ed25519_dalek/struct.VerifyingKey.html#method.verify_strict).
Prefer no accidental native crypto dependency in the pure core or browser
bundle. Any customized TLS verifier must still check signatures/name/time;
pin comparison alone is insufficient.

## Real-daemon acceptance matrix

Each case uses temporary user-state/runtime/workspace directories, isolated
test credentials and an opt-in **loopback-only TLS test listener**. No case
touches the operator's running gateway/dashboard/team or enables LAN access.
Run modified raw WebSocket clients as well as the connector; client policy
must not be the reason a server rejects a forbidden action.

| Case | Observable pass condition |
| --- | --- |
| G01 off/bind/TLS | Plain team/web start opens no native socket; missing/wildcard bind or credentials refuses; test TLS handshake succeeds only with valid configured identity, wrong pin/name/expired cert/plaintext fails before any device proof/upstream dial. |
| G02 origin/assets/disclosure | Origin-present request rejected; all non-upgrade routes/queries denied; no assets or pre-auth version/workspace/device metadata; raw unauth calls reach no daemon. |
| G03 key/enrollment | Device key generated locally; host stores public half only; no window refuses begin; valid proof yields matching code; console approval creates exactly one device/grant set. An agent-context CLI/control attempt creates none. |
| G04 replay/expiry | Reuse signature on another nonce/connection/endpoint/cert/purpose/workspace fails; expired nonce/window/intent or sixth wrong comparison cannot approve; copied code with different key fails; lost approval reply recovers same record through key auth without duplicate enrollment. |
| G05 grant route/listing | Devices A/B each see only their selected workspace entries/summaries/questions; `others=[]`; unauthorized workspace id, guessed path, foreign agent/resource and hidden listing push produce no dial/data. Rev increments only for each device's authorized visible changes. |
| G06 exhaustive policy | For every v6 method, Read/Control/Terminal raw clients match the table; Read send/answer/input/kill/stop fail with no daemon side effect; explicit Control allows confirmed UI kill/stop; Terminal-only inputs work but Control-only raw PTY/prompt fails. Unknown/new enum method defaults to deny. |
| G07 data disclosure | Authorized Read returns documented transcript/diff/rendered screen/path diagnostics; ungranted structured metadata and credential/error-data contents never appear. Token/private-key synthetic markers absent from HTTP, frames, errors, registry and logs. |
| G08 ordering/backpressure | Subscribe reply precedes pushes; snapshot/resync barrier closes gaps under interleaved task/agent/messages; partial/oversized/line-break frames cannot become extra calls; slow reader stays bounded and recovers by resync or disconnect. |
| G09 active revoke | While A has state/read/action and open terminal channels, revoke A; pending dial blocked and writers canceled/drained before successful reply; later input/new auth/raw requests reach no daemon. B and local web/TUI still work. Already-forwarded mutation may complete once and is never replayed. |
| G10 grant/expiry/restart | Remove one workspace grant without revoking A's other grants; old grant attempts/confirmations cannot write. Device expiry closes active channels. Gateway restart preserves denied/active registry but gets new epoch/nonces; dashboard restart leaves native credentials intact; daemon restart forces new incarnation/token/snapshot. |
| G11 stop/crash/store failure | Stop denies authentication, drains channels then releases listener; crash/restart keeps durable revokes; failed atomic registry write reports failure without uncommitted authority changes; forced teardown failure never claims revocation success. |
| G12 network budgets | Exhaust TLS/HTTP/auth/enrollment/queue/request/device capacities, including bounded source-bucket churn; memory stays bounded, no unauthorized work occurs, local administration and an existing healthy authorized device remain usable. |
| G13 sandbox/source skew | With XDG_STATE_HOME inside and outside home, registry/key stay under the protected runtime; real agent sandbox cannot read/open native registry/control/TLS-key paths or approve/enroll itself. External TLS-key path refuses startup. Mismatched authenticated protocol returns safe incompatibility, never raw daemon path; old scope/callbacks after grant change are ignored. |
| G14 unchanged dashboard | Existing `web`, `web_server`, `web_wire`, `operator_auth` tests remain byte/source unchanged and pass (60 at reviewed checkpoint); browser exact Host/Origin, code/proof, loopback/control/W7 and individual team lifetime retain behavior. |

Required implementation commands once those suites exist:

```sh
./scripts/build-env cargo test -p crew --features native-gateway --test native_gateway
./scripts/build-env cargo test -p crew --features native-gateway --lib native::
./scripts/build-env cargo test -p crew-protocol native::
./scripts/build-env cargo test -p crew-client-transport gateway::
./scripts/build-env cargo test -p crew --test web --test web_server --test web_wire --test operator_auth
```

**Apple verification deferred by operator.** On the available Apple runner,
compile/test the same shared packages and actual connector, then run the real
app/device suite for Keychain unlock/retirement, permission denial, pinned TLS,
enrollment, foreground resynchronization and live revocation. Conditional
package commands (not tests run by this card):

```sh
cargo test -p crew-protocol -p crew-client -p crew-client-transport
cargo check -p crew-protocol -p crew-client -p crew-client-transport --target aarch64-apple-ios
cargo check -p crew-protocol -p crew-client -p crew-client-transport --target aarch64-apple-ios-sim
```

App-specific Xcode/simulator commands belong to the actual projects in the
native platform contract. Expected results are successful package builds,
passing shared/connector suites and demonstrated on-device behavior; none
is claimed here. This design has not implemented or tested gateway enrollment,
TLS, credentials, grants or revocation. Inventory's 60-test Linux result
verifies only the existing source contracts it cites.
