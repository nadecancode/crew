// Capture the real production application against the mock. Nothing here is
// imported by src/ or shipped; report contains no pairing/session material.
// Run from web/: bun run gallery --out /tmp/crew-gallery --scenario baseline
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { chromium } from "@playwright/test";
import type { Page } from "@playwright/test";
import { DEFAULT_DIST, FIXTURE_NAMES, startMock } from "./mock.ts";
import type { FixtureName, Mock } from "./mock.ts";
import { advanceScenario, QUESTION_ID, SCENARIO_NAMES, SCENARIO_TIME, setPanelRead } from "./scenarios.ts";
import type { PanelMethod, ReadMode, ScenarioName } from "./scenarios.ts";
import { a11yFailures, auditPage } from "./a11y.ts";
import { assertNoDialog, dismissQuestions } from "./capture.ts";

export const SURFACES = ["team", "team-graph", "team-list", "tasks", "tasks-graph", "tasks-list", "task-detail", "events", "event-detail", "usage", "inspector", "terminal", "transcript", "diff", "message-detail", "switcher", "help", "confirm", "context-menu", "model", "question", "pending", "error", "inspector-pending", "inspector-error", "diff-pending", "diff-error", "terminal-pending", "terminal-error", "transcript-empty", "transcript-unavailable", "transcript-truncated", "diff-empty", "terminal-ended", "workspace-disconnected", "workspace-stopped", "workspace-removed", "dashboard-stopped"] as const;
type Surface = (typeof SURFACES)[number];
const VIEWPORTS = {
  desktop: { width: 1440, height: 960 }, tablet: { width: 1024, height: 768 }, narrow: { width: 390, height: 844 },
  // 200% browser zoom gives this CSS viewport for a 1440×960 desktop. DSF
  // preserves the physical screenshot size; it does not alone simulate zoom.
  zoom: { width: 720, height: 480 },
};
type ViewportName = keyof typeof VIEWPORTS;
const ROOT = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), "../.."));
const usage = `usage: node dev/gallery.ts --out <directory outside repository> [--scenario ${SCENARIO_NAMES.join(",")}] [--surface ${SURFACES.join(",")}] [--theme light,dark] [--viewport desktop,tablet,narrow,zoom] [--fixture snapshot|subagents|team10] [--dist <bundle>] [--strict] [--allow-overflow narrow,zoom]`;

function choices<T extends string>(value: string | undefined, allowed: readonly T[], defaults = allowed): T[] {
  const selected = value ? value.split(",") : [...defaults];
  for (const name of selected) if (!(allowed as readonly string[]).includes(name)) throw new Error(`Unknown choice '${name}'; expected ${allowed.join(", ")}`);
  return [...new Set(selected)] as T[];
}
const { values } = parseArgs({ options: {
  out: { type: "string" }, scenario: { type: "string" }, surface: { type: "string" }, theme: { type: "string" }, viewport: { type: "string" },
  fixture: { type: "string", default: "subagents" }, dist: { type: "string" }, help: { type: "boolean", short: "h" },
  strict: { type: "boolean" }, "allow-overflow": { type: "string" },
} });
if (values.help) { console.log(usage); process.exit(0); }
if (!values.out) throw new Error(usage);
const out = resolve(values.out);
const inside = (path: string) => { const rel = relative(ROOT, path); return rel !== ".." && !rel.startsWith("../") && !isAbsolute(rel); };
if (inside(out)) throw new Error("Gallery output must be outside the repository");
let ancestor = dirname(out);
while (!existsSync(ancestor)) ancestor = dirname(ancestor);
if (inside(resolve(realpathSync(ancestor), relative(ancestor, out)))) throw new Error("Gallery output resolves inside the repository");
mkdirSync(out, { recursive: true });
if (inside(realpathSync(out))) throw new Error("Gallery output resolves inside the repository");
const scenarios = choices(values.scenario, SCENARIO_NAMES);
const surfaces = choices(values.surface, SURFACES);
const themes = choices(values.theme, ["light", "dark"] as const);
const viewports = choices(values.viewport, Object.keys(VIEWPORTS) as ViewportName[]);
const allowedOverflow = choices(values["allow-overflow"], Object.keys(VIEWPORTS) as ViewportName[], []);
const fixture = choices(values.fixture, FIXTURE_NAMES)[0];
const dist = resolve(values.dist ?? DEFAULT_DIST);
const manifest = JSON.parse(readFileSync(join(dist, "manifest.json"), "utf8"));
const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH || undefined });
const captures: Record<string, unknown>[] = [];
let failures = 0;
const frame = (page: Page) => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done(null)))));
const shellQuote = (s: string) => `'${s.replaceAll("'", "'\\''")}'`;

