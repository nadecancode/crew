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
nix-shell -I nixpkgs="$NATIVE_NIXPKGS" -p xdotool --run \
  'crates/crew-desktop/tests/run-window-test.sh --keyboard'
```

Default features are empty. `native-ui` enables Linux-only GTK/VTE and portable
projection types; `local` enables the real local Rust service. The GUI build
runner enables both. Default CLI builds need no GTK development environment.
QA owns the final shared workspace/lock registration; its bounded dependency
changes are delivered separately from the desktop source.

## Presentation and service ownership

The five destinations are Team, Tasks, Messages, Events and Usage. Native lists
and graphs share qualified entity keys and the same Rust projections. The
controller retains selections per destination; native scrolling and graph zoom
remain local presentation state. Graphs consume the shared task layers and
satellite relationships; they do not calculate domain work or eligibility.

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

The local source checkpoint still needs the final receipt/control test run,
source review and the final connected UI/accessibility matrix.
Gateway verification and Apple execution are separate gates; Apple verification
is deferred by the operator. Native Linux success never implies Apple success.

For local installation, place `data/dev.crew.Crew.desktop` in the applications
directory, `resources/icons/scalable/apps/dev.crew.Crew.svg` in the scalable
application-icon directory and the executable in `bin`. Installed-launcher and
distribution verification are separate from a development build/run.
