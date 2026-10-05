# Shared web policy measurement

Measured on 2026-10-05 against exact source
`6049cdf63e36e303327235834f23a798aa5a82c1` plus accepted focus source
`a20d78405151b5d91c103929e0efeaddcb08774d`. A generated timeout selector and
command catalog normalized next to its sparse tuple initializer add **zero P1
bytes to the focus build**, leaving **3 bytes** under the original ceiling.
The earlier 18-byte policy route fails the combined gate by 15 bytes. This is a
bounded implementation experiment, not completed web/native adoption.

Only this note is delivered as spike source. Production web, Cargo manifests,
and committed `web-dist` remain unchanged. Experimental code and bundles live
under `target/native-validation/web-policy-spike/` in graph's workspace.

## Authority and scope

The approved W12 exception permits one generated policy file from the same
typed Rust descriptors used by the client at runtime. The seam is
`crew-client/src/policy.rs::web_policy() -> WebPolicy`, owned by `content`:
borrowed method, command, and status descriptors plus validation limits. See
`notes/native-architecture.md` at `28681ceb` and seam decisions
`m-f7b04d65b0bd` / `m-052df55530bf`.

That implementation was not published during this experiment. The temporary
Rust exporter borrows existing command names, aliases, arguments, and help
text from real `crew_tui::slash::DASHBOARD`. Timeout rules and browser-only
metadata are provisional representations of current source. A temporary Rust
timeout consumer and serializer read the same static descriptor slice; there
is no second generator-only timeout table. This does not prove that production
Rust consumers already use the proposed authority. Final adoption must read
the live `policy.rs` descriptors after N2C review.

The measured production imports replace these existing values:

| Surface | Current source and behavior | Measured replacement |
| --- | --- | --- |
| RPC timeouts | `web/src/net/conn.ts:59-78`; ordinary 3,000ms, lifecycle 30,000ms, models 20,000ms, resume/shutdown 90,000ms. Matches `crew-tui/src/net.rs:31-54`. | Generated `timeoutFor(method)` from method descriptors, including the ordinary fallback. |
| Command metadata | `web/src/commands.ts:10-29`; 15 TUI commands plus browser `usage`, `graph`, and `keys`. Names, aliases, order, arguments, help text, and browser annotations remain identical. | Generated `COMMANDS` uses the same sparse tuple initializer and tuple-to-object map; `commands.ts` imports/reexports it. |
| Identified-message retry wait | `web/src/app.ts:313`; after `closed` only, wait up to 30,000ms for live state, retry once with the same message ID/payload. | Tested generated scalar in the earlier route, then excluded to satisfy the combined gate. Final measured `app.ts` is byte-identical to the baseline. |

The final import scope is timeout classification and command metadata only.
No current-work ranking, completed-card tie behavior, reducer, action
availability, confirmation dispatch, connection proof, or mutation replay rule
changes. All four existing confirmations retain their current behavior. Their
membership/target policy is a later real consumer of the descriptor; help text
containing “asks first” is not counted as shared confirmation logic.

Retention/snapshot defaults are excluded: web 1,000 messages/5,000 events/600
context samples and snapshot 300/1,000 differ from TUI 500/2,000 and snapshot
300/400. They belong to a separately frozen `ClientPreset` seam, not
`ValidationLimits`. The browser has no existing body/options validation literal
to replace in this slice. Experimental status/frame exports are unused and
tree-shaken; they receive no runtime reuse credit. Status validation and legacy
work ranking remain unchanged.

## Exact build measurements

The isolated baseline export contains 155 tracked web/wire-fixture files from
`6049cdf`, with its dependency directory linked to the installed locked web
dependencies. Its normal `bun run build` reproduces the committed manifest
**exactly**: `fnv1a64:f22f3cf5ad74d25f`, six payload files, 597,922 bytes, including
the original hashed file names.

Values below are complete uncompressed manifest payloads, not generated-file
size or gzip estimates. P1 includes every nonterminal script; P4 includes every
payload file, including HTML and lazy terminal assets.

| Variant | P1 JS | P2 CSS | P3 terminal | P4 payload | P1 remaining |
| --- | ---: | ---: | ---: | ---: | ---: |
| Reviewed baseline | 230,415 | 33,906 | 332,915 | 597,922 | 85 |
| Scalar timeouts + sparse tuples | 230,451 | 33,906 | 332,915 | 597,958 | 49 |
| Object timeout lookup + object commands | 230,913 | 33,906 | 332,915 | 598,420 | **-413** |
| Generated timeout selector + sparse tuples | **230,433** | **33,906** | **332,915** | **597,940** | **67** |
| Existing ceiling | 230,500 | 34,000 | 350,000 | 600,000 | |

