# Neutral dashboard integration verification

Production bundle: `064037442ab479f5bbbe921361dda492757bf253`.
Source fingerprint: `fnv1a64:50c88732a97c7d7b`. Base: `15de7cf0`.

Integrated sources: UI `2578c713`; graph `15ed4cdf`, `e4b58f21`, `89034e85`;
selection correction `0b072f44`. Paired QA checks: `d35a8fcc`, `2069b4c2`.
The production source delta is four files: theme tokens, terminal fallbacks,
graph selection styling, and the shared list selection marker.

Dark canvas/panels/text/borders/terminal are grayscale, with blue actions.
Selected graph outlines remain distinct from keyboard focus and the hub.
All five virtual lists retain a visible marker while another row is hovered,
including warning/error event rows. Light palette tokens are unchanged;
Team/Task/Transcript gain the existing light green selection marker.

## Final bytes

| Gate | Bytes | Ceiling | Remaining |
| --- | ---: | ---: | ---: |
| P1 nonterminal scripts | 230,360 | 230,500 | 140 |
| P2 stylesheets | 33,906 | 34,000 | 94 |
| P3 terminal scripts | 332,915 | 350,000 | 17,085 |
| P4 manifest payload | 597,867 | 600,000 | 2,133 |

The manifest source fingerprint and each of its six file hashes/sizes were
independently recomputed from disk; no extra asset remains. The final CSS and
payload are 18 bytes larger than the inherited delivery. Budgets are unchanged.

## Executed checks

From `web/`, with `PLAYWRIGHT_EXECUTABLE_PATH=/etc/profiles/per-user/nade/bin/brave`:

```sh
bun run build
bunx playwright test e2e/theme.spec.ts e2e/lists.spec.ts --project=mock \
  --workers=2 --output=/tmp/crew-qa-neutral-selection-final
bun run gallery --out /tmp/crew-qa-neutral-final --scenario busy \
  --surface team-graph,team-list,tasks-graph,tasks-list,events,usage,inspector,terminal,transcript,switcher,help,confirm \
  --theme dark,light --viewport desktop,narrow --strict
bun run gallery --out /tmp/crew-qa-neutral-final-long --scenario long-text \
  --surface team-list,tasks-list --theme dark,light --viewport narrow,zoom --strict
```

Build/typecheck succeeds. The final browser run passes 27/27, including all 15
theme cases and 12 list cases. Before the selection correction, the six
Team/Task/Transcript marker cases failed and the four message/event cases passed.

The two final galleries contain 56 images and 6,204 rendered text checks, with
zero strict failures, skips, missing images, horizontal document overflow,
unnamed controls, page errors, CSP reports, or requests after load. Both
manifests record the final source fingerprint. Gallery output stays outside
the repository and contains fixture content only.

From the repository root:

```sh
node notes/web-revamp/contrast.mjs
just web-check
```

Contrast/neutrality: 57 pass, zero fail. Byte-for-byte temporary rebuild:
`web-dist matches the sources`.

Reviewer independently verifies this exact bundle: 175 unit tests, 225
mock/performance checks, two `web_bundle` tests, and a matching independent
build. Reported performance: first paint at most 92ms, live at most 175ms,
workspace switch p95 49ms, heap 7.9MB, zero idle frames/timers. The performance
run used a coordinated quiet host window. QA's earlier 175-unit/49-browser
run and 80 captures used `cf8956b55a22c53c`; they are inherited evidence, not
counted as checks of the corrected selector. The historical 2,432-case matrix
and live-daemon suite were not repeated for these styling changes.

## Durable evidence and reload

[Final visual sign-off](../notes/web-revamp/neutral-signoff/index.html) pins the
exact bundle/fingerprint and retains eleven final production captures.
[Selection audit](neutral-selection-audit.md) retains both-theme Team, Task,
and Transcript images plus fourteen capture measurements across all five lists.
The earlier green palette approval and candidate galleries are marked historical.

For a local production preview, run `bun run mock --fixture subagents --scenario busy`
from `web/` and open its freshly printed bundle pairing address. To update a
running dashboard, rebuild or reinstall `crew`, run that new binary with
`crew web --restart`, and open the new pairing address. The dashboard server
is replaced; the team daemon keeps running. README `83b72e07` documents this.

No open web finding remains. Visual sign-off `b36fbc1d`, source and test
artifacts, rebuilt bundle, and reload instructions are integrated.
