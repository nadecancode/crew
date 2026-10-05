// The measurements of notes/web-design.md §perf and notes/web-multi.md
// §multi-perf, against the built bundle and
// the mock (synthetic load through the real WebSocket, decoders and
// reducers). Prints one report and fails when a limit is exceeded. Timing
// limits scale with CREW_PERF_SCALE on a slow machine; counts never do.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import type { Mock } from "./fixtures.ts";
import type { MockState } from "../dev/mock.ts";
import { expect, test } from "./fixtures.ts";

const SCALE = Number(process.env.CREW_PERF_SCALE ?? 1);
const report: Record<string, unknown> = {};
test.describe.configure({ mode: "serial" });
test.afterAll(() => console.log(`\nREPORT ${JSON.stringify(report, null, 1)}`));

type Stats = { n: number; p50: number | null; p95: number | null; max: number | null };
function stats(values: number[]): Stats {
  const s = [...values].sort((a, b) => a - b);
  const q = (p: number) => (s.length ? +s[Math.min(s.length - 1, Math.floor(p * s.length))].toFixed(2) : null);
  return { n: s.length, p50: q(0.5), p95: q(0.95), max: q(1) };
}

/** Grow a workspace's state to the live budget's size: 300 messages, 1000 events. */
function grow(mock: Mock, st: MockState = mock.state) {
  const ids = st.agents.map((a) => a.id as string);
  const t = mock.now();
  let seq = Math.max(0, ...st.events.map((e) => e.seq as number));
  while (st.messages.length < 300) {
    const n = st.messages.length;
    st.messages.push({ id: `m-grow-${n}`, from: ids[n % ids.length], to: ids[(n + 1) % ids.length], type: "message", body: `message ${n} about the work`, timestamp: t - 300_000 + n, correlation_id: null, reply_to: null, metadata: null });
  }
  while (st.events.length < 1000) {
    const n = st.events.length;
    st.events.push({ seq: ++seq, timestamp: t - 300_000 + n, source: "bench", event_type: "bench.tick", agent_id: ids[n % ids.length], task_id: null, message: `synthetic event ${n}`, level: n % 17 === 0 ? "warn" : "info", data: null });
  }
}

/** Push at `rate` per second for `ms`: events, one in ten an agent update, one in ten a message (a pulse). */
async function load(mock: Mock, rate: number, ms: number): Promise<number> {
  const ids = mock.state.agents.map((a) => a.id as string);
  let seq = Math.max(0, ...mock.state.events.map((e) => e.seq as number)) + 1;
  let sent = 0;
  const start = Date.now();
  await new Promise<void>((done) => {
    const timer = setInterval(() => {
      const due = Math.floor(((Date.now() - start) * rate) / 1000) - sent;
      for (let i = 0; i < due; i++, sent++) {
        const id = ids[sent % ids.length];
        const kind = sent % 10;
        if (kind === 0) mock.push({ kind: "message", data: { id: `m-load-${seq++}`, from: id, to: ids[(sent + 1) % ids.length], type: "message", body: "x", timestamp: Date.now(), correlation_id: null, reply_to: null, metadata: null } });
        else if (kind === 1) mock.push({ kind: "agent", data: { ...(mock.state.agents.find((a) => a.id === id) as object), activity: `step ${sent}`, progress: sent % 100 } });
        else mock.push({ kind: "event", data: { seq: seq++, timestamp: Date.now(), source: "bench", event_type: "bench.tick", agent_id: id, task_id: null, message: `synthetic event ${sent}`, level: "info", data: null } });
      }
      if (Date.now() - start >= ms) {
        clearInterval(timer);
        done();
      }
    }, 2);
  });
  return sent;
}

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

const perf = (page: Page) => page.evaluate(() => (window as unknown as { crewPerf: { log: { apply: number[]; toFrame: number[]; lag: number[]; live: number | null; switches: number[] } } }).crewPerf.log);
const measure = (page: Page, on: boolean) => page.evaluate((on) => (window as unknown as { crewPerf: { measure(on: boolean): void } }).crewPerf.measure(on), on);

test("P1–P4: the bundle's sizes", () => {
  const dist = join(process.cwd(), "..", "crates", "crew", "web-dist");
  const manifest = JSON.parse(readFileSync(join(dist, "manifest.json"), "utf8")) as { files: { path: string; bytes: number }[] };
  const sum = (f: (p: string) => boolean) => manifest.files.filter((x) => f(x.path)).reduce((n, x) => n + x.bytes, 0);
  const terminal = (p: string) => /assets\/(Terminal|xterm)-/.test(p);
  const sizes = {
    p1_dashboard_script: sum((p) => p.endsWith(".js") && !terminal(p)),
    p2_stylesheet: sum((p) => p.endsWith(".css")),
    p3_terminal_chunk: sum((p) => p.endsWith(".js") && terminal(p)),
    p4_web_dist: sum(() => true),
  };
  report.sizes_bytes = sizes;
  expect(sizes.p1_dashboard_script).toBeLessThanOrEqual(230_500);
  expect(sizes.p2_stylesheet).toBeLessThanOrEqual(34_000);
  expect(sizes.p3_terminal_chunk).toBeLessThanOrEqual(350_000);
  expect(sizes.p4_web_dist).toBeLessThanOrEqual(600_000);
});