The original-baseline selector route leaves P2 94, P3 17,085, and P4 2,060 bytes.
It adds one build
module (65 to 66 transformed modules), keeps six payload files, and adds no
runtime dependency. The packed lookup used `Object.hasOwn` so names such as
`__proto__` and `toString` still receive the ordinary fallback; dropping that
guard would change behavior rather than save acceptable bytes.

| Variant | Generated TS bytes | Manifest source |
| --- | ---: | --- |
| Scalars/tuples | 1,949 | `fnv1a64:d8b8b35da9a3f07d` |
| Objects | 2,685 | `fnv1a64:7e49891ced1fdc4a` |
| Selector/tuples | 2,133 | `fnv1a64:d0eea7d0ca4d6954` |

The original selector/tuple generated file's SHA-256 is
`27945fd094975ef215aaf7ba51eb27eba86ba4397559ee81e6e0d5f4437b49fc`.
Two exporter runs for each variant produced byte-equal output. No timestamps,
paths, host metadata, or unordered-map iteration enter serialization.

Observed toolchain: Rust 1.93.1, Node 24.19.0, Bun 1.3.9, locked Vite 7.3.6.
These byte results depend on the exact baseline, output shape, and toolchain.
The 85 bytes of original headroom are shared with the focus correction; they
cannot be allocated independently.

### Combined focus target

`web-focus-fallback` is Done with source `a20d7840` and independent QA test
`bfe84371df4631d1938bb2e395660aaed6e3960e`. Only `web/src/ui.ts` from that
accepted source is overlaid on the exact baseline export. Its private
baseline-import commit is not merged. The isolated focus build reproduces
QA's `fnv1a64:b1e2e05bd16fb63d` and complete payload sizes.

| Combined variant | P1 JS | P2 CSS | P3 terminal | P4 payload | P1 remaining |
| --- | ---: | ---: | ---: | ---: | ---: |
| Accepted focus only | 230,497 | 33,906 | 332,915 | 598,004 | 3 |
| Focus + original selector/tuple/retry imports | 230,515 | 33,906 | 332,915 | 598,022 | **-15** |
| Focus + adjacent tuple normalization + retry import | 230,509 | 33,906 | 332,915 | 598,016 | **-9** |
| Focus + timeout selector/normalized commands only | **230,497** | **33,906** | **332,915** | **598,004** | **3** |

Two bounded output changes recover the policy overhead: put the existing map
next to the generated tuple initializer, avoiding an extra hoisted tuple
binding; leave the retry-wait scalar unadopted. No unrelated source cleanup or
focus behavior changes were used. The final combined source fingerprint is
`fnv1a64:281831a35b9d64df`; P4 remaining is 1,996 bytes. Six payload files and
three runtime dependencies remain.

The generated file is 2,366 source bytes with SHA-256
`7abdf23ec31c21b53a077ff7528b31183fdf1b53b3f657985e0aaeda3fe78892`.
Repeated Rust exporter runs reproduce it exactly. Unused retry/status/frame
exports remain tree-shaken in the experiment and receive no reuse credit; the
final generator may omit them. Recompute the live-authority build before
adoption. Three bytes of measured headroom provide no independent allowance
for another change.

## Generator and import proposal

`content` owns `crew-client/examples/generate_web_policy.rs` and
`crew-client/tests/web_policy.rs`. The example reads `web_policy()` once and
renders exactly `web/src/generated/client-policy.ts`. It does not create its
own constants, operation table, command catalog, or handwritten classifier.

The minimal output contains:

```ts
export function timeoutFor(method: string): number {
  // Descriptor-derived case labels and values; ordinary fallback included.
}
export type GeneratedCommand = { name: string; about: string; args: string; aliases: string[]; web?: "declined" | "new" };
export type CommandTuple = [string, string, string?, string[]?, ("declined" | "new")?];
export const COMMANDS: GeneratedCommand[] = ([/* descriptor-derived tuples */]
  as CommandTuple[]).map(([name, about, args = "", aliases = [], web]) =>
    ({ name, about, args, aliases, web }));
```

The renderer groups equal timeout values in first-descriptor order and emits
a closed switch using JSON-quoted method names. The default comes from the
runtime's ordinary fallback descriptor. It must not infer membership from a
second browser list or choose 3,000ms independently. Sparse tuples omit only
trailing default fields; preserve argument/alias slots when a later browser
annotation is present. Command order remains runtime descriptor order.

