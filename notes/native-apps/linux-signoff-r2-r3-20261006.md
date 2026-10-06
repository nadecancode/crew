# Linux sign-off fixes R2, R3 and L1–L5

Task `native-linux-signoff-r2-r3`: the designer's required items R2 and R3 and
the non-blocking items L1–L5 from the
[visual sign-off of main 3c6c3d32](visual-signoff-20261006.md). R1 (Detach
status) is scheduler's `native-linux-signoff-r1`, already on main `5c5644c0`.
"Before" captures are the sign-off's own, on `3c6c3d32`.

## What changed

| Item | Change | Before | After |
| --- | --- | --- | --- |
| R2 | A granted workspace's button put its host path on one unwrapped line, so Access granted, and Connection details after it, grew wider than the window. The path is now one line shortened in the middle, with the full path in the tooltip and accessible description. The project and state line wraps. The dialog keeps its 640-pixel width. | [granted](visual-signoff-20261006/gw-03-granted-dialog-overflow.png), [details](visual-signoff-20261006/gw-04-details-dialog-overflow.png) | [granted](linux-signoff-r2-r3-20261006/gateway-C06-s07-granted.png), [details](linux-signoff-r2-r3-20261006/gateway-C08-s08-details.png) |
| R3 | The header workspace selector showed a gateway workspace's opaque ID. It now shows the listing's project, or the workspace path when there is no project. A local scope is already its path, so local connections are unchanged. Its accessible name is "project, path". The "Access in …" section is named the same way. | [opaque ID](visual-signoff-20261006/gw-05-read-only-opaque-workspace-label.png) | [project](linux-signoff-r2-r3-20261006/gateway-C07-read-only.png) |
| L1 | The split pane that F8 focuses is named "Resize details pane". GTK gives a split the generic role, and ARIA forbids naming that role, so GTK dropped the label. The split is now a group, which can carry the name ([AT-SPI probe](linux-signoff-r2-r3-20261006/f8-atspi.txt)). | unnamed `panel` | `grouping: 'Resize details pane'` |
| L2 | Remove from this app is styled destructive, both on Connection details and on its confirmation. On the confirmation, Cancel comes first and has focus. | [normal style](visual-signoff-20261006/gw-07-remove-not-destructive.png) | [destructive](linux-signoff-r2-r3-20261006/gateway-C18-remove.png) |
| L3 | Answer is the suggested action only while the workspace is Live. New message stays enabled, since drafting offline is allowed. | | in-process check |
| L4 | Message evidence leaves out absent metadata, and event evidence leaves out absent data, instead of printing `null`. | `Metadata: null` | [message](linux-signoff-r2-r3-20261006/local-L06-detail.png) |
| L5 | When the connector names the revocation, Connections reads "Device access ended", the approved copy for `revoked`. Other sign-outs keep "Device access unavailable". | "Device access unavailable" | [ended](linux-signoff-r2-r3-20261006/gateway-C16-revoked.png) |

The acceptance case C16 accepted only "no longer available", "unavailable" or
"revoked" after a host revocation. It now also accepts "access ended"
(`scripts/native/acceptance/cases_gateway.py`).

## Checks

Source: `agents/content` 424c1705, which fast-forwards main `5c5644c0`.
Binaries: [tool-versions.txt](linux-signoff-r2-r3-20261006/tool-versions.txt),
with the app built with `native-ui,local,gateway` and the CLI built with
`native-gateway`.

- Acceptance, gateway suite: 14/14 cases, 67/67 checks
  ([results](linux-signoff-r2-r3-20261006/acceptance-gateway-results.md)).
  On e52cfa99, before the C16 change, C16 failed only its copy check.
- Acceptance, local suite: 16/16 cases, 101/101 checks
  ([results](linux-signoff-r2-r3-20261006/acceptance-local-results.md)).
  Both runs ended with an empty `remaining=`.
- `crates/crew-desktop/tests/run-window-test.sh` on an export of 424c1705 passes
  in every mode: `--gateway` 1/1, window 1/1, `--client` 8/8, `--keyboard` and
  `--interaction`. GTK warnings: `--gateway` logs only the Connections
  dialog's pre-existing "min height of 336 for width of 592" (3). `--client`
  logs only the pre-existing live-daemon "width of 926" family (6).
- Strict Clippy (`-D warnings`, all targets) for `native-ui,local`,
  `native-ui,local,gateway` and `native-ui,gateway-tests`; `cargo fmt --check`
  clean.

New in-process checks:

- Gateway presentation test (`--gateway`):
  - A 232-character path keeps the dialog's minimum width at or below 640 pixels, with the full path as the tooltip (R2).
  - A named revocation reads "Device access ended" (L5).
  - Remove is destructive on details and on its confirmation, where Cancel comes first and has focus (L2).
- Controller test (`--client`):
  - The selector reads the project, then the path, then the local scope (R3). It fails with the listing lookup removed.
  - The split has the group role and a label (L1). It fails with the generic role.
  - Answer is styled suggested only while Live (L3).
- Unit test: absent message metadata (null, `{}` or `[]`) is left out, and present metadata is shown (L4).

Seen in passing, not changed here: the sidebar result line still reads "Action
acknowledged by the daemon." after the profile is signed out
([C18](linux-signoff-r2-r3-20261006/gateway-C18-remove.png)). That line is
the window's mutation status, and R1 owns its rules.
