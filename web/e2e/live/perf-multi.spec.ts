// §multi-perf against real daemons and the real server (notes/web-multi.md §multi-tests "Live"):
// P17 a switch, P18 a question in a workspace that is not on screen, P19 an idle page with five
// joined workspaces, P20 the server's memory. Prints a report; fails when a limit is exceeded.
// Timing limits scale with CREW_PERF_SCALE, counts never do.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import { CREW_BIN, expect, test, teamYaml } from "./harness.ts";
import type { Daemon } from "./harness.ts";
import { signedIn } from "./rawws.mjs";

const SCALE = Number(process.env.CREW_PERF_SCALE ?? 1);
const report: Record<string, unknown> = {};
test.afterAll(() => console.log(`\nMULTI REPORT ${JSON.stringify(report, null, 1)}`));
test.use({ viewport: { width: 1440, height: 900 } });

type Stats = { n: number; p50: number | null; p95: number | null; max: number | null };
function stats(values: number[]): Stats {
  const s = [...values].sort((a, b) => a - b);
  const q = (p: number) => (s.length ? +s[Math.min(s.length - 1, Math.floor(p * s.length))].toFixed(2) : null);
  return { n: s.length, p50: q(0.5), p95: q(0.95), max: q(1) };
}

const button = (page: Page) => page.locator("[data-switcher-button]");
const list = (page: Page) => page.getByRole("listbox", { name: "workspaces" });

test("P17: a switch, from enter in the switcher to the other workspace's first frame with its snapshot applied", async ({ page, daemon: A, sibling, openLive }) => {
  test.setTimeout(180_000);
  const B = sibling("beta");
  A.start(teamYaml(A.ws, A.vcs, 4, { project: A.project }));
  B.start(teamYaml(B.ws, B.vcs, 4, { project: "beta" }));
  // History for each, as the budget names it: 300 messages and 1000 events in each snapshot.
  const history = async (d: Daemon) => {
    const info = d.web();
    const label = new URL(info.url).host;
    const op = (await signedIn(info.port, label, `http://${label}`, new URLSearchParams(new URL(info.pair_url).hash.slice(1)).get("pair")!, 6, info.workspace)) as unknown as { send(m: string, p: object): number; replies: number; errors: number };
    const r0 = op.replies;
    for (let i = 0; i < 330; i++) op.send("send", { to: ["idle", "echo", "w1", "w2", "w3", "w4"][i % 6], type: "message", body: `history ${i}`, id: `w-hist-${i}` });
    while (op.replies - r0 < 330) await new Promise((r) => setTimeout(r, 50));
    expect(op.errors).toBe(0);
    const messages = (JSON.parse(d.c(["messages", "--json", "-n", "500"])) as unknown[]).length;
    let events = 0;
    d.until(`${d.project}: 1000 events`, () => ((events = Math.max(...d.events().map((e) => e.seq))), events >= 1000), 60_000);
    expect(messages, `${d.project}: messages in the snapshot`).toBeGreaterThanOrEqual(300);
    expect(events, `${d.project}: events in the snapshot`).toBeGreaterThanOrEqual(1000);
    return { messages: Math.min(messages, 300), events: Math.min(events, 1000) };
  };
  const occupancy = { [A.project]: await history(A), beta: await history(B) };
  await openLive(A.web());
  B.web();
  await button(page).click();
  await expect(list(page).getByRole("option")).toHaveCount(2, { timeout: 15_000 });
  await page.keyboard.press("Escape");
  // In the page: a switch starts at the `enter` that picks the row, and ends at the first frame in which the
  // workspace says live, shows the target's agents and the switcher names it.
  await page.evaluate(() => {
    const w = window as unknown as { __sw: { t0: number | null; done: number[]; want: string; agents: number } };
    w.__sw = { t0: null, done: [], want: "", agents: 0 };
    document.addEventListener("keydown", (e) => { if (e.key === "Enter" && document.querySelector(".switcher-pop")) w.__sw.t0 = performance.now(); }, true);
    const check = () => {
      if (w.__sw.t0 === null) return;
      const page = document.querySelector("[data-workspace-state]");
      const btn = document.querySelector("[data-switcher-button]")?.textContent ?? "";
      if (page?.getAttribute("data-workspace-state") === "live" && btn.startsWith(w.__sw.want) && document.querySelectorAll(".node.member").length === w.__sw.agents) {
        const t0 = w.__sw.t0;
        w.__sw.t0 = null;
        requestAnimationFrame(() => w.__sw.done.push(performance.now() - t0));
      }
    };
    new MutationObserver(check).observe(document.body, { subtree: true, childList: true, attributes: true, characterData: true });
  });
  for (let i = 0; i < 24; i++) {
    const [want, agents] = i % 2 === 0 ? ["beta", 10] : [A.project, 10];
    await page.evaluate(([w, n]) => { const s = (window as unknown as { __sw: { want: string; agents: number } }).__sw; s.want = w as string; s.agents = n as number; }, [want, agents]);
    await page.keyboard.press("w");
    await expect(list(page)).toBeFocused();
    await page.keyboard.type(want);
    await page.keyboard.press("Enter");
    await expect.poll(() => page.evaluate(() => (window as unknown as { __sw: { done: number[] } }).__sw.done.length)).toBe(i + 1);
    await page.waitForTimeout(150);
  }
  const done = await page.evaluate(() => (window as unknown as { __sw: { done: number[] } }).__sw.done);
  report.p17_switch_ms = { ...stats(done), agents: 10, snapshot_occupancy: occupancy };
  expect(stats(done).p95!).toBeLessThanOrEqual(150 * SCALE);
});

