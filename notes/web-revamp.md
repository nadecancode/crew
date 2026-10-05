# Crew dashboard revamp

Design for the existing `crew web` application in `web/`. This is an operator workspace: understand what the team is doing, locate the work that needs attention, inspect the evidence, and act without losing context. The public website is outside this change.

The direction is a quiet, legible operational interface. Use a warm light canvas or an ink dark canvas, clear typography, thin divisions, and one restrained action colour. Keep the live graph and the useful density of a developer tool. Spend space on message bodies, current work, relationships, and actionable state rather than decoration.

This document replaces the visual/layout recommendations in [web-design.md §ui](web-design.md#ui) and the node appearance, interaction presentation, and usage-strip layout paragraphs of [§graphs](web-design.md#graphs), while retaining that document's security, transport, keyboard, and performance contracts and [web-multi.md](web-multi.md). The implementation must not require new daemon data or a protocol revision. The delivery tiers in §budget govern scope: T0 is required; T1 and T2 descriptions are conditional and are not acceptance requirements unless a measured allowance is assigned before work begins. Tiers take precedence over the complete future interaction descriptions in §views/§content. Any new §views behaviour not explicitly named in T0 is optional under the T1 rule: it requires a measured delta within the assigned slice allowance before acceptance scope expands.

## Reference material

- [Visual gallery](web-revamp/index.html): current screenshots and proposed screens, grouped by surface.
- [Light team reference](web-revamp/mockups/team-light-desktop.png), [dark team reference](web-revamp/mockups/team-dark-desktop.png), [narrow team reference](web-revamp/mockups/team-light-narrow.png).
- [Attention state](web-revamp/mockups/attention-light-desktop.png), [task DAG](web-revamp/mockups/dag-dark-desktop.png), [700px panel replacement](web-revamp/mockups/replacement-light-narrow.png), [1024px split layout](web-revamp/mockups/inspector-light-compact.png).
- [Inspector](web-revamp/mockups/inspector-dark-desktop.png), [tasks and task detail](web-revamp/mockups/tasks-light-desktop.png), [events](web-revamp/mockups/events-light-desktop.png), [usage](web-revamp/mockups/usage-dark-desktop.png), [question](web-revamp/mockups/question-light-desktop.png).
- [Editable mockup](web-revamp/mockups/index.html): `?scene=team|inspector|tasks|dag|events|usage|question|attention|replacement&theme=light|dark`. Static, authored sample content; buttons are visual references. These files never ship in `web-dist`.

Mockups show hierarchy, surfaces, and spacing, not new wire values. Wording is illustrative; the implementation shows the actual objective, task titles, IDs, model, and diagnostics. The mockup's sandbox is assumed `required`; the existing warning must appear when it is not. Keep the visible subagent and runtime distinctions required below. References mix delivery tiers: the DAG/attention/panel layouts are T0; usage aggregate/attention count/timestamps are T1; List modes and bus filter/follow controls are T2. See §budget before treating a visible optional control as implementation scope.

<a id="audit"></a>
## §audit — current application

### Method and evidence

Audited the committed production bundle, manifest source `fnv1a64:c5dd2a33534c6d7e`, through `web/dev/mock.ts`, using the generated `subagents` fixture plus a second `team10` workspace and one unjoined session. Opened the app with `bun run mock --port 47731 --fixture subagents`; reviewed the main views and interactions in Chromium (Brave on this NixOS host). Then captured the same states through Playwright using the real mock and pairing flow.

[capture-baseline.mjs](web-revamp/capture-baseline.mjs) produces 60 screenshots: 15 surfaces at 1440×960 and 390×844, each with dark and light OS preference. Surfaces: agents/shell/bus, task DAG, task detail, events, event detail, usage, inspector overview, terminal, transcript, diff, message detail, workspace switcher, help, confirmation, context menu. Each file's dimensions, body colour, document width, and panel width are recorded in [baseline/manifest.json](web-revamp/baseline/manifest.json). Screenshots intentionally retain existing clipping; no CSS was changed for the audit. Light preference produces the same dark app, because no light palette exists.

Source checks covered `protocol.ts`, generated wire fixtures, route/keyboard handling, list anchoring, theme, operation dialogs, and the multi-workspace lifecycle. This was a design audit, not a new performance benchmark; existing budgets remain gates. No product source was changed.

### Ranked findings

| Priority | Finding and evidence | Consequence | Design response / owner |
|---|---|---|---|
| P1 | At 390px the document is 907px wide; `body { min-width: 900px }`. Navigation and actions fall outside the viewport; the 380px bus overlays the main area. [Narrow baseline](web-revamp/baseline/agents-dark-narrow.png). | Basic reading, switching, and sending are unavailable without horizontal scrolling. | Remove page minimum; usable shell from 640px and explicit panel opening (T0). Full sub-640px layout and Graph/List alternatives are T2. `ui`, `graph`. |
| P1 | All workspace context, view navigation, help, stop-team, and connection state compete in one 34px row. The objective truncates before any useful summary emerges. [Desktop baseline](web-revamp/baseline/agents-dark-desktop.png). | The operator has to decode small labels before seeing the work. A destructive action has disproportionate prominence. | Separate global controls, objective/summary, and view tabs. Put stop-team in a labelled workspace actions menu; preserve shortcut/confirmation. `ui`. |
| P1 | Bus rows are 22px. Time, both endpoints and type consume most of the 460px panel; message bodies reduce to a few words. [Bus baseline](web-revamp/baseline/agents-dark-desktop.png). | The page has content but hides its meaning; almost every message requires opening another panel. | Fixed 84px rows: compact metadata and two readable body lines; full detail remains one activation away. `content`. |
| P1 | `--dim: #4e525e` on `--bg: #15161b` measures 2.32:1. Used for timestamps, empty text, and absent data. Muted text is about 4.68:1 on the canvas but lower on lighter surfaces. | Meaningful secondary text is hard to read, especially at 11–12px. | All meaningful text tokens meet 4.5:1 on their actual surfaces; separate faint decorative lines from text. `ui`, all views. |
| P1 | Inspector presents eight or nine operations before a long facts list. Current task, problem, and mailbox are below process/backend details. [Inspector baseline](web-revamp/baseline/inspector-dark-desktop.png). | High-value work is harder to find than low-frequency controls. | Primary Message/Pause; More menu; current work, session, problems, then collapsible process details. Retain every action and fact. `graph`, `ui`. |
| P1 | Light OS preference is ignored. [Light-preference baseline](web-revamp/baseline/agents-light-desktop.png). | No readable light working environment. | Matched light/dark semantic tokens; default follows system; explicit override. `ui`. |
| P2 | Small graph nodes sit in a large empty canvas with no local title, legend, or visible zoom/fit actions. A right click, double click, or footer hint reveals capabilities. | Rich graph behaviour is easy to miss; fit-to-window can shrink text further. | View toolbar, Graph/List toggle, zoom/fit controls, runtime/status legend, and visible More action through inspector. `graph`. |
| P2 | Task DAG communicates dependencies but offers no sortable/readable list when the graph becomes large or the window is narrow. [Tasks baseline](web-revamp/baseline/tasks-dark-desktop.png). | Finding an assignee, blocked task, or long title requires panning. | Graph/List presentation of the same task set; compact local filters; no new route. `graph`. |
| P2 | Usage has useful figures but little summary, and unknown data is dim text inside empty sections. All protocol fixture snapshots currently have no quotas; context is absent in the baseline. [Usage baseline](web-revamp/baseline/usage-dark-desktop.png). | Unknown reporting can resemble zero usage. Important quota and fallback states are hard to exercise in development. | Totals with reporting coverage; explicit no-data explanations; realistic mock scenarios. `content`, `qa`. |
| P2 | Event rows have six fixed metadata columns and 22px height. Many values are repeated; long messages truncate. [Events baseline](web-revamp/baseline/events-dark-desktop.png). | Troubleshooting details get less space than their labels. | 36px desktop rows and 64px narrow rows; combined source/type cell, full event detail, clear filters. `content`. |
| P2 | Switcher minimum width is 680px; its path and state columns clip at 390px. [Switcher baseline](web-revamp/baseline/switcher-light-narrow.png). | The workspace identity and availability cannot be assessed on a narrow window. | 560px desktop popup; viewport-bounded full-width narrow sheet with two-line rows. `ui`. |
| P2 | Side-panel separator has `tabindex="-1"` despite an arrow-key resize handler. Many default buttons are around 24px tall; context menu is primarily discovered by pointer. | Useful operations are not consistently reachable by keyboard/touch. | Focusable separator with value and keyboard instructions; visible More; Shift+F10; 32px desktop and 44px touch targets. `ui`, `graph`. |
| P2 | Fetch feedback often occupies only one line (`loading…`, `reading the diff…`), with errors sometimes appearing in the global status area. | A slow request can look like an empty result or detach the error from its action. | Stable-height local loading region, retained prior data with freshness label, local Retry where safe; no artificial delay. `graph`, `content`. |

### Keep what already works

The existing interface is functional and unusually rich. Preserve workspace deep links and back/forward, the radial team topology, explicit task dependencies, visible runtime identity, selected-agent keys, questions, plain-text evidence, diff colours, streamed terminal, connection proof, no polling, fixed-height virtual lists, and stable selection while data arrives. The redesign is not permission to simplify away these capabilities.

<a id="system"></a>
## §system — visual and component contract

### Type and layout

Use system fonts only. UI: `system-ui, -apple-system, "Segoe UI", Roboto, Ubuntu, Cantarell, "Noto Sans", sans-serif`. Evidence/IDs: `ui-monospace, SFMono-Regular, "JetBrains Mono", Menlo, Consolas, "DejaVu Sans Mono", monospace`. A named local font is a fallback, not a font load. No `@import`, remote font, icon font, image host, or new runtime library.

| Token | Value | Use |
|---|---|---|
| `--font-xs` | 12px / 1.5 | Metadata, table headers, secondary labels |
| `--font-sm` | 13px / 1.5 | Dense rows, graph card text, code |
| `--font-base` | 14px / 1.5 | Controls, standard reading text |
| `--font-lg` | 16px / 1.4 | Panel title |
| `--font-title` | 20px / 1.3 | Objective and detail title |
| `--font-metric` | 24px / 1.2 | Small number of summary figures |
| `--weight-normal`, `--weight-medium`, `--weight-semibold` | 400, 500, 600 | Body, controls, titles |
| `--space-1` through `--space-8` | 4, 8, 12, 16, 20, 24, 32, 40px | Shared rhythm; 24px desktop gutters, 16px narrow gutters |
| `--radius-sm`, `--radius`, `--radius-lg` | 4, 6, 10px | Tags, controls, nodes/dialogs |
| `--control-h`, `--touch-h` | 32, 44px | Minimum target height; use 44px at coarse pointer/narrow width |
| `--panel-default`, `--panel-min` | 390, 320px | Resizable panel; clamp to available workspace |

IDs and machine values use monospace; descriptions and message bodies use the UI font. Terminal retains its configured monospace font. Numeric columns use `font-variant-numeric: tabular-nums`. Normal text is never smaller than 12px; keyboard chips may use 11px. Primary copy never depends on a hover title to become readable. In dense fixed rows, intentional truncation must have a reachable full-detail surface.

### Colour

Names describe purpose. `theme.css` defines the values and compatibility aliases for existing selectors. A colour must not encode status on its own: pair it with a word or distinct glyph. Runtime colour identifies runtime, not task health.

| Semantic token | Light | Dark |
|---|---|---|
| `--canvas` | `#f5f6f3` | `#101716` |
| `--surface` | `#ffffff` | `#17201e` |
| `--surface-raised` | `#eef1ed` | `#1e2a26` |
| `--text` | `#182321` | `#e5eeea` |
| `--muted` | `#53635d` | `#a7b8b0` |
| `--quiet` | `#596b62` | `#9aafa4` |
| `--line` | `#d9e1dc` | `#2c3c35` |
| `--control-border` | `#75867d` | `#79978a` |
| `--action` | `#116e59` | `#71d5ae` |
| `--action-surface` | `#e7f3ec` | `#203a2f` |
| `--info` / `--focus` | `#315ebe` | `#8cb7f2` |
| `--ok` | `#216e4d` | `#7ccea3` |
| `--warn` | `#8a570b` | `#e5bf6b` |
| `--warn-surface` | `#fff4df` | `#332b1b` |
| `--err` | `#b23b3b` | `#f39595` |
| `--err-surface` | `#faeaea` | `#382424` |
| `--accent` (Claude) | `#9a491e` | `#e8ac82` |
| `--codex` | `#136b66` | `#70cbbb` |
| `--pi` | `#82620c` | `#debd6f` |
| `--shell` | `#6954ab` | `#bda5ec` |
| `--p2p` | `#ab3e68` | `#f0a0bd` |
| `--token-input` | `#315ebe` | `#8cb7f2` |
| `--token-output` | `#116e59` | `#71d5ae` |
| `--token-cache-read` | `#6954ab` | `#bda5ec` |
| `--token-cache-write` | `#8a570b` | `#e5bf6b` |

Aliases: `--bg = --canvas`, `--panel/--node = --surface`, `--bar-bg/--node-hi = --surface-raised`, `--orch = --info`, `--dim = --quiet`, `--faint = --line`, `--gauge = --action`, `--gauge-track = --line`. Keep `--p2p` pink, distinct from the shell runtime. Usage categories use the four `--token-*` names exclusively; a labelled usage legend defines those hues within the chart, independently of runtime identity. `--bg-term` can remain near-black in both modes; its text and xterm theme must form their own matching palette.

Measured examples: light muted on white 6.34:1; dark muted on surface 8.03:1. Across canvas, surface, raised, action, warning and error surfaces, the minimum quiet-text contrast is 4.87:1 light / 5.29:1 dark; the minimum control-border contrast is 3.30:1 light / 3.87:1 dark. Action on `--surface` is 6.18:1 light / 9.38:1 dark; on `--action-surface` it is 5.42:1 light / 6.93:1 dark. [contrast.mjs](web-revamp/contrast.mjs) checks these six surfaces. Use the strong border for controls whose boundary is needed; `--line` is a decorative divider. Test composed warning, selection, disabled, and chart states separately. Do not apply container opacity to meaningful read-only text until contrast is lost; show a stale tint and status banner while keeping text readable.

Default `theme=system`; Light/Dark/System choices are in the visible appearance control. Persist only the choice, as another validated dashboard `web_prefs` field on the index connection. The existing endpoint stores an opaque JSON preference object; this is a frontend schema extension, not new daemon data. Do not use browser storage as a second preference authority. A system media change updates the palette only in System mode. Initial system palette comes from CSS before preference load. An explicit preference differing from the system may produce one palette change while `web_prefs` arrives; this small loopback-load flash is accepted. Do not add browser storage to eliminate it.

### Surfaces, focus, and motion

Keep the shell and lists flat. Graph nodes, the selected work block, and dialogs can have frames. Avoid a card around every label. Popovers/dialogs use one elevation token: `0 12px 32px #10251a18` in light, `0 12px 32px #0006` in dark; no glass blur. Focus: 2px `--focus` ring, 2px offset; it must remain visible on selected nodes, menus, fields, and tabs.

Motion: 100ms for colour/press feedback, 160ms for a popover, 220ms for an inspector entering; easing `cubic-bezier(.2,.8,.2,1)`. Exit can use 120ms. Animate opacity/transform only. There is no entry stagger, number count-up, continuously pulsing Live dot, idle shimmer, or graph-wide ambient animation. Existing event-driven graph pulse/expiry timing remains governed by `anim.json`; do not change its semantics just to fit these chrome durations. `prefers-reduced-motion: reduce` removes spatial motion and preserves static highlights and expiry. The page must request no animation frames or sub-second timers while idle.

Icons: small inline SVG paths in one `Icon` component, 16px with 1.75px stroke (20px when needed), `currentColor`, consistent round joins. T0 adds at most six shared icons actually used: chevron, close, more, plus, minus, fit. Reuse native text and existing glyphs elsewhere. Search, expand, copy, warning, terminal and sun/moon SVGs are T2 and are not shipped pre-emptively. Keep TUI runtime glyphs (`✱ ❂ π $`) and the orchestrator diamond. Decorative icons are hidden from assistive technology; icon buttons have concrete names. No emoji or imported icon package.

### Shared primitives

`ui` creates only primitives used by the required T0 slices (unused exports remain unshipped): `Icon`, `StatusBadge`, `EmptyState`, `ViewHeader`, and shared button/control/section classes. `PanelHead` remains the panel header export in `SidePanel.tsx`. These accept text, children, semantic tone, and optional actions; they do not fetch or inspect the app store. Use native buttons, labels, inputs, selects, details/summary, and existing Solid control flow. Avoid a generic component framework or a second state store.

An empty state has a short title, one sentence of cause/next step, and at most one relevant action. Do not show a retry control when retry would reload a stopped origin or invent data. Loading and error are distinct states, with `aria-busy`/local status feedback. Network errors preserve drafts and evidence already loaded.

<a id="views"></a>
## §views — layouts and interactions

### Shell and status bar

Desktop hierarchy: 56px global header; approximately 94px objective/summary row on tall Team windows; 46px four-view navigation; flexible main/panel region; composer and concise focus hints. These are targets, not rigid heights for wrapping content. At viewport height below 800px, or outside Team, the objective/summary becomes a single 40px row; put secondary path/elapsed details in its existing title/disclosure affordance and keep current task/question counts readable. When warnings, usage and question strips coexist, the row also collapses to 40px regardless of height. The main region is never squeezed by a mandatory 94px summary. Do not fix a minimum document width or minimum viewport height that cuts off controls.

Global header: crew identity, workspace switcher, index connection state, Commands (`Ctrl K`), appearance, Help, Workspace actions. Workspace actions contains Stop team with the existing confirmation and `S` shortcut. The connection text distinguishes dashboard availability from workspace availability. It must never say the team is live merely because the index is live.

Objective/summary: the actual objective (up to two lines with a reachable expand control), workspace path, elapsed time, VCS; four concise counts: total agents, working agents, completed/total tasks, open operator questions. Counts are derived from the current store. An optional T1 attention summary can appear when a blocked/failed/paused agent or operator question exists; count unique affected agents and state the separate open-question count. T0 relies on the existing node states, usage strip, and question strip rather than introducing an attention drawer. Questions count `store.openQuestions()`, not every historical message of type `question`. Do not invent overall task progress by averaging unrelated percentages. Preserve completion summary and completion time when present.

Navigation labels: Team, Tasks, Events, Usage. Internal routes remain `graph`, `tasks`, `logs`, `usage` and the existing fragments. `t`, `l`, `v` retain their toggle behaviour. Events is a visible copy change only. A Message bus control opens the bus when it is hidden or a detail is selected; Message opens the existing composer. Commands opens the existing command line with its command suggestions; no separate command interpreter.

The existing UsageStrip remains visible on every view whenever quota data or agents paused at a limit exist. Place it below navigation and warnings, above the question strip. Keep runtime labels, quota bars/percent/reset/stale state, and the paused-limit row with Continue now and Restart on current login; retain `u`/`U`, confirmation, `.usage-strip`, and `data-region="usage"`. Reflow it rather than moving it into Usage-only content. Warnings and questions retain dedicated full-width strips with their current semantics. A sandbox/socket/VCS warning is never hidden inside a menu. A question strip names the asker, previews the request, counts additional questions, and exposes Answer. Existing auto-open behaviour is retained: wait until no dialog, text field, or terminal owns the keyboard; open an unseen question once; Later leaves a visible reminder. No auto-focus takeover while typing.

Responsive behaviour:

| Width | Main content | Panel | Chrome |
|---|---|---|---|
| ≥768px | Flexible main, persistent bus or selected detail | Default 390px, min 320px, max remaining width minus 420px; keyboard/pointer resize | Full objective and summary; four views always visible |
| 640–767px | Main fills width | Hidden until selection or Message bus; replaces the main content region at full width, Close restores prior main | Workspace path may truncate; actions can move into labelled More menu |
| <640px (T2) | Team/Tasks default to List if that tier ships, Graph remains selectable | Full-width reading surface below navigation; visible Back/Close | 16px gutters; four tabs fit; objective wraps; counts wrap; composer recipient above text when needed; 44px touch targets |

The split threshold is derived from 420px main + 320px panel + 1px separator, rounded up to 768px for gutters and resize tolerance. A 1024×768 window retains graph and inspector side by side. Below 768px the panel is a replacement region, not a floating modal. Main is hidden/inert while the panel is shown; navigation remains available. Focus moves to panel heading/first relevant control on opening and returns to its trigger on close. No modal role or focus trap is needed for this replacement. Changing view closes the narrow detail region. A real dialog retains its existing modal trap. At 200% zoom the same layout rules apply. Even if T2 is deferred, remove the 900px body minimum: the shell must not force document overflow at 390px; graph/code may pan internally, and the selected panel fits the viewport. Full sub-640px information layout and 44px touch targets are conditional T2, not a promise of phone support for a loopback-only app. Text-bearing panes wrap; code, diff, terminal, and an explicitly selected graph may scroll internally without making the document wider.

The resizer becomes a Tab stop with `role=separator`, current/min/max values, and Arrow keys (Shift for larger steps). Clamp preferences when the viewport changes. A user who never uses single-key shortcuts can reach all features with Tab and buttons.

### Team graph and list

Keep the orchestrator at the centre, members in TUI order, satellites visibly tethered to their parent, and user-dragged positions scoped to the workspace. Neutral nodes use status labels and small runtime accents; selected/focused nodes have a strong outline. Do not give every completed node a bright green frame.

Target world dimensions: member 244×132, hub 260×132, satellite 200×80; the extra row preserves the model label rather than removing it. Adjust gaps and fit bounds with these dimensions. Three information levels: ID/runtime and status; task title or role; activity and numeric progress/context. A context meter is labelled as context; a progress meter is labelled as progress. Keep the current model label as a separate bottom-row button on each main node and hub; clicking it opens Model and CLI. Full fallback, role, mailbox, and timings remain in hover/inspector. Working, paused, blocked, failed, waiting, done, and stopped remain visually distinct by text/glyph. Keep question, unread inbox, limit, compact-pending, temporary, and subagent indicators accessible; do not silently drop them for aesthetics.

The T0 view toolbar contains agent count and visible Zoom out / Zoom in / Fit controls. Graph/List is conditional T2; do not reserve dead controls for an unshipped mode. Show the current zoom and a concise legend for connection types. At fit scales that make labels unreadable, suggest Zoom in (or List if T2 ships) rather than pretending the entire graph is readable. Do not aggressively re-fit when a push arrives or override a manual pan/zoom.

Conditional T2 List mode is the same agents and selection, with status, work, runtime/model, progress/context, and last activity in responsive rows. Group satellites beneath parents; maintain TUI navigation order. At <640px select List initially, but preserve a user's in-document choice during resizing. Switching modes keeps selected agent; only one presentation is mounted so roving tabindex and `data-agent` identity remain unique.

Interactions stay: arrows/Home/End select; Enter inspects; double click opens terminal; pointer drag moves; context menu opens from right click, Shift+F10, and a visible More control. Graph edges filter the bus and light the matching connection; choosing a pair must open the bus on narrow layouts. Hover content must also be obtainable from focus/inspector. Edge animation remains event-driven, bounded, and semantically identical to the fixture contract.

Empty: “No agents yet” / “Agents appear here when the team starts.” Use the actual workspace availability notice above it. Do not provide a fake web Start action. With no orchestrator, preserve the scheduler hub. Removed agent selection keeps an explanation and Back to team action.

### Existing graph and shell features retained in T0

This is a presentation change, not deletion of §graphs behaviour. Preserve these selectors/actions unless a reviewed selector migration lists the exact replacement:

| Existing feature | Place and behaviour after revamp |
|---|---|
| Node model label, `.node .model` | Fourth node row and hub footer; click opens the same model dialog; keyboard equivalent remains `s`/`O` and Inspector Model |
| Node task title / `data-task-link` | Readable task-title row; click opens that task card; hover task summary remains |
| `?` / `.badge.ask` | Header badge, never only a passive status; click opens the corresponding operator question |
| Unread inbox, `⑂N`, temporary, paused, limit, compact, pending badge | Compact header/footer marks; in-session subagent count remains distinct from visible satellite agents; pending means RPC in flight |
| Provider error/refusal | `Agent.block_kind=provider_error|refused` with blocked status uses the existing `providerError`/`statusTail` text and error tone; stale activity must not hide it |
| Mark chips / `data-mark`, `.mark`, `.sweep` | Immediately below the related node; existing click copies a commit when supplied and opens the VCS event; bounded event-driven animation unchanged |
| Message pulse / `.pulse.msg` | Same hit target: click opens that message; hover previews its text |
| VCS link / `.link.vcs` and its pulse | Existing outcome/held appearance; hover shows VCS data; click filters Events to VCS and opens the event when seq exists |
| Other live connection links | Click filters the bus to the pair, opening the bus region if needed; hover preview remains |
| `.p2p-banner` | Small graph-local “p2p channel OPEN · agents talking directly” notice while existing `p2pLive` is true; pink p2p token; no new timer |
| `.usage-strip` and paused-limit controls | Full-width shell strip on every view, above questions; runtime quota bars, Continue/Restart and `u`/`U` intact |
| `.toasts` / `.toast` | Viewport-bounded notice stack above composer; error/dismiss behaviour, cross-workspace question notice and Switch action retained. No announcement for every bus row |

### Task DAG and list

T0 retains the DAG and its current operations; the new List/search/filter presentation is conditional T2. Preserve dependency layering and lineage hover; add Graph/List toolbar, status/assignee filters, task count, and numeric completed/total summary. A filter in graph mode must not silently erase dependency edges and suggest independence: either keep context nodes subdued or switch to a clearly labelled filtered List. List mode shows title/ID, status, assignee, progress, reviewer when in review, and dependency summary. Prioritize working/blocked/review in an optional status grouping; default dependency order remains understandable. Search is local to the loaded task set.

Use the same roving keyboard rules as the existing DAG. List uses arrows/Home/End/Enter without intercepting `k` as Kill. If a dependency is missing from the snapshot, show its ID as unresolved; never draw it as completed. Cycles or layout fallbacks remain visible in List. Empty and no-filter-results have different copy and a Clear filters action only for the latter.

Task detail order: human title and status; assignee/reviewer; progress and note; description; dependency statuses; result; artifacts; timestamps/timeout. `created_at`, `started_at`, and `completed_at` become visible with absolute time available. Artifact cards show kind, full value/copy, description, producer, and creation time when supplied. A commit value is copied, not assumed to have a GitHub URL. File artifacts are plain text/copy; never request the file from an arbitrary origin. Results and descriptions remain plain text with safe URL linking.

While `task_get` enriches a locally known task, show its existing content immediately with local loading feedback. Failure to fetch dependencies is a local error with Retry while live; it must not blank the title or substitute another task's response. If missing entirely: “Task unavailable” with the ID and returned explanation. Not-running/stopped preserves loaded detail and disables wire actions.

### Message bus and message detail

Bus is the default desktop side panel. Header names it “Message bus”, shows number currently loaded, and offers Load older. Rows are fixed 84px: 12px endpoint/type/time metadata plus two 13px body lines. Use a full-width row selection affordance and a clear selected state. A body preview, not an internal message ID, is the dominant content. Preserve chronological order, newest at the bottom.

T0 keeps the existing pair chip and Clear. Conditional T2 filters add local text search and message type (including Questions/Artifacts). A pair is explicit in the header; do not reinterpret peer questions as operator questions. Searches only cover loaded history, with that scope visible. Do not fetch on each keystroke. “No messages yet” and “No messages match these filters” differ. Clear is offered for filtered emptiness. Load older shows “Loading…” in place, stays disabled while in flight, and reports failure locally.

Existing tail-follow and scroll anchoring stay in T0. The explicit following control and arrival counter below are conditional T2. Following latest is visible. Scrolling away suspends following; a “N new messages · Jump to latest” button resumes it without stealing focus. N counts arrivals since following stopped, not total history. Selection and scroll anchor remain stable when older rows arrive or a full ring drops rows. `VirtualList` exposes a small following/jump API; no second virtualizer.

Message detail: sender/recipient, type, time, full body; Message sender action using the existing composer; parent message and locally available replies; optional correlation ID/metadata in a collapsed plain-text diagnostic section. Do not imply that local replies are the entire conversation. Inbox delivery/read state is shown only where `InboxEntry` supplies it; the bus alone cannot infer it. Pending send preserves the draft; success and failure use the existing operation feedback. Do not label a new message as a threaded reply unless its `reply_to` is actually sent. This slice does not extend the send payload.

### Events and event detail

Rename visible “logs” to “Events” while keeping route/key. Place the title and loaded count above a labelled toolbar: search, agent, minimum level, Version control, Clear, Load older. Explain that search covers loaded events. Preserve `f` for the selected-agent filter.

Desktop rows target 36px, with time, severity label, compact source/type, agent, and the largest column for the message. At narrow width use a fixed 64px two-line row; remount or make `VirtualList` row height reactive at the breakpoint, preserving the selected event and scroll anchor. Do not only change CSS height while its math still uses 22px. Full text is one Enter/click away. Error/warn use a small tone marker and subtle background, not an entire sentence of saturated red.

Event detail leads with the full message and level/type/time, then related agent/task. Version-control data is grouped: action/outcome, operation/backend, commit/change/branch, affected files, summary. Conditional T2: `Event.data` for non-VCS events gets a bounded, escaped JSON/text disclosure (at most 16,000 displayed characters, with truncation identified). Never render arbitrary metadata as HTML. Missing/evicted event: “This event is no longer in the page's history” with return to Events; no invented event-get RPC.

If the T2 explicit-follow controls ship, expose Follow latest / Jump to latest consistently with Bus. New rows never pull the reader off an older selected event. Clear filters is local and remains usable when stopped; Load older and Retry require a live workspace.

### Usage

Conditional T1 top line gives three useful aggregates from reported data: total tokens, turns, agents paused at a limit. Total includes input/output/cache read/cache creation; label it “Total reported tokens”, not cost or billable tokens. If reporting is incomplete, show “N of M agents reporting”; absent usage is unknown, not zero. Explain the four categories with labelled colour swatches and numeric breakdown on focus or detail.

Usage windows appear when quota data exists, grouped by runtime/window with percent, reset time, age, and explicit Stale label. Staleness uses the existing `STALE_MS`; it is not a new watchdog. High usage and active limits show text as well as colour. Continue / Restart on current login preserve current actions and confirmation semantics. Expose `limit.retry_at`, `resets_at`, and attempts when present; do not claim a retry will happen when the protocol does not say so.

Agent rows expose context reported percent/window, token stack and total, turns, compactions, and link to inspector. Runtime series are consistently coloured; T2 select/focus can isolate one agent. Context history is explicitly “Since this page opened”; no claimed daily/monthly trend or historical spend. No samples: explain that the runtime has not reported context. One sample: show a point/current value, not a fabricated trajectory. Offer textual latest values beside the chart so hover and colour are not the only access.

At narrow width stack per-agent facts or use a deliberately labelled internal table scroller; the page itself does not overflow. Prefer readable rows to squeezing five columns into 390px. No graph/chart package.

### Agent inspector, terminal, transcript, diff

Header: runtime glyph, agent ID, role, explicit status and model; expand/dock and Close. Primary actions: Message, Pause/Resume. More: Prompt, Restart, Restart fresh, Compact, Model and CLI, Jump to pane when applicable, Kill. All remain reachable without shortcuts. Keep pending/disabled semantics and existing confirmations. Refresh is a visible control for fetched tabs, and `r` retains its inspector meaning.

Overview order: Current work (title/status/progress/note/dependencies), Session (runtime/model/fallback/context/usage/branch/start/last activity/mailbox), Problems (agent error and recent relevant events), Messages, other assigned/review tasks, Process and delivery details. Advanced details retain PID/exit, session ID, backend/reference, workdir, delivery, parent/subagents, temporary, restarts and compactions. Show fallback from/to, reason, policy flag and since time where present; show limit kind/message/retry/reset/attempts. Never merge process failure and usage pause into the same “offline” status.

Tabs remain Overview, Terminal, Transcript, Diff, with the current roving tablist and saved scroll positions. Panel identity is keyed to the agent; old inbox/transcript/diff responses must never appear under the next agent or workspace. Local loading rows reserve space. Read failure is distinct from “no messages”, “no transcript”, or “no changes”; retain prior evidence and expose Retry only when live.

Terminal has a visible attachment/focus status and “Ctrl+] returns to dashboard” hint. Use a dark terminal surface even in light appearance, bounded by a matching toolbar. The xterm import remains requested at document start. Read-only/ended/disconnected/unavailable states explain what is possible and never accept terminal input. Herdr stays a screen capture with Jump to pane and event-driven Refresh, not a pretend live PTY. Preserve SGR stripping and no clipboard addon.

