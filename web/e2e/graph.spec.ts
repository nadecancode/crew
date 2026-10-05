// The agent graph (notes/web-design.md §graphs, §tests "graph.spec.ts").
import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures.ts";
import { WORKSPACE_IDS } from "../dev/mock.ts";

test.use({ mockOptions: { fixture: "team10" }, viewport: { width: 1500, height: 900 } });

/** The viewport's transform: `translate(x y) scale(k)`. */
async function view(page: Page) {
  const t = (await page.locator(".agent-graph > g").getAttribute("transform"))!;
  const m = /translate\(([-\d.e]+) ([-\d.e]+)\) scale\(([-\d.e]+)\)/.exec(t)!;
  return { x: Number(m[1]), y: Number(m[2]), k: Number(m[3]) };
}

async function svgBox(page: Page) {
  return (await page.locator(".agent-graph").boundingBox())!;
}

test("ten agents and the hub are drawn, each a button named by id and status", async ({ page, openPage, csp }) => {
  await openPage();
  await expect(page.locator(".node.member")).toHaveCount(10);
  await expect(page.locator(".node.hub")).toHaveCount(1);
  await expect(page.getByRole("button", { name: "claude-7, working" })).toBeVisible();
  await expect(page.locator('[data-agent="orchestrator"]')).toHaveAttribute("aria-label", "orchestrator, waiting");
  expect(csp).toEqual([]);
});

test("the wheel zooms about the pointer", async ({ page, openPage }) => {
  await openPage();
  const box = await svgBox(page);
  const at = { x: box.x + box.width * 0.3, y: box.y + box.height * 0.4 };
  const before = await view(page);
  const world = { x: (at.x - box.x - before.x) / before.k, y: (at.y - box.y - before.y) / before.k };
  await page.mouse.move(at.x, at.y);
  await page.mouse.wheel(0, -300);
  await expect.poll(async () => (await view(page)).k).toBeGreaterThan(before.k * 1.2);
  const after = await view(page);
  expect(world.x * after.k + after.x + box.x).toBeCloseTo(at.x, 0);
  expect(world.y * after.k + after.y + box.y).toBeCloseTo(at.y, 0);
});

test("dragging the background pans; a double click fits again", async ({ page, openPage }) => {
  await openPage();
  const box = await svgBox(page);
  const before = await view(page);
  // A spot with no node: the top left corner of the graph.
  await page.mouse.move(box.x + 8, box.y + 8);
  await page.mouse.down();
  await page.mouse.move(box.x + 60, box.y + 48, { steps: 5 });
  await page.mouse.up();
  const after = await view(page);
  expect(after.x - before.x).toBeCloseTo(52, 0);
  expect(after.y - before.y).toBeCloseTo(40, 0);
  await page.mouse.dblclick(box.x + 8, box.y + 8);
  await expect.poll(async () => (await view(page)).x).toBeCloseTo(before.x, 0);
});

test("dragging a node moves it; the place is kept in web_prefs and survives a reload", async ({ page, mock, openPage }) => {
  await openPage();
  const node = page.locator('[data-agent="claude-3"]');
  const start = (await node.locator(".node-head").boundingBox())!;
  await page.mouse.move(start.x + 10, start.y + 6);
  await page.mouse.down();
  await page.mouse.move(start.x + 90, start.y + 126, { steps: 8 });
  await page.mouse.up();
  const moved = (await node.locator(".node-head").boundingBox())!;
  expect(moved.x - start.x).toBeCloseTo(80, 0);
  expect(moved.y - start.y).toBeCloseTo(120, 0);
  // Not a click: the inspector did not open.
  await expect(page.locator("[data-inspector]")).toHaveCount(0);
  const call = await mock.waitForCall("web_prefs", (c) => !!(c.params as { set?: { layout?: Record<string, unknown> } })?.set?.layout?.["claude-3"]);
  expect((call.params as { set: { layout: Record<string, { x: number; y: number }> } }).set.layout["claude-3"]).toBeTruthy();
  await page.reload();
  await expect(page.locator('[data-conn="live"]')).toBeAttached();
  await expect.poll(async () => {
    const b = (await page.locator('[data-agent="claude-3"] .node-head').boundingBox())!;
    const o = (await page.locator('[data-agent="claude-1"] .node-head').boundingBox())!;
    return Math.round(b.y - o.y);
  }).not.toBe(0);
});

