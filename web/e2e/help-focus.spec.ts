import { expect, noRequestsAfterStart, test } from "./fixtures.ts";

for (const viewport of [{ width: 390, height: 844 }, { width: 720, height: 480 }, { width: 1440, height: 960 }]) {
  test.describe(`Help at ${viewport.width}×${viewport.height}`, () => {
    test.use({ viewport });

    test("opens at its heading, traps Tab, and returns focus when closed", async ({ page, mock, openPage, csp }) => {
      const guard = noRequestsAfterStart(page, mock);
      await openPage();
      const trigger = page.getByRole("button", { name: "keys and commands", exact: true });
      await trigger.click();
      const dialog = page.getByRole("dialog", { name: "keys and commands", exact: true });
      const heading = dialog.getByRole("heading", { name: "keys and commands", exact: true });
      await expect(heading).toBeFocused();
      const geometry = await dialog.evaluate((el) => ({ top: el.scrollTop, content: el.scrollHeight, height: el.clientHeight }));
      expect(geometry.content).toBeGreaterThan(geometry.height);
      expect(geometry.top).toBe(0);

      const close = dialog.getByRole("button", { name: "close", exact: true });
      await page.keyboard.press("Tab");
      await expect(close).toBeFocused();
      await page.keyboard.press("Tab");
      await expect(close).toBeFocused();
      await page.keyboard.press("Shift+Tab");
      await expect(close).toBeFocused();
      await page.keyboard.press("Escape");
      await expect(dialog).toHaveCount(0);
      await expect(trigger).toBeFocused();

      await trigger.click();
      await expect(heading).toBeFocused();
      expect(await dialog.evaluate((el) => el.scrollTop)).toBe(0);
      await close.click();
      await expect(dialog).toHaveCount(0);
      await expect(trigger).toBeFocused();
      expect(guard.requests).toEqual([]);
      expect(csp).toEqual([]);
    });
  });
}
