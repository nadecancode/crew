// Review-only evidence for neutral-content. Run with Node 24 after building
// web/: node review/neutral-content/capture.mjs. Nothing enters web-dist.
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
const out = join(root, "web/test-results/content-neutral");
mkdirSync(out, { recursive: true });
const manifest = JSON.parse(readFileSync(join(DEFAULT_DIST, "manifest.json"), "utf8"));
const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH || undefined });
const captures = [];
const viewports = {
  desktop: { width: 1440, height: 960 },
  narrow: { width: 390, height: 844 },
  zoom: { width: 720, height: 480 },
};
const base = ["bus", "events", "task", "message", "event", "usage"];
const states = ["task-pending", "task-error", "message-pending", "message-error", "event-missing", "events-error"];
const cases = [
  ...["dark", "light"].flatMap((theme) => ["desktop", "narrow"].flatMap((viewport) => base.map((surface) => ({ theme, viewport, surface, scenario: "busy" })))),
  ...base.map((surface) => ({ theme: "dark", viewport: "zoom", surface, scenario: "busy" })),
  ...["desktop", "narrow"].flatMap((viewport) => ["bus", "events", "usage"].map((surface) => ({ theme: "dark", viewport, surface, scenario: "empty" }))),
  ...["desktop", "narrow"].flatMap((viewport) => ["bus", "events", "task"].map((surface) => ({ theme: "dark", viewport, surface, scenario: "long-text" }))),
  ...["desktop", "narrow"].flatMap((viewport) => states.map((surface) => ({ theme: "dark", viewport, surface, scenario: "busy" }))),
];
const frame = (page) => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done(null)))));
let failed = 0;
try {
  for (const item of cases) {
    const id = `${item.scenario}-${item.surface}-${item.theme}-${item.viewport}`;
    const mock = await startMock({ scenario: item.scenario, fixture: "subagents" });
    mock.prefs = { keys: true, theme: "system" };
    const context = await browser.newContext({ viewport: viewports[item.viewport], colorScheme: item.theme, reducedMotion: "reduce", deviceScaleFactor: item.viewport === "zoom" ? 2 : 1, locale: "en-US", timezoneId: "UTC" });
    const page = await context.newPage();
    page.setDefaultTimeout(5000);
    await page.clock.setFixedTime(SCENARIO_TIME);
    const releases = [];
    const errors = [], csp = [], requests = [];
    let loaded = false;
    page.on("load", () => { loaded = true; });
    page.on("request", (request) => {
      if (loaded && !request.isNavigationRequest()) requests.push(`${request.method()} ${new URL(request.url()).pathname}`);
    });
    page.on("pageerror", (error) => errors.push(error.message));
    await page.exposeBinding("__contentCsp", (_source, value) => csp.push(value));
    await page.addInitScript(() => document.addEventListener("securitypolicyviolation", (event) => window.__contentCsp(`${event.effectiveDirective}: ${event.blockedURI}`)));
    const capture = { ...item, id, errors, csp, requests };
    try {
      await page.goto(mock.pairUrl());
      await page.locator('[data-workspace-state="live"]').waitFor();
      if (mock.state.questions.length) await dismissQuestions(page, mock.state.questions.length);
      const route = async (suffix) => {
        await page.evaluate((hash) => { location.hash = hash; }, `#/w/${mock.workspace().id}/${suffix}`);
        await frame(page);
      };
      const task = mock.state.tasks.find((entry) => entry.id === "scenario-done") ?? mock.state.tasks[0];
      const event = [...mock.state.events].reverse().find((entry) => entry.source === "vcs") ?? mock.state.events.at(-1);
      if (item.surface === "bus") {
        await page.getByRole("button", { name: "Message bus", exact: true }).click();
        await page.locator(".bus-list").waitFor();
        if (mock.state.messages.length) {
          await page.locator(".bus-list").focus();
          await page.keyboard.press("End");
        }
      } else if (item.surface === "events" || item.surface === "events-error") {
        await route("logs");
        await page.locator(".log-list").waitFor();
        if (item.surface === "events-error") {
          releases.push(setPanelRead(mock, "events", "error"));
          await page.locator(".filters").getByRole("button", { name: "Load older", exact: true }).click();
          await page.locator(".fetch-state.err").waitFor();
        } else if (mock.state.events.length && item.viewport === "desktop") {
          await page.locator(".log-list").focus();
          await page.keyboard.press("End");
        }
      } else if (item.surface.startsWith("task")) {
        if (item.surface.endsWith("pending")) releases.push(setPanelRead(mock, "task_get", "pending"));
        if (item.surface.endsWith("error")) releases.push(setPanelRead(mock, "task_get", "error"));
        await route(`tasks?task=${task.id}`);
        await page.locator(".task-title").waitFor();
        if (item.surface.endsWith("pending")) await page.locator('.task-panel[aria-busy="true"]').waitFor();
        else {
          await page.locator('.task-panel[aria-busy="false"]').waitFor();
          if (item.surface.endsWith("error")) await page.locator(".fetch-state").getByText("Mock task_get read failed.", { exact: false }).waitFor();
        }
      } else if (item.surface.startsWith("message")) {
        let id = mock.state.messages.at(-1).id;
        if (item.surface.includes("-")) {
          // Add server-side history after subscription, without pushing it to
          // this page, so the selected message must use message_get.
          id = "m-neutral-uncached";
          mock.state.messages.push({ ...mock.state.messages.at(-1), id });
          releases.push(setPanelRead(mock, "message_get", item.surface.endsWith("pending") ? "pending" : "error"));
        }
        await route(`?message=${id}`);
        await page.locator(".message-panel").waitFor();
        if (item.surface.endsWith("pending")) await page.locator('.message-panel[aria-busy="true"]').waitFor();
        else if (item.surface.endsWith("error")) await page.getByText("Mock message_get read failed.", { exact: false }).waitFor();
        else await page.locator(".message-panel pre.body").waitFor();
      } else if (item.surface.startsWith("event")) {
        await route(`logs?event=${item.surface === "event-missing" ? 999999 : event.seq}`);
        await page.locator(".event-panel").waitFor();
        if (item.surface === "event-missing") await page.getByText("Event unavailable", { exact: true }).waitFor();
        else await page.locator(".event-panel pre.body").waitFor();
      } else if (item.surface === "usage") {
        await route("usage");
        await page.locator(".usage-view").waitFor();
      }
      await page.evaluate(() => scrollTo(0, 0));
      await frame(page);
      capture.route = await page.evaluate(() => location.hash);
      capture.audit = await auditPage(page);
      capture.failures = a11yFailures(capture.audit);
      capture.rows = await page.locator(".bus-row,.log-row").evaluateAll((elements) => elements.map((el) => ({ class: el.className, height: el.getBoundingClientRect().height, background: getComputedStyle(el).backgroundColor })));
      capture.surfaceStyles = await page.locator(".bus,.logs,.usage-view,.task-panel,.message-panel,.event-panel").evaluateAll((elements) => elements.map((el) => ({ class: el.className, background: getComputedStyle(el).backgroundColor, color: getComputedStyle(el).color })));
      capture.image = `${id}.png`;
      await page.screenshot({ path: join(out, capture.image), animations: "disabled" });
      if (capture.failures.length || errors.length || csp.length || requests.length) failed++;
    } catch (error) {
      capture.error = String(error).replaceAll(mock.origin, "<mock-origin>").replace(/#pair=[^\s"']+/g, "#pair=<redacted>");
      failed++;
    } finally {
      releases.forEach((release) => release());
      await context.close();
      await mock.stop();
      captures.push(capture);
      writeFileSync(join(out, "manifest.json"), JSON.stringify({ source: manifest.source, fixture: "subagents", reducedMotion: true, captures, failures: failed }, null, 2) + "\n");
      console.log(`${id}: ${capture.error ? "error" : capture.failures.length || errors.length || csp.length || requests.length ? "failed" : "captured"}`);
    }
  }
} finally {
  await browser.close();
}
console.log(`${captures.length} captures, ${failed} failures; ${join(out, "manifest.json")}`);
if (failed) process.exitCode = 1;
