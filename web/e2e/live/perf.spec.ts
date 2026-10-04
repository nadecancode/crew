// §perf against a real daemon (notes/web-design.md §tests "perf.spec.ts"): a team of ten
// scripted agents, real messages and a real terminal. Prints a report; fails
// when a limit is exceeded. Timing limits scale with CREW_PERF_SCALE, counts never do.
import type { Page } from "@playwright/test";
import { expect, test, teamYaml } from "./harness.ts";
import { signedIn } from "./rawws.mjs";

const SCALE = Number(process.env.CREW_PERF_SCALE ?? 1);
const report: Record<string, unknown> = {};
test.afterAll(() => console.log(`\nLIVE REPORT ${JSON.stringify(report, null, 1)}`));

type Stats = { n: number; p50: number | null; p95: number | null; max: number | null };
function stats(values: number[]): Stats {
  const s = [...values].sort((a, b) => a - b);
  const q = (p: number) => (s.length ? +s[Math.min(s.length - 1, Math.floor(p * s.length))].toFixed(2) : null);
  return { n: s.length, p50: q(0.5), p95: q(0.95), max: q(1) };
}
type PerfLog = { apply: number[]; toFrame: number[]; lag: number[]; live: number | null };
const perf = (page: Page) => page.evaluate(() => (window as unknown as { crewPerf: { log: PerfLog } }).crewPerf.log);

test.use({ viewport: { width: 1440, height: 900 } });

test("P5–P9, P14 against a real daemon with ten agents", async ({ browser, daemon, openLive }) => {
  test.setTimeout(180_000);
  daemon.start(teamYaml(daemon.ws, daemon.vcs, 4));
  // History for the snapshot: real messages and the events they leave.
  for (let i = 0; i < 100; i++) daemon.send(i % 2 ? "echo" : "idle", `history ${i}`);
  const info = () => daemon.web();

  // P5, P6: first paint and live, cold cache.
  const runs: { fcp: number | null; live: number | null }[] = [];
  for (let i = 0; i < 7; i++) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    await openLive(info(), page);
    await expect(page.locator(".node.member")).toHaveCount(10);
    await expect.poll(async () => (await perf(page)).live).not.toBeNull();
    runs.push(
      await page.evaluate(() => {
        const fcp = performance.getEntriesByType("paint").find((p) => p.name === "first-contentful-paint")?.startTime;
        const live = (window as unknown as { crewPerf: { log: PerfLog } }).crewPerf.log.live;
        return { fcp: fcp == null ? null : +fcp.toFixed(1), live: live == null ? null : +live.toFixed(1) };
      }),
    );
    await context.close();
  }
  const fcp = stats(runs.map((r) => r.fcp!));
  const live = stats(runs.map((r) => r.live!));
  report.p5_first_contentful_paint_ms = fcp;
  report.p6_live_ms = live;

  // P7, P8, P9: pushes one at a time, real messages through the real relay.
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  await openLive(info(), page);
  await page.evaluate(() => (window as unknown as { crewPerf: { measure(on: boolean): void } }).crewPerf.measure(true));
  for (let i = 0; i < 60; i++) {
    daemon.send("echo", `one ${i}`);
    await page.waitForTimeout(25);
  }
  await page.waitForTimeout(500);
  const log = await perf(page);
  report.single_pushes = { p7_daemon_to_handler_ms: stats(log.lag), p8_apply_ms: stats(log.apply), p9_handler_to_presented_frame_ms: stats(log.toFrame) };

  // P14: a key typed in the terminal tab to its echo.
  await page.locator('[data-agent="idle"] .node-head').click();
  await page.getByRole("tab", { name: "terminal", exact: true }).click();
  await expect(page.locator(".xterm")).toBeVisible();
  await expect.poll(() => page.locator(".xterm-host").evaluate((el) => (el as unknown as { crewTerminal?: { bytes: number } }).crewTerminal?.bytes ?? 0)).toBeGreaterThan(0);
  const echo = await page.locator(".xterm-host").evaluate(async (el) => {
    const t = (el as unknown as { crewTerminal: { frames: number; term: { input(s: string): void } } }).crewTerminal;
    const out: number[] = [];
    for (let i = 0; i < 30; i++) {
      const f0 = t.frames, t0 = performance.now();
      t.term.input("x");
      while (t.frames === f0 && performance.now() - t0 < 500) await new Promise((r) => setTimeout(r, 0));
      out.push(performance.now() - t0);
      await new Promise((r) => setTimeout(r, 30));
    }
    return out;
  });
  report.p14_key_to_echo_ms = stats(echo);
  await context.close();

  expect(fcp.max!).toBeLessThanOrEqual(150 * SCALE);
  expect(live.max!).toBeLessThanOrEqual(300 * SCALE);
  const sp = report.single_pushes as { p7_daemon_to_handler_ms: Stats; p8_apply_ms: Stats; p9_handler_to_presented_frame_ms: Stats };
  expect(sp.p7_daemon_to_handler_ms.p95!).toBeLessThanOrEqual(10 * SCALE);
  expect(sp.p8_apply_ms.p95!).toBeLessThanOrEqual(1 * SCALE);
  expect(sp.p9_handler_to_presented_frame_ms.p95!).toBeLessThanOrEqual(35 * SCALE);
  expect((report.p14_key_to_echo_ms as Stats).p95!).toBeLessThanOrEqual(20 * SCALE);
});

