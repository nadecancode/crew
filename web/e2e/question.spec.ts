// Questions for the operator (notes/web-design.md §ui "Questions"): a
// question push shows the banner, a `?` on the asking agent's node and the
// count in the page title, and opens its dialog by itself once nothing else
// holds the keyboard; `1`–`6` pick a suggested answer, the text field sends
// the operator's own words; `esc` puts it off and `a` reopens it; an answered
// question leaves the banner with the call and stays gone.

import type { Page } from "@playwright/test";
import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
import type { Mock } from "./fixtures.ts";

const TITLE = "crew · checkout-rewrite";
const node = (page: Page, id: string) => page.locator(`[data-agent="${id}"]`);
const banner = (page: Page) => page.locator(".question-banner");
const dialog = (page: Page) => page.getByRole("dialog", { name: "question" });

/** A question for the operator, as an agent's `send` makes it. */
function question(mock: Mock, id: string, from: string, body: string, options: string[], askedBy?: string) {
  return {
    kind: "message",
    data: {
      id,
      from,
      to: "operator",
      type: "question",
      body,
      timestamp: mock.now(),
      correlation_id: null,
      reply_to: null,
      metadata: { options, ...(askedBy ? { asked_by: askedBy } : {}) },
    },
  };
}

/** Let the page finish what a push started (its microtasks and one frame). */
const frame = (page: Page) => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => done(null))));

test("a question push shows the banner, the ? on the asking node and the title count, and opens its dialog; a number picks an answer", async ({ page, mock, csp, openPage }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  await expect(page).toHaveTitle(TITLE);
  await expect(banner(page)).toHaveCount(0);

  // The orchestrator asks for claude-2: the badge is on claude-2's node.
  mock.push(mock.pushes("m-9001")[0]);
  await expect(banner(page)).toContainText("orchestrator, for claude-2 asks: The legacy export writes amounts as integer cents");
  await expect(node(page, "claude-2").locator(".badge.ask")).toBeVisible();
  await expect(node(page, "claude-3").locator(".badge.ask")).toHaveCount(0);
  await expect(page).toHaveTitle(`(1) ${TITLE}`);

  const d = dialog(page);
  await expect(d).toBeVisible();
  await expect(d.locator(".asker")).toContainText("orchestrator, for claude-2");
  await expect(d).toContainText("Which do you want?");
  await expect(d.getByRole("button", { name: /Convert in the adapter/ })).toBeFocused();

  await page.keyboard.press("2");
  expect((await mock.waitForCall("answer")).params).toEqual({ id: "m-9001", body: "Change the export and update the dashboards" });
  await expect(d).toHaveCount(0);
  // The mock pushes the operator's answer: the question is gone everywhere.
  await expect(banner(page)).toHaveCount(0);
  await expect(node(page, "claude-2").locator(".badge.ask")).toHaveCount(0);
  await expect(page).toHaveTitle(TITLE);

  expect(guard.requests).toEqual([]);
  expect(csp).toEqual([]);
});

test("esc puts the question off (the banner stays), a reopens it, and an own answer is sent with Enter", async ({ page, mock, csp, openPage }) => {
  await openPage();
  mock.push(mock.pushes("m-9001")[0]);
  const d = dialog(page);
  await expect(d).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(d).toHaveCount(0);
  await expect(banner(page)).toBeVisible();
  await expect(page).toHaveTitle(`(1) ${TITLE}`);
  // Put off, it does not pop up again by itself when the focus moves.
  await node(page, "claude-5").locator(".node-head").click();
  await frame(page);
  await expect(d).toHaveCount(0);

  await page.keyboard.press("a");
  await expect(d).toBeVisible();
  const own = d.getByRole("textbox", { name: "your own answer" });
  await own.fill("Convert in the adapter, and log every mismatch");
  // A digit typed in the text field is text, not a pick.
  await own.press("End");
  await own.type(" 1");
  await own.press("Enter");
  expect((await mock.waitForCall("answer")).params).toEqual({ id: "m-9001", body: "Convert in the adapter, and log every mismatch 1" });
  expect(mock.calls.filter((c) => c.method === "answer")).toHaveLength(1);
  await expect(d).toHaveCount(0);
  await expect(banner(page)).toHaveCount(0);
  await expect(page).toHaveTitle(TITLE);

  // `a` with nothing waiting says so.
  await page.keyboard.press("a");
  await expect(page.locator("footer .status-line")).toHaveText("no question is waiting for you");
  expect(csp).toEqual([]);
});

test("the banner's answer button reopens the dialog; the later button puts it off", async ({ page, mock, csp, openPage }) => {
  await openPage();
  mock.push(mock.pushes("m-9001")[0]);
  const d = dialog(page);
  await expect(d).toBeVisible();
  await d.getByRole("button", { name: "later" }).click();
  await expect(d).toHaveCount(0);
  await banner(page).getByRole("button", { name: "answer" }).click();
  await expect(d).toBeVisible();
  // The `?` badge opens it as well.
  await page.keyboard.press("Escape");
  await expect(d).toHaveCount(0);
  await node(page, "claude-2").locator(".badge.ask").click();
  await expect(d).toBeVisible();
  await expect(d).toContainText("orchestrator, for claude-2");
  expect(csp).toEqual([]);
});