test.describe("ten agents", () => {
  test.use({ mockOptions: { fixture: "team10" }, viewport: { width: 1440, height: 900 } });

  test("P5, P6: first paint and live, cold cache", async ({ browser, mock }) => {
    grow(mock);
    const runs: { fcp: number | null; live: number | null }[] = [];
    for (let i = 0; i < 7; i++) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await context.newPage();
      await page.goto(mock.pairUrl());
      await page.locator('[data-conn="live"]').waitFor({ state: "attached" });
      await expect(page.locator(".node.member")).toHaveCount(10);
      await expect.poll(async () => (await perf(page)).live).not.toBeNull();
      runs.push(await page.evaluate(() => {
        const fcp = performance.getEntriesByType("paint").find((p) => p.name === "first-contentful-paint")?.startTime;
        const live = (window as unknown as { crewPerf: { log: { live: number | null } } }).crewPerf.log.live;
        return { fcp: fcp == null ? null : +fcp.toFixed(1), live: live == null ? null : +live.toFixed(1) };
      }));
      await context.close();
    }
    const fcp = stats(runs.map((r) => r.fcp!));
    const live = stats(runs.map((r) => r.live!));
    report.p5_first_contentful_paint_ms = fcp;
    report.p6_live_ms = live;
    expect(fcp.max!).toBeLessThanOrEqual(150 * SCALE);
    expect(live.max!).toBeLessThanOrEqual(300 * SCALE);
  });

  test("P7, P8, P9: pushes, one at a time", async ({ page, mock, openPage }) => {
    await openPage();
    await measure(page, true);
    for (let i = 0; i < 60; i++) {
      mock.push({ kind: "event", data: { seq: 100_000 + i, timestamp: Date.now(), source: "bench", event_type: "bench.one", agent_id: "claude-1", task_id: null, message: `one ${i}`, level: "info", data: null } });
      if (i % 3 === 0) mock.push({ kind: "agent", data: { ...(mock.state.agents[0] as object), activity: `one ${i}` } });
      await page.waitForTimeout(25);
    }
    await page.waitForTimeout(200);
    const log = await perf(page);
    const r = { p7_daemon_to_handler_ms: stats(log.lag), p8_apply_ms: stats(log.apply), p9_handler_to_presented_frame_ms: stats(log.toFrame) };
    report.single_pushes = r;
    expect(r.p7_daemon_to_handler_ms.p95!).toBeLessThanOrEqual(10 * SCALE);
    expect(r.p8_apply_ms.p95!).toBeLessThanOrEqual(1 * SCALE);
    expect(r.p9_handler_to_presented_frame_ms.p95!).toBeLessThanOrEqual(35 * SCALE);
  });

  test("P10, P11, P12, P15: frames under load, rows in the DOM, the heap", async ({ page, mock, openPage }) => {
    await openPage();
    // Fill the log to 5000 events first.
    await load(mock, 4000, 1500);
    await measure(page, true);
    const out: Record<string, unknown> = {};
    // On the graph, where one push in ten starts a pulse; then on the log.
    for (const [name, rate, ms, limit, view] of [
      ["p10_steady_200_per_s_graph", 200, 5000, 18, "graph"],
      ["p11_burst_2000_per_s_graph", 2000, 3000, 20, "graph"],
      ["p10_steady_200_per_s_log", 200, 3000, 18, "logs"],
      ["p11_burst_2000_per_s_log", 2000, 3000, 20, "logs"],
    ] as const) {
      if (view === "logs" && !(await page.locator(".logs").isVisible())) await page.keyboard.press("l");
      await expect(page.locator(view === "logs" ? ".logs" : ".agent-graph")).toBeVisible();
      const f = frames(page, ms);
      const sent = await load(mock, rate, ms);
      const { intervals, long } = await f;
      const fs = stats(intervals);
      out[name] = { sent, frame_ms: fs, long_tasks: long.length };
      expect(fs.p95!, name).toBeLessThanOrEqual(limit * SCALE);
      expect(long.length, name).toBe(0);
    }
    const log = await perf(page);
    out.apply_ms_under_load = stats(log.apply);
    const rows = await page.locator(".log-row").count();
    const visible = await page.locator(".log-list").evaluate((el) => Math.ceil(el.clientHeight / (innerWidth < 768 ? 64 : 36)));
    out.p12_log_rows_in_dom = { rows, visible, events: await page.locator(".logs .muted").first().textContent() };
    expect(rows).toBeLessThanOrEqual(visible + 20 + 1);
    // Both rings full: 5000 events, 1000 messages.
    out.p15_heap_mb = await page.evaluate(() => {
      const m = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory;
      return m ? +(m.usedJSHeapSize / 1048576).toFixed(1) : null;
    });
    if (out.p15_heap_mb !== null) expect(out.p15_heap_mb as number).toBeLessThanOrEqual(60);
    report.load = out;
  });
});