Transcript retains bounded virtualization, kinds/timestamps, and selection detail. Explicitly distinguish unavailable, empty, truncated, and failed. Diff keeps file/hunk/add/remove visual structure and plain text; show “No uncommitted changes” only for a successful empty diff. Long code scrolls inside the pane, and copying still works while stopped.

### Switcher, dialogs, and context menu

Switcher: current workspace name always visible. Desktop popup max 560px; narrow width `min(100vw - 24px, 560px)`, positioned within the viewport. Each row gives name/path, explicit state, tasks done/total, working agents, attention and question counts. Counts are summaries, not permission to fetch off-screen workspace detail. Dim stopped rows without hiding their text. Show the unjoined group with the existing instruction to run `crew web` there; no Join action unsupported by the protocol. Keep typing filter, arrows, Enter, 1–9, Escape, `w`, and focus return. Names can repeat: path is the disambiguator.

Dialogs: 480–560px maximum, 12px narrow inset, bounded height with scrollable content and sticky action footer. Title names the operation and target. Forms have visible labels and inline errors. Destructive confirmation names the scope and consequence; focus starts on Cancel. Preserve a typed answer/model choice after recoverable failure. Suggested-answer buttons and keys 1–6 send that answer immediately, as today; label them as quick answers. Enter sends the typed answer; Shift+Enter adds a line; Escape/Later postpones. Do not preselect or automatically submit any answer. “Later” does not answer. The model picker retains runtime, name, tier, price string, summary and unavailable explanation from `models_list`; do not calculate new prices. Help reads the same `KEYS`/command table as the dispatcher, grouped by task with the existing slash-command way to switch single-key shortcuts; a visible toggle is T1. Group using `KEYS.scope`, retaining every table entry exactly once; no second hand-maintained list.

