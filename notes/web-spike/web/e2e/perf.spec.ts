// The measurements quoted in notes/web-design.md §perf. Prints one report.
import { execFileSync } from "node:child_process";
import { expect, test } from "@playwright/test";

const PORT = Number(process.env.PORT ?? 47615);
const url = `http://${process.env.LABEL ?? "spike"}.localhost:${PORT}/#k=spiketoken`;
const report: Record<string, unknown> = {};
test.afterAll(() => console.log("\nREPORT " + JSON.stringify(report, null, 1)));

const stats = `(a) => { const s = [...a].sort((x, y) => x - y); const q = (p) => s.length ? +s[Math.min(s.length - 1, Math.floor(p * s.length))].toFixed(2) : null; return { n: s.length, p50: q(0.5), p95: q(0.95), max: q(1) }; }`;

test("cold load", async ({ browser }) => {
  const runs: any[] = [];
  for (let i = 0; i < 7; i++) {
    const context = await browser.newContext();
    const page = await context.newPage();
    await page.goto(url);
    await expect(page.locator("#conn")).toHaveText("live");
    await expect(page.locator(".node")).toHaveCount(6);
    runs.push(await page.evaluate(() => {
      const fcp = performance.getEntriesByType("paint").find((p) => p.name === "first-contentful-paint")?.startTime;
      return { fcp: fcp ? +fcp.toFixed(1) : null, live: +performance.now().toFixed(1) };
    }));
    await context.close();
  }
  const live = runs.map((r) => r.live).sort((a, b) => a - b);
  report.cold_load_ms = { runs, live_median: live[3], live_max: live[6] };
  expect(live[6]).toBeLessThan(300);
});

test("real pushes: daemon to handler, handler to frame", async ({ page }) => {
  test.skip(!process.env.CREW || !process.env.WS, "needs CREW and WS (run.sh sets them)");
  await page.goto(url);
  await expect(page.locator("#conn")).toHaveText("live");
  for (let i = 0; i < 12; i++) {
    execFileSync(process.env.CREW!, ["send", "-C", process.env.WS!, `claude-${(i % 6) + 1}`, `ping ${i}`]);
    await page.waitForTimeout(150);
  }
  await page.waitForTimeout(300);
  const r = await page.evaluate(`(() => { const f = ${stats}; const P = window.__perf; return { daemon_to_handler_ms: f(P.lag), apply_ms: f(P.apply), handler_to_second_raf_ms: f(P.toFrame) }; })()`) as any;
  report.real_pushes = r;
  expect(r.daemon_to_handler_ms.n).toBeGreaterThan(10);
  expect(r.daemon_to_handler_ms.p95).toBeLessThan(20);
  expect(r.handler_to_second_raf_ms.p95).toBeLessThan(40);
});

test("frame cost: 10 agents, 5000 events, synthetic pushes", async ({ page }) => {
  await page.goto(url);
  await expect(page.locator("#conn")).toHaveText("live");
  const r = await page.evaluate(`(async () => {
    const f = ${stats}; const P = window.__perf;
    const base = JSON.parse(JSON.stringify(window.__state.state.agents[0]));
    for (let i = 7; i <= 10; i++) window.__conn.h.push({ kind: "agent", data: { ...base, id: "claude-" + i, role: "extra" } }, performance.now());
    const out = {};
    for (const [name, rate, ms] of [["fill_2000_per_s", 2000, 3000], ["steady_200_per_s", 200, 5000], ["burst_2000_per_s", 2000, 3000], ["burst_10000_per_s", 10000, 2000]]) {
      P.toFrame.length = 0; P.apply.length = 0;
      const long = [];
      const po = new PerformanceObserver((l) => l.getEntries().forEach((e) => long.push(e.duration)));
      po.observe({ entryTypes: ["longtask"] });
      const sent = await window.__bench(rate, ms);
      await new Promise((r) => setTimeout(r, 200));
      po.disconnect();
      out[name] = { sent, apply_ms: f(P.apply), frame_ms: f(P.frames), handler_to_second_raf_ms: f(P.toFrame), long_tasks: long.length, events: window.__state.events.length, dom_rows: document.querySelectorAll(".row").length, dom_nodes: document.querySelectorAll("*").length };
    }
    out.agents = window.__state.state.agents.length;
    out.heap_mb = performance.memory ? +(performance.memory.usedJSHeapSize / 1048576).toFixed(1) : null;
    return out;
  })()`) as any;
  report.synthetic = r;
  expect(r.agents).toBe(10);
  expect(r.steady_200_per_s.events).toBe(5000);
  for (const k of ["steady_200_per_s", "burst_2000_per_s"]) {
    expect(r[k].frame_ms.p95).toBeLessThan(20);
    expect(r[k].long_tasks).toBe(0);
    expect(r[k].dom_rows).toBeLessThan(80);
  }
});

test("terminal: key to echo, and an idle page asks for no frames", async ({ page }) => {
  await page.goto(url);
  await expect(page.locator("#conn")).toHaveText("live");
  const idle = await page.evaluate(async () => {
    let n = 0; const orig = window.requestAnimationFrame;
    window.requestAnimationFrame = (cb) => { n++; return orig(cb); };
    await new Promise((r) => setTimeout(r, 3000));
    window.requestAnimationFrame = orig;
    return n;
  });
  await page.getByRole("button", { name: /claude-3/ }).press("Enter");
  await expect.poll(() => page.evaluate(() => (window as any).__term?.bytes ?? 0)).toBeGreaterThan(0);
  const echo = await page.evaluate(`(async () => {
    const f = ${stats}; const T = window.__term, out = [];
    for (let i = 0; i < 30; i++) {
      const f0 = T.frames, t0 = performance.now();
      T.term.input("x");
      while (T.frames === f0 && performance.now() - t0 < 500) await new Promise((r) => setTimeout(r, 0));
      out.push(performance.now() - t0);
      await new Promise((r) => setTimeout(r, 30));
    }
    return f(out);
  })()`) as any;
  report.terminal = { key_to_echo_ms: echo, raf_requests_in_3s_idle: idle };
  expect(idle).toBe(0);
  expect(echo.p95).toBeLessThan(50);
});
