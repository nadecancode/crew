import type { Page } from "@playwright/test";
import { expect, noRequestsAfterStart, test } from "./fixtures.ts";

test.use({ viewport: { width: 1440, height: 960 } });

async function menuFor(page: Page, id: string) {
  const node = page.locator(`[data-agent="${id}"]`);
  await node.focus();
  await node.locator(".node-head").click({ button: "right" });
  const menu = page.getByRole("menu", { name: `actions for ${id}`, exact: true });
  await expect(menu.getByRole("menuitem", { name: "message", exact: true })).toBeFocused();
  return { node, menu };
}

test("menu Home/End/arrows stay local; Escape returns focus without closing its inspector", async ({ page, mock, openPage, csp }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  await page.locator('[data-agent="claude-4"]').focus();
  await page.keyboard.press("Enter");
  const { node, menu } = await menuFor(page, "claude-4");
  await page.keyboard.press("End");
  await expect(menu.getByRole("menuitem", { name: "kill…", exact: true })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(menu.getByRole("menuitem", { name: "message", exact: true })).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(menu.getByRole("menuitem", { name: "kill…", exact: true })).toBeFocused();
  await page.keyboard.press("Home");
  await expect(menu.getByRole("menuitem", { name: "message", exact: true })).toBeFocused();
  for (const key of ["p", "r", "k", "s", "l", "t", "Control+k"]) await page.keyboard.press(key);
  await expect(menu).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(mock.calls.filter((c) => ["agent_pause", "agent_restart", "agent_kill", "agent_set_model"].includes(c.method))).toEqual([]);
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
  await expect(node).toBeFocused();
  await expect(page.locator('[data-inspector="claude-4"]')).toBeVisible();
  expect(guard.requests).toEqual([]); expect(csp).toEqual([]);
});

test("new prompt/restart-fresh/herdr operations retain their existing payload and confirmation", async ({ page, mock, openPage }) => {
  await openPage();
  let opened = await menuFor(page, "claude-4");
  await opened.menu.getByRole("menuitem", { name: "prompt", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "prompt for claude-4", exact: true })).toBeFocused();
  await page.keyboard.press("Escape");
  opened = await menuFor(page, "claude-4");
  await opened.menu.getByRole("menuitem", { name: "restart fresh…", exact: true }).click();
  const confirmation = page.getByRole("dialog", { name: "restart claude-4 fresh · no resume", exact: true });
  await expect(confirmation).toBeVisible();
  await expect(confirmation.getByRole("button", { name: "cancel", exact: true })).toBeFocused();
  await page.keyboard.press("Escape");
  expect(mock.calls.filter((c) => c.method === "agent_restart_fresh")).toEqual([]);
  opened = await menuFor(page, "claude-4");
  await opened.menu.getByRole("menuitem", { name: "jump to pane", exact: true }).click();
  expect((await mock.waitForCall("agent_focus")).params).toEqual({ id: "claude-4" });
});

test("menu uses its rendered dimensions to clamp within the viewport", async ({ page, openPage }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  await openPage();
  await page.locator('[data-agent="claude-4"]').focus();
  await page.locator('[data-agent="claude-4"]').evaluate((el) => el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: innerWidth - 1, clientY: innerHeight - 1 })));
  const menu = page.getByRole("menu", { name: "actions for claude-4", exact: true });
  await expect(menu).toBeVisible();
  const box = await menu.boundingBox();
  expect(box!.x).toBeGreaterThanOrEqual(8);
  expect(box!.y).toBeGreaterThanOrEqual(8);
  expect(box!.x + box!.width).toBeLessThanOrEqual(1024 - 8);
  expect(box!.y + box!.height).toBeLessThanOrEqual(768 - 8);
});

test("agent updates refocus a retained menu; removing that agent closes it", async ({ page, mock, openPage }) => {
  await openPage();
  const { menu } = await menuFor(page, "claude-4");
  await page.keyboard.press("End");
  const a = mock.state.agents.find((a) => a.id === "claude-4")!;
  mock.push({ kind: "agent", data: { ...a, paused: true } });
  await expect(menu.getByRole("menuitem", { name: "resume", exact: true })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "message", exact: true })).toBeFocused();
  mock.push({ kind: "agent_removed", data: { id: a.id } });
  await expect(menu).toHaveCount(0);
});

test("Tab and Shift+Tab close the menu and continue through page controls", async ({ page, openPage }) => {
  await openPage();
  for (const key of ["Tab", "Shift+Tab"]) {
    const { menu } = await menuFor(page, "claude-4");
    await page.keyboard.press(key);
    await expect(menu).toHaveCount(0);
    expect(await page.evaluate(() => document.activeElement !== document.body && !document.activeElement?.closest('[role="menu"]'))).toBe(true);
  }
});
