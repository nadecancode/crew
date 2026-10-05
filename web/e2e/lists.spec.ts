// The bus and the event log (notes/web-design.md §ui "Bus", "Logs", §perf):
// both follow the tail while scrolled to the bottom and stay where the reader
// is when scrolled up; the log filters by agent, level, `vcs` and text; the
// bus filters to the pair of a link clicked in the graph; "load older" asks
// `events` (log) and `history` (bus) and keeps the reader's place; the lists
// are virtual, so 5000 events are a few dozen rows in the DOM.

import type { Locator, Page } from "@playwright/test";
import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
import type { Mock } from "./fixtures.ts";

const BUS_ROW = 84;
const LOG_ROW = 36;
const LOG_NARROW_ROW = 64;
const busList = (page: Page) => page.locator("#side-panel .bus-list");
const logList = (page: Page) => page.locator("#main .log-list");
const logCount = (page: Page) => page.getByRole("toolbar", { name: "event filters" }).locator("span.muted", { hasText: / of / });

function message(mock: Mock, id: string, from: string, to: string, body: string) {
  return { kind: "message", data: { id, from, to, type: "message", body, timestamp: mock.now(), correlation_id: null, reply_to: null, metadata: null } };
}

type EventOver = { source?: string; event_type?: string; agent_id?: string | null; level?: string; message?: string; timestamp?: number };
function event(seq: number, timestamp: number, over: EventOver = {}) {
  return { seq, timestamp, source: "daemon", event_type: "agent.status", agent_id: "claude-2", task_id: null, message: `claude-2 step ${seq}`, level: "info", data: null, ...over };
}

/** How many rows the list holds (its spacer is one row height per row). */
const rows = (list: Locator, rowHeight = LOG_ROW) => list.locator(".vlist-pad").evaluate((el, h) => Math.round(el.getBoundingClientRect().height / h), rowHeight);

/** The `attr` of the first row whose bottom is below the list's top edge. */
const firstVisible = (list: Locator, attr: string) =>
  list.evaluate((el, a) => {
    const top = el.getBoundingClientRect().top;
    const shown = [...el.querySelectorAll(`[${a}]`)]
      .map((r) => ({ r, box: r.getBoundingClientRect() }))
      .filter(({ box }) => box.bottom > top + 1)
      .sort((x, y) => x.box.top - y.box.top);
    return shown[0]?.r.getAttribute(a) ?? null;
  }, attr);

/** Scroll the list to `y` and wait for its scroll event (none comes when it is there already). */
const scrollTo = (list: Locator, y: number) =>
  list.evaluate(
    (el, to) =>
      new Promise<number>((done) => {
        const was = el.scrollTop;
        el.addEventListener("scroll", () => done(el.scrollTop), { once: true });
        el.scrollTop = to;
        if (el.scrollTop === was) done(was);
      }),
    y,
  );

const atBottom = (list: Locator) => list.evaluate((el) => el.scrollTop + el.clientHeight >= el.scrollHeight - 2);
const seqs = (page: Page) => logList(page).locator(".log-row").evaluateAll((els) => els.map((e) => Number(e.getAttribute("data-seq"))));

test.use({ viewport: { width: 1400, height: 900 } });

test("the bus follows the tail, and scrolled up it stays where the reader is", async ({ page, mock, csp, openPage }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  const list = busList(page);
  await expect(list.locator("[data-message='m-0007']")).toBeVisible();
  const before = await rows(list, BUS_ROW);

  for (let i = 1; i <= 60; i++) mock.push(message(mock, `m-b${String(i).padStart(3, "0")}`, "orchestrator", `claude-${(i % 6) + 1}`, `step ${i} of the plan`));
  await expect.poll(() => rows(list, BUS_ROW)).toBe(before + 60);
  await expect(list.locator("[data-message='m-b060']")).toBeInViewport();
  expect(await atBottom(list)).toBe(true);
  mock.push(message(mock, "m-b061", "claude-1", "claude-2", "one more"));
  await expect(list.locator("[data-message='m-b061']")).toBeInViewport();
  expect(await atBottom(list)).toBe(true);

  // Scrolled up: new messages do not move what is being read.
  const y = await scrollTo(list, 10 * BUS_ROW + 5);
  const first = await firstVisible(list, "data-message");
  expect(first).not.toBeNull();
  for (let i = 62; i <= 81; i++) mock.push(message(mock, `m-b${String(i).padStart(3, "0")}`, "claude-3", "claude-5", `later ${i}`));
  await expect.poll(() => rows(list, BUS_ROW)).toBe(before + 81);
  expect(await firstVisible(list, "data-message")).toBe(first);
  expect(await list.evaluate((el) => el.scrollTop)).toBe(y);
  await expect(list.locator("[data-message='m-b081']")).not.toBeInViewport();

  // Back at the bottom it follows again.
  await scrollTo(list, 1e6);
  mock.push(message(mock, "m-b082", "claude-3", "claude-5", "following again"));
  await expect(list.locator("[data-message='m-b082']")).toBeInViewport();

  expect(guard.requests).toEqual([]);
  expect(csp).toEqual([]);
});

