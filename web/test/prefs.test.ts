// Preferences in two places (notes/web-design.md §tests "prefs.test.ts",
// notes/web-multi.md §multi-transport "Preferences"): the layout is the
// workspace's daemon's, keys, panel and the last workspace are the server's.
import { afterEach, expect, test, vi } from "vitest";
import { createDashboardPrefs, createLayoutPrefs, joinPrefs, validate, validateDashboard, validateLayout, WRITE_DELAY_MS } from "../src/prefs";

afterEach(() => vi.useRealTimers());

const timers = { setTimeout: (f: () => void, ms: number) => setTimeout(f, ms) as unknown as number, clearTimeout: (id: number) => clearTimeout(id) };

test("fields of the wrong type and unknown keys are dropped", () => {
  expect(validate({ layout: { a: { x: 1, y: 2 }, b: { x: "1", y: 2 }, c: null, d: { x: Infinity, y: 0 } }, keys: "no", panel: 500, extra: 1 }))
    .toEqual({ layout: { a: { x: 1, y: 2 } }, keys: true, panel: 500 });
  expect(validate(null)).toEqual({ layout: {}, keys: true, panel: null });
  expect(validate([1, 2])).toEqual({ layout: {}, keys: true, panel: null });
  expect(validate({ keys: false, panel: 10 })).toEqual({ layout: {}, keys: false, panel: null });
  expect(validateLayout({ layout: { a: { x: 1, y: 2 } }, keys: false })).toEqual({ layout: { a: { x: 1, y: 2 } } });
  expect(validateDashboard({ keys: false, panel: 600, last: "946d4617aa702870", layout: {} })).toEqual({ keys: false, panel: 600, last: "946d4617aa702870" });
  expect(validateDashboard({ last: "../../etc" }).last).toBeNull();
});

test("the layout is read after hello; a change is written once, 500 ms later, as a whole, with nothing of the operator's", async () => {
  vi.useFakeTimers();
  const calls: [string, unknown][] = [];
  const p = createLayoutPrefs(async (method, params) => {
    calls.push([method, params]);
    return { prefs: { layout: { "claude-1": { x: 10, y: 20 } }, keys: false, junk: true } };
  }, timers);
  await p.load();
  expect(calls).toEqual([["web_prefs", {}]]);
  expect(p.prefs.layout["claude-1"]).toEqual({ x: 10, y: 20 });
  p.setPosition("claude-2", { x: 1.4, y: 2.6 });
  p.setPosition("claude-2", { x: 5, y: 6 });
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS - 1);
  expect(calls.length).toBe(1);
  await vi.advanceTimersByTimeAsync(1);
  expect(calls.length).toBe(2);
  expect(calls[1]).toEqual(["web_prefs", { set: { layout: { "claude-1": { x: 10, y: 20 }, "claude-2": { x: 5, y: 6 } } } }]);
});

test("the operator's are the server's: keys, panel, last; written whole, 500 ms later", async () => {
  vi.useFakeTimers();
  const calls: [string, unknown][] = [];
  const p = createDashboardPrefs(async (method, params) => {
    calls.push([method, params]);
    return { prefs: { keys: false, last: "946d4617aa702870", layout: { a: { x: 1, y: 1 } } } };
  }, timers);
  await p.load();
  expect(p.prefs).toEqual({ keys: false, panel: null, last: "946d4617aa702870" });
  p.setKeys(true);
  p.setPanel(480);
  p.setLast("0b54c1d2e3f4a596");
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS);
  expect(calls.slice(1)).toEqual([["web_prefs", { set: { keys: true, panel: 480, last: "0b54c1d2e3f4a596" } }]]);
});

