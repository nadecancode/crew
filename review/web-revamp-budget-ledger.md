# Web revamp: byte ledger (agent: reviewer)

The reserve of `notes/web-revamp.md` §budget is the reviewer's to assign, by a recorded comment. This file
is that record. Figures are minified production bytes from `bun run build`.

Ceilings (orchestrator, m-ddb0736dbdc7): P1 228,000; P2 34,000; P3 350,000; P4 600,000.

| | P1 script | P2 CSS | base |
|---|---|---|---|
| Before the revamp (`12207423`) | 213,530 | 28,487 | |
| Approved foundation (`6f5ae5d5`) | 213,570 | 29,227 | |
| `ui` remainder, allowance | +2,200 | +1,200 | foundation |
| `graph`, allowance | +6,500 | +1,400 | foundation |
| `content`, allowance | +3,500 | +1,200 | foundation |
| Reserve after the foundation | 2,230 | 973 | |

## Rules applied

- A slice's T0 fits its own allowance. The reserve pays for funded T1 items only.
- 500 bytes of script and 200 of CSS stay unassigned until integration: the slices are measured apart and
  minified together.
- Unused allowance can move only from a slice that has been approved, at its measured size.
- A slice over its allowance plus its grant cuts T1 in the design's order before it asks for more.

## Grants

| when | to | script | CSS | for | left in reserve |
|---|---|---|---|---|---|
| m-595b4d542ea2 | `content`, provisional | +550 | +250 | funded T1 (task lifecycle and artifact dates, local read feedback, usage coverage); lapses if unused at its review | 1,680 / 723 |
| m-5e814c6d5523, m-cf3371c3f24e | `content` returns the CSS grant | | −250 | its candidate measures +4,010 script and +1,200 CSS: T0 +3,443, one T1 item (total reported tokens, N of M reporting) +567 | 1,680 / 973 |
| m-606eff766778 | `graph` | +900 | | T0 and the Task List | 780 / 973 |

`content` is therefore reviewed against +4,050 script (P1 at most 217,620) and +1,200 CSS over the
foundation; `graph` against +7,400 script (P1 at most 220,970) and +1,400 CSS.

## The shortfall for the two List modes (put to the orchestrator)

`graph` measures T0 at +5,279 and the two List modes at +4,782, together +10,061 against +6,500. With
`content` at +4,010 and `ui` at its allowance of +2,200 the bundle would be 229,841: 1,841 over P1 with
nothing kept for integration. Both lists cannot be had inside 228,000 unless `ui` returns most of its
allowance.

The assignment above follows the design's cut order, in which the Team List goes before the Task List.
The Team List waits for one of: it fits under +7,400 after the lists share their code; `ui` is approved
under its +2,200 and the unused bytes move; or the orchestrator decides otherwise. P4 would allow a
further raise (596,706 with every slice at its figure, of 600,000), but that is not the reviewer's to
give.

Of the 780 bytes of script left, 500 are held for integration and 280 are unassigned.

## Second raise of P1 (orchestrator, m-362e9ff64bfe)

Decided on the shortfall above: **P1 230,500** (from 228,000). P2 34,000, P3, P4 600,000 and P5–P20 are
unchanged. Both List modes are in scope. No further raise: if integration measures P1 over 230,500 the
Team List is cut and nothing else, so `graph` delivers the Team List in its own commit or commits on top,
revertable without touching the Task List or T0. `qa` changes the two test files in `integrate`; the
designer records it in §budget at `design-signoff`.

This replaces the +900 grant to `graph`. The figures the slices are reviewed against, over the
foundation `6f5ae5d5` (P1 213,570, P2 29,227):

| slice | script | P1 at most | CSS | P2 at most |
|---|---|---|---|---|
| `ui` remainder | +2,200 | 215,770 | +1,200 | 30,427 |
| `content` | +4,050 | 217,620 | +1,200 | 30,427 |
| `graph` | +10,161 | 223,731 | +1,400 | 30,627 |
| all three at their caps | +16,411 | 229,981 | +3,800 | 33,027 |
| reserve | 519 | | 973 | |

The 519 bytes of script are held for integration; none is assignable. With everything at its cap the
bundle is at most 598,338 (P1 230,500, P2 34,000, P3 333,152, `index.html` 686), 1,662 under P4.

## Combined scratch builds

