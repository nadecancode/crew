// The task DAG (notes/web-design.md §graphs "Task DAG", §ui "Tasks"): `t`
// shows it, layers left to right by longest path; hovering a task keeps what
// it builds on and what waits for it lit and dims the rest; a click opens its
// card in the side panel (`task_get`); arrows and `h` `j` `k` `l` move the
// focus along and across layers, `enter` opens the card; a task that changes
// state flashes its outline once.
//
// The fixture: db and queue have no dependencies; ledger and search build on
// db, notify on queue; billing on ledger and queue.

import type { Page } from "@playwright/test";
import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
import type { Mock } from "./fixtures.ts";

const task = (page: Page, id: string) => page.locator(`[data-task="${id}"]`);
const IDS = ["db", "queue", "ledger", "search", "notify", "billing"];

/** Every task's box, read in one go once the view has settled (the DAG is
 * fitted to the window as it first draws: boxes read across that fit would
 * mix two scales). */
async function boxes(page: Page): Promise<Record<string, { x: number; y: number }>> {
  const read = () =>
    page.evaluate((ids) => {
      const out: Record<string, { x: number; y: number }> = {};
      for (const id of ids) {
        const b = document.querySelector(`[data-task="${id}"] rect.box`)!.getBoundingClientRect();
        out[id] = { x: Math.round(b.x), y: Math.round(b.y) };
      }
      return out;
    }, IDS);
  let last = JSON.stringify(await read());
  for (;;) {
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
    const now = await read();
    if (JSON.stringify(now) === last) return now;
    last = JSON.stringify(now);
  }
}

const focused = (page: Page) => page.evaluate(() => document.activeElement?.getAttribute("data-task") ?? null);
const dimmed = (page: Page) => page.locator(".task.dim").evaluateAll((els) => els.map((e) => e.getAttribute("data-task")).sort());

/** A `task` push: the mock's task with `patch` applied. */
function taskPush(mock: Mock, id: string, patch: Record<string, unknown>) {
  return { kind: "task", data: { ...structuredClone(mock.state.tasks.find((t) => t.id === id)!), ...patch } };
}

test.use({ viewport: { width: 1400, height: 900 } });

test("t shows the DAG in layers, left to right; t again goes back to the graph", async ({ page, mock, csp, openPage }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  await page.keyboard.press("t");
  await expect(page).toHaveURL(/#\/w\/[0-9a-f]{16}\/tasks$/);
  for (const id of IDS) await expect(task(page, id)).toBeVisible();
  await expect(task(page, "billing")).toHaveAccessibleName("billing, working");
  await expect(page.locator(".task-dag path.edge")).toHaveCount(5);

  const b = await boxes(page);
  // One column per layer.
  expect(b.db.x).toBe(b.queue.x);
  expect(b.ledger.x).toBe(b.search.x);
  expect(b.search.x).toBe(b.notify.x);
  expect(b.db.x).toBeLessThan(b.ledger.x);
  expect(b.ledger.x).toBeLessThan(b.billing.x);

  await page.keyboard.press("t");
  await expect(page.locator(".task-dag")).toHaveCount(0);
  await expect(page.locator('[data-agent="claude-1"]')).toBeVisible();
  expect(guard.requests).toEqual([]);
  expect(csp).toEqual([]);
});

test("hovering a task dims what it neither builds on nor is waited for by", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await page.keyboard.press("t");
  await expect(task(page, "db")).toBeVisible();
  await expect(page.locator(".task.dim")).toHaveCount(0);

  await task(page, "ledger").hover();
  // ledger builds on db; billing waits for it.
  await expect(page.locator(".task.dim")).toHaveCount(3);
  expect(await dimmed(page)).toEqual(["notify", "queue", "search"]);
  await expect(page.locator(".task-dag path.edge.lit")).toHaveCount(2);

  await task(page, "db").hover();
  await expect(page.locator(".task.dim")).toHaveCount(2);
  expect(await dimmed(page)).toEqual(["notify", "queue"]);

  await task(page, "notify").hover();
  await expect(page.locator(".task.dim")).toHaveCount(4);
  expect(await dimmed(page)).toEqual(["billing", "db", "ledger", "search"]);

  await page.mouse.move(3, 400);
  await expect(page.locator(".task.dim")).toHaveCount(0);
  expect(csp).toEqual([]);
});

test("a click opens the task's card in the side panel and asks task_get", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await page.keyboard.press("t");
  await task(page, "ledger").click();
  expect((await mock.waitForCall("task_get")).params).toEqual({ id: "ledger" });
  const panel = page.locator("#side-panel");
  await expect(panel.locator(".panel-title")).toHaveText("task ledger");
  await expect(panel).toContainText("implement ledger");
  await expect(panel.locator(".deps")).toContainText("db");
  await expect(task(page, "ledger")).toHaveClass(/\bselected\b/);
  await expect(page).toHaveURL(/#\/w\/[0-9a-f]{16}\/tasks\?task=ledger$/);

  // The card's dependency link moves the card.
  await panel.locator(".deps").getByRole("button", { name: "db" }).click();
  expect((await mock.waitForCall("task_get", (c) => c.params.id === "db")).params).toEqual({ id: "db" });
  await expect(panel.locator(".panel-title")).toHaveText("task db");

  // A `task` push for it asks again.
  const before = mock.calls.filter((c) => c.method === "task_get").length;
  mock.push(taskPush(mock, "db", { note: "schema migrated" }));
  await expect.poll(() => mock.calls.filter((c) => c.method === "task_get").length).toBe(before + 1);
  await expect(panel).toContainText("schema migrated");
  expect(csp).toEqual([]);
});

