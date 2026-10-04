// The task DAG (notes/web-design.md §graphs "Task DAG"): layers left to
// right, edges from a task's right edge to its dependant's left edge. Hover
// keeps what a task builds on and what waits for it lit and dims the rest;
// click opens its card; the keyboard walks along a layer (`j`/`k`, up and
// down) and across layers (`h`/`l`, left and right). A task that changes
// state flashes its outline once.

import { For, Show, createEffect, createMemo, createSignal, on } from "solid-js";
import { useApp } from "../app";
import { MARK_SWEEP_MS } from "../anim/anims";
import { now as clock } from "../clock";
import { runtimeGlyph, taskIcon, taskTone } from "../format";
import type { Task } from "../protocol";
import { layered, lineage } from "./layers";
import { union } from "./view";
import { Viewport } from "./Viewport";

export function TaskDag() {
  const app = useApp();
  const { store, ui } = app;
  let host!: HTMLDivElement;
  const layout = createMemo(() => layered(store.state.tasks.map((t) => ({ id: t.id, dependencies: [...t.dependencies] }))));
  const bounds = createMemo(() => union([...layout().boxes.values()]));
  const [hover, setHover] = createSignal<string | null>(null);
  const lit = createMemo(() => (hover() ? lineage(store.state.tasks, hover()!) : null));
  const selectedTask = () => (ui.sel()?.kind === "task" ? (ui.sel() as { id: string }).id : null);
  const [cursor, setCursor] = createSignal<string | null>(null);
  const tabStop = () => {
    const ids = layout().layers.flat();
    const c = cursor() ?? selectedTask();
    return c && ids.includes(c) ? c : ids[0] ?? null;
  };

  // A state change flashes the task's outline once.
  const [flashes, setFlashes] = createSignal<ReadonlyMap<string, number>>(new Map());
  const last = new Map<string, string>();
  createEffect(on(() => store.state.tasks.map((t) => `${t.id}:${t.status}`).join(","), () => {
    const t0 = clock();
    const next = new Map(flashes());
    for (const t of store.state.tasks) {
      const was = last.get(t.id);
      if (was !== undefined && was !== t.status) next.set(t.id, t0);
      last.set(t.id, t.status);
    }
    if (next.size !== flashes().size || [...next].some(([k, v]) => flashes().get(k) !== v)) {
      setFlashes(next);
      window.setTimeout(() => setFlashes((m) => new Map([...m].filter(([, at]) => clock() - at < MARK_SWEEP_MS))), MARK_SWEEP_MS + 20);
    }
  }));

  const focusTask = (id: string) => (host.querySelector(`[data-task="${CSS.escape(id)}"]`) as HTMLElement | null)?.focus();
  const move = (id: string, dLayer: number, dRow: number) => {
    const ls = layout().layers;
    const l = ls.findIndex((x) => x.includes(id));
    if (l < 0) return;
    let target: string | undefined;
    if (dLayer !== 0) {
      const nl = ls[Math.min(ls.length - 1, Math.max(0, l + dLayer))];
      const row = ls[l].indexOf(id);
      target = nl[Math.min(row, nl.length - 1)];
    } else {
      const row = ls[l].indexOf(id) + dRow;
      target = ls[l][Math.min(ls[l].length - 1, Math.max(0, row))];
    }
    if (target) {
      setCursor(target);
      queueMicrotask(() => focusTask(target!));
    }
  };
  const onKey = (e: KeyboardEvent, id: string) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    const k = e.key;
    const map: Record<string, [number, number]> = { ArrowDown: [0, 1], j: [0, 1], ArrowUp: [0, -1], k: [0, -1], ArrowLeft: [-1, 0], h: [-1, 0], ArrowRight: [1, 0], l: [1, 0] };
    if (map[k]) {
      e.preventDefault();
      e.stopPropagation();
      move(id, map[k][0], map[k][1]);
    } else if (k === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      ui.select({ kind: "task", id });
    }
  };

  const edgePath = (from: string, to: string) => {
    const a = layout().boxes.get(from)!, b = layout().boxes.get(to)!;
    const x0 = a.x + a.w, y0 = a.y + a.h / 2, x1 = b.x, y1 = b.y + b.h / 2;
    const dx = Math.max(40, (x1 - x0) / 2);
    return `M${x0} ${y0}C${x0 + dx} ${y0} ${x1 - dx} ${y1} ${x1} ${y1}`;
  };

  return (
    <div class="dag-host" ref={host}>
      <Show when={store.state.tasks.length > 0} fallback={<div class="empty">no tasks yet</div>}>
        <Viewport label="task graph" class="task-dag" bounds={bounds} api={(a) => (app.views.tasks = a)} onBackground={() => ui.sel()?.kind === "task" && ui.select(null)}>
          <g class="edges">
            <For each={layout().edges}>
              {(e) => <path class="edge" classList={{ dim: !!lit() && !(lit()!.has(e.from) && lit()!.has(e.to)), lit: !!lit() && lit()!.has(e.from) && lit()!.has(e.to) }} d={edgePath(e.from, e.to)} />}
            </For>
          </g>
          <For each={store.state.tasks}>
            {(task: Task) => {
              const b = () => layout().boxes.get(task.id);
              const assignee = () => (task.assigned_agent ? store.agent(task.assigned_agent) : undefined);
              return (
                <Show when={b()}>
                  <g class={`task tone-${taskTone(task.status)} ts-${task.status}`} data-task={task.id} role="button" aria-label={`${task.id}, ${task.status}`}
                    tabindex={tabStop() === task.id ? 0 : -1}
                    classList={{ dim: !!lit() && !lit()!.has(task.id), selected: selectedTask() === task.id }}
                    transform={`translate(${b()!.x} ${b()!.y})`}
                    onPointerDown={(e) => e.stopPropagation()}
                    onClick={() => {
                      setCursor(task.id);
                      ui.select({ kind: "task", id: task.id });
                    }}
                    onPointerEnter={() => setHover(task.id)} onPointerLeave={() => setHover(null)}
                    onFocus={() => setCursor(task.id)}
                    onKeyDown={(e) => onKey(e, task.id)}>
                    <rect class="box" width={b()!.w} height={b()!.h} rx="7" />
                    <Show when={flashes().has(task.id)}>
                      <rect class="flash" width={b()!.w} height={b()!.h} rx="7" pathLength="1" />
                    </Show>
                    <foreignObject width={b()!.w} height={b()!.h}>
                      <div class="task-body">
                        <div class="task-head">
                          <span class={`icon ${taskTone(task.status)}`} classList={{ spin: task.status === "working" }}>{taskIcon(task.status)}</span>
                          <span class="tid">{task.id}</span>
                          <span class={`tstatus ${taskTone(task.status)}`}>{task.status}</span>
                        </div>
                        <div class="ttitle">{task.title}</div>
                        <div class="task-foot">
                          <Show when={task.assigned_agent}>
                            <button class={`assignee rt-${assignee()?.runtime ?? "none"}`} tabindex="-1"
                              onClick={(e) => {
                                e.stopPropagation();
                                ui.setSelected(task.assigned_agent!);
                                ui.setRoute({ view: "graph", sel: { kind: "agent", id: task.assigned_agent! } });
                              }}>{assignee() ? runtimeGlyph(assignee()!.runtime) : "·"} {task.assigned_agent}</button>
                          </Show>
                          <Show when={task.status === "review" && task.reviewer}><span class="orch">review: {task.reviewer}</span></Show>
                          <Show when={task.timeout_secs}><span class="muted">timeout</span></Show>
                          <Show when={task.progress != null && task.status !== "done"}>
                            <span class="bar"><span class="fill" style={{ width: `${task.progress}%` }} /></span>
                          </Show>
                        </div>
                      </div>
                    </foreignObject>
                  </g>
                </Show>
              );
            }}
          </For>
        </Viewport>
      </Show>
    </div>
  );
}
