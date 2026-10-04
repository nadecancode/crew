// The dashboard's preferences, in two places (notes/web-multi.md
// §multi-transport "Preferences"), neither of them the browser's, since the
// page's origin is new at every server start:
//
// - what belongs to a workspace, `layout` (where agents were dragged): its
//   daemon's `web_prefs`, on the workspace connection;
// - what belongs to the operator, `keys`, `panel` and `last` (the workspace
//   last on screen): the dashboard server's `web_prefs`, on the index.
//
// Each is read after `hello`; every field the page uses is validated, unknown
// keys and wrong types are dropped; the whole object is written back 500 ms
// after the last change. Two tabs: the last write wins. A workspace's `keys`
// and `panel` from before the split are read once, as defaults, when the
// server has none.

import { createStore, reconcile, unwrap } from "solid-js/store";
import { WORKSPACE_ID } from "./route";

export type Point = { x: number; y: number };
/** A workspace's: kept by its daemon. */
export type LayoutPrefs = {
  /** Where the operator dragged agents on the graph, by id. */
  layout: Record<string, Point>;
};
/** The operator's: kept by the dashboard server. */
export type DashboardPrefs = {
  /** Single-key shortcuts (`/keys off` turns them off). */
  keys: boolean;
  /** Width of the side panel, in pixels. */
  panel: number | null;
  /** The id of the workspace last on screen. */
  last: string | null;
};
/** What the page reads of both: the views' one object. */
export type Prefs = LayoutPrefs & Omit<DashboardPrefs, "last">;

export const DEFAULT_PREFS: Prefs = { layout: {}, keys: true, panel: null };
export const WRITE_DELAY_MS = 500;
const MAX_LAYOUT = 512;

const finite = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && Math.abs(v) < 1e6;
const record = (raw: unknown): Record<string, unknown> | null => (typeof raw === "object" && raw !== null && !Array.isArray(raw) ? (raw as Record<string, unknown>) : null);

/** What the page keeps of a daemon's `web_prefs`: dragged positions. */
export function validateLayout(raw: unknown): LayoutPrefs {
  const out: LayoutPrefs = { layout: {} };
  const o = record(raw);
  const layout = record(o?.layout);
  if (!layout) return out;
  for (const [id, p] of Object.entries(layout).slice(0, MAX_LAYOUT)) {
    if (typeof p === "object" && p !== null && finite((p as Point).x) && finite((p as Point).y) && id.length <= 128) {
      out.layout[id] = { x: (p as Point).x, y: (p as Point).y };
    }
  }
  return out;
}

/** What the page keeps of the server's `web_prefs`: known fields of the right type. */
export function validateDashboard(raw: unknown): DashboardPrefs {
  const out: DashboardPrefs = { keys: true, panel: null, last: null };
  const o = record(raw);
  if (!o) return out;
  if (typeof o.keys === "boolean") out.keys = o.keys;
  if (finite(o.panel) && o.panel >= 240 && o.panel <= 4000) out.panel = Math.round(o.panel);
  if (typeof o.last === "string" && WORKSPACE_ID.test(o.last)) out.last = o.last;
  return out;
}

/** Both at once, as the page shows them. */
export function validate(raw: unknown): Prefs {
  const { keys, panel } = validateDashboard(raw);
  return { ...validateLayout(raw), keys, panel };
}

type Caller = (method: string, params?: unknown) => Promise<unknown>;
type Timers = { setTimeout(fn: () => void, ms: number): number; clearTimeout(id: number): void };

