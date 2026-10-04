// The VCS animations of notes/jj-design.md §anim on the graph (§tests "anim.spec.ts").
import type { Mock, PushFrame } from "./fixtures.ts";
import { expect, test } from "./fixtures.ts";

const vcs = (mock: Mock, op: string, outcome: string): PushFrame => {
  const [frame] = mock.pushes((f) => {
    const d = (f.push.data as { data?: { op?: string; outcome?: string } } | undefined)?.data;
    return f.push.kind === "event" && d?.op === op && d?.outcome === outcome;
  });
  if (!frame) throw new Error(`no ${op} ${outcome} in pushes.jsonl`);
  return mock.retime(frame);
};

test.use({ viewport: { width: 1400, height: 860 } });

test("a merge: running label and held link, then the pulse, then the done label, gone after MARK_MS", async ({ page, mock, openPage, csp }) => {
  await openPage();
  mock.push(vcs(mock, "v12", "started"));
  const mark = page.locator('[data-mark="claude-3"]');
  await expect(mark).toHaveText(/merge claude-5/);
  await expect(mark.locator(".spin")).toHaveCount(1);
  const link = page.locator('[data-link="claude-5:claude-3"]');
  await expect(link).toHaveClass(/held/);
  // A held link has no pulse.
  await expect(link.locator(".pulse")).toHaveCount(0);

  const t0 = Date.now();
  mock.push(vcs(mock, "v12", "ok"));
  await expect(link).toHaveClass(/tone-ok/);
  await expect(link.locator(".pulse.vcs")).toHaveCount(1);
  // The done label shows when the pulse arrives (PULSE_MS 900), then the sweep runs.
  await expect(mark).toHaveText("◆ 5e6f7a8b");
  expect(Date.now() - t0).toBeGreaterThanOrEqual(800);
  await expect(page.locator(".mark.done .sweep")).toHaveCount(1);
  await expect(link.locator(".pulse")).toHaveCount(0);
  // Gone after MARK_MS (2400 after it started), the link after LINK_MS (3000).
  await expect(mark).toHaveCount(0, { timeout: 4000 });
  expect(Date.now() - t0).toBeGreaterThanOrEqual(900 + 2400 - 200);
  await expect(link).toHaveCount(0, { timeout: 2000 });
  expect(csp).toEqual([]);
});

test("a read shows the glance: muted, on the bottom edge, short", async ({ page, mock, openPage }) => {
  await openPage();
  mock.push(vcs(mock, "v5", "ok"));
  const mark = page.locator('[data-mark="claude-2"]');
  await expect(mark).toHaveText("· status");
  await expect(page.locator(".mark.glance")).toHaveCount(1);
  await expect(mark).toHaveCount(0, { timeout: 2000 });
});

test("a conflict links in the conflict tone and counts the files", async ({ page, mock, openPage }) => {
  await openPage();
  mock.push(vcs(mock, "v13", "started"));
  mock.push(vcs(mock, "v13", "conflict"));
  await expect(page.locator('[data-link="claude-6:claude-3"]')).toHaveClass(/tone-conflict/);
  await expect(page.locator('[data-mark="claude-3"]')).toHaveText("✗ 2 files");
});

test("hovering a mark shows the event; clicking it opens the event in the log", async ({ page, mock, openPage }) => {
  await openPage();
  mock.push(vcs(mock, "v3", "ok"));
  const mark = page.locator('[data-mark="claude-1"]');
  await expect(mark).toHaveText("◆ 3f4e5d6c");
  await mark.hover();
  await expect(page.locator(".hover-card")).toContainText("3f4e5d6c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a");
  await mark.click();
  await expect(page).toHaveURL(/#\/w\/[0-9a-f]{16}\/logs\?event=\d+/);
  await expect(page.locator("#side-panel")).toContainText("vcs.commit");
});

test("a message push starts a pulse along its link", async ({ page, mock, openPage }) => {
  await openPage();
  mock.push(mock.retime({ kind: "message", data: { id: "m-p2p", from: "claude-3", to: "claude-1", type: "message", body: "hi", timestamp: 0 } }));
  const link = page.locator('[data-link="claude-3:claude-1"]');
  await expect(link).toHaveClass(/kind-p2p/);
  await expect(link.locator(".pulse.msg")).toHaveCount(1);
  await expect(page.locator(".p2p-banner")).toBeVisible();
  await expect(link.locator(".pulse")).toHaveCount(0, { timeout: 2000 });
});

test("an old event plays nothing; an idle page asks for no animation frame", async ({ page, mock, openPage }) => {
  await openPage();
  const frame = vcs(mock, "v3", "ok");
  (frame.push.data as { timestamp: number }).timestamp = mock.now() - 11_000;
  mock.push(frame);
  await expect(page.locator(".log-row, [data-mark]")).toHaveCount(0);
  const frames = await page.evaluate(async () => {
    let n = 0;
    const orig = window.requestAnimationFrame;
    window.requestAnimationFrame = (cb) => (n++, orig(cb));
    await new Promise((r) => setTimeout(r, 1500));
    window.requestAnimationFrame = orig;
    return n;
  });
  expect(frames).toBe(0);
});

test.describe("with reduced motion", () => {
  test.use({ reducedMotion: "reduce" });
  test("nothing moves; links and labels still light up and expire", async ({ page, mock, openPage }) => {
    await openPage();
    mock.push(vcs(mock, "v12", "started"));
    mock.push(vcs(mock, "v12", "ok"));
    const link = page.locator('[data-link="claude-5:claude-3"]');
    await expect(link).toHaveClass(/tone-ok/);
    await expect(page.locator(".pulse")).toHaveCount(0);
    await expect(page.locator('[data-mark="claude-3"]')).toHaveText("◆ 5e6f7a8b");
    await expect(page.locator(".sweep")).toHaveCount(0);
    await expect(page.locator('[data-mark="claude-3"]')).toHaveCount(0, { timeout: 4000 });
  });
});
