import type { Page } from "@playwright/test";
import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
import type { Mock } from "./fixtures.ts";
import { advanceScenario, PARKED_TASKS } from "../dev/scenarios.ts";

test.use({ mockOptions: { scenario: "parked", fixture: "subagents" }, viewport: { width: 1440, height: 960 }, colorScheme: "dark", reducedMotion: "reduce" });

const agentId = "claude-1";
const task = (mock: Mock, id: string) => mock.state.tasks.find((t) => t.id === id)!;

async function projection(page: Page, mock: Mock, id: string) {
  const title = task(mock, id).title;
  const modes = page.getByRole("group", { name: "Team presentation", exact: true });
  await modes.getByRole("button", { name: "Graph", exact: true }).click();
  const link = page.locator(`.agent-graph [data-agent="${agentId}"] [data-task-link]`);
  await expect(link).toHaveAttribute("data-task-link", id);
  await expect(link).toHaveText(title);
  await expect(page.locator(".current-work .current-title")).toHaveText(title);
  await modes.getByRole("button", { name: "List", exact: true }).click();
  await expect(page.locator(`.team-list [data-agent="${agentId}"] .summary-title`)).toHaveText(title);
  await modes.getByRole("button", { name: "Graph", exact: true }).click();
}

test("current work follows executing, then Waiting, then resumed older card across Graph, List and inspector", async ({ page, mock, openPage, csp }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  await page.locator(`.agent-graph [data-agent="${agentId}"] .node-head`).click();
  await expect(page.locator(`[data-inspector="${agentId}"]`)).toBeVisible();
  await projection(page, mock, PARKED_TASKS.current);
  for (const card of mock.state.tasks) {
    const row = page.locator(".insp-tasks li").filter({ has: page.getByText(card.id, { exact: true }) });
    await expect(row.getByText(card.status, { exact: true })).toBeVisible();
    await expect(row).toContainText(card.title);
  }
  advanceScenario(mock, "parked");
  await projection(page, mock, PARKED_TASKS.newer);
  advanceScenario(mock, "parked");
  await projection(page, mock, PARKED_TASKS.older);
  const newer = page.locator(".insp-tasks li").filter({ has: page.getByText(PARKED_TASKS.newer, { exact: true }) });
  await expect(newer.getByText("waiting", { exact: true })).toBeVisible();
  const done = page.locator(".insp-tasks li").filter({ has: page.getByText(PARKED_TASKS.current, { exact: true }) });
  await expect(done.getByText("done", { exact: true })).toBeVisible();
  expect(guard.requests).toEqual([]);
  expect(csp).toEqual([]);
});

test("parked cards remain individually inspectable without changing execution or the selected task", async ({ page, mock, openPage, csp }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  await page.locator(`.agent-graph [data-agent="${agentId}"] .node-head`).click();
  const older = page.locator(".insp-tasks li").filter({ has: page.getByText(PARKED_TASKS.older, { exact: true }) });
  await older.getByRole("button").click();
  await mock.waitForCall("task_get", (c) => c.params.id === PARKED_TASKS.older);
  await expect(page.locator(".task-panel .task-title")).toHaveText(task(mock, PARKED_TASKS.older).title);
  await expect(page.locator(`.agent-graph [data-agent="${agentId}"] [data-task-link]`)).toHaveAttribute("data-task-link", PARKED_TASKS.current);
  await page.getByRole("navigation", { name: "views", exact: true }).getByRole("button", { name: "Tasks", exact: true }).click();
  const newer = page.locator(`.task-dag [data-task="${PARKED_TASKS.newer}"]`);
  await expect(newer).toHaveClass(/ts-waiting/);
  await newer.click();
  await mock.waitForCall("task_get", (c) => c.params.id === PARKED_TASKS.newer);
  await expect(page.locator(".task-panel .panel-title")).toHaveText(`task ${PARKED_TASKS.newer}`);
  await expect(newer).toHaveClass(/selected/);
  await expect(page.locator(`.task-dag [data-task="${PARKED_TASKS.current}"]`)).not.toHaveClass(/selected/);
  await page.getByRole("group", { name: "Tasks presentation", exact: true }).getByRole("button", { name: "List", exact: true }).click();
  await expect(page.locator(".task-list .vrow.selected [data-task]")).toHaveAttribute("data-task", PARKED_TASKS.newer);
  const oldRow = page.locator(`.task-list [data-task="${PARKED_TASKS.older}"]`);
  await expect(oldRow.getByText("waiting", { exact: true })).toBeVisible();
  await oldRow.click();
  await expect(page.locator(".task-panel .panel-title")).toHaveText(`task ${PARKED_TASKS.older}`);
  await expect(page.locator(".task-list .vrow.selected [data-task]")).toHaveAttribute("data-task", PARKED_TASKS.older);
  expect(mock.state.tasks.filter((t) => t.status === "working").map((t) => t.id)).toEqual([PARKED_TASKS.current]);
  expect(mock.state.agents.find((a) => a.id === agentId)?.task).toBe(task(mock, PARKED_TASKS.current).title);
  expect(guard.requests).toEqual([]);
  expect(csp).toEqual([]);
});

for (const variant of ["started Blocked", "unknown-start Working"] as const) {
  test(`${variant} stays current ahead of newer parked Waiting cards`, async ({ page, mock, openPage }) => {
    const current = task(mock, PARKED_TASKS.current);
    current.status = variant === "started Blocked" ? "blocked" : "working";
    if (variant === "unknown-start Working") current.started_at = null;
    const agent = mock.state.agents.find((a) => a.id === agentId)!;
    agent.status = current.status;
    await openPage();
    await page.locator(`.agent-graph [data-agent="${agentId}"] .node-head`).click();
    await projection(page, mock, PARKED_TASKS.current);
  });
}