test("hover shows the card; a click selects and opens the inspector", async ({ page, openPage }) => {
  await openPage();
  await page.locator('[data-agent="claude-2"] .node-head').hover();
  await expect(page.locator(".hover-card")).toContainText("claude-2");
  await expect(page.locator(".hover-card")).toContainText("Sonnet 5.5");
  await page.locator('[data-agent="claude-2"] .node-head').click();
  await expect(page.locator('[data-inspector="claude-2"]')).toBeVisible();
  await expect(page.locator('[data-agent="claude-2"]')).toHaveClass(/selected/);
  expect(new URL(page.url()).hash).toBe(`#/w/${WORKSPACE_IDS[0]}/?agent=claude-2`);
});

test("without an orchestrator agent the empty hub is not a tab stop: tab enters the graph once, on a member", async ({ page, mock, openPage }) => {
  const agents = mock.state.agents as { id: string }[];
  agents.splice(agents.findIndex((a) => a.id === "orchestrator"), 1);
  await openPage();
  await expect(page.locator(".node.hub.empty")).toHaveCount(1);
  await page.getByRole("button", { name: "keys and commands" }).focus();
  const stops: string[] = [];
  for (let i = 0; i < 6; i++) {
    await page.keyboard.press("Tab");
    stops.push(await page.evaluate(() => {
      const el = document.activeElement;
      if (!el?.closest(".viewport")) return "outside";
      return el.getAttribute("data-agent") ? `node:${el.getAttribute("data-agent")}:${el.getAttribute("tabindex")}` : `graph:${el.getAttribute("class")}`;
    }));
  }
  const inGraph = stops.filter((s) => s !== "outside");
  expect(inGraph).toEqual(["node:claude-1:0"]);
});

test("the graph is one tab stop; arrows walk the agents in the TUI's order; enter opens the inspector", async ({ page, openPage }) => {
  await openPage();
  await expect(page.locator('[data-agent][tabindex="0"]')).toHaveCount(1);
  await page.locator('[data-agent="claude-1"]').focus();
  const focused = () => page.evaluate(() => document.activeElement?.getAttribute("data-agent"));
  await page.keyboard.press("ArrowRight");
  expect(await focused()).toBe("claude-2");
  await page.keyboard.press("ArrowDown");
  expect(await focused()).toBe("claude-3");
  await page.keyboard.press("ArrowLeft");
  expect(await focused()).toBe("claude-2");
  await page.keyboard.press("End");
  expect(await focused()).toBe("orchestrator");
  await page.keyboard.press("ArrowRight");
  expect(await focused()).toBe("claude-1");
  await page.keyboard.press("ArrowUp");
  expect(await focused()).toBe("orchestrator");
  await page.keyboard.press("Home");
  expect(await focused()).toBe("claude-1");
  await expect(page.locator('[data-agent][tabindex="0"]')).toHaveAttribute("data-agent", "claude-1");
  await page.keyboard.press("Enter");
  await expect(page.locator('[data-inspector="claude-1"]')).toBeVisible();
  // Focus moved to the side panel; esc gives it back to the node.
  expect(await page.evaluate(() => !!document.activeElement?.closest("#side-panel"))).toBe(true);
  await page.keyboard.press("Escape");
  await expect(page.locator("[data-inspector]")).toHaveCount(0);
  expect(await focused()).toBe("claude-1");
});

test("the task title on a node opens its card; the graph stays", async ({ page, openPage }) => {
  await openPage();
  await page.locator('[data-agent="claude-1"] [data-task-link]').click();
  await expect(page.locator("#side-panel")).toContainText("task billing");
  expect(new URL(page.url()).hash).toBe(`#/w/${WORKSPACE_IDS[0]}/?task=billing`);
  await expect(page.locator(".agent-graph")).toBeVisible();
});

test("+ - 0 zoom and fit", async ({ page, openPage }) => {
  await openPage();
  const k0 = (await view(page)).k;
  await page.keyboard.press("+");
  expect((await view(page)).k).toBeCloseTo(k0 * 1.25, 3);
  await page.keyboard.press("-");
  await page.keyboard.press("-");
  expect((await view(page)).k).toBeCloseTo(k0 / 1.25, 3);
  await page.keyboard.press("0");
  expect((await view(page)).k).toBeCloseTo(k0, 3);
});

test("a context menu offers the actions of a node", async ({ page, openPage }) => {
  await openPage();
  await page.locator('[data-agent="claude-4"] .node-head').click({ button: "right" });
  const menu = page.getByRole("menu", { name: "actions for claude-4" });
  await expect(menu).toBeVisible();
  const names = ["message", "prompt", "pause", "restart", "restart fresh…", "model and CLI", "compact", "open terminal", "jump to pane", "kill…"];
  await expect(menu.getByRole("menuitem")).toHaveCount(names.length);
  for (const [i, name] of names.entries()) await expect(menu.getByRole("menuitem").nth(i)).toHaveAccessibleName(name);
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
});