async function route(page: Page, mock: Mock, suffix = "") {
  await page.evaluate((hash) => { location.hash = hash; }, `#/w/${mock.workspace().id}/${suffix}`);
  await frame(page);
}
async function activate(page: Page, selector: string) {
  // DOM activation keeps a baseline's overflowing 390px page at scrollX=0.
  // Accessibility tests use real keyboard/pointer actions separately.
  await page.locator(selector).first().evaluate((el) => (el as HTMLElement).click());
}

async function prepare(page: Page, mock: Mock, surface: Surface, releases: (() => void)[]): Promise<string | null> {
  const agent = mock.state.agents.find((a) => !a.orchestrator)?.id;
  const task = mock.state.tasks.find((t) => t.id === "scenario-done") ?? mock.state.tasks[0];
  const message = mock.state.messages.at(-1);
  const event = [...mock.state.events].reverse().find((e) => e.source === "vcs") ?? mock.state.events.at(-1);
  const agentSurfaces = ["inspector", "terminal", "transcript", "diff", "context-menu", "model", "pending", "error", "inspector-pending", "inspector-error", "diff-pending", "diff-error", "terminal-pending", "terminal-error", "transcript-empty", "transcript-unavailable", "transcript-truncated", "diff-empty", "terminal-ended"];
  if (agentSurfaces.includes(surface) && !agent) return "Scenario has no agents";
  if (surface === "task-detail" && !task) return "Scenario has no tasks";
  if (surface === "message-detail" && !message) return "Scenario has no messages";
  if (surface === "event-detail" && !event) return "Scenario has no events";
  const read = (method: PanelMethod, mode: ReadMode) => releases.push(setPanelRead(mock, method, mode));
  switch (surface) {
    case "team": break;
    case "tasks": await route(page, mock, "tasks"); if (mock.state.tasks.length) await page.locator("#main [data-task]").first().waitFor(); break;
    case "team-graph": case "tasks-graph": {
      if (surface === "tasks-graph") await route(page, mock, "tasks");
      const graph = page.getByRole("button", { name: "Graph", exact: true });
      if (await graph.count()) await graph.evaluate((el) => (el as HTMLElement).click());
      else if (await page.getByRole("button", { name: "List", exact: true }).count()) return "Graph presentation is not present in this build";
      break;
    }
    case "team-list": case "tasks-list": {
      if (surface === "tasks-list") await route(page, mock, "tasks");
      const list = page.getByRole("button", { name: "List", exact: true });
      if (!await list.count()) return "List presentation is not present in this build";
      await list.evaluate((el) => (el as HTMLElement).click());
      break;
    }
    case "task-detail": await route(page, mock, `tasks?task=${encodeURIComponent(task!.id)}`); await page.locator(".task-panel .task-title").waitFor(); break;
    case "events": await route(page, mock, "logs"); await page.locator(".log-list").waitFor(); break;
    case "event-detail": await route(page, mock, `logs?event=${event!.seq}`); await page.locator(".event-panel").waitFor(); break;
    case "usage": await route(page, mock, "usage"); await page.locator(".usage-view").waitFor(); break;
    case "message-detail": await route(page, mock, `?message=${encodeURIComponent(message!.id)}`); await page.locator(".message-panel").waitFor(); break;
    case "switcher": await activate(page, "[data-switcher-button]"); await page.locator(".switcher-pop").waitFor(); break;
    case "help": await page.locator("body").click({ position: { x: 2, y: 2 } }); await page.keyboard.press("?"); await page.getByRole("dialog").waitFor(); break;
    case "confirm": await page.locator("body").click({ position: { x: 2, y: 2 } }); await page.keyboard.press("S"); await page.getByRole("dialog").waitFor(); break;
    case "question":
      if (!mock.state.questions.length) {
        if (!agent) return "Scenario has no asker";
        mock.push({ kind: "message", data: { id: QUESTION_ID, from: "orchestrator", to: "operator", type: "question", body: "The legacy export uses integer cents. Which compatibility strategy should the adapter use?", timestamp: mock.now(), correlation_id: null, reply_to: null, metadata: { asked_by: agent, options: ["Convert in the adapter", "Update the export contract", "Wait for review"] } } });
      }
      await page.locator("body").click({ position: { x: 2, y: 2 } }); await page.keyboard.press("a"); await page.getByRole("dialog", { name: "question", exact: true }).waitFor(); break;
    case "workspace-disconnected": advanceScenario(mock, "disconnected"); await page.locator('[data-workspace-state="not-running"]').waitFor(); break;
    case "workspace-stopped": mock.stopDaemon(); await page.locator('[data-workspace-state="not-running"]').waitFor(); break;
    case "workspace-removed": mock.remove(mock.workspace().id); await page.locator('[data-workspace-state="removed"]').waitFor(); break;
    case "dashboard-stopped": await mock.shutdown(); await page.locator('[data-conn="stopped"]').waitFor(); break;
    default: {
      const mode: ReadMode = surface === "pending" || surface.endsWith("-pending") ? "pending" : surface === "error" || surface.endsWith("-error") ? "error" : surface === "transcript-empty" || surface === "diff-empty" ? "empty" : surface === "transcript-unavailable" || surface === "terminal-ended" ? "unavailable" : surface === "transcript-truncated" ? "truncated" : "ready";
      if (surface === "diff" || surface === "diff-empty") read("git_diff", mode);
      else if (surface === "terminal" || surface === "terminal-ended") read("agent_output", mode);
      else if (surface.startsWith("diff-")) read("git_diff", mode);
      else if (surface.startsWith("terminal-")) read("agent_output", mode);
      else if (surface.startsWith("inspector-")) { read("inbox", mode); read("events", mode); }
      else if (surface !== "inspector" && surface !== "model" && surface !== "context-menu") read("agent_transcript", mode);
      await route(page, mock, `?agent=${encodeURIComponent(agent)}`);
      await page.locator(`[data-inspector="${agent}"]`).waitFor();
      if (surface === "context-menu") {
        await page.locator(`[data-agent="${agent}"]`).evaluate((el) => el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 180, clientY: 250 })));
        await page.getByRole("menu").waitFor();
      } else if (surface === "model") {
        await page.locator("body").click({ position: { x: 2, y: 2 } }); await page.keyboard.press("s"); await page.getByRole("dialog", { name: "model and CLI", exact: true }).waitFor();
        await mock.waitForCall("models_list"); await page.locator(".models").waitFor();
      } else if (!surface.startsWith("inspector")) {
        const tab = surface.startsWith("diff") ? "diff" : surface.startsWith("terminal") ? "terminal" : "transcript";
        await page.getByRole("tab", { name: tab, exact: true }).evaluate((el) => (el as HTMLElement).click());
        const method = tab === "diff" ? "git_diff" : tab === "terminal" ? "agent_output" : "agent_transcript";
        await mock.waitForCall(method);
        if (mode === "pending") await page.getByRole("tab", { name: tab, exact: true }).waitFor();
        else if (mode === "error") await page.getByText(`Mock ${method} read failed.`, { exact: false }).waitFor();
        else if (tab === "transcript" && mode === "ready") await page.locator(".transcript-list .vrow").first().waitFor();
        else if (tab === "diff" && mode === "ready") await page.locator(".diff-text").waitFor();
        else if (tab === "terminal" && mode === "ready") await page.locator(".terminal-tab .screen").waitFor();
      }
    }
  }
  await frame(page);
  return null;
}

