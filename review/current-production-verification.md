# Current production dashboard verification

Recovery checkpoint: `6049cdf63e36e303327235834f23a798aa5a82c1`, clean before
verification, on 2026-10-05. This report adds verification evidence to the
[approved final review](final-neutral-and-overlap.md) and
[runtime delivery](runtime-delivery.md); it changes no product source or assets.

## Source and bundle

`web/`, `crates/crew/web-dist` and `crates/crew-tui` still equal reviewed
`9d2e838747fefae2684c66dd56ae854215446bbd`. Core source, tests and manifests
still equal reviewed `a700f29e3a5fa2321d5f04bfa89595d21429c207`.

`just web-check` rebuilt the bundle in a temporary directory and reported
`web-dist matches the sources`. The source fingerprint, all six listed file
sizes/hashes, and the absence of extra assets were independently recomputed:
`fnv1a64:f22f3cf5ad74d25f`. Ceilings are unchanged.

| Gate | Bytes | Ceiling | Remaining |
| --- | ---: | ---: | ---: |
| P1 nonterminal scripts | 230,415 | 230,500 | 85 |
| P2 stylesheets | 33,906 | 34,000 | 94 |
| P3 terminal scripts | 332,915 | 350,000 | 17,085 |
| P4 manifest payload | 597,922 | 600,000 | 2,078 |

## Fresh checks

From the repository root:

```sh
just web-check
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH \
  ./scripts/build-env cargo build -p crew
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH \
  PLAYWRIGHT_EXECUTABLE_PATH=/etc/profiles/per-user/nade/bin/brave \
  ./scripts/web-e2e jj --workers=1 --output=target/recovery-live-results
env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH \
  ./scripts/build-env cargo test -p crew --test web_bundle
(cd web && bun run test test/budget.test.ts)
```

Build and bundle comparison passed. **36/36 live cases passed in 7.1 minutes**;
the final-source live-suite gap is closed. The targeted bundle suite passed
2/2 and budget/dependency checks passed 5/5.

Live measurements: first paint at most 60ms, live at most 116.6ms, switch p95
48.1ms, real-push frame p95 at most 16.8ms, zero long tasks under real load, terminal echo p95
4.3ms and heap 20.8MB. Five idle joined workspaces produced zero index frames
in 60 seconds; server wake-ups were all explained by ticks, pings and the
timer wheel. Debug server RSS was 26.9MB.

The live project uses the freshly built binary with isolated workspace,
runtime, state and configuration directories for each test. Its stop, kill,
restart, pairing and action cases operate on those scripted test teams.
Reviewer and designer held builds/browser work during the timing checks.
`CREW_PERF_SCALE` was unset: original thresholds applied.

Logs remain in ignored `target/recovery-{build,web-check,live,web-bundle,budget}.log`;
extracted measurements are `target/recovery-live{,-multi}-perf.json`.
Playwright's output path is relative to `web/`; its successful run record was
moved afterward to ignored `target/recovery-live-results/`.

The unchanged workspace, full frontend unit, mock/performance matrix and
contrast checks retain the evidence in the approved reports; those full
matrices were not repeated.

## Activation, observed without restarting production

Production listener PID `2657615`, port `23987`, protocol `6`, build
`0.1.0+d5063fc1d304fc9a` served `/`, all five assets and `manifest.json`
byte for byte equal to this checkpoint. Active operator-team daemon PID
`2658115` came from the root checkout's `.crew/daemon.json`.

Both running executable images and the current root release executable have
SHA-256 `d40f215508b87e3d5cbf7a9e86336d5c63b93ee3059f3eb7dbc7eba70fdbb310`.
The listener and operator daemon started after that release build. The
dashboard assets and current operator team are therefore already active;
neither needs a recovery reload. No active team or listener was stopped,
restarted or re-paired by this verification.

The separate `demo` session at `demo-workspace`, PID `1925176`, still uses
its older executable image (`/proc/1925176/exe` points to the deleted prior
release; SHA-256
`563b3e40c6d9cdd71dd0f0ee334a66d77450704c6825231ef267e2cf8da2b268`).
That team needs a later stop/start with the current binary to activate the
reviewed runtime behavior after its work finishes. It was preserved here.

The asset comparison is reproducible with read-only requests:

```sh
bun - <<'JS'
import { readFileSync } from 'node:fs';
const meta = JSON.parse(readFileSync('/run/user/1000/crew/web.json', 'utf8'));
const dist = 'crates/crew/web-dist';
const manifest = JSON.parse(readFileSync(`${dist}/manifest.json`, 'utf8'));
for (const path of ['manifest.json', ...manifest.files.map(file => file.path)]) {
  const response = await fetch(`http://127.0.0.1:${meta.port}/${path === 'index.html' ? '' : path}`, {
    headers: { Host: new URL(meta.url).host },
  });
  const bytes = Buffer.from(await response.arrayBuffer());
  if (response.status !== 200 || !bytes.equals(readFileSync(`${dist}/${path}`))) {
    throw new Error(`Production differs: ${path}`);
  }
}
console.log('Production manifest and six files match the checkout');
JS
```

## Presentation evidence and remaining gaps

Designer owns the focused current-production-asset visual check in
`verify-delivered-design`; current source, manifest and activation provenance
were shared directly after the live timing window.
Existing [runtime presentation captures](runtime-presentation/index.html)
and [neutral sign-off](../notes/web-revamp/neutral-signoff/index.html) remain
the retained gallery; no replacement capture matrix was requested.

No verification failure remains. The separate demo's older runtime is the
remaining activation gap; the current operator team and dashboard are active.
P1 and P2 headroom remain small; future source changes must keep the measured
ceilings. No speculative redesign or runtime rewrite was performed.
