// The operator's actions (notes/web-design.md §ui "Operator actions"): each
// row of the table sends its call with its parameters, from the buttons
// (inspector toolbar, status bar, usage strip, a node's context menu) and the
// keys; the four confirmations ask first, and `esc` or "cancel" sends
// nothing; no optimistic state; an error answer is shown as text.

import type { Locator, Page } from "@playwright/test";
import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
import type { Mock } from "./fixtures.ts";

const node = (page: Page, id: string) => page.locator(`[data-agent="${id}"]`);
const toolbar = (page: Page, id: string) => page.getByRole("toolbar", { name: `actions for ${id}` });
const status = (page: Page) => page.locator("footer .status-line");
const button = (scope: Page | Locator, name: string) => scope.getByRole("button", { name, exact: true });
const calls = (mock: Mock, method: string) => mock.calls.filter((c) => c.method === method);

async function moreAction(page: Page, id: string, name: string): Promise<Locator> {
  await button(toolbar(page, id), "More").click();
  return page.getByRole("menu", { name: `actions for ${id}`, exact: true }).getByRole("menuitem", { name, exact: true });
}

/** Select an agent by its node's header (the centre of a node is its task's
 * title, a link of its own) and wait for its inspector. */
async function inspect(page: Page, id: string): Promise<void> {
  await node(page, id).locator(".node-head").click();
  await expect(page.locator(`#side-panel [data-inspector="${id}"]`)).toBeVisible();
}

/** Every call the page made so far has reached the mock: the inspector's `r`
 * reads the inbox again, and a socket keeps its order. Needs an inspector on
 * its overview and the keyboard free. */
async function settle(page: Page, mock: Mock): Promise<void> {
  const from = mock.calls.length;
  await page.keyboard.press("r");
  await mock.waitForCall("inbox", (c) => c.seq >= from);
}

/** An agent at a usage limit, as the daemon pushes it. */
function limited(mock: Mock, id: string) {
  const agent = structuredClone(mock.state.agents.find((a) => a.id === id)!);
  const now = mock.now();
  agent.limit = { kind: "usage", message: "5-hour limit reached", since: now - 60_000, resets_at: now + 2 * 3_600_000, retry_at: null, attempts: 0 };
  return { kind: "agent", data: agent };
}

