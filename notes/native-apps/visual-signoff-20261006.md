# Native visual sign-off

Task: `native-visual-signoff`. Date: 2026-10-06. Target: main `3c6c3d32`, the
approved `native-app-integration` result. Reference: `notes/native-apps.md`
(S02 amended in `ad6ed514`/`43539319`, S08 in `bb04d615`), the
[early shell review](shell-review-20261005.md), the
[menu classification](menu-accessibility-20261005.md) and the
[design precheck](design-precheck-20261005.md). The designer edited no product
file.

## Verdict

**Linux: conditionally signed off.** The connected app meets the
experience contract across the five destinations, inspector, evidence,
questions, messages, confirmations, terminal, pairing, grants and revocation.
All precheck findings (G1–G17, LX1–LX5, V1–V8) are closed on this target.
Three fixes are required before final acceptance (R1–R3 below). Each goes back to
its owner and is rechecked by the designer.

**Apple: source structure and runner plan accepted.** `apps/apple` on main is
byte-equal to `7893ced9`, where every Apple design item from this review cycle
was closed. **Apple execution deferred by operator**: no screenshot, build,
simulator, device, Keychain or VoiceOver result exists or is claimed.

## What was run

| Run | Source and binaries | Result |
| --- | --- | --- |
| Acceptance, local suite (`scripts/native/linux-acceptance.sh`) | Own locked build from an export of `3c6c3d32`: `crew-desktop` `native-ui,local,gateway` and `crew` `native-gateway` ([hashes](visual-signoff-20261006/tool-versions.txt)) | L01–L16, 101/101 checks ([results](visual-signoff-20261006/acceptance-local-results.md)) |
| Acceptance, gateway suite (real `crew native` host console, loopback gateway, private keyring) | Same binaries | C01–C08, C10, C12, C14–C16, C18, 67/67 checks ([results](visual-signoff-20261006/acceptance-gateway-results.md)) |
| Designer session: scripted 7-agent team, private Xvfb, Openbox, owned D-Bus and AT-SPI buses, `claude`/`codex` stubbed | Same binaries ([launcher](visual-signoff-20261006/run-session.sh)) | Keyboard, focus, menu, dark appearance, terminal and renderer checks below |

Both acceptance runs ended with an empty `remaining=` in their cleanup record.
All three designer sessions were stopped and their private roots removed. No
operator display, daemon, listener, keyring or team was used.

## Specific re-verifications

| Check | Observed on `3c6c3d32` |
| --- | --- |
| Escape on help (NSR-L01) | F1 focuses Close; Escape closes help and restores focus to the page control (`Choose workspace`). |
| Static empty focus (NSR-L02) | Disconnected Tasks, Ctrl+I, 560-pixel window: focus on `Destination: Tasks`; Alt+Left returns focus to the named `Choose workspace`. No unnamed scroll pane takes focus. |
| Menu classification | The integrated application menu still has six `menu item` objects with blank `Name`; each `LABELLED_BY` target returns its full label through Text (Show Sidebar, Show Details, Appearance, Keyboard Shortcuts, About Crew, Quit), and Show Sidebar now exposes `checked` ([record](visual-signoff-20261006/menu-app.json)). Down and Escape work; Escape returns focus to the named `Application menu`. The classification (GTK provider name exposure, no Crew defect) therefore applies unchanged. It is still not an Orca listening result. |
| F6 and F8 | Inside an attached terminal, F6 moves focus to Detach terminal. Outside a terminal, F6 leaves focus where it was. F8 focuses the pane handle; the next Tab continues normally, so focus is not stranded (L1). |
| Connect focus | Startup focus is `Choose workspace`. When Connections is opened from the header selector, Connect returns focus to that selector, which then reads "Live" as its description. |
| Dark appearance | Neutral dark surfaces, blue selection bar and tab underline, light-blue suggested Answer, red destructive Stop team, status chips with text in every view ([team](visual-signoff-20261006/local-05-dark-team.png), [tasks](visual-signoff-20261006/local-06-dark-tasks.png)). |
| Gateway screens | S01–S08 follow the amended spec: entered fingerprint with group table, comparison code in four groups with expiry and no device-side approval ([compare](visual-signoff-20261006/gw-02-compare-code.png)), Access granted, Connection details, read-only reasons, revocation and local removal copy. |

