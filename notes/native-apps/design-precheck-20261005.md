# Native design precheck

Task: `native-design-precheck`. Date: 2026-10-05. Advisory, so that
`native-visual-signoff` finds nothing new. Reference: `notes/native-apps.md` at
`bb04d615` (S02 amended in `ad6ed514`/`43539319`, S08 in `bb04d615`) and the
earlier [shell review](shell-review-20261005.md). No product file was edited.

## What was checked

| Target | Exact source | How |
| --- | --- | --- |
| Linux local app (`graph`, `native-linux-ui`) | `aafbf0dc`; `crates/crew-desktop` is byte-equal in main `19b779bf` | Own locked build of a `git archive` export, `native-ui,local`, binary SHA-256 `796cc4d6…dd02c`. Run against an isolated daemon (`crew` `1d30e447…6df2`) with the scripted team in [team.yaml.in](design-precheck-20261005/team.yaml.in): seven agents, six tasks, one dependency, one failed task, one operator question. Private Xvfb 1440×960 at 96 DPI, Openbox, owned D-Bus and accessibility buses ([launcher](design-precheck-20261005/run-session.sh), [reader](design-precheck-20261005/focus.py), [tools](design-precheck-20261005/tool-versions.txt)). |
| Linux remote screens (`linux`, in progress) | `5c79212f` on `agents/linux` | Own build with `native-ui,local,gateway`, SHA-256 `b8807525…c57843`. S01, S02 and S02b were driven with a synthetic public profile and a throwaway self-signed certificate for `precheck.invalid`, whose key was deleted after the profile was written. S03–S08 need a gateway and Secret Service and were not run. |
| Apple (`ui`, `native-apple-ui`) | `d549bbff`…`c6bd07a4`, `d49d2451`, `2c62896e`, `0149f1c3`, follow-up `7893ced9` | Source and structure only. **Apple execution deferred by operator.** |

All daemon, display and bus processes were owned by the runs and stopped afterwards,
and their private `/tmp/cdp*` roots were removed. The operator's display, daemon and
listener were not used. The interaction transcript is in
[atspi-keyboard.txt](design-precheck-20261005/atspi-keyboard.txt).

## Verdict

**Linux local app: works, but is not visually signed off yet.** Connect, live
Team/Tasks/Messages/Events/Usage, answering a question, compose and cancel, the
Stop confirmation (Cancel focused, destructive Stop), terminal attach with live
output, the dark neutral surfaces and the 560-pixel detail page all work against
the real daemon. However, the content views have not received the visual system
(spacing, hierarchy, status encoding). The graphs do not show topology, and three
interaction defects block keyboard and pointer use: G1–G3.

**Linux remote screens (S01–S02b):** follow the amended S02 (fingerprint entry,
group table with matches/differs text, spelled accessible labels, error summary,
focus on the field). There are five small items, LX1–LX5.

**Apple:** every design item raised during this precheck is closed in source
(A07 receipts, A02 availability, D1–D3, A1–A6). Execution is still deferred.

## Linux local app findings (owner `graph`)

Severity: **High** blocks keyboard/pointer use or misrepresents state;
**Medium** is visible quality or accessibility friction; **Low** is polish.