for (const which of ["bus", "log"] as const) {
  test(`a full ${which} drops its oldest rows without moving the rows being read, and keeps the selected row`, async ({ page, mock, csp, openPage }) => {
    await openPage();
    const cap = which === "bus" ? 1000 : 5000;
    const rowHeight = which === "bus" ? BUS_ROW : LOG_ROW;
    const attr = which === "bus" ? "data-message" : "data-seq";
    const push = (i: number) =>
      mock.push(which === "bus"
        ? message(mock, `review-${i}`, "claude-3", "claude-5", `review ${i}`)
        : { kind: "event", data: event(100_000 + i, mock.now(), { message: `review ${i}` }) });
    if (which === "log") await page.keyboard.press("l");
    const list = which === "bus" ? busList(page) : logList(page);
    for (let i = 0; i < cap + 50; i++) push(i);
    const last = which === "bus" ? "review-" + (cap + 49) : String(100_000 + cap + 49);
    await expect(list.locator(`[${attr}="${last}"]`)).toBeInViewport();
    expect(await rows(list, rowHeight)).toBe(cap);
    // Scrolled up to row 400 of the full list.
    await scrollTo(list, 400 * rowHeight);
    const first = await firstVisible(list, attr);
    const want = which === "bus" ? "review-450" : String(100_450);
    expect(first).toBe(want);
    // Select a row with the keyboard (a click on a bus row opens the message).
    await list.focus();
    await page.keyboard.press("ArrowUp");
    const selected = list.locator(".vrow.selected").locator(`[${attr}]`);
    const picked = await selected.getAttribute(attr);
    expect(picked).not.toBeNull();
    for (let i = cap + 50; i < cap + 60; i++) push(i);
    await page.waitForTimeout(300);
    expect(await firstVisible(list, attr)).toBe(want);
    await expect(selected).toHaveAttribute(attr, picked!);
    expect(csp).toEqual([]);
  });
}

test("the bus filters to the pair of a link clicked in the graph; hovering a row lights the link", async ({ page, mock, csp, openPage }) => {
  // No pulses, so that the click lands on the link and not on a pulse in flight.
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openPage();
  const list = busList(page);
  await expect(list.locator(".bus-row")).toHaveCount(7);

  // Hovering a row lights the link between its two agents.
  await list.locator("[data-message='m-0001']").hover();
  await expect(page.locator(".agent-graph path.hover-link")).toHaveCount(1);
  await page.mouse.move(5, 300);
  await expect(page.locator(".agent-graph path.hover-link")).toHaveCount(0);

  mock.push(message(mock, "m-p2p", "claude-3", "claude-1", "renamed the macro"));
  const link = page.locator('[data-link="claude-3:claude-1"]');
  await expect(link).toBeAttached();
  // A point on the link itself (its middle), in page coordinates.
  const at = await link.locator(".link-hit").evaluate((p: SVGPathElement) => {
    const m = p.getPointAtLength(p.getTotalLength() / 2);
    const ctm = p.getScreenCTM()!;
    return { x: ctm.a * m.x + ctm.c * m.y + ctm.e, y: ctm.b * m.x + ctm.d * m.y + ctm.f };
  });
  await page.mouse.click(at.x, at.y);
  await expect(page.locator("#side-panel .chip.filter")).toContainText("claude-3 ↔ claude-1");
  const shown = list.locator(".bus-row");
  await expect(shown).toHaveCount(2);
  expect(await shown.evaluateAll((els) => els.map((e) => e.getAttribute("data-message")))).toEqual(["m-0006", "m-p2p"]);

  // New messages of other pairs stay out; one of the pair comes in.
  mock.push(message(mock, "m-other", "orchestrator", "claude-4", "not this pair"));
  mock.push(message(mock, "m-back", "claude-1", "claude-3", "thanks"));
  await expect(shown).toHaveCount(3);
  await expect(list.locator("[data-message='m-other']")).toHaveCount(0);

  await page.getByRole("button", { name: "clear the filter" }).click();
  await expect(page.locator("#side-panel .chip.filter")).toHaveCount(0);
  await expect(shown).toHaveCount(10);
  await expect.poll(() => rows(list, BUS_ROW)).toBe(10);
  await expect(list.locator("[data-message='m-back']")).toBeInViewport();
  expect(csp).toEqual([]);
});