Context menu: same action source as Inspector More, selected-agent name as heading, grouping between communication, process operations, and Kill. Include shortcut hints. Clamp to the actual viewport using rendered size; handle Home/End as well as arrows, Enter, Escape. Restore focus to the originating node/button. It must open from visible More and Shift+F10, not require right click. No disabled control should look like a working action while pending or disconnected.

### Common availability states

| State | Reading experience | Available action |
|---|---|---|
| Initial connecting/snapshot | Workspace identity if known; bounded “Connecting…” / “Loading workspace…” region; no fake empty team | Switcher only if index is live; no repeated manual fetch loop |
| Empty live collection | Specific noun, cause, and next step; no spinner | Local clear-filter or existing compose action when relevant |
| Local RPC error | Preserve known content/draft; error next to the affected region | Explicit Retry through existing RPC while live |
| Workspace reconnecting | Last known content, stale tint and plain status text | Local reading/navigation; mutations disabled; automatic existing retry |
| Workspace not-running | Distinct “not running” notice with `crew run` and workspace path | Read last known content, switch workspace; do not reload |
| Workspace incompatible/removed/none | Exact existing explanation; never another workspace's data | Switch workspace; existing CLI instructions |
| Dashboard reconnecting | Existing global read-only state and proof/retry lifecycle | Read/navigation only |
| Dashboard stopped/signed-out | Stop reason/time and existing safe new-address instructions | Read/copy cached content; no reload CTA, reconnect, new HTTP, or mutations |

