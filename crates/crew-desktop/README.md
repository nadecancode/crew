# Crew for Linux

Crew's Linux application uses GTK widgets and a VTE terminal. It shares the
Rust client reducer, projections, action planner and native transport with the
other native applications. There is no embedded browser or webview.

The local application is being validated by `native-linux-ui`. Remote profiles,
gateway enrollment and device key storage belong to `native-linux-gateway`.
The provisional application ID is `dev.crew.Crew`; this package makes no domain
ownership, signing or distribution claim. Closing Crew or choosing Quit closes
the client and leaves the team running. Stop team is a separate confirmed action.

## Build and run

Native Rust is 1.93 (verified toolchain 1.93.1). GTK4-rs is pinned to 0.10.3 with
the `v4_6` API feature; VTE4-rs is pinned to 0.9.0. The reviewed Nix environment
supplies GTK 4.22.4 and VTE 0.84.1. Older distribution toolkits have not been
verified. Resources are embedded by `glib-compile-resources`; startup does not
depend on an adjacent source tree.

```sh
export NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source
rustc -Vv
cargo -V
scripts/native/linux-build.sh -j 2
scripts/native/linux-smoke.sh -- "$PWD/target/native-validation/cargo/debug/crew-desktop"
crates/crew-desktop/tests/run-window-test.sh
crates/crew-desktop/tests/run-window-test.sh --client
crates/crew-desktop/tests/run-window-test.sh --gateway
nix-shell -I nixpkgs="$NATIVE_NIXPKGS" -p xdotool --run \
  'crates/crew-desktop/tests/run-window-test.sh --keyboard'
```

Default features are empty. `native-ui` enables Linux-only GTK/VTE and portable
projection types; `local` enables the real local Rust service; `gateway` adds
remote profiles, gateway enrollment and the Secret Service device signer, and
`gateway-tests` its isolated gateway/daemon tests. The GUI build runner enables
`native-ui,local`; a remote-capable build adds `gateway`. Default CLI builds need no GTK development environment.
QA owns the final shared workspace/lock registration; its bounded dependency
changes are delivered separately from the desktop source.

## Presentation and service ownership

The five destinations are Team, Tasks, Messages, Events and Usage. Native lists
and graphs share qualified entity keys and the same Rust projections. The
controller retains selections per destination; native scrolling and graph zoom
remain local presentation state. Graphs consume the shared task layers and
satellite relationships; they do not calculate domain work or eligibility.

Both graphs place layers in columns from left to right, centred per layer,
and keep list order within a layer. The Team graph shows the orchestrator, or
the scheduler when the team has none, coordinating its members, with subagents
tethered to their parents; the Task graph draws each dependency towards the
task that waits on it and names unresolved dependencies on the node. A wide
layer wraps into several columns; edges between non-adjacent columns run
through the gaps between nodes. Every node carries a status chip with text
and semantic colour. Zoom scales the drawing uniformly, so text never
rewraps; Fit puts the whole graph in view and runs by itself only when a graph
first appears. The graph is one Tab stop: arrow keys move between nodes,
Home/End go to the first and last, and Enter opens the node. Each node's
accessible description states its relationships.

`service.rs` runs protected discovery and all `NativeSession` calls on a Rust
worker. Bounded typed channels deliver notices, query results and terminal
chunks to GTK's main context. Widgets do no socket, credential, file or protocol
IO. A native instance tag and the full `ClientScope` reject late callbacks after
service replacement, workspace switch or loss of access. Shared Rust owns
history retention, cursors, resource status, retries and action decisions.

`controller.rs` routes projections, local selection, drafts, dialogs and user
invocations. Agent controls render shared availability. Workspace and form
controls query the complete captured intent through the reviewed immutable
`ViewQuery::Availability` API, showing its exact unavailable reason. The query
does not plan an operation or allocate confirmation/timer IDs.
Plans are used only for explicit invocation. Only Kill, Restart fresh, Restart
after limit and Stop team open confirmations; Cancel is initially safe/default.
Software-keyboard Return adds a newline; Send, Answer and Submit prompt are
explicit buttons. Command-shaped input uses the shared slash parser and never
falls through to a team message.