test("a workspace's keys and panel from before are read once, as defaults, when the server has none", async () => {
  vi.useFakeTimers();
  const server: Record<string, unknown> = { last: null };
  const writes: unknown[] = [];
  const dash = createDashboardPrefs(async (_m, params) => {
    const set = (params as { set?: Record<string, unknown> }).set;
    if (set) writes.push(set);
    return { prefs: server };
  }, timers);
  await dash.load();
  const one = createLayoutPrefs(async () => ({ prefs: { layout: {}, keys: false, panel: 700 } }), timers);
  await one.load();
  dash.adopt(one.legacy());
  expect(dash.prefs.keys).toBe(false);
  expect(dash.prefs.panel).toBe(700);
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS);
  // Only what was chosen or adopted is stored: no default the page shows.
  expect(writes).toEqual([{ keys: false, panel: 700 }]);
  // A second workspace's old values come after: the first one's stay.
  const two = createLayoutPrefs(async () => ({ prefs: { layout: {}, keys: true, panel: 300 } }), timers);
  await two.load();
  dash.adopt(two.legacy());
  expect(dash.prefs).toMatchObject({ keys: false, panel: 700 });

  // The server has them: another workspace's old values change nothing.
  const dash2 = createDashboardPrefs(async () => ({ prefs: { keys: true, panel: 300 } }), timers);
  await dash2.load();
  dash2.adopt({ keys: false, panel: 900 });
  expect(dash2.prefs).toMatchObject({ keys: true, panel: 300 });
  // Nor does a workspace that kept none.
  const none = createLayoutPrefs(async () => ({ prefs: { layout: {} } }), timers);
  await none.load();
  expect(none.legacy()).toEqual({ keys: undefined, panel: undefined });
});

test("a workspace's old values never replace what the operator chose in this document", async () => {
  vi.useFakeTimers();
  const writes: unknown[] = [];
  const dash = createDashboardPrefs(async (_m, params) => {
    const set = (params as { set?: unknown }).set;
    if (set) writes.push(set);
    return { prefs: {} };
  }, timers);
  await dash.load();
  dash.setKeys(false);
  dash.setPanel(520);
  dash.adopt({ keys: true, panel: 900 });
  expect(dash.prefs).toMatchObject({ keys: false, panel: 520 });
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS);
  expect(writes).toEqual([{ keys: false, panel: 520 }]);
});

test("the page's own write of the last workspace stores no default for keys or panel", async () => {
  vi.useFakeTimers();
  const writes: unknown[] = [];
  const dash = createDashboardPrefs(async (_m, params) => {
    const set = (params as { set?: unknown }).set;
    if (set) writes.push(set);
    return { prefs: { keys: "junk" } };
  }, timers);
  await dash.load();
  dash.setLast("946d4617aa702870");
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS);
  expect(writes).toEqual([{ last: "946d4617aa702870" }]);
  // Nothing established for keys: a workspace's old value is still taken.
  dash.adopt({ keys: false });
  expect(dash.prefs.keys).toBe(false);
});

test("a reset forgets dragged positions; the views read both places as one", async () => {
  vi.useFakeTimers();
  const layout = createLayoutPrefs(async () => ({ prefs: { layout: { a: { x: 1, y: 1 } } } }), timers);
  const dash = createDashboardPrefs(async () => ({ prefs: { keys: false } }), timers);
  await Promise.all([layout.load(), dash.load()]);
  const p = joinPrefs(layout, dash);
  expect(p.prefs.layout).toEqual({ a: { x: 1, y: 1 } });
  expect(p.prefs.keys).toBe(false);
  p.resetLayout();
  expect(p.prefs.layout).toEqual({});
});

test("leaving a workspace sends a layout write that is still waiting", async () => {
  vi.useFakeTimers();
  const calls: unknown[] = [];
  const p = createLayoutPrefs(async (_m, params) => (calls.push(params), { prefs: {} }), timers);
  await p.load();
  p.setPosition("a", { x: 3, y: 4 });
  p.flush();
  expect(calls).toEqual([{}, { set: { layout: { a: { x: 3, y: 4 } } } }]);
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS);
  expect(calls.length).toBe(2);
});
