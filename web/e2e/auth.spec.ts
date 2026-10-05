// Pairing, the session, and what the page asks of its origin
// (notes/web-design.md §security, §tests "auth.spec.ts").
import { expect, noRequestsAfterStart, test } from "./fixtures.ts";

test("the fragment is gone after load; no cookie, no service worker; the session is under one key", async ({ page, context, mock, openPage, csp }) => {
  await openPage();
  expect(new URL(page.url()).hash).not.toContain("pair");
  expect(await context.cookies()).toEqual([]);
  expect(await page.evaluate(async () => (await navigator.serviceWorker?.getRegistrations?.())?.length ?? 0)).toBe(0);
  const keys = await page.evaluate(() => Object.keys(localStorage));
  expect(keys).toEqual(["crew.web.session"]);
  const stored = JSON.parse((await page.evaluate(() => localStorage.getItem("crew.web.session")))!);
  expect(Object.keys(stored).sort()).toEqual(["token", "verifier"]);
  expect(mock.calls.filter((c) => c.method === "web_pair").length).toBe(1);
  expect(csp).toEqual([]);
});

test("without a session the page is signed out and makes no call", async ({ page, mock }) => {
  await page.goto(mock.url);
  await expect(page.locator('[data-conn="signed-out"]')).toBeVisible();
  await expect(page.getByText("run crew web")).toBeVisible();
  expect(mock.calls).toEqual([]);
});

test("a second tab of the same address is live without pairing", async ({ context, mock, openPage }) => {
  await openPage();
  const second = await context.newPage();
  await second.goto(mock.url);
  await expect(second.locator('[data-conn="live"]')).toBeAttached();
  expect(mock.calls.filter((c) => c.method === "web_pair").length).toBe(1);
  // One index connection per tab, each with its own hello.
  expect(mock.calls.filter((c) => c.method === "hello" && c.params?.workspace === undefined).length).toBe(2);
});

test("a spent code with a stored session: the stored session is used", async ({ context, mock, openPage }) => {
  const page = await openPage();
  const url = mock.pairUrl();
  await page.goto("about:blank");
  const again = await context.newPage();
  await again.goto(url);
  await expect(again.locator('[data-conn="live"]')).toBeAttached();
  const spent = await context.newPage();
  await spent.goto(url);
  await expect(spent.locator('[data-conn="live"]')).toBeAttached();
  await expect(spent.locator(".footer")).toContainText("already used");
});

test("after the document started the page asks its origin for nothing, the terminal included", async ({ page, mock, openPage }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  // A pty agent, so the terminal tab is xterm.js on a terminal socket.
  const agent = { ...(mock.state.agents[0] as object), id: "claude-9", backend: "pty", status: "working" };
  mock.push({ kind: "agent", data: agent });
  await page.locator('[data-agent="claude-9"] .node-head').click();
  await page.getByRole("tab", { name: "terminal" }).click();
  await expect(page.locator(".xterm")).toBeVisible();
  await expect.poll(() => mock.connections.filter((c) => c.attached === "claude-9").length).toBe(1);
  for (const tab of ["transcript", "diff", "overview"]) await page.getByRole("tab", { name: tab }).click();
  for (const key of ["t", "l", "v", "v", "?"]) await page.keyboard.press(key);
  await page.keyboard.press("Escape");
  await page.locator('[data-agent="claude-2"] .node-head').click();
  await page.getByRole("toolbar", { name: "actions for claude-2", exact: true }).getByRole("button", { name: "More", exact: true }).click();
  await page.getByRole("menu", { name: "actions for claude-2", exact: true }).getByRole("menuitem", { name: "model and CLI", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "model and CLI" })).toBeVisible();
  await page.keyboard.press("Escape");
  expect(guard.requests).toEqual([]);
});