<a id="content"></a>
## §content — protocol v6 inventory

The UI already renders more fields than its first screen suggests. “Underused” below means present but hard to discover; “missing” means not surfaced by the audited views. Follow generated fixture types, never infer missing values from neighbouring fields.

| Source | Current use | New presentation |
|---|---|---|
| `SessionInfo.objective`, project, workspace, team start/completion | Compact status row, truncation | Objective/summary row; completion result/time when supplied |
| Session summary, style, backend/isolation/sandbox/VCS note, version/daemon metadata | Summary/style and most environment facts are not in a coherent session surface | T2 workspace details disclosure under workspace actions; warnings stay prominent in T0 |
| `WorkspaceEntry`, `WorkspaceSummary`, `OtherSession` | Existing switcher already has counts and state | Readable two-line switcher rows; clear running/stopped/removed distinctions; no off-screen snapshots |
| Agent role/task/status/activity/progress/block_kind | Present in graph/facts | Current-work hierarchy; preserve provider-refused/provider-error and progress semantics; List is T2 |
| Agent model/running model/name/effort/fallback | Model labels and a fallback sentence | Fallback reason/since/policy in inspector; clear requested vs running model where they differ |
| Agent limit `{kind,message,since,resets_at,retry_at,attempts}` | Some pause/reset copy, inspector kind/message | Full limit explanation and timings in inspector/usage; state text near actions |
| Agent context `{tokens,window,percent,updated_at}`, compactions/pending | Gauge, usage, facts; history in page only | Clear meter label, timestamp/freshness, queued compact distinction, accessible current values |
| Agent usage categories/turns | Existing usage table and facts | Aggregate reported totals with coverage, exact breakdown; never prices |
| Agent PID/exit/restarts/session/backend/ref/workdir/branch/delivery | Long facts list | Collapsible Process and delivery section; copy machine identifiers |
| Agent parent/temporary/subagents, inbox unread/total/outbox | Existing badges/facts | Preserve graph lineage; group list; usable mailbox summary |
| `Task` title/description/status/assignee/reviewer/dependencies/progress/note/result | Most already in panel/DAG | Stronger current-work/result hierarchy and existing dependency links (T0); Task List is T2 |
| Task created/started/completed/timeout | Timeout appears; lifecycle dates absent in task panel | Timeline facts and elapsed computation only when both bounds exist |
| `Artifact` kind/value/description/agent/created_at | Kind/value/description; commit copy | Producer/date and clear copy on all machine values; no filesystem reads |
| Message from/to/type/body/time/reply_to | Bus truncation; full detail/replies exist | Readable preview, local search/type filter, reply/correlation context |
| Message correlation/metadata; `askedBy`/options | Options/asked-by in question; generic metadata hidden | T2 bounded plain diagnostic disclosure; no metadata-driven HTML or commands |
| `InboxEntry.delivered_at/acked_at` | Inspector marks | Text equivalents “Delivered” / “Read”; never invent status for regular Message |
| Event seq/time/level/source/type/agent/task/message | Table/detail already present | Message gets space, explicit filters, focusable relation links |
| `Event.data`, decoded `VcsEvent` | VCS structured detail exists; non-VCS payload absent | Grouped VCS outcome/files (T0), optional bounded raw data text (T2) |
| `GitOutcome`, commit/conflict files/detail | Operation results, diff | Preserve conflict explanation, filenames, and copy feedback near action |
| Transcript `{available,truncated,entries(kind,at,text)}` | Existing tab/detail | Stronger unavailable/truncated/empty/error differentiation |
| `OutputResult.alive`, PTY state | Terminal backend-specific behaviour | Explicit attachment/focus/ended labels; retain transport logic |
| `ModelsResult` choices/unavailable | Existing model dialog | Readable choices, current value, explanation for disabled choice |
| `SendResult.duplicate/recipients`, errors | Result decoded; current send only surfaces success/failure | Preserve acknowledgement/failure and failed draft; duplicate/recipient details need no new surface in this slice |