test.describe("two workspaces of ten agents", () => {
  test.use({
    mockOptions: { workspaces: [{ fixture: "team10" }, { fixture: "team10", project: "search", workspace: "/home/op/work/search" }] },
    viewport: { width: 1440, height: 900 },
  });

  test("P17: a switch, from the key in the switcher to the first frame with the other workspace's snapshot applied", async ({ page, mock, openPage }) => {
    grow(mock, mock.workspace(0).state);
    grow(mock, mock.workspace(1).state);
    await openPage();
    await expect(page.locator(".node.member")).toHaveCount(10);
    await measure(page, true);
    const n = 20;
    for (let i = 0; i < n; i++) {
      await page.keyboard.press("w");
      await page.keyboard.press(i % 2 === 0 ? "2" : "1");
      await expect(page.locator("[data-switcher-button]")).toHaveText(i % 2 === 0 ? "search ▾" : "checkout-rewrite ▾");
      await expect.poll(async () => (await perf(page)).switches.length).toBe(i + 1);
      await expect(page.locator(".node.member")).toHaveCount(10);
    }
    await measure(page, false);
    const s = stats((await perf(page)).switches);
    report.p17_switch_ms = s;
    expect(s.p95!).toBeLessThanOrEqual(150 * SCALE);
  });
});

test.describe("five workspaces", () => {
  test.use({
    mockOptions: { workspaces: ["payments", "search", "billing", "infra", "docs"].map((project) => ({ fixture: "snapshot" as const, project, workspace: `/home/op/work/${project}` })) },
  });

  test("P19: an idle page with five joined workspaces receives no index frame and makes no call in 60 s", async ({ page, mock, openPage }) => {
    test.setTimeout(120_000);
    await openPage();
    await page.waitForTimeout(1000);
    const index = () => mock.connections.filter((c) => c.workspace === null);
    const sentBefore = index().reduce((n, c) => n + c.sent, 0);
    const callsBefore = mock.calls.length;
    await page.waitForTimeout(60_000);
    const out = {
      index_frames: index().reduce((n, c) => n + c.sent, 0) - sentBefore,
      calls: mock.calls.slice(callsBefore).filter((c) => c.method !== "ping").map((c) => c.method),
    };
    report.p19_idle_60s = out;
    expect(out.index_frames).toBe(0);
    expect(out.calls).toEqual([]);
  });
});

test("P13: an idle page asks for no animation frame and no timer under a second", async ({ page, openPage }) => {
  await openPage();
  await page.waitForTimeout(500);
  const idle = await page.evaluate(async () => {
    let frames = 0, short = 0;
    const raf = window.requestAnimationFrame, st = window.setTimeout, si = window.setInterval;
    window.requestAnimationFrame = (cb) => (frames++, raf(cb));
    window.setTimeout = ((fn: TimerHandler, ms?: number, ...a: unknown[]) => ((ms ?? 0) < 1000 && short++, st(fn, ms, ...a))) as typeof setTimeout;
    window.setInterval = ((fn: TimerHandler, ms?: number, ...a: unknown[]) => ((ms ?? 0) < 1000 && short++, si(fn, ms, ...a))) as typeof setInterval;
    await new Promise((r) => st(r, 3000));
    window.requestAnimationFrame = raf;
    window.setTimeout = st;
    window.setInterval = si;
    return { frames, short_timers: short };
  });
  report.p13_idle_3s = idle;
  expect(idle.frames).toBe(0);
  expect(idle.short_timers).toBe(0);
});

test("P14: terminal, key to echo", async ({ page, mock, openPage }) => {
  await openPage();
  mock.push({ kind: "agent", data: { ...(mock.state.agents[0] as object), id: "claude-9", backend: "pty", status: "working" } });
  await page.locator('[data-agent="claude-9"] .node-head').click();
  await page.getByRole("tab", { name: "terminal" }).click();
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
  const s = stats(echo);
  report.p14_key_to_echo_ms = s;
  expect(s.p95!).toBeLessThanOrEqual(20 * SCALE);
});
