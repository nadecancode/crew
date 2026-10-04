// What the page shows besides the daemon's state: the route and selection,
// the side panel, dialogs, the status line and notices.

import { createSignal } from "solid-js";
import { formatRoute, parseRoute, sameSelection, type Route, type Selection, type View } from "./route";

export type InspectorTab = "overview" | "terminal" | "transcript" | "diff";

export type Confirm =
  | { kind: "kill"; agent: string }
  | { kind: "restart-fresh"; agent: string }
  | { kind: "stop" }
  | { kind: "restart-paused"; count: number };

export type Dialog =
  | { kind: "question"; id: string }
  | { kind: "model"; agent: string }
  | { kind: "confirm"; confirm: Confirm }
  | { kind: "help" };

/** The message box: who it is addressed to, what is typed, and whether it
 * is the inspector's prompt box (submitted to the agent's terminal). */
export type Compose = { to: string; text: string; prompt: boolean };

export type StatusLine = { text: string; error: boolean; id: number };
export type Notice = { text: string; id: number };

export type LogFilter = { agent: string | null; level: "debug" | "info" | "warn" | "error"; text: string; vcs: boolean };

export type ContextMenu = { agent: string; x: number; y: number };

const STATUS_MS = 3000;
const STATUS_ERROR_MS = 6000;
const NOTICE_MS = 30_000;

type Loc = { hash: string };
type Hist = { pushState(data: unknown, unused: string, url?: string): void; replaceState(data: unknown, unused: string, url?: string): void };

export function createUi(loc: Loc = location, hist: Hist = history) {
  const [route, setRouteSignal] = createSignal<Route>(parseRoute(loc.hash));
  /** The agent keys act on, and the graph's one tab stop. */
  const [selected, setSelected] = createSignal<string | null>(route().sel?.kind === "agent" ? (route().sel as { id: string }).id : null);
  const [tab, setTab] = createSignal<InspectorTab>("overview");
  const [panelFull, setPanelFull] = createSignal(false);
  const [dialog, setDialogSignal] = createSignal<Dialog | null>(null);
  const [status, setStatusSignal] = createSignal<StatusLine | null>(null);
  const [notices, setNotices] = createSignal<Notice[]>([]);
  const [menu, setMenu] = createSignal<ContextMenu | null>(null);
  const [compose, setCompose] = createSignal<Compose>({ to: "*", text: "", prompt: false });
  /** Bumped by the refresh key: the inspector reads its tab again. */
  const [refreshTick, setRefreshTick] = createSignal(0);
  const [logFilter, setLogFilter] = createSignal<LogFilter>({ agent: null, level: "debug", text: "", vcs: false });
  /** The bus filtered to messages between two agents (a link was clicked). */
  const [busPair, setBusPair] = createSignal<[string, string] | null>(null);
  /** A link hovered in the bus: the graph lights it. */
  const [hoverPair, setHoverPair] = createSignal<[string, string] | null>(null);
  /** Questions whose dialog has been on screen once (it does not pop again). */
  const shownQuestions = new Set<string>();
  /** Where focus goes back to when a dialog or the side panel closes. */
  const focusStack: (HTMLElement | null)[] = [];
  let statusTimer: number | null = null;
  let nextId = 1;

  function setRoute(next: Route, push = true): void {
    const cur = route();
    if (cur.view === next.view && sameSelection(cur.sel, next.sel)) return;
    if (next.sel?.kind === "agent") setSelected(next.sel.id);
    if (next.sel?.kind === "agent" && !(cur.sel?.kind === "agent" && cur.sel.id === next.sel.id)) setTab("overview");
    setRouteSignal(next);
    const hash = formatRoute(next);
    if (loc.hash !== hash) (push ? hist.pushState : hist.replaceState).call(hist, null, "", hash);
  }

  /** Back and forward. */
  function fromLocation(): void {
    const next = parseRoute(loc.hash);
    if (next.sel?.kind === "agent") setSelected(next.sel.id);
    setRouteSignal(next);
  }

  function setStatus(text: string, error = false): void {
    if (statusTimer !== null) window.clearTimeout(statusTimer);
    const id = nextId++;
    setStatusSignal({ text, error, id });
    statusTimer = window.setTimeout(() => {
      statusTimer = null;
      setStatusSignal((s) => (s?.id === id ? null : s));
    }, error ? STATUS_ERROR_MS : STATUS_MS);
  }

  function notice(text: string): void {
    const id = nextId++;
    setNotices((ns) => [...ns.slice(-3), { text, id }]);
    window.setTimeout(() => setNotices((ns) => ns.filter((n) => n.id !== id)), NOTICE_MS);
  }

  function openDialog(d: Dialog): void {
    if (!dialog()) focusStack.push(document.activeElement as HTMLElement | null);
    if (d.kind === "question") shownQuestions.add(d.id);
    setMenu(null);
    setDialogSignal(d);
  }

  function closeDialog(): void {
    if (!dialog()) return;
    setDialogSignal(null);
    const back = focusStack.pop();
    queueMicrotask(() => back?.isConnected && back.focus());
  }

  return {
    route,
    setRoute,
    fromLocation,
    view: () => route().view,
    sel: () => route().sel,
    setView: (view: View) => setRoute({ view, sel: route().sel }),
    select: (sel: Selection) => setRoute({ view: route().view, sel }),
    selected,
    setSelected,
    tab,
    setTab,
    panelFull,
    setPanelFull,
    dialog,
    openDialog,
    closeDialog,
    /** Replace the open dialog's state (typing in the compose box). */
    updateDialog: (d: Dialog) => setDialogSignal(d),
    status,
    setStatus,
    clearStatus: () => setStatusSignal(null),
    notices,
    notice,
    dismissNotice: (id: number) => setNotices((ns) => ns.filter((n) => n.id !== id)),
    menu,
    setMenu,
    compose,
    setCompose,
    refreshTick,
    refresh: () => setRefreshTick((n) => n + 1),
    logFilter,
    setLogFilter,
    busPair,
    setBusPair,
    hoverPair,
    setHoverPair,
    shownQuestions,
  };
}

export type Ui = ReturnType<typeof createUi>;
