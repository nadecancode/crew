// The key table and its dispatcher (notes/web-design.md §ui "Focus and keys").
// One data structure: the handler runs it and the help overlay lists it.
//
// `tab` belongs to the browser and is never taken outside a dialog.
// Single-key shortcuts act on the selected agent whichever region has focus,
// and are inactive while a text field, a dialog or the terminal has focus,
// and after `/keys off`; `ctrl-k` (the command line), `esc` and the buttons
// remain. Keys a browser keeps for itself are not rebound.

import type { App } from "./app";
import type { Operations } from "./operate";

export type Scope = "page" | "graph" | "inspector" | "lists" | "logs" | "tasks";

export type KeyDef = {
  /** As the help overlay shows them. */
  keys: string;
  action: string;
  /** What the TUI does with it, or `new`. */
  tui: string;
  scope: Scope;
  /** `e.key` values this row handles here; none for rows handled where focus
   * is (the graph's nodes, the lists). */
  match?: string[];
  run?(app: App, ops: Operations, key: string): void;
};

const inspectorOpen = (app: App) => app.ui.sel()?.kind === "agent";
const inspected = (app: App) => {
  const s = app.ui.sel();
  return s?.kind === "agent" ? s.id : null;
};

/** Hand a key to the main view's list or graph, focusing it first. The
 * forwarded event reaches the page's handler again on its way up: it is
 * cancelable, so the list's preventDefault ends it there, and a forwarded key
 * is never forwarded twice. */
let forwarding = false;
function toMain(key: string): void {
  if (forwarding) return;
  const target = document.querySelector<HTMLElement>("#main .vlist, #main [data-task][tabindex='0']");
  if (!target) return;
  target.focus();
  forwarding = true;
  try {
    target.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  } finally {
    forwarding = false;
  }
}

export const KEYS: KeyDef[] = ([
  ["→ ↓ / ← ↑", "next / previous agent, in the graph", "tab / shift-tab, arrows", "graph"],
  ["enter", "open the selected agent's inspector", "same", "page", ["Enter"], (app) => {
      const id = app.ui.selected();
      if (!id) return app.ui.setStatus("no agent selected", true);
      app.ui.select({ kind: "agent", id });
      queueMicrotask(() => document.querySelector<HTMLElement>("#side-panel [data-focus-first]")?.focus());
    }],
  ["o", "select the orchestrator", "same", "page", ["o"], (_a, ops) => ops.selectOrchestrator()],
  ["m / M", "message the selected agent / broadcast", "same", "page", ["m", "M"], (_a, ops, k) => (k === "M" ? ops.compose("*") : ops.compose())],
  ["/ or ctrl-k", "command line", "/", "page", ["/"], (_a, ops) => ops.command()],
  ["t / l", "tasks / logs (again: back to the graph)", "same", "page", ["t", "l"], (_a, ops, k) => ops.toggleView(k === "t" ? "tasks" : "logs")],
  ["p", "pause / resume the selected agent", "same", "page", ["p"], (_a, ops) => ops.togglePause()],
  ["r (R in the inspector)", "restart the selected agent; in the inspector r reads its tab again", "same", "page", ["r", "R"], (app, ops, k) => {
      if (inspectorOpen(app) && k === "r") return app.ui.refresh();
      if (inspectorOpen(app) || k === "r") ops.restart();
    }],
  ["k", "kill the selected agent (asks first); scrolls in tasks and logs", "same", "page", ["k", "j"], (app, ops, k) => {
      if (app.ui.view() === "logs" || app.ui.view() === "tasks") return toMain(k);
      if (k === "k") ops.kill();
    }],
  ["g", "jump to the agent's herdr pane", "same", "page", ["g"], (_a, ops) => ops.focusPane()],
  ["O / s", "model and CLI of the orchestrator / of the selected agent", "same", "page", ["O", "s"], (app, ops, k) => (k === "O" ? ops.orchestratorModel() : ops.model(inspected(app) ?? app.ui.selected()))],
  ["u / U", "continue the agents paused at a limit / restart them on the current login (asks first)", "same", "page", ["u", "U"], (_a, ops, k) => (k === "u" ? ops.resumePaused() : ops.restartPaused())],
  ["a", "reopen a question that was put off", "same", "page", ["a"], (_a, ops) => ops.questions()],
  ["S", "stop the team (asks first)", "same", "page", ["S"], (_a, ops) => ops.stop()],
  ["?", "keys and commands", "same", "page", ["?"], (app) => app.ui.openDialog({ kind: "help" })],
  ["esc", "close the dialog, else the side panel, else clear the status line", "same", "page"],
  ["i", "prompt box: text typed is submitted to the agent as one prompt", "same", "inspector", ["i"], (app, ops) => inspected(app) && ops.prompt(inspected(app)!)],
  ["d, f", "diff tab; side panel fills the main area (in logs: f filters to the selected agent)", "same", "inspector", ["d", "f"], (app, _ops, k) => {
      if (k === "f" && app.ui.view() === "logs") {
        const f = app.ui.logFilter();
        return app.ui.setLogFilter({ ...f, agent: f.agent ? null : app.ui.selected() });
      }
      if (!inspectorOpen(app)) return;
      if (k === "d") app.ui.setTab(app.ui.tab() === "diff" ? "overview" : "diff");
      else app.ui.setPanelFull(!app.ui.panelFull());
    }],
  ["click the terminal", "keys go to the agent; ctrl-] gives the keyboard back", "same", "inspector"],
  ["j / k, arrows, PageUp / PageDown, Home / End", "move in lists (bus, log, transcript, task graph)", "same", "lists"],
  ["f (logs)", "filter the log to the selected agent", "same", "logs"],
  ["+ - 0, drag, wheel", "graph: zoom in, out, fit; pan; zoom", "new", "graph", ["+", "=", "-", "0"], (app, _ops, k) => {
      const v = app.ui.view() === "tasks" ? app.views.tasks : app.ui.view() === "graph" ? app.views.graph : undefined;
      if (!v) return;
      if (k === "0") v.fit();
      else v.zoom(k === "-" ? 1 / 1.25 : 1.25);
    }],
  ["v", "the usage view", "new", "page", ["v"], (_a, ops) => ops.toggleView("usage")],
  ["w", "switch workspace: arrows, enter; typing filters; 1–9 directly", "new", "page", ["w"], (app) => app.page.setSwitcher(true)],
  ["q", "nothing: closing the tab leaves the team running", "quits the TUI", "page"]
] as [string, string, string, Scope, string[]?, KeyDef["run"]?][]).map(([keys, action, tui, scope, match, run]) => ({ keys, action, tui, scope, match, run }));

