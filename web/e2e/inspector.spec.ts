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

for (const text of ["", "retained ended output"]) {
  test(`an ended ${text ? "nonempty" : "empty"} Herdr terminal explains its state and sends no typed or pasted input`, async ({ page, mock, openPage, csp }) => {
    const guard = noRequestsAfterStart(page, mock);
    mock.on("agent_output", () => ({ alive: false, text }));
    try {
      await openPage();
      await page.locator('[data-agent="claude-1"]').focus();
      await page.keyboard.press("Enter");
      await page.getByRole("tab", { name: "terminal", exact: true }).click();
      const terminal = page.locator('.terminal-tab.herdr[data-terminal="claude-1"]');
      await expect(terminal).toContainText("terminal ended; refresh checks for output");
      const screen = terminal.locator(".screen");
      await expect(screen).toHaveText(text);
      if (text) await expect(screen).toHaveAttribute("tabindex", "0");
      else await expect(screen).not.toHaveAttribute("tabindex", "0");
      await expect(terminal).not.toContainText(/click (it )?to type|ctrl-\]/i);
      await expect(terminal.getByRole("button", { name: "jump to pane", exact: true })).toBeDisabled();
      const refresh = terminal.getByRole("button", { name: "refresh", exact: true });
      await expect(refresh).toBeEnabled();

      await screen.dispatchEvent("keydown", { key: "x", code: "KeyX", bubbles: true });
      await screen.evaluate((el) => {
        const data = new DataTransfer();
        data.setData("text/plain", "must not reach the ended terminal");
        el.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
      });
      // A later read on the same socket makes any input already sent observable.
      const next = mock.calls.length;
      await refresh.click();
      await mock.waitForCall("agent_output", (call) => call.seq >= next);
      expect(mock.calls.filter((call) => call.method === "agent_input")).toEqual([]);
      expect(guard.requests).toEqual([]);
      expect(csp).toEqual([]);
    } finally { mock.on("agent_output", null); }
  });
}

test("live Herdr terminal still accepts typed and pasted input", async ({ page, mock, openPage }) => {
  const release = setPanelRead(mock, "agent_output", "ready");
  try {
    await openPage();
    await page.locator('[data-agent="claude-1"]').focus();
    await page.keyboard.press("Enter");
    await page.getByRole("tab", { name: "terminal", exact: true }).click();
    const terminal = page.locator('.terminal-tab.herdr[data-terminal="claude-1"]');
    const screen = terminal.locator(".screen");
    await expect(screen).toContainText("test result: ok. 12 passed");
    await expect(screen).toHaveAttribute("tabindex", "0");
    await expect(terminal.getByRole("button", { name: "jump to pane", exact: true })).toBeEnabled();
    await screen.focus();
    await page.keyboard.press("x");
    const typed = await mock.waitForCall("agent_input", (call) => call.params.text === "x");
    expect(typed.params).toEqual({ id: "claude-1", text: "x", submit: false });
    await screen.evaluate((el) => {
      const data = new DataTransfer();
      data.setData("text/plain", "pasted evidence");
      el.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
    });
    const pasted = await mock.waitForCall("agent_input", (call) => call.params.text === "pasted evidence");
    expect(pasted.params).toEqual({ id: "claude-1", text: "pasted evidence", submit: false });
  } finally { release(); }
});
