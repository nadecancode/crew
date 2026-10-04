import { expect, test } from "@playwright/test";

const PORT = Number(process.env.PORT ?? 47615);
const STRICT_PORT = PORT + 1;
const LABEL = process.env.LABEL ?? "spike";
const origin = (port: number) => `http://${LABEL}.localhost:${port}`;
const csp = (page: import("@playwright/test").Page) => {
  const seen: string[] = [];
  page.on("console", (m) => { if (m.type() === "error" && m.text().includes("Content Security Policy")) seen.push(m.text()); });
  return seen;
};

test("the dashboard goes live, drops the token from the URL and attaches a terminal", async ({ page }) => {
  const violations = csp(page);
  await page.goto(`${origin(PORT)}/#k=spiketoken`);
  await expect(page.locator("#conn")).toHaveText("live");
  await expect(page.locator(".node")).toHaveCount(6);
  expect(new URL(page.url()).hash).toBe("");
  await page.getByRole("button", { name: /claude-3/ }).press("Enter");
  await expect(page.locator(".xterm")).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as any).__term?.bytes ?? 0)).toBeGreaterThan(0);
  // The pty snapshot of the scripted agent, through `attach`.
  await expect.poll(() => page.evaluate(() => (window as any).__term.term.buffer.active.getLine(0)?.translateToString(true))).toContain("claude-3");
  expect(violations).toEqual([]);
});

test("without a token nothing but errors comes back", async ({ page }) => {
  await page.goto(`${origin(PORT)}/`);
  await expect(page.locator("#conn")).toContainText("error");
  await expect(page.locator(".node")).toHaveCount(0);
});

test("a page from another origin cannot open the socket", async ({ page }) => {
  await page.goto("data:text/html,<title>x</title>");
  const result = await page.evaluate(({ port, label }) => new Promise((done) => {
    const ws = new WebSocket(`ws://${label}.localhost:${port}/ws`);
    ws.onopen = () => done("open"); ws.onerror = () => done("refused");
  }), { port: PORT, label: LABEL });
  expect(result).toBe("refused");
});

test("xterm.js needs style elements: `style-src 'self'` alone blocks it", async ({ page }) => {
  const violations = csp(page);
  await page.goto(`${origin(STRICT_PORT)}/#k=spiketoken`);
  await expect(page.locator("#conn")).toHaveText("live");
  expect(violations).toEqual([]); // the app itself is clean under the strict policy
  await page.getByRole("button", { name: /claude-3/ }).press("Enter");
  await expect.poll(() => violations.length).toBeGreaterThan(0);
  expect(violations[0]).toContain("style-src");
});