test("the bus's load older asks history and keeps the reader's place", async ({ page, mock, csp, openPage }) => {
  // 400 messages: the page's snapshot holds the newest 300.
  const t0 = mock.now() - 400_000;
  mock.state.messages = Array.from({ length: 400 }, (_, i) => ({
    id: `m-h${String(i).padStart(3, "0")}`, from: "orchestrator", to: "claude-1", type: "message", body: `history ${i}`,
    timestamp: t0 + i * 1000, correlation_id: null, reply_to: null, metadata: null,
  }));
  await openPage();
  const list = busList(page);
  await expect.poll(() => rows(list, BUS_ROW)).toBe(300);
  await scrollTo(list, 0);
  expect(await firstVisible(list, "data-message")).toBe("m-h100");

  await page.locator("#side-panel").getByRole("button", { name: "Load older", exact: true }).click();
  expect((await mock.waitForCall("history")).params).toEqual({ limit: 600 });
  await expect.poll(() => rows(list, BUS_ROW)).toBe(400);
  // The same row is under the reader; the older ones are above it.
  expect(await firstVisible(list, "data-message")).toBe("m-h100");
  await scrollTo(list, 0);
  expect(await firstVisible(list, "data-message")).toBe("m-h000");
  expect(csp).toEqual([]);
});

test("the log follows the tail, and scrolled up it stays where the reader is", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await page.keyboard.press("l");
  const list = logList(page);
  await expect(list.locator("[data-seq='6']")).toBeVisible();
  await expect(logCount(page)).toHaveText("6 of 6");

  for (let seq = 101; seq <= 180; seq++) mock.push({ kind: "event", data: event(seq, mock.now()) });
  await expect(logCount(page)).toHaveText("86 of 86");
  await expect(list.locator("[data-seq='180']")).toBeInViewport();
  expect(await atBottom(list)).toBe(true);

  const y = await scrollTo(list, 20 * LOG_ROW + 7);
  const first = await firstVisible(list, "data-seq");
  expect(first).not.toBeNull();
  for (let seq = 181; seq <= 220; seq++) mock.push({ kind: "event", data: event(seq, mock.now()) });
  await expect(logCount(page)).toHaveText("126 of 126");
  expect(await firstVisible(list, "data-seq")).toBe(first);
  expect(await list.evaluate((el) => el.scrollTop)).toBe(y);
  await expect(list.locator("[data-seq='220']")).not.toBeInViewport();

  await scrollTo(list, 1e6);
  mock.push({ kind: "event", data: event(221, mock.now()) });
  await expect(list.locator("[data-seq='221']")).toBeInViewport();
  expect(csp).toEqual([]);
});

