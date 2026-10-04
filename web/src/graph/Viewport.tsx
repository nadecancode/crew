// Pan and zoom, shared by the agent graph and the task DAG: one transform on
// one `<g>`, so a drag changes one attribute (§perf 6).
//
// wheel: zoom about the pointer; ctrl-wheel (trackpad pinch) the same; two
// pointers: pinch; drag on the background: pan; double click on the
// background or `0`: fit; `+`/`-`: zoom about the centre; arrow keys while
// the viewport itself has focus: pan.

import { createEffect, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { CLICK_SLOP, fit, pan, toWorld, zoomAt, type Box, type Pt, type Size, type View } from "./view";

export type ViewportApi = {
  view: () => View;
  size: () => Size;
  /** Screen (client) coordinates to world coordinates. */
  worldAt(clientX: number, clientY: number): Pt;
  fit(): void;
  zoom(factor: number): void;
  panBy(dx: number, dy: number): void;
  /** The operator moved the view: it is no longer refitted on its own. */
  moved: () => boolean;
  /** Stop following the content (a node is being dragged). */
  hold(): void;
};

export function Viewport(props: {
  bounds: () => Box;
  label: string;
  class?: string;
  /** The background was clicked (not dragged). */
  onBackground?(): void;
  /** The API, for the key table and the parent. */
  api?(api: ViewportApi): void;
  children: JSX.Element;
}) {
  let svg!: SVGSVGElement;
  const [view, setView] = createSignal<View>({ x: 0, y: 0, k: 1 });
  const [size, setSize] = createSignal<Size>({ w: 0, h: 0 });
  const [moved, setMoved] = createSignal(false);

  const local = (clientX: number, clientY: number): Pt => {
    const r = svg.getBoundingClientRect();
    return { x: clientX - r.left, y: clientY - r.top };
  };
  const doFit = () => setView(fit(props.bounds(), size()));
  const api: ViewportApi = {
    view,
    size,
    worldAt: (cx, cy) => toWorld(view(), local(cx, cy)),
    fit() {
      setMoved(false);
      doFit();
    },
    zoom(factor) {
      setMoved(true);
      setView(zoomAt(view(), { x: size().w / 2, y: size().h / 2 }, factor));
    },
    panBy(dx, dy) {
      setMoved(true);
      setView(pan(view(), dx, dy));
    },
    moved,
    hold: () => setMoved(true),
  };
  props.api?.(api);

  onMount(() => {
    const ro = new ResizeObserver(() => {
      const r = svg.getBoundingClientRect();
      setSize({ w: r.width, h: r.height });
    });
    ro.observe(svg);
    onCleanup(() => ro.disconnect());
  });
  // Until the operator moves it, the view follows the content and the window.
  createEffect(() => {
    const b = props.bounds();
    const s = size();
    if (!moved() && s.w > 0 && b.w > 0) setView(fit(b, s));
  });

  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    const scale = e.ctrlKey ? 0.01 : 0.0015;
    const delta = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY;
    setMoved(true);
    setView(zoomAt(view(), local(e.clientX, e.clientY), Math.exp(-delta * scale)));
  };

  // Pointers on the background: one pans, two pinch.
  const pointers = new Map<number, Pt>();
  let start: { at: Pt; view: View; dragged: boolean } | null = null;
  let pinch: { dist: number; view: View; mid: Pt } | null = null;
  const onDown = (e: PointerEvent) => {
    if (e.button !== 0 && e.pointerType === "mouse") return;
    svg.setPointerCapture(e.pointerId);
    pointers.set(e.pointerId, local(e.clientX, e.clientY));
    if (pointers.size === 1) start = { at: local(e.clientX, e.clientY), view: view(), dragged: false };
    else if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      pinch = { dist: Math.hypot(a.x - b.x, a.y - b.y) || 1, view: view(), mid: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 } };
      start = null;
    }
  };
  const onMove = (e: PointerEvent) => {
    if (!pointers.has(e.pointerId)) return;
    const p = local(e.clientX, e.clientY);
    pointers.set(e.pointerId, p);
    if (pinch && pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      const dist = Math.hypot(a.x - b.x, a.y - b.y);
      setMoved(true);
      setView(zoomAt(pinch.view, pinch.mid, dist / pinch.dist));
    } else if (start) {
      const dx = p.x - start.at.x, dy = p.y - start.at.y;
      if (!start.dragged && Math.abs(dx) + Math.abs(dy) <= CLICK_SLOP) return;
      start.dragged = true;
      setMoved(true);
      setView(pan(start.view, dx, dy));
    }
  };
  const onUp = (e: PointerEvent) => {
    if (!pointers.delete(e.pointerId)) return;
    if (svg.hasPointerCapture(e.pointerId)) svg.releasePointerCapture(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (pointers.size === 0) {
      if (start && !start.dragged && e.type === "pointerup") props.onBackground?.();
      start = null;
    }
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.target !== svg) return;
    const step = 40;
    const moves: Record<string, [number, number]> = { ArrowLeft: [step, 0], ArrowRight: [-step, 0], ArrowUp: [0, step], ArrowDown: [0, -step] };
    const m = moves[e.key];
    if (m) {
      e.preventDefault();
      api.panBy(m[0], m[1]);
    }
  };

  return (
    <svg
      ref={svg}
      class={`viewport ${props.class ?? ""}`}
      role="application"
      aria-label={props.label}
      tabindex="-1"
      onWheel={onWheel}
      onPointerDown={onDown}
      onPointerMove={onMove}
      onPointerUp={onUp}
      onPointerCancel={onUp}
      onDblClick={(e) => {
        if (e.target === svg) api.fit();
      }}
      onKeyDown={onKey}
    >
      <g transform={`translate(${view().x} ${view().y}) scale(${view().k})`}>{props.children}</g>
    </svg>
  );
}
