// §perf against a real daemon (notes/web-design.md §tests "perf.spec.ts"): a team of ten
// scripted agents, real messages and a real terminal. Prints a report; fails
// when a limit is exceeded. Timing limits scale with CREW_PERF_SCALE, counts never do.
import type { Page } from "@playwright/test";
import { expect, test, teamYaml } from "./harness.ts";

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
  daemon.start(teamYaml(daemon.ws, daemon.vcs, 5));
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
