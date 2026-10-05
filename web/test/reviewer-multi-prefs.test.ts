// Regression coverage for preference migration across workspace switches.
import { afterEach, expect, test, vi } from "vitest";
import { createDashboardPrefs, WRITE_DELAY_MS } from "../src/prefs";

afterEach(() => vi.useRealTimers());

const timers = {
  setTimeout: (f: () => void, ms: number) => setTimeout(f, ms) as unknown as number,
  clearTimeout: (id: number) => clearTimeout(id),
};

test("legacy defaults from another workspace do not overwrite preferences already adopted in this document", async () => {
  vi.useFakeTimers();
  const writes: unknown[] = [];
  const prefs = createDashboardPrefs(async (_method, params) => {
    const set = (params as { set?: unknown }).set;
    if (set) writes.push(set);
    return { prefs: {} };
  }, timers);
  await prefs.load();
  prefs.adopt({ keys: false, panel: 700 });
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS);
  prefs.adopt({ keys: true, panel: 300 });
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS);
  expect(prefs.prefs).toMatchObject({ keys: false, panel: 700 });
  expect(writes).toEqual([{ keys: false, panel: 700, last: null }]);
});

test("explicit operator preferences win over a later workspace's legacy defaults", async () => {
  vi.useFakeTimers();
  const prefs = createDashboardPrefs(async () => ({ prefs: {} }), timers);
  await prefs.load();
  prefs.setKeys(false);
  prefs.setPanel(850);
  await vi.advanceTimersByTimeAsync(WRITE_DELAY_MS);
  prefs.adopt({ keys: true, panel: 300 });
  expect(prefs.prefs).toMatchObject({ keys: false, panel: 850 });
});

test("remembering the last workspace does not suppress migration of either missing field", async () => {
  vi.useFakeTimers();
  const prefs = createDashboardPrefs(async () => ({ prefs: {} }), timers);
  await prefs.load();
  prefs.setLast("946d4617aa702870");
  prefs.adopt({ keys: false });
  prefs.adopt({ panel: 700 });
  expect(prefs.prefs).toEqual({ keys: false, panel: 700, last: "946d4617aa702870", theme: "system" });
});