`conn.ts` imports/reexports the generated selector and removes its duplicate
timeout switch. `commands.ts` imports/reexports the generated normalized
catalog and retains its parser, dispatch, completion, and hints. Its existing
map moves into the generated initializer without changing the resulting data.
`app.ts`, protocol, store, snapshot parameters, and rendering modules stay
unchanged; the accepted focus source remains the sole `ui.ts` difference.

Reject duplicate method/command names, invalid descriptor combinations, or
numeric exports outside JavaScript's exact range. Use a fixed newline and
deterministic quoting; emit no executable snippets from descriptor strings.
There is no `eval`, dynamic code loader, external resource, WASM, or build-time
Node requirement for Rust consumers.

The adoption card commits the generated file under the narrowly amended W12
rule. Normal web builds consume that file and require no Cargo. Generation is an explicit
maintainer command; it is not a web build hook or a Rust `build.rs` hook.
`cargo install --path crates/crew` continues to require no Node. Existing
`scripts/stamp.ts` already fingerprints all `web/src/**`, including this file.

## Validation and adoption ownership

The original selector export passed normal TypeScript/build checks,
**109 focused unit checks across nine files**, and **18 existing browser
action/CSP checks**. The final combined variant passed **109 focused unit
checks and 23 browser checks**, including all five accepted focus regressions.
Unit coverage includes commands, timeouts, protocol, store, current work,
workspace state, page behavior, all P1–P4 budgets, and three direct comparisons
with the exact baseline. Comparisons cover every surfaced RPC timeout plus
unknown/prototype-property names, full command objects/aliases/help/dispatch,
and unchanged protocol sets. The packed alternative passed 22 focused semantic
checks but fails the measured P1 gate, so it is rejected.

The final browser run used Chromium 151.0.7922.108 on the combined built bundle:
`e2e/actions.spec.ts`, `e2e/policy.spec.ts`, and `e2e/panel-focus.spec.ts`, one
worker; all 23 passed in 9.1s. All four
confirmations, cancel paths, message failures, pause/resume/model operations,
and markup handling pass. The policy test observes zero CSP violations and no
unexpected origin requests through all views/tabs/dialogs, including terminal.
The server CSP/Host/Origin code and npm runtime dependencies are unchanged.
No full performance matrix, live daemon, or native application verification
was repeated or claimed by this spike.

Permanent adoption checks belong to the separate N7 implementation:

- `content`: generate from the reviewed live Rust authority; add a stale-output
  test comparing the renderer's bytes with the committed TS file. Mutating a
  exported descriptor must fail that test until regeneration. Native/TUI
  timeout/planner consumers and serializer must borrow the same descriptors.
- `content`: add Rust/web differential vectors for every timeout category,
  fallback, command alias/order/argument/help annotation, and exported retry
  value if it is later adopted; preserve separately labelled legacy web
  scope/ranking differences. The first measured release excludes retry imports.
- `qa`: rebuild and integrate `web-dist`, confirm source freshness and complete
  P1–P4 costs, then run required web security/behavior/performance gates on the
  current combined source. Runtime/package manifests still have three web roots.
- `reviewer`: approve the final authority/import diff and measured integrated
  bundle before production adoption. This note does not release that card.

## Reproduction evidence

Ignored experiment root:
`target/native-validation/web-policy-spike/`. It contains `checkpoint.json`,
`export-policy.rs`, compiled exporter, `measurements.json`, and independent
`baseline`, `compact`, `packed`, `selector`, `focus`, `combined`, `mapped`, and
final `essential` exports. All experimental web
source edits occur inside those exports. This keeps the spike separate from
the reviewed production tree and later policy owner.

From the final `essential/web/` directory, the validation commands
are:

```sh
bun run build
bun run test -- test/commands.test.ts test/conn.test.ts test/protocol.test.ts \
  test/store.test.ts test/work.test.ts test/workspace.test.ts \
  test/budget.test.ts test/policy-spike.test.ts test/page.test.tsx
PLAYWRIGHT_EXECUTABLE_PATH=/nix/store/kkbid9hhapp4pxd3rsy1bcllxcl2d9g2-chromium-151.0.7922.108/bin/chromium \
  bunx playwright test --project=mock --workers=1 --output ../browser-results \
  e2e/policy.spec.ts e2e/actions.spec.ts e2e/panel-focus.spec.ts
```

The experiment's temporary Rust source/fixtures are measurement aids. They
must not be copied into production as another authority. Final N7 reruns the
same measurements with the real `policy.rs` implementation and a fresh export
of the approved integration checkpoint.
