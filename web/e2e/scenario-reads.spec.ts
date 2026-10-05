import { expect, noRequestsAfterStart, setPanelRead, test } from "./fixtures.ts";

test.use({ mockOptions: { scenario: "busy", fixture: "subagents" }, viewport: { width: 1440, height: 960 } });

test("pending transcript is distinct from empty and resolves through the normal RPC", async ({ page, mock, openPage, csp }) => {
  const guard = noRequestsAfterStart(page, mock);
  const release = setPanelRead(mock, "agent_transcript", "pending");
  try {
    await openPage();
    await page.locator('[data-agent="claude-1"]').focus();
    await page.keyboard.press("Enter");
    await page.getByRole("tab", { name: "transcript", exact: true }).click();
    await mock.waitForCall("agent_transcript");
    await expect(page.locator(".transcript")).toContainText(/reading|loading/i);
    await expect(page.locator(".transcript-list .vrow")).toHaveCount(0);
    release();
    await expect(page.locator(".transcript-list")).toContainText("Compatibility tests passed. Evidence committed.");
    expect(guard.requests).toEqual([]);
    expect(csp).toEqual([]);
  } finally {
    release();
  }
});

for (const mode of ["empty", "unavailable", "truncated", "error"] as const) {
  test(`transcript ${mode} state remains readable without after-load requests`, async ({ page, mock, openPage, csp }) => {
    const guard = noRequestsAfterStart(page, mock);
    const release = setPanelRead(mock, "agent_transcript", mode);
    try {
      await openPage();
      await page.locator('[data-agent="claude-1"]').focus();
      await page.keyboard.press("Enter");
      await page.getByRole("tab", { name: "transcript", exact: true }).click();
      const transcript = page.locator(".transcript");
      if (mode === "empty") {
        await expect(transcript).toContainText("no transcript entries yet");
        await expect(page.locator(".transcript-list .vrow")).toHaveCount(0);
        await expect(transcript).not.toContainText(/reading|loading/i);
        await expect(transcript).not.toContainText(/no session file|not available|unavailable|failed/i);
      }
      if (mode === "unavailable") await expect(transcript).toContainText(/no session file|not available|unavailable/i);
      if (mode === "truncated") {
        await expect(transcript).toContainText(/truncat|older entries|last .*entries/i);
        await expect(page.locator(".transcript-list")).toContainText("Compatibility tests passed. Evidence committed.");
      }
      if (mode === "error") await expect(transcript).toContainText("Mock agent_transcript read failed.");
      expect(guard.requests).toEqual([]);
      expect(csp).toEqual([]);
    } finally { release(); }
  });
}

test("a delayed old transcript cannot replace another selected agent's evidence", async ({ page, mock, openPage }) => {
  const release = setPanelRead(mock, "agent_transcript", "pending");
  try {
    await openPage();
    await page.locator('[data-agent="claude-1"]').focus();
    await page.keyboard.press("Enter");
    await page.getByRole("tab", { name: "transcript", exact: true }).click();
    await mock.waitForCall("agent_transcript", (c) => c.params.id === "claude-1");
    await page.evaluate(() => { location.hash = location.hash.replace("agent=claude-1", "agent=claude-2"); });
    await expect(page.locator('[data-inspector="claude-2"]')).toBeVisible();
    release();
    await page.getByRole("tab", { name: "transcript", exact: true }).click();
    await mock.waitForCall("agent_transcript", (c) => c.params.id === "claude-2");
    await expect(page.locator('[data-inspector="claude-2"]')).not.toContainText("(claude-1)");
    await expect(page.locator(".transcript-list .vrow")).not.toHaveCount(0);
  } finally { release(); }
});

test("releasing an old held read preserves a newer controller error mode", async ({ page, mock, openPage }) => {
  const old = setPanelRead(mock, "agent_transcript", "pending");
  let current = () => {};
  try {
    await openPage();
    await page.locator('[data-agent="claude-1"]').focus();
    await page.keyboard.press("Enter");
    await page.getByRole("tab", { name: "transcript", exact: true }).click();
    await mock.waitForCall("agent_transcript");
    current = setPanelRead(mock, "agent_transcript", "error");
    old(); old();
    await expect(page.locator(".transcript-list")).toContainText("Compatibility tests passed.");
    await page.evaluate(() => { location.hash = location.hash.replace("agent=claude-1", "agent=claude-2"); });
    await expect(page.locator('[data-inspector="claude-2"]')).toBeVisible();
    await page.getByRole("tab", { name: "transcript", exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("Mock agent_transcript read failed.");
  } finally { old(); current(); }
});