test("P18: a question asked in a workspace that is not on screen: from the daemon's event to the badge", async ({ page, daemon: A, sibling, openLive }) => {
  test.setTimeout(120_000);
  const B = sibling("beta");
  A.start();
  B.start(teamYaml(B.ws, B.vcs, 0, { project: "beta", askerLoop: true }));
  await openLive(A.web());
  B.web();
  await button(page).click();
  await expect(list(page).getByRole("option")).toHaveCount(2, { timeout: 15_000 });
  await page.keyboard.press("Escape");
  await page.evaluate(() => {
    const w = window as unknown as { __q: { n: number; at: number }[] };
    w.__q = [];
    const read = () => Number(/\d+/.exec(document.querySelector(".switcher > .badge.q")?.textContent ?? "")?.[0] ?? 0);
    let last = 0;
    new MutationObserver(() => {
      const n = read();
      if (n !== last) (last = n, w.__q.push({ n, at: Date.now() }));
    }).observe(document.body, { subtree: true, childList: true, characterData: true });
  });
  const N = 12;
  for (let i = 0; i < N; i++) {
    B.send("asker", `ask ${i}`);
    await page.waitForTimeout(800);
  }
  await expect.poll(() => page.evaluate(() => (window as unknown as { __q: unknown[] }).__q.length), { timeout: 15_000 }).toBeGreaterThanOrEqual(N);
  const seen = await page.evaluate(() => (window as unknown as { __q: { n: number; at: number }[] }).__q);
  const asked = B.events().filter((e) => e.event_type === "question.asked").map((e) => (e as unknown as { timestamp: number }).timestamp).sort((a, b) => a - b);
  expect(asked.length).toBeGreaterThanOrEqual(N);
  const lat = seen.slice(0, N).map((s, i) => s.at - asked[i]);
  report.p18_question_badge_ms = stats(lat);
  expect(stats(lat).p95!).toBeLessThanOrEqual(500 * SCALE);
});

test("P19, P20: an idle page with five joined workspaces makes no index traffic; the server's memory", async ({ browser, daemon: A, sibling, openLive }) => {
  test.setTimeout(240_000);
  const others = ["beta", "gamma", "delta", "epsilon"].map((n) => sibling(n));
  A.start();
  for (const d of others) d.start(teamYaml(d.ws, d.vcs, 0, { project: d.project }));
  const info = A.web();
  for (const d of others) d.web();
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  const sockets: { kind: string; sent: number; received: number }[] = [];
  page.on("websocket", (ws) => {
    const s = { kind: "?", sent: 0, received: 0 };
    sockets.push(s);
    ws.on("framesent", (f) => {
      s.sent++;
      if (s.kind === "?") s.kind = /"workspace"/.test(String(f.payload)) ? "workspace" : /"hello"/.test(String(f.payload)) ? "index" : "?";
    });
    ws.on("framereceived", () => s.received++);
  });
  await openLive(info, page);
  await button(page).click();
  await expect(list(page).getByRole("option")).toHaveCount(5, { timeout: 20_000 });
  await page.keyboard.press("Escape");
  // Two more pages: three in all.
  const more = [];
  for (let i = 0; i < 2; i++) {
    const c = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const p = await c.newPage();
    await openLive(A.web(), p);
    more.push(c);
  }
  await page.waitForTimeout(3000);
  const index = sockets.filter((s) => s.kind === "index");
  expect(index.length).toBe(1);
  const before = { sent: index[0].sent, received: index[0].received };
  const serverPid = JSON.parse(readFileSync(join(A.root, "run/crew/web.json"), "utf8")).pid as number;
  // Wake-ups of the server while idle: the voluntary context switches of all its threads, sampled every 50 ms;
  // a bin in which the sum moved is one wake-up (a burst of switches is one).
  const switches = () => { let n = 0; for (const t of readdirSync(`/proc/${serverPid}/task`)) { const m = /voluntary_ctxt_switches:\s+(\d+)/.exec(readFileSync(`/proc/${serverPid}/task/${t}/status`, "utf8")); n += Number(m?.[1] ?? 0); } return n; };
  const wakeups: number[] = [];
  let last = switches();
  const t0 = Date.now();
  while (Date.now() - t0 < 60_000) {
    await page.waitForTimeout(50);
    const now = switches();
    if (now !== last) (wakeups.push(Date.now() - t0), last = now);
  }
  // Clusters closer than 200 ms are one.
  const clusters = wakeups.filter((t, i) => i === 0 || t - wakeups[i - 1] > 200);
  const gaps = clusters.slice(1).map((t, i) => t - clusters[i]);
  report.p20_server_wakeups_60s = { clusters: clusters.length, shortest_gap_ms: gaps.length ? Math.min(...gaps) : null, median_gap_ms: gaps.length ? [...gaps].sort((a, b) => a - b)[Math.floor(gaps.length / 2)] : null, open_connections: 3 * 2 + 4 * 0 };
  const idle = { index_frames_sent: index[0].sent - before.sent, index_frames_received: index[0].received - before.received };
  report.p19_idle_60s_five_workspaces = idle;
  expect(idle.index_frames_sent).toBe(0);
  expect(idle.index_frames_received).toBe(0);
  // Nothing recurs faster than the 5 s housekeeping tick except the pings (20 s per connection, three pages
  // with two connections each): at most 12 ticks and 18 pings in 60 s, with room for a few more.
  expect(clusters.length).toBeLessThanOrEqual(36);
  const rssKb = Number(/VmRSS:\s+(\d+) kB/.exec(readFileSync(`/proc/${serverPid}/status`, "utf8"))?.[1]);
  const release = CREW_BIN.includes("/release/");
  report.p20_server_rss_mb = { rss: +(rssKb / 1024).toFixed(1), build: release ? "release" : "debug" };
  expect(rssKb / 1024).toBeLessThanOrEqual(40);
  for (const c of more) await c.close();
  await ctx.close();
});
