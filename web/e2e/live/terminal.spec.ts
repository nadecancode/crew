// The terminal tab against a real pty agent (notes/web-design.md §tests "terminal.spec.ts"):
// it shows the agent's screen, and typed keys are echoed.
import { expect, test } from "./harness.ts";

test("the terminal shows the agent's screen and echoes typed keys", async ({ page, daemon, openLive }) => {
  daemon.start();
  await openLive(daemon.web());
  // The agent writes to its terminal when it acts.
  daemon.send("echo", "wake up");
  await page.locator('[data-agent="echo"] .node-head').click();
  await page.getByRole("tab", { name: "terminal", exact: true }).click();
  const term = page.locator('[data-terminal="echo"]');
  await expect(term.locator(".xterm-rows")).toContainText("[echo]", { timeout: 15_000 });
  await term.locator(".xterm-host").click();
  await page.keyboard.type("typed-in-browser");
  await expect(term.locator(".xterm-rows")).toContainText("typed-in-browser");
  await page.keyboard.press("Control+]");
  await page.keyboard.press("Escape");
});