No daemon changes are required. Deliberately excluded: historical spend, token cost calculation, per-file browsing, build/test status, CPU/RAM charts, remote workspace management, or durable usage history. Protocol v6 does not provide those facts. Theme/density/view choices are UI state, not requests for new telemetry. Runtime mock scenario data must be derived from real v6 shapes, not written into generated Rust fixtures.

<a id="dx"></a>
## §dx — development and verification

### Local loop

Use the existing two processes: `cd web && bun run mock` and `cd web && bun run dev`; open the pairing URL printed by the mock. Vite gives hot reload; the mock keeps its documented Enter-to-regenerate pairing code. To validate production CSP/network behaviour, `bun run build` and use the mock's bundle URL. Development HMR is not evidence that production is allowed to request code after startup.

`qa` adds named scenarios on top of `startMock`, preserving `--fixture`: `baseline`, `busy`, `attention`, `empty`, `long-text`, `limits`, `disconnected`, `multi`. Scenarios live under `web/dev/`, not in generated wire fixtures. Include task statuses queued/ready/working/waiting/blocked/review/done/failed; agent fallback/paused/limit/context/compact states; artifacts; timestamps; replies; long Unicode text; no quotas vs stale/fresh quotas; delayed and failed panel reads. Programmatic scenarios change state through the same controller/pushes as tests. Their timed activity must be opt-in so still captures are reproducible.

