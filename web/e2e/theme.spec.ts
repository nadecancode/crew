import type { Page } from "@playwright/test";
import { expect, noRequestsAfterStart, test } from "./fixtures.ts";

test.use({ viewport: { width: 1440, height: 960 } });

const surfaces = ["canvas", "surface", "surface-raised", "action-surface", "warn-surface", "err-surface"];
const text = ["text", "muted", "quiet", "action", "info", "ok", "warn", "err", "accent", "codex", "pi", "shell", "p2p"];
const neutrals = ["canvas", "surface", "surface-raised", "action-surface", "text", "muted", "quiet", "line", "control-border", "bg-term", "term-text", "term-selection"];
const categories = ["token-input", "token-output", "token-cache-read", "token-cache-write"];
const names = [...new Set([...surfaces, ...text, ...neutrals, ...categories])];
type Palette = Record<string, number[]>;

async function palette(page: Page): Promise<Palette> {
  return page.evaluate((names) => {
    // Resolve light-dark() and aliases in the browser, rather than duplicating
    // source hex values or relying on unresolved custom-property strings.
    const probe = document.createElement("span");
    probe.hidden = true;
    document.body.append(probe);
    try {
      return Object.fromEntries(names.map((name) => {
        if (!getComputedStyle(document.documentElement).getPropertyValue(`--${name}`).trim()) throw new Error(`Missing palette token --${name}`);
        probe.style.color = `var(--${name})`;
        const color = getComputedStyle(probe).color;
        const rgb = /^rgb\(([^)]+)\)$/.exec(color);
        if (!rgb) throw new Error(`Unsupported palette color --${name}: ${color}`);
        return [name, rgb[1].split(/[,\s]+/).filter(Boolean).map(Number)];
      }));
    } finally { probe.remove(); }
  }, names);
}

function luminance(rgb: number[]): number {
  const linear = rgb.map((v) => { const n = v / 255; return n <= .04045 ? n / 12.92 : ((n + .055) / 1.055) ** 2.4; });
  return linear[0] * .2126 + linear[1] * .7152 + linear[2] * .0722;
}
function contrast(a: number[], b: number[]): number {
  const x = luminance(a), y = luminance(b);
  return (Math.max(x, y) + .05) / (Math.min(x, y) + .05);
}

test("dark appearance has neutral charcoal surfaces, text and terminal with non-green action chrome", async ({ page, mock, openPage, csp }) => {
  const guard = noRequestsAfterStart(page, mock);
  mock.prefs = { keys: true, theme: "dark" };
  await page.emulateMedia({ colorScheme: "light" });
  await openPage();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  const colors = await palette(page);
  for (const name of neutrals) {
    const [r, g, b] = colors[name];
    expect(Math.max(r, g, b) - Math.min(r, g, b), `--${name} channel spread`).toBeLessThanOrEqual(3);
    expect(g, `--${name} green dominance`).toBeLessThanOrEqual(Math.max(r, b));
  }
  expect(Math.max(...colors.canvas), "dark canvas").toBeLessThan(64);
  expect(luminance(colors.surface)).toBeGreaterThan(luminance(colors.canvas));
  expect(luminance(colors["surface-raised"])).toBeGreaterThan(luminance(colors.surface));
  const [r, g, b] = colors.action;
  expect(g, "non-green action").toBeLessThanOrEqual(Math.max(r, b));
  expect(await page.locator("body").evaluate((el) => getComputedStyle(el).backgroundColor)).toBe(`rgb(${colors.canvas.join(", ")})`);
  expect(await page.locator(".status-bar").evaluate((el) => getComputedStyle(el).backgroundColor)).toBe(`rgb(${colors.surface.join(", ")})`);
  expect(await page.locator("#side-panel").evaluate((el) => getComputedStyle(el).backgroundColor)).toBe(`rgb(${colors.surface.join(", ")})`);
  const modes = page.getByRole("group", { name: "Team presentation", exact: true });
  const active = modes.getByRole("button", { name: "Graph", exact: true });
  const inactive = modes.getByRole("button", { name: "List", exact: true });
  await expect(active).toHaveAttribute("aria-pressed", "true");
  await expect(inactive).toHaveAttribute("aria-pressed", "false");
  const activeBorder = await active.evaluate((el) => getComputedStyle(el).borderColor);
  const inactiveBorder = await inactive.evaluate((el) => getComputedStyle(el).borderColor);
  expect(activeBorder).not.toBe(inactiveBorder);
  expect(contrast(colors.action, colors["control-border"]), "pressed border has a luminance cue").toBeGreaterThan(1.25);
  expect(guard.requests).toEqual([]);
  expect(csp).toEqual([]);
});