| ID | Sev | Finding and evidence | Direction |
| --- | --- | --- | --- |
| G1 | High | Clicking a list row's text does not select it. Every row label comes from `views/common.rs::text`, which is `selectable(true)`, so the label takes the click and shows a caret ([caret, no selection](design-precheck-20261005/linux-05-row-click-caret.png)). Only the 18-pixel padding opens the inspector ([edge click](design-precheck-20261005/linux-06-row-edge-inspector.png)). This applies to the Team, Tasks, Messages and Events lists. | List rows use non-selectable labels. Selectable, copyable text belongs to the inspector, evidence and detail panes. |
| G2 | High | Tab through the sidebar focuses and selects each destination in turn, so five Tab presses change Team → … → Usage (window title `Usage — Crew`, [transcript](design-precheck-20261005/atspi-keyboard.txt)). | Make the sidebar one Tab stop. Arrow keys move within it, and only activation (Enter or click) changes the destination. |
| G3 | High | After Stop team → Cancel or Escape, nothing has focus, whether the dialog was opened by pointer or keyboard. The opener turns insensitive while the dialog is open (tooltip "this action is already pending"), so GTK cannot return focus to it. Compose → Escape does return focus correctly. | Move focus back to the opener once it re-enables, or keep the opener sensitive and guard against re-entry. Apply the same rule to Kill, Restart fresh and Restart after limit. |
| G4 | High | The graphs show no topology. Team graph is a single column of agent boxes with no edges, and Fit graph enlarges it so only two of seven agents are visible ([default](design-precheck-20261005/linux-03-team-graph-default.png), [after Fit](design-precheck-20261005/linux-04-team-graph-fit.png)). In the Task DAG, layer 0 is one column and `notify` sits below every node, so it needs scrolling. Zoom changes box width, so text wraps to six lines instead of scaling. A failed, done or working task looks identical apart from one word ([staircase](design-precheck-20261005/linux-08-task-dag-staircase.png)). | Use layered columns with nodes centred per layer and drawn relationship edges. Fit should scale the whole graph into view, and zoom should scale uniformly. Show a status chip with text plus the semantic status colour. List order and graph must agree (spec: Team, Tasks). |
| G5 | Medium | The visual system is not applied to the content. The header text touches the pane edge, and the workspace path, objective, "1 questions" and "agents are not sandboxed" are plain lines. The action FlowBox has uneven heights: Stop team is shorter and clipped at 1120 pixels, and it has no destructive style. Rows read "asker · asker". The inspector uses `title-2` headings with 150-pixel gaps, plain `Key: value` lines, empty labels ("Waiting:", "Dependencies:") and "None"/"Not reported" noise. In light appearance, list selection is GTK blue while the sidebar is green; spec says light uses the green accent. ([team](design-precheck-20261005/linux-03-team-graph-default.png), [inspector](design-precheck-20261005/linux-06-row-edge-inspector.png), [dark](design-precheck-20261005/linux-14-dark-team.png)) | Header: a title (project), muted path, objective, and a counts row whose question count names the asker with Answer. Warnings get warning styling. Rows: id, then role · runtime, a status chip, current work and progress. Inspector: two-column facts, omit absent values, `heading`-sized section titles, 12/24-pixel rhythm. One consistent selection accent per appearance. |
| G6 | Medium | The status line uses `{:?}` formatting (`controller.rs:715`), so it reads "Disconnected · NoneSelected" and "Authenticated · Live" ([start](design-precheck-20261005/linux-01-disconnected-status.png)). | Map the states to the spec's lifecycle copy. |
| G7 | Medium | The sidebar width follows the status text. When "Action pending · awaiting the daemon's result" wraps, the whole main pane shifts by 19 pixels. | Give the sidebar a fixed width and let the status wrap inside it. |
| G8 | Medium | Evidence reads (Transcript, Diff, Inbox tabs) set the global "Action pending · awaiting the daemon's result", which stays until a later action replaces it (`controller.rs:1391` covers every non-input dispatch). | Show read progress inside the tab. The global status is only for mutations. |
| G9 | Medium | Selectable labels are focus stops. Tab visits "Authenticated · Live", the result line and the reason labels, and each FlowBox child adds a second stop (table cell, then button). Restoring focus to the Team summary highlights all of its text ([select-all](design-precheck-20261005/linux-12-summary-select-all.png)). Startup focus is a caret inside the status label. | Make FlowBox children non-focusable and use non-selectable status labels. Pages restore focus to their first control. |
| G10 | Medium | Availability reasons appear in error red before any input: "answer: some text is required" and "body: some text is required" ([question](design-precheck-20261005/linux-10-question-red-reason.png)). In Usage, "no agent is paused at a usage limit" is shown twice. | Show a neutral helper line ("Type an answer to enable Answer") and use error style only after a failed submit. Remove duplicated reasons. Show Core reasons in sentence case without the `field:` prefix (or ask the Core for display text). |
| G11 | Medium | Agent actions are nine equal full-width buttons at the bottom of a long overview. Pause and Unpause appear together, Kill and Restart fresh have no destructive style, and the disabled reasons sit in a block below all the buttons ([actions](design-precheck-20261005/linux-07-agent-actions.png)). | Put Message and Attach terminal in the inspector header and the rest in an Actions menu. Pause/Resume is one control chosen by state. Each disabled item shows its reason beside it. Destructive items are styled as destructive. |
| G12 | Medium | At the default width the inspector holds five or six tabs in about 300 pixels, so the arrows hide Diff, Inbox, Terminal and Host screen. The pane cannot be resized, and the terminal is 34 columns wide ([terminal](design-precheck-20261005/linux-13-terminal-attached.png)). A window size request of 560 gives an actual width of about 775 pixels ([narrow](design-precheck-20261005/linux-15-narrow-request-560.png)), so half-screen tiling on 1366/1440 displays does not fit. | Use a resizable `Paned` with a sensible minimum, or a tab dropdown. Find which child forces the 775-pixel minimum. |
| G13 | Medium | Connections lists the profile as plain lines including "daemon <pid>". All buttons look the same. Reconnect and Disconnect are offered while disconnected. Copy reads "Remote gateway setup is delivered by the separately reviewed connector integration." ([connections](design-precheck-20261005/linux-02-connections-local.png)) | Use list rows with project, path and state, and style Connect as the suggested action. Show actions that fit the state. Use user-facing copy (`linux` replaces this dialog in the gateway build). |
| G14 | Medium | Compose shows the recipient as a raw `*` entry with no visible label, and the editor's accessible name is "Message or answer text". | Add a labelled To selector (Everyone, or agent ids) and name the editor "Message". |
| G15 | Medium | While the terminal has focus, Ctrl+1–5 do not change the destination; VTE takes the keys. Detach and Copy are enabled when nothing is attached, and Attach stays enabled while attached. | Document a way out of the terminal (for example F6 or Ctrl+Shift+1–5) in F1 help. Enable controls by state. |
| G16 | Low | "Questions (0)" stays visible. Usage repeats "Token usage not reported / Context Not reported / Compactions 0" seven times ([usage](design-precheck-20261005/linux-09-usage.png)). Event levels look the same (warn and info). | Hide the zero count. Collapse to "0 of 7 agents report usage". Give each event level a text marker plus colour. |
| G17 | Low | The empty "No team loaded" state has no action. | Add a Choose workspace button that opens Connections. |

