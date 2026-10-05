# Native Linux runtime environment correction

The linked GTK shell failed to start because the development environment
provided pkg-config/link directories without runtime loader paths. The original
launcher left `libgtk-4.so.1`, Pango, GIO, GObject and GLib unresolved. The
environment-only correction in `0198317b` derives the selected GTK/VTE public
link closure and prepends it to the spawned command's `LD_LIBRARY_PATH`,
preserving any inherited value. After entering `nix-shell`, the helper re-enters
itself before launching the command. Toolkit pins, Cargo files, product source
and the caller's environment are unchanged.

## Actual binary before and after

Graph supplied the immutable development `crew-desktop` executable with SHA-256
`9e98ed6697325b58a62f81042fc63220e941c20f529558bb8c5aaec473e3cc51` in
`m-46e95653e4f8`. QA copied it under its own artifact directory and used those
same bytes for both runs. This proves the runtime environment correction for
an actual GTK product shell. The source was still changing when that binary
was built; this image does not establish the later `c45703ff` shell's design
or source acceptance.

The old `common.sh`, `linux-env.sh` and `linux-smoke.sh` were copied byte for
byte from `d0b1e4e9` into a private artifact tree. Its isolated Xvfb/D-Bus run
failed before a window mapped, with `libgtk-4.so.1: cannot open shared object
file` in the application log. `ldd` identified five unresolved direct libraries.
With the corrected launcher, the unchanged binary mapped its real `Crew`
window, displayed the disconnected/no-team state, and captured successfully.
The corrected environment's `ldd` had zero unresolved libraries. Both runs'
recorded app and Xvfb children were gone after cleanup.

The retained [evidence](linux-runtime-environment.json) and
[capture](linux-runtime-environment.png) record actual geometry/settings and
tool versions: 1120×720 window on a 1440×960×24 private display, 96 DPI,
GDK scale/DPI scale 1, X11/Cairo, portals disabled; GTK 4.22.4, VTE 0.84.1,
GLib 2.88.3, xwininfo 1.1.6, ImageMagick 7.1.2-29 and D-Bus 1.16.2.
MESA DRI3 warnings are expected under this private Xvfb server; window/capture
checks passed. No operator display or process was used.

## Reproduction and limits

QA's ignored artifacts are under
`/home/nade/workspaces/personal/crew/.crew/worktrees/qa/target/native-validation/linux-runtime-env/`.
They include the immutable executable, old private tooling, both runner/app
logs, before/after `ldd` output, screenshot/settings, and environment checks.
The corrected run was:

```sh
NATIVE_NIXPKGS=/nix/store/ab93n7qqycnn3xfdnav0wnqqigvddd5r-source \
  scripts/native/linux-smoke.sh \
  --out target/native-validation/linux-runtime-env/after \
  -- "$PWD/target/native-validation/linux-runtime-env/crew-desktop"
```

Use a new output directory when replaying. The changed helper passes shell
syntax checking. A separate spawned-command check from a directory containing
spaces preserved a spaced argument and the inherited loader suffix, introduced
no empty library directory, and left the caller's environment unchanged.
No full native/web matrix was repeated.

This is startup/loader evidence for a disconnected development shell.
Connected core/daemon/gateway/action/terminal flows, final shell design,
screen-reader journeys, Wayland/portal behavior and Apple execution remain
separate acceptance gates. No service restart or release installation occurred.
