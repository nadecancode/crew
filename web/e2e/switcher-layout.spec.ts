import { expect, test } from "./fixtures.ts";

for (const width of [390, 1440]) {
  test.describe(`workspace switcher at ${width}px`, () => {
    test.use({ viewport: { width, height: 844 }, mockOptions: { workspaces: [{ fixture: "snapshot", ...(width === 1440 ? { project: "Long workspace project " + "x".repeat(27) } : {}) }] } });
    test("current workspace status and counts fit, path has width, popup stays in viewport", async ({ page, openPage }) => {
      await openPage();
      await page.locator("[data-switcher-button]").click();
      const popup = page.locator(".switcher-pop");
      await expect(popup).toBeVisible();
      const current = popup.locator('.switcher-row[aria-current="true"]');
      await expect(current).toHaveCount(1);
      const cells = width === 390 ? [".ws-name", ".ws-state", ".ws-tasks", ".ws-dot"] : [".ws-dot", ".ws-tasks"];
      for (const selector of cells) {
        const size = await current.locator(selector).evaluate((el) => ({ client: el.clientWidth, scroll: el.scrollWidth }));
        expect(size.client, `${selector} must have reading width`).toBeGreaterThan(0);
        expect(size.scroll, `${selector} must not clip`).toBeLessThanOrEqual(size.client);
      }
      expect(await current.locator(".ws-dir").evaluate((el) => el.clientWidth)).toBeGreaterThan(0);
      const bounds = await popup.boundingBox();
      expect(bounds!.x).toBeGreaterThanOrEqual(0);
      expect(bounds!.y).toBeGreaterThanOrEqual(0);
      expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
      expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(844);
    });
  });
}
