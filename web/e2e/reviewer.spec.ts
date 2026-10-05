import { expect, NO_REPLY, test } from "./fixtures.ts";

test("question shortcuts cannot send before a reconnected server proves itself", async ({ page, mock, openPage }) => {
  await openPage();
  mock.push(mock.pushes("m-9001")[0]);
  const dialog = page.getByRole("dialog", { name: "question" });
  await expect(dialog).toBeVisible();

  const first = mock.calls.length;
  mock.on("hello", () => NO_REPLY);
  mock.dropSockets();
  await mock.waitForCall("hello", (c) => c.seq >= first);
  await expect(page.locator('[data-conn="reconnecting"]')).toBeAttached();
  await expect(dialog.locator(".options button").first()).toBeDisabled();
  await dialog.getByRole("button", { name: "later", exact: true }).focus();
  await page.keyboard.press("1");
  await page.waitForTimeout(150);
  expect(mock.calls.filter((c) => c.method === "answer")).toEqual([]);
});

test("a resync snapshot timeout does not leave stale state marked live", async ({ page, mock, openPage }) => {
  await openPage();
  (mock.state.agents[1] as { activity: string }).activity = "Updated during the lag";
  (mock.state.agents[1] as { status: string }).status = "working";
  const release = mock.holdSnapshot();
  const first = mock.calls.length;
  mock.push({ kind: "resync" });
  await mock.waitForCall("snapshot", (c) => c.seq >= first);
  await page.waitForTimeout(3300);
  release();
  await expect(page.locator('[data-agent="claude-2"] .doing .what')).toHaveText("▸ Updated during the lag", { timeout: 8000 });
});

test("changing terminal agent through history after stop creates no connection", async ({ page, mock, openPage }) => {
  for (const agent of mock.state.agents) (agent as { backend: string }).backend = "pty";
  await openPage();
  await page.evaluate(() => { location.hash = "/?agent=claude-1"; });
  await page.getByRole("tab", { name: "terminal", exact: true }).click();
  await mock.waitForCall("attach");
  await expect(page.locator(".xterm-screen")).toBeVisible();
  await mock.shutdown();
  await expect(page.locator('[data-conn="stopped"]')).toBeAttached();
  await page.waitForTimeout(100);
  const before = mock.requests.length;
  await page.evaluate(() => { location.hash = "/?agent=claude-3"; });
  await expect(page.locator('[data-inspector="claude-3"]')).toBeVisible();
  await page.waitForTimeout(300);
  expect(mock.requests.slice(before)).toEqual([]);
});

test("a full message ring preserves the visible message when its oldest row leaves", async ({ page, mock, openPage }) => {
  const make = (i: number) => ({
    kind: "message",
    data: { id: `review-${i}`, from: "claude-1", to: "claude-3", type: "message", body: `Review row ${i}`,
      timestamp: mock.now() + i, correlation_id: null, reply_to: null, metadata: null },
  });
  await openPage();
  for (let i = 0; i < 1050; i++) mock.push(make(i));
  await expect(page.locator('[data-message="review-1049"]')).toBeAttached();
  const list = page.locator(".bus-list");
  await list.evaluate((el) => { el.scrollTop = 400 * 22; el.dispatchEvent(new Event("scroll")); });
  const firstVisible = () => list.evaluate((el) => {
    const edge = el.getBoundingClientRect().top;
    return [...el.querySelectorAll<HTMLElement>("[data-message]")].find((row) => row.getBoundingClientRect().top >= edge)?.dataset.message;
  });
  const anchor = await firstVisible();
  expect(anchor).toBeTruthy();
  mock.push(make(1050));
  await page.waitForTimeout(100);
  expect(await firstVisible()).toBe(anchor);
});

test("an unproven reconnect cannot feed the message panel fabricated content", async ({ page, mock, openPage }) => {
  await openPage();
  const first = mock.calls.length;
  mock.on("hello", () => NO_REPLY);
  mock.on("message_get", () => ({
    id: "review-not-in-cache", from: "orchestrator", to: "operator", type: "message",
    body: "FABRICATED BY UNPROVEN SERVER", timestamp: mock.now(), correlation_id: null, reply_to: null, metadata: null,
  }));
  mock.dropSockets();
  await mock.waitForCall("hello", (c) => c.seq >= first);
  await page.evaluate(() => { location.hash = "/?message=review-not-in-cache"; });
  await page.waitForTimeout(200);
  await expect(page.getByText("FABRICATED BY UNPROVEN SERVER")).toHaveCount(0);
  expect(mock.calls.filter((c) => c.method === "message_get")).toEqual([]);
});

test("a rejected prompt keeps the operator's text for retry", async ({ page, mock, openPage }) => {
  mock.on("agent_input", () => { throw { code: "unavailable", message: "agent unavailable" }; });
  await openPage();
  await page.evaluate(() => { location.hash = "/?agent=claude-2"; });
  await page.getByRole("toolbar", { name: "actions for claude-2" }).getByRole("button", { name: "More", exact: true }).click();
  await page.getByRole("menu", { name: "actions for claude-2", exact: true }).getByRole("menuitem", { name: "prompt", exact: true }).click();
  const box = page.locator("#compose-text");
  await box.fill("Carefully drafted instructions that were never delivered");
  await box.press("Enter");
  await mock.waitForCall("agent_input");
  await expect(page.locator("footer .status-line")).toContainText("agent unavailable");
  await expect(box).toHaveValue("Carefully drafted instructions that were never delivered");
});
