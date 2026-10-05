import type { Page } from "@playwright/test";

const frame = (page: Page) => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done(null)))));

/** Each unseen operator question opens once. Later must be used for all of
 * them before capturing another surface, without answering or dropping data. */
export async function dismissQuestions(page: Page, count: number): Promise<void> {
  for (let i = 0; i < count; i++) {
    await page.getByRole("dialog", { name: "question", exact: true }).waitFor();
    await page.keyboard.press("Escape");
    await frame(page);
  }
  await page.getByRole("dialog").waitFor({ state: "hidden" });
}

export async function assertNoDialog(page: Page, surface: string): Promise<void> {
  const dialogs = page.getByRole("dialog");
  if (await dialogs.count()) {
    const names = await dialogs.evaluateAll((els) => els.map((el) => el.getAttribute("aria-label") ?? "unnamed"));
    throw new Error(`Unexpected dialog on surface '${surface}': ${names.join(", ")}`);
  }
}
