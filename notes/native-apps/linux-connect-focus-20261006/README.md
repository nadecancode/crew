# Linux focus after connecting

Task `native-linux-connect-focus`, from the review of main `1e3b8a37` and
`2da46639`. `run-window-test.sh --interaction` failed 2 of 4 times at
"connected, focus on the connected page's first control". Focus stayed on an
unnamed `panel` at depth 4, the split pane between the main page and the
details.

## Cause

Each new workspace scope sets the destination pages again
(`Controller::notice`). `Shell::set_main` replaced a slot's child even when it
was the same page, so the page was removed and re-added. GTK 4.22 then defers
moving focus away from a removed widget to the next painted frame
(`gtkwindow.c`, `_gtk_window_unset_focus_and_default` and
`maybe_unset_focus_and_default`). At that point it grabs the first focusable
widget above the removed one. Nothing between the page and the split pane takes
focus, and `GtkPaned` is focusable (its resize handle), so focus went to the
split pane. If the app's own page focus request ran before that frame, it found
focus still in the page and did nothing, so focus stayed on the handle.
Whether it happened depended on frame timing.

`tests/window.rs` reproduces it without timing: setting the shown page again
left focus on `GtkPaned` in 2 of 3 attempts before the fix.

## Fix

- A slot keeps the child it already shows (`replace_child`), so the page is
  never taken out and put back.
- Focus that reaches the split pane's handle by any route other than the pane's
  own keys (F6, F8) moves into the page's first control.
- `tests/interaction/atspi.py` prints the accessible path of an unnamed focused
  object, so a failure names the widget.

Each change has its own assertion in `tests/window.rs`. Without the slot change,
"setting the shown page again leaves it in place" fails. Without the handle
rule, focusing the handle does not reach the page. Without both, focus is left
on `GtkPaned`.

## Runs

[interaction-runs.txt](interaction-runs.txt) lists ten consecutive
`--interaction` runs on the fix. [atspi-keyboard.txt](atspi-keyboard.txt) is
the transcript of the last one.
