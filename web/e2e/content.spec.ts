import { expect, noRequestsAfterStart, setPanelRead, test } from "./fixtures.ts";
import { SCENARIO_TIME } from "../dev/scenarios.ts";
import { STALE_MS } from "../src/views/usage";

test.use({ mockOptions: { scenario: "busy", fixture: "subagents" }, viewport: { width: 1440, height: 960 } });

test("pending task reads keep cached title/evidence and cannot overwrite a newer task push", async ({ page, mock, openPage, csp }) => {
  const guard = noRequestsAfterStart(page, mock);
  const release = setPanelRead(mock, "task_get", "pending");
  const task = mock.state.tasks.find((t) => t.id === "scenario-done")!;
  try {
    await openPage();
    await page.evaluate((id) => { location.hash = `#/w/${location.hash.split("/")[2]}/tasks?task=${id}`; }, task.id);
    const panel = page.locator(".task-panel");
    await mock.waitForCall("task_get");
    await expect(panel).toHaveAttribute("aria-busy", "true");
    await expect(panel.locator(".task-title")).toHaveText(task.title);
    await expect(panel).toContainText(task.result);
    await expect(panel.getByRole("status")).toContainText("Loading task details…");
    mock.push({ kind: "task", data: { ...task, title: "Newer task title from a live push" } });
    await expect(panel.locator(".task-title")).toHaveText("Newer task title from a live push");
    release();
    await expect(panel).toHaveAttribute("aria-busy", "false");
    await expect(panel.locator(".task-title")).toHaveText("Newer task title from a live push");
    expect(guard.requests).toEqual([]); expect(csp).toEqual([]);
  } finally { release(); }
});

test("failed task dependency reads retain cached content with a local error", async ({ page, mock, openPage, csp }) => {
  const release = setPanelRead(mock, "task_get", "error");
  const task = mock.state.tasks.find((t) => t.id === "scenario-done")!;
  try {
    await openPage();
    await page.evaluate((id) => { location.hash = `#/w/${location.hash.split("/")[2]}/tasks?task=${id}`; }, task.id);
    const panel = page.locator(".task-panel");
    await expect(panel.locator(".task-title")).toHaveText(task.title);
    await expect(panel.getByRole("status")).toContainText("Mock task_get read failed.");
    await expect(panel).toHaveAttribute("aria-busy", "false");
    await expect(panel).toContainText(task.result);
    await expect(panel.getByRole("button", { name: task.dependencies[0], exact: true })).toBeVisible();
    expect(csp).toEqual([]);
  } finally { release(); }
});

test("task artifact copy sends every machine value to the clipboard API", async ({ page, mock, openPage }) => {
  await page.addInitScript(() => {
    (window as unknown as { __copies: string[] }).__copies = [];
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (value: string) => { (window as unknown as { __copies: string[] }).__copies.push(value); } } });
  });
  await openPage();
  const task = mock.state.tasks.find((t) => t.id === "scenario-done")!;
  await page.evaluate((id) => { location.hash = `#/w/${location.hash.split("/")[2]}/tasks?task=${id}`; }, task.id);
  for (const artifact of task.artifacts) await page.locator(".task-panel").getByRole("button", { name: artifact.value, exact: true }).click();
  expect(await page.evaluate(() => (window as unknown as { __copies: string[] }).__copies)).toEqual(task.artifacts.map((a) => a.value));
});

test("usage preserves explicit zero reports and unknown reports in separate rows", async ({ page, openPage }) => {
  await openPage(); await page.keyboard.press("v");
  const table = page.locator(".usage-table");
  const zero = table.getByRole("row").filter({ has: page.getByRole("button", { name: "π claude-4", exact: true }) });
  const missing = table.getByRole("row").filter({ has: page.getByRole("button", { name: "✱ claude-3", exact: true }) });
  await expect(zero.getByRole("cell").nth(2)).toHaveText("0");
  await expect(zero.getByRole("cell").nth(2).getByRole("img")).toHaveAttribute("aria-label", "input 0 · output 0 · cache read 0 · cache written 0");
  await expect(missing.getByRole("cell").nth(2)).toHaveText("not reported");
  await expect(missing.getByRole("cell").nth(2).getByRole("img")).toHaveCount(0);
});

test("quota age marks stale only after the freshness boundary", async ({ page, mock, openPage }) => {
  await page.clock.setFixedTime(SCENARIO_TIME);
  mock.state.session.quotas = [
    { runtime: "claude", window: "5h", percent_used: 25, updated_at: SCENARIO_TIME - STALE_MS, resets_at: null },
    { runtime: "claude", window: "week", percent_used: 60, updated_at: SCENARIO_TIME - STALE_MS - 1, resets_at: null },
  ];
  await openPage(); await page.keyboard.press("v");
  const windows = page.locator(".usage-view .window");
  await expect(windows).toHaveCount(2);
  await expect(windows.nth(0)).not.toHaveClass(/stale/);
  await expect(windows.nth(0)).toContainText("Reported");
  await expect(windows.nth(1)).toHaveClass(/stale/);
  await expect(windows.nth(1)).toContainText("Stale · Reported");
});

test("one real context sample appears as a visible point without a fabricated interval", async ({ page, mock, openPage }) => {
  for (const a of mock.state.agents) a.context = a.id === "claude-1" ? { tokens: 1000, window: 2000, percent: 50, updated_at: mock.now() } : null;
  await openPage(); await page.keyboard.press("v");
  const point = page.locator(".lines-svg .ctx-line.point");
  await expect(point).toHaveCount(1);
  await expect(point.locator("title")).toHaveText("claude-1: 50% now");
  expect(await point.evaluate((el) => getComputedStyle(el).strokeLinecap)).toBe("round");
  expect(await point.evaluate((el: SVGPathElement) => el.getTotalLength())).toBe(0);
});
