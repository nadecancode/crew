// The agent graph's layout: the TUI's topology (crates/crew-tui/src/layout.rs),
// so that both dashboards read the same. The orchestrator in the centre,
// members on an ellipse, the first at the top left and the rest clockwise in
// the TUI's order, subagents as satellites beside their parent. A pure
// function of the agents; world coordinates, the viewport fits them.

import { ORCHESTRATOR_ID } from "../protocol";
import { isOrchestrator, satelliteParent } from "../store/order";
import { center, overlaps, type Box } from "./view";

export const NODE_W = 212;
export const NODE_H = 80;
export const HUB_W = 236;
export const HUB_H = 80;
export const SAT_W = 180;
export const SAT_H = 64;
const GAP = 28;

type A = { id: string; orchestrator: boolean; parent?: string | null };

export type Layout = {
  /** By agent id; the hub is under `orchestrator` whether or not that agent exists. */
  boxes: Map<string, Box>;
  /** Members on the ring, in order. */
  ring: string[];
  /** Satellite id to its parent. */
  satellites: Map<string, string>;
};

/** The angle of member `i` of `n`, clockwise from the top, as the TUI's `angle`. */
export function angle(i: number, n: number): number {
  const degrees = n === 1 ? 0 : -180 / n + (i * 360) / n;
  return (degrees * Math.PI) / 180;
}

function ring(n: number, rx: number, ry: number): Box[] {
  const out: Box[] = [];
  for (let i = 0; i < n; i++) {
    const a = angle(i, n);
    const cx = rx * Math.sin(a), cy = -ry * Math.cos(a);
    out.push({ x: cx - NODE_W / 2, y: cy - NODE_H / 2, w: NODE_W, h: NODE_H });
  }
  return out;
}

function clear(boxes: readonly Box[], hub: Box): boolean {
  for (let i = 0; i < boxes.length; i++) {
    if (overlaps(boxes[i], hub, GAP * 2)) return false;
    for (let j = i + 1; j < boxes.length; j++) if (overlaps(boxes[i], boxes[j], GAP)) return false;
  }
  return true;
}

export function radial(agents: readonly A[]): Layout {
  const hub: Box = { x: -HUB_W / 2, y: -HUB_H / 2, w: HUB_W, h: HUB_H };
  const boxes = new Map<string, Box>([[ORCHESTRATOR_ID, hub]]);
  const satellites = new Map<string, string>();
  const members: string[] = [];
  const subs = new Map<string, string[]>();
  for (const a of agents) {
    if (isOrchestrator(a)) {
      if (a.id !== ORCHESTRATOR_ID) boxes.set(a.id, hub);
      continue;
    }
    const parent = satelliteParent(agents, a);
    if (parent) {
      satellites.set(a.id, parent.id);
      subs.set(parent.id, [...(subs.get(parent.id) ?? []), a.id]);
    } else members.push(a.id);
  }
  const n = members.length;
  // A pleasantly wide ellipse, grown until nothing touches.
  let rx = Math.max(300, (n * (NODE_W + GAP)) / (2 * Math.PI) * 1.15);
  let ry = Math.max(HUB_H / 2 + NODE_H / 2 + GAP * 3, rx * 0.58);
  let placed = ring(n, rx, ry);
  for (let i = 0; i < 200 && !clear(placed, hub); i++) {
    rx *= 1.06;
    ry *= 1.06;
    placed = ring(n, rx, ry);
  }
  members.forEach((id, i) => boxes.set(id, placed[i]));

  // Satellites take the free spot nearest their parent, preferring the side
  // away from the hub, with air around them so the tether stays visible.
  const taken: Box[] = [hub, ...placed];
  for (const parent of members) {
    const kids = subs.get(parent);
    if (!kids) continue;
    const p = boxes.get(parent)!;
    const pc = center(p);
    const len = Math.hypot(pc.x, pc.y) || 1;
    const ux = len < 1 ? 0 : pc.x / len, uy = len < 1 ? -1 : pc.y / len;
    const px = -uy, py = ux;
    for (const kid of kids) {
      let spot: Box | null = null;
      search: for (let d = 1; d <= 8; d++) {
        const dist = (NODE_H / 2 + SAT_H / 2 + GAP) * (d === 1 ? 1 : 0.75 * d) + (Math.abs(ux) * (NODE_W - NODE_H)) / 2;
        for (const side of [0, 1, -1, 2, -2, 3, -3]) {
          const off = side * (SAT_W * 0.6 + GAP);
          const cx = pc.x + ux * dist + px * off, cy = pc.y + uy * dist + py * off;
          const b = { x: cx - SAT_W / 2, y: cy - SAT_H / 2, w: SAT_W, h: SAT_H };
          if (!taken.some((t) => overlaps(t, b, GAP / 2))) {
            spot = b;
            break search;
          }
        }
      }
      // Nowhere near: further out, still on its parent's side.
      spot ??= { x: pc.x + ux * 900 - SAT_W / 2, y: pc.y + uy * 900 - SAT_H / 2 + taken.length * (SAT_H + GAP), w: SAT_W, h: SAT_H };
      taken.push(spot);
      boxes.set(kid, spot);
    }
  }
  return { boxes, ring: members, satellites };
}