The gallery is a developer artifact, never an authenticated product route. `web/dev/gallery.html` (Vite development only) or a generated report can expose `#/gallery/<scenario>/<surface>/<theme>/<viewport>` for every captured surface and state. Each item shows its fixture/scenario, target application route, dimensions, image, and reproduction command. No gallery code enters `web/src` or `web-dist`; it holds no pairing tokens, session storage, or real operator content. This document's standalone [gallery](web-revamp/index.html) is the initial example. Capture real components against the mock, not a divergent component demo implementation.

### Component and state conventions

Keep `AppContext`, store, route and operations as the authorities. Derive summary values with `createMemo`, subscribing to ring version signals where needed; do not serialize the full store per keystroke. Every fetched region belongs to a keyed workspace/entity; stale results are ignored/disposed. Preserve one in-flight request and existing event-driven refresh triggers. Shared primitives accept data and callbacks; view-specific formatting stays with the view unless already in `format.ts`. Avoid deep imports between slice-owned views; use the agreed primitives/`PanelHead`/operations interfaces.

Keep fixed list heights in one constant per density/presentation and pass the same number into `VirtualList`. If changing it at runtime, preserve selection and scroll anchor; do not leave `const H = props.rowHeight` frozen. Match CSS and virtual geometry in tests. Any per-agent list supports a stable key, not an array index that changes while pushes arrive.

### Tests and visual sign-off

`bun run build` and `bun run test` remain mandatory. Run existing mock E2E after each integrated slice, then performance and live suites through their normal harness. On NixOS set `PLAYWRIGHT_EXECUTABLE_PATH` to the local Chromium executable. Every run uses its own `--output` directory so concurrent captures cannot delete each other's results.

Screenshot matrix: 1440×960, 1024×768, 390×844, and 200% desktop zoom; light/dark; reduced motion; baseline and empty/attention/limits/long-text/disconnected scenarios. At minimum capture all 15 audited surfaces; add model picker, question, any approved T2 list modes, and the pending/error/removed/stopped states. Freeze scenario timestamps/animation for visual comparison without changing production clocks. Review real image differences manually; do not claim that pixel equality establishes usability.

Accessibility checks: Tab-only path through navigation, graphs/lists, panel controls, composer, switcher, dialogs; visible focus, return focus, modal containment, labelled fields/buttons, status announcements, 4.5:1 text and 3:1 meaningful controls/graphics, no colour-only statuses, 44px coarse-pointer controls if T2 ships, no shell-forced document overflow at 390px or 200% zoom. New-message arrivals must not cause an assertive announcement per row. Existing `aria-live` regions remain limited to meaningful operation/availability feedback.

Meaningful new tests: theme preference validation/restoration; derived counts do not double-count attention; narrow panel opens/closes/restores focus; list mode retains selection; responsive virtual row heights preserve anchoring; no-result vs no-data vs RPC-error states; reply/metadata text remains inert; quota/usage missing-data coverage; keyboard context menu. Pure token/spacing changes need visual checks, not brittle unit tests that restate CSS.

<a id="budget"></a>
## §budget — delivery tiers and byte allocation

Keep P1–P4 unchanged. The first draft described more new behaviour than the existing 6,470 script bytes and 1,513 CSS bytes could support. This revision makes the visual/reading improvement the required delivery and makes larger new surfaces conditional. No assumed deduplication credit and no "we will fit it at integration" exception. Static mockup CSS is a visual reference, not code to paste into the product.

### Required T0

T0 is the revamp acceptance baseline: semantic light/dark tokens and theme choice; type/spacing/contrast; hierarchy around the existing shell counts and controls; compact/short-window chrome; retained warnings/usage/questions/p2p/toasts; 84px bus previews using the same list; 36px events and responsive row geometry; inspector section order and current operations; task/usage/evidence styling; focusable separator, visible graph controls/More; readable full-width panel below 768px; no document overflow forced by the shell. Preserve every existing fact, action and lifecycle behaviour. Limit new copy by replacing old copy rather than adding repeated explanations.

T0 does not add Team List, Task List/search/filter, an attention drawer, a session-details drawer, bus search/type filters, explicit follow controls, raw metadata/data viewers, new charts, or a 14-icon library. Existing pointer/keyboard graph navigation, tail follow, pair filter, local log filters and detail panels remain available. The UI can be elegant and content-rich by exposing today's content more clearly before adding more state and controls.

### Allowances

The frozen measurement baseline is commit `12207423` (product source unchanged from audited bundle): P1 213,530; P2 28,487; P3 332,896; P4 575,599 bytes. All figures below are minified production bytes, not gzip. Per-slice allowance is the net change against the same approved foundation, including any QA test-independent source fix pulled into the slice. Moving files/chunks is not a saving: P1 sums all nonterminal script.

| Stage/slice | Maximum net P1 change | Maximum net P2 change | Measurement base |
|---|---|---|---|
| `ui` foundation | +1,000 | +700 | Frozen product baseline |
| `ui` remainder | +1,800 | +200 | Approved foundation |
| `graph` | +1,300 | +200 | Approved foundation |
| `content` | +1,700 | +300 | Approved foundation |
| Integration reserve at maximum foundation size | 670 | 113 | Unallocated; reviewer-controlled |
| Total maximum change | 6,470 | 1,513 | Reaches, never raises, P1/P2 ceilings |

