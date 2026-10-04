// The page's Content-Security-Policy and agent text (notes/web-design.md
// §security "Agent text in the page", §tests `policy.spec.ts`): a session that
// opens every view, every inspector tab (the xterm.js terminal included) and
// every dialog logs no violation and asks its origin for nothing; markup in
// an agent's name, a message, a question or an event is shown as text.

import type { Page } from "@playwright/test";
import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
import type { Mock } from "./fixtures.ts";

const node = (page: Page, id: string) => page.locator(`[data-agent="${id}"]`);
const tab = (page: Page, name: string) => page.locator("#side-panel").getByRole("tab", { name, exact: true });

async function inspect(page: Page, id: string): Promise<void> {
  await node(page, id).locator(".node-head").click();
  await expect(page.locator(`#side-panel [data-inspector="${id}"]`)).toBeVisible();
}

function agentPush(mock: Mock, base: string, patch: Record<string, unknown>) {
  return { kind: "agent", data: { ...structuredClone(mock.state.agents.find((a) => a.id === base)!), ...patch } };
}

test.use({ viewport: { width: 1400, height: 900 } });

test("no policy violation and no request across every view, inspector tab and dialog", async ({ page, mock, csp, openPage }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();

  // The graph: the hover card and the context menu.
  await node(page, "claude-3").locator(".node-head").hover();
  await expect(page.getByRole("tooltip")).toContainText("claude-3");
  await node(page, "claude-3").locator(".node-head").click({ button: "right" });
  await expect(page.getByRole("menu", { name: "actions for claude-3" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);

  // Every dialog: help, question, model, confirm.
  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: "keys and commands" })).toBeVisible();
  await page.keyboard.press("Escape");
  mock.push(mock.pushes("m-9001")[0]);
  await expect(page.getByRole("dialog", { name: "question" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await inspect(page, "claude-2");
  await page.keyboard.press("s");
  await expect(page.getByRole("dialog", { name: "model and CLI" }).getByRole("option").first()).toBeVisible();
  await page.keyboard.press("Escape");
  await page.keyboard.press("k");
  await expect(page.getByRole("dialog", { name: "kill agent" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);

  // The inspector's four tabs on herdr: the last screen, the transcript, the diff.
  await tab(page, "terminal").click();
  await mock.waitForCall("agent_output", (c) => c.params.id === "claude-2");
  await expect(page.locator('[data-terminal="claude-2"] pre.screen')).toBeVisible();
  await tab(page, "transcript").click();
  await mock.waitForCall("agent_transcript", (c) => c.params.id === "claude-2");
  await tab(page, "diff").click();
  await mock.waitForCall("git_diff", (c) => c.params.agent === "claude-2");
  await expect(page.locator("#side-panel .diff")).toBeVisible();
  await tab(page, "overview").click();

  // A pty agent: xterm.js on a terminal socket of its own, typed keys echoed.
  mock.push(agentPush(mock, "claude-1", { backend: "pty", backend_ref: null }));
  await inspect(page, "claude-1");
  await tab(page, "terminal").click();
  const term = page.locator('[data-terminal="claude-1"]');
  await expect(term.locator(".xterm-rows")).toContainText("claude-1: mock terminal", { timeout: 10_000 });
  await term.locator(".xterm-host").click();
  await page.keyboard.type("ls");
  await mock.waitForCall("attach_input");
  await expect(term.locator(".xterm-rows")).toContainText("$ ls");
  await page.keyboard.press("Control+]");
  await page.keyboard.press("Escape");

  // The other views and the panels of a message, a task and an event.
  await page.locator("#side-panel [data-message='m-0006']").click();
  await expect(page.locator("#side-panel .message-panel")).toContainText("your macro shadows mine");
  await page.keyboard.press("Escape");
  await page.keyboard.press("t");
  await page.locator('[data-task="ledger"]').click();
  await expect(page.locator("#side-panel .task-panel")).toContainText("implement ledger");
  // (`l` on a focused task walks the DAG: the view's button instead.)
  await page.locator(".status-bar").getByRole("button", { name: "logs" }).click();
  await page.locator('#main [data-seq="4"]').dblclick();
  await expect(page.locator("#side-panel .event-panel")).toContainText("cherry_pick");
  await page.keyboard.press("v");
  await expect(page).toHaveURL(/#\/usage/);
  mock.push(agentPush(mock, "claude-4", { limit: { kind: "usage", message: "limit", since: mock.now(), resets_at: mock.now() + 3_600_000, retry_at: null, attempts: 0 } }));
  await expect(page.locator(".usage-strip")).toBeVisible();
  await page.keyboard.press("v");
  await expect(page.locator(".agent-graph")).toBeVisible();

  expect(guard.requests).toEqual([]);
  expect(csp).toEqual([]);
});

const MARKUP = "<img src=x onerror=window.__pwned=1>";

test("markup in an agent's name, a message, a question and an event is shown as text", async ({ page, mock, csp, openPage }) => {
  await openPage();
  expect(await page.locator("img").count()).toBe(0);

  mock.push(agentPush(mock, "claude-1", { id: MARKUP, name: MARKUP, role: `<b>role</b>${MARKUP}`, activity: `<script>window.__pwned=2</script>${MARKUP}`, task: null }));
  const weird = page.locator(`[data-agent="${MARKUP}"]`);
  await expect(weird).toBeVisible();
  await expect(weird.locator(".node-head .id")).toHaveText(MARKUP);
  await expect(weird).toHaveAccessibleName(`${MARKUP}, working`);

  const body = `<b>bold</b><script>window.__pwned=3</script>${MARKUP} <a href="javascript:window.__pwned=4">click</a>`;
  mock.push({ kind: "message", data: { id: "m-x1", from: MARKUP, to: "claude-2", type: "message", body, timestamp: mock.now(), correlation_id: null, reply_to: null, metadata: null } });
  mock.push({ kind: "message", data: { id: "q-x1", from: MARKUP, to: "operator", type: "question", body: `Which? ${MARKUP}`, timestamp: mock.now(), correlation_id: null, reply_to: null, metadata: { options: [MARKUP, "<i>no</i>"] } } });
  mock.push({ kind: "event", data: { seq: 500, timestamp: mock.now(), source: "daemon", event_type: "agent.status", agent_id: MARKUP, task_id: null, message: `status ${MARKUP}`, level: "warn", data: null } });

  // The question: banner and dialog.
  const dialog = page.getByRole("dialog", { name: "question" });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator(".question-body")).toHaveText(`Which? ${MARKUP}`);
  await expect(dialog.getByRole("button", { name: MARKUP })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "<i>no</i>" })).toBeVisible();
  await expect(page.locator(".question-banner")).toContainText(`${MARKUP} asks: Which? ${MARKUP}`);
  await page.keyboard.press("Escape");

  // The bus row and the message's panel.
  const row = page.locator("[data-message='m-x1']");
  await expect(row.locator(".from")).toHaveText(MARKUP);
  await expect(row.locator(".first")).toContainText("<b>bold</b><script>");
  await row.click();
  const panel = page.locator("#side-panel .message-panel");
  await expect(panel.locator("pre.body")).toHaveText(body);
  await expect(panel.locator("a")).toHaveCount(0);

  // The inspector and the hover card.
  await page.keyboard.press("Escape");
  await weird.locator(".node-head").hover();
  await expect(page.getByRole("tooltip")).toContainText(`<b>role</b>${MARKUP}`);
  await weird.locator(".node-head").click();
  const inspector = page.locator("#side-panel [data-inspector]");
  await expect(inspector.locator(".panel-title")).toHaveText(MARKUP);
  await expect(inspector).toContainText(`<b>role</b>${MARKUP}`);
  await expect(inspector).toContainText("<script>window.__pwned=2</script>");

  // The log.
  await page.keyboard.press("l");
  await expect(page.locator('#main [data-seq="500"] .msg')).toHaveText(`status ${MARKUP}`);
  await expect(page.locator('#main [data-seq="500"] .agent')).toHaveText(MARKUP);

  expect(await page.locator("img").count()).toBe(0);
  expect(await page.locator("#main b, #side-panel b:has-text('bold'), [role=dialog] i").count()).toBe(0);
  expect(await page.locator("body script").count()).toBe(0);
  expect(await page.evaluate(() => (window as unknown as { __pwned?: unknown }).__pwned)).toBeUndefined();
  expect(csp).toEqual([]);
});
