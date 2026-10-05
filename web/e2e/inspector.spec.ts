import { expect, noRequestsAfterStart, setPanelRead, test } from "./fixtures.ts";

test.use({ viewport: { width: 1440, height: 960 } });

for (const [tab, method, text] of [
  ["transcript", "agent_transcript", "Compatibility tests passed."],
  ["diff", "git_diff", "round_cents"],
  ["terminal", "agent_output", "test result: ok. 12 passed"],
] as const) {
  test(`failed ${tab} refresh keeps evidence and displays a local error`, async ({ page, mock, openPage, csp }) => {
    const guard = noRequestsAfterStart(page, mock);
    const ready = setPanelRead(mock, method, "ready");
    let failed = () => {};
    try {
      await openPage();
      await page.locator('[data-agent="claude-1"]').focus(); await page.keyboard.press("Enter");
      await page.getByRole("tab", { name: tab, exact: true }).click();
      const panel = page.locator('[data-inspector="claude-1"]');
      await expect(panel).toContainText(text);
      failed = setPanelRead(mock, method, "error");
      await page.getByRole("tab", { name: tab, exact: true }).focus();
      await page.keyboard.press("r");
      await expect(panel).toContainText(`Mock ${method} read failed.`);
      await expect(panel).toContainText(text);
      expect(guard.requests).toEqual([]); expect(csp).toEqual([]);
    } finally { ready(); failed(); }
  });
}

test("process facts are reachable in a disclosure and mailbox words never invent receipt", async ({ page, mock, openPage }) => {
  const now = mock.now();
  const messages = ["Delivered example", "Read example", "Unknown delivery example"].map((body, i) => ({ id: `receipt-${i}`, body, from: "operator", to: "claude-1", type: "message", timestamp: now - 1000 + i, correlation_id: null, reply_to: null, metadata: null }));
  mock.state.messages.push(...messages);
  mock.on("inbox", () => ({ messages: [{ ...messages[0], delivered_at: now, acked_at: null }, { ...messages[1], delivered_at: now, acked_at: now + 1 }] }));
  await openPage();
  await page.locator('[data-agent="claude-1"]').focus(); await page.keyboard.press("Enter");
  const panel = page.locator('[data-inspector="claude-1"]');
  const details = panel.locator(".process-details");
  await expect(details).not.toHaveAttribute("open", "");
  await details.getByText("Process and delivery details", { exact: true }).click();
  await expect(details).toHaveAttribute("open", "");
  await expect(details).toContainText(String(mock.state.agents.find((a) => a.id === "claude-1")!.pid));
  await expect(panel.locator(".insp-messages li").filter({ hasText: "Delivered example" }).locator("span.ok")).toHaveText("Delivered");
  await expect(panel.locator(".insp-messages li").filter({ hasText: "Read example" }).locator("span.ok")).toHaveText("Read");
  await expect(panel.locator(".insp-messages li").filter({ hasText: "Unknown delivery example" }).locator("span.ok")).toHaveText("");
});

test("light theme preserves readable terminal text on its dark surface", async ({ page, openPage }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await openPage();
  await page.locator('[data-agent="claude-1"]').focus(); await page.keyboard.press("Enter");
  await page.getByRole("tab", { name: "terminal", exact: true }).click();
  const screen = page.locator(".screen");
  await expect(screen).toBeVisible();
  const contrast = await screen.evaluate((el) => {
    const css = getComputedStyle(el);
    const luma = (color: string) => (color.match(/[\d.]+/g) ?? []).slice(0, 3).map(Number).reduce((sum, value, i) => {
      const x = value / 255;
      return sum + (x <= .04045 ? x / 12.92 : ((x + .055) / 1.055) ** 2.4) * [.2126, .7152, .0722][i];
    }, 0);
    const foreground = luma(css.color), background = luma(css.backgroundColor);
    return (Math.max(foreground, background) + .05) / (Math.min(foreground, background) + .05);
  });
  expect(contrast).toBeGreaterThanOrEqual(4.5);
});