## Linux remote screens findings (owner `linux`)

| ID | Sev | Finding and evidence | Direction |
| --- | --- | --- | --- |
| LX1 | Medium | The Public host profile text view grows with its content (about 25 lines). It pushes the fingerprint field and Verify this host below the fold, so a click meant for the fingerprint landed in the profile ([unbounded](design-precheck-20261005/gateway-01-s02-unbounded-profile.png)). | Bound it to about 8 lines with internal scrolling. |
| LX2 | Medium | Hex is shown in upper case (`profile::groups` uppercases) in the echo, the comparison table and the endpoint ID. `crew native status` prints lower case, and Apple shows lower case ([table](design-precheck-20261005/gateway-03-s02b-table.png)). | Use lower case everywhere, so the operator compares identical strings. |
| LX3 | Low | Connection name is required. The cross-platform rule (Apple) is that an empty name defaults to the profile's `name`. | Default to the profile's name. |
| LX4 | Low | After the fingerprint is corrected to "64 of 64", the error "This one has 60." stays until the next submit ([stale](design-precheck-20261005/gateway-02-s02-stale-error.png)). After a failed submit, focus moves to the fingerprint field but the view scrolls to the top summary, so the field is off-screen. | Clear a field's error when it is edited, and keep the focused field visible. |
| LX5 | Low | Verify this host and Trust this host are not styled as primary. Back, Cancel and the dialog's Close all dismiss. Opening a page focuses its selectable heading and highlights its text (same root cause as G9). ([trust](design-precheck-20261005/gateway-04-s02b-trust.png)) | Style the primary action as suggested. Use one exit (Cancel) plus Back. Put page focus on the first control. |

Note for review rather than design: `ProfileInput::check` calls
`GatewayProfile::trust("validation", public, public.tls_leaf_sha256)` to check the
profile's structure. The result is discarded and the real trust call uses the
entered value, but a structure-only parse would avoid code that looks like
self-pinning.

## Apple findings (owner `ui`): all closed in source

| ID | Item | Closed by |
| --- | --- | --- |
| A07 R1/R2 | Retiring a pending send keeps A as an unknown receipt; success after later edits is shown on the receipt | `74d17aef` |
| A02 D1–D3 | No checking flash while typing (last answer kept per control); "Message sent." announced; compose sheet scrolls | `c6bd07a4` |
| A1–A6 | Spelled-out hex labels; device-name error under its field with focus; Remove confirmation; "Access in <workspace>"; expiry with time zone; header traits | `7893ced9` |

Still to check in the deferred Apple run: Send reachable at AX5 on iPhone SE with
the keyboard up, VoiceOver announcements, spelled hex, typing latency of the
availability query, and the Remove confirmation's default button.

## Limits

These Linux observations are real GTK runs on Xvfb with Openbox and the Cairo
renderer. Black popover shadows and Openbox title bars come from the capture
setup, not the product. AT-SPI component positions were not used; the extents were
reported as zero. This was a programmatic accessibility inspection, not an Orca
journey. Remote S03–S08, Secret Service, enrollment and revocation were not
exercised. Apple results are source-only.
