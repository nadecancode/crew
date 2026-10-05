import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
import { dismissQuestions } from "../dev/capture.ts";

test.use({ viewport: { width: 1440, height: 960 } });

test("appearance follows System, persists explicit choices only on the index, and restores", async ({ page, mock, openPage, csp }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await openPage();
  const appearance = page.getByRole("combobox", { name: "Appearance", exact: true });
  const storageKeys = await page.evaluate(() => Object.keys(localStorage));
  await expect(appearance).toHaveValue("system");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  const light = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  await appearance.selectOption("dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  const dark = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  expect(dark).not.toBe(light);
  await mock.waitForCall("web_prefs", (c) => c.conn.workspace === null && c.params?.set?.theme === "dark");
  expect(mock.workspace().prefs).not.toHaveProperty("theme");
  expect(await page.evaluate(() => Object.keys(localStorage))).toEqual(storageKeys);
  expect(await page.evaluate(() => Object.keys(sessionStorage).filter((k) => /prefs|theme|appearance/i.test(k)))).toEqual([]);
  await page.reload();
  await expect(appearance).toHaveValue("dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.emulateMedia({ colorScheme: "light" });
  expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe(dark);
  await appearance.selectOption("system");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  expect(csp).toEqual([]);
});

test("summary counts exclude paused working agents and historical peer questions", async ({ page, mock, openPage }) => {
  const first = mock.state.agents.find((a) => !a.orchestrator)!;
  first.paused = true; first.status = "working";
  await openPage();
  const values = () => page.locator(".summary-counts dd").allTextContents();
  const total = mock.state.agents.length, working = mock.state.agents.filter((a) => a.status === "working" && !a.paused).length;
  const done = mock.state.tasks.filter((t) => t.status === "done").length;
  await expect.poll(values).toEqual([String(total), String(working), `${done}/${mock.state.tasks.length}`, "0"]);
  for (let i = 0; i < 2; i++) mock.push({ kind: "message", data: { id: `q-summary-${i}`, from: "orchestrator", to: "operator", type: "question", body: "Which release path?", timestamp: mock.now(), metadata: { options: ["Review first"] } } });
  await dismissQuestions(page, 2);
  await expect.poll(values).toEqual([String(total), String(working), `${done}/${mock.state.tasks.length}`, "2"]);
});

test("keyboard separator exposes bounds, resizes by20/80 and clamps", async ({ page, openPage }) => {
  await openPage();
  const separator = page.getByRole("separator", { name: "resize the side panel", exact: true });
  await expect(separator).toHaveAttribute("tabindex", "0");
  await expect(separator).toHaveAttribute("aria-valuemin", "320");
  const initial = Number(await separator.getAttribute("aria-valuenow"));
  await separator.focus(); await page.keyboard.press("ArrowLeft");
  await expect(separator).toHaveAttribute("aria-valuenow", String(initial + 20));
  await page.keyboard.press("Shift+ArrowLeft");
  await expect(separator).toHaveAttribute("aria-valuenow", String(initial + 100));
  await page.keyboard.press("Shift+ArrowRight");
  await expect(separator).toHaveAttribute("aria-valuenow", String(initial + 20));
  for (let i = 0; i < 30; i++) await page.keyboard.press("Shift+ArrowRight");
  await expect(separator).toHaveAttribute("aria-valuenow", "320");
  for (let i = 0; i < 30; i++) await page.keyboard.press("Shift+ArrowLeft");
  await expect(separator).toHaveAttribute("aria-valuenow", await separator.getAttribute("aria-valuemax") ?? "");
});

test("IME Enter does not submit an operator question; a failed answer keeps the draft", async ({ page, mock, openPage }) => {
  mock.on("answer", () => { throw { code: "unavailable", message: "Answer could not be delivered" }; });
  await openPage();
  mock.push({ kind: "message", data: { id: "q-ime", from: "orchestrator", to: "operator", type: "question", body: "How should this release ship?", timestamp: mock.now(), metadata: { options: [] } } });
  const dialog = page.getByRole("dialog", { name: "question", exact: true });
  const answer = dialog.getByRole("textbox", { name: "your own answer", exact: true });
  await answer.fill("Ship after review");
  await answer.dispatchEvent("keydown", { key: "Enter", isComposing: true });
  expect(mock.calls.filter((c) => c.method === "answer")).toEqual([]);
  await expect(answer).toHaveValue("Ship after review");
  await answer.press("Enter");
  expect((await mock.waitForCall("answer")).params).toEqual({ id: "q-ime", body: "Ship after review" });
  await expect(page.locator("footer .status-line")).toContainText("Answer could not be delivered");
  await expect(answer).toHaveValue("Ship after review");
});

test.describe("replacement panel", () => {
  test.use({ viewport: { width: 390, height: 844 } });
  test("Message bus hides/inerts main and Close/Escape restore its trigger", async ({ page, mock, openPage, csp }) => {
    const guard = noRequestsAfterStart(page, mock);
    await openPage();
    const main = page.locator("#main"), panel = page.locator("#side-panel");
    const bus = page.getByRole("navigation", { name: "views", exact: true }).getByRole("button", { name: "Message bus", exact: true });
    await expect(panel).toBeHidden();
    await bus.click();
    await expect(panel).toBeVisible(); await expect(main).toHaveAttribute("inert", "");
    await expect(main).toBeHidden();
    await expect(panel.getByRole("heading", { name: "Message bus", exact: true })).toBeFocused();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await panel.getByRole("button", { name: "close the panel (esc)", exact: true }).click();
    await expect(panel).toBeHidden(); await expect(bus).toBeFocused();
    await bus.click(); await page.keyboard.press("Escape");
    await expect(panel).toBeHidden(); await expect(bus).toBeFocused();
    await bus.click();
    await page.getByRole("navigation", { name: "views", exact: true }).getByRole("button", { name: "Events", exact: true }).click();
    await expect(panel).toBeHidden(); await expect(main).not.toHaveAttribute("inert", "");
    await expect(page.locator(".log-list")).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(390);
    expect(guard.requests).toEqual([]); expect(csp).toEqual([]);
  });
  test("a Team List selection opens a nonmodal reading surface and returns to the list", async ({ page, openPage }) => {
    await openPage();
    const list = page.getByRole("listbox", { name: "team list", exact: true });
    await list.focus(); await page.keyboard.press("Home"); await page.keyboard.press("ArrowDown"); await page.keyboard.press("Enter");
    await expect(page.locator('[data-inspector="claude-2"]')).toBeVisible();
    await expect(page.locator("#main")).toHaveAttribute("inert", "");
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await page.keyboard.press("Escape");
    await expect(list).toBeFocused();
    await expect(page.locator("#side-panel")).toBeHidden();
    await expect(list.locator(".vrow.selected [data-agent]")).toHaveAttribute("data-agent", "claude-2");
  });
});
