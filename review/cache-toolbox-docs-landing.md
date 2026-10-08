# Approved cache and toolbox documentation landing

Task: `cache-toolbox-docs-landing`. Base: `c8052373c0d2f8b7a993773e5cda3a79e1b834d6`. Replayed tip: `cc467095c8556a9ab64109b06293bcb5f50dc64c`.

The approved toolbox, cache audit and benchmark changes were replayed in that order as 16 single-parent commits. Every replay retains a source trailer and has the same stable Git patch ID as its source. No conflict or manual resolution occurred.

The benchmark branch merge `fb24ecacd980964d1a84fbd33926c81ad34f5876` was omitted: its complete tree equals the base. The five benchmark commits after it were replayed. The previous content bookmark was preserved as `content-pre-cache-docs-af0d8a72` before publication.

## Per-commit equivalence

| Source commit | Replayed commit | Shared stable patch ID |
| --- | --- | --- |
| `e4d570fe70a51eb265bb7339a4d7397776f75d0c` | `eec047f1f83d0efc77574596ec8d0e72edf9ce2b` | `65374e8747fd29874817329312e82a1af0e33854` |
| `935ca66bb77dec57f4da3ce3976f0da5526f5598` | `d4679cbc81ec0efdf171c5cb635ff95cec1882b5` | `835007e2e04d75c5667d199642a4f838e49c4722` |
| `58bc8095268ace9cf64c4083b4a66d094c0f6c50` | `653f9524f379e08584507d27ac1652583ec2c035` | `28b6f18f016352257215945d901da77d97f1b93c` |
| `fe7ace315c9728b898e1725fdfbad572b87ffaa0` | `d41f2066f6821fea87de58590aceed1ad117116b` | `e1b1a417753da74c6e667720853276655b99ebcc` |
| `6680f2da20eec788f7ded00015182e35baf9c211` | `a4529bc526331a24e20f573a7defb2642c959307` | `c01b77cfeb44c3dc40f02c8aa68e69a5f497b0d8` |
| `6f8514c98790ade5e96578d1c100e2b0d4eabf8d` | `3bf6e07273bf232aa3dd5f0e6417d78993e5b616` | `8dff679e263960b2aad4373ee4c04e61569b93da` |
| `8924ed46ec731e98a89224d5d1036e65aed41a7b` | `dab2bdda1dfb7e715e72a9d0329a6601834c0608` | `a04af0e8fb1b5695485f23656344449f16a230bf` |
| `0f6c0cf5c07abf3a7ea7169512334760da2f08b7` | `89cc2ae86eb4e38077a25300773ee2ea39689065` | `0a8351e92b0db877f3aa5b41da076782a00ccb80` |
| `870dc7b3054e7aafb5b95e7809cbcc970b7c189e` | `4231defd961ad73d420d1702fc943f0254bab123` | `bc3710c336d2b2aaae1b02669b7cdc1cb581387d` |
| `64e001a649c4d0881a04fcdeab2c5e89248a4f78` | `2636cb87db057e2aae55fe4faf308a39c0230e63` | `6c31e909a5c4374e7c38400317eaf91c5e2a8eea` |
| `562482daa0c553432f5f98d82baf7cd81f06b722` | `e018cf04ecf84ca397dbe4a95986e014f44e6651` | `16bfd680adef1098f68e7aa8056c959d245931b4` |
| `56986edfbc354edd063fe24a6eb0fb27cffe3d9a` | `e69bbdb659fae93154e609a93b5a5466d551b748` | `dc6caf050c08c8d263cfc2e423c1b5074aeaeac5` |
| `a41925c1cc84bee35b7e30d35e0a481d8da2aefc` | `2510106367f084f72185fb222173bd45891e3e6c` | `065e1b83ff3c596d6202dc1ddf934f53bc49931a` |
| `979b3943cad24d2bd03f58d4d5d15755aa8e085f` | `42b40ae90f686a65def00b0e29e74505a5656d36` | `6ad8c57019cee30f139cd3a4dc3497c54fa99440` |
| `a1cd0cf56f18bc709e5e554e5dcfb907eb191c9a` | `d9637aa65cd9048e8ac48bfbeb7085d8beb97dbb` | `3218a5db64747314259254a1821ac891b0c747cc` |
| `5275807ed759e93b6f1df851bac0efd15d9b59bb` | `cc467095c8556a9ab64109b06293bcb5f50dc64c` | `a73f2492ea34396a6913cd6ba9b1f00c0857291c` |

## Source tree equivalence

Each path below was compared between the stated source tip and the replayed tip with `jj diff --from SOURCE --to REPLAY --name-only PATHS`. All comparisons were empty. The complete changed-path set of the replayed chain equals the union below; no other files changed.

### Toolbox specification and audit

Source tip: `8924ed46ec731e98a89224d5d1036e65aed41a7b`.

- `notes/native-apps.md`
- `notes/toolboxes.md`

### Prompt cache audit

Source tip: `562482daa0c553432f5f98d82baf7cd81f06b722`.

- `notes/prompt-cache-audit/cohort.tsv`
- `notes/prompt-cache-audit/cost.sh`
- `notes/prompt-cache-audit/costs.tsv`
- `notes/prompt-cache-audit/models.tsv`
- `notes/prompt-cache-audit/prices.tsv`
- `notes/prompt-cache-audit/run.sh`
- `notes/prompt-cache-audit/session.sh`
- `notes/prompt-cache-audit/sessions.tsv`
- `notes/prompt-cache-audit/signatures.tsv`
- `notes/prompt-cache-audit/totals.tsv`
- `notes/prompt-cache.md`

### Prompt cache benchmark

Source tip: `5275807ed759e93b6f1df851bac0efd15d9b59bb`.

- `notes/prompt-cache/bench-baseline-attempt.json`
- `notes/prompt-cache/bench-baseline.json`
- `notes/prompt-cache/bench.md`
- `scripts/cache/bench`
- `scripts/cache/bench.py`
- `scripts/cache/team.yaml`
- `scripts/cache/test_bench.py`
- `scripts/cache/usage.py`

## Validation scope

Validation covered ancestry, order, source trailers, per-commit stable patch IDs, exact source-path trees and the complete changed-path set. Runtime tests and live benchmarks were not rerun for this mechanical landing; their approved evidence is carried unchanged. This report is the only additional file in the following evidence commit.