Candidates merged in a scratch worktree, conflicts resolved by file ownership (each side's own
stylesheet; `ui.ts` from the side that carries `ui`'s commit), built with `bun run build`.

| what | P1 | P2 | P3 | P4 |
|---|---|---|---|---|
| foundation `6f5ae5d5` | 213,570 | 29,227 | 332,896 | 576,379 |
| `content` `f74bcc9a` alone | 217,580 (+4,010) | 30,404 (+1,177) | 332,896 | 581,566 |
| `graph` `ce9d0e60` alone (carries `ui`'s menu `0f5fbcb2` and `content`'s `VirtualList`) | 225,142 (+11,572) | 30,479 (+1,252) | 332,915 | 589,222 |
| `graph` + `content` | 228,075 (+14,505) | 31,656 (+2,429) | 332,915 | 593,332 |

`graph`'s own share is +9,971 script (cap +10,161; the Team List is 1,026 of it) and +1,252 CSS; the rest
of its delta is `ui`'s menu (about 1,280) and the shared list (about 321). Added separately the three
figures come to 228,831; built together they are 228,075: 756 bytes are shared or saved by minifying
together.

**What is left for `ui`.** Under 230,500 and 34,000, with 300 bytes of script and 150 of CSS kept for
integration, everything of `ui` beyond its menu commit may add at most **2,125 bytes of script and 2,194
of CSS** to the `graph` + `content` build. That is `ui`'s measured room, larger than its cap of +2,200
including the menu (the unused 40 of `content`, 190 of `graph`, the 756 shared and the 519 of reserve
all go to it). `ui` reported +4,981 and +2,611, so it is about 1,580 bytes of script and 420 of CSS over.
The test is the three-way build, not the sum.

### Three-way build with `ui`'s work-in-progress candidate `9712908b`

| what | P1 | P2 | P3 | P4 |
|---|---|---|---|---|
| `ui` `9712908b` alone | 218,703 (+5,133) | 32,352 (+3,125) | 332,896 | 584,637 |
| `graph` `ce9d0e60` + `content` `f74bcc9a` + `ui` `9712908b` | **231,938** | **34,781** | 332,915 | **600,320** |
| ceilings | 230,500 | 34,000 | 350,000 | 600,000 |
| over | 1,438 | 781 | | 320 |

Beyond its menu `ui` adds 3,863 bytes of script and 3,125 of CSS to the combined build, against a
measured room of 2,125 and 2,194. To pass with the integration margin `ui` has to come down by **1,738
bytes of script and 931 of CSS**; P4 then follows. Cutting the Team List (1,026 bytes) would not be
enough for P1 and does nothing for P2.

### Three-way build after `ui`'s cuts

| what | P1 | P2 | P3 | P4 |
|---|---|---|---|---|
| `ui` `109b394f` alone | 216,886 (+3,316) | 31,411 (+2,184) | 332,896 | 581,879 |
| `graph` `05627434` + `content` `95f182ee` + `ui` `109b394f` | **230,178** | **33,853** | 332,915 | **597,632** |
| ceilings | 230,500 | 34,000 | 350,000 | 600,000 |
| under | 322 | 147 | | 2,368 |

All three ceilings hold with every slice's candidate and both List modes. Every owned file in the merged
tree is its owner's version and no frozen file differs from the foundation. `scratchpad/combine.sh`
(graph, content, ui commit ids) repeats the build.

These are the figures the remaining reviews hold the slices to: `ui` at most +3,316 script and +2,184
CSS standing alone, `graph` at most +10,097 of its own (P1 225,313 with its two prerequisites), and the
three-way build under the ceilings. From here a byte added by one slice has to be taken out by the same
slice; 322 and 147 are the whole margin for integration.

`content` `3eb8e667` (one-line list fix after approval): alone 217,539, +21 bytes; the three-way margin is
about 301 bytes of script until the next combined build.

### Three-way build with the latest heads

| what | P1 | P2 | P3 | P4 |
|---|---|---|---|---|
| `ui` `028e56d9` alone | 216,821 (+3,251) | 31,409 (+2,182) | 332,896 | 581,812 |
| `graph` `324b0d1b` + `content` `3eb8e667` + `ui` `028e56d9` | **230,107** | **33,842** | 332,915 | **597,550** |
| under the ceilings by | 393 | 158 | | 2,450 |

Strict gallery on this build, attention and long-text, twelve surfaces including the dialogs and menus,
desktop and narrow: 48 images, 0 failures; no document overflow at 390 px.

### Three-way build with the switcher fix

60 bytes of CSS were granted to `ui` from the margin (m-56aa4164eaf8) for the switcher's rows, which
squeezed the path to nothing and cut the state and the task count; `ui` `dfb8094f` uses all 60.

| what | P1 | P2 | P3 | P4 |
|---|---|---|---|---|
| `graph` `b108dafe` + `content` `3eb8e667` + `ui` `eb41b115` (as first integrated, `fd9d868d`) | 230,128 | 33,842 | 332,915 | 597,571 |
| the same with `ui` `dfb8094f` | **230,128** | **33,902** | 332,915 | **597,631** |
| under the ceilings by | 372 | 98 | | 2,369 |

The integrator's tree at `fd9d868d` is the first row exactly: `web/src` equal to the scratch merge, the
committed manifest equal to the scratch build's. One more line is asked of `ui` (wrap at every width),
which takes about 14 bytes back.

### Polish cards after the first integration

The orchestrator assigned the script margin of 372 to two findings of the designer: 200 to `graph`
(`polish-terminal-states`) and 80 to `ui` (`polish-help-focus`); CSS is to be offset.

| what | P1 | P2 | P3 | P4 |
|---|---|---|---|---|
| integrator's tree with `ui` `dfb8094f` | 230,128 | 33,902 | 332,915 | 597,631 |
| + `graph` `8f378cf7` (ended terminal, empty transcript) | 230,324 (+196 of 200) | 33,902 | 332,915 | 597,827 |
| at most, with `ui`'s 80 | 230,404 | 33,902 | | 597,907 |
| ceilings | 230,500 | 34,000 | 350,000 | 600,000 |
| + `ui` `0156102f` (wrap, −14 CSS) and `6e6856c2` (Help, +36 script of 80): the final source set | **230,360** | **33,888** | 332,915 | **597,849** |
| under the ceilings by | 140 | 112 | 17,085 | 2,151 |

Source stamp of that tree: `fnv1a64:14e8acd06e0d4cac`. Nothing further is assignable; a later change has
to be byte-neutral inside its owner's files.

### Final integrated head

`a8829763` on `agents/integrator` (bundle commit `d092f4a7`): P1 230,360, P2 33,888, P3 332,915,
P4 597,849, source `fnv1a64:14e8acd06e0d4cac`, the figures of the final source set above. The committed
manifest equals my own build's; `just web-check` and `web_bundle` agree. The Team List fallback was not
used. The ledger closes with 140 bytes of script and 112 of CSS under the ceilings.