const textual = (el: Element | null) =>
  !!el && (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement || (el as HTMLElement).isContentEditable);

/** Whether single-key shortcuts may act: not in a text field, a dialog, the
 * terminal or the workspace switcher. */
export function keyboardFree(app: App, target: Element | null = document.activeElement): boolean {
  if (app.ui.dialog()) return false;
  if (app.ui.menu()) return false;
  if (textual(target)) return false;
  if (target?.closest?.("[data-terminal]")) return false;
  // The workspace switcher's list takes its own keys.
  if (target?.closest?.('[data-region="switcher"]')) return false;
  return true;
}

/** The page's keydown handler. */
export function handleKey(app: App, ops: Operations, e: KeyboardEvent): void {
  if (e.defaultPrevented || e.key === "Tab" || e.isComposing) return;
  const target = (e.target as Element | null) ?? document.activeElement;
  if (!keyboardFree(app, target)) return;
  // Native controls keep Enter as activation; SVG agents use the page action.
  if (e.key === "Enter" && target?.closest?.("button, summary, a[href]")) return;
  // The command line opens even with the shortcuts off; ctrl-k is taken only then.
  if (e.ctrlKey && !e.metaKey && !e.altKey && (e.key === "k" || e.key === "K")) {
    if (!app.link.live()) return;
    e.preventDefault();
    ops.command();
    return;
  }
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  if (e.key === "Escape") {
    app.ui.setMenu(null);
    if (app.ui.panelOpen()) app.ui.closePanel();
    else app.ui.clearStatus();
    return;
  }
  if (!app.prefs.prefs.keys) return;
  // A stopped or reconnecting page is read-only: views may change, nothing is sent.
  const readOnly = !app.link.live();
  const def = KEYS.find((k) => k.match?.includes(e.key));
  if (!def?.run) return;
  if (readOnly && !["t", "l", "v", "w", "?", "+", "=", "-", "0", "o", "Enter", "j", "k", "f", "d"].includes(e.key)) return;
  e.preventDefault();
  def.run(app, ops, e.key);
}
