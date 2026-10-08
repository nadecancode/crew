# Dashboard features: epics, many tasks, docs, usage metrics, asides

Operator request (w-2d3dcc6b066fa029), in five parts:

1. a `/btw` way to talk to the whole crew without affecting context windows
   or workflow state;
2. live rendering of the knowledge base, so a person can watch the docs
   evolve;
3. epics: each big ask in a session becomes an epic made of tasks, so the
   board is not one flat list;
4. a better board when a session has finished many tasks;
5. token usage metrics, cache hit rate among them, to judge whether the team
   works efficiently.

This note is the experience contract for the five (card `feature-ux-spec`).
It covers the web dashboard first, then what the TUI and the native apps
show. The backend and protocol designs follow it:

| Design card | Owner |
| --- | --- |
| `epics-model` | native-architect |
| `btw-mechanism` | ui |
| `usage-metrics-design` | runtime |
| `knowledge-base-design` | content |

Where a design card has not settled a shape yet, this note states what the
experience needs from it (§Requirements on the design cards).

The visual system, keyboard model, availability states and budgets are those
of [web-revamp.md](web-revamp.md) (§system, §views) and
[web-design.md](web-design.md) (§security, §perf). Nothing here introduces a
font, an icon set, a chart or markdown library, a second state store, or an
HTTP request after the document starts.

## Principles

- **Active work first.** Every view opens on what is moving now: the focus
  epic, working and blocked tasks, open questions. Finished work is one
  action away and never in the way.
- **One name per thing.** The dashboard adds four nouns:
  - **Epic**: a big ask and its tasks.
  - **Docs**: the knowledge base view.
  - **Aside**: a `/btw` question and its answers.
  - **Estimate**: a cost at list price.

  A task stays a task: the dashboard does not say "story".
- **Nothing invented.** Unknown is not zero. A figure the runtimes did not
  report is shown as not reported, an estimate says so, and an epic is never
  marked done by guesswork.
- **Text, not colour.** Every status, trend and selection is readable without
  colour (§system "Colour").
- **The cache comes first.** None of these features puts live data
  (epics, docs, usage, asides) into a member's standing instructions or tool
  descriptions. Epic and usage data reach members only through tools they
  call. Asides never touch a member's live session at all (§Asides).
- **Hundreds, not tens.** A session can hold several hundred tasks, thousands
  of messages and a few hundred docs. Every list is virtualized, and every
  graph has a bounded default view.

## Navigation

The view row becomes **Team, Tasks, Docs, Events, Usage**. Docs is new; its
route is `docs`, and its key is `n` (notes). It is free in the key table, and
`d` stays with the Diff tab.

Asides live in the side panel as a second tab beside **Message bus**:
**Message bus · Asides**. The key `b` opens the Asides tab, and `/btw` in the
command line or composer starts an aside. Help lists the new keys from the
same `KEYS` table.

At 640–767px the five tabs still fit at `--font-sm`. Below 640px, Events and
Usage move into a labelled **More** menu at the end of the row. Docs stays
visible, because live observation is the reason it exists.

The summary row (web-revamp §views "Shell and status bar") gains the focus
epic. It is described under Epics.

## Epics

### What an epic is, as the dashboard sees it

The model is native-architect's `epics-model` (notes/epics.md, in progress).
The dashboard relies on this much of it:

- **Two levels only.** Epic, then task. A task has at most one epic. Tasks
  without one are grouped as **No epic**. That group comes after the open
  epics and before the done-epics archive.
- **Epic fields:** id, title (at most 80 characters), objective text, the
  operator message it came from (`w-…`) when there is one, `created_at`, and
  `closed_at` (when its tasks last all reached an end state).
- **Counts per epic,** computed in core: total, and one per task status, with
  failures as a separate count. Clients draw progress from these counts and
  never count tasks themselves.
- **Derived status,** computed in core:

  | Order | Status | When | Shown as |
  | --- | --- | --- | --- |
  | 1 | planned | the epic has no tasks | **Planned** |
  | 2 | done | all of its tasks have ended | **Done**, or **Done · 1 failed** |
  | 3 | blocked | every unfinished task is blocked | **Blocked** |
  | 4 | active | any task has started or ended (working, review, waiting on a teammate, or finished) | **Active** |
  | 5 | planned | only queued and ready tasks | **Planned** |

  The first rule that matches decides, so an empty epic is planned, never
  done. An epic that has started stays active when a task is reopened,
  because core keeps a first-start marker. "Waiting" means waiting on a
  teammate and counts as active; core infers no operator wait from text.
  There is no fifth status: failures are a count beside the status.
- **The focus epic,** derived in core: the most recently created epic with
  unfinished work, or else the most recent epic. It is part of the snapshot.
  There is no control to set it in v1.
- **Cross-epic dependencies are allowed.** A task carries its epic id, so a
  dependency in another epic can be labelled "in *other epic*".

### Tasks view, grouped by epic

The Tasks view's List mode gains grouping. Group by **Epic** is the default
whenever the session has at least one epic. The other groupings are Status,
Agent and None.

```
Tasks   148 tasks · 112 done        [Search tasks…]  Group: Epic ▾  Status ▾  Agent ▾  ☐ Show done  Density ▾
──────────────────────────────────────────────────────────────────────────────
▾ Toolboxes for team members                          Active · 9 of 14 done      graph, ui, runtime   2 h
    ● toolbox-launch         Working   graph    Launch members with their toolbox     62%
    ● toolbox-dashboards     Review    ui       Show the effective toolbox            (cache-reviewer)
    ○ toolbox-mcp-cli        Queued    scheduler  ← toolbox-launch
    ▸ 9 done  ·  last 14 min ago
▸ Prompt cache hit rate                               Active · 6 of 11 done · 1 failed
▸ Native apps                                         Done · 41 tasks · 6 h ago
  No epic                                             3 tasks
```

