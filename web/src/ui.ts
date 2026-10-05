// What the page shows besides the daemon's state. The status line and
// notices are the page's (`createChrome`); the route and selection, the
// side panel and dialogs are the workspace's on screen (`createUi`), made
// anew for each workspace and disposed with it (notes/web-multi.md §multi-ui
// "A switch").

import { createSignal } from "solid-js";
import { formatRoute, parseRoute, sameSelection, type Route, type Selection, type View } from "./route";

/** A workspace's route: its view and its one selection. */
export type Place = Omit<Route, "workspace">;

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
/** A notice may offer one action ("switch"). */
export type Notice = { text: string; id: number; action?: { label: string; run(): void } };

export type LogFilter = { agent: string | null; level: "debug" | "info" | "warn" | "error"; text: string; vcs: boolean };

export type ContextMenu = { agent: string; x: number; y: number };

const STATUS_MS = 3000;
const STATUS_ERROR_MS = 6000;
const NOTICE_MS = 30_000;

type Loc = { hash: string };
type Hist = { pushState(data: unknown, unused: string, url?: string): void; replaceState(data: unknown, unused: string, url?: string): void };

/** The page's: the status line and notices, kept across switches. */
export function createChrome() {
  const [status, setStatusSignal] = createSignal<StatusLine | null>(null);
  const [notices, setNotices] = createSignal<Notice[]>([]);
  let statusTimer: number | null = null;
  let nextId = 1;

  function setStatus(text: string, error = false): void {
    if (statusTimer !== null) window.clearTimeout(statusTimer);
    const id = nextId++;
    setStatusSignal({ text, error, id });
    statusTimer = window.setTimeout(() => {
      statusTimer = null;
      setStatusSignal((s) => (s?.id === id ? null : s));
    }, error ? STATUS_ERROR_MS : STATUS_MS);
  }

  function notice(text: string, action?: Notice["action"]): void {
    const id = nextId++;
    setNotices((ns) => [...ns.slice(-3), { text, id, action }]);
    window.setTimeout(() => setNotices((ns) => ns.filter((n) => n.id !== id)), NOTICE_MS);
  }

  return {
    status,
    setStatus,
    clearStatus: () => setStatusSignal(null),
    notices,
    notice,
    dismissNotice: (id: number) => setNotices((ns) => ns.filter((n) => n.id !== id)),
  };
}

export type Chrome = ReturnType<typeof createChrome>;

export type UiOptions = {
  /** The workspace on screen; its id goes into every address. */
  workspace?: string | null;
  /** Its route when it opens: the address's, or the one it had when left. */
  place?: Place;
  chrome?: Chrome;
  loc?: Loc;
  hist?: Hist;
};

export function createUi(o: UiOptions = {}) {
  const loc = o.loc ?? location;
  const hist = o.hist ?? history;
  const workspace = o.workspace ?? null;
  const chrome = o.chrome ?? createChrome();
  const [route, setRouteSignal] = createSignal<Place>(o.place ?? parseRoute(loc.hash));
  /** The agent keys act on, and the graph's one tab stop. */
  const [selected, setSelected] = createSignal<string | null>(route().sel?.kind === "agent" ? (route().sel as { id: string }).id : null);
  const [tab, setTab] = createSignal<InspectorTab>("overview");
  const [panelFull, setPanelFull] = createSignal(false);
  /** The message bus explicitly opened in a compact workspace. */
  const [busOpen, setBusOpen] = createSignal(false);
  const [viewport, setViewport] = createSignal(window.innerWidth);
  const [dialog, setDialogSignal] = createSignal<Dialog | null>(null);
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
  let panelFocus: HTMLElement | null = null;
  let menuFocus: HTMLElement | null = null;

  /** This workspace's address for `place`. */
  const address = (place: Place) => formatRoute({ workspace, ...place });

  function setRoute(next: Place, push = true): void {
    const cur = route();
    if (cur.view === next.view && sameSelection(cur.sel, next.sel)) return;
    if (next.sel && !cur.sel && !busOpen()) panelFocus = document.activeElement as HTMLElement | null;
    if (next.sel?.kind === "agent") setSelected(next.sel.id);
    if (next.sel?.kind === "agent" && !(cur.sel?.kind === "agent" && cur.sel.id === next.sel.id)) setTab("overview");
    setRouteSignal({ view: next.view, sel: next.sel });
    const hash = address(next);
    if (loc.hash !== hash) (push ? hist.pushState : hist.replaceState).call(hist, null, "", hash);
  }

  /** Back and forward within this workspace. */
  function fromLocation(): void {
    const next = parseRoute(loc.hash);
    if (next.sel?.kind === "agent") setSelected(next.sel.id);
    setRouteSignal({ view: next.view, sel: next.sel });
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

  function closePanel(): void {
    const id = selected();
    setBusOpen(false);
    setPanelFull(false);
    setRoute({ view: route().view, sel: null });
    queueMicrotask(() => {
      if (panelFocus?.isConnected) panelFocus.focus();
      else if (id) document.querySelector<HTMLElement>(`[data-agent="${CSS.escape(id)}"]`)?.focus();
    });
  }

  function closeMenu(refocus = true): void {
    setMenu(null);
    if (refocus && menuFocus?.isConnected) menuFocus.focus();
  }

  return {
    workspace,
    route,
    setRoute,
    fromLocation,
    address,
    view: () => route().view,
    sel: () => route().sel,
    setView: (view: View) => {
      if (viewport() < 768) {
        setBusOpen(false);
        setPanelFull(false);
      }
      setRoute({ view, sel: viewport() < 768 ? null : route().sel });
    },
    select: (sel: Selection) => setRoute({ view: route().view, sel }),
    selected,
    setSelected,
    tab,
    setTab,
    panelFull,
    setPanelFull,
    busOpen,
    setBusOpen: (open: boolean) => {
      if (open && !busOpen() && !route().sel) panelFocus = document.activeElement as HTMLElement | null;
      setBusOpen(open);
    },
    panelOpen: () => !!route().sel || busOpen(),
    closePanel,
    viewport,
    setViewport,
    dialog,
    openDialog,
    closeDialog,
    /** Replace the open dialog's state (typing in the compose box). */
    updateDialog: (d: Dialog) => setDialogSignal(d),
    status: chrome.status,
    setStatus: chrome.setStatus,
    clearStatus: chrome.clearStatus,
    notices: chrome.notices,
    notice: chrome.notice,
    dismissNotice: chrome.dismissNotice,
    menu,
    setMenu: (m: ContextMenu | null) => {
      if (m) menuFocus = document.activeElement as HTMLElement | null;
      setMenu(m);
    },
    closeMenu,
    compose,
    setCompose,
    refreshTick,
    refresh: () => setRefreshTick((n) => n + 1),
    logFilter,
    setLogFilter,
    busPair,
    setBusPair: (pair: [string, string] | null) => {
      if (pair) {
        if (!route().sel && !busOpen()) panelFocus = document.activeElement as HTMLElement | null;
        setBusOpen(true);
      }
      setBusPair(pair);
    },
    hoverPair,
    setHoverPair,
    shownQuestions,
  };
}

export type Ui = ReturnType<typeof createUi>;
