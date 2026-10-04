import { For, Show, createMemo, createSignal, onCleanup, createEffect } from "solid-js";
import { state, pulses } from "./store";

const PULSE_MS = 900, LINK_MS = 3000, W = 150, H = 56;
type P = { x: number; y: number };

// Hub in the middle, workers on an ellipse, first one top-left, clockwise:
// the TUI's layout::radial.
function radial(ids: string[], hub: string | undefined, w: number, h: number): Map<string, P> {
  const out = new Map<string, P>();
  const cx = w / 2, cy = h / 2;
  if (hub) out.set(hub, { x: cx, y: cy });
  const ring = ids.filter((id) => id !== hub);
  const n = ring.length;
  ring.forEach((id, i) => {
    const a = ((n === 1 ? 0 : -180 / n + (i * 360) / n) - 90) * (Math.PI / 180);
    out.set(id, { x: cx + Math.cos(a) * (w / 2 - W), y: cy + Math.sin(a) * (h / 2 - H) });
  });
  return out;
}

export function Graph(props: { onOpen(id: string): void }) {
  const [view, setView] = createSignal({ x: 0, y: 0, k: 1 });
  const [moved, setMoved] = createSignal<Record<string, P>>({});
  const [now, setNow] = createSignal(performance.now());
  const [hover, setHover] = createSignal<string | null>(null);
  let svg!: SVGSVGElement;
  const size = { w: 1000, h: 600 };

  const hub = createMemo(() => state.agents.find((a) => a.orchestrator)?.id);
  const base = createMemo(() => radial(state.agents.map((a) => a.id), hub(), size.w, size.h));
  const pos = (id: string): P => moved()[id] ?? base().get(id) ?? { x: size.w / 2, y: size.h / 2 };

  // A frame loop only while something moves.
  let raf = 0;
  const live = () => pulses().some((p) => performance.now() - p.started < LINK_MS);
  const frame = () => { setNow(performance.now()); raf = live() ? requestAnimationFrame(frame) : 0; };
  createEffect(() => { pulses(); if (!raf && live()) raf = requestAnimationFrame(frame); });
  onCleanup(() => cancelAnimationFrame(raf));

  const toWorld = (e: PointerEvent | WheelEvent): P => {
    const r = svg.getBoundingClientRect(), v = view();
    return { x: ((e.clientX - r.left) * (size.w / r.width) - v.x) / v.k, y: ((e.clientY - r.top) * (size.h / r.height) - v.y) / v.k };
  };
  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    const v = view(), p = toWorld(e), k = Math.min(4, Math.max(0.25, v.k * Math.exp(-e.deltaY * 0.0015)));
    setView({ k, x: v.x + p.x * (v.k - k), y: v.y + p.y * (v.k - k) });
  };
  let drag: { id?: string; sx: number; sy: number; ox: number; oy: number; moved: boolean } | null = null;
  const down = (e: PointerEvent, id?: string) => {
    e.stopPropagation(); svg.setPointerCapture(e.pointerId);
    const o = id ? pos(id) : view();
    drag = { id, sx: e.clientX, sy: e.clientY, ox: o.x, oy: o.y, moved: false };
  };
  const move = (e: PointerEvent) => {
    if (!drag) return;
    const r = svg.getBoundingClientRect(), s = size.w / r.width;
    const dx = (e.clientX - drag.sx) * s, dy = (e.clientY - drag.sy) * s;
    if (Math.abs(dx) + Math.abs(dy) > 3) drag.moved = true;
    if (drag.id) setMoved({ ...moved(), [drag.id]: { x: drag.ox + dx / view().k, y: drag.oy + dy / view().k } });
    else setView({ ...view(), x: drag.ox + dx, y: drag.oy + dy });
  };
  const up = (e: PointerEvent) => { const d = drag; drag = null; if (d?.id && !d.moved) props.onOpen(d.id); svg.releasePointerCapture(e.pointerId); };
  const ease = (t: number) => (t < 0.5 ? 2 * t * t : 1 - (-2 * t + 2) ** 2 / 2);

  return (
    <svg ref={svg} class="graph" viewBox={`0 0 ${size.w} ${size.h}`} onWheel={onWheel} onPointerDown={(e) => down(e)} onPointerMove={move} onPointerUp={up}>
      <g transform={`translate(${view().x} ${view().y}) scale(${view().k})`}>
        <For each={state.agents.filter((a) => a.id !== hub())}>
          {(a) => <line class="spoke" x1={pos(hub() ?? a.id).x} y1={pos(hub() ?? a.id).y} x2={pos(a.id).x} y2={pos(a.id).y} />}
        </For>
        <For each={pulses()}>
          {(p) => {
            const t = () => (now() - p.started) / PULSE_MS;
            const a = () => pos(p.from), b = () => pos(p.to);
            return (
              <Show when={now() - p.started < LINK_MS}>
                <line class="link" x1={a().x} y1={a().y} x2={b().x} y2={b().y} />
                <Show when={t() < 1}>
                  <circle class="pulse" r="5" cx={a().x + (b().x - a().x) * ease(t())} cy={a().y + (b().y - a().y) * ease(t())} />
                </Show>
              </Show>
            );
          }}
        </For>
        <For each={state.agents}>
          {(a) => (
            <g class="node" classList={{ hub: !!a.orchestrator, hover: hover() === a.id, [a.status]: true }} transform={`translate(${pos(a.id).x - W / 2} ${pos(a.id).y - H / 2})`}
              tabindex="0" role="button" aria-label={`${a.id}, ${a.status}`}
              onPointerDown={(e) => down(e, a.id)} onPointerEnter={() => setHover(a.id)} onPointerLeave={() => setHover(null)}
              onKeyDown={(e) => e.key === "Enter" && props.onOpen(a.id)}>
              <rect width={W} height={H} rx="6" />
              <text x="10" y="20" class="id">{a.id}</text>
              <text x={W - 10} y="20" class="status" text-anchor="end">{a.status}{a.progress != null ? ` ${a.progress}%` : ""}</text>
              <text x="10" y="40" class="activity">{(a.activity || a.task || a.role).slice(0, 22)}</text>
            </g>
          )}
        </For>
      </g>
    </svg>
  );
}
