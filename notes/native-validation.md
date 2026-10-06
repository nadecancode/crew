# Native validation workflows

This is validation preparation for the operator-approved native iOS, Linux and
macOS scope. It does not report a Crew native application pass. Source/app
validation is a separate integration card after the reviewed implementation
lands. The operator deferred Apple compilation, simulator and device execution
to their Apple runner; no remote workflow, signing or publishing was performed.

## Ownership and reproducible environment

QA owns `scripts/native/**`, this document and `review/native/**`.
The source and feature names follow `notes/native-architecture.md`: portable
`crew-protocol`/`crew-client`; Linux `crew-desktop` with `native-ui,local`; FFI
`crew-client-ffi`, generator `crew-client-bindgen`; Apple project
`apps/apple/Crew.xcodeproj`, schemes `CrewMac` and `CrewIOS`.

Run scripts from any directory. Build/capture output stays under this
checkout's `target/native-validation/`; Cargo uses its `cargo/` directory.
Inherited `CREW_IDENTITY`, `CREW_SOCKET` and `CREW_LAUNCH` are removed. Git's
repository/index/object/ref/configuration context (`GIT_*`) and legacy agent
context (`HERDR_*`) are also removed before
build/test subprocesses. An inherited `GIT_DIR` overrides a fixture's `git -C`
and previously redirected a review run into root metadata. The boundary lives
in sourced `common.sh` and runs again when `linux-env.sh` re-enters after Nix.
Toolchain selection, GTK/pkg-config/runtime paths, caller arguments and unrelated
environment remain available. Output
directories must be new, stay under that prefix and contain no symlink or
traversal components. Missing source/toolchain produces a named nonzero result,
never a passing skip. No script touches the production listener or team.

On this NixOS 26.11 x86_64 host, use the verified Nix input when replaying:

```sh
export NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source
scripts/native/linux-env.sh --headless bash
```

`linux-env.sh` enters `nix-shell` with GTK4/VTE development files, GCC and
pkg-config, adding Xvfb, xwininfo, ImageMagick, D-Bus and AT-SPI for headless
capture. `nix shell` alone did not populate GTK's pkg-config development paths.
The host's default Rust is 1.93.1; `scripts/build-env` supplies a compiler shell,
**not a Rust version pin**. Native bindings explicitly use Rust 1.93.1 by
default; `NATIVE_RUST_TOOLCHAIN` overrides that selection with an installed
toolchain. The CLI/portable Rust 1.82 floor is
unchanged; compilation here does not establish that older-toolchain gate.

The launcher derives runtime library directories from the selected GTK/VTE
pkg-config link closure, then prepends them to the spawned command's
`LD_LIBRARY_PATH`. It preserves an existing value and adds no empty directory
component. This also runs after entering `nix-shell`: development pkg-config
paths alone do not let an executable without Nix runtime paths locate GTK.
The caller's environment and toolkit versions are unchanged.

## Runner environment isolation

Run the bounded environment regression from any directory:

```sh
scripts/native/tests/environment.sh
```

It supplies disposable repository/index/ref/object and Crew-context sentinels,
then observes actual child environments at the common boundary, Linux's available
toolkit path, Nix bootstrap/re-entry and Linux build dispatch. The Nix test hook
deliberately introduces context after the first boundary; re-entry must remove
it again. It also checks spaced/literal arguments, retained Rust/Nix selection,
Cargo and loader paths, ordinary environment and unchanged foreign metadata
sentinels. The compiler/package/bootstrap commands in this regression are
observers, so their success is not a Cargo build, GTK application or Apple pass.
The existing `tests/tooling.sh` remains the real isolated GTK capture/cleanup gate.

