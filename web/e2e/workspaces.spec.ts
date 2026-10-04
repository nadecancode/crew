// One page, several workspaces (notes/web-multi.md §multi-ui, §multi-tests
// "Frontend"): the switcher by keyboard and by mouse, a switch that leaves
// nothing of the workspace left on screen, the back button, deep links,
// badges and the notice, a workspace that stops, returns, is taken out, is
// not joined; and the races of §multi-ui "Two levels" in a real browser.
import type { Page } from "@playwright/test";
import { WORKSPACE_IDS } from "../dev/mock.ts";
import { expect, test } from "./fixtures.ts";

const [A, B] = WORKSPACE_IDS;
const TWO = {
  workspaces: [{ fixture: "snapshot" as const }, { fixture: "team10" as const, project: "search", workspace: "/home/op/work/search" }],
  others: [{ id: "4c2e8a1b9d0f3e57", project: "docs", workspace: "/home/op/work/docs" }],
};
test.use({ mockOptions: TWO });

const button = (page: Page) => page.locator("[data-switcher-button]");
const list = (page: Page) => page.getByRole("listbox", { name: "workspaces" });
const live = (page: Page) => expect(page.locator('[data-workspace-state="live"]')).toBeAttached();
const hellos = (mock: { calls: { method: string; params: any }[] }, id: string) => mock.calls.filter((c) => c.method === "hello" && c.params?.workspace === id).length;

/** Every value `[data-workspace-state]` takes from now on, in the page. */
async function recordStates(page: Page) {
  await page.evaluate(() => {
    const seen: string[] = [];
    (window as unknown as { __states: string[] }).__states = seen;
    const note = () => {
      const v = document.querySelector("[data-workspace-state]")?.getAttribute("data-workspace-state");
      if (v && seen[seen.length - 1] !== v) seen.push(v);
    };
    note();
    new MutationObserver(note).observe(document.body, { subtree: true, attributes: true, childList: true, attributeFilter: ["data-workspace-state"] });
  });
}
const states = (page: Page) => page.evaluate(() => (window as unknown as { __states: string[] }).__states);

test.describe("the races of an attempt", () => {
  test("taken out and joined again in one listing, the old close held back 300 ms: live again with the agents, never removed", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await recordStates(page);
    mock.removeAndRejoin(A, { holdCloseMs: 300 });
    await expect.poll(() => hellos(mock, A)).toBe(2);
    await live(page);
    await expect(page.locator(".node.member")).toHaveCount(6);
    expect(await states(page)).not.toContain("removed");
    expect(csp).toEqual([]);
  });

  test("a protocol answer held back while its daemon was replaced: asks, finds it running, and is live; never incompatible", async ({ page, mock, openPage }) => {
    await openPage();
    await recordStates(page);
    mock.workspace(0).holdHello("protocol", 300);
    for (const c of mock.workspace(0).connections) c.close(4001, "the workspace's connection ended");
    await expect.poll(() => hellos(mock, A)).toBe(2);
    mock.startDaemon(A);
    await expect.poll(() => hellos(mock, A), { timeout: 5000 }).toBe(3);
    await live(page);
    await expect(page.locator(".node.member")).toHaveCount(6);
    expect(await states(page)).not.toContain("incompatible");
  });
});

test("the switcher by keyboard: w opens it on the current one, arrows and enter switch, esc gives the focus back, 1–9 switch directly", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await expect(button(page)).toHaveText("checkout-rewrite ▾");
  await page.keyboard.press("w");
  await expect(list(page)).toBeFocused();
  const options = list(page).getByRole("option");
  await expect(options).toHaveCount(2);
  await expect(options.nth(0)).toHaveAttribute("aria-current", "true");
  await expect(options.nth(0)).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ArrowDown");
  await expect(options.nth(1)).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(new RegExp(`#/w/${B}/$`));
  await live(page);
  await expect(button(page)).toHaveText("search ▾");
  await expect(page.locator(".node.member")).toHaveCount(10);
  await expect(button(page)).toBeFocused();
  await expect(page).toHaveTitle("search · crew");

  await page.keyboard.press("w");
  await expect(list(page)).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(list(page)).toHaveCount(0);
  await expect(button(page)).toBeFocused();

  await page.keyboard.press("w");
  await page.keyboard.press("1");
  await expect(page).toHaveURL(new RegExp(`#/w/${A}/$`));
  await live(page);
  await expect(page.locator(".node.member")).toHaveCount(6);
  // The workspace last on screen is the operator's, kept by the server.
  await expect.poll(() => mock.prefs.last).toBe(A);
  expect(csp).toEqual([]);
});

