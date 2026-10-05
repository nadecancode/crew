// R-GREEN-5 review evidence. Run after building web/ with Node 24.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { startMock, DEFAULT_DIST } from "../../web/dev/mock.ts";
import { SCENARIO_TIME, setPanelRead } from "../../web/dev/scenarios.ts";
import { auditPage, a11yFailures } from "../../web/dev/a11y.ts";
import { dismissQuestions } from "../../web/dev/capture.ts";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const require = createRequire(join(root, "web/package.json"));
const { chromium } = require("@playwright/test");
const out = join(root, "web/test-results/neutral-selection");
mkdirSync(out, { recursive: true });
const source = JSON.parse(readFileSync(join(DEFAULT_DIST, "manifest.json"), "utf8")).source;
const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH || undefined });
const captures = [];
const cases = ["team", "task", "transcript", "bus", "events", "events-warn", "events-error"];
const frame = (page) => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done(null)))));
try {
  for (const theme of ["dark", "light"]) for (const surface of cases) {
    const id = `${surface}-${theme}`;
    const mock = await startMock({ scenario: "busy", fixture: "subagents" });
    mock.prefs = { keys: true, theme: "system" };
    const release = setPanelRead(mock, "agent_transcript", "ready");
    const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, colorScheme: theme, reducedMotion: "reduce", locale: "en-US", timezoneId: "UTC" });
    const page = await context.newPage();
    page.setDefaultTimeout(5000);
    await page.clock.setFixedTime(SCENARIO_TIME);
    const capture = { id, theme, surface };
    try {
      await page.goto(mock.pairUrl());
      await page.locator('[data-workspace-state="live"]').waitFor();
      if (mock.state.questions.length) await dismissQuestions(page, mock.state.questions.length);
      const route = async (suffix) => {
        await page.evaluate((hash) => { location.hash = hash; }, `#/w/${mock.workspace().id}/${suffix}`);
        await frame(page);
      };
      let selector;
      if (surface === "team" || surface === "task") {
        if (surface === "task") await route("tasks");
        await page.getByRole("button", { name: "List", exact: true }).click();
        selector = `.${surface === "team" ? "team" : "task"}-list`;
      } else if (surface === "transcript") {
        const agent = mock.state.agents.find((entry) => !entry.orchestrator);
        await route(`?agent=${agent.id}`);
        await page.getByRole("tab", { name: "transcript", exact: true }).click();
        selector = ".transcript-list";
        await page.locator(`${selector} .vrow`).first().waitFor();
      } else if (surface === "bus") {
        selector = ".bus-list";
      } else {
        await route("logs");
        selector = ".log-list";
      }
      const list = page.locator(selector);
      await list.focus();
      await page.keyboard.press("Home");
      await page.keyboard.press("ArrowDown");
      if (surface === "events-warn" || surface === "events-error") {
        await list.locator(`.log-row.lv-${surface.split("-")[1]}`).first().click();
      }
      const hovered = list.locator(".vrow:not(.selected)").first();
      await hovered.hover();
      await page.evaluate(() => scrollTo(0, 0));
      await frame(page);
      capture.selection = await list.locator(".vrow.selected").evaluate((row) => ({
        background: getComputedStyle(row).backgroundColor,
        height: row.getBoundingClientRect().height,
        children: [...row.children].map((el) => ({ class: el.className, shadow: getComputedStyle(el).boxShadow, background: getComputedStyle(el).backgroundColor })),
      }));
      capture.hover = await hovered.evaluate((row) => ({ background: getComputedStyle(row).backgroundColor, children: [...row.children].map((el) => ({ class: el.className, shadow: getComputedStyle(el).boxShadow })) }));
      capture.audit = await auditPage(page);
      capture.failures = a11yFailures(capture.audit);
      capture.image = `${id}.png`;
      await page.screenshot({ path: join(out, capture.image), animations: "disabled" });
    } catch (error) {
      capture.error = String(error).replaceAll(mock.origin, "<mock-origin>").replace(/#pair=[^\s"']+/g, "#pair=<redacted>");
    } finally {
      release();
      await context.close();
      await mock.stop();
      captures.push(capture);
      writeFileSync(join(out, "summary.json"), JSON.stringify({ source, captures }, null, 2) + "\n");
      console.log(`${id}: ${capture.error ? "error" : capture.failures.length ? "failed" : "captured"}`);
    }
  }
} finally {
  await browser.close();
}
const failed = captures.filter((capture) => capture.error || capture.failures.length || !capture.selection.children.every((child) => child.shadow !== "none") || !capture.hover.children.every((child) => child.shadow === "none"));
console.log(`${captures.length} captures, ${failed.length} failures`);
if (failed.length) process.exitCode = 1;