The prefix boundary includes Git's local variables, namespace, discovery and
injected configuration, including keys beyond the active configuration count.
It matches the raw fixture isolation policy rather than relying on a list that
can omit another repository selector.
Use explicit per-command repository arguments for an archive read; never export
Git overrides across test runs. Runtime's separate `fixture-git-isolation` owns
the affected raw Rust fixture constructors. The recovery and retained root
backups are documented in [root Git recovery](../review/root-git-recovery.md).
This runner correction changes no product source or immutable earlier evidence;
Apple compilation and execution remain deferred by operator.

## Linux build, run and capture

After the Linux package lands:

```sh
scripts/native/linux-build.sh
scripts/native/linux-smoke.sh -- \
  "$PWD/target/native-validation/cargo/debug/crew-desktop"
```

The build is locked and uses `native-ui,local`. The smoke runner creates a
private Xvfb display and D-Bus session, isolated configuration/state/runtime/
cache directories, and an isolated working directory. Its high display number
avoids a lazy Xwayland server's `:0`. It accepts only a live, mapped, nontrivial
native window, then captures `screen.png`. Early exit, no window and capture
failure are failures. Cleanup terminates only the application/display children
that invocation started, with a bounded grace period.

Display lock/socket checks are not an atomic reservation. If another Xvfb
claims the chosen display first, the runner fails with `private display failed`
and retains its log; it never adopts that other display.

