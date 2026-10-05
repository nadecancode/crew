# Crew for Linux

This package owns the native GTK window, local navigation, appearance, and view
composition. The foundation starts genuinely disconnected: it loads no domain
records, discovers no socket, and dispatches no daemon action. The shared client
and approved transport are integrated in the subsequent native UI cards. A
successful launch of this foundation is not a connected application result.

The application uses the provisional local ID `dev.crew.Crew`, display name
`Crew`, and the existing Crew diamond icon. Closing the window or choosing Quit
only closes the client. The package makes no domain ownership, signing, portable
binary, or publication claim.

## Build and inspect

Native Rust is 1.93 (verified toolchain 1.93.1). GTK4-rs is pinned to 0.10.3 with
the `v4_6` API feature: custom native menu-button labels need ellipsis at narrow
widths and enlarged text sizes. The system API floor is GTK 4.6; local execution
uses the reviewed Nix GTK 4.22.4 environment. Older distributions have not been
tested. Resources are embedded by `glib-compile-resources`; startup does not
depend on the working directory or an adjacent source tree.

```sh
export NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source
rustc -Vv
cargo -V
scripts/native/linux-build.sh -j 2
scripts/native/linux-smoke.sh -- "$PWD/target/native-validation/cargo/debug/crew-desktop"
crates/crew-desktop/tests/run-window-test.sh
```

The native runners use their own display, session bus, and configuration/state
directories under `target/native-validation`. They do not drive the operator's
desktop. The GTK test runs on one initialization thread and explicitly uses
`GTK_A11Y=test` to assert native accessibility metadata; it does not establish a
screen-reader journey. Appearance respects native contrast settings and text
scaling, with no animated page transitions.

Default features are empty. The executable requires `native-ui`; GTK is optional
and Linux-only. `local` is reserved for the later approved adapter and currently
enables no transport. Default CLI/workspace builds need no GTK development
environment. Shared workspace/lock registration belongs to QA; its native delta
is delivered separately from these package sources.

## Composition and ownership

`application.rs`, `shell.rs`, `navigation.rs`, `adaptive.rs`, `appearance.rs`,
`resources/**`, `data/**`, and the foundation tests belong to the Linux shell.
`Destination` is a local navigation identity, not a copy of a shared domain type.
The persistent destinations are Team, Tasks, Messages, Events, and Usage.

The Linux content card owns `src/views/{team,tasks,messages,events,usage,inspector,
connections}.rs` and its content tests. The later service/terminal integration
owns its adapter files within this package. `src/views/mod.rs` currently supplies
honest disconnected and no-selection widgets; the content owner adds the module
exports there. No engine or service method stub exists in the foundation.

`Shell::set_main(destination, widget)` supplies a complete native main view.
`set_detail(destination, title, widget)` supplies evidence without opening the
pane or moving focus. `set_workspace_selector(widget)` and `set_status_widget`
provide native header/status composition slots. The content/service owner must
validate scope, selection, grants, and read freshness before replacing a slot.
Graph/terminal views own their scrolling; the shell never wraps them in HTML.

`connect_destination_changed` observes local navigation. `navigate`,
`set_detail_visible`, `toggle_sidebar`, and `back` perform local presentation
only. Page widgets persist across destination changes and pane collapse. At
constrained width or enlarged text, sidebar/details use the same widgets through
in-window navigation. Visible header/menu controls always restore hidden panes.
Main-view focus returns to its retained widget where it still exists, then to a
focusable item or the destination control. Feature views own the more specific
entity-row fallback after a selected row disappears.

Ctrl+1–5 select destinations; Ctrl+B toggles the sidebar; Ctrl+I toggles details;
Alt+Left returns from compact navigation/details; F1 opens native shortcut help;
Ctrl+W closes the window; Ctrl+Q quits. Native sidebar selection remains distinct
from keyboard focus. Appearance and Help stay outside the five destinations.

For local installation, place `data/dev.crew.Crew.desktop` in the applications
directory and `resources/icons/scalable/apps/dev.crew.Crew.svg` in the scalable
application-icon directory of the chosen prefix. Install the executable in that
prefix's `bin`. The embedded icon and CSS remain available without an installation;
distribution packaging and installed-launcher verification are later work.
