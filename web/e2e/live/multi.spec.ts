// One dashboard server, several real daemons (notes/web-multi.md §multi-tests "Live"): joining,
// the switcher, live state per workspace, an operator action landing in its own workspace,
// daemons leaving and coming back, `--remove`, `--restart`, `--stop`, and a session that was never joined.
import type { Locator, Page } from "@playwright/test";
import { expect, test, teamYaml } from "./harness.ts";
import type { Daemon, WebInfo } from "./harness.ts";
import { rawWs } from "./rawws.mjs";

test.use({ viewport: { width: 1400, height: 900 } });

const button = (page: Page) => page.locator("[data-switcher-button]");
const list = (page: Page) => page.getByRole("listbox", { name: "workspaces" });
const live = (page: Page) => expect(page.locator('[data-workspace-state="live"]')).toBeAttached({ timeout: 20_000 });
const members = (page: Page) => page.locator(".node.member");
const node = (page: Page, id: string) => page.locator(`[data-agent="${id}"]`);

/** B's team: two more agents than A's, and one that asks the operator a question when it is messaged. */
const betaTeam = (b: Daemon) => teamYaml(b.ws, b.vcs, 2, { project: "beta", asker: true });

async function switchTo(page: Page, project: string, wait = true) {
  await button(page).click();
  await expect(list(page)).toBeVisible();
  await list(page).getByRole("option", { name: new RegExp(project) }).click();
  await expect(button(page)).toHaveText(`${project} ▾`);
  if (wait) await live(page);
}

async function compose(page: Page, to: string, text: string) {
  await node(page, to).locator(".node-head").click();
  await expect(page.locator(`#side-panel [data-inspector="${to}"]`)).toBeVisible();
  await page.getByRole("toolbar", { name: `actions for ${to}` }).getByRole("button", { name: "message", exact: true }).click();
  const box = page.locator("#compose-text");
  await box.fill(text);
  await box.press("Enter");
  await expect(page.locator("footer .status-line")).toHaveText(`✓ message sent to ${to}`);
  await box.press("Escape");
  await page.keyboard.press("Escape");
}

test("crew web in A, then in B: the same origin; the open tab shows B without a reload; B's agents; an action in B lands in B only", async ({ page, daemon: A, sibling, openLive }) => {
  const B = sibling("beta");
  A.start();
  B.start(betaTeam(B));
  const infoA = A.web();
  await openLive(infoA);
  await expect(button(page)).toHaveText("web-live ▾");
  await expect(members(page)).toHaveCount(6);
  await page.evaluate(() => ((window as unknown as { __tab: number }).__tab = 1));
  const sessionBefore = await page.evaluate(() => localStorage.getItem("crew.web.session"));

  const infoB = B.web();
  expect(infoB.url).toBe(infoA.url);
  expect(infoB.port).toBe(infoA.port);
  expect(infoB.started).toBe(false);
  expect(infoB.joined).toEqual([infoB.workspace]);
  expect(infoB.workspace).not.toBe(infoA.workspace);
  // The tab shows B in its switcher at once: no reload, no new pairing.
  await button(page).click();
  await expect(list(page).getByRole("option")).toHaveCount(2, { timeout: 15_000 });
  await page.keyboard.press("Escape");
  await switchTo(page, "beta");
  await expect(members(page)).toHaveCount(9);
  await expect(node(page, "asker")).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { __tab?: number }).__tab)).toBe(1);
  expect(await page.evaluate(() => localStorage.getItem("crew.web.session"))).toBe(sessionBefore);

  // An operator action in B: B's events confirm it and A's log never sees it.
  await compose(page, "echo", "hello beta only");
  B.until("B's bus event", () => B.events().some((e) => e.event_type === "message.sent" && e.message.includes("hello beta only")));
  await expect(page.locator("[data-message]", { hasText: "operator → echo" })).toContainText("hello beta only");
  expect(A.events().some((e) => e.message.includes("hello beta only"))).toBe(false);
  expect(A.c(["messages", "--json", "-n", "200"])).not.toContain("hello beta only");
  // Back in A: its bus does not show it either.
  await switchTo(page, "web-live");
  await expect(members(page)).toHaveCount(6);
  await expect(page.getByText("hello beta only")).toHaveCount(0);
});

