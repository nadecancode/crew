// Operator actions from the page against a real daemon; the daemon's own
// events confirm each (notes/web-design.md §tests "operate.spec.ts").
import type { Page } from "@playwright/test";
import { expect, test } from "./harness.ts";

const node = (page: Page, id: string) => page.locator(`[data-agent="${id}"]`);
const inspect = async (page: Page, id: string) => {
  await node(page, id).locator(".node-head").click();
  await expect(page.locator(`#side-panel [data-inspector="${id}"]`)).toBeVisible();
};
const toolbar = (page: Page, id: string) => page.getByRole("toolbar", { name: `actions for ${id}` });
const status = (page: Page) => page.locator("footer .status-line");

test.use({ viewport: { width: 1400, height: 900 } });

test("message an agent: it reacts, and the bus shows both sides", async ({ page, daemon, openLive }) => {
  daemon.start();
  await openLive(daemon.web());
  await inspect(page, "echo");
  await toolbar(page, "echo").getByRole("button", { name: "message", exact: true }).click();
  const box = page.locator("#compose-text");
  await box.fill("hello from the browser");
  await box.press("Enter");
  await expect(status(page)).toHaveText("✓ message sent to echo");
  await box.press("Escape");
  await page.keyboard.press("Escape");
  await expect(page.locator("#side-panel [data-inspector]")).toHaveCount(0);
  await expect(page.locator("[data-message]", { hasText: "operator → echo" })).toContainText("hello from the browser");
  await expect(page.locator("[data-message]", { hasText: "echo → operator" })).toContainText("ack from echo");
  daemon.until("the daemon's bus event", () => daemon.events().some((e) => e.event_type === "message.sent" && e.message.includes("hello from the browser")));
});

test("a question from an agent: the dialog opens by itself and the answer reaches the daemon", async ({ page, daemon, openLive }) => {
  daemon.start();
  await openLive(daemon.web());
  // dev commits and publishes, peer cherry-picks and then asks.
  daemon.send("dev", "go");
  const dialog = page.getByRole("dialog", { name: "question" });
  await expect(dialog).toBeVisible({ timeout: 15_000 });
  await expect(dialog).toContainText("Ship the schema?");
  await expect(node(page, "peer").locator(".badge.ask")).toBeVisible();
  await expect(page).toHaveTitle(/^\(1\) web-live · crew$/);
  await dialog.getByRole("textbox", { name: "your own answer" }).fill("yes, ship it");
  await dialog.getByRole("textbox", { name: "your own answer" }).press("Enter");
  await expect(dialog).toHaveCount(0);
  await expect(page.locator(".question-banner")).toHaveCount(0);
  daemon.until("question.answered", () => daemon.events().some((e) => e.event_type === "question.answered"));
  // The answer is on the bus, to the asker.
  expect(daemon.c(["messages", "--json", "-n", "100"])).toContain("yes, ship it");
  await expect(page).toHaveTitle("web-live · crew");
});

test("pause and resume, then kill after the confirmation", async ({ page, daemon, openLive }) => {
  daemon.start();
  await openLive(daemon.web());
  await inspect(page, "idle");
  const tb = toolbar(page, "idle");
  await tb.getByRole("button", { name: "pause", exact: true }).click();
  await expect(node(page, "idle").locator(".badge.paused")).toBeVisible();
  daemon.until("agent.paused", () => daemon.events().some((e) => e.event_type === "agent.paused" && e.agent_id === "idle"));
  await tb.getByRole("button", { name: "resume", exact: true }).click();
  await expect(node(page, "idle").locator(".badge.paused")).toHaveCount(0);
  daemon.until("agent.resumed", () => daemon.events().some((e) => e.event_type === "agent.resumed" && e.agent_id === "idle"));

  // Kill asks first: esc sends nothing.
  await page.keyboard.press("k");
  const dialog = page.getByRole("dialog", { name: "kill agent" });
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  expect(daemon.events().some((e) => e.event_type === "agent.killed")).toBe(false);
  await tb.getByRole("button", { name: "kill", exact: true }).click();
  await dialog.getByRole("button", { name: "kill idle" }).click();
  daemon.until("agent.killed", () => daemon.events().some((e) => e.event_type === "agent.killed" && e.agent_id === "idle"));
  await expect(status(page)).toHaveText("✓ kill idle");
});