for (const theme of ["light", "dark"] as const) {
  test(`${theme} selected graph and task outlines remain distinct from keyboard focus and the hub`, async ({ page, mock, openPage, csp }) => {
    mock.prefs = { keys: true, theme };
    await openPage();
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    const node = page.locator('.agent-graph [data-agent="claude-1"]');
    await node.locator(".node-head").click();
    await expect(page.locator('[data-inspector="claude-1"]')).toBeVisible();
    await expect(node).toHaveClass(/\bselected\b/);
    const selectedNode = await node.locator("rect.box").evaluate((el) => getComputedStyle(el).stroke);
    const hub = await page.locator(".agent-graph .node.hub rect.box").evaluate((el) => getComputedStyle(el).stroke);
    expect(selectedNode, "selected agent and orchestrator hub").not.toBe(hub);
    await page.keyboard.press("Tab");
    await node.focus();
    await expect(node).toBeFocused();
    expect(await node.evaluate((el) => el.matches(":focus-visible"))).toBe(true);
    const focusedNode = await node.locator("rect.box").evaluate((el) => getComputedStyle(el).stroke);
    expect(selectedNode, "selected agent and keyboard focus").not.toBe(focusedNode);

    await page.getByRole("navigation", { name: "views", exact: true }).getByRole("button", { name: "Tasks", exact: true }).click();
    const task = page.locator('.task-dag [data-task="ledger"]');
    await task.click();
    await expect(page.locator(".task-panel")).toBeVisible();
    await expect(task).toHaveClass(/\bselected\b/);
    const selectedTask = await task.locator("rect.box").evaluate((el) => getComputedStyle(el).stroke);
    await page.keyboard.press("Tab");
    await task.focus();
    await expect(task).toBeFocused();
    expect(await task.evaluate((el) => el.matches(":focus-visible"))).toBe(true);
    expect(await task.locator("rect.box").evaluate((el) => getComputedStyle(el).stroke), "selected task shows keyboard focus").not.toBe(selectedTask);
    await page.keyboard.press("ArrowDown");
    await expect(task).toHaveClass(/\bselected\b/);
    await expect(task).not.toBeFocused();
    expect(await task.locator("rect.box").evaluate((el) => getComputedStyle(el).stroke)).toBe(selectedTask);
    const focusedTask = page.locator(".task-dag .task:focus-visible rect.box");
    await expect(focusedTask).toHaveCount(1);
    expect(await focusedTask.evaluate((el) => getComputedStyle(el).stroke), "selected task and arrow-key cursor").not.toBe(selectedTask);
    expect(csp).toEqual([]);
  });

  test(`${theme} semantic text and control boundaries retain contrast on all six surfaces`, async ({ page, mock, openPage, csp }, testInfo) => {
    const guard = noRequestsAfterStart(page, mock);
    mock.prefs = { keys: true, theme };
    await openPage();
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    const colors = await palette(page);
    const measured = Object.fromEntries([...text, "control-border"].map((name) => [name, Math.min(...surfaces.map((surface) => contrast(colors[name], colors[surface])))]));
    for (const name of text) expect(measured[name], `${theme} --${name} contrast`).toBeGreaterThanOrEqual(4.5);
    expect(measured["control-border"]).toBeGreaterThanOrEqual(3);
    expect(contrast(colors["term-text"], colors["bg-term"])).toBeGreaterThanOrEqual(4.5);
    for (let i = 0; i < categories.length; i++) for (let j = i + 1; j < categories.length; j++) {
      const a = colors[categories[i]], b = colors[categories[j]];
      expect(Math.hypot(...a.map((v, c) => v - b[c])), `${categories[i]} and ${categories[j]} remain distinct`).toBeGreaterThan(32);
    }
    await testInfo.attach("resolved-palette-and-contrast", { body: JSON.stringify({ theme, colors, minimumContrast: measured }, null, 2), contentType: "application/json" });
    expect(guard.requests).toEqual([]);
    expect(csp).toEqual([]);
  });
}
