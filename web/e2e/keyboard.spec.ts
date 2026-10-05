// The page with the keyboard alone (notes/web-design.md §ui "Focus and keys",
// §tests "keyboard.spec.ts").
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures.ts";

test.use({ viewport: { width: 1400, height: 860 } });

/** Where focus is, in words a test can compare. */
const where = (page: Page) =>
  page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el || el === document.body) return "body";
    const agent = el.getAttribute("data-agent");
    if (agent) return `node:${agent}`;
    if (el.id === "compose-text") return "compose";
    const region = el.closest("[data-region]")?.getAttribute("data-region") ?? "?";
    const name = el.getAttribute("aria-label") || el.getAttribute("role") || el.textContent?.trim().slice(0, 20) || el.tagName;
    return `${region}:${name}`;
  });

async function walk(page: Page, key: "Tab" | "Shift+Tab", until: (w: string) => boolean, max = 60): Promise<string[]> {
  const seen: string[] = [];
  for (let i = 0; i < max; i++) {
    await page.keyboard.press(key);
    const w = await where(page);
    seen.push(w);
    if (until(w)) return seen;
  }
  throw new Error(`never reached the end: ${seen.join(" → ")}`);
}

test("tab goes from the status bar through the graph (one stop) and the side panel to the message box, and back", async ({ page, openPage, csp }) => {
  await openPage();
  await page.getByRole("navigation", { name: "views" }).getByRole("button", { name: "Team", exact: true }).focus();
  const forward = await walk(page, "Tab", (w) => w === "compose");
  const nodes = forward.filter((w) => w.startsWith("node:"));
  expect(nodes).toEqual(["node:claude-1"]);
  const iNode = forward.indexOf("node:claude-1");
  expect(forward.findIndex((w) => w.startsWith("status:"))).toBeLessThan(iNode);
  expect(forward.findIndex((w) => w.startsWith("panel:"))).toBeGreaterThan(iNode);
  const back = await walk(page, "Shift+Tab", (w) => w === "status:Team");
  expect(back.filter((w) => w.startsWith("node:"))).toEqual(["node:claude-1"]);
  expect(csp).toEqual([]);
});

test("with the inspector open, tab reaches its tabs and its action buttons; esc gives focus back to the node", async ({ page, openPage }) => {
  await openPage();
  await page.locator('[data-agent="claude-2"]').focus();
  await page.keyboard.press("Enter");
  await expect(page.locator('[data-inspector="claude-2"]')).toBeVisible();
  // Focus starts on the first tab of the panel.
  expect(await where(page)).toBe("panel:tab");
  // Arrows move between the tabs.
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("tab", { name: "terminal" })).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  const seen = await walk(page, "Shift+Tab", (w) => w.startsWith("node:"));
  for (const action of ["message", "prompt", "pause", "restart", "restart fresh", "compact", "model", "jump to pane", "kill"]) {
    expect(seen.some((w) => w.startsWith("panel:") && w.includes(action)), action).toBe(true);
  }
  await page.getByRole("tab", { name: "overview" }).focus();
  await page.keyboard.press("Escape");
  await expect(page.locator("[data-inspector]")).toHaveCount(0);
  expect(await where(page)).toBe("node:claude-2");
});

test("focus is never stuck in the graph", async ({ page, openPage }) => {
  await openPage();
  await page.locator('[data-agent="claude-3"]').focus();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("Tab");
  expect(await where(page)).not.toMatch(/^node:/);
  await page.keyboard.press("Shift+Tab");
  expect(await where(page)).toBe("node:claude-4");
  await page.keyboard.press("Shift+Tab");
  expect(await where(page)).not.toMatch(/^node:/);
});

test("a dialog keeps focus inside and gives it back; focus starts on cancel in a confirmation", async ({ page, mock, openPage }) => {
  await openPage();
  await page.locator('[data-agent="claude-5"]').focus();
  await page.keyboard.press("k");
  const dialog = page.getByRole("dialog", { name: "kill agent" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "cancel" })).toBeFocused();
  for (let i = 0; i < 5; i++) {
    await page.keyboard.press("Tab");
    expect(await page.evaluate(() => !!document.activeElement?.closest("[role=dialog]"))).toBe(true);
  }
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  expect(await where(page)).toBe("node:claude-5");
  expect(mock.calls.some((c) => c.method === "agent_kill")).toBe(false);
});

