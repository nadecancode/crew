import { expect, NO_REPLY, test } from "./fixtures.ts";

const resultFor = (tab: string, id: string) => {
  const text = `INSPECTOR CONTENT FOR ${id}`;
  if (tab === "diff") return { ok: true, text, commit: null, conflict: null };
  if (tab === "transcript") return { available: true, entries: [{ at: null, kind: "reply", text }], truncated: false };
  return { alive: true, text };
};

for (const [tab, method] of [["diff", "git_diff"], ["transcript", "agent_transcript"], ["terminal", "agent_output"]]) {
  test(`history navigation after stop does not show another agent's ${tab}`, async ({ page, mock, openPage }) => {
    for (const a of mock.state.agents) a.backend = "herdr";
    mock.on(method, (p: { id?: string; agent?: string }) => resultFor(tab, (p.id ?? p.agent)!));
    await openPage();
    await page.evaluate(() => { location.hash = "/?agent=claude-1"; });
    await page.getByRole("tab", { name: tab, exact: true }).click();
    await expect(page.locator("#side-panel")).toContainText("INSPECTOR CONTENT FOR claude-1");
    await mock.shutdown();
    await expect(page.locator('[data-conn="stopped"]')).toBeAttached();
    await page.evaluate(() => { location.hash = "/?agent=claude-2"; });
    await expect(page.locator(".panel-title")).toHaveText("claude-2");
    await expect(page.locator("#side-panel")).not.toContainText("INSPECTOR CONTENT FOR claude-1");
  });
}

test("history navigation during a transcript request fetches the newly selected agent", async ({ page, mock, openPage }) => {
  let release!: (result: unknown) => void;
  mock.on("agent_transcript", (p: { id: string }) => p.id === "claude-1"
    ? new Promise((resolve) => { release = resolve; })
    : resultFor("transcript", p.id));
  await openPage();
  await page.evaluate(() => { location.hash = "/?agent=claude-1"; });
  await page.getByRole("tab", { name: "transcript", exact: true }).click();
  await mock.waitForCall("agent_transcript", (c) => c.params.id === "claude-1");
  await page.evaluate(() => { location.hash = "/?agent=claude-2"; });
  await expect(page.locator(".panel-title")).toHaveText("claude-2");
  release(resultFor("transcript", "claude-1"));
  await expect(page.locator("#side-panel")).toContainText("INSPECTOR CONTENT FOR claude-2");
});

test("a transcript opened during reconnect is fetched when the verified connection returns", async ({ page, mock, openPage }) => {
  mock.on("agent_transcript", (p: { id: string }) => resultFor("transcript", p.id));
  await openPage();
  const first = mock.calls.length;
  mock.on("hello", () => NO_REPLY);
  mock.dropSockets();
  await mock.waitForCall("hello", (c) => c.seq >= first);
  await page.evaluate(() => { location.hash = "/?agent=claude-1"; });
  await page.getByRole("tab", { name: "transcript", exact: true }).click();
  expect(mock.calls.some((c) => c.method === "agent_transcript")).toBe(false);
  mock.on("hello", null);
  mock.dropSockets();
  await expect(page.locator('[data-conn="live"]')).toBeAttached();
  await expect(page.locator("#side-panel")).toContainText("INSPECTOR CONTENT FOR claude-1");
});