test("the log filters by agent, level, vcs and text", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await page.keyboard.press("l");
  // 30 more events: claude-2's steps, every fifth a warning, every tenth from claude-1.
  for (let seq = 101; seq <= 130; seq++) {
    const over: EventOver = {};
    if (seq % 5 === 0) Object.assign(over, { level: "warn", message: `claude-2 slow step ${seq}` });
    if (seq % 10 === 0) Object.assign(over, { agent_id: "claude-1", message: `claude-1 retried ${seq}` });
    mock.push({ kind: "event", data: event(seq, mock.now(), over) });
  }
  await expect(logCount(page)).toHaveText("36 of 36");
  const filters = page.getByRole("toolbar", { name: "event filters" });
  const [agent, level] = [filters.locator("select").nth(0), filters.locator("select").nth(1)];

  await agent.selectOption("claude-1");
  await expect(logCount(page)).toHaveText("5 of 36");
  expect(await seqs(page)).toEqual([5, 6, 110, 120, 130]);

  await level.selectOption("warn");
  await expect(logCount(page)).toHaveText("4 of 36");
  expect(await seqs(page)).toEqual([5, 110, 120, 130]);
  await level.selectOption("error");
  expect(await seqs(page)).toEqual([5]);

  await filters.getByRole("button", { name: "clear" }).click();
  await expect(logCount(page)).toHaveText("36 of 36");
  await level.selectOption("warn");
  await expect(logCount(page)).toHaveText("8 of 36");
  expect(await seqs(page)).toEqual([4, 5, 105, 110, 115, 120, 125, 130]);
  await level.selectOption("debug");

  const vcs = filters.getByRole("button", { name: "Version control", exact: true });
  await vcs.click();
  await expect(vcs).toHaveAttribute("aria-pressed", "true");
  await expect(logCount(page)).toHaveText("1 of 36");
  expect(await seqs(page)).toEqual([4]);
  await expect(logList(page).locator("[data-seq='4']")).toContainText("vcs.cherry_pick");
  await vcs.click();

  const text = filters.getByRole("searchbox", { name: "filter text" });
  await text.fill("crashed");
  await expect(logCount(page)).toHaveText("1 of 36");
  expect(await seqs(page)).toEqual([5]);
  await text.fill("RETRIED");
  expect(await seqs(page)).toEqual([110, 120, 130]);
  await text.fill("no such thing");
  await expect(logList(page)).toContainText("No events match these filters");

  // `f` filters to the selected agent.
  await filters.getByRole("button", { name: "clear" }).click();
  await expect(text).toHaveValue("");
  await page.keyboard.press("Escape");
  await page.keyboard.press("o");
  await page.keyboard.press("f");
  await expect(agent).toHaveValue("orchestrator");
  await expect(logCount(page)).toHaveText("0 of 36");
  await page.keyboard.press("f");
  await expect(agent).toHaveValue("");
  expect(csp).toEqual([]);
});

test("the log's load older asks events and keeps the reader's place", async ({ page, mock, csp, openPage }) => {
  // 1500 events: the page's snapshot holds the newest 1000.
  const t0 = mock.now() - 3_000_000;
  mock.state.events = Array.from({ length: 1500 }, (_, i) => event(i + 1, t0 + i * 1000));
  await openPage();
  await page.keyboard.press("l");
  const list = logList(page);
  await expect(logCount(page)).toHaveText("1000 of 1000");
  await scrollTo(list, 0);
  expect(await firstVisible(list, "data-seq")).toBe("501");

  await page.getByRole("toolbar", { name: "event filters" }).getByRole("button", { name: "Load older", exact: true }).click();
  expect((await mock.waitForCall("events", (c) => c.params?.agent === undefined)).params).toEqual({ limit: 2000 });
  await expect(logCount(page)).toHaveText("1500 of 1500");
  expect(await firstVisible(list, "data-seq")).toBe("501");
  await scrollTo(list, 0);
  expect(await firstVisible(list, "data-seq")).toBe("1");
  expect(csp).toEqual([]);
});

test("5000 events: the log keeps fewer than 80 rows in the DOM and still follows the tail", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await page.keyboard.press("l");
  await expect(logCount(page)).toHaveText("6 of 6");
  const t = mock.now();
  for (let seq = 101; seq <= 5100; seq++) mock.push({ kind: "event", data: event(seq, t, { message: `event number ${seq}` }) });
  // The ring holds 5000: the oldest six went.
  await expect(logCount(page)).toHaveText("5000 of 5000", { timeout: 20_000 });
  expect(await logList(page).locator(".log-row").count()).toBeLessThan(80);
  await expect(logList(page).locator("[data-seq='5100']")).toBeInViewport();
  // Scrolling through keeps it small.
  await scrollTo(logList(page), 2500 * LOG_ROW);
  await expect(logList(page).locator("[data-seq='2601']")).toBeAttached();
  expect(await logList(page).locator(".log-row").count()).toBeLessThan(80);
  expect(csp).toEqual([]);
});