Foundation review must report measured `F1`/`F2`, plus P3/P4 and all manifest asset sizes. After foundation, exact reserve = `220000 - F1 - 4800` script and `30000 - F2 - 700` CSS. If foundation is smaller, additional headroom stays in reserve; no slice can spend it silently. If it exceeds its own caps, revise it before downstream work. Reviewer may reassign measured reserve or a completed slice's unused allowance in a recorded review comment; the sum must still fit P1/P2. No slice can approve its own allowance increase.

Graph owns terminal theme changes only: P3 growth allowance at most 256 bytes (revamp target ≤333,152). No terminal dependency/features are added. Including that allowance, full maximum script/CSS growth is 8,239 bytes; projected P4 upper bound 583,838, leaving 16,162 bytes under the unchanged 600,000 ceiling. Still measure P4: fonts, assets, manifest or unexpected chunks cannot hide outside P1/P2. QA/dev/gallery/tests add zero shipped bytes.

Every slice review request includes source commit, approved foundation commit, QA test commit, production build command, P1/P2/P3/P4 measured totals and deltas, relevant tests, and features delivered by tier. A slice above its allowance is not approvable even if its isolated build passes P1. All slices share the same remaining space; four individually green absolute-budget checks are insufficient. Integration remeasures the combined bundle before final approval.

### Conditional T1 and T2; cut order

After T0 is green within its allowance, T1 candidates are small uses of already-loaded values: task lifecycle timestamps and artifact producer/date; inspector fallback/limit timing/attempt details; usage aggregate totals with reporting coverage; a simple summary attention count; local Retry/error copy; visible key-preference toggle. No new dataset or persistent state. A candidate requires measured byte delta and recorded remaining allowance before acceptance scope is expanded.

T2 is not committed implementation scope. It covers the larger alternatives shown in some reference images: Team and Task List modes and their search/filter state; full 390px/coarse-pointer optimization; explicit Follow/Jump counters; bus search/type filter; usage-series isolation; raw Event.data/message metadata; session details disclosure; icons beyond six. Build none speculatively while required T0 work remains.

If an optional candidate makes its slice or integration exceed the allocation, cut in this fixed order (first removed first): (1) extra icon paths and raw data/metadata; (2) session details and usage isolation; (3) bus search/type and explicit-follow counters; (4) full sub-640px/coarse-pointer specialization; (5) Team List; (6) Task List/search/filter; (7) optional summary/usage aggregates and attention count; (8) extra lifecycle/fallback/artifact facts beyond today's display; (9) new retry/toggle controls. Existing functionality is never a cut candidate. If T0 alone does not fit after ordinary replacement/refactoring, stop that slice for design review with its measured delta; no budget increase is authorized by this document.

Mockup scope labels matter: Team/Tasks List and extra aggregate/filter controls are T2/T1 references. New DAG, attention shell, inspector, bus rows, themes, and panel replacement are T0 references. During implementation omit an unapproved optional control entirely. Final QA report lists actual tiers delivered and any deferred references; it does not count a deferred T2 feature as a regression.

<a id="keep"></a>
## §keep — constraints and existing tests

### Security and lifecycle

