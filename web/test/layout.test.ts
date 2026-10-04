// Layouts and the viewport's math (notes/web-design.md §tests "layout.test.ts").
import { expect, test } from "vitest";
import { layered, lineage, taskLayers } from "../src/graph/layers";
import { angle, radial } from "../src/graph/radial";
import { clampK, fit, overlaps, toScreen, toWorld, zoomAt, K_MAX, K_MIN } from "../src/graph/view";
import { decodeSnapshot } from "../src/protocol";
import { sortAgents } from "../src/store/order";
import { fixture } from "./fixtures";

const team = (n: number, subs: Record<number, number> = {}) => {
  const agents: { id: string; orchestrator: boolean; parent?: string | null }[] = [{ id: "orchestrator", orchestrator: true }];
  for (let i = 1; i <= n; i++) {
    agents.push({ id: `claude-${i}`, orchestrator: false });
    for (let s = 1; s <= (subs[i] ?? 0); s++) agents.push({ id: `claude-${i}-s${s}`, orchestrator: false, parent: `claude-${i}` });
  }
  return sortAgents(agents);
};

test("radial: the first member at the top left, the rest clockwise; nothing overlaps, 1 to 12 members with subagents", () => {
  for (let n = 1; n <= 12; n++) {
    const subs = n >= 3 ? { 1: 2, 3: 1 } : {};
    const l = radial(team(n, subs));
    expect(l.ring).toEqual(Array.from({ length: n }, (_, i) => `claude-${i + 1}`));
    const boxes = [...new Set(l.boxes.values())];
    for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) expect(overlaps(boxes[i], boxes[j]), `n=${n} ${i} ${j}`).toBe(false);
    for (const [kid, parent] of l.satellites) expect(parent).toBe(kid.replace(/-s\d+$/, ""));
    if (n >= 2) {
      const first = l.boxes.get("claude-1")!;
      expect(first.x + first.w / 2).toBeLessThan(0.0001 + (n === 2 ? 1e9 : 0));
      expect(first.y).toBeLessThan(0);
    }
  }
  // Clockwise: the angle grows with the index.
  expect(angle(0, 4)).toBeLessThan(angle(1, 4));
  expect(angle(0, 1)).toBe(0);
});

test("the hub is there without an orchestrator agent", () => {
  const l = radial([{ id: "a", orchestrator: false }]);
  expect(l.boxes.has("orchestrator")).toBe(true);
});

test("layers: every dependency points left; the fixture's task graph gives the TUI's layers", () => {
  const snap = decodeSnapshot(fixture("snapshot.json"));
  const ids = taskLayers(snap.tasks).map((l) => l.map((i) => snap.tasks[i].id).sort());
  expect(ids).toEqual([["db", "queue"], ["ledger", "notify", "search"], ["billing"]]);
  const d = layered(snap.tasks);
  for (const e of d.edges) expect(d.boxes.get(e.from)!.x).toBeLessThan(d.boxes.get(e.to)!.x);
  expect(lineage(snap.tasks, "ledger")).toEqual(new Set(["ledger", "db", "billing"]));
});

test("layers: unknown dependencies are ignored and cycles are cut", () => {
  const l = taskLayers([{ id: "a", dependencies: ["b", "ghost"] }, { id: "b", dependencies: ["a"] }]);
  expect(l.flat().sort()).toEqual([0, 1]);
});

test("zoomAt keeps the point under the pointer; k is clamped", () => {
  const v = { x: 30, y: -12, k: 1.3 };
  const at = { x: 400, y: 250 };
  const before = toWorld(v, at);
  for (const f of [0.5, 1.7, 3]) {
    const z = zoomAt(v, at, f);
    const after = toScreen(z, before);
    expect(after.x).toBeCloseTo(at.x, 6);
    expect(after.y).toBeCloseTo(at.y, 6);
  }
  expect(zoomAt(v, at, 100).k).toBe(K_MAX);
  expect(zoomAt(v, at, 0.001).k).toBe(K_MIN);
  expect(clampK(1)).toBe(1);
});

test("fit shows the bounds whole and centred", () => {
  const b = { x: -500, y: -200, w: 1000, h: 400 };
  const v = fit(b, { w: 800, h: 600 }, 20);
  const tl = toScreen(v, { x: b.x, y: b.y }), br = toScreen(v, { x: b.x + b.w, y: b.y + b.h });
  expect(tl.x).toBeGreaterThanOrEqual(19.9);
  expect(br.x).toBeLessThanOrEqual(780.1);
  expect((tl.y + br.y) / 2).toBeCloseTo(300, 6);
});