test("in the focused log, j k, arrows, Home and End move the selected row; enter opens the event", async ({ page, mock, csp, openPage }) => {
  await openPage();
  await page.keyboard.press("l");
  const list = logList(page);
  await expect(list.locator("[data-seq='6']")).toBeVisible();
  const selected = () => list.locator(".vrow.selected [data-seq]").getAttribute("data-seq");

  await list.focus();
  // Nothing selected: the first k takes the bottom row.
  await page.keyboard.press("k");
  await expect.poll(selected).toBe("6");
  await page.keyboard.press("k");
  await expect.poll(selected).toBe("5");
  await page.keyboard.press("ArrowUp");
  await expect.poll(selected).toBe("4");
  await page.keyboard.press("j");
  await expect.poll(selected).toBe("5");
  await page.keyboard.press("Home");
  await expect.poll(selected).toBe("1");
  await page.keyboard.press("End");
  await expect.poll(selected).toBe("6");
  await page.keyboard.press("k");
  await page.keyboard.press("Enter");
  await expect(page.locator("#side-panel .event-panel .panel-title")).toHaveText("event 5");
  await expect(page.locator("#side-panel .event-panel")).toContainText("agent.crashed");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(mock.calls.filter((c) => c.method === "agent_kill")).toEqual([]);
  expect(csp).toEqual([]);
});

test("in the log view, k with the focus outside the list moves the selection one row", async ({ page, mock, csp, openPage }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await openPage();
  await page.keyboard.press("l");
  const list = logList(page);
  await expect(list.locator("[data-seq='6']")).toBeVisible();
  const selected = () => list.locator(".vrow.selected [data-seq]").getAttribute("data-seq");
  await page.keyboard.press("k");
  await expect(list).toBeFocused();
  await expect.poll(selected, { timeout: 2000 }).toBe("6");
  await page.keyboard.press("k");
  await expect.poll(selected, { timeout: 2000 }).toBe("5");
  expect(errors).toEqual([]);
  expect(mock.calls.filter((c) => c.method === "agent_kill")).toEqual([]);
  expect(csp).toEqual([]);
});

test("event rows change from 36 to 64px without moving the keyed fractional scroll anchor", async ({ page, mock, openPage, csp }) => {
  mock.state.events = Array.from({ length: 1000 }, (_, i) => event(i + 1, mock.now() - 1000 + i));
  await openPage();
  await page.keyboard.press("l");
  const list = logList(page);
  await scrollTo(list, 400 * LOG_ROW + LOG_ROW / 4);
  const anchor = await firstVisible(list, "data-seq");
  expect(anchor).toBe("401");
  await expect.poll(() => list.locator(".vrow").first().evaluate((el) => el.getBoundingClientRect().height)).toBe(LOG_ROW);
  await expect.poll(() => list.locator(".log-row").first().evaluate((el) => el.getBoundingClientRect().height)).toBe(LOG_ROW);
  await page.setViewportSize({ width: 700, height: 900 });
  await expect.poll(() => list.locator(".vrow").first().evaluate((el) => el.getBoundingClientRect().height)).toBe(LOG_NARROW_ROW);
  expect(await firstVisible(list, "data-seq")).toBe(anchor);
  expect(await list.evaluate((el) => el.scrollTop)).toBe(400 * LOG_NARROW_ROW + LOG_NARROW_ROW / 4);
  expect(await rows(list, LOG_NARROW_ROW)).toBe(1000);
  await expect.poll(() => list.locator(".log-row").first().evaluate((el) => el.getBoundingClientRect().height)).toBe(LOG_NARROW_ROW);
  const visible = await list.evaluate((el) => {
    const box = el.getBoundingClientRect();
    return [...el.querySelectorAll(".log-row")].filter((row) => {
      const r = row.getBoundingClientRect();
      return r.bottom > box.top && r.top < box.bottom;
    }).length;
  });
  expect(await list.locator(".log-row").count()).toBeLessThanOrEqual(visible + 20);
  await page.setViewportSize({ width: 1400, height: 900 });
  await expect.poll(() => list.locator(".vrow").first().evaluate((el) => el.getBoundingClientRect().height)).toBe(LOG_ROW);
  expect(await firstVisible(list, "data-seq")).toBe(anchor);
  expect(await list.evaluate((el) => el.scrollTop)).toBe(400 * LOG_ROW + LOG_ROW / 4);
  expect(csp).toEqual([]);
});
