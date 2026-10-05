import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
import { a11yFailures, auditPage } from "../dev/a11y.ts";

test.use({ mockOptions: { scenario: "busy", fixture: "subagents" }, viewport: { width: 1440, height: 960 } });

test("Tab-only path reaches workspace switching, graph, panel, and labelled composer", async ({ page, openPage }) => {
  await openPage();
  await page.locator("[data-switcher-button]").focus();
  const seen = new Set<string>();
  for (let i = 0; i < 80; i++) {
    await page.keyboard.press("Tab");
    const region = await page.evaluate(() => {
      const el = document.activeElement;
      if (el?.id === "compose-text") return "composer";
      if (el?.hasAttribute("data-agent")) return "agent";
      return el?.closest("[data-region]")?.getAttribute("data-region") ?? "";
    });
    seen.add(region);
    if (region === "composer") break;
  }
  expect(seen.has("agent")).toBe(true);
  expect(seen.has("panel")).toBe(true);
  expect(seen.has("composer")).toBe(true);
  await expect(page.getByRole("textbox", { name: /message|broadcast/i }).first()).toBeVisible();
});

test("modal Tab containment and Escape return focus to its keyboard trigger", async ({ page, openPage }) => {
  await openPage();
  const node = page.locator('[data-agent="claude-1"]');
  await node.focus();
  await page.keyboard.press("s");
  const modal = page.getByRole("dialog", { name: "model and CLI", exact: true });
  await expect(modal).toHaveAttribute("aria-modal", "true");
  for (let i = 0; i < 15; i++) {
    await page.keyboard.press(i % 2 ? "Shift+Tab" : "Tab");
    expect(await page.evaluate(() => !!document.activeElement?.closest('[role="dialog"]'))).toBe(true);
  }
  await page.keyboard.press("Escape");
  await expect(modal).toHaveCount(0);
  await expect(node).toBeFocused();
});

test("long Unicode evidence is plain text and never executes or fetches markup", async ({ page, mock, openPage, csp }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  const message = { ...mock.state.messages.at(-1), id: "m-a11y-inert", body: "日本語 · café · مرحبًا · 👩🏽‍💻\n<img src=https://example.invalid/leak onerror=alert(1)>\n<script>window.hacked=true</script>" };
  mock.push({ kind: "message", data: message });
  await page.evaluate((id) => { location.hash = `#/w/${location.hash.split("/")[2]}/?message=${id}`; }, message.id);
  await expect(page.locator(".message-panel")).toContainText(message.body);
  await expect(page.locator(".message-panel img, .message-panel script")).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { hacked?: boolean }).hacked)).toBeUndefined();
  expect(guard.requests).toEqual([]);
  expect(csp).toEqual([]);
});

test("capture includes inspectable accessibility evidence from rendered controls", async ({ page, openPage }, testInfo) => {
  await openPage();
  await page.locator('[data-agent="claude-1"]').focus();
  const evidence = await auditPage(page);
  expect(evidence.contrast.checked).toBeGreaterThan(0);
  expect(evidence.focused?.element).toBe("claude-1, working");
  expect(a11yFailures(evidence)).toEqual([]);
  await testInfo.attach("accessibility-evidence", { body: JSON.stringify(evidence, null, 2), contentType: "application/json" });
});

test("strict collector gates active contrast, names, and overflow while exempting disabled text", async ({ page }) => {
  // Synthetic page tests the measurement tool only; galleries always use the
  // real production application. This page has no production CSP to bypass.
  await page.setContent('<main style="min-width:1800px;color:#000;background:#fff;font:14px system-ui"><p style="color:#aaa">Unreadable active text</p><button disabled style="color:#aaa;background:#fff">Disabled control</button><input aria-label=" "><button>Named control</button></main>');
  const evidence = await auditPage(page);
  expect(evidence.contrast.failures.some((f) => f.text === "Unreadable active text")).toBe(true);
  expect(evidence.contrast.failures.some((f) => f.text === "Disabled control")).toBe(false);
  expect(evidence.contrast.disabledSkipped).toBeGreaterThan(0);
  expect(evidence.unnamedControls).toHaveLength(1);
  expect(evidence.overflow).toBe(true);
  expect(a11yFailures(evidence).some((f) => f.startsWith("Document overflow"))).toBe(true);
});