test("the keyboard walks the DAG: j k and arrows along a layer, h l and arrows across, enter opens the card", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await page.keyboard.press("t");
  await expect(task(page, "db")).toBeVisible();
  const b = await boxes(page);
  const layers = [["db", "queue"], ["ledger", "search", "notify"], ["billing"]].map((l) => l.sort((x, y) => b[x].y - b[y].y));

  // One tab stop: the first task of the first layer.
  await expect(page.locator("[data-task][tabindex='0']")).toHaveCount(1);
  await page.locator("[data-task][tabindex='0']").focus();
  expect(await focused(page)).toBe(layers[0][0]);

  await page.keyboard.press("j");
  await expect.poll(() => focused(page)).toBe(layers[0][1]);
  await page.keyboard.press("j");
  await expect.poll(() => focused(page)).toBe(layers[0][1]);
  await page.keyboard.press("k");
  await expect.poll(() => focused(page)).toBe(layers[0][0]);
  await page.keyboard.press("l");
  await expect.poll(() => focused(page)).toBe(layers[1][0]);
  await page.keyboard.press("ArrowDown");
  await expect.poll(() => focused(page)).toBe(layers[1][1]);
  await page.keyboard.press("ArrowDown");
  await expect.poll(() => focused(page)).toBe(layers[1][2]);
  await page.keyboard.press("ArrowUp");
  await expect.poll(() => focused(page)).toBe(layers[1][1]);
  await page.keyboard.press("ArrowRight");
  await expect.poll(() => focused(page)).toBe("billing");
  await page.keyboard.press("h");
  await expect.poll(() => focused(page)).toBe(layers[1][0]);
  await page.keyboard.press("ArrowLeft");
  await expect.poll(() => focused(page)).toBe(layers[0][0]);
  // The view keys did not fire on the way (`l` is also the log view's key).
  await expect(page).toHaveURL(/#\/w\/[0-9a-f]{16}\/tasks$/);
  expect(mock.calls.filter((c) => c.method === "agent_kill")).toEqual([]);
  await expect(page.getByRole("dialog")).toHaveCount(0);

  await page.keyboard.press("l");
  await page.keyboard.press("l");
  await expect.poll(() => focused(page)).toBe("billing");
  await page.keyboard.press("Enter");
  expect((await mock.waitForCall("task_get")).params).toEqual({ id: "billing" });
  await expect(page.locator("#side-panel .panel-title")).toHaveText("task billing");
  await expect(task(page, "billing")).toHaveAttribute("tabindex", "0");
  expect(csp).toEqual([]);
});

test("a task that changes state flashes its outline once", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await page.keyboard.press("t");
  await expect(task(page, "billing")).toBeVisible();
  await expect(page.locator(".task .flash")).toHaveCount(0);

  // A push that changes nothing but the note does not flash.
  mock.push(taskPush(mock, "billing", { note: "half way" }));
  await expect(task(page, "billing")).toContainText("billing");
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => done(null))));
  await expect(page.locator(".task .flash")).toHaveCount(0);

  mock.push(taskPush(mock, "billing", { status: "review", reviewer: "claude-3" }));
  await expect(task(page, "billing").locator(".flash")).toBeAttached();
  await expect(task(page, "billing")).toHaveAccessibleName("billing, review");
  await expect(task(page, "billing")).toContainText("review: claude-3");
  await expect(page.locator(".task .flash")).toHaveCount(1);
  // Once: it is gone after the sweep.
  await expect(task(page, "billing").locator(".flash")).toHaveCount(0, { timeout: 3000 });

  mock.push(taskPush(mock, "billing", { status: "done" }));
  await expect(task(page, "billing").locator(".flash")).toBeAttached();
  await expect(task(page, "billing")).toHaveClass(/ts-done/);
  await expect(task(page, "billing").locator(".flash")).toHaveCount(0, { timeout: 3000 });
  expect(csp).toEqual([]);
});

test("in the tasks view, j with the focus outside the DAG moves the focus one task along the layer", async ({ page, mock, csp, openPage }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await openPage();
  await page.keyboard.press("t");
  await expect(task(page, "ledger")).toBeVisible();
  const b = await boxes(page);
  const middle = ["ledger", "search", "notify"].sort((x, y) => b[x].y - b[y].y);
  await task(page, middle[0]).click();
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  expect(await focused(page)).toBeNull();

  await page.keyboard.press("j");
  await expect.poll(() => focused(page), { timeout: 2000 }).toBe(middle[1]);
  expect(errors).toEqual([]);
  expect(csp).toEqual([]);
});
