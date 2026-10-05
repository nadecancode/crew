import { expect, noRequestsAfterStart, test } from "./fixtures.ts";

test.use({ viewport: { width: 1440, height: 960 } });

test("Team Graph/List keeps selection by ID through pushes and mounts one presentation", async ({ page, mock, openPage, csp }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  const modes = page.getByRole("group", { name: "Team presentation", exact: true });
  await modes.getByRole("button", { name: "List", exact: true }).click();
  const list = page.getByRole("listbox", { name: "team list", exact: true });
  await expect(page.locator(".agent-graph")).toHaveCount(0);
  await expect(list).toHaveAttribute("tabindex", "0");
  await expect(list.locator("[data-agent][tabindex='0']")).toHaveCount(0);
  await list.focus(); await page.keyboard.press("Home"); await page.keyboard.press("ArrowDown");
  await expect(list.locator(".vrow.selected [data-agent]")).toHaveAttribute("data-agent", "claude-2");
  const base = mock.state.agents.find((a) => a.id === "claude-1")!;
  mock.push({ kind: "agent", data: { ...base, id: "claude-0", name: "claude-0" } });
  await expect(list.locator(".vrow.selected [data-agent]")).toHaveAttribute("data-agent", "claude-2");
  await modes.getByRole("button", { name: "Graph", exact: true }).click();
  await expect(list).toHaveCount(0);
  await expect(page.locator(".agent-graph [data-agent='claude-2']")).toHaveClass(/selected/);
  await modes.getByRole("button", { name: "List", exact: true }).click();
  await expect(list.locator(".vrow.selected [data-agent]")).toHaveAttribute("data-agent", "claude-2");
  await list.focus(); await page.keyboard.press("Enter");
  await expect(page.locator('[data-inspector="claude-2"]')).toBeVisible();
  expect(guard.requests).toEqual([]); expect(csp).toEqual([]);
});

test("Task List arrows/Home/End/j/k/h/l/Enter preserve task identity and never Kill", async ({ page, mock, openPage }) => {
  await openPage(); await page.keyboard.press("t");
  const modes = page.getByRole("group", { name: "Tasks presentation", exact: true });
  await modes.getByRole("button", { name: "List", exact: true }).click();
  const list = page.getByRole("listbox", { name: "task list", exact: true });
  await expect(page.locator(".task-dag")).toHaveCount(0);
  const ids = await list.locator("[data-task]").evaluateAll((els) => els.map((el) => el.getAttribute("data-task")));
  const selected = () => list.locator(".vrow.selected [data-task]");
  await list.focus(); await page.keyboard.press("Home");
  await expect(selected()).toHaveAttribute("data-task", ids[0]!);
  await page.keyboard.press("ArrowDown"); await expect(selected()).toHaveAttribute("data-task", ids[1]!);
  await page.keyboard.press("k"); await expect(selected()).toHaveAttribute("data-task", ids[0]!);
  await page.keyboard.press("j"); await expect(selected()).toHaveAttribute("data-task", ids[1]!);
  await page.keyboard.press("h"); await expect(selected()).toHaveAttribute("data-task", ids[0]!);
  await page.keyboard.press("l"); await expect(selected()).toHaveAttribute("data-task", ids[1]!);
  await page.keyboard.press("End"); await expect(selected()).toHaveAttribute("data-task", ids.at(-1)!);
  await page.keyboard.press("Enter");
  await expect(page.locator(".task-panel .panel-title")).toHaveText(`task ${ids.at(-1)}`);
  await modes.getByRole("button", { name: "Graph", exact: true }).click();
  await expect(list).toHaveCount(0);
  await expect(page.locator(`.task-dag [data-task="${ids.at(-1)}"]`)).toHaveClass(/selected/);
  await modes.getByRole("button", { name: "List", exact: true }).click();
  await expect(selected()).toHaveAttribute("data-task", ids.at(-1)!);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(mock.calls.filter((c) => c.method === "agent_kill")).toEqual([]);
});