test("a question asked in B while A is on screen: the badge and the notice, then the banner after the switch", async ({ page, daemon: A, sibling, openLive }) => {
  const B = sibling("beta");
  A.start();
  B.start(betaTeam(B));
  await openLive(A.web());
  B.web();
  await button(page).click();
  await expect(list(page).getByRole("option")).toHaveCount(2, { timeout: 15_000 });
  await page.keyboard.press("Escape");
  B.send("asker", "ask away");
  await expect(page.locator(".switcher > .badge.q")).toHaveText("? 1", { timeout: 15_000 });
  await expect(page).toHaveTitle("(1) web-live · crew");
  const toast = page.locator(".toast", { hasText: "beta: 1 question is waiting" });
  await expect(toast).toBeVisible();
  await expect(page.getByText("Which way?")).toHaveCount(0);
  await toast.getByRole("button", { name: "switch" }).click();
  await live(page);
  await expect(button(page)).toHaveText("beta ▾");
  await expect(page.locator(".question-banner")).toContainText("Which way?");
  const dialog = page.getByRole("dialog", { name: "question" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("textbox", { name: "your own answer" }).fill("left");
  await dialog.getByRole("textbox", { name: "your own answer" }).press("Enter");
  await expect(dialog).toHaveCount(0);
  B.until("question.answered in B", () => B.events().some((e) => e.event_type === "question.answered"));
  expect(A.events().some((e) => e.event_type === "question.answered")).toBe(false);
  await expect(page.locator(".switcher > .badge.q")).toHaveCount(0);
});

for (const how of ["crew stop", "SIGKILL"] as const) {
  test(`${how} for A: the tab stays live, A is not running; crew run again: A is live again in the same tab with the same session`, async ({ page, daemon: A, sibling, openLive }) => {
    const B = sibling("beta");
    A.start();
    B.start(betaTeam(B));
    await openLive(A.web());
    B.web();
    const session = await page.evaluate(() => localStorage.getItem("crew.web.session"));
    if (how === "crew stop") A.stop();
    else A.kill();
    await expect(page.locator('[data-workspace-state="not-running"]')).toBeAttached({ timeout: 20_000 });
    await expect(page.locator('.status-bar [data-conn="live"]')).toBeAttached();
    await expect(page.locator("[data-workspace-bar]")).toContainText("web-live is not running");
    // What it showed stays, read-only; the other workspace is one switch away and live.
    await expect(members(page)).toHaveCount(6);
    await expect(page.locator("#compose-text")).toBeDisabled();
    await switchTo(page, "beta");
    await expect(members(page)).toHaveCount(9);
    await switchTo(page, "web-live", false);
    await expect(page.locator('[data-workspace-state="not-running"]')).toBeAttached();
    A.start();
    await live(page);
    await expect(members(page)).toHaveCount(6);
    await expect(page.locator("#compose-text")).toBeEnabled();
    expect(await page.evaluate(() => localStorage.getItem("crew.web.session"))).toBe(session);
  });
}

test("crew web --remove for B while on screen, with a message sent every 50 ms through the page: nothing sent after the command returned reaches B; B comes back in the same tab", async ({ page, daemon: A, sibling, openLive }) => {
  test.setTimeout(120_000);
  const B = sibling("beta");
  A.start();
  B.start(betaTeam(B));
  await openLive(A.web());
  B.web();
  await switchTo(page, "beta");
  await expect(members(page)).toHaveCount(9);
  await node(page, "echo").locator(".node-head").click();
  await page.getByRole("toolbar", { name: "actions for echo" }).getByRole("button", { name: "message", exact: true }).click();
  const box = page.locator("#compose-text");
  const pressed: { i: number; at: number }[] = [];
  let running = true;
  const sender = (async () => {
    for (let i = 0; running; i++) {
      await box.fill(`rm-${i}`, { timeout: 500 }).catch(() => {});
      pressed.push({ i, at: Date.now() });
      await box.press("Enter", { timeout: 500 }).catch(() => {});
      await page.waitForTimeout(50);
    }
  })();
  await expect.poll(() => pressed.length).toBeGreaterThan(10);
  const removed = await B.crewAsync(["web", "--remove"]);
  expect(removed.ok, removed.stderr).toBe(true);
  await expect(page.locator('[data-workspace-state="removed"]')).toBeAttached({ timeout: 15_000 });
  await expect(page.locator("[data-workspace-bar]")).toContainText("beta was taken out of this dashboard");
  await page.waitForTimeout(500);
  running = false;
  await sender;
  const inB = (JSON.parse(B.c(["messages", "--json", "-n", "1000"])) as { body: string }[]).map((m) => /^rm-(\d+)$/.exec(m.body)?.[1]).filter((x) => x !== undefined).map(Number);
  expect(inB.length, "messages sent while it was joined").toBeGreaterThan(5);
  const late = pressed.filter((p) => p.at > removed.ended).map((p) => p.i);
  expect(late.length, "messages the page tried after the command returned").toBeGreaterThan(0);
  expect(inB.filter((i) => late.includes(i)), "messages sent after the command returned that are in B").toEqual([]);
  // `crew web` in B again: B is back in the same tab.
  const again = B.web();
  expect(again.joined).toEqual([again.workspace]);
  await live(page);
  await expect(button(page)).toHaveText("beta ▾");
  await expect(members(page)).toHaveCount(9);
});

test("crew web --restart with A running and B stopped: a new address that shows both, B not running; the daemons' pids did not change", async ({ page, browser, daemon: A, sibling, openLive }) => {
  const B = sibling("beta");
  A.start();
  B.start(betaTeam(B));
  const infoA = A.web();
  B.web();
  await openLive(infoA);
  B.stop();
  await expect(page.locator('.status-bar [data-conn="live"]')).toBeAttached();
  const pidA = A.pid();
  const restarted = await A.crewAsync(["web", "--restart", "--json"]);
  expect(restarted.ok, restarted.stderr).toBe(true);
  const next = JSON.parse(restarted.stdout) as WebInfo;
  expect(new URL(next.url).origin).not.toBe(new URL(infoA.url).origin);
  expect(next.port).toBe(infoA.port);
  expect(A.pid()).toBe(pidA);
  // The tab of the old address stops with its server.
  await expect(page.locator('[data-conn="stopped"]')).toBeAttached({ timeout: 20_000 });
  const fresh = await (await browser.newContext({ viewport: { width: 1400, height: 900 } })).newPage();
  await openLive(next, fresh);
  await expect(members(fresh)).toHaveCount(6);
  await button(fresh).click();
  await expect(list(fresh).getByRole("option")).toHaveCount(2);
  await expect(list(fresh).getByRole("option", { name: /beta/ })).toHaveAttribute("data-state", "stopped");
  await expect(list(fresh).getByRole("option", { name: /web-live/ })).not.toHaveAttribute("data-state", "stopped");
  await fresh.context().close();
});

test("crew web --stop: every tab stops; no daemon does", async ({ page, daemon: A, sibling, openLive }) => {
  const B = sibling("beta");
  A.start();
  B.start(betaTeam(B));
  await openLive(A.web());
  B.web();
  const pidA = A.pid(), pidB = B.pid();
  const out = A.c(["web", "--stop"]);
  expect(out).toMatch(/stopped/i);
  await expect(page.locator('[data-conn="stopped"]')).toBeAttached({ timeout: 20_000 });
  await expect(page.locator(".conn-bar")).toContainText("The dashboard's server was stopped at");
  expect(A.pid()).toBe(pidA);
  expect(B.pid()).toBe(pidB);
});

test("a running session that was never joined is named in the switcher, and a hello with its id is not_found", async ({ page, daemon: A, sibling, openLive }) => {
  const C = sibling("gamma");
  A.start();
  C.start(teamYaml(C.ws, C.vcs, 0, { project: "gamma" }));
  await openLive(A.web());
  const info = A.web();
  await button(page).click();
  const other = page.locator("[data-other]");
  await expect(other).toContainText("gamma", { timeout: 15_000 });
  await expect(other).toContainText("crew web -C");
  const id = (await other.getAttribute("data-other"))!;
  expect(id).toMatch(/^[0-9a-f]{16}$/);
  await page.keyboard.press("Escape");
  // On the wire: sign in, then ask for it by its id.
  const label = new URL(info.url).host;
  const index = await rawWs(info.port, label, `http://${label}`);
  const code = new URLSearchParams(new URL(info.pair_url).hash.slice(1)).get("pair")!;
  const token = ((await index.call("web_pair", { code })) as { result: { token: string } }).result.token;
  const ws = await rawWs(info.port, label, `http://${label}`);
  const reply = (await ws.call("hello", { role: "operator", token, client: "web", protocol: 6, workspace: id })) as { error?: { code?: string } };
  expect(reply.error?.code).toBe("not_found");
  index.close();
  ws.close();
});