/** One place's preferences: read once, written whole after the last change. */
function createKept<T extends object>(call: Caller, check: (raw: unknown) => T, timers: Timers) {
  const [prefs, setPrefs] = createStore<T>(check(null));
  let timer: number | null = null;
  let loaded = false;
  let raw: Record<string, unknown> | null = null;

  async function load(): Promise<void> {
    const reply = (await call("web_prefs", {})) as { prefs?: unknown } | null;
    raw = record(reply?.prefs);
    setPrefs(reconcile(check(reply?.prefs)));
    loaded = true;
  }

  const write = () => void call("web_prefs", { set: structuredClone(unwrap(prefs)) }).catch(() => {});

  function schedule(): void {
    if (timer !== null) timers.clearTimeout(timer);
    timer = timers.setTimeout(() => {
      timer = null;
      write();
    }, WRITE_DELAY_MS);
  }

  function update(change: (p: T) => T): void {
    setPrefs(reconcile(check(change(structuredClone(unwrap(prefs) as T)))));
    schedule();
  }

  return {
    prefs,
    load,
    update,
    get loaded() {
      return loaded;
    },
    /** Whether what was read held `key` at all. */
    had: (key: string) => raw !== null && key in raw,
    /** What was read, unvalidated: for the defaults from before the split. */
    get raw() {
      return raw;
    },
    /** A write that is still waiting goes now (the workspace is being left). */
    flush() {
      if (timer === null) return;
      timers.clearTimeout(timer);
      timer = null;
      write();
    },
  };
}

/** A workspace's: its layout. */
export function createLayoutPrefs(call: Caller, timers: Timers = window) {
  const kept = createKept(call, validateLayout, timers);
  return {
    prefs: kept.prefs,
    load: kept.load,
    get loaded() {
      return kept.loaded;
    },
    /** `keys` and `panel` as this workspace kept them before the split. */
    legacy: () => {
      const { keys, panel } = validateDashboard(kept.raw);
      return { keys: kept.had("keys") ? keys : undefined, panel: kept.had("panel") ? panel : undefined };
    },
    flush: kept.flush,
    setPosition: (id: string, p: Point) => kept.update((x) => ({ layout: { ...x.layout, [id]: { x: Math.round(p.x), y: Math.round(p.y) } } })),
    resetLayout: () => kept.update(() => ({ layout: {} })),
  };
}

/** The operator's: shortcuts, the side panel's width, the workspace last on screen. */
export function createDashboardPrefs(call: Caller, timers: Timers = window) {
  const kept = createKept(call, validateDashboard, timers);
  return {
    prefs: kept.prefs,
    load: kept.load,
    get loaded() {
      return kept.loaded;
    },
    /** A workspace's `keys` and `panel` from before: taken for what the
     * server's file does not hold yet, once. */
    adopt(old: { keys?: boolean; panel?: number | null }): void {
      // Unread, the server's file is unknown: nothing is written over it.
      if (!kept.loaded) return;
      const keys = old.keys !== undefined && !kept.had("keys");
      const panel = old.panel !== undefined && old.panel !== null && !kept.had("panel");
      if (!keys && !panel) return;
      kept.update((x) => ({ ...x, ...(keys ? { keys: old.keys } : {}), ...(panel ? { panel: old.panel } : {}) }));
    },
    setKeys: (on: boolean) => kept.update((x) => ({ ...x, keys: on })),
    setPanel: (px: number | null) => kept.update((x) => ({ ...x, panel: px })),
    /** Written by the page itself, so only over what was read. */
    setLast: (id: string) => kept.loaded && kept.prefs.last !== id && kept.update((x) => ({ ...x, last: id })),
  };
}

export type LayoutPrefsApi = ReturnType<typeof createLayoutPrefs>;
export type DashboardPrefsApi = ReturnType<typeof createDashboardPrefs>;

/** What the views use: one object over both places. */
export function joinPrefs(layout: LayoutPrefsApi, dashboard: DashboardPrefsApi) {
  const prefs: Prefs = {
    get layout() {
      return layout.prefs.layout;
    },
    get keys() {
      return dashboard.prefs.keys;
    },
    get panel() {
      return dashboard.prefs.panel;
    },
  };
  return {
    prefs,
    get loaded() {
      return layout.loaded;
    },
    setPosition: layout.setPosition,
    resetLayout: layout.resetLayout,
    setKeys: dashboard.setKeys,
    setPanel: dashboard.setPanel,
  };
}

export type PrefsApi = ReturnType<typeof joinPrefs>;
