// The dashboard's preferences: kept by the daemon (`web_prefs`), not by the
// browser, since the page's origin is new at every server start
// (notes/web-design.md §transport "Preferences live in the daemon"). Read
// after `hello`; every field the page uses is validated, unknown keys and
// wrong types are dropped; the whole object is written back 500 ms after the
// last change. Two tabs: the last write wins.

import { createStore, reconcile, unwrap } from "solid-js/store";

export type Point = { x: number; y: number };
export type Prefs = {
  /** Where the operator dragged agents on the graph, by id. */
  layout: Record<string, Point>;
  /** Single-key shortcuts (`/keys off` turns them off). */
  keys: boolean;
  /** Width of the side panel, in pixels. */
  panel: number | null;
};

export const DEFAULT_PREFS: Prefs = { layout: {}, keys: true, panel: null };
export const WRITE_DELAY_MS = 500;
const MAX_LAYOUT = 512;

const finite = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && Math.abs(v) < 1e6;

/** What the page keeps of whatever the daemon holds: known fields of the right type. */
export function validate(raw: unknown): Prefs {
  const out: Prefs = { layout: {}, keys: true, panel: null };
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return out;
  const o = raw as Record<string, unknown>;
  if (typeof o.layout === "object" && o.layout !== null && !Array.isArray(o.layout)) {
    for (const [id, p] of Object.entries(o.layout as Record<string, unknown>).slice(0, MAX_LAYOUT)) {
      if (typeof p === "object" && p !== null && finite((p as Point).x) && finite((p as Point).y) && id.length <= 128) {
        out.layout[id] = { x: (p as Point).x, y: (p as Point).y };
      }
    }
  }
  if (typeof o.keys === "boolean") out.keys = o.keys;
  if (finite(o.panel) && o.panel >= 240 && o.panel <= 4000) out.panel = Math.round(o.panel);
  return out;
}

type Caller = (method: string, params?: unknown) => Promise<unknown>;
type Timers = { setTimeout(fn: () => void, ms: number): number; clearTimeout(id: number): void };

export function createPrefs(call: Caller, timers: Timers = window) {
  const [prefs, setPrefs] = createStore<Prefs>(structuredClone(DEFAULT_PREFS));
  let timer: number | null = null;
  let loaded = false;

  /** After `hello`: read what the daemon keeps. */
  async function load(): Promise<void> {
    const reply = (await call("web_prefs", {})) as { prefs?: unknown } | null;
    setPrefs(reconcile(validate(reply?.prefs)));
    loaded = true;
  }

  function schedule(): void {
    if (timer !== null) timers.clearTimeout(timer);
    timer = timers.setTimeout(() => {
      timer = null;
      void call("web_prefs", { set: structuredClone(unwrap(prefs)) }).catch(() => {});
    }, WRITE_DELAY_MS);
  }

  function update(change: (p: Prefs) => Prefs): void {
    setPrefs(reconcile(validate(change(structuredClone(unwrap(prefs))))));
    schedule();
  }

  return {
    prefs,
    load,
    get loaded() {
      return loaded;
    },
    setPosition: (id: string, p: Point) => update((x) => ({ ...x, layout: { ...x.layout, [id]: { x: Math.round(p.x), y: Math.round(p.y) } } })),
    resetLayout: () => update((x) => ({ ...x, layout: {} })),
    setKeys: (on: boolean) => update((x) => ({ ...x, keys: on })),
    setPanel: (px: number | null) => update((x) => ({ ...x, panel: px })),
  };
}

export type PrefsApi = ReturnType<typeof createPrefs>;