test("the switcher by mouse; typing filters; others are named with the command that adds them", async ({ page, openPage }) => {
  await openPage();
  await button(page).click();
  await expect(list(page)).toBeVisible();
  const other = page.locator("[data-other]");
  await expect(other).toContainText("docs");
  await expect(other).toContainText("crew web -C ~/work/docs");
  await expect(other).not.toHaveAttribute("role", "option");
  await page.keyboard.type("sear");
  await expect(list(page).getByRole("option")).toHaveCount(1);
  await page.keyboard.press("Backspace");
  await page.keyboard.press("Backspace");
  await page.keyboard.press("Backspace");
  await page.keyboard.press("Backspace");
  await expect(list(page).getByRole("option")).toHaveCount(2);
  await list(page).getByRole("option", { name: /search/ }).click();
  await live(page);
  await expect(button(page)).toHaveText("search ▾");
  // A click elsewhere closes it.
  await button(page).click();
  await expect(list(page)).toBeVisible();
  await page.locator("#main").click({ position: { x: 5, y: 5 } });
  await expect(list(page)).toHaveCount(0);
});

test("by keyboard alone the copy buttons are reached: tab moves into them, the list stays open, esc closes and gives the focus back, tab out closes", async ({ page, openPage }) => {
  await openPage();
  await page.keyboard.press("w");
  await expect(list(page)).toBeFocused();
  await page.keyboard.press("Tab");
  const copy = page.locator("[data-other]").getByRole("button", { name: "copy" });
  await expect(copy).toBeFocused();
  await expect(list(page)).toBeVisible();
  // Single keys are the list's, not the page's: t changes no view.
  await page.keyboard.press("t");
  await expect(page.locator(".agent-graph")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(list(page)).toHaveCount(0);
  await expect(button(page)).toBeFocused();
  await page.keyboard.press("w");
  await page.keyboard.press("Tab");
  await expect(copy).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(list(page)).toHaveCount(0);
  await expect(page.locator("[data-other]")).toHaveCount(0);
});

test("a switch disposes the workspace left: nothing of it remains while the next one's snapshot is held back", async ({ page, mock, openPage }) => {
  await openPage();
  await page.locator('[data-agent="claude-2"] .node-head').click();
  await expect(page.locator('[data-inspector="claude-2"]')).toBeVisible();
  const release = mock.workspace(1).holdSnapshot();
  await page.keyboard.press("Escape");
  await page.keyboard.press("w");
  await page.keyboard.press("2");
  await expect(button(page)).toHaveText("search ▾");
  await expect(page.locator("#main")).toContainText("connecting to search");
  await expect(page.locator(".node")).toHaveCount(0);
  await expect(page.locator("[data-inspector]")).toHaveCount(0);
  await expect(page.locator(".question-banner, .usage-strip")).toHaveCount(0);
  await expect(page.locator(".status-bar")).not.toContainText("checkout-rewrite");
  release();
  await live(page);
  await expect(page.locator(".node.member")).toHaveCount(10);
});

test("the back button returns to the workspace left, with the view it had", async ({ page, openPage }) => {
  await openPage();
  await page.keyboard.press("t");
  await expect(page).toHaveURL(new RegExp(`#/w/${A}/tasks$`));
  await page.keyboard.press("w");
  await page.keyboard.press("2");
  await expect(page).toHaveURL(new RegExp(`#/w/${B}/$`));
  await live(page);
  await page.goBack();
  await expect(page).toHaveURL(new RegExp(`#/w/${A}/tasks$`));
  await live(page);
  await expect(button(page)).toHaveText("checkout-rewrite ▾");
  await expect(page.locator(".task-dag, [data-task]").first()).toBeVisible();
  await page.goForward();
  await expect(button(page)).toHaveText("search ▾");
  // Back again by the switcher: the view A had is kept for this document.
  await page.keyboard.press("w");
  await page.keyboard.press("1");
  await expect(page).toHaveURL(new RegExp(`#/w/${A}/tasks$`));
});

test("a deep link to each workspace: the pairing address with w, and an address in another tab", async ({ page, context, mock }) => {
  await page.goto(mock.pairUrl(undefined, B));
  await expect(page).toHaveURL(new RegExp(`#/w/${B}/$`));
  await live(page);
  await expect(button(page)).toHaveText("search ▾");
  const second = await context.newPage();
  await second.goto(`${mock.origin}/#/w/${A}/logs`);
  await live(second);
  await expect(button(second)).toHaveText("checkout-rewrite ▾");
  await expect(second.locator(".logs")).toBeVisible();
  expect(mock.calls.filter((c) => c.method === "web_pair").length).toBe(1);
});

test("a question in the other workspace: the badge, the title, one notice with a way there; no text of it shown here", async ({ page, mock, openPage }) => {
  await openPage();
  mock.workspace(1).push({ kind: "message", data: { id: "q-b1", from: "orchestrator", to: "operator", type: "question", body: "Merge search now?", timestamp: mock.now(), correlation_id: null, reply_to: null, metadata: null } });
  await expect(page.locator(".switcher > .badge.q")).toHaveText("? 1");
  await expect(page).toHaveTitle("(1) checkout-rewrite · crew");
  const toast = page.locator(".toast", { hasText: "search: 1 question is waiting" });
  await expect(toast).toBeVisible();
  await expect(page.getByText("Merge search now?")).toHaveCount(0);
  await toast.getByRole("button", { name: "switch" }).click();
  await live(page);
  await expect(button(page)).toHaveText("search ▾");
  await expect(page.getByText(/Merge search now\?/).first()).toBeVisible();
  await expect(page.locator(".switcher > .badge.q")).toHaveCount(0);
});

test("a workspace stops and returns: the page stays live, the workspace says so, then comes back with the same session", async ({ page, mock, openPage }) => {
  await openPage();
  mock.stopDaemon(A);
  await expect(page.locator('[data-workspace-state="not-running"]')).toBeAttached();
  await expect(page.locator('.status-bar [data-conn="live"]')).toBeAttached();
  await expect(page.locator("[data-workspace-bar]")).toContainText("checkout-rewrite is not running. Start it (crew run in /work/demo) and it comes back here.");
  // The page's own bar says nothing: the page is live.
  await expect(page.locator(".conn-bar:not(.ws-bar)")).toHaveCount(0);
  // What it showed stays, read-only.
  await expect(page.locator(".node.member")).toHaveCount(6);
  await expect(page.locator(".page")).toHaveClass(/readonly/);
  await expect(page.locator("#compose-text")).toBeDisabled();
  // The other one is still reachable.
  await button(page).click();
  await expect(list(page).getByRole("option").nth(0)).toHaveAttribute("data-state", "stopped");
  await page.keyboard.press("Escape");
  mock.startDaemon(A);
  await live(page);
  await expect(page.locator("#compose-text")).toBeEnabled();
  expect(mock.calls.filter((c) => c.method === "web_pair").length).toBe(1);
  expect(await page.evaluate(() => localStorage.getItem("crew.web.session"))).not.toBeNull();
});

test("a workspace taken out while on screen: it says so, its dialog closes; joined again, it comes back in the same tab", async ({ page, mock, openPage }) => {
  await openPage();
  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: "keys and commands" })).toBeVisible();
  mock.remove(A);
  await expect(page.locator('[data-workspace-state="removed"]')).toBeAttached();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.locator("[data-workspace-bar]")).toContainText("checkout-rewrite was taken out of this dashboard");
  await expect(page.locator('.status-bar [data-conn="live"]')).toBeAttached();
  mock.join(A);
  await live(page);
  await expect(page.locator(".node.member")).toHaveCount(6);
});

test("an id that is not joined: it says so and connects nothing; the switcher offers the others", async ({ page, mock }) => {
  await page.goto(mock.pairUrl(undefined, "ffffffffffffffff"));
  await expect(page.locator('[data-workspace-state="removed"]')).toBeAttached();
  await expect(page.locator("#main")).toContainText("That workspace is not in this dashboard: run crew web in it.");
  expect(hellos(mock, "ffffffffffffffff")).toBe(0);
  await page.keyboard.press("w");
  await page.keyboard.press("2");
  await live(page);
  await expect(button(page)).toHaveText("search ▾");
});

test("a dialog open when the back button changes the workspace is closed, not carried over", async ({ page, openPage }) => {
  await openPage();
  await page.keyboard.press("w");
  await page.keyboard.press("2");
  await live(page);
  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: "keys and commands" })).toBeVisible();
  await page.goBack();
  await expect(button(page)).toHaveText("checkout-rewrite ▾");
  await expect(page.getByRole("dialog")).toHaveCount(0);
});
