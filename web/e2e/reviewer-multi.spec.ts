// Keyboard access to the action offered for a workspace outside the dashboard.
import { expect, test } from "./fixtures.ts";

test.use({ mockOptions: { others: [{ id: "4c2e8a1b9d0f3e57", project: "docs", workspace: "/srv/docs" }] } });

test("the switcher's copy command can be reached and activated from the keyboard", async ({ page, openPage }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: {
      writeText: async (text: string) => { (window as any).__copiedCommand = text; },
    } });
  });
  await openPage();
  await page.keyboard.press("w");
  await expect(page.getByRole("listbox", { name: "workspaces" })).toBeFocused();
  await page.keyboard.press("Tab");
  const copy = page.locator("[data-other]").getByRole("button", { name: "copy", exact: true });
  await expect(copy).toBeFocused();
  await page.keyboard.press("Enter");
  await expect.poll(() => page.evaluate(() => (window as any).__copiedCommand)).toMatch(/^crew web -C /);
});

test("Enter on the focused copy button performs its native activation", async ({ page, openPage }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: {
      writeText: async (text: string) => { (window as any).__copiedCommand = text; },
    } });
  });
  await openPage();
  await page.keyboard.press("w");
  const copy = page.locator("[data-other]").getByRole("button", { name: "copy", exact: true });
  // Isolate activation from the separate Tab-navigation regression above.
  await copy.focus();
  await page.keyboard.press("Enter");
  await expect.poll(() => page.evaluate(() => (window as any).__copiedCommand)).toMatch(/^crew web -C /);
});