test("an answered question leaves the banner with the call and does not come back when the answer message arrives", async ({ page, mock, csp, openPage }) => {
  // The daemon answers the call; its push comes later.
  mock.on("answer", (p: { id: string; body: string }) => ({
    message: { id: "m-0103", from: "operator", to: "orchestrator", type: "answer", body: p.body, timestamp: mock.now(), correlation_id: null, reply_to: p.id, metadata: null },
    duplicate: false,
    recipients: ["orchestrator"],
  }));
  await openPage();
  mock.push(mock.pushes("m-9001")[0]);
  const d = dialog(page);
  await expect(d).toBeVisible();
  await page.keyboard.press("1");
  expect((await mock.waitForCall("answer")).params).toEqual({ id: "m-9001", body: "Convert in the adapter" });
  await expect(d).toHaveCount(0);
  await expect(page.locator("footer .status-line")).toHaveText("✓ answer sent");
  await expect(banner(page)).toHaveCount(0);
  await expect(page).toHaveTitle(TITLE);
  await frame(page);
  await expect(banner(page)).toHaveCount(0);

  mock.push(mock.retime(mock.pushes("m-0103")[0]));
  await expect(banner(page)).toHaveCount(0);
  await expect(page).toHaveTitle(TITLE);
  await expect(node(page, "claude-2").locator(".badge.ask")).toHaveCount(0);
  // And it does not come back as a dialog.
  await frame(page);
  await expect(d).toHaveCount(0);
  expect(csp).toEqual([]);
});

test("a question waits while the operator types in the message box, and opens once the box is left", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await page.keyboard.press("M");
  await expect(page.locator("#compose-text")).toBeFocused();
  mock.push(question(mock, "q-1", "claude-3", "Keep the old endpoint alive?", ["yes", "no"]));
  await expect(banner(page)).toContainText("claude-3 asks: Keep the old endpoint alive?");
  await frame(page);
  await expect(dialog(page)).toHaveCount(0);
  await expect(page.locator("#compose-text")).toBeFocused();

  await page.locator("#compose-text").press("Escape");
  await expect(dialog(page)).toBeVisible();
  // A worker that asks by itself carries the badge.
  await expect(node(page, "claude-3").locator(".badge.ask")).toBeVisible();
  expect(csp).toEqual([]);
});

test("two questions: the banner counts the rest, the dialog walks them, and each is answered in turn", async ({ page, mock, csp, openPage }) => {
  await openPage();
  mock.push(question(mock, "q-1", "claude-3", "Which ledger format?", ["decimal strings", "integer cents", "both, behind a flag"]));
  const d = dialog(page);
  await expect(d).toBeVisible();
  mock.push(question(mock, "q-2", "claude-5", "Drop the legacy index?", ["drop it", "keep it"]));
  await expect(banner(page)).toContainText("claude-3 asks: Which ledger format?");
  await expect(banner(page)).toContainText("and 1 more");
  await expect(page).toHaveTitle(`(2) ${TITLE}`);
  await expect(d.locator(".dialog-title")).toHaveText("question 1 of 2");
  await expect(node(page, "claude-3").locator(".badge.ask")).toBeVisible();
  await expect(node(page, "claude-5").locator(".badge.ask")).toBeVisible();

  await d.getByRole("button", { name: "next question" }).click();
  await expect(d.locator(".dialog-title")).toHaveText("question 2 of 2");
  await expect(d.locator(".asker")).toContainText("claude-5");
  await page.keyboard.press("2");
  expect((await mock.waitForCall("answer")).params).toEqual({ id: "q-2", body: "keep it" });
  // On to the one left.
  await expect(d.locator(".asker")).toContainText("claude-3");
  await expect(d.locator(".dialog-title")).toHaveText("question");
  await expect(page).toHaveTitle(`(1) ${TITLE}`);
  await expect(banner(page)).not.toContainText("more");
  await expect(node(page, "claude-5").locator(".badge.ask")).toHaveCount(0);

  await d.getByRole("button", { name: /integer cents/ }).click();
  expect((await mock.waitForCall("answer", (c) => c.params.id === "q-1")).params).toEqual({ id: "q-1", body: "integer cents" });
  await expect(d).toHaveCount(0);
  await expect(banner(page)).toHaveCount(0);
  await expect(page).toHaveTitle(TITLE);
  expect(csp).toEqual([]);
});

test("after a number answered one of two questions, the dialog keeps the focus and a number answers the next", async ({ page, mock, csp, openPage }) => {
  await openPage();
  mock.push(question(mock, "q-1", "claude-3", "Which ledger format?", ["decimal strings", "integer cents", "both, behind a flag"]));
  const d = dialog(page);
  await expect(d).toBeVisible();
  mock.push(question(mock, "q-2", "claude-5", "Drop the legacy index?", ["drop it", "keep it"]));
  await expect(d.locator(".dialog-title")).toHaveText("question 1 of 2");
  await expect(d.getByRole("button", { name: /decimal strings/ })).toBeFocused();

  await page.keyboard.press("3");
  expect((await mock.waitForCall("answer")).params).toEqual({ id: "q-1", body: "both, behind a flag" });
  await expect(d.locator(".asker")).toContainText("claude-5");
  await expect.poll(() => page.evaluate(() => !!document.activeElement?.closest("[role=dialog]")), { timeout: 2000 }).toBe(true);
  await page.keyboard.press("2");
  expect((await mock.waitForCall("answer", (c) => c.params.id === "q-2", 2000)).params).toEqual({ id: "q-2", body: "keep it" });
  await expect(d).toHaveCount(0);
  expect(csp).toEqual([]);
});