All of [web-design.md §security](web-design.md#security) and [web-multi.md §multi-security](web-multi.md#multi-security) remain binding. Keep CSP, plain JSX text, allowed `http:`/`https:` links, no Markdown renderer or HTML sinks, no token exposure, and safe terminal handling. `web/test/no-html-sinks.test.ts` must remain green. No remote font/image/icon requests and no newly lazy-loaded chunks. Every chunk/asset required by the document must be requested before its initial load finishes; after load only permitted WebSocket upgrades occur. Test this for every newly reachable interaction.

Stopped is final for the document. Do not add Reload, Retry connection, or fetch-on-tab-open that defeats it. Existing exact stopped-origin warning about running `crew web` for a new address remains. Local navigation, filtering, selecting and copying cached text remain possible. Wire mutations require the existing `link.live()` guard; visually hiding a button is not the guard.

Retain the two-level index/workspace state model; workspace status comes from the listing, never a socket close alone. Preserve revision/generation/daemon attempt rules, switching disposal, back/forward/deep links, route restoration, summary badges, stopped workspace return, and unjoined session handling. A view may not fetch all workspace snapshots to fill a switcher. Preferences remain split between dashboard and workspace. Do not change protocol/net/store while styling unless a specific reviewed bug requires it.

### Budgets

| Gate | Limit | Audited committed bundle | Headroom |
|---|---|---|---|
| P1 nonterminal JS | 220,000 bytes | 213,530 | 6,470 |
| P2 all CSS | 30,000 bytes | 28,487 | 1,513 |
| P3 terminal JS | 350,000 bytes | 332,896 | 17,104 |
| P4 entire `web-dist` | 600,000 bytes | 575,599 | 24,401 |

These are ceilings, not suggestions. Replace/refactor existing styles and duplicated view markup; do not append a second theme below the old one. Extracting styles does not create size headroom by itself. No icon/chart/animation/component library, shipped font, or mockup image. `web/test/budget.test.ts` stays unchanged, including the exact three runtime dependencies. `qa` reports final sizes before integration completes. If the design cannot fit, bring a concrete byte breakdown and simpler implementation to review; never quietly increase a budget.

Other performance invariants: P5/P6 first paint/live readiness; P7–P11 push/frame latency; P12 visible virtual rows +20; P13 no idle frames/sub-second timers; P14 terminal latency; P15 bounded heap; P17–P20 multi-workspace switching/idle constraints from `web-multi.md`. Keep the existing bounded rings and event-driven animations. Do not continuously animate the Live indicator or poll for freshness.

### Test compatibility and deliberate changes

Preserve existing semantic identities where possible: `data-conn`, `data-workspace-state`, `data-agent`, `data-task`, `data-inspector`, message/event IDs, route fragments, ARIA list labels, and operation method names. Do not weaken assertions just to make a redesign pass. Class hooks are also compatibility contracts: preserve `.node.member`, `.node-head`, `.badge.ask`, `.panel-title`, `.usage-strip`, `.question-banner`, `.vrow`, `.vlist-pad`, `.bus-row`, `.log-row`, `.hover-card`, `.task.dim`, `.pulse`, `.mark`, `.sweep` and every other existing class selector until its owner supplies an explicit old-selector/new-selector migration list to QA. Exact accessible names such as `model` and `copy` follow the same rule. A generic wildcard locator is not an acceptable migration.

| Existing suite | Expected treatment |
|---|---|
| `auth`, `policy`, `reconnect`, `workspaces`, `reviewer-multi`, `reviewer-resources`, `test/page.test.tsx`, live lifecycle/outside/multi/open | Behaviour unchanged; security/network/lifecycle assertions preserved |
| `actions`, `question`, command/unit tests, live operate/vcs | Actions and RPC payloads unchanged; update selectors only for controls deliberately placed in More menus |
| `keyboard`, keys unit tests | Existing shortcuts/scope preserved; add Tab-only and Shift+F10 coverage; visible labels may change |
| `graph`, `tasks`, `anim`, layout/anims unit tests | Topology, order, lineage, selection and animation meaning preserved; geometry assertions may change for new card sizes; add List checks only if T2 ships |
| `lists`, `virtual.test.tsx`, transcript assertions | Update expected row height/count geometry intentionally; preserve anchoring, tail-following, ring bounds, keyboard and P12 |
| `reviewer-inspector-identity`, `reviewer` | Preserve keyed identity and text expectations; disclosures may require opening before asserting advanced facts |
| `perf` and live perf/terminal/wakeups | Limits unchanged; run against rebuilt production assets |
| `e2e/mock.spec.ts` | Preserve all mock-controller contracts while adding scenarios; generated fixtures unchanged |
| `prefs` and multi-preference tests | Add validated theme choice without changing dashboard/workspace ownership or legacy defaults |

Visible copy changes to account for deliberately: “graph” navigation becomes “Team”; “logs” navigation becomes “Events”; “bus” heading becomes “Message bus”; grouping of inspector operations; richer empty-state wording. Key names and route strings do not change. Tests asserting labels should use the new meaningful label, not a broad selector that stops testing accessibility.

<a id="split"></a>
## §split — work order, file ownership, and acceptance

### Order and shared interfaces

1. Reviewer approves this design and reference images.
2. `ui` lands a small foundation commit first: tokens, primitives, and extraction of the existing styles into the ownership files below. It builds and preserves existing behaviour. Send that commit directly to `graph`, `content`, and `qa`.
3. `ui`, `graph`, and `content` implement in their owned files on the foundation. `qa` builds scenarios/gallery/tests in parallel on the same foundation. Coordinate additions to a shared interface through its owner; no two agents edit `theme.css` or `ui.ts` independently.
4. `qa` integrates all slice commits, builds `web-dist`, runs suites and records screenshot/budget evidence. Designer reviews final live screenshots against these references; reviewer handles code/security/functional sign-off. Fixes stay with the owning slice, then integrate and re-check the affected cases.

Foundation exports under `web/src/components/`: `Icon`, `StatusBadge`, `EmptyState`, `ViewHeader`. Preserve `PanelHead` in `views/SidePanel.tsx`. Add `ui.busOpen()/setBusOpen(boolean)` for explicit narrow bus visibility; SidePanel also opens for an entity selection or pair selection. Content uses existing `ui.select`, `ui.setBusPair`, and filters; graph uses existing `ui.setMenu` for a visible More trigger. Graph/List choice is local view state, not a new route/protocol. If a primitive's API needs changing, ask `ui` to change it.

### Ownership

| Slice | Exclusive source ownership after foundation |
|---|---|
| `ui` | `web/src/theme.css`; `web/src/styles/ui.css`; `web/src/components/**`; `views/Shell.tsx`, `SidePanel.tsx`, `Switcher.tsx`, `ContextMenu.tsx`, `MessageBox.tsx`, `dialogs.tsx`; `keys.ts`, `ui.ts`, `prefs.ts`, `commands.ts`, `operate.ts`, `views/ops.ts`; `main.tsx` only if needed for style/theme wiring |
| `graph` | `web/src/graph/**`; `web/src/anim/**`; `views/Inspector.tsx`, `TerminalTab.tsx`; `web/src/term/Terminal.ts` for theme values only; `web/src/styles/graph.css`; `format.ts` only for necessary shared graph/inspector formatting |
| `content` | `views/Bus.tsx`, `Logs.tsx`, `EventPanel.tsx`, `MessagePanel.tsx`, `TaskPanel.tsx`, `UsageView.tsx`, `VirtualList.tsx`, `usage.ts`, `waiting.ts`; `web/src/styles/content.css`; small new content-only helpers alongside these |
| `qa` | `web/dev/**`, `web/e2e/**`, `web/test/**`, `web/vite.config.ts` and Playwright/Vitest config if needed, `web/package.json` scripts/dev dependencies only, README web instructions, `crates/crew/web-dist/**` build output |
| `designer` | `notes/web-revamp.md`, `notes/web-revamp/**` |

Frozen in this revamp: `web/src/app.ts`, `clock.ts`, `text.tsx`, `route.ts`, `protocol.ts`, `net/**`, `store/**`, `web/index.html`, backend code. No slice owns a protocol/net/store/backend rewrite. Request a specific exception with its need; prefer local view composition. `qa` owns all test files to avoid conflicts. The handoff below makes each source branch independently reviewable. Tests are part of each slice's acceptance even though QA commits them. `ui` foundation moves graph/node/hover/inspector/terminal CSS into `styles/graph.css`; lists/task-detail/message/event/usage CSS into `styles/content.css`; shell/dialog/control CSS into `styles/ui.css`. `theme.css` holds tokens/reset/imports. Do not import `graph.css` from `content.css` or create a circular dependency.

### Build output, test handoff, and shared interfaces

Before any source work, the slice checks that `crates/crew/web-dist` is clean. It may run `bun run build` locally, measure the manifest, and run frontend/e2e tests against that temporary bundle. Before every non-QA commit, remove only untracked generated files inside `crates/crew/web-dist` (`git clean -fd -- crates/crew/web-dist`) and restore its tracked files (`git restore --worktree -- crates/crew/web-dist`). Do not restore source edits. Verify no bundle path appears in the commit. Crew's `git_commit` commits the whole worktree, so this cleanup is mandatory. QA alone commits the final integrated bundle; `crates/crew/tests/web_bundle.rs` is expected to fail the source-stamp check on source-only slice branches until that rebuild. Report this narrow expected failure separately from frontend checks.

Test handoff sequence: implementer sends QA the source candidate commit plus exact selector/name/geometry changes and requested behavioural assertions; QA cherry-picks that source into its review branch, commits only test changes, and returns the test commit; implementer cherry-picks that test commit, rebuilds locally, runs its required suites, records the figures, restores generated bundle output, and requests review naming both source and test commits. Candidate commits can precede final green review; no candidate is declared accepted before the paired tests pass. Foundation follows the same sequence if any test contract changes. QA keeps test commits separate from source/integration commits so they cherry-pick cleanly.

`VirtualList` remains backwards compatible with every existing prop and the transcript's 22px default use. T0 responsive row height may become reactive internally without changing callers' semantics. Any T2 follow/jump API must be optional; no required new prop or return type. Content sends the exact proposed interface to graph and QA before changing it. Graph does not edit the shared list to solve a transcript-only problem. New shared formatting goes through graph, the `format.ts` owner; do not fork formatters across slices.

Help ownership deliberately moves from the card's content grouping to `ui` because it shares `dialogs.tsx` and the key table. Empty-state visuals belong to shared primitives; state-specific copy/logic belongs to each view owner. These are intentional ownership choices, not unassigned work.

### Acceptance by slice

`ui`: matched themes, readable contrast, coherent typography/spacing, four visible views and objective/summary; reachable Commands/Help/appearance/workspace actions; responsive side-by-side layout from 768px and main/panel replacement below it, with no shell-forced 390px document overflow; clear loading/availability/warning/question states; labelled controls, focus return/trap/resize/menu behaviour; all keys/actions and multi-workspace behaviour retained. Foundation and final builds fit unchanged size budgets. No HTML sinks, remote assets, or after-load HTTP.

`graph`: readable restrained nodes and visible tools/legend; lineage/runtime/status/badges intact; correct satellite and task dependency geometry; T0 graph/DAG retained with readable tools and selection; Graph/List and narrow default List required only if T2 is approved; keyboard/pointer parity; better inspector hierarchy without losing fields/actions; reliable terminal/transcript/diff states; keyed response isolation; reduced-motion and idle behaviour retained. Existing graph/task/anim/identity/terminal tests plus any approved T2 list mode checks pass.

`content`: message bodies readable without opening each row; existing pair/log filter scope clear; preserved list anchoring/tail-follow; new search/type/follow controls only if T2 is approved; event message gets space; existing task result/artifact details; added timestamps/producer facts only if T1 is approved; explicit unknown versus zero usage and quota freshness; plain-text metadata only if T2 is approved; accurate local empty/loading/error states; narrow layouts; bounded rows/rings and no new polling. Existing lists/actions/resources tests plus approved new pending/error/missing-data cases pass.

`qa`: deterministic scenarios, complete real-component screenshot gallery with reproduction commands, keyboard/contrast/responsive checks, unchanged security/performance gates, intentional selector/geometry updates documented, production bundle rebuilt once integration is final, README dev loop current. Artifact report names failing/passing commands, final sizes and remaining gaps; do not present mockup images as implemented screenshots.

### Design provenance

Applied the local `ui-ux-pro-max` and `agent-browser` skills. The design-system search's documentation-landing suggestion was not a fit and was discarded. A narrower `real-time monitoring dashboard` style query matched the monitoring/attention use case. Adopted its legibility, explicit state and keyboard principles; did not adopt suggested polling, continuous pulse, external fonts, GSAP, or chart libraries because they conflict with this product's existing contracts. The screen-specific design above is based on the running application and its actual protocol.
