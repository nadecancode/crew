// Preferences kept by the daemon (notes/web-design.md §tests "prefs.test.ts").
import { afterEach, expect, test, vi } from "vitest";
import { createPrefs, validate, WRITE_DELAY_MS } from "../src/prefs";

afterEach(() => vi.useRealTimers());

test("fields of the wrong type and unknown keys are dropped", () => {
  expect(validate({ layout: { a: { x: 1, y: 2 }, b: { x: "1", y: 2 }, c: null, d: { x: Infinity, y: 0 } }, keys: "no", panel: 500, extra: 1 }))
    .toEqual({ layout: { a: { x: 1, y: 2 } }, keys: true, panel: 500 });
  expect(validate(null)).toEqual({ layout: {}, keys: true, panel: null });
  expect(validate([1, 2])).toEqual({ layout: {}, keys: true, panel: null });
  expect(validate({ keys: false, panel: 10 })).toEqual({ layout: {}, keys: false, panel: null });
});

test("preferences are read after hello; a change is written once, 500 ms later, as a whole", async () => {
  vi.useFakeTimers();
  const calls: [string, unknown][] = [];
  const p = createPrefs(async (method, params) => {
    calls.push([method, params]);
    return { prefs: { layout: { "claude-1": { x: 10, y: 20 } }, keys: false, junk: true } };
  }, { setTimeout: (f, ms) => setTimeout(f, ms) as unknown as number, clearTimeout: (id) => clearTimeout(id) });
  await p.load();
  expect(calls).toEqual([["web_prefs", {}]]);
  expect(p.prefs.keys).toBe(false);
  expect(p.prefs.layout["claude-1"]).toEqual({ x: 10, y: 20 });
  p.setPosition("claude-2", { x: 1.4, y: 2.6 });
  p.setPosition("claude-2", { x: 5, y: 6 });
  p.setKeys(true);
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS - 1);
  expect(calls.length).toBe(1);
  await vi.advanceTimersByTimeAsync(1);
  expect(calls.length).toBe(2);
  expect(calls[1]).toEqual(["web_prefs", { set: { layout: { "claude-1": { x: 10, y: 20 }, "claude-2": { x: 5, y: 6 } }, keys: true, panel: null } }]);
});

test("a reset forgets dragged positions", async () => {
  vi.useFakeTimers();
  const p = createPrefs(async () => ({ prefs: { layout: { a: { x: 1, y: 1 } } } }), { setTimeout: (f, ms) => setTimeout(f, ms) as unknown as number, clearTimeout: (id) => clearTimeout(id) });
  await p.load();
  p.resetLayout();
  expect(p.prefs.layout).toEqual({});
});