Submitted message receipts remain read-only across reconnect attempts. GTK
correlates an authoritative final notice by native service instance, endpoint,
workspace and original operation ID; the shared core alone owns its same-ID
retry. Close preserves the receipt without canceling the host operation.
Success clears only the submitted text, while unknown outcomes retain it and
ask the operator to check message history. Explicit restoration checks the
current verified scope and shared availability before opening an editor; the
original receipt stays read-only with its own outcome. A message ID belongs to
one exact recipient and body: an unchanged restored retry keeps it, while an
edited body or recipient takes a fresh ID on explicit submission, since the
message bus would otherwise return the original as a duplicate. Unsent drafts
are kept by endpoint, workspace and purpose, so an ordinary reconnect keeps
them; a new submission still plans with the current verified scope. Known
revocation, removal, sign-out or service replacement purges affected receipts
and drafts.

A successful Attach notice creates a renderer identified by native instance,
full scope and attachment ID before any output is displayed. A later plan
continuation associates that existing renderer with its captured agent panel;
it cannot reset or revive an ended terminal. Unknown/pre-attach chunks are
discarded and counted. VTE receives bytes from the shared service and never
spawns a local process. Input is confined to the focused, live terminal; end,
detach or disconnect makes it read-only. No terminal bytes are replayed after
reconnect. OSC output does not open links, change application identity or write
the clipboard. Native Copy selection is an explicit user action. Evidence and
artifact paths remain selectable/copyable text; no host file is fetched.

Ctrl+1–5 select destinations; Ctrl+B toggles the sidebar; Ctrl+I toggles details;
Alt+Left returns from compact navigation/details; F1 opens shortcut help;
Ctrl+W closes the window; Ctrl+Q quits. Appearance follows native contrast and
text settings. Dark app-owned surfaces remain neutral. There are no continuous
decorative animations or graph refits after every push.

## Remote connections

With `gateway`, Connections (the workspace selector) lists this computer's
workspaces and remote hosts and runs the reverse-approval setup in
`src/remote`. A remote profile stores only public host metadata in
`$XDG_CONFIG_HOME/crew-desktop/connections.json` (owner-only, atomically
replaced). The host fingerprint is typed from `crew native status` at the
trusted host console and passed to the connector's `GatewayProfile::trust`;
the pasted profile never supplies its own trust check. The connector owns TLS,
proofs, enrollment, grants and revocation; GTK renders typed progress and safe
reasons and sends explicit requests.

The device signing key is an Ed25519 seed in the user's Secret Service
default collection (GNOME Keyring, KWallet or compatible). Only public-key
retrieval and explicit user actions may show the keyring's unlock prompt;
signing returns Locked promptly so the short authentication deadline never
waits on a person. There is no plaintext key file. When no Secret Service is
available, the only alternative is the explicit **Use until this app closes**
choice, which keeps the key in memory and needs a new approval after Crew
closes. Remove from this app retires the local key; host access remains until
revoked at the trusted host console.

`run-window-test.sh --gateway` drives the GTK pages with a recording host,
the signer against a private GNOME Keyring, then the production service
against isolated daemons and a loopback gateway with the synthetic test
certificate. It uses a short private runtime directory under `/tmp` for Crew
and keyring sockets and removes it on exit, and fails on any GTK critical.

## Validation boundaries

The runners create their own X11 display, session bus and private configuration,
state and registry under `target/native-validation`; they do not drive the
operator's desktop or running team. GTK initializes on one test thread.
`GTK_A11Y=test` checks accessibility metadata, not a completed screen-reader
journey. `--client` uses a recording service for deterministic presentation
cases, then the production service against a real disposable daemon and real
shell PTY. Screenshots are named `fixture-*` and `live-local-*` accordingly.

A lagging worker can supply `CREW_NATIVE_UI_TEST_MANIFEST` pointing to an exact
reviewed prerequisite archive below its own `target/native-validation`. The
runner checks that location, preserves private display/state isolation and uses
the locked dependencies. A staged result must name its prerequisite source
checkpoint; it is not a default CLI or all-workspace compatibility claim.

On the integrated main base the receipt/control cases, the production local
service against a disposable daemon and real PTY, the window and keyboard
runners, the smoke run and Clippy for both features pass in the checkout
itself; no staged manifest is needed. The live journey waits for the daemon's
real PTY process before Attach, since the Team row can precede the spawn.
Source review and the final connected accessibility matrix remain.
Gateway verification and Apple execution are separate gates; Apple verification
is deferred by the operator. Native Linux success never implies Apple success.

For local installation, place `data/dev.crew.Crew.desktop` in the applications
directory, `resources/icons/scalable/apps/dev.crew.Crew.svg` in the scalable
application-icon directory and the executable in `bin`. Installed-launcher and
distribution verification are separate from a development build/run.