test("every key of the table does its action from the page", async ({ page, mock, openPage }) => {
  await openPage();
  await page.locator('[data-agent="claude-2"]').focus();
  const press = async (key: string) => {
    await page.keyboard.press(key);
  };
  await press("t");
  await expect(page.locator(".task-dag")).toBeVisible();
  await press("t");
  await expect(page.locator(".agent-graph")).toBeVisible();
  await press("l");
  await expect(page.locator(".logs")).toBeVisible();
  await press("v");
  await expect(page.locator(".usage-view")).toBeVisible();
  await press("v");
  await expect(page.locator(".agent-graph")).toBeVisible();
  await press("?");
  await expect(page.getByRole("dialog", { name: "keys and commands" })).toBeVisible();
  await press("Escape");
  await press("o");
  expect(await where(page)).toBe("node:orchestrator");
  await press("m");
  await expect(page.locator("#compose-text")).toBeFocused();
  await expect(page.getByLabel("recipient")).toHaveValue("orchestrator");
  await press("Escape");
  await press("M");
  await expect(page.getByLabel("recipient")).toHaveValue("*");
  await press("Escape");
  await press("/");
  await expect(page.locator("#compose-text")).toHaveValue("/");
  await press("Escape");
  await page.locator('[data-agent="claude-2"]').focus();
  await press("p");
  await mock.waitForCall("agent_pause", (c) => (c.params as { id: string }).id === "claude-2");
  await press("r");
  await mock.waitForCall("agent_restart", (c) => (c.params as { id: string }).id === "claude-2");
  await press("s");
  await expect(page.getByRole("dialog", { name: "model and CLI" })).toBeVisible();
  await press("Escape");
  await press("O");
  await expect(page.getByRole("dialog", { name: "model and CLI" })).toContainText("orchestrator");
  await press("Escape");
  await press("S");
  await expect(page.getByRole("dialog", { name: "stop the team" })).toBeVisible();
  await press("Escape");
  await press("g");
  await mock.waitForCall("agent_focus", (c) => (c.params as { id: string }).id === "claude-2");
  await press("Enter");
  await expect(page.locator('[data-inspector="claude-2"]')).toBeVisible();
  await press("d");
  await expect(page.getByRole("tab", { name: "diff" })).toHaveAttribute("aria-selected", "true");
  await mock.waitForCall("git_diff", (c) => (c.params as { agent: string }).agent === "claude-2");
  await press("f");
  await expect(page.locator(".page-body")).toHaveClass(/full/);
  await press("f");
  await press("i");
  await expect(page.locator("#compose-text")).toBeFocused();
  await expect(page.locator(".message-box")).toHaveClass(/prompt/);
});

test("nothing fires while a text field has focus; after /keys off a k does nothing and the kill button still works", async ({ page, mock, openPage }) => {
  await openPage();
  await page.locator("#compose-text").focus();
  await page.keyboard.type("tlkSp");
  await expect(page.locator(".agent-graph")).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.locator("#compose-text").fill("/keys off");
  await page.keyboard.press("Enter");
  await mock.waitForCall("web_prefs", (c) => (c.params as { set?: { keys?: boolean } }).set?.keys === false);
  await page.locator('[data-agent="claude-4"]').focus();
  await page.keyboard.press("k");
  await page.keyboard.press("t");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.locator(".agent-graph")).toBeVisible();
  // ctrl-k still opens the command line.
  await page.keyboard.press("Control+k");
  await expect(page.locator("#compose-text")).toHaveValue("/");
  await page.keyboard.press("Escape");
  await page.locator('[data-agent="claude-4"]').focus();
  await page.keyboard.press("Enter");
  await page.locator(".insp-actions").getByRole("button", { name: "More", exact: true }).click();
  await page.getByRole("menu", { name: "actions for claude-4", exact: true }).getByRole("menuitem", { name: "kill…", exact: true }).click();
  await page.getByRole("dialog", { name: "kill agent" }).getByRole("button", { name: "kill claude-4" }).click();
  await mock.waitForCall("agent_kill", (c) => (c.params as { id: string }).id === "claude-4");
});