test.describe("operator actions", () => {
  test.use({ viewport: { width: 1400, height: 900 } });

  test("message: m and the toolbar button send to the agent with a client id; the push shows it in the bus", async ({ page, mock, csp, openPage }) => {
    const guard = noRequestsAfterStart(page, mock);
    await openPage();
    await inspect(page, "claude-2");
    const box = page.locator("#compose-text");
    const recipient = page.getByRole("combobox", { name: "recipient" });

    await button(toolbar(page, "claude-2"), "message").click();
    await expect(box).toBeFocused();
    await expect(recipient).toHaveValue("claude-2");
    await box.fill("take the totals from 3f4e5d6c");
    await box.press("Enter");
    const first = await mock.waitForCall("send");
    expect(first.params).toEqual({ to: "claude-2", type: "message", body: "take the totals from 3f4e5d6c", id: expect.stringMatching(/^w-[0-9a-f]{16}$/) });
    await expect(status(page)).toHaveText("✓ message sent to claude-2");
    await expect(box).toHaveValue("");

    // `esc` leaves the box, `m` opens it again for the selected agent.
    await box.press("Escape");
    await expect(box).not.toBeFocused();
    await page.keyboard.press("m");
    await expect(box).toBeFocused();
    await expect(recipient).toHaveValue("claude-2");
    await box.fill("and rebase");
    await box.press("Enter");
    const second = await mock.waitForCall("send", (c) => c.params.body === "and rebase");
    expect(second.params).toMatchObject({ to: "claude-2", type: "message" });
    expect(second.params.id).toMatch(/^w-/);
    expect(second.params.id).not.toBe(first.params.id);

    // The message appears in the bus through its push.
    await box.press("Escape");
    await page.keyboard.press("Escape");
    await expect(page.locator("#side-panel [data-inspector]")).toHaveCount(0);
    await expect(page.locator(`[data-message="${first.params.id}"]`)).toContainText("take the totals from 3f4e5d6c");
    await expect(page.locator(`[data-message="${second.params.id}"]`)).toContainText("operator → claude-2");

    expect(guard.requests).toEqual([]);
    expect(csp).toEqual([]);
  });

  test("broadcast: M sends to everyone", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await page.keyboard.press("M");
    const box = page.locator("#compose-text");
    await expect(box).toBeFocused();
    await expect(page.getByRole("combobox", { name: "recipient" })).toHaveValue("*");
    await box.fill("everyone: rebase on agents/integration");
    await box.press("Enter");
    const call = await mock.waitForCall("send");
    expect(call.params).toEqual({ to: "*", type: "message", body: "everyone: rebase on agents/integration", id: expect.stringMatching(/^w-/) });
    await expect(status(page)).toHaveText("✓ broadcast sent");
    await expect(page.locator(`[data-message="${call.params.id}"]`)).toContainText("operator → all");
    expect(csp).toEqual([]);
  });

  test("a message that fails stays in the box with its text", async ({ page, mock, csp, openPage }) => {
    mock.on("send", () => {
      throw { code: "too_large", message: "message body exceeds 65536 bytes" };
    });
    await openPage();
    await inspect(page, "claude-2");
    await page.keyboard.press("m");
    const box = page.locator("#compose-text");
    await box.fill("a long story");
    await box.press("Enter");
    await mock.waitForCall("send");
    await expect(status(page)).toHaveText("message sent to claude-2: message body exceeds 65536 bytes");
    await expect(status(page)).toHaveClass(/\berr\b/);
    await expect(box).toHaveValue("a long story");
    expect(csp).toEqual([]);
  });

  test("pause and resume: p, then the toolbar's resume; the page changes on the daemon's push", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await inspect(page, "claude-2");
    const tb = toolbar(page, "claude-2");
    await expect(button(tb, "pause")).toBeVisible();

    await page.keyboard.press("p");
    const pause = await mock.waitForCall("agent_pause");
    expect(pause.params).toEqual({ id: "claude-2" });
    await expect(button(tb, "resume")).toBeVisible();
    await expect(node(page, "claude-2").locator(".badge.paused")).toBeVisible();
    await expect(status(page)).toHaveText("✓ pause claude-2");

    await button(tb, "resume").click();
    const resume = await mock.waitForCall("agent_resume");
    expect(resume.params).toEqual({ id: "claude-2" });
    await expect(button(tb, "pause")).toBeVisible();
    await expect(node(page, "claude-2").locator(".badge.paused")).toHaveCount(0);

    // The context menu's entry follows the agent's state.
    await node(page, "claude-3").locator(".node-head").click({ button: "right" });
    const menu = page.getByRole("menu", { name: "actions for claude-3" });
    await menu.getByRole("menuitem", { name: "pause" }).click();
    expect((await mock.waitForCall("agent_pause", (c) => c.params.id === "claude-3")).params).toEqual({ id: "claude-3" });
    await expect(node(page, "claude-3").locator(".badge.paused")).toBeVisible();
    expect(csp).toEqual([]);
  });

  test("restart: More and R; the node shows a pending mark until the daemon answers", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await inspect(page, "claude-2");
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    mock.on("agent_restart", async (p: { id: string }) => {
      await gate;
      return structuredClone(mock.state.agents.find((a) => a.id === p.id));
    });

    await (await moreAction(page, "claude-2", "restart")).click();
    expect((await mock.waitForCall("agent_restart")).params).toEqual({ id: "claude-2" });
    await expect(node(page, "claude-2").locator(".badge.pending")).toBeVisible();
    const restart = await moreAction(page, "claude-2", "restart");
    await expect(restart).toBeDisabled();
    release();
    await expect(node(page, "claude-2").locator(".badge.pending")).toHaveCount(0);
    await expect(restart).toBeEnabled();
    await expect(status(page)).toHaveText("✓ restart claude-2");
    await page.keyboard.press("Escape");

    mock.on("agent_restart", null);
    const before = calls(mock, "agent_restart").length;
    await page.keyboard.press("R");
    await expect.poll(() => calls(mock, "agent_restart").length).toBe(before + 1);
    expect(calls(mock, "agent_restart").at(-1)!.params).toEqual({ id: "claude-2" });
    expect(csp).toEqual([]);
  });

  test("restart fresh asks first; esc and cancel send nothing", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await inspect(page, "claude-2");
    const more = button(toolbar(page, "claude-2"), "More");
    const dialog = page.getByRole("dialog", { name: "restart claude-2 fresh · no resume" });

    await (await moreAction(page, "claude-2", "restart fresh…")).click();
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText("The old conversation is not resumed.");
    await expect(button(dialog, "cancel")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(more).toBeFocused();
    await expect(page.locator("#side-panel [data-inspector='claude-2']")).toBeVisible();

    await (await moreAction(page, "claude-2", "restart fresh…")).click();
    await button(dialog, "cancel").click();
    await expect(dialog).toHaveCount(0);
    await settle(page, mock);
    expect(calls(mock, "agent_restart_fresh")).toEqual([]);

    await (await moreAction(page, "claude-2", "restart fresh…")).click();
    await button(dialog, "restart fresh").click();
    await expect(dialog).toHaveCount(0);
    expect((await mock.waitForCall("agent_restart_fresh")).params).toEqual({ id: "claude-2" });
    await expect(status(page)).toHaveText("✓ restart claude-2 fresh (no resume)");
    expect(csp).toEqual([]);
  });

  test("kill asks first (k, the context menu, the toolbar); esc and cancel send nothing", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await inspect(page, "claude-4");
    const dialog = page.getByRole("dialog", { name: "kill agent" });

    await page.keyboard.press("k");
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText("Stop claude-4's process? Its task stays open.");
    await expect(button(dialog, "cancel")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);

    await node(page, "claude-4").locator(".node-head").click({ button: "right" });
    await page.getByRole("menu", { name: "actions for claude-4" }).getByRole("menuitem", { name: "kill…" }).click();
    await expect(dialog).toBeVisible();
    await button(dialog, "cancel").click();
    await expect(dialog).toHaveCount(0);
    await settle(page, mock);
    expect(calls(mock, "agent_kill")).toEqual([]);

    await (await moreAction(page, "claude-4", "kill…")).click();
    await button(dialog, "kill claude-4").click();
    expect((await mock.waitForCall("agent_kill")).params).toEqual({ id: "claude-4" });
    await expect(status(page)).toHaveText("✓ kill claude-4");
    expect(csp).toEqual([]);
  });

  test("compact from the context menu and the toolbar", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await node(page, "claude-3").locator(".node-head").click({ button: "right" });
    const menu = page.getByRole("menu", { name: "actions for claude-3" });
    await expect(menu.getByRole("menuitem").first()).toBeFocused();
    await menu.getByRole("menuitem", { name: "compact" }).click();
    await expect(menu).toHaveCount(0);
    expect((await mock.waitForCall("agent_compact")).params).toEqual({ id: "claude-3" });
    await expect(status(page)).toHaveText("✓ compaction of claude-3's context queued");

    await inspect(page, "claude-5");
    await (await moreAction(page, "claude-5", "compact")).click();
    expect((await mock.waitForCall("agent_compact", (c) => c.params.id === "claude-5")).params).toEqual({ id: "claude-5" });
    await expect(status(page)).toHaveText("✓ compaction of claude-5's context queued");
    expect(csp).toEqual([]);
  });

  test("model and CLI: the dialog lists models_list and sets {id, model, runtime}", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await inspect(page, "claude-2");
    await (await moreAction(page, "claude-2", "model and CLI")).click();
    const dialog = page.getByRole("dialog", { name: "model and CLI" });
    await expect(dialog).toBeVisible();
    expect((await mock.waitForCall("models_list")).params ?? null).toBeNull();
    // claude-2 runs on Codex: that tab is open, and moving to Claude Code says so.
    await expect(dialog.getByRole("tab", { selected: true })).toContainText("Codex");
    await expect(dialog.getByRole("option", { name: /GPT 6 Astra/ })).toBeVisible();
    await dialog.getByRole("tab", { name: /Claude Code/ }).click();
    await expect(dialog).toContainText("another CLI: a fresh Claude Code session there");
    await dialog.getByRole("option", { name: /Opus 5\.5/ }).click();
    await expect(dialog).toHaveCount(0);
    expect((await mock.waitForCall("agent_set_model")).params).toEqual({ id: "claude-2", model: "opus", runtime: "claude" });
    await expect(status(page)).toHaveText(/moving claude-2 to Claude Code on Opus 5\.5/);
    // The daemon's push moves the agent; the page follows it.
    await expect(page.locator("[data-inspector='claude-2'] .insp-head .glyph")).toHaveClass(/rt-claude/);

    // `s` with the selection, the number keys switch tabs; `O` is the orchestrator's.
    await page.keyboard.press("s");
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole("tab", { selected: true })).toContainText("Claude Code");
    await expect(dialog.getByRole("tab", { selected: true })).toBeFocused();
    await page.keyboard.press("2");
    await expect(dialog.getByRole("tab", { selected: true })).toContainText("Codex");
    await expect(dialog.getByRole("option", { name: /GPT 6 Luna/ })).toContainText("not here");
    await page.keyboard.press("ArrowLeft");
    await expect(dialog.getByRole("tab", { selected: true })).toContainText("Claude Code");
    await expect(dialog.getByRole("option", { selected: true })).toContainText("Opus 5.5");
    await dialog.getByRole("option", { name: /Fable 5\.1/ }).click();
    expect((await mock.waitForCall("agent_set_model", (c) => c.params.model === "fable")).params).toEqual({ id: "claude-2", model: "fable", runtime: "claude" });

    await page.keyboard.press("O");
    await expect(page.getByRole("dialog", { name: "model and CLI" })).toContainText("orchestrator · model and CLI");
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    expect(calls(mock, "agent_set_model").map((c) => c.params.id)).toEqual(["claude-2", "claude-2"]);
    expect(csp).toEqual([]);
  });

  test("continue paused agents: resume {agents: [], restart: false} from the usage strip and u", async ({ page, mock, csp, openPage }) => {
    await openPage();
    // Nothing is paused at a limit: u says so and sends nothing.
    await inspect(page, "claude-2");
    await page.keyboard.press("u");
    await expect(status(page)).toHaveText("no agent is paused at a usage limit");
    await expect(page.locator(".usage-strip")).toHaveCount(0);

    mock.push(limited(mock, "claude-1"));
    const strip = page.locator(".usage-strip");
    await expect(strip).toContainText("usage limit reached · 1 paused (claude-1)");
    await expect(node(page, "claude-1").locator(".badge.limit")).toBeVisible();
    await settle(page, mock);
    expect(calls(mock, "resume")).toEqual([]);

    await button(strip, "continue now").click();
    expect((await mock.waitForCall("resume")).params).toEqual({ agents: [], restart: false });
    await expect(status(page)).toHaveText("✓ told the paused agents to continue");

    await page.keyboard.press("u");
    await expect.poll(() => calls(mock, "resume").length).toBe(2);
    expect(calls(mock, "resume")[1].params).toEqual({ agents: [], restart: false });
    expect(csp).toEqual([]);
  });

  test("restart paused agents asks first (U, the usage strip); esc and cancel send nothing", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await inspect(page, "claude-2");
    mock.push(limited(mock, "claude-1"));
    const strip = page.locator(".usage-strip");
    await expect(strip).toBeVisible();
    const dialog = page.getByRole("dialog", { name: "restart paused agents" });

    await page.keyboard.press("U");
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText("Restart 1 agent(s) paused at a usage limit?");
    await expect(button(dialog, "cancel")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);

    await button(strip, "restart on the current login").click();
    await expect(dialog).toBeVisible();
    await button(dialog, "cancel").click();
    await expect(dialog).toHaveCount(0);
    await settle(page, mock);
    expect(calls(mock, "resume")).toEqual([]);

    await page.keyboard.press("U");
    await button(dialog, "restart").click();
    expect((await mock.waitForCall("resume")).params).toEqual({ agents: [], restart: true });
    await expect(status(page)).toHaveText("✓ restarted the paused agents on the current login");
    expect(csp).toEqual([]);
  });

  test("stop the team asks first (the status bar, S); esc and cancel send nothing; the team's stop leaves the page live and the workspace not running", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await inspect(page, "claude-2");
    const stop = page.locator(".status-bar").getByRole("button", { name: "stop team" });
    const dialog = page.getByRole("dialog", { name: "stop the team" });

    await stop.click();
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText("Stop every agent and the crew daemon?");
    await expect(button(dialog, "cancel")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(stop).toBeFocused();

    await page.keyboard.press("S");
    await expect(dialog).toBeVisible();
    await button(dialog, "cancel").click();
    await expect(dialog).toHaveCount(0);
    await settle(page, mock);
    expect(calls(mock, "shutdown")).toEqual([]);

    await page.keyboard.press("S");
    await button(dialog, "stop the team").click();
    expect((await mock.waitForCall("shutdown")).params).toEqual({ stop_agents: true });

    mock.stopDaemon();
    await expect(page.locator('[data-workspace-state="not-running"]')).toBeAttached();
    await expect(page.locator('[data-conn="live"]').first()).toBeAttached();
    await expect(page.locator("[data-workspace-bar]")).toContainText("checkout-rewrite is not running");
    await expect(stop).toBeDisabled();
    // What it showed stays, dimmed.
    await expect(page.locator(".node.member").first()).toBeVisible();
    expect(csp).toEqual([]);
  });

  test("prompt: the inspector's prompt button and i submit {id, text, submit: true}", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await inspect(page, "claude-2");
    await (await moreAction(page, "claude-2", "prompt")).click();
    const box = page.getByRole("textbox", { name: "prompt for claude-2" });
    await expect(box).toBeFocused();
    await box.fill("run the tests and report");
    await box.press("Enter");
    expect((await mock.waitForCall("agent_input")).params).toEqual({ id: "claude-2", text: "run the tests and report", submit: true });
    await expect(status(page)).toHaveText("✓ typed into claude-2");
    // Sent: the box is the message box again.
    await expect(page.locator("#compose-text")).toHaveAttribute("aria-label", "message, or a / command");

    await page.locator("#compose-text").press("Escape");
    await page.keyboard.press("i");
    await expect(box).toBeFocused();
    await box.fill("and commit");
    await box.press("Enter");
    expect((await mock.waitForCall("agent_input", (c) => c.params.text === "and commit")).params).toEqual({ id: "claude-2", text: "and commit", submit: true });
    expect(csp).toEqual([]);
  });

  test("jump to pane: agent_focus on herdr, from the toolbar and g", async ({ page, mock, csp, openPage }) => {
    await openPage();
    await inspect(page, "claude-2");
    await (await moreAction(page, "claude-2", "jump to pane")).click();
    expect((await mock.waitForCall("agent_focus")).params).toEqual({ id: "claude-2" });
    await expect(status(page)).toHaveText("✓ focus claude-2");

    await inspect(page, "claude-5");
    await page.keyboard.press("g");
    expect((await mock.waitForCall("agent_focus", (c) => c.params.id === "claude-5")).params).toEqual({ id: "claude-5" });
    expect(csp).toEqual([]);
  });

  test("an error answer is shown as text, and the page does not pretend it happened", async ({ page, mock, csp, openPage }) => {
    mock.on("agent_pause", () => {
      throw { code: "not_found", message: "no agent 'x'" };
    });
    await openPage();
    await inspect(page, "claude-2");
    await page.keyboard.press("p");
    await mock.waitForCall("agent_pause");
    await expect(status(page)).toHaveText("pause claude-2: no agent 'x'");
    await expect(status(page)).toHaveClass(/\berr\b/);
    await expect(button(toolbar(page, "claude-2"), "pause")).toBeEnabled();
    await expect(node(page, "claude-2").locator(".badge.paused")).toHaveCount(0);
    await expect(node(page, "claude-2").locator(".badge.pending")).toHaveCount(0);
    expect(csp).toEqual([]);
  });
});

test("the model dialog shows why the models could not be listed, and the page goes on", async ({ page, mock, csp, openPage }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  mock.on("models_list", () => { throw { code: "unavailable", message: "the model catalog is unavailable" }; });
  await openPage();
  await page.locator('[data-agent="claude-2"] .node-head').click();
  await page.keyboard.press("s");
  const d = page.getByRole("dialog");
  await expect(d).toContainText("the models could not be listed: the model catalog is unavailable");
  await page.keyboard.press("Escape");
  await expect(d).toHaveCount(0);
  await page.keyboard.press("t");
  await expect(page.locator(".task-dag")).toBeVisible();
  expect(errors).toEqual([]);
  expect(csp).toEqual([]);
});
