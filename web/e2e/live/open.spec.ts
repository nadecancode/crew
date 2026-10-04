// Pairing and the live page against a real daemon (notes/web-design.md §tests "open.spec.ts").
import { expect, test } from "./harness.ts";

test("pairing through the printed address: live, then agents, tasks and messages change as the script runs; the spent address is refused and the stored session works", async ({ page, context, browser, daemon, openLive }) => {
  daemon.start();
  const info = daemon.web();
  expect(info.url).toMatch(/^http:\/\/[a-z0-9-]+\.localhost:\d+\/$/);
  expect(info.pair_url).toContain("#pair=");

  await openLive(info);
  // The fragment is gone from the address.
  await expect(page).toHaveURL(info.workspace_url!);
  expect(page.url()).not.toContain("pair=");
  for (const id of ["dev", "peer", "rival", "echo", "idle"]) await expect(page.locator(`[data-agent="${id}"]`)).toBeVisible();

  // The script runs: the bus shows what the agents say.
  daemon.send("echo", "hello echo");
  await expect(page.locator("[data-message]", { hasText: "operator → echo" })).toContainText("hello echo");
  await expect(page.locator("[data-message]", { hasText: "echo → operator" })).toContainText("ack from echo");

  // A task created through the CLI reaches the page.
  daemon.c(["task", "add", "--id", "t-live", "--title", "added after open", "--agent", "idle"]);
  await page.evaluate(() => (location.hash = "#/tasks"));
  await expect(page.locator('[data-task="t-live"]')).toBeVisible({ timeout: 10_000 });
  await page.evaluate(() => (location.hash = "#/"));

  // The spent address in a browser with no session: refused, nothing of the dashboard shows.
  const stranger = await browser.newContext();
  const second = await stranger.newPage();
  await second.goto(info.pair_url);
  await expect(second.locator('[data-conn="signed-out"]')).toBeAttached({ timeout: 15_000 });
  await expect(second.locator("[data-agent]")).toHaveCount(0);
  await stranger.close();
  // In the paired browser the spent address falls back to the stored session; a new tab is live.
  const third = await context.newPage();
  await third.goto(info.workspace_url!);
  await third.locator('[data-conn="live"]').waitFor({ state: "attached", timeout: 15_000 });
  await expect(third.locator('[data-agent="dev"]')).toBeVisible();
  // No cookie, no service worker.
  expect(await context.cookies()).toEqual([]);
});
