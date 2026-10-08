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

**Epic header row.** One row of 44px, or 32px when Compact. It shows, in
order:

- a disclosure triangle;
- the title at `--font-base` medium weight;
- the status word;
- `N of M done`, plus `· K failed` when there are failures;
- the agents with unfinished tasks in it (at most three ids, then `+N`);
- the age since the last change.

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
- **All tasks** above 150 nodes shows a notice: "148 tasks are too many to
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
  it in the Message bus;
- the objective;
- the counts, as figures and a bar;
- the timeline: created, first task started, closed;
- the agents with their task counts;
- the epic's usage when the metrics provide it (§Usage metrics);
- its tasks, as a compact list with the same fold;
- the docs its tasks link to (§Docs, "Links into Docs").

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

### Performance

- **Rendering.** The task list uses the existing `VirtualList` with
  variable-height group rows. Its row heights are recomputed when density
  changes.
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
  - the **Workspace**: the operator's checkout, which is what landed;
  - every **member's worktree**: what is being written now.

  A file present or different in a worktree is marked with that member's id.

```
Docs    Workspace ▾                                   [Filter files…]
┌───────────────────────────┬──────────────────────────────────────────────────┐
│ README.md                 │ notes/toolboxes.md            Workspace · 2 min ago │
│ ▾ notes                   │ Edited by designer (not landed) · View ▾  History  │
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
  and Right/Left expand and collapse directories. A filter field narrows it
  by path. Files the team changed in the last hour can be listed first with
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
  - A workspace image arrives as a `data:` URL inside the tree, within a
    size limit (2 MiB per image, 8 MiB per document).
  - An oversized image shows its alt text and "Image too large to show
    (3.4 MiB)".
  - Remote images are never fetched; their alt text and URL are shown as a
    link.
  - SVG is shown only through `img` (`data:image/svg+xml`), which runs no
    script.
- **Budget.** The renderer and the Docs view load as a second chunk requested
  when the document starts, as the terminal chunk is (web-design §security
  allows no later request). Their bytes count against P1/P4 like everything
  else. A budget raise for this chunk is decided with its implementation
  card, as for toolbox-dashboards.

### Live updates

- **The open document updates in place** when its file changes, with no
  flash. The scroll position is anchored to the heading at the top of the
  viewport, so reading is not interrupted. A one-line notice under the header
  reads "Updated just now by graph's worktree · Show changes".
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

- **Recorded:** each version-control change that touched the file, on the
  workspace's history and on members' branches, with author, time,
  description and change id.
- **Seen here:** versions the page observed since it opened, which cover
  uncommitted saves between commits.

Selecting one entry shows it. Selecting two shows a diff between them, using
the existing Diff component (file and hunk structure, line-based on the
markdown source). **Compare with Workspace** diffs a member's version against
the landed one. The history is read once when it opens and then grows from
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

v1 searches file paths and headings, among the documents loaded in the page.
Full-text search is conditional on `knowledge-base-design` providing a server
index; the field then says "Search all docs" and shows matched lines with the
file and heading.

## Usage metrics

### Definitions

These are shared with `usage-metrics-design` and `notes/prompt-cache.md`, so
every view and benchmark uses the same words:

| Measure | Definition | Shown as |
| --- | --- | --- |
| Input | uncached input tokens | tokens |
| Cache read | tokens read from the prompt cache | tokens |
| Cache write | tokens written to the prompt cache (all TTLs) | tokens |
| Output | output tokens | tokens |
| Hit rate | cache read ÷ (input + cache read + cache write) | percent, one decimal |
| Write share | cache write list cost ÷ input-side list cost | percent |
| Estimate | tokens × the model's list price per category | `≈ $12.40`, labelled "list-price estimate" |
| Cost per done task | estimate ÷ tasks done in the scope | `≈ $0.84` |

The prompt-cache audit showed that write volume, not hit rate, drives cost
(97.9% hit rate with writes at 56.3% of input-side cost). So every summary
shows Hit rate and Write share together, never Hit rate alone.

Estimates use a price table that names its source and date (the audit's
`prices.tsv` format). A model without a price shows **No price**, and the
total then says "excludes 2 models without a price". Subscription billing is
not modelled; the label says list price.

Not every runtime reports every field. A missing category is **not
reported**, never zero. A scope where only some agents report shows "N of M
agents reporting".

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
  - Cost per done task.
- **Trend.** A small chart over the session, with one point per bucket (5
  minutes up to 6 hours, then hourly). It shows either hit rate and write
  share as lines, or the four token categories as stacked bars (using the
  `--token-*` colours), through a two-way toggle. The latest values are
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
  - Turns.

  Rows sort by any column. Numbers use tabular figures. Below 768px the
  table becomes stacked rows (web-revamp "At narrow width…").
- **Big writes.** The requests that wrote at least 10k tokens and more than
  they read (the audit's definition), with agent, time, tokens written,
  estimate and cause class when known. A row opens the agent's inspector at
  that time in the transcript when it is available. This list is what makes
  the metric actionable.
- **History.** The workspace's sessions, one row each:
  - start, duration;
  - tasks done;
  - hit rate, write share;
  - estimate, cost per done task.

  The current session is marked. Selecting a row scopes the figures to that
  session read-only; this is the benchmark comparison the operator asked for.

### Elsewhere

- **Inspector.** The Session section adds the agent's hit rate, write share
  and estimate.
- **Epic detail.** It shows the epic's totals, hit rate, write share and
  estimate. Usage is attributed to an epic through the task each turn worked
  on, as `usage-metrics-design` defines it. Turns without a task show as
  "Not attributed".
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
| Epic grouping in task list, fold of done tasks | v1: grouped list, Enter/arrows fold, `h` toggles done | v1 | v1 |
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
  - `h`: in Tasks, show or hide done tasks.

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
| F01 | Tasks opens on a large session | List, grouped by Epic. The focus epic and active epics are expanded, done epics are collapsed under "Done epics (1)", and done tasks are folded per epic. Failed tasks are visible. The first render is at most 50 ms. |
| F02 | Epic header | It shows the title, status word, `N of M done · K failed`, the agents and the age. Progress reads correctly with colour removed (forced colours). Enter, Left and Right expand and collapse. The screen reader reads the level, the expanded state and the counts. |
| F03 | Status derivation | Fixtures for planned (an empty epic, and one with only queued and ready tasks), active (one working; one waiting on a teammate), blocked (all unfinished tasks blocked) and done with one failure show the specified words. A client never computes them itself; the DTO supplies them. |
| F04 | Search and filters | Typing narrows to the matching tasks with the count `x of 480`. Needs attention shows exactly the blocked, failed and review tasks. A selected task that is filtered out keeps its detail and offers Show it. Clear filters restores everything. No request is sent while typing. |
| F05 | Finishing work | A task push to done moves it into the fold, updates the header counts, announces once and does not scroll the list. A selected finishing task stays selected and shown. |
| F06 | DAG by epic | It draws the focus epic. A cross-epic dependency appears as a labelled, subdued context node. All tasks above 150 shows the notice. A status filter subdues nodes and keeps edges. |
| F07 | Docs tree and rendering | The tree lists every doc. Changed files show markers with the member ids. A document renders headings, lists, tables, code and images. Raw HTML shows as text, `javascript:` links stay text, an oversized image shows the size notice, and no CSP violation is logged. |
| F08 | Live document update | While a doc is open, a member's worktree change updates it in place. Scroll stays anchored at the top heading, and the notice names the member. Show changes marks the changed blocks with text. A burst of 20 saves gives one update per second. |
| F09 | Workspace and member versions | View switches between Workspace and the member's version. Compare with Workspace shows a diff in the existing Diff component. |
| F10 | History | The timeline lists the recorded changes and those seen since the page opened. Picking two entries shows the diff between them. |
| F11 | Links into Docs | Task text, message bodies and `file` artifacts containing `notes/x.md` or `notes/x.md §Heading` link to the document at that heading. Unknown paths are not links. Referenced by lists the linking tasks and messages, with its scope stated. |
| F12 | Usage figures | Total, Hit rate, Write share, Estimate and Cost per done task match a hand computation from the fixture under the §Definitions formulas. The model without a price shows No price, and the total states the exclusion. A non-reporting agent shows not reported, not 0. |
| F13 | Trend and breakdown | The trend draws buckets from the samples, with the latest values as text. Show as table gives the same figures. The breakdown sorts by every column, and the narrow layout stacks without page overflow. |
| F14 | Big writes and history | Big writes lists exactly the fixture's qualifying requests, and opens the agent at that time. History lists both sessions, and selecting the earlier one scopes the figures read-only. |
| F15 | Aside ask | `/btw` switches the composer to aside mode with the copy line and the `--info` border. The recipient picker lists unavailable members with reasons. Escape keeps the text. Sending opens the Asides tab. |
| F16 | Aside answers | Answers arrive independently; the slow one shows Answering and the failed one shows its reason. The footer names the source copy and time, and tokens and estimate are shown. Cancel stops pending answers. |
| F17 | Aside isolation | During and after an aside: no bus row, inbox entry, transcript line, task change or briefing change. Only `aside.*` events without bodies appear. The fixture's live sessions receive no input. Send as message opens the normal composer prefilled, and only sending it reaches the member. |
| F18 | Keyboard and screen reader | Every new control is reachable by Tab. The `n`, `b` and `h` keys work and are listed in Help. Tree and treegrid semantics hold, the announcements are polite and bounded, and focus is never stolen. |
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

Slices 2 and 6–7 need only `epics-model`. Slice 3 needs
`knowledge-base-design`, slice 4 `usage-metrics-design`, and slice 5
`btw-mechanism`.

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
  - The four token categories, with "not reported" distinct from zero.
  - A versioned, dated price table, and the big-write classification.
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
