import { expect, NO_REPLY, test } from "./fixtures.ts";

test("an uncached message opened during reconnect recovers after the verified connection returns", async ({ page, mock, openPage }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await openPage();
  const first = mock.calls.length;
  mock.on("hello", () => NO_REPLY);
  mock.on("message_get", () => ({
    id: "review-recovered", from: "orchestrator", to: "operator", type: "message",
    body: "MESSAGE FROM VERIFIED CONNECTION", timestamp: mock.now(), correlation_id: null, reply_to: null, metadata: null,
  }));
  mock.dropSockets();
  await mock.waitForCall("hello", (c) => c.seq >= first);
  await page.evaluate(() => { location.hash = "/?message=review-recovered"; });
  await page.waitForTimeout(100);
  expect(mock.calls.filter((c) => c.method === "message_get")).toEqual([]);
  mock.on("hello", null);
  mock.dropSockets();
  await expect(page.locator('[data-conn="live"]')).toBeAttached({ timeout: 5000 });
  expect(errors).toEqual([]);
  await expect(page.getByText("MESSAGE FROM VERIFIED CONNECTION")).toBeVisible();
});

test("a missing uncached message displays its error without an uncaught exception", async ({ page, mock, openPage }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  mock.on("message_get", () => { throw { code: "not_found", message: "review message is gone" }; });
  await openPage();
  await page.evaluate(() => { location.hash = "/?message=review-missing"; });
  await mock.waitForCall("message_get");
  await page.waitForTimeout(100);
  expect(errors).toEqual([]);
  await expect(page.locator(".message-panel")).toContainText("review message is gone");
});

test("a failed model listing displays its error without an uncaught exception", async ({ page, mock, openPage }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  mock.on("models_list", () => { throw { code: "unavailable", message: "review catalog is unavailable" }; });
  await openPage();
  await page.evaluate(() => { location.hash = "/?agent=claude-2"; });
  await page.getByRole("toolbar", { name: "actions for claude-2" }).getByRole("button", { name: "More", exact: true }).click();
  await page.getByRole("menu", { name: "actions for claude-2", exact: true }).getByRole("menuitem", { name: "model and CLI", exact: true }).click();
  await mock.waitForCall("models_list");
  await page.waitForTimeout(100);
  expect(errors).toEqual([]);
  await expect(page.getByRole("dialog", { name: "model and CLI" })).toContainText("review catalog is unavailable");
});

test("changing an uncached message after stop never displays another message's body", async ({ page, mock, openPage }) => {
  mock.on("message_get", (params: { id: string }) => ({
    id: params.id, from: "orchestrator", to: "operator", type: "message",
    body: `Body belonging to ${params.id}`, timestamp: mock.now(), correlation_id: null, reply_to: null, metadata: null,
  }));
  await openPage();
  await page.evaluate(() => { location.hash = "/?message=review-first"; });
  await expect(page.getByText("Body belonging to review-first")).toBeVisible();
  await mock.shutdown();
  await expect(page.locator('[data-conn="stopped"]')).toBeAttached();
  await page.evaluate(() => { location.hash = "/?message=review-second"; });
  await expect(page.locator(".message-panel .panel-title")).toHaveText("message review-second");
  await expect(page.getByText("Body belonging to review-first")).toHaveCount(0);
  await expect(page.locator(".message-panel")).toContainText("not connected");
});
