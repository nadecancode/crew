# Linux menu accessibility classification

Task: `native-menu-accessibility`. Date: 2026-10-05. This follows
[NSR-L03 in the early shell review](shell-review-20261005.md#nsr-l03--native-menu-labels-are-unresolved-in-the-at-spi-probe).
The original observations remain unchanged in that record.

**Classification: GTK 4.22.4 provider name exposure, combined with an incomplete
interpretation of the original probe. No Crew-specific missing-menu-label defect
was demonstrated.** The empty AT-SPI `Name` values are real, including in raw
D-Bus replies. However, the related label objects expose the full strings through
the AT-SPI Text interface. An actual Orca 50.2 process generated the six Crew menu
labels and control states while handling real keyboard events. Its speech
transport was unavailable: these are event and speech-generation observations,
not evidence of audible output or a complete screen-reader journey.

No Crew product correction is required by this diagnostic. The raw-name
compatibility limitation remains recorded; a consumer that reads only `Name`
still receives an empty string. `graph` and `qa` must revalidate the menu on the
exact integrated application/toolkit during N17 and final native visual review.
Do not patch GTK private menu children or infer a universal assistive-technology
pass from this result. No connected-app acceptance row passes here.

## Exact target

The disconnected shell is `c45703ff` plus `d4d31a13` and the reviewed empty-focus
correction `706bd6358b4e9d4390b3ac907350c0a5ecc70ce3`. The reused independent binary
has SHA-256 `8c109210360dff7788b021fae83b1ae07313669d34be9a3a6b9d638e4f8e49dc`.
Its build and registration provenance are preserved in the earlier review.
`src/shell.rs` has SHA-256
`d11b551bdce653978b7d0769ddf324c3a1805debf533d1d5c9326699613bc8d3`.

QA confirmed no menu/toolkit change in `e27cb12d` (`m-92ea732978c8`); an independent
`jj diff --from 706bd635 --to e27cb12d --summary crates/crew-desktop` is empty.
Graph also confirmed its active source retained these application/destination
menus (`m-1c9d6de54817`). This is applicability evidence for those menus, not a
claim to have executed the later integrated binary.

The exact approved runtime helper is `0198317b`. The pinned Nix environment
provided GTK 4.22.4, GLib 2.88.3, AT-SPI 2.60.6, pyatspi 2.58.1, Python 3.14.7 and
Orca 50.2. Rendering used a private Xvfb display, Openbox, X11/Cairo and scale 1.
No operator display, Crew daemon, listener, team, credential or root Git state
was used. Full source/tool/hash records: [evidence.json](menu-accessibility-20261005/evidence.json)
and [tools.txt](menu-accessibility-20261005/tools.txt).

## Observations

| Check | Result |
| --- | --- |
| Crew application menu | All six `menu item` objects have empty cached, cache-cleared and raw D-Bus `Name`. Each `LABELLED_BY` target supplies its full visible string through `Text.GetText(0, -1)`. |
| Meaning of `invalid` | The related objects remain showing/visible, respond to D-Bus calls and expose Text. Raw `GetRoleName` says `presentation`; libatspi reports `invalid`. They are not missing or defunct objects. |
| Independent native comparison | A separate unstyled Python application uses only GTK/Gio controls, no Crew code, CSS or Rust bindings. Its three model-menu entries have the same blank Name/text-relation behavior. Its ordinary button and checkbox have nonempty raw Names. |
| Actual reader output | Orca's real GTK event pipeline generates Show Sidebar, Show Details, Appearance, Keyboard Shortcuts, About Crew and Quit. It also generates submenu labels, checked/not-checked states, Team/Tasks and the standalone comparison labels. This is generated speech text, not heard audio. |
| Keyboard and focus | Down visits all six application items. Escape returns focus to the named Application menu toggle. Submenu navigation/Return changes appearance from Light to System; reopening exposes System checked. Escape returns to the named opener. |
| Destination selection | Team is initially the selected list row. Opening its destination menu, Down, Return selects Tasks; the list's selected state and focused `Destination: Tasks` control agree. |
| Independent check state | The comparison menu's checked item changes from checked to unchecked after Return. Labels remain available through Text in both states. |

The [raw menu replies](menu-accessibility-20261005/menu-wire.json) retain Name,
role, state, relation and text data, including both pyatspi and direct D-Bus
queries. [Keyboard records](menu-accessibility-20261005/keyboard.json) retain
the focused and selected objects and checked transitions.
[Orca output](menu-accessibility-20261005/orca-speech.txt) preserves exact log lines
with original line numbers and the full log hash. Complete raw runs remain in
the ignored `target/native-menu-a11y/run01`; their hashes are in the manifest.
The retained assertions check these observations, not general accessibility.

## Why the original probe was inconclusive

The Crew source supplies ordinary Gio menu labels in
`crates/crew-desktop/src/shell.rs::application_menu` and `destinations_menu`.
GTK's [`GtkModelButton` implementation](https://github.com/GNOME/gtk/blob/4.22.4/gtk/gtkmodelbutton.c)
creates its internal label with the presentation role and relates the menu item
to that label. The pinned [`name computation`](https://github.com/GNOME/gtk/blob/4.22.4/gtk/gtkatcontext.c)
excludes author-supplied names for presentation roles; this path produces the
observed empty computed Name. The GTK source files extracted from the exact Nix
4.22.4 tarball match the upstream files byte-for-byte. The only selected Nix patch
concerns a 32-bit Vulkan null handle, not this code.

GTK's [`AT-SPI role mapping`](https://github.com/GNOME/gtk/blob/4.22.4/gtk/a11y/gtkatspiutils.c)
maps presentation to `ATSPI_ROLE_INVALID`. That explains the original role
string; it does not establish a dead relation. Orca's
[`get_displayed_label`](https://github.com/GNOME/orca/blob/50.2/src/orca/ax_utilities.py)
reads each related object's Name or its Text. Its packaged source matches that
upstream 50.2 file, and the observed speech-generation output exercises this
fallback. Sources checked on 2026-10-05.

The original probe recorded only names and role strings. It neither requested
the relation's Text nor ran the reader's label resolution. Cache clearing does
not fix the blank Name, and switching from pyatspi to raw D-Bus does not change
it. The native control comparison isolates this behavior from Crew. The reader
observation resolves the narrower question of whether this GTK/Orca combination
can recover these labels; other consumers and actual audio remain unverified.

## Bounded reproduction

Use the hashed `706bd635` binary and approved `0198317b` runtime helper. All output
paths must be inside the invoking workspace. The retained
[session launcher](menu-accessibility-20261005/run-session.sh) starts only owned
processes, clears Git/Crew environment overrides, and creates private XDG homes
and buses. The [comparison](menu-accessibility-20261005/comparison.py) has no Crew
dependency. Supply the GTK/VTE development and Xvfb/D-Bus environment using the
pinned Nixpkgs path in `tools.txt`, with these additional packages:

```sh
nix-shell --pure -I nixpkgs=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  -p gtk4 vte-gtk4 pkg-config gcc xorg-server xwininfo xdotool imagemagick \
  dbus at-spi2-core gobject-introspection openbox adwaita-icon-theme \
  '(python3.withPackages (ps: [ ps.pyatspi ps.pygobject3 ]))'
```

Inside that shell, use absolute paths for `menu_helper`, `menu_binary`,
`menu_evidence` and a fresh `menu_run`. Retain the selected Adwaita icon data and
pinned librsvg loader cache, as in the previous shell review. In one terminal:

```sh
bash "$menu_helper" --headless dbus-run-session -- bash \
  "$menu_evidence/run-session.sh" "$menu_run" "$menu_binary" \
  "$menu_evidence/comparison.py"
```

In a second terminal, source only that owned `session.env`. The
[probe](menu-accessibility-20261005/probe.py) rejects other accessibility buses.
Open a menu through its native accessible action, then inspect actual replies:

```sh
source "$menu_run/session.env"
python3 "$menu_evidence/probe.py" activate 'Application menu'
python3 "$menu_evidence/probe.py" > "$menu_run/menu.json"
python3 "$menu_evidence/keyboard.py" "$menu_run" "$menu_app_pid"
```

`menu_app_pid` is the recorded `app_pid` in `processes.txt`. The
[keyboard driver](menu-accessibility-20261005/keyboard.py) activates only that
one visible Crew window. Initial appearance changes which submenu item receives
focus first; read the generated records rather than assuming an index.
For the reader trace, run the [Orca launcher](menu-accessibility-20261005/run-orca.sh)
against the same private session with a short workspace-local runtime path:

```sh
bash "$menu_evidence/run-orca.sh" "$menu_run" "$PWD/target/am-replay"
```

This uses the pinned Orca/AT-SPI store paths recorded in the launcher, memory
GSettings, a workspace data home and an absent private speech socket. It must not
use `--replace`. Repeat keyboard traversal while it runs; inspect generated text,
not an assumed audible result. End the reader with `touch "$menu_run/stop-orca"`,
then end the session with `touch "$menu_run/stop"` and verify owned PIDs are gone.

## Harness limits and cleanup

The first Orca attempt omitted `XDG_DATA_HOME` and created an empty customization
stub, its bytecode and the `sounds` directory under `~/.local/share/orca`.
It was stopped. The orchestrator authorized a reversible, attributed restoration
in `m-b0010d2a4415`: all five entries had matching creation times, modes and known
hashes; two inventory checks found no symlinks or new user changes. The tree was
moved intact to the ignored `target/native-menu-a11y/attributed-orca-backup`,
without deletion; the original path is absent. The
[before/after record](menu-accessibility-20261005/harness-restoration.json)
preserves that evidence. The retained reproduction launchers now isolate the
data home as well as config/cache/state/runtime. The second Orca run used those
explicit data/speech safeguards and supplies the retained reader excerpt.

Both Orca runs report an unavailable speech client. No speech process was
spawned; existing host speech processes were left untouched. There was no
auditory or braille evaluation. Native component focus-grab was unsupported in
one exploratory probe, so traversal used the existing accessible menu action
and real X keyboard events; no product failure was inferred from that call.

All 13 recorded application, comparison, display, window-manager, accessibility,
session, reader and diagnostic process PIDs are absent, as are the private X
socket and lock; see [cleanup.json](menu-accessibility-20261005/cleanup.json).
No product source, toolkit version, registered service or operator preference
change is part of this artifact. The acknowledged temporary data-directory
side effect and its restoration are recorded above, not hidden by that scope.
