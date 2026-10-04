// jj-design §anim on the page, driven by what a real daemon emits for a real
// scripted team (notes/web-design.md §tests "vcs.spec.ts"). The marks last
// about two seconds, so an observer in the page records every mark and link
// that ever appears.
import type { Page } from "@playwright/test";
import { expect, test } from "./harness.ts";

type Seen = { marks: string[]; links: string[] };

async function record(page: Page) {
  await page.addInitScript(() => {
    const seen = { marks: [] as string[], links: [] as string[] };
    (window as unknown as { __seen: typeof seen }).__seen = seen;
    const scan = () => {
      for (const m of document.querySelectorAll("[data-mark]")) {
        const key = `${m.getAttribute("data-mark")}|${(m.textContent ?? "").trim()}`;
        if (!seen.marks.includes(key)) seen.marks.push(key);
      }
      for (const l of document.querySelectorAll("[data-link]")) {
        const tone = /tone-(\w+)/.exec(l.getAttribute("class") ?? "")?.[1] ?? "";
        const key = `${l.getAttribute("data-link")}|${tone}`;
        if (!seen.links.includes(key)) seen.links.push(key);
      }
    };
    new MutationObserver(scan).observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
  });
}
const seen = (page: Page) => page.evaluate(() => (window as unknown as { __seen: Seen }).__seen);

test("a commit, a cherry-pick, a conflicting merge and a read animate on the right nodes", async ({ page, daemon, openLive }) => {
  await record(page);
  daemon.start();
  await openLive(daemon.web());

  // dev commits and publishes; peer cherry-picks the commit.
  daemon.send("dev", "go");
  daemon.until("dev's commit and peer's cherry-pick", () => {
    const e = daemon.vcsEvents().filter((v) => v.vcs.outcome !== "started");
    return e.some((v) => v.vcs.action === "commit" && v.vcs.agent === "dev" && v.vcs.outcome === "ok") && e.some((v) => v.vcs.action === "cherry_pick" && v.vcs.agent === "peer" && v.vcs.outcome === "ok");
  });
  // rival's clashing schema, then a merge of dev that conflicts.
  daemon.send("rival", "go");
  daemon.until("rival's conflicting merge", () => daemon.vcsEvents().some((v) => v.vcs.action === "merge" && v.vcs.agent === "rival" && v.vcs.outcome === "conflict"));

  // The pulse travels before the result shows (PULSE_MS): wait for the last mark.
  await expect.poll(async () => (await seen(page)).marks.some((m) => m.startsWith("rival|✗")), { timeout: 10_000 }).toBe(true);
  // A read: a glance on the box of the agent that looked.
  daemon.send("echo", "look");
  await expect.poll(async () => (await seen(page)).marks.includes("echo|· status"), { timeout: 10_000 }).toBe(true);
  const s = await seen(page);
  const events = daemon.vcsEvents().filter((v) => v.vcs.outcome !== "started");
  const commit = events.find((v) => v.vcs.action === "commit" && v.vcs.agent === "dev")!.vcs.commit!;
  // The commit: a done mark on dev with the short commit id.
  expect(s.marks).toContain(`dev|◆ ${commit.slice(0, 8)}`);
  // The cherry-pick: a link from dev to peer, and a mark on peer.
  expect(s.links.some((l) => l.startsWith("dev:peer|"))).toBe(true);
  expect(s.marks.some((m) => m.startsWith("peer|"))).toBe(true);
  // The conflict: a link in the conflict tone from dev to rival, and the files on rival.
  expect(s.links).toContain("dev:rival|conflict");
  expect(s.marks.filter((m) => m.startsWith("rival|✗")), JSON.stringify(s)).not.toEqual([]);
  // Each action the page animated is in the daemon's log, with the backend the team runs on.
  const backends = new Set(events.map((v) => v.vcs.backend));
  expect(backends.size).toBe(1);
  if (process.env.CREW_TEST_VCS === "jj" || process.env.CREW_TEST_VCS === "git") expect([...backends][0]).toBe(process.env.CREW_TEST_VCS);
  // Every operation that started ended exactly once.
  const all = daemon.vcsEvents();
  for (const st of all.filter((v) => v.vcs.outcome === "started")) expect(all.filter((v) => v.vcs.op === st.vcs.op && v.vcs.outcome !== "started")).toHaveLength(1);
});