Each image has actual window geometry, tool versions and observed settings:
1440×960×24 screen, 96 DPI, GDK scale 1 and Cairo renderer. Capture disables
desktop portals to keep service activation off the operator's desktop; native
portal/Wayland behavior is a separate real-session gate. These are supported
GTK capture settings, not app appearance acceptance.
([GTK running/debugging](https://docs.gtk.org/gtk4/running.html))

`linux-smoke.sh` proves startup/capture only. The final Linux card must add RPC
assertions for an actual disposable daemon: workspace read, named message send,
question answer, reversible control, confirmation cancellation/confirmation,
and terminal output/input/resize/detach. A screenshot or fixture service is
insufficient for that acceptance. Run destructive cases solely against the
isolated scripted team; do not pass the operator's socket/token/listener.

The transport owner supplies protected Unix setup and isolated daemon tests.
Reuse its real-daemon fixture rather than duplicating credential/bootstrap
logic here. Keep runtime/state/config private per run. Unix socket paths must
fit the platform limit; a long checkout requires a short owned runtime alias,
whose backing state remains in the artifact directory. Record that alias and
cleanup in final integration evidence. Existing `web/e2e/live/harness.ts`
documents the working scripted-team isolation, but its `/tmp` scratch layout
is not the output contract for these preparation scripts.

## Linux app acceptance run

`scripts/native/linux-acceptance.sh` drives the real GTK app against a private
scripted Crew team and checks each effect in the app's accessibility tree and
in the daemon. It is the black-box acceptance runner for `notes/native-apps.md`;
the owners' in-process GTK tests remain separate evidence.

```sh
export NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source
scripts/native/linux-build.sh                      # add --features gateway for remote profiles
./scripts/build-env cargo build --locked -p crew --features native-gateway --bin crew
scripts/native/linux-acceptance.sh -- \
  "$PWD/target/native-validation/cargo/debug/crew-desktop" "$PWD/target/debug/crew"
scripts/native/linux-acceptance.sh --scale 2 --cases L01,L02,L03,L14 -- APP CREW
```

`--out DIR` names a new run directory below `target/native-validation/`;
`--cases` selects case IDs; `--scale 2` doubles the display and `GDK_SCALE`;
`--explore SECONDS` starts the same session without cases, for
`scripts/native/acceptance/probe.sh RUN tree|act|actions|key|type|capture|resize`.
The exit status is nonzero when any case fails. Missing app, CLI or tools
exit 2 with a named reason.

Isolation. The runner clears `GIT_*`, `HERDR_*`, every `CREW_*`, `CLAUDE*`,
`CODEX*`, `ANTHROPIC*`, `OPENAI*`, the desktop session, D-Bus, AT-SPI, Wayland
and X variables. Configuration, state, cache and data homes live in the run
directory. `XDG_RUNTIME_DIR` is a short private `/tmp/crew-acc.*` directory so
daemon sockets fit the Unix path limit; `runtime-alias.txt` records it and the
session removes it. `claude` and `codex` resolve to private stubs that exit
127, so a daemon usage probe never reads the operator's provider
configuration. Inside `linux-env.sh --acceptance` and `dbus-run-session`, the
session starts its own Xvfb display (1440×960, 96 DPI, Cairo, no portals), a
private AT-SPI bus whose address the driver checks before acting, and openbox.
Cleanup stops the app, runs `crew stop` and then stops any remaining process
whose environment carries this run's runtime directory, never a process
matched by name. `cleanup.txt` records each step and must end with an empty
`remaining=`.

Fixture. `crew run` starts a real daemon with five `crew scripted-agent`
agents on daemon-owned PTYs (`pty` backend, `shell` runtime, no sandbox, Git
workspace): `dev` executes `schema` with `docs` queued behind it, `reviewer`
reports Blocked on `release`, `asker` asks the operator a question, `echo`
replies to messages and `idle` takes controls. `api` depends on `schema`.
Nothing calls a model provider.

Driver. `acceptance/driver.py` launches the app, then for each case uses
AT-SPI to find controls by accessible name and role, invokes buttons through
their native action, types with `xdotool` after a pointer click into a field
(GTK 4 implements no AT-SPI focus grab), uses real `Tab`/shortcut keys for
keyboard checks and queries `crew status|messages|questions --json` for daemon
effects. Every case writes its checks to `results.json`/`results.md`, its
screenshots to `captures/`, failing trees to `trees/`, tracebacks and the Tab
sequence to `logs/`. A case that fails does not stop later cases; leftover
dialogs are dismissed with Escape first.

| Case | Rows | Checks |
| --- | --- | --- |
| L01 | N01 N02 C01 | Starts disconnected; Connections lists the private workspace, has no code/token/key field and starts nothing; connect reaches Live; objective, counts and local-operator copy. |
| L02 | N15 N17 | Ctrl+1–5 select Team, Tasks, Messages, Events, Usage; selected row and window title agree. |
| L03 | N03 | Graph, list and inspector agree on executing and started-Blocked current work. |
| L04 | N04 | Every task listed; a queued card opens without changing any task status; return keeps the agent selection. |
| L05 | N05 | Question names its asker; typed answer clears it in the daemon and reaches the asker; count returns to 0. |
| L06 | N06 | Question and answer listed; a row opens its detail. |
| L07 | N07 | Composer sends exactly one message to `echo`; the reply arrives in the daemon and in the app without reload. |
| L08 | N08 | Pause and Unpause change the daemon's `paused`; unsupported Compact is disabled with its reason. |
| L09 | N09 | Kill, Restart fresh, Stop team name target and consequence; Cancel is initial focus; Escape/Cancel send nothing; focus returns to the opener. |
| L10 | N10 N17 | Events listed; every agent evidence tab is exposed to assistive technology; Transcript opens. |
| L11 | N11 | Terminal page reachable; Attach renders the PTY; typed input reaches the agent's PTY and is rendered; resize keeps the terminal; accessible text. |
| L12 | N12 | Unreported usage says Not reported, never zero. |
| L13 | N13 | Ctrl+W leaves the team running; reopen reconnects; daemon SIGKILL leaves Live with honest, Last-known labelled content; restarted team returns to Live. |
| L14 | N15 | 1024×600 and 560-wide windows keep destinations reachable; Ctrl+B toggles the sidebar. |
| L15 | N17 | F1 help; 45 Tab stops never lose focus, all named, one stop per control, no static text, no destination change, no busy error. |
| L16 | N09 N13 | Confirmed Stop team stops the daemon; the app leaves Live and keeps running. |

These cases verify keyboard operation, accessible names/roles/states and daemon
effects through AT-SPI. They are not a screen-reader (Orca) listening test;
high-contrast themes, the on-screen keyboard and enlarged text through system
settings remain separate N17 checks. A private-settings `gtk-xft-dpi` attempt
did not visibly change text size, so no large-text result is claimed;
`--scale 2` covers display scaling.

### Gateway suite

`--suite gateway` needs an app built with `--features gateway` and a CLI built
with `native-gateway`. The session adds a private GNOME Keyring (Secret
Service) on its own session bus, unlocked with a random password kept in the
run directory. The driver plays the trusted host console with the real
`crew native` CLI: it copies the synthetic TLS fixture from
`crates/crew-client-transport/tests/fixtures/gateway/` into the private runtime,
starts the gateway on a free `127.0.0.1` port, and reads `status`, `enroll`,
`pending`, `approve`, `grant`, `revoke`, `devices` and `stop` as JSON
(`logs/host-console.log`). It never touches the operator's listener, keyring
or devices. The app types the host's actual status output and fingerprint, so
the fingerprint, device key and comparison code are real values of this
isolated run, not operator credentials.

| Case | Rows | Checks |
| --- | --- | --- |
| C01 | N01 C01 | Local and remote sections; opening Connections starts no gateway. |
| C02 | N01 C02 | Only the three public-profile fields; empty submit names the field and marks it invalid; malformed profile keeps other input and requests nothing. |
| C03 | N01 C03 | Mismatched fingerprint names the differing group, offers no trust or override and sends no proof; the match shows all 16 groups, then Trust this host. |
| C12 | N01 C12 | Private Secret Service store ready; memory-only storage not offered while it is; the label is not presented as verification. |
| C04 | N01 C04 | No host window: one bounded refusal, no pending intent, no retry. |
| C05 | N01 C05 | After `crew native enroll`, all four code groups equal the host's pending code, each with its own accessible label; the host sees the typed label; no device-side Approve. |
| C06 | N01 N02 C06 C09 | Host approves Read for the workspace; app shows Access granted and only that workspace. |
| C08 | N01 C08 C18 | Read, Control and Terminal disclosures and the no-sandbox sentence; Disconnect, revoke instructions and local removal offered. |
| C07 | N08 N11 C07 | Read-only device: Pause and Attach disabled, Control and Terminal named as required, no message dispatched. |
| C15 | N09 C15 | Host adds Control; the app retires the old authority, reopens, and Pause through the gateway reaches the daemon. |
| C14 | N13 C14 | Gateway stop leaves Live with consistent status and the team running; after restart the saved profile reconnects. |
| C16 | N13 C16 | Host revocation ends Live, the app explains it and the team keeps running. |
| C18 | N13 C18 | Remove from this app explains local key retirement and the remaining host access. |

C09 empty/filtered listings, C10 rejected or expired comparisons, C11 lost
approval reply, C13 network loss, C17 expiry and renewal and the screen-reader
part of C19 are not yet scripted. The unavailable-keyring path of C12 needs a
run without the private keyring.

### Results

All results come from exact exports of main built in this worktree, with GTK
4.22.4, VTE 0.84.1, AT-SPI 2.60.6, pyatspi 1.9.0, openbox 3.6.1, xdotool
4.20260303.1, GNOME Keyring 50.0 and the Nixpkgs above. Cleanup left no
process and removed the runtime directory in every run listed.

| Run | Source and binaries | Result |
| --- | --- | --- |
| `acceptance-local-8` | main `19b779bf` (`crates/crew-desktop` byte-equal graph's reviewed `aafbf0dc`); app `07a976ed…`, CLI `0cfb067e…` | Local 90/101 checks: L02–L08, L12, L14, L16 pass. |
| `acceptance-local-8-scale2` | same, `--scale 2` | L02, L03, L14 pass; L01 the same copy checks. |
| `acceptance-local-10` | main `b3238139` (gateway app); app `a724c362…`, CLI `6751cd77…` | Local 90/101: as above, and the busy error is gone (graph `fc26357d`). |
| `acceptance-gateway-8` | same `b3238139` binaries | Gateway 61/62 checks: every case except C14 passes. |

| Finding | Case | Observed on `b3238139` |
| --- | --- | --- |
| Disconnected status shows an internal state name | L01 | `Disconnected · NoneSelected` |
| Local authority copy differs from the S-section contract | L01 | `Authenticated · Live`, not `Local operator · full Crew access` |
| Focus is not restored after Escape on Kill | L09 | No focused accessible after the dialog closes |
| Agent tabs scrolled out of the tab strip are absent from the accessibility tree | L10 | Terminal and Host screen missing at 1280 px |
| Attached terminal exposes no accessible text until new output | L11 | Empty text, then only later output |
| Retained content is not labelled Last known while reconnecting | L13 | Header and graph unchanged; status `Reconnecting · Reconnecting` |
| Header actions are two Tab stops each | L15 | Table cell, then button, for New message, Questions, Stop team, Refresh |
| Tab through the sidebar selects each destination | L15 | Traversal from Team ends on Usage |
| Static labels are Tab stops, two with no name | L15 | Sidebar status labels, every Usage label |
| Stopped gateway leaves a contradictory status | C14 | `Disconnected · Live` |
| Fast keyboard navigation reported the client busy | L15 | `The client is busy. Try again.` on `19b779bf`; fixed by `fc26357d` |

The inspector's lists also kept glyph remnants after switching agents
(`captures/L08-paused.png`); content attributes these to Cairo damage under
Xvfb. The summary read `1 questions`. Neither is asserted. Findings went to
their owners: copy and lifecycle to content, keyboard and focus to scheduler,
the request backlog to graph. Content's pending polish renames controls
(`Stop team…`, `Kill…` and `Restart fresh…` in an Agent actions menu,
`Attach terminal`, Connections `Connect`); the cases need those names once
it lands.

## Shared core and one conformance corpus

After N1 lands, run:

```sh
scripts/native/check.sh
scripts/native/check.sh --conformance
```

The first command runs locked protocol/client tests, unchanged TUI render
tests, WASM portability checks and normal dependency trees. The second also
consumes the reviewed fixture owners' checkers in place:
`fixtures/native/state/check.mjs` and `fixtures/native/actions/check.ts --wire`.
Node/Bun and the C linker are required. Both receive
`CREW_NATIVE_FIXTURE_CACHE=target/native-validation/fixture-cache` and the
worktree-local Cargo target. State/action data is not copied or reauthored.

Existing checkers establish baseline DTO/TUI/web and Request-byte compatibility,
not the future native engine's behavior. The engine/transport owners add actual
reducer/action/daemon consumers, stale-attempt and authorization regressions.
Retain explicitly labelled web completed-work differences. A first real-daemon
consumer must execute daemon/index/admission expectations and treat mismatches
as fixture defects. N1 relocates legacy source anchors; source references need
the fixture owners' reviewed relocation resolution before integrated replay.

Reuse current security suites (`web_wire`, `web`, `web_server`, `operator_auth`)
and the transport owner's new native regressions. Their passing baseline is
not a scoped-gateway or native-adapter pass. Device trust, workspace grants,
individual revoke/teardown, bad proof, version mismatch, snapshot/reconnect,
stale attempt and no mutation/input replay remain actual integration gates.

## Apple runner commands: verification deferred by operator

Prerequisites: committed Apple/FFI source, macOS with selected Xcode, Rust
1.93.1, an ordinary Git checkout of the reviewed commit, and the Rust targets
required by the chosen platform. Mac packaging
requires both `aarch64-apple-darwin` and `x86_64-apple-darwin`. iOS packaging
requires device `aarch64-apple-ios` and Apple-silicon simulator
`aarch64-apple-ios-sim`; Intel simulator support is an explicit additional gate.
The project owns iOS 17/macOS 14 deployment settings and Swift 5 language mode.

```sh
# Linux or Mac: real host generation and a stale-source comparison, once FFI exists.
scripts/native/apple-bindings.sh --check-source

# Apple runner, build and tests in a private source copy:
scripts/native/apple-build.sh --target macos
scripts/native/apple-test.sh --target macos --destination 'platform=macOS'
scripts/native/apple-build.sh --target ios
xcrun simctl list devices available
scripts/native/apple-test.sh --target ios \
  --destination 'platform=iOS Simulator,id=THE_ACTUAL_LISTED_UDID'
```

Use an actual listed simulator ID. Xcode's build/test commands take the shared
scheme and destination; result bundles remain under the run directory.
([Apple command-line testing](https://developer.apple.com/library/archive/technotes/tn2339/_index.html),
[Apple test results](https://developer.apple.com/documentation/xcode/running-tests-and-interpreting-results))

`apple-bindings.sh` builds the pinned host library and bindgen with the same
lockfile, generates Swift/headers/modulemap, and diffs the committed generated
sources. Product generation runs from `crates/crew-client-ffi` with absolute
library/output paths and `generate --no-format`, matching the source owner's
generation command. UniFFI 0.31 otherwise discovers optional Swift formatters,
which can change generated bytes between Linux and Apple hosts. No formatter
dependency is required. Standalone feasibility probe replay runs from its own
probe directory; its immutable evidence is unchanged.

Differences fail. The FFI source owner regenerates source separately.
Darwin `--target macos|ios` builds the matching static-library slices and stages
`CrewClientFFI.xcframework` with checksum/Info.plist evidence. iOS excludes
`local`; its exported thin local connector must return `UnsupportedTransport`.
That ABI consistency is the binding owner's product test, not a shell inference.

`apple-build.sh`/`apple-test.sh` verify staged checksums and generated-source
freshness, read committed Apple files into a private artifact copy, install
the staged framework there, and invoke Xcode with private DerivedData/package
checkouts/result bundles. They record Xcode/developer path/SDKs/destinations,
Rust and lockfile identity. They never modify `apps/apple/**`. `--bindings DIR`
reuses an already staged, verified binding build. `--preflight` still requires
the real project/bindings; absence is not success. The optional developer
source-tree artifact installation command belongs to the Apple source owner.

Unsigned Debug/simulator checks are distinct from signed device, local Mac
transport, iOS gateway lifecycle, accessibility and distribution verification.
No Apple compile/link/simulator/device or signing result is claimed here.

## Preparation evidence, 2026-10-05

Executed on QA's worktree at `/home/nade/workspaces/personal/crew/.crew/worktrees/qa`:

```sh
bash -n scripts/native/*.sh scripts/native/tests/tooling.sh
NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  scripts/native/tests/tooling.sh
```

The actual GTK toolkit probe mapped at 480×240 and captured successfully.
The helper also rejected a command exiting before a window and a live command
with no window; all recorded application/display children were gone afterward.
Measured GTK DPI was 98,304 (96×1,024), font `Sans 10`. The probe is explicitly
not a Crew app or native daemon-flow acceptance. Tool versions were GTK
4.22.4, VTE 0.84.1, GLib 2.88.3, xwininfo 1.1.6, ImageMagick 7.1.2-29 and D-Bus
1.16.2. The retained MESA DRI3 warnings are expected with this private Xvfb
server; the probe used Cairo and its mapped-window/capture checks passed.
The operator's Wayland display/team/listener were preserved.

Artifacts: ignored `target/native-validation/tooling-1TPWyk/`; replay log
`target/native-tooling-prep-final.log`. Source/tool absence gates return 2 with
named reasons on this checkpoint: no `crew-desktop`, no `crew-protocol`/
`crew-client`, no `crew-client-ffi`/bindgen, and no macOS/Xcode. Apple shell
syntax and the Linux rejection path were checked; Apple execution remains
**deferred by operator**. Final source, conformance, native accessibility,
real-session Wayland and real-daemon GUI verification remain separate gates.
