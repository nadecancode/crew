# Multi-workspace frontend review

Reviewed `10591074`, `c9fb49c6` and `8d27deb7`, on protocol contract
`3e06b1bc`, against the approved `notes/web-multi.md`. Changes requested
for the three findings below.

## P2: Later workspaces overwrite preferences already chosen

At `web/src/prefs.ts:164`, `adopt()` decides whether keys and panel width
are established by consulting `kept.had()`. That reads only the object
originally loaded from the server. Neither adoption nor subsequent operator
changes update that information.

Start with empty server preferences, visit a workspace whose legacy values
are `keys: false, panel: 700`, and then visit one with `keys: true, panel:
300`. The second workspace replaces the first values, although migration
should supply defaults once. Explicitly disabling shortcuts and changing
panel width also loses to the next workspace's old settings. This can
silently turn shortcuts back on.

`web/test/reviewer-multi-prefs.test.ts` reproduces both failures. Its third
test is a passing control: writing `last` must not suppress migration of
either missing preference, and missing fields may be supplied separately.
Track establishment per field, including explicit operator changes.

## P2: The copied join command changes or splits the workspace path

`web/src/views/Switcher.tsx:193` interpolates the display abbreviation
`homeDir(workspace)` directly into a shell command. A space splits the path,
an apostrophe produces a shell syntax error, and a directory under another
user's home is rewritten to the current user's home. The copied command
therefore fails or joins a different workspace. Shell metacharacters would
also be interpreted instead of preserved as part of the directory.

`web/test/reviewer-multi-command.test.tsx` reads the rendered command and
executes it against a harmless shell function that prints its arguments;
it never invokes crew. All three cases fail:

- `/srv/crew work/billing` becomes two arguments.
- `/srv/crew's work/billing` is an unmatched quote.
- `/home/another-operator/project` becomes the reviewer's own `~/project`.

Copy the exact absolute path with shell quoting. Abbreviating the displayed
directory should not affect the command's argument.

## P2: The copy command is inaccessible by keyboard

`web/src/views/Switcher.tsx:132` closes the popup on Tab before the browser
can focus any of the copy buttons following the listbox. Arrow navigation
only visits joined workspaces. A keyboard user can open and traverse the
switcher, but cannot reach its action for an unjoined workspace.

The first test in `web/e2e/reviewer-multi.spec.ts` opens the switcher with
`w` and presses Tab. The copy button is removed instead of receiving
focus. Keep the popup open while focus moves within it and close it when
focus leaves. A second browser test isolates activation by focusing the
copy button directly: Enter still does not copy, because the document's
global Enter shortcut intercepts it. Both browser tests fail. Preserve
native button activation as well as the route to the button.

## Validation and limits

Independent baseline checks on the submitted frontend passed:

- `bun run build`: reproduced the committed 574,737-byte bundle.
- `bun run test`: 144 tests in 18 files.
- Playwright mock and performance projects: 150 tests; P17 switch p95
  48.4 ms, P19 zero index frames and calls over 60 seconds.
- `cargo test -p crew --test web_bundle`: both tests.

Reviewed the index and workspace connections, snapshot and retry behavior,
proof gates, page and workspace lifetimes, routing, preferences, switcher,
terminal changes, mock transport and regression coverage. The existing
tests exercise the previously identified remove/rejoin and stale-outcome
races and passed independently.

The new regressions have seven failing cases and one passing control on
the reviewed commit. Run
their unit tests with `bun run test test/reviewer-multi-prefs.test.ts
test/reviewer-multi-command.test.tsx`, and their browser tests with
`bunx playwright test --project=mock e2e/reviewer-multi.spec.ts` after building
the bundle. This review used the mock server. Validation against the real
shared server remains pending its implementation.
