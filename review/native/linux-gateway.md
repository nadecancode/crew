# Linux remote gateway connections (`native-linux-gateway`)

Source: one commit on main `2cb18b2b`, changing only `crates/crew-desktop/**`
and the `crew-desktop` dependency list in `Cargo.lock` (ed25519-dalek,
getrandom 0.3.4, serde, zeroize; sha2 for tests). All are already locked
for other workspace crates; no new package enters the lock.

## What is delivered

- `src/remote/profile.rs`: public host metadata only, saved owner-only and
  atomically at `$XDG_CONFIG_HOME/crew-desktop/connections.json`. S02 checks
  the pasted `crew native status` output for shape without constructing a
  trusted profile. The operator-entered fingerprint follows the shared rule
  (ASCII whitespace and `:` ignored, lower case, exactly 64 `0-9a-f`) and is
  the only value passed to `GatewayProfile::trust`.
- `src/remote/secret.rs`: the Linux OS credential adapter. A `DeviceSigner`
  keeps each profile's Ed25519 seed in the Secret Service default collection
  (`org.freedesktop.secrets` over GIO D-Bus; no new crate). Only
  `public_key` and explicit user actions (Continue, Remove) may prompt;
  signing returns `Locked` promptly. No plaintext file. Without a service the
  only alternative is the transport's explicit memory-only signer.
- `src/remote/worker.rs`: setup and sessions through the connector's
  `GatewayClient` on the native service worker, one client per profile so
  setup and sessions share the authority fence.
- `src/remote/connections.rs`: Connections and S01–S08 in GTK, with the
  designer precheck LX1–LX5, G13 row shape (shared with content's
  `entry_row`) and S08 `bb04d615`.
- Touchpoints in the local app: `service.rs` Remote request/response
  variants, one session-replacement helper (`bind`), leaving a bound remote
  profile before a local connect; `controller.rs` delegates Connections,
  forwards remote responses, notices and context replies, implements
  `remote::Host`. Without the `gateway` feature the local app is unchanged.

## Commands and results (Linux, GTK 4.22.4, VTE 0.84.1, Rust 1.93.1)

```sh
export NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source
crates/crew-desktop/tests/run-window-test.sh --gateway   # 1 passed, ~15 s
crates/crew-desktop/tests/run-window-test.sh --client    # 1 passed (local app unchanged)
crates/crew-desktop/tests/run-window-test.sh             # 1 passed
scripts/native/linux-env.sh cargo test --locked -p crew-desktop --features native-ui,gateway --lib remote::   # 7 passed
scripts/native/linux-env.sh cargo clippy --locked -p crew-desktop --features <set> --all-targets -- -D warnings
#   sets: native-ui,gateway-tests | native-ui,gateway | native-ui,local | native-ui | default: clean
cargo fmt -p crew-desktop -- --check                     # clean
```

`--gateway` runs on a private Xvfb display and session bus with a private
GNOME Keyring 50.0 and xdotool from the pinned nixpkgs. Crew and keyring
sockets use a short private runtime directory under `/tmp` (removed on exit);
the test asserts Crew does not fall back to the shared `/tmp/crew-<uid>`. Any
GTK critical fails the run. Captures land in the run's
`target/native-validation/linux-window-test-*` directory (`fixture-gateway-*`
from the recording host, `live-gateway-*` from the production service).

## Journey covered by `tests/gateway.rs`

Recording host (real GTK widgets, typed requests):
S01 local and remote sections; keyboard-only Return on Add, typing, Tab out
of the name and out of the profile paste area (no Tab trap); field errors in
reading order with focus on the first invalid field, retained safe input and
errors cleared on edit; mismatch table with "differs" in text and no trust
action, Review returns focus to the fingerprint; unavailable secure storage
offers only the explicit memory choice; one Prepare and one Enroll request;
comparison code only after the accepted reply, four exact lower-case groups,
selectable, labelled; real Escape cancels the request first; Canceled,
OutcomeUnknown (Reconnect to check sends the same-key Connect), Approved
(Access granted distinct from Loading workspaces, empty listing shows host
instructions); S08 access for the open route, other workspaces by name and
availability, disclosures, revocation instructions; access loss focuses a
Status explanation without the lost name; Remove retires and deletes the
public profile.

Secret Service (private GNOME Keyring): readiness Ready, no implicit key,
create then reuse, one key per profile, redacted Debug, retire.

Production controller and service, two isolated daemons, loopback TLS
gateway with the synthetic test certificate:
request without a host window is refused once with nothing pending; after
`native_enroll` the app's code equals `native_pending` and the request
carries the Secret Service key hash; host approves alpha Read+Terminal and
beta Read; Access granted then both authorized workspaces; alpha live with
agent controls withheld ("Control authority is required") and the terminal
allowed; input typed into the identified VTE renderer reaches the real PTY
through the gateway; host grant change shows "changed at the host", Open
workspace again restores live with Control; single-workspace revoke shows
"no longer available" without the name and beta stays usable; normal
service restart shows "Native service is unavailable" and Reconnect returns
to live; a locked keyring stops authentication with the unlock copy and
Try again recovers with the same key; device revocation signs out; a profile
whose pin is not the served certificate fails TLS ("Host identity could not
be verified", Review host profile, no trust action) with an enrollment
window open and nothing pending; Remove from this app retires the OS key.
Saved profiles never contain key, signature or code material.

## Not covered here

- Screen-reader journey: `GTK_A11Y=test` metadata (roles, labels, invalid
  state) is asserted; an Orca run is not claimed.
- Credential expiry and renewal (C17), network loss during enrollment (C13),
  the memory-only choice against the live gateway (presentation only), wide
  list/detail layout (Connections uses forward/back pages at every width).
- Observation for transport/server owners: in an earlier ordering (grant
  edit, then native service restart, then single-workspace revoke) the index
  ended with GrantChanged, stayed Reconnecting without a scheduled retry,
  and a manual Reconnect was refused with Authentication. The delivered test
  orders restart after the revoke; the Workspaces page offers Reconnect now
  while stale.
- Apple: not applicable to this card.