test("Shift+F10 and visible More reach the same selected agent menu from Team List", async ({ page, openPage }) => {
  await openPage();
  await page.getByRole("group", { name: "Team presentation", exact: true }).getByRole("button", { name: "List", exact: true }).click();
  const list = page.getByRole("listbox", { name: "team list", exact: true });
  await list.focus(); await page.keyboard.press("Home"); await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Shift+F10");
  const menu = page.getByRole("menu", { name: "actions for claude-2", exact: true });
  await expect(menu).toBeVisible();
  await page.keyboard.press("Escape"); await expect(list).toBeFocused();
  await list.getByRole("button", { name: "More actions for claude-2", exact: true }).click();
  await expect(menu).toBeVisible();
  await expect(page.locator("[data-inspector]")).toHaveCount(0);
  await page.keyboard.press("Escape");
});

test("Team and Tasks remember the chosen presentation across local view navigation", async ({ page, openPage }) => {
  await openPage();
  const team = page.getByRole("group", { name: "Team presentation", exact: true });
  await team.getByRole("button", { name: "List", exact: true }).click();
  await page.keyboard.press("v"); await page.keyboard.press("v");
  await expect(team.getByRole("button", { name: "List", exact: true })).toHaveAttribute("aria-pressed", "true");
  await team.getByRole("button", { name: "Graph", exact: true }).click();
  await page.keyboard.press("l"); await page.keyboard.press("l");
  await expect(team.getByRole("button", { name: "Graph", exact: true })).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("t");
  const tasks = page.getByRole("group", { name: "Tasks presentation", exact: true });
  await tasks.getByRole("button", { name: "List", exact: true }).click();
  await page.keyboard.press("t"); await page.keyboard.press("t");
  await expect(tasks.getByRole("button", { name: "List", exact: true })).toHaveAttribute("aria-pressed", "true");
});

test.describe("narrow default", () => {
  test.use({ viewport: { width: 390, height: 844 } });
  test("Team and Tasks start in List; a reader can select Graph", async ({ page, openPage }) => {
    await openPage();
    const team = page.getByRole("group", { name: "Team presentation", exact: true });
    await expect(team.getByRole("button", { name: "List", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(page.getByRole("listbox", { name: "team list", exact: true })).toBeVisible();
    await expect(page.locator(".agent-graph")).toHaveCount(0);
    const graph = team.getByRole("button", { name: "Graph", exact: true });
    await graph.focus();
    await graph.press("Enter");
    await expect(page.locator(".agent-graph")).toBeVisible();
    await expect(page.getByRole("listbox", { name: "team list", exact: true })).toHaveCount(0);
    await page.keyboard.press("v"); await page.keyboard.press("v");
    await expect(team.getByRole("button", { name: "Graph", exact: true })).toHaveAttribute("aria-pressed", "true");
    await page.keyboard.press("t");
    await expect(page.getByRole("group", { name: "Tasks presentation", exact: true }).getByRole("button", { name: "List", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(page.getByRole("listbox", { name: "task list", exact: true })).toBeVisible();
    await expect(page.locator(".task-dag")).toHaveCount(0);
  });
  test("a large Task List renders 160px rows within the visible range plus20", async ({ page, mock, openPage }) => {
    const base = mock.state.tasks[0];
    mock.state.tasks = Array.from({ length: 1000 }, (_, i) => ({ ...base, id: `bulk-${String(i).padStart(4, "0")}`, title: `Task ${i}`, dependencies: [], artifacts: [], result: null }));
    await openPage(); await page.keyboard.press("t");
    const list = page.getByRole("listbox", { name: "task list", exact: true });
    await expect(list.locator("[data-task='bulk-0000']")).toBeAttached();
    await list.evaluate((el) => { el.scrollTop = 400 * 160; el.dispatchEvent(new Event("scroll")); });
    await expect(list.locator("[data-task='bulk-0400']")).toBeAttached();
    const visible = await list.evaluate((el) => Math.ceil(el.clientHeight / 160));
    expect(await list.locator(".vrow").count()).toBeLessThanOrEqual(visible + 20);
    expect(await list.locator(".vrow").first().evaluate((el) => el.getBoundingClientRect().height)).toBe(160);
    await expect(page.locator(".task-dag")).toHaveCount(0);
  });
});