**Epic header row.** One row of 44px, or 32px when Compact, the same height as a task row
(§Performance). It shows, in
order:

- a disclosure triangle;
- the title at `--font-base` medium weight;
- the status word;
- `N of M done`, plus `· K failed` when there are failures;
- the agents with unfinished tasks in it (at most three ids, then `+N`);
- the age since the last change (the epic's `updated_at`).

A thin progress bar under the title uses `--ok` for done and `--err` for
failed. It is decorative: the text carries the figures. Selecting the header
opens the epic detail in the side panel. Enter toggles expansion, Right
expands and Left collapses, as in a tree. Space selects without toggling.

**Rows inside an epic** are today's task rows (web-revamp §views "Task DAG
and list"), indented 16px. Unfinished tasks come first, in this status order:
working, review, blocked, waiting, ready, queued. Within one status they keep
board order. The finished tasks of an epic fold into a single row: `▸ 9 done
· last 14 min ago`. Activating it lists them under the row in completion
order, newest first. Tasks without a recorded completion time (from older
sessions) come last, with no time shown. Failed tasks never fold: they stay
visible above the fold row with their status.

**Group order:** the open epics (by `created_at`), then No epic, then the
separator `Done epics (N)` with the done epics, newest first.

**Default expansion:**

- The focus epic, and every active or blocked epic, starts expanded.
- Planned epics start expanded when they have at most five tasks, and
  collapsed otherwise.
- Done epics start collapsed and move below a separator:
  `Done epics (4)`.
- A choice the operator makes holds for the rest of the page's life. It is
  not stored as a preference.

**Dependency labels.** A row whose dependency belongs to another epic shows
`← toolbox-launch (in Toolboxes)` in its dependency summary.

### Task DAG with epics

The DAG draws one epic at a time. Its toolbar gains an **Epic** selector,
which defaults to the focus epic, plus an **All tasks** choice:

- A dependency on a task in another epic is drawn as a subdued context node
  labelled with that epic, not dropped. This follows the existing rule that a
  filter must never suggest independence.
- **All tasks** above 150 nodes shows a notice: "212 tasks are too many to
  read as one graph. Showing the focus epic; choose All tasks to draw
  everything (slower)." The layout itself is unchanged.
- In the DAG, done tasks keep their place, at the same quiet weight as today.
  The DAG never folds them, because folding would break edges.

### Focus epic in the shell

The summary row shows the focus epic beside the team objective, for example
**Epic: Toolboxes for team members · 9 of 14 done**. The task count becomes
the focus epic's count, and the session total sits in its title and
accessible description ("Session: 112 of 148 tasks done"). Without epics,
the row is unchanged.

The workspace switcher row keeps the session total.

### Epic detail

The side panel's detail for an epic shows, in this order:

- the title and status;
- the source ask: the operator message's first lines, with a link that opens
  it in the Message bus. The epic carries only the message id. The text is
  read through the authorized message read when the panel opens, and is
  never copied into the epic data. A viewer who cannot read the message
  sees only its id;
- the objective;
- the counts, as figures and a bar;
- the timeline: created, first task started, closed;
- the agents with their task counts (from the epic summary's agent counts);
- the epic's usage when the metrics provide it (§Usage metrics);
- its tasks, as a compact list with the same fold;
- the docs its tasks link to (§Docs, "Links into Docs");
- the **Report**, when the epic has one (`notes/epics.md`, "Per-epic
  reports and session completion").

**Report.** The section shows the report's summary, its time, and links to
its decisions, which open the operator questions in the Message bus. Under
the heading, a line states what the report covers: "Covers revision 14 ·
current". When the epic changed afterwards, the line reads "Covers revision
14 · 3 changes since" and the summary is shown muted, with the text "This
report is older than the epic's current work." A stale report is never
presented as the current verdict. An epic that was reopened keeps showing
its last report this way. The epic's header row in Tasks marks a report as
"Report" or "Report · outdated" from the summary row alone, without loading
the body. The body is read when the detail opens. An epic without a report
shows no Report section.

There are no epic mutation actions in v1. Creating, renaming and closing
epics belong to the orchestrator's tools, as `epics-model` defines them.

### Team graph

A node's task row is unchanged. Its hover card and the inspector's Current
work line add the epic title.

## Sessions with many tasks

### Defaults by size

| Tasks in the session | Tasks view opens in | Done tasks |
| --- | --- | --- |
| up to 40 | the current default (DAG) | shown |
| more than 40 | List, grouped by Epic (or by Status without epics) | folded per group |

A choice the operator makes overrides these defaults for the rest of the
page's life. The threshold applies when the view first mounts. It never
flips the view while the operator is in it.

### Toolbar

One toolbar row above the list, wrapping at narrow width:

- **Search tasks.** Matches title, id and description, among the tasks
  loaded in the page. The count reads `12 of 148`. Typing filters in place
  with a 120 ms debounce and no request.
- **Group:** Epic, Status, Agent or None.
- **Status:** a multi-select of the task statuses, plus a **Needs attention**
  shortcut for blocked, failed and review.
- **Agent:** the assignee, with **Unassigned**. The reviewer is not an
  option.
- **Show done.** A checkbox that unfolds every group's done row at once. It
  is off by default above 40 tasks.
- **Density:** Comfortable (44px rows) or Compact (32px rows, title and
  status only, with the assignee in the row's accessible name). Density is
  the one choice stored in `web_prefs`, because it is a reading preference
  rather than session state.
- **Clear filters.** Appears only while a filter is set.

The selection survives filtering. If the selected task is filtered out, the
side panel keeps its detail and the list shows "Selected task hidden by
filters · Show it". Filters and grouping also apply to the DAG where they
can: a status filter subdues non-matching nodes and does not remove them.

### Keeping active work in focus

- The list never scrolls on its own. New, changed and finished tasks update
  in place.
- When a task finishes, it moves into its group's done row. The row says `▸
  10 done · last just now`, and the change is announced once through the
  existing polite live region, not per task. If the finished task was
  selected, it stays selected and expanded until the selection changes.
- The **Needs attention** shortcut, and the summary row's counts, make every
  blocked, failed or in-review task reachable in one action.
- **Filters reach folded and archived work.** Search and every filter cover
  all tasks, including those folded into a done row and those in a collapsed
  or archived epic. While a filter is set, each epic and fold holding a
  match is shown expanded, so a failed task in a done epic appears with
  Needs attention. This expansion lasts only while the filter does: Clear
  filters restores the operator's own expansion.

### Performance

- **Rendering.** The task list flattens groups into one list of rows of a
  single height per density: epic headers, task rows, done-fold rows and the
  `Done epics (N)` separator are all 44px in Comfortable and 32px in
  Compact. The epic header fits its title, status and progress bar in that
  height. This keeps `VirtualList`'s fixed row height. Slice 2 extends
  `VirtualList` with a role option (`treegrid` here, `listbox` as today)
  and per-row `aria-level`, `aria-expanded` and `aria-setsize`/`aria-posinset`.
  This is a shared change that the other lists keep working with. The list
  remounts when density changes, preserving selection and scroll anchor.
- **Budgets for 500 tasks in 20 epics**, at P5/P6 hardware:
  - first render of the Tasks list: at most 50 ms;
  - one search keystroke: at most 16 ms of work after the debounce;
  - one task push re-renders only the affected group header and row.
- **DAG.** The layout runs for the selected epic only, unless All tasks is
  chosen.
- **No polling.** Grouping and counts update from pushes.

## Docs (knowledge base)

### What it shows

Docs shows the markdown the team writes, as it is written. Its sources are
listed below; the exact roots are `knowledge-base-design`'s, with a setting
for more.

- **Files:** `README.md`, `notes/**`, `review/**`, `docs/**` and other `.md`
  files in the workspace root and its top-level directories.
- **Images:** files referenced by those docs.
- **Two trees:**
  - the **Workspace**: the operator's checkout as it is on disk. It may
    be on any revision and carry uncommitted edits, so it is labelled
    **Workspace (working copy)**, and the header names its revision. What
    landed is in the recorded history (§History and diff);
  - every **member's worktree**: what is being written now.

  A file present or different in a worktree is marked with that member's id.

```
Docs    Workspace ▾                       [Search paths and headings]
┌───────────────────────────┬──────────────────────────────────────────────────┐
│ README.md                 │ notes/toolboxes.md            Workspace · 2 min ago │
│ ▾ notes                   │ Edited by designer (not committed) · View ▾  History  │
│   dashboard-features.md • │ ─────────────────────────────────────────────────── │
│     designer              │ # Member toolboxes                                 │
│   toolboxes.md •          │ Operator request (w-9599bbf059d8f7a6): …           │
│     designer, graph       │ ## Current state                                   │
│   ▸ toolbox-probes        │ …                                                  │
│ ▸ review                  │                                                    │
└───────────────────────────┴──────────────────────────────────────────────────┘
```

### Layout

- **Desktop.** Docs fills the main region. On the left is a file tree
  (240px, resizable, minimum 180px), on the right the document, at a
  readable measure of 72 characters (`max-width: 72ch`). The side panel works
  as on other views: Message bus, Asides, or a selected entity's detail.
- **Below 768px.** The tree and the document replace each other, with a Back
  control, as the side panel does.
- **Tree.** It shows directories and files by name. A changed file has a
  marker dot, followed in the row by the ids of the members whose worktree
  differs (as text, not just a dot). Arrows navigate it, Enter opens a file,
  and Right/Left expand and collapse directories. A search field
  (placeholder "Search paths and headings") narrows it by path and heading
  text; a heading match opens the file at that heading. Full-text search is
  not in v1 (`notes/knowledge-base.md`). Files the team changed in the last hour can be listed first with
  **Sort: Recently changed**.
- **Document header.** It shows:
  - the path in monospace;
  - the version shown: Workspace, or a member's worktree;
  - when that version last changed, and by whom when the history says so;
  - **View:** Workspace or each member with a differing version;
  - **History**;
  - **Copy path**.

  A heading outline (`h2`/`h3`) appears as a right-hand rail when the
  window is wider than 1200px, and in a header menu otherwise.

### Rendering

- **Server-side parsing.** The server parses markdown into a restricted
  syntax tree and sends it over the existing WebSocket. The client draws that
  tree with its own components: no HTML string is ever inserted, which keeps
  `style-src-attr 'none'` and the script policy intact.
- **Supported nodes:** headings, paragraphs, emphasis, strong, inline code,
  code blocks (monospace, no highlighting, horizontal scroll inside the
  block), block quotes, lists, task-list items (read-only), tables (scrolling
  inside their own box), thematic breaks, links and images.
- **Raw HTML** in a document is shown as literal text in a muted code style.
- **Links:**
  - A relative link to another doc opens it in Docs.
  - A link to a heading scrolls to it.
  - `http:`/`https:` links open in a new tab with `rel="noopener noreferrer"`.
  - Every other scheme is shown as text (web-design `text.test.ts` rules).
- **Images:**
  - Only static PNG, JPEG, GIF and WebP images are shown, within the
    limits of `notes/knowledge-base.md`: 2 MiB per image, 8 MiB per
    document, 8,192 pixels per axis and 16 million pixels.
  - The syntax tree carries an image reference (path and content hash),
    not the bytes. The client fetches the image in chunks over the existing
    authenticated WebSocket (each chunk well under the 1 MiB frame limit),
    assembles it, and shows it as a `data:` URL. Images are fetched as they
    come into view, a few at a time, and are cached by content hash, so a
    new version of a document does not fetch an unchanged image again. No
    HTTP request is made. While an image is loading, its alt text holds its
    place.
  - An image that is not shown keeps its alt text, followed by the reason as
    text:
    - "Image not shown · SVG is not supported";
    - "Image not shown · animated images are not supported";
    - "Image too large to show (3.4 MiB)", or with its dimensions,
      "Image too large to show (9000 × 400 px)";
    - "Image not shown · outside the docs folders";
    - "Image not shown · this version's image is no longer kept here", for
      an expired image of a Seen-here version. A current image is never
      shown in its place.
  - Remote images are never fetched; their alt text and URL are shown as a
    link.
  - SVG is never shown in any form: not as an image source, document,
    object or embed.
- **Budget.** The renderer and the Docs view load as a second chunk requested
  when the document starts, as the terminal chunk is (web-design §security
  allows no later request). Their bytes count against P1/P4 like everything
  else. A budget raise for this chunk is decided with its implementation
  card, as for toolbox-dashboards.

### Live updates

- **The open document updates in place** when the version being viewed
  changes, with no flash: the Workspace working copy, or the one member's
  worktree chosen in View. The scroll position is anchored to the heading at
  the top of the viewport, so reading is not interrupted. A one-line notice
  under the header reads "Updated just now · Show changes".
- **A change in another version** never replaces what is shown. While the
  Workspace is viewed and graph's worktree changes the same file, the
  notice reads "graph's worktree has a newer version · View it · Compare",
  and the View menu marks that member.
- **Show changes** marks the blocks changed since the page opened the file:
  a 3px `--info` bar on the left plus the text "changed" for assistive
  technology. It stays until the operator turns it off.
- **Deleted files.** A file deleted while open keeps its last content, with
  the notice "This file was removed from the workspace · Back to Docs".
- **Tree markers** update from pushes. No polling. A burst of saves is
  coalesced to at most one update per file per second.

### History and diff

**History** opens a timeline for the file in the side panel, with two kinds
of entry:

- **Recorded:** each published commit that touched the file, on the
  workspace's history and on members' branches, with author, time,
  description and change id. A working copy is never recorded as a
  commit.
- **Seen here:** versions this connection received since it opened, which
  cover uncommitted saves between commits. They are kept in memory only.
  The section's footer reads "Seen here: up to 16 versions or 4 MiB of this
  file while connected; older versions may expire." Expired entries drop
  from the list, unless one is selected.

A **selected Seen-here version that has expired** keeps its selection and
header path. Its body reads "This version is no longer kept here. Seen-here
versions expire when the connection drops or space runs low.", with
**View Workspace** and, when a recorded commit exists, **View recorded
<short sha>**. The view never switches by itself.

Selecting one entry shows it. Selecting two shows a diff between them, using
the existing Diff component (file and hunk structure, line-based on the
markdown source). **Compare with Workspace** diffs a member's version against
the Workspace working copy. To compare with a recorded revision, pick it in
the history. The history is read once when it opens and then grows from
pushes.

### Links into Docs

Paths mentioned in task titles, descriptions, results and notes, message
bodies, event messages and `file` artifacts become links into Docs. A path
counts when it names a file the Docs index knows, such as
`notes/toolboxes.md`. `notes/toolboxes.md §Spec` also scrolls to the heading
whose text starts with `Spec`.

Matching runs only against known paths, so nothing else becomes a link. A
known path that is not available yet (only in a member's worktree, say) opens
that version.

A document's header menu offers **Referenced by**: the tasks and messages
loaded in the page that link to it, with the scope stated ("in loaded
history").

### Search

v1 searches file paths and headings from the server's index; the field's
placeholder is "Search paths and headings". Full-text search is not in v1
(`notes/knowledge-base.md`). If a later version adds a server full-text
index, the field then says "Search all docs" and shows matched lines with the
file and heading.

## Usage metrics

### Definitions

These are shared with `notes/usage-metrics.md` (approved b1fcceca) and
`notes/prompt-cache.md`, so every view and benchmark uses the same words.
The server computes every figure and its coverage; clients display them and
never recompute from a filtered list.

| Measure | Definition | Shown as |
| --- | --- | --- |
| Input | uncached input tokens | tokens |
| Cache read | tokens read from the prompt cache | tokens |
| Cache write | tokens written to the prompt cache, split into 5-minute and 1-hour writes where the runtime reports the split; an aggregate without the split is kept as **TTL not reported** | tokens |
| Output | output tokens | tokens |
| Total input | the runtime's own normalized input total when it reports one (Codex reports total input with cached tokens inside it); otherwise input + cache read + cache write, when all three are reported | tokens |
| Hit rate | cache read ÷ total input | percent, one decimal |
| Write share | list cost of cache writes ÷ input-side list cost, on the same requests | percent, or a range `38–44%` when write TTLs are unknown |
| Estimate | each category's tokens × that model's list price for the category; 5-minute and 1-hour writes at their own prices | `≈ $12.40`, a range `≈ $2,540–$2,610`, or a partial `at least $2,556` (below) |
| Per completed task | for the tasks first completed in the period: their observed cost from the start of the run up to each first completion, ÷ the number of those tasks | `≈ $22.80`, with "112 tasks first completed in this period" |
| Requests | distinct requests the runtime reports | a number, or `≥ 9,612` with the reason when some usage arrives in batches |
| Turns | turns correlated between the runtime and the daemon | a number, or `≥ N` with its own reason |

**Coverage, metric by metric.** Each metric uses every request that reports
what that metric needs, and states its own coverage when that is not every
request, for example "Hit rate 97.9% · over 9,412 of 9,801 requests". A
missing field is never counted as zero. There is no single "agents
reporting" count for all metrics: the scope line names the hit rate's
("hit rate from 11 of 12 agents"), and each figure whose coverage is
incomplete carries its own "K of N".

- **Hit rate** needs cache read and a total input. So a Codex request
  reporting total input 100 and cache read 80, with no write counters, gives
  a hit rate of 80%.
- **Write share** needs the write tokens and the input-side cost, so that
  same Codex request is outside its coverage: unknown, not 0%.
- **Estimate.** A Claude request reporting an aggregate cache write without
  the TTL split has a known hit rate, but an unknown exact write price. The
  estimate prices that portion as a range:
  - from the 5-minute price, the lower bound;
  - to the 1-hour price, the upper bound.

  It then reads `≈ $12.40–$13.10` with "includes 1.2 M write tokens with no
  TTL reported".
- **Three estimate states**, which never look alike:
  - a finite range: `≈ $2,540–$2,610`;
  - a partial subtotal, when some cost has no upper bound (a model without a
    price, tasks without observed cost): `at least $2,556 · 1 model has no
    price`, or for Per completed task `at least $22.80 · cost observed for
    104 of 112`. No "≈" and no range dash;
  - none: `Estimate unavailable · <reason>`.

  An "at least" value rounds down. Both ends of a range round outward.
- **Per completed task.** A task counts once, at its first completion, even
  if it is reopened and completed again. Its cost can predate the period;
  the note then adds "· includes their earlier spend". With an epic scope,
  the cohort is the tasks first completed in that epic. All-period spend ÷
  the current done count is a different ratio and is not shown in v1.
- **Unavailable.** A rate whose denominator is zero, or a scope with no
  covered requests, shows **unavailable** with the reason, which tells
  apart "no covered input", "zero input", "no task first completed in this
  period" and "no cost observed for these tasks". For example: "Hit rate
  unavailable · no input reported", "Write share unavailable · write counts
  not reported", "Per completed task unavailable · no task first completed
  in this period".
- **Not reported.** A runtime that never reports a field shows **not
  reported** for it.
- **Source gaps.** While session files are still being read, or one is
  missing or in a schema crew does not support, one line under the figures
  says so: "Still reading 2 session files · figures may change". Figures can
  go down as well as up while that line shows. A gap is never shown as 0.
- **Tracking boundary.** The scope line says "Tracking since Oct 7, 22:40".
  The period before it shows "Earlier usage not tracked".
- **Price basis.** The scope line says "list-price estimate, prices checked
  Oct 8". Help states that this is the standard API price for the same
  tokens, not a bill, a subscription charge or a quota.


**Why both rates.** Hit rate and Write share always appear together. In the
prompt-cache audit's cohort (49 sessions, Oct 6–8, list prices read Oct 8),
the hit rate was 97.9% while cache writes were 56.3% of input-side list cost.
That is an observation of one cohort, not a general law; showing both lets
each session be judged on its own figures.

Estimates use a price table that names its source and date (the audit's
`prices.tsv` format), with separate 5-minute and 1-hour write prices. A
model without a price shows **No price**, and the total then says "excludes
2 models without a price". Subscription billing is not modelled; the label
says list price.

**Asides are a separate scope.** The usage of `/btw` answers is recorded
apart from the live sessions' usage. Session, epic and agent figures,
history and benchmarks exclude it. The Usage view shows it as its own
**Asides** line ("Asides: ≈ $1.12 · 6 answers"), and the scope selector
offers **Asides** to see it alone.

### Usage view

The Usage view (web-revamp §views "Usage") keeps its quota windows and
per-agent rows, and gains a metrics section above them.

- **Scope.** Session (the default), the focus epic, or any epic, through a
  selector. The workspace's earlier sessions appear in History, below.
- **Summary figures.** At most five, at `--font-metric`, each with a label
  and its period:
  - Total tokens;
  - Hit rate;
  - Write share;
  - Estimate;
  - Per completed task.
- **Trend.** A small chart over the session, with one point per bucket (5
  minutes up to 6 hours, then hourly). It shows either hit rate and write
  share as lines, or the four token categories as stacked bars (using the
  `--token-*` colours), through a two-way toggle. A bucket whose requests do
  not all report the four categories is drawn as a hatched gap labelled
  "split not reported", never as a zero-height bar. The latest values are
  printed beside it as text. History comes from stored samples
  (`usage-metrics-design`). Before any sample exists, the chart says "No
  samples yet" and draws nothing.
- **Breakdown table.** By Agent, Epic or Model. Columns:
  - Input;
  - Cache read;
  - Cache write;
  - Output;
  - Hit rate;
  - Write share;
  - Estimate;
  - Requests;
  - Turns.

  Rows sort by any column. With Epic, the rows include **No epic** (tasks
  outside any epic) and **Not attributed** (usage with no task) as two
  separate rows. Numbers use tabular figures. Below 768px the
  table becomes stacked rows (web-revamp "At narrow width…").
- **Big writes.** The requests that wrote at least 10k tokens and more than
  they read (the audit's definition), with agent, time, tokens written per
  TTL and estimate. A row opens the agent's inspector at that time in the
  transcript when it is available. Each row also has a **Context** field:
  - an event recorded next to the request, named as observed with its
    time, for example "Compaction at 00:40 (1 min before)" or "Session
    resumed at 00:12";
  - or **Unknown** when no such event is recorded.

  The dashboard never calls a write avoidable, and never names a cause it
  did not observe.
- **History.** The workspace's sessions, one row each:
  - start, duration;
  - tasks done;
  - hit rate, write share;
  - estimate, per completed task.

  The current session is marked. Selecting a row scopes the figures to that
  session read-only; this is the benchmark comparison the operator asked for.

### Elsewhere

- **Inspector.** The Session section adds the agent's hit rate, write share
  and estimate.
- **Epic detail.** It shows the epic's totals, hit rate, write share and
  estimate. Usage is attributed to an epic through the task each turn worked
  on, as `notes/usage-metrics.md` defines it. Its Per completed task uses
  the tasks first completed in this epic.
- **Summary row.** It shows nothing new; the existing UsageStrip keeps quota
  pressure visible. Metrics are a Usage-view concern.

## Asides (`/btw`)

### What an aside is

An aside is a question put to members on the side. Each chosen member
answers from a copy of its current context. The live session, its context
window, inbox, tasks, the message bus and the board are untouched. Members
never learn that an aside happened.

The mechanism is `btw-mechanism`'s. This contract only assumes four things:
answers are produced away from live sessions, they arrive asynchronously,
each can fail on its own, and each reports its own usage.

### Asking

- **Starting an aside.** There are three ways:
  - type `/btw` followed by the question in the composer or the command
    line;
  - choose **Ask aside…** in Commands;
  - press `b` and then **New aside**.
- **The composer in aside mode.** The composer switches to aside mode
  visibly:
  - the recipient line reads **Aside to: Everyone ▾**;
  - the composer border uses `--info` instead of `--action`;
  - one line of text under the field reads: "Answered from copies. Live
    sessions, inboxes and tasks are not touched."

  Escape leaves aside mode and keeps the typed text.
- **Recipients.**
  - **Everyone** is the default: every member that can answer.
  - The picker lists members with checkboxes. A member that cannot answer is
    listed, disabled, with its reason, such as "No session yet", "Shell
    member" or "Runtime cannot copy sessions" (`btw-mechanism` decides
    which).
  - The orchestrator is included in Everyone, and can be deselected.
- **Before sending.** No cost estimate is shown, because none is known until
  the answers report usage. The first aside in a page shows a one-time note:
  "Each answer reads the member's whole context, so an aside to everyone
  costs about one turn per member."
- **Sending** closes aside mode and opens the Asides tab on the new aside.

### Answers

The **Asides** side panel tab lists asides newest first. Each aside shows:

- the question (first three lines, with More);
- the time and recipients;
- an aggregate state, such as `4 of 6 answered`;
- one answer card per member:
  - **header:** the member id, runtime and model, then the status word:
    Waiting, Answering, Answered, Failed (with the reason) or Not available;
  - **body:** the answer as plain text with safe links. Long answers fold
    after 12 lines.
  - **footer:** "From a copy of claude-1's context at 14:02", the answer's
    tokens and estimate, and **Copy**.

The aside header shows the total cost of all its answers.

Each aside offers **Cancel** while answers are pending, and **Remove from
this list**. Removing only clears the dashboard view; it does not undo
usage.

**Send to claude-1 as a message…** is the only way an aside's content
reaches a live session. It opens the normal composer prefilled with the
question and answer, so the operator sees and edits exactly what will be
sent. Sending then follows normal message rules, with no special path.

### Never in the live paths

- Asides never appear in the Message bus, a member's inbox or transcript,
  Events (except one `aside.asked`/`aside.answered` event per aside, with no
  body), the board, or any briefing.
- The Asides tab heading carries the fixed text **Live sessions untouched**.
  Each answer's footer repeats where it came from.

### Kept where

Asides live for the daemon session, with a bounded history: the last 50
asides, with their answers truncated to 64 KiB each. They are not saved to
the roster. Whether they survive a daemon restart is `btw-mechanism`'s
decision; the tab says "Asides from before the dashboard reconnected are
gone" when they did not survive.

## TUI and native apps

v1 means the same release as the web feature; later means a follow-up card.

| Feature | TUI | GTK (Linux) | Apple |
| --- | --- | --- | --- |
| Epic grouping in task list, fold of done tasks | v1: grouped list, Enter/arrows fold, `D` toggles done | v1 | v1 |
| Focus epic in summary | v1, status line | v1, Team header | v1 |
| Epic detail | v1, detail pane | v1, inspector | v1 |
| Task search, filters, density | v1 search and status filter; no density | v1 search, status, agent | v1 search, status |
| DAG by epic | later | later (graph view per epic) | later |
| Docs | later: a file list and plain-text view are possible, but not rendered markdown | later | later |
| Usage metrics: summary figures, breakdown | v1 text table in Usage | v1 | v1 |
| Usage trend chart, big writes, history | later | later | later |
| Asides: ask, answers | v1: `/btw` in the command line, answers in a panel | later | later |

The native apps reuse `crew-client` DTOs, so epic, usage and aside data
arrive there with no extra protocol. Each client renders them with its own
controls (GTK list sections; SwiftUI `Section` with headers). Copy and status
words are identical to this note.

## Accessibility and keyboard

- **Grouped task list.** It is a `treegrid`: epic rows have `aria-level=1`
  and `aria-expanded`, and task rows `aria-level=2`. A done-fold row is a
  button, and its label states the count. Arrows, Home/End, Enter and
  Left/Right follow the tree pattern; single-key shortcuts follow the
  existing rules.
- **File tree.** It is a `tree` with the same keys. The document is an
  `article` with real heading levels, and each image has its alt text, or the
  file name when the alt text is empty.
- **Live updates.** Polite announcements, at most one per change burst,
  phrased like "notes/toolboxes.md updated". They are never assertive and
  never steal focus.
- **Charts.** Every chart has its figures as adjacent text, and a table
  equivalent is reachable from a **Show as table** button.
- **Aside mode.** It is announced when entered: "Aside mode: answered from
  copies, live sessions untouched".
- **New keys:**
  - `n`: the Docs view;
  - `b`: the Asides tab;
  - `D`: in Tasks, show or hide done tasks. (`h` already moves left in the
    DAG and to the previous row in lists, so it is not reused.)

  Each is listed once in `KEYS`, and Help reads that table.
- **Contrast.** Contrast and focus rules are web-revamp §system's, and the
  new surfaces join `contrast.mjs`.

## Acceptance cases

QA runs these against a fixture workspace with:

- 3 epics plus No epic;
- 480 tasks in all, 430 of them done and 4 failed;
- 40 markdown files, with 2 members' worktrees changing 3 of them during the
  test;
- recorded usage samples for 6 agents across 2 runtimes and 3 models, one of
  them without a price;
- a stub aside backend with one slow answer, one failed answer and one
  unavailable member.

Captures are taken in light and dark at 1440×960 and 390×844.

| ID | Case | Pass when |
| --- | --- | --- |
| F01 | Tasks opens on a large session | List, grouped by Epic. The focus epic and active epics are expanded, done epics are collapsed under "Done epics (1)", and done tasks are folded per epic. Failed tasks of open epics are visible above their fold; the archived done epic with a failure says "Done · 1 failed" in its header. The first render is at most 50 ms. |
| F02 | Epic header | It shows the title, status word, `N of M done · K failed`, the agents and the age. Progress reads correctly with colour removed (forced colours). Enter, Left and Right expand and collapse. The screen reader reads the level, the expanded state and the counts. An epic with a report shows "Report"; after one more change to the epic, "Report · outdated", and its detail reads "Covers revision R · 1 change since" with the summary muted. |
| F03 | Status derivation | Fixtures for planned (an empty epic, and one with only queued and ready tasks), active (one working; one waiting on a teammate), blocked (all unfinished tasks blocked) and done with one failure show the specified words. A client never computes them itself; the DTO supplies them. |
| F04 | Search and filters | Typing narrows to the matching tasks with the count `x of 480`. Needs attention shows exactly the blocked, failed and review tasks. A selected task that is filtered out keeps its detail and offers Show it. A failed task in an archived done epic appears, its epic expanded, under Needs attention. Clear filters restores everything, including the earlier expansion. No request is sent while typing. |
| F05 | Finishing work | A task push to done moves it into the fold, updates the header counts, announces once and does not scroll the list. A selected finishing task stays selected and shown. |
| F06 | DAG by epic | It draws the focus epic. A cross-epic dependency appears as a labelled, subdued context node. All tasks above 150 shows the notice. A status filter subdues nodes and keeps edges. |
| F07 | Docs tree and rendering | The tree lists every doc. Changed files show markers with the member ids. A document renders headings, lists, tables, code and images. Raw HTML shows as text and `javascript:` links stay text. An oversized image, an SVG and an animated GIF each show their alt text and the matching reason; no request is made for any of them and no CSP violation is logged. Searching for a heading's text lists that heading and opens the file there. |
| F08 | Live document update | While a member's version is viewed, a change to it updates the document in place, with scroll anchored at the top heading and the notice "Updated just now". Show changes marks the changed blocks with text. A burst of 20 saves gives one update per second. While the Workspace is viewed, a member's change only shows the newer-version notice and never replaces the content. |
| F09 | Workspace and member versions | View switches between Workspace and the member's version. Compare with Workspace shows a diff in the existing Diff component. |
| F10 | History | The timeline lists the recorded commits and the versions seen since the page opened, with the Seen-here footer. Picking two entries shows the diff between them. After the connection drops, an unselected Seen-here entry is gone, and a selected one shows the expired state with View Workspace; the shown content never switches by itself. |
| F11 | Links into Docs | Task text, message bodies and `file` artifacts containing `notes/x.md` or `notes/x.md §Heading` link to the document at that heading. Unknown paths are not links. Referenced by lists the linking tasks and messages, with its scope stated. |
| F12 | Usage figures | Total, Hit rate, Write share, Estimate and Per completed task match a hand computation from the fixture under the §Definitions formulas (the arithmetic examples of `notes/usage-metrics.md`). The model without a price shows No price, and the estimate shows `at least …` with the exclusion, never `≈`. A non-reporting agent shows not reported, not 0. A Codex fixture request with total input and cache read but no write counters counts toward hit rate and not toward write share. A Claude request with an aggregate write and no TTL split makes the estimate and write share ranges with their note. A task reopened and completed again counts once. A period with no first completion shows Per completed task unavailable with the reason. |
| F13 | Trend and breakdown | The trend draws buckets from the samples, with the latest values as text. A bucket without a full category split is a hatched gap labelled split not reported. Show as table gives the same figures. The breakdown sorts by every column, shows `≥` Requests with its reason for a batch-reporting runtime, lists No epic and Not attributed as separate rows, and the narrow layout stacks without page overflow. |
| F14 | Big writes, history and gaps | Big writes lists exactly the fixture's qualifying requests, and opens the agent at that time. History lists both sessions, and selecting the earlier one scopes the figures read-only. With a session file still being read, the gap line shows and no figure shows 0 for it; a later revision that lowers a figure is drawn as is. The scope line shows the tracking boundary and the price date. |
| F15 | Aside ask | `/btw` switches the composer to aside mode with the copy line and the `--info` border. The recipient picker lists unavailable members with reasons. Escape keeps the text. Sending opens the Asides tab. |
| F16 | Aside answers | Answers arrive independently; the slow one shows Answering and the failed one shows its reason. The footer names the source copy and time, and tokens and estimate are shown. Cancel stops pending answers. |
| F17 | Aside isolation | During and after an aside: no bus row, inbox entry, transcript line, task change or briefing change. Only `aside.*` events without bodies appear. The fixture's live sessions receive no input. Send as message opens the normal composer prefilled, and only sending it reaches the member. |
| F18 | Keyboard and screen reader | Every new control is reachable by Tab. The `n`, `b` and `D` keys work and are listed in Help. Tree and treegrid semantics hold, the announcements are polite and bounded, and focus is never stolen. |
| F19 | Budgets | P1, P2 and P4 hold, or a raise is recorded with its card (as for toolbox-dashboards). The Docs chunk is requested at document start and never later. The idle page requests no animation frames. |
| F20 | Parity | The TUI and native clients show epic grouping, the done fold, the focus epic and usage summary figures with the same words for the same fixture. The TUI also has `/btw`. |

## Implementation split

Each slice is one card. The protocol pieces depend on the design cards.

1. **Protocol and core (owners of the design cards).**
   - Epic DTOs, counts, status and focus (`epics-model`).
   - Usage samples, attribution and price table (`usage-metrics-design`).
   - Docs index, syntax tree, watching, history (`knowledge-base-design`).
   - Asides lifecycle and events (`btw-mechanism`).
   - All of it in `crew-protocol`/`crew-core` and the daemon, with the web
     server relaying it as WebSocket frames.
2. **Web, tasks** (`web/src/views/TaskPanel.tsx`, `web/src/graph/EntityList.tsx`,
   `web/src/graph/TaskDag.tsx`, `web/src/store`):
   - grouping, the fold, the toolbar, density;
   - the DAG epic selector;
   - the summary row's focus epic;
   - epic detail.

   Covers F01–F06.
3. **Web, Docs** (new `web/src/views/Docs*.tsx` in its own chunk; route and
   keys in `web/src/route.ts`, `web/src/keys.ts`):
   - tree, renderer, live update;
   - history and diff (reusing the Diff component);
   - links into Docs (`web/src/text.tsx`).

   Covers F07–F11.
4. **Web, Usage** (`web/src/views/UsageView.tsx`, `web/src/views/usage.ts`,
   the Inspector's Session section): figures, trend, breakdown, big writes,
   history. Covers F12–F14.
5. **Web, Asides** (`web/src/views/SidePanel.tsx`, the composer in
   `web/src/views/MessageBox.tsx`, a new `web/src/views/Asides.tsx`,
   `web/src/commands.ts`): aside mode, picker, answers, send as message.
   Covers F15–F17.
6. **TUI** (`crates/crew-tui`): epic grouping and fold, focus epic, search
   and status filter, usage figures, `/btw`. Covers F20 (TUI).
7. **Native** (`crates/crew-desktop`, `apps/apple`): epic grouping and fold,
   focus epic, usage figures. Covers F20 (native).
8. **QA:** the fixture workspace, captures in light and dark at both sizes,
   F18 and F19.

Dependencies:

- Slice 2 needs `epics-model`.
- Slice 3 needs `knowledge-base-design`.
- Slice 4 needs `usage-metrics-design`.
- Slice 5 needs `btw-mechanism`.
- Slice 6 (TUI) needs `epics-model`, `usage-metrics-design` and
  `btw-mechanism`.
- Slice 7 (native) needs `epics-model` and `usage-metrics-design`.

Each slice's epic part can land as soon as `epics-model` does, ahead of its
usage and aside parts.

## Requirements on the design cards

What this experience needs from each design, stated so each card can confirm
or push back:

- **`epics-model`** (native-architect): agreed in messages m-8eaacfefb5fc and
  m-f121eb53d7a7:
  - Epic then task. Epic fields as above.
  - Counts per status, plus failed.
  - Derived status with the precedence above.
  - Focus epic in the snapshot.
  - `completed_at` on every finished task.
  - Epic data only in the snapshot, events and tools, never in briefings.
- **`usage-metrics-design`** (runtime):
  - Per-request samples or buckets with agent, model, task (for epic
    attribution) and time. They are kept for the workspace's sessions, so
    History and trends survive restarts.
  - Per request: input, cache read, cache write (with its TTL split when the
    runtime reports one), output, and the runtime's own normalized total
    input when it has one. Each field is "not reported" when absent,
    distinct from zero, so each metric can state its own coverage.
  - A versioned, dated price table with separate write prices per TTL.
  - Big writes with the events observed next to them, for the Context
    field.
  - Aside usage recorded apart from live sessions.
  - The same definitions as §Definitions and `notes/prompt-cache.md`.
- **`knowledge-base-design`** (content):
  - The roots and a setting to extend them.
  - A watched index of the workspace and members' worktrees, coalesced to one
    update per file per second.
  - The restricted syntax tree, parsed server-side, with no raw HTML.
  - Images as data within the size limits.
  - Per-file history from version control, plus a seen-here buffer.
  - The known-path list for link matching.
  - Optional full-text search.
  - Read-only access confined to the roots, with no path traversal and no
    symlinks out of the workspace.
- **`btw-mechanism`** (ui):
  - Answers produced from copies of members' contexts, without delivery to,
    waking of, or appending to live sessions (orchestrator's inputs, card
    text).
  - Per-member availability with reasons.
  - Asynchronous answers with their own status and usage.
  - Cancel.
  - `aside.asked`/`aside.answered` events without bodies.
  - The retention stated above, or a different one with a reason.

## Mockups

The captures in [dashboard-features/](dashboard-features/) are static HTML
mockups built on `web/src/theme.css` tokens, at 1440×960 in light and dark:

- Tasks grouped by epic;
- Docs with a live-update notice;
- Usage metrics;
- the Asides tab.

They fix layout, hierarchy and copy, not pixels. The implementation follows
the existing components where they differ. They were captured with headless
Chromium 151 from the files (`tasks.html#dark` selects the dark palette).
Their hashes are in `captures.sha256`.
