// Losing the connection, and stopping (notes/web-design.md §transport
// "Reconnect and resync", §security "What a stopped page is", §tests "reconnect.spec.ts").
import { NO_REPLY } from "../dev/mock.ts";
import { expect, test } from "./fixtures.ts";

const activity = (page: import("@playwright/test").Page, id: string) => page.locator(`[data-agent="${id}"] .doing .what`);

test("the socket drops: reconnecting, then live with the new snapshot", async ({ page, mock, openPage, csp }) => {
  await openPage();
  mock.dropSockets();
  await expect(page.locator('[data-conn="reconnecting"]')).toBeAttached();
  (mock.state.agents[0] as { activity: string }).activity = "Changed while away";
  await expect(page.locator('[data-conn="live"]')).toBeAttached({ timeout: 10_000 });
  await expect(activity(page, "claude-1")).toHaveText("▸ Changed while away");
  expect(mock.calls.filter((c) => c.method === "snapshot").length).toBe(2);
  expect(csp).toEqual([]);
});

test("resync asks for a snapshot again on the same connection", async ({ page, mock, openPage }) => {
  await openPage();
  const before = mock.calls.filter((c) => c.method === "snapshot").length;
  mock.push({ kind: "resync" });
  await expect.poll(() => mock.calls.filter((c) => c.method === "snapshot").length).toBe(before + 1);
  expect(mock.calls.filter((c) => c.method === "hello").length).toBe(1);
});

test("a resync snapshot that times out starts over on a new connection, which brings the page up to date", async ({ page, mock, openPage, csp }) => {
  await openPage();
  // Changed without a push: the push was lost to the lag that resync reports.
  (mock.state.agents[0] as { activity: string }).activity = "Changed during the lag";
  const release = mock.holdSnapshot();
  const n = mock.calls.length;
  mock.push({ kind: "resync" });
  await mock.waitForCall("snapshot", (c) => c.seq >= n);
  await page.waitForTimeout(3300);
  release();
  await expect.poll(() => mock.calls.filter((c) => c.method === "hello").length, { timeout: 10_000 }).toBe(2);
  await expect(page.locator('[data-conn="live"]')).toBeAttached({ timeout: 10_000 });
  await expect(activity(page, "claude-1")).toHaveText("▸ Changed during the lag");
  expect(csp).toEqual([]);
});

test("a push between the page's snapshot and its reply is not shown; one after it is", async ({ page, mock, openPage }) => {
  await openPage();
  const release = mock.holdSnapshot();
  const n = mock.calls.length;
  mock.push({ kind: "resync" });
  await mock.waitForCall("snapshot", (c) => c.seq >= n);
  const agent = (activity: string) => ({ kind: "agent", data: { ...(structuredClone(mock.state.agents[1]) as object), activity, status: "working" } });
  const original = (mock.state.agents[1] as { activity: string }).activity;
  // Sent while the snapshot is in flight: discarded by the page.
  mock.push(agent("Between"));
  (mock.state.agents[1] as { activity: string }).activity = original;
  release();
  await page.waitForTimeout(150);
  await expect(activity(page, "claude-2")).not.toHaveText(/Between/);
  mock.push(agent("After"));
  await expect(activity(page, "claude-2")).toHaveText("▸ After");
});

test("shutdown ends the page: no retry, the storage emptied, what it showed kept, every control disabled", async ({ page, mock, openPage }) => {
  await openPage();
  await mock.shutdown();
  await expect(page.locator('[data-conn="stopped"]')).toBeAttached();
  await expect(page.locator(".conn-bar")).toContainText("The team was stopped at");
  expect(await page.evaluate(() => localStorage.getItem("crew.web.session"))).toBeNull();
  await expect(page.locator(".node.member")).toHaveCount(6);
  await expect(page.getByRole("button", { name: "stop team" })).toBeDisabled();
  await expect(page.locator("#compose-text")).toBeDisabled();
  // Reading and switching views still work.
  await page.keyboard.press("l");
  await expect(page.locator(".logs")).toBeVisible();
  await page.keyboard.press("l");
  await page.locator('[data-agent="claude-2"] .node-head').click();
  for (const name of ["pause", "restart", "kill", "message"]) await expect(page.locator(".insp-actions").getByRole("button", { name, exact: true })).toBeDisabled();
  const requests = mock.requests.length;
  await page.waitForTimeout(2000);
  expect(mock.requests.length).toBe(requests);
});

for (const mode of ["wrong", "verifier", "missing"] as const) {
  test(`a server whose proof is ${mode} is refused and nothing it sends is drawn`, async ({ page, mock }) => {
    mock.proofMode = mode;
    await page.goto(mock.pairUrl());
    await expect(page.locator('[data-conn="stopped"]')).toBeAttached();
    await expect(page.getByText("could not prove it is this dashboard's server")).toBeVisible();
    await expect(page.locator("[data-agent]")).toHaveCount(0);
    expect(mock.calls.map((c) => c.method)).toEqual(["web_pair", "hello"]);
    expect(await page.evaluate(() => localStorage.getItem("crew.web.session"))).toBeNull();
    const requests = mock.requests.length;
    await page.waitForTimeout(1500);
    expect(mock.requests.length).toBe(requests);
  });
}

test("a page that was live and then meets a server without the proof stops and keeps what it showed", async ({ page, mock, openPage }) => {
  await openPage();
  mock.proofMode = "wrong";
  mock.dropSockets();
  await expect(page.locator('[data-conn="stopped"]')).toBeAttached({ timeout: 10_000 });
  await expect(page.locator(".node.member")).toHaveCount(6);
  expect(mock.calls.filter((c) => c.method === "subscribe").length).toBe(1);
});

test("a message opened while the connection is unproven is fetched once it is proven again; one that does not exist says so", async ({ page, mock, openPage, csp }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await openPage();
  // Only the daemon knows it: older than any snapshot holds.
  const old = { id: "m-old-0001", from: "claude-4", to: "orchestrator", type: "message", body: "An old message, fetched on demand", timestamp: mock.now() - 3_600_000, correlation_id: null, reply_to: null, metadata: null };
  mock.on("message_get", (p: { id: string }) => {
    if (p.id === old.id) return old;
    throw { code: "not_found", message: `no message '${p.id}'` };
  });
  const first = mock.calls.length;
  mock.on("hello", () => NO_REPLY);
  mock.dropSockets();
  await mock.waitForCall("hello", (c) => c.seq >= first);
  await page.evaluate(() => { location.hash = "/?message=m-old-0001"; });
  const panel = page.locator("#side-panel");
  await expect(panel).toContainText("not connected");
  expect(mock.calls.filter((c) => c.method === "message_get")).toEqual([]);

  // Proven again: the panel fetches what it shows.
  mock.on("hello", null);
  await expect(page.locator('[data-conn="live"]')).toBeAttached({ timeout: 15_000 });
  await expect(panel).toContainText("An old message, fetched on demand");
  expect(mock.calls.filter((c) => c.method === "message_get").map((c) => c.params)).toEqual([{ id: "m-old-0001" }]);

  await page.evaluate(() => { location.hash = "/?message=m-gone-0002"; });
  await expect(panel).toContainText("not found: no message 'm-gone-0002'");
  expect(errors).toEqual([]);
  expect(csp).toEqual([]);
});