function galleryHtml() {
  const escape = (s: unknown) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
  const rows = captures.map((c) => `<article id="${escape(c.id)}"><h2>${escape(c.scenario)} · ${escape(c.surface)} · ${escape(c.theme)} · ${escape(c.viewport)}</h2><p>Fixture ${escape(c.fixture)}; ${escape(c.width)}×${escape(c.height)} CSS px; target <code>${escape(c.route ?? "unavailable")}</code></p>${c.image ? `<a href="${escape(c.image)}"><img loading="lazy" src="${escape(c.image)}" alt="${escape(c.scenario)} ${escape(c.surface)} ${escape(c.theme)} ${escape(c.viewport)} screenshot"></a>` : ""}<p>${escape(c.error ?? c.skipped ?? "")}</p><p>Contrast failures ${escape((c.audit as Awaited<ReturnType<typeof auditPage>> | undefined)?.contrast.failures.length ?? "n/a")}; document overflow ${escape((c.audit as Awaited<ReturnType<typeof auditPage>> | undefined)?.overflow ?? "n/a")}${c.overflowAllowed ? " (explicitly allowed)" : ""}</p><pre>${escape((c.a11yFailures as string[] | undefined)?.join("\n") ?? "")}</pre><pre>${escape(c.reproduce)}</pre><a href="#/gallery/${escape(c.scenario)}/${escape(c.surface)}/${escape(c.theme)}/${escape(c.viewport)}">Link to capture</a></article>`).join("\n");
  return `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Crew real-component screenshot gallery</title><link rel="stylesheet" href="gallery.css"><body><header><h1>Crew dashboard captures</h1><p>Built application against deterministic mock data. Fixture content only; no authentication data. Source ${escape(manifest.source)}. Contrast/target measurements need manual review; failures retained as evidence. Zoom uses a 720×480 CSS viewport at device scale 2.</p><p><a href="manifest.json">Manifest and accessibility evidence</a> · <a href="#">All captures</a></p></header><main>${rows}</main><script src="gallery.js"></script></body></html>`;
}
function report() {
  writeFileSync(join(out, "manifest.json"), JSON.stringify({ source: manifest.source, commit: execFileSync("git", ["rev-parse", "HEAD"], { cwd: ROOT, encoding: "utf8" }).trim(), fixture, scenarioClock: SCENARIO_TIME, strict: !!values.strict, allowedOverflow, tiers: "T0 and funded T1 facts/List modes; additional T2 filters/follow/metadata excluded", failures, captures }, null, 2) + "\n");
  writeFileSync(join(out, "index.html"), galleryHtml());
  writeFileSync(join(out, "gallery.css"), "body{font:14px/1.5 system-ui;margin:24px;background:#f5f6f3;color:#182321}main{display:grid;gap:24px;grid-template-columns:repeat(auto-fit,minmax(min(100%,480px),1fr))}article{background:white;padding:16px;min-width:0}img{width:100%;height:auto;border:1px solid #82948b}pre{white-space:pre-wrap;overflow-wrap:anywhere}a{color:#315ebe}[hidden]{display:none}");
  writeFileSync(join(out, "gallery.js"), `function select(){const p=location.hash.split('/');const id=p.length===6&&p[1]==='gallery'?p.slice(2).join('-'):null;for(const a of document.querySelectorAll('article'))a.hidden=!!id&&a.id!==id;}addEventListener('hashchange',select);select();`);
}
try {
  for (const scenario of scenarios) for (const theme of themes) for (const viewport of viewports) for (const surface of surfaces) {
    const size = VIEWPORTS[viewport];
    const id = `${scenario}-${surface}-${theme}-${viewport}`;
    const mock = await startMock({ scenario, fixture: fixture as FixtureName, dist });
    mock.prefs = { keys: true, theme: "system" };
    const context = await browser.newContext({ viewport: size, colorScheme: theme, reducedMotion: "reduce", deviceScaleFactor: viewport === "zoom" ? 2 : 1, locale: "en-US", timezoneId: "UTC" });
    const page = await context.newPage();
    page.setDefaultTimeout(5000);
    page.setDefaultNavigationTimeout(15000);
    await page.clock.setFixedTime(SCENARIO_TIME);
    const releases: (() => void)[] = [];
    const errors: string[] = [], csp: string[] = [], afterLoadRequests: string[] = [];
    let loaded = false;
    page.on("load", () => { loaded = true; });
    page.on("request", (r) => {
      if (loaded && !r.isNavigationRequest()) afterLoadRequests.push(`${r.method()} ${new URL(r.url()).pathname}`);
    });
    page.on("pageerror", (e) => errors.push(e.message));
    await page.exposeBinding("__galleryCsp", (_source, text: string) => csp.push(text));
    await page.addInitScript(() => document.addEventListener("securitypolicyviolation", (e) => (window as unknown as { __galleryCsp(s: string): void }).__galleryCsp(`${e.effectiveDirective}: ${e.blockedURI}`)));
    const reproduce = `bun run gallery --out ${shellQuote(join(out, "repro", id))} --scenario ${scenario} --surface ${surface} --theme ${theme} --viewport ${viewport} --fixture ${fixture}${values.dist ? ` --dist ${shellQuote(dist)}` : ""}${values.strict ? " --strict" : ""}${allowedOverflow.includes(viewport) ? ` --allow-overflow ${viewport}` : ""}`;
    const capture: Record<string, unknown> = { id, fixture, scenario, surface, theme, viewport, width: size.width, height: size.height, deviceScaleFactor: viewport === "zoom" ? 2 : 1, reducedMotion: true, reproduce };
    try {
      await page.goto(mock.pairUrl());
      await page.locator('[data-workspace-state="live"]').waitFor();
      if (mock.state.questions.length) await dismissQuestions(page, mock.state.questions.length);
      const skipped = await prepare(page, mock, surface, releases);
      if (skipped) capture.skipped = skipped;
      else {
        if (!["help", "confirm", "model", "question"].includes(surface)) await assertNoDialog(page, surface);
        await page.evaluate(() => scrollTo(0, 0));
        capture.route = await page.evaluate(() => location.hash);
        capture.audit = await auditPage(page);
        capture.a11yFailures = a11yFailures(capture.audit as Awaited<ReturnType<typeof auditPage>>, allowedOverflow.includes(viewport));
        capture.overflowAllowed = allowedOverflow.includes(viewport);
        capture.image = `${id}.png`;
        await page.screenshot({ path: join(out, String(capture.image)), animations: "disabled" });
      }
      capture.pageErrors = errors;
      capture.csp = csp.map((s) => s.replaceAll(mock.origin, "<mock-origin>"));
      capture.afterLoadRequests = afterLoadRequests;
      capture.failed = !!(errors.length || csp.length || afterLoadRequests.length || (values.strict && (capture.a11yFailures as string[] | undefined)?.length));
      if (capture.failed) failures++;
    } catch (error) {
      // Pairing fragments and ephemeral origins must never enter the report.
      capture.error = String(error).replaceAll(mock.origin, "<mock-origin>").replace(/#pair=[^\s"']+/g, "#pair=<redacted>");
      failures++;
    } finally {
      releases.forEach((release) => release());
      await context.close(); await mock.stop();
      captures.push(capture); report();
      console.log(`${id}: ${capture.error || capture.failed ? "failed" : capture.skipped ? "skipped" : "captured"}`);
    }
  }
} finally {
  await browser.close(); report();
}
console.log(`${captures.filter((c) => c.image).length} images, ${failures} failures; ${join(out, "index.html")}`);
if (failures) process.exitCode = 1;