## Required before final acceptance

| ID | Finding and evidence | Direction | Owner |
| --- | --- | --- | --- |
| R1 | After Detach terminal, or after leaving an attached terminal by selecting another agent, the window status reads "Action pending · awaiting the daemon's result" indefinitely (observed at 2, 5, 12 and 27 seconds). A later Attach resolves it to "acknowledged". This shows a false pending state. ([capture](visual-signoff-20261006/local-04-detach-pending-cairo.png)) | Resolve Detach in the window status when it completes, or treat it like AttachInput/AttachResize (no window status) and show "Detached · read-only" in the terminal tab, as it already does. | Linux app (`controller.rs`; content/scheduler) |
| R2 | Gateway Access granted and Connection details: a long workspace path in the workspace row makes the dialog wider than the window and the screen. The row text and right edge are cut off ([granted](visual-signoff-20261006/gw-03-granted-dialog-overflow.png), [details](visual-signoff-20261006/gw-04-details-dialog-overflow.png)). | Keep the dialog at its normal width. Wrap the path, or ellipsize it in the middle with the full value in a tooltip and accessible description. | Linux gateway screens (`remote/connections.rs`) |
| R3 | On a gateway connection the header workspace selector shows an opaque ID (`8e33c2e23a387ad6`) instead of the project or path that local connections show ([capture](visual-signoff-20261006/gw-05-read-only-opaque-workspace-label.png)). | Use the listing's project name, falling back to the workspace path, as in the local selector. | Linux gateway/controller |

## Not blocking

| ID | Finding | Direction |
| --- | --- | --- |
| L1 | The pane handle reached with F8 is an unnamed `panel`. | Name it, for example "Resize details pane". |
| L2 | Remove from this app (gateway) uses a normal button style ([capture](visual-signoff-20261006/gw-07-remove-not-destructive.png)). | Destructive style, Cancel as default. |
| L3 | While not Live (daemon lost, gateway stopped, team stopped), Answer keeps its suggested-action style and New message stays enabled; their dialogs then refuse with a reason. | Normal style while not Live; drafting offline is acceptable. |
| L4 | Message evidence shows `Metadata: null`. | Omit absent metadata. |
| L5 | Revocation reads "Device access unavailable" with sidebar "Signed out". | Spec permits the generic text; the authenticated revocation reason may be named precisely when the connector supplies it. |

## Renderer residue: harness limit, not a product defect

Several captures from the Cairo renderer, which the capture harness forces,
show leftover pixel rows of earlier text after a page or label changes: the
Verify this host page ([capture](visual-signoff-20261006/gw-01-verify-host-cairo-residue.png)),
the comparison code page, the Kill dialog subtitle and the inspector tab row.
The same steps under GTK's default GL renderer (`GSK_RENDERER=gl`, Mesa
software rendering on the same Xvfb display) leave no residue
([capture](visual-signoff-20261006/local-07-gl-renderer-no-residue.png)). This
matches the V2 diagnosis (a full repaint removes the residue). It is recorded as
a limit of Cairo-based capture, not a product defect. Desktops that fall back to
Cairo rendering may show it. Captures for acceptance evidence should note the
renderer.

## Remaining limits

The Linux app was checked under X11 with GTK 4.22.4 and VTE 0.84.1 on private
displays. Not covered: Wayland, older toolkits, Orca listening, system large
text and high contrast. The scripted scale-2 acceptance subset exists in QA's
integration evidence and was not rerun here. Gateway cases C09, C11, C13, C17
and the screen-reader part of C19 are not scripted. The GTK 4.6 API target
means the terminal's escape hint is an accessible description, not a spoken
announcement. Apple results are source-only: **Apple execution deferred by
operator**.

Hashes of all images are in [captures.sha256](visual-signoff-20261006/captures.sha256).
Gateway and local acceptance captures are cropped to the window region of
1440×960 screens at 96 DPI, Cairo renderer except `local-07`.