/** Frame intervals and long tasks in the page for `ms`. */
function frames(page: Page, ms: number) {
  return page.evaluate(async (ms) => {
    const out: number[] = [];
    const long: number[] = [];
    const po = new PerformanceObserver((l) => l.getEntries().forEach((e) => long.push(e.duration)));
    po.observe({ entryTypes: ["longtask"] });
    await new Promise<void>((done) => {
      const start = performance.now();
      let prev = start;
      const tick = (t: number) => {
        out.push(t - prev);
        prev = t;
        if (t - start < ms) requestAnimationFrame(tick);
        else done();
      };
      requestAnimationFrame(tick);
    });
    po.disconnect();
    return { intervals: out.slice(1), long };
  }, ms);
}

type Operator = { send(method: string, params?: object): number; call(method: string, params?: object): Promise<{ result?: unknown }>; replies: number; close(): void };

/** Operator `send` calls at `rate` per second for `ms` through a signed-in web socket: each is a message
 * push (a pulse on the graph) and the events it leaves. Returns how many were sent. */
async function load(op: Operator, rate: number, ms: number, to: string[]): Promise<number> {
  let sent = 0;
  const start = Date.now();
  await new Promise<void>((done) => {
    const timer = setInterval(() => {
      const due = Math.floor(((Date.now() - start) * rate) / 1000) - sent;
      for (let i = 0; i < due; i++, sent++) op.send("send", { to: to[sent % to.length], type: "message", body: `load ${sent}`, id: `w-load-${Date.now().toString(16)}-${sent}` });
      if (Date.now() - start >= ms) (clearInterval(timer), done());
    }, 2);
  });
  return sent;
}

test("P10, P11, P12, P15 against a real daemon: frames under real pushes, rows in the DOM, the heap", async ({ browser, daemon, openLive }) => {
  test.setTimeout(240_000);
  daemon.start(teamYaml(daemon.ws, daemon.vcs, 4));
  const info = daemon.web();
  const label = new URL(info.url).host;
  const protocol = Number(JSON.parse(daemon.c(["status", "--json"])).session.protocol ?? 5);
  const op = (await signedIn(info.port, label, `http://${label}`, new URL(info.pair_url).hash.replace("#pair=", ""), protocol)) as unknown as Operator;
  const targets = ["idle", "echo", "w1", "w2", "w3", "w4"];
  // The log to 5000 events, at whatever rate the daemon takes.
  const lastSeq = () => Math.max(0, ...daemon.events().map((e) => e.seq));
  const t0 = Date.now();
  let preloaded = 0;
  while (lastSeq() < 5000 && Date.now() - t0 < 120_000) preloaded += await load(op, 1000, 1000, targets);
  report.live_preload = { messages: preloaded, last_event_seq: lastSeq(), seconds: +((Date.now() - t0) / 1000).toFixed(1) };

  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  await openLive(daemon.web(), page);
  await expect(page.locator(".node.member")).toHaveCount(10);
  await page.evaluate(() => (window as unknown as { crewPerf: { measure(on: boolean): void } }).crewPerf.measure(true));
  const out: Record<string, unknown> = {};
  for (const [name, rate, ms, limit, view] of [
    ["p10_steady_200_per_s_graph", 200, 5000, 18, "graph"],
    ["p11_burst_2000_per_s_graph", 2000, 3000, 20, "graph"],
    ["p10_steady_200_per_s_log", 200, 3000, 18, "logs"],
    ["p11_burst_2000_per_s_log", 2000, 3000, 20, "logs"],
  ] as const) {
    if (view === "logs" && !(await page.locator(".logs").isVisible())) await page.keyboard.press("l");
    await expect(page.locator(view === "logs" ? ".logs" : ".agent-graph")).toBeVisible();
    const seq0 = lastSeq();
    const f = frames(page, ms);
    const sent = await load(op, rate, ms, targets);
    const { intervals, long } = await f;
    const fs = stats(intervals);
    out[name] = { sent_per_s: Math.round((sent * 1000) / ms), events_emitted: lastSeq() - seq0, frame_ms: fs, long_tasks: long.length };
    expect(fs.p95!, name).toBeLessThanOrEqual(limit * SCALE);
    expect(long.length, name).toBe(0);
  }
  const rows = await page.locator(".log-row").count();
  const visible = await page.locator(".log-list").evaluate((el) => Math.ceil(el.clientHeight / 22));
  out.p12_log_rows_in_dom = { rows, visible };
  expect(rows).toBeLessThanOrEqual(visible + 20 + 1);
  out.p15_heap_mb = await page.evaluate(() => {
    const m = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory;
    return m ? +(m.usedJSHeapSize / 1048576).toFixed(1) : null;
  });
  if (out.p15_heap_mb !== null) expect(out.p15_heap_mb as number).toBeLessThanOrEqual(60);
  report.real_load = out;
  op.close();
  await context.close();
});
