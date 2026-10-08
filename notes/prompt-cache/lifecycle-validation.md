# Lifecycle baseline validation

The delivery candidate is 0f878f23 on main 66eadf2f. This record adds no
executable change. The ten owned commits were replayed without conflicts;
each carries a cherry-picked-from trailer. The pre-replay history remains at
`agents/qa-pre-lifecycle-final` (af140739).

| Original | Replay |
| --- | --- |
| 5dd8363e | b085106d |
| 22ffd88d | d3229450 |
| 90b57182 | 77b8548b |
| 55851b03 | a8163c75 |
| 0d7f506d | 3aebb0a7 |
| ffbeb0be | 170c2cfa |
| b20bd3c0 | 89933e47 |
| 0f143860 | e831f4b0 |
| ef22c5ec | 6380c236 |
| af140739 | 0f878f23 |

Earlier collector imports were skipped because main already contains the
reviewed collector. Every owned source and evidence file is byte-equal to
af140739. The candidate delta from main is confined to `scripts/cache` and
`notes/prompt-cache`: no Rust product source, dependency, web bundle or runtime
activation changed.

Validation on the replayed candidate:

```sh
nix shell nixpkgs#python3 --command \
  python3 -B -m unittest discover -s scripts/cache -p 'test_*.py' -v
node --test scripts/cache/baseline.test.mjs
```

All 21 Python tests and all 23 collector tests passed. The Python suite
includes the actual Node collector handoff with SQLite metadata, explicit
session selectors and native-source hash preservation; neither suite calls a
provider. The two long-idle watchdog transitions have a dedicated regression.

The final main collector was also run read-only on each completed live run,
with exactly three `--session-file runtime=PATH` arguments and `--since` /
`--until` set to that report's start/end. It exactly reproduced each member's
request count, total input and cache reads: 113 observations in sample 01,
112 in sample 02r, and 113 in sample 03. No model request was repeated. The
collector SHA-256 was
`a5f12d7ba7e0ff7555db57961d1e2af1c3440d41a0f4da22dbfa06e2890c7a8f`.
Those derived reports remain at
`target/cache-bench/lifecycle-final-{01,02r,03}/baseline-reviewed.json`.

The committed individual reports are exact copies of their original
`report.json` files. Their runner hashes, template, binary hash, CLI versions,
model names and off mode match across the three samples. Every completed run
and rejected attempt has stop exit code 0 and no tracked process remaining.
Original native sessions were neither edited nor deleted. The numeric series
records all attempts and separates first-request rates from whole-step rates.

Limits remain explicit: these are before samples in the operator's normal
CLI environment, the roster observer is scripted, native persisted hashes do
not establish provider wire equality, and Haiku's lack of a history-rewrite
diagnostic does not demonstrate an Opus improvement. There is no after-run or
cache-savings claim in this delivery.
