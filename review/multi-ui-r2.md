# Multi-workspace frontend: second review

Reviewed `7d5182c9`, comprising fixes `bced0dec`, `28475004`, `2f6b0679`
and `7d5182c9`. All three P2 findings in `review/multi-ui.md` are closed.
The frontend is acceptable with the minor issue below; this is not an
approval of integration with the real shared server, which remains pending.

Preference migration now records establishment per field, both when it is
adopted and when the operator sets it. Writing `last` does not establish
the other preferences. The join command preserves the absolute directory
and quotes it for the shell. Tab now reaches the copy buttons, Enter
activates them, and global shortcuts do not consume keys inside the
switcher.

Independent checks passed: the build reproduced the committed 575,443-byte
bundle; all 153 unit tests passed; 21 browser tests covering the original
reviewer regressions, workspace switching and keyboard behavior passed;
both Rust `web_bundle` tests passed. The author's full mock and performance
run reports 153 tests passing, with P17 switch p95 49.2 ms. The previous
review independently ran all baseline mock and performance tests.

## P3: Clicking the open switcher trigger reopens it

The new `onFocusOut` in `web/src/views/Switcher.tsx` closes the popup when
focus moves to its trigger. This happens on mousedown. The trigger's click
handler then toggles the now-false state to true, reopening the popup.
Clicking the trigger to dismiss it no longer works; Escape and clicking
outside still work.

The added case in `web/e2e/reviewer-multi.spec.ts` opens the switcher by
click, clicks the trigger again, and expects the listbox to disappear and
`aria-expanded` to become false. It fails on this revision, leaving one
listbox. Thus the focused browser run was 21 passed and this one new case
failed. Preserve the trigger click's toggle semantics when handling focus
leaving the popup.
