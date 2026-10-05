import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures.ts";
import { PARKED_TASKS } from "../dev/scenarios.ts";

test.use({
  mockOptions: { fixture: "subagents", scenario: "parked", dist: process.env.CREW_FOCUS_DIST },
  viewport: { width: 1440, height: 960 },
  reducedMotion: "reduce",
});

const agentName = "claude-1, working";
const taskName = `${PARKED_TASKS.newer}, waiting`;
const panel = (page: Page) => page.getByRole("complementary", { name: "side panel", exact: true });
const view = (page: Page, name: "Team" | "Tasks") => page.getByRole("navigation", { name: "views", exact: true }).getByRole("button", { name, exact: true });

async function openAgent(page: Page) {
  const agent = page.getByRole("button", { name: agentName, exact: true });
  await agent.focus();
  await page.keyboard.press("Enter");
  await expect(panel(page).getByRole("heading", { name: "claude-1", exact: true })).toBeVisible();
}

async function openTaskAcrossViews(page: Page) {
  await openAgent(page);
  await panel(page).getByRole("listitem").filter({ has: page.getByText(PARKED_TASKS.newer, { exact: true }) }).getByRole("button").click();
  await expect(page.getByRole("heading", { name: `task ${PARKED_TASKS.newer}`, exact: true })).toBeVisible();
  await expect(page.locator(".task-panel")).toHaveAttribute("aria-busy", "false");
  await view(page, "Tasks").click();
  await expect(view(page, "Tasks")).toHaveAttribute("aria-pressed", "true");
}

async function expectTaskFocus(page: Page) {
  // Public focus destinations: the inspected task in the current presentation,
  // or the current view's navigation control when the opener no longer exists.
  await expect.poll(() => page.evaluate(() => {
    const active = document.activeElement;
    return active === document.body ? "BODY" : active?.getAttribute("aria-label") || active?.textContent?.trim();
  }), { message: `Escape must return focus to task ${PARKED_TASKS.newer} or its Tasks view control` }).toMatch(/^(parked-new, waiting|Tasks)$/);
  await expect(page.locator(":focus")).toBeVisible();
}

test("Escape returns to Tasks after an inspector task crosses views and resizes to 390px", async ({ page, openPage, csp }) => {
  await openPage();
  await openTaskAcrossViews(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect.poll(() => panel(page).evaluate((detail) => detail.contains(document.activeElement)), { message: "The resized task detail receives focus before Escape" }).toBe(true);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: `task ${PARKED_TASKS.newer}`, exact: true })).toHaveCount(0);
  await expectTaskFocus(page);
  expect(csp).toEqual([]);
});

test("Escape returns to Tasks after a crossed-view task reading region receives keyboard focus at 1440px", async ({ page, openPage, csp }) => {
  await openPage();
  await openTaskAcrossViews(page);
  // Tab from the close button enters the task's focusable reading region.
  await panel(page).getByRole("button", { name: "close the panel (esc)", exact: true }).press("Tab");
  await expect.poll(() => panel(page).getByRole("heading", { name: "Plan parked migration", exact: true }).evaluate((heading) => heading.parentElement === document.activeElement)).toBe(true);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: `task ${PARKED_TASKS.newer}`, exact: true })).toHaveCount(0);
  await expectTaskFocus(page);
  expect(csp).toEqual([]);
});

test("direct narrow Task List open and Escape return focus to that task", async ({ page, openPage }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openPage();
  await view(page, "Tasks").click();
  await page.getByRole("group", { name: "Tasks presentation", exact: true }).getByRole("button", { name: "List", exact: true }).click();
  const task = page.getByRole("button", { name: taskName, exact: true });
  await task.click();
  await expect(page.getByRole("heading", { name: `task ${PARKED_TASKS.newer}`, exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(task).toBeVisible();
  await expect(task).toBeFocused();
});

test("direct Team Graph open and Escape return focus to that agent", async ({ page, openPage }) => {
  await openPage();
  await openAgent(page);
  await page.keyboard.press("Escape");
  const agent = page.getByRole("button", { name: agentName, exact: true });
  await expect(agent).toBeVisible();
  await expect(agent).toBeFocused();
});

test("closing a crossed-view task after it disappears from the board returns focus to the Tasks view control", async ({ page, mock, openPage }) => {
  await openPage();
  await openTaskAcrossViews(page);
  const first = mock.calls.length;
  mock.state.tasks = mock.state.tasks.filter((task) => task.id !== PARKED_TASKS.newer);
  mock.push({ kind: "resync" });
  await mock.waitForCall("snapshot", (call) => call.seq >= first);
  await expect(page.getByRole("button", { name: taskName, exact: true })).toHaveCount(0);
  await expect(page.locator(".task-panel")).toHaveAttribute("aria-busy", "false");
  await panel(page).getByRole("button", { name: "close the panel (esc)", exact: true }).press("Tab");
  await expect.poll(() => panel(page).getByRole("heading", { name: "Plan parked migration", exact: true }).evaluate((heading) => heading.parentElement === document.activeElement)).toBe(true);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: `task ${PARKED_TASKS.newer}`, exact: true })).toHaveCount(0);
  await expect(view(page, "Tasks")).toHaveAttribute("aria-pressed", "true");
  await expect(view(page, "Tasks")).toBeVisible();
  await expect(view(page, "Tasks")).toBeFocused();
});
