// The task DAG's layout (notes/web-design.md §graphs "Task DAG"): layers by
// longest path from the roots, as the TUI's `app::task_layers`, drawn left to
// right; inside a layer the order that lowers crossings, by two barycentre
// sweeps. A pure function of the task list.

import type { Box } from "./view";

export const TASK_W = 220;
export const TASK_H = 72;
const COL_GAP = 96;
const ROW_GAP = 22;

type T = { id: string; dependencies: readonly string[] };

/** Indices into `tasks`, by layer: layer 0 has no dependencies, layer n
 * depends on something in layer n-1. Unknown dependencies are ignored and
 * cycles are cut rather than looping. */
export function taskLayers(tasks: readonly T[]): number[][] {
  const index = new Map(tasks.map((t, i) => [t.id, i]));
  const memo: (number | "visiting" | undefined)[] = new Array(tasks.length);
  const depth = (i: number): number => {
    const m = memo[i];
    if (typeof m === "number") return m;
    if (m === "visiting") return 0;
    memo[i] = "visiting";
    let d = 0;
    for (const dep of tasks[i].dependencies) {
      const j = index.get(dep);
      if (j !== undefined) d = Math.max(d, depth(j) + 1);
    }
    memo[i] = d;
    return d;
  };
  const layers: number[][] = [];
  for (let i = 0; i < tasks.length; i++) {
    const d = depth(i);
    while (layers.length <= d) layers.push([]);
    layers[d].push(i);
  }
  return layers;
}

export type DagLayout = {
  boxes: Map<string, Box>;
  /** Task ids by layer, top to bottom: the order the arrow keys walk. */
  layers: string[][];
  /** Edges from a dependency to its dependant. */
  edges: { from: string; to: string }[];
};

export function layered(tasks: readonly T[]): DagLayout {
  const layersIdx = taskLayers(tasks);
  const index = new Map(tasks.map((t, i) => [t.id, i]));
  const deps = tasks.map((t) => t.dependencies.map((d) => index.get(d)).filter((j): j is number => j !== undefined));
  const dependants: number[][] = tasks.map(() => []);
  deps.forEach((ds, i) => ds.forEach((j) => dependants[j].push(i)));

  // Barycentre sweeps: down using dependencies, then up using dependants.
  const order = layersIdx.map((l) => [...l]);
  const pos = new Map<number, number>();
  const place = () => order.forEach((l) => l.forEach((t, i) => pos.set(t, i)));
  place();
  const sweep = (layer: number[], neighbours: (t: number) => number[]) => {
    const bary = new Map<number, number>();
    for (const t of layer) {
      const ns = neighbours(t).filter((n) => pos.has(n));
      bary.set(t, ns.length ? ns.reduce((s, n) => s + pos.get(n)!, 0) / ns.length : pos.get(t)!);
    }
    layer.sort((a, b) => bary.get(a)! - bary.get(b)! || pos.get(a)! - pos.get(b)!);
    layer.forEach((t, i) => pos.set(t, i));
  };
  for (let l = 1; l < order.length; l++) sweep(order[l], (t) => deps[t]);
  for (let l = order.length - 2; l >= 0; l--) sweep(order[l], (t) => dependants[t]);

  const tallest = Math.max(0, ...order.map((l) => l.length));
  const boxes = new Map<string, Box>();
  order.forEach((layer, l) => {
    const top = ((tallest - layer.length) * (TASK_H + ROW_GAP)) / 2;
    layer.forEach((t, i) => boxes.set(tasks[t].id, { x: l * (TASK_W + COL_GAP), y: top + i * (TASK_H + ROW_GAP), w: TASK_W, h: TASK_H }));
  });
  const edges: { from: string; to: string }[] = [];
  deps.forEach((ds, i) => ds.forEach((j) => edges.push({ from: tasks[j].id, to: tasks[i].id })));
  return { boxes, layers: order.map((l) => l.map((t) => tasks[t].id)), edges };
}

/** Everything `id` builds on and everything that waits for it. */
export function lineage(tasks: readonly T[], id: string): Set<string> {
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const out = new Set<string>([id]);
  const up = (x: string) => {
    for (const d of byId.get(x)?.dependencies ?? []) if (!out.has(d) && byId.has(d)) (out.add(d), up(d));
  };
  const down = (x: string) => {
    for (const t of tasks) if (t.dependencies.includes(x) && !out.has(t.id)) (out.add(t.id), down(t.id));
  };
  up(id);
  down(id);
  return out;
}
