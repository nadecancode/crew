// The agent graph (notes/web-design.md §graphs "Agent graph"): the TUI's
// topology as SVG, with what a browser adds: drag, pan, zoom, hover and
// click-through on every node, link and mark, a context menu, and the
// keyboard (one tab stop, arrows inside).

import { For, Show, createMemo, createSignal, onCleanup, type JSX } from "solid-js";
import { useApp } from "../app";
import { linkLive, pulse, sweep, markLive, type Link, type Mark } from "../anim/anims";
import { ORCHESTRATOR_ID, askedBy, type Agent } from "../protocol";
import { isOrchestrator } from "../store/order";
import { EmptyState, ViewHeader } from "../components";
import { AgentNode, HubNode } from "./AgentNode";
import { Hover, type HoverTarget } from "./Hover";
import { radial } from "./radial";
import { CLICK_SLOP, center, edgePoint, union, type Box, type Pt } from "./view";
import { Viewport, type ViewportApi } from "./Viewport";

const reducedMotion = () => typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Where a link runs: box edge to box edge; between two members it bends
 * away from the hub, so it is never mistaken for two spokes. */
export function linkGeometry(a: Box, b: Box, hub: Pt, curved: boolean): { p0: Pt; c: Pt; p1: Pt } {
  const p0 = edgePoint(a, center(b));
  const p1 = edgePoint(b, center(a));
  const mid = { x: (p0.x + p1.x) / 2, y: (p0.y + p1.y) / 2 };
  if (!curved) return { p0, c: mid, p1 };
  let ox = mid.x - hub.x, oy = mid.y - hub.y;
  const len = Math.hypot(ox, oy);
  if (len < 1) [ox, oy] = [-(p1.y - p0.y), p1.x - p0.x];
  const n = Math.hypot(ox, oy) || 1;
  const push = Math.max(50, Math.hypot(p1.x - p0.x, p1.y - p0.y) * 0.22);
  return { p0, c: { x: mid.x + (ox / n) * push, y: mid.y + (oy / n) * push }, p1 };
}

export const along = (g: { p0: Pt; c: Pt; p1: Pt }, t: number): Pt => {
  const u = 1 - t;
  return { x: u * u * g.p0.x + 2 * u * t * g.c.x + t * t * g.p1.x, y: u * u * g.p0.y + 2 * u * t * g.c.y + t * t * g.p1.y };
};

const pathOf = (g: { p0: Pt; c: Pt; p1: Pt }) => `M${g.p0.x.toFixed(1)} ${g.p0.y.toFixed(1)}Q${g.c.x.toFixed(1)} ${g.c.y.toFixed(1)} ${g.p1.x.toFixed(1)} ${g.p1.y.toFixed(1)}`;

export function AgentGraph() {
  const app = useApp();
  const { store, ui, prefs, frames } = app;
  const reduced = reducedMotion();
  let api: ViewportApi | undefined;
  let host!: HTMLDivElement;

  const layout = createMemo(() => radial(store.state.agents.map((a) => ({ id: a.id, orchestrator: a.orchestrator, parent: a.parent }))));
  const [drag, setDrag] = createSignal<{ id: string; at: Pt } | null>(null);

  /** Where a box is: being dragged, where it was dragged to, or where the layout puts it. */
  const box = (id: string): Box | undefined => {
    const base = layout().boxes.get(id);
    if (!base) return undefined;
    const key = layout().boxes.get(id) === layout().boxes.get(ORCHESTRATOR_ID) ? ORCHESTRATOR_ID : id;
    const d = drag();
    const c = d && d.id === key ? d.at : prefs.prefs.layout[key];
    return c ? { x: c.x - base.w / 2, y: c.y - base.h / 2, w: base.w, h: base.h } : base;
  };
  const hubBox = () => box(ORCHESTRATOR_ID)!;
  const bounds = createMemo(() => union([...layout().boxes.keys()].map((id) => box(id)).filter((b): b is Box => !!b).map((b) => ({ ...b, h: b.h + 32 }))));

  const hubAgent = () => store.state.agents.find(isOrchestrator);
  const workers = () => store.state.agents.filter((a) => !isOrchestrator(a));
  const nodes = () => store.state.agents;

  // ---- selection and keys ----------------------------------------------------

  /** The one node in the tab order: the selected agent, else the first. */
  const tabStop = createMemo(() => {
    const sel = ui.selected();
    const ids = nodes().map((a) => a.id);
    return sel && ids.includes(sel) ? sel : ids[0] ?? null;
  });
  const focusNode = (id: string) => (host.querySelector(`[data-agent="${CSS.escape(id)}"]`) as HTMLElement | null)?.focus();
  const step = (from: string, by: number) => {
    const ids = nodes().map((a) => a.id);
    if (ids.length === 0) return;
    const i = ids.indexOf(from);
    const next = ids[(((i < 0 ? 0 : i + by) % ids.length) + ids.length) % ids.length];
    ui.setSelected(next);
    queueMicrotask(() => focusNode(next));
  };
  const open = (id: string, tab: "overview" | "terminal" = "overview") => {
    setHover(null);
    ui.select({ kind: "agent", id });
    ui.setTab(tab);
  };
  const onNodeKey = (e: KeyboardEvent, id: string) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    switch (e.key) {
      case "ArrowRight":
      case "ArrowDown":
        e.preventDefault();
        step(id, 1);
        break;
      case "ArrowLeft":
      case "ArrowUp":
        e.preventDefault();
        step(id, -1);
        break;
      case "Home":
        e.preventDefault();
        step(nodes()[0]?.id ?? id, 0);
        break;
      case "End":
        e.preventDefault();
        step(nodes()[nodes().length - 1]?.id ?? id, 0);
        break;
      case "Enter":
      case " ":
        e.preventDefault();
        e.stopPropagation();
        open(id);
        queueMicrotask(() => (document.querySelector("#side-panel [data-focus-first]") as HTMLElement | null)?.focus());
        break;
      case "F10":
      case "ContextMenu":
        if (e.shiftKey || e.key === "ContextMenu") {
          e.preventDefault();
          e.stopPropagation();
          const r = (e.currentTarget as Element).getBoundingClientRect();
          ui.setMenu({ agent: id, x: r.left, y: r.bottom });
        }
        break;
    }
  };

  // ---- dragging a node -------------------------------------------------------

  let pressed: { id: string; key: string; pointer: number; sx: number; sy: number; offset: Pt; moved: boolean } | null = null;
  const onNodeDown = (e: PointerEvent, id: string) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    const b = box(id);
    if (!b || !api) return;
    const w = api.worldAt(e.clientX, e.clientY);
    const key = layout().boxes.get(id) === layout().boxes.get(ORCHESTRATOR_ID) ? ORCHESTRATOR_ID : id;
    pressed = { id, key, pointer: e.pointerId, sx: e.clientX, sy: e.clientY, offset: { x: center(b).x - w.x, y: center(b).y - w.y }, moved: false };
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
  };
  const onNodeMove = (e: PointerEvent) => {
    if (!pressed || e.pointerId !== pressed.pointer || !api) return;
    if (!pressed.moved && Math.abs(e.clientX - pressed.sx) + Math.abs(e.clientY - pressed.sy) <= CLICK_SLOP) return;
    if (!pressed.moved) api.hold();
    pressed.moved = true;
    setHover(null);
    const w = api.worldAt(e.clientX, e.clientY);
    setDrag({ id: pressed.key, at: { x: w.x + pressed.offset.x, y: w.y + pressed.offset.y } });
  };
  const onNodeUp = (e: PointerEvent) => {
    const p = pressed;
    if (!p || e.pointerId !== p.pointer) return;
    pressed = null;
    const d = drag();
    if (p.moved && d && e.type !== "pointercancel") prefs.setPosition(d.id, d.at);
    setDrag(null);
    if (!p.moved && e.type !== "pointercancel") {
      ui.setSelected(p.id);
      open(p.id);
    }
  };

  // ---- hover -----------------------------------------------------------------

  const [hover, setHoverSignal] = createSignal<{ target: HoverTarget; x: number; y: number } | null>(null);
  const setHover = (h: { target: HoverTarget; x: number; y: number } | null) => setHoverSignal(h);
  const hoverAt = (target: HoverTarget) => (e: PointerEvent | MouseEvent) => {
    if (pressed?.moved) return;
    const r = host.getBoundingClientRect();
    setHover({ target, x: e.clientX - r.left, y: e.clientY - r.top });
  };
  const unhover = () => setHover(null);

  // ---- links ---------------------------------------------------------------------

  const links = createMemo(() => {
    store.animsVersion();
    return store.anims.links;
  });
  const asking = (id: string) => store.openQuestions().some((q) => (askedBy(q) ?? q.from) === id || (id === ORCHESTRATOR_ID && q.from === ORCHESTRATOR_ID && !askedBy(q)));
  const questionOf = (id: string) => store.openQuestions().find((q) => (askedBy(q) ?? q.from) === id);

  const LinkView = (props: { link: Link }) => {
    const l = props.link;
    const a = () => box(l.from), b = () => box(l.to);
    const hubEnd = l.from === ORCHESTRATOR_ID || l.to === ORCHESTRATOR_ID;
    const geo = createMemo(() => {
      const [ba, bb] = [a(), b()];
      return ba && bb ? linkGeometry(ba, bb, center(hubBox()), !hubEnd) : null;
    });
    const live = () => linkLive(l, frames.now());
    const t = () => (reduced ? null : pulse(l, frames.now()));
    const tone = l.kind === "vcs" ? (l.held !== null ? "held" : `tone-${l.tone}`) : `kind-${l.kind}`;
    const click = (e: MouseEvent) => {
      e.stopPropagation();
      if (l.kind === "vcs" && l.event?.seq !== undefined) {
        ui.setLogFilter({ ...ui.logFilter(), vcs: true });
        ui.setRoute({ view: "logs", sel: { kind: "event", seq: l.event.seq } });
      } else if (l.kind !== "vcs") {
        ui.setBusPair([l.from, l.to]);
        ui.setBusOpen(true);
        if (ui.sel()?.kind === "agent") ui.select(null);
      }
    };
    return (
      <Show when={geo() && live()}>
        <g class={`link ${tone}`} data-link={`${l.from}:${l.to}`} onPointerDown={(e) => e.stopPropagation()} onClick={click}
          onPointerEnter={hoverAt(l.kind === "vcs" ? { kind: "vcs", event: l.event! } : { kind: "pair", a: l.from, b: l.to })} onPointerLeave={unhover}>
          <path class="link-hit" d={pathOf(geo()!)} />
          <path class="link-line" d={pathOf(geo()!)} />
          <Show when={t() !== null}>
            {(() => {
              const at = (dt: number) => along(geo()!, Math.max(0, t()! - dt));
              return l.kind === "vcs" ? (
                <g class="pulse vcs">
                  <rect class="tail" x={at(0.07).x - 3} y={at(0.07).y - 3} width="6" height="6" transform={`rotate(45 ${at(0.07).x} ${at(0.07).y})`} />
                  <rect x={at(0).x - 5} y={at(0).y - 5} width="10" height="10" transform={`rotate(45 ${at(0).x} ${at(0).y})`} />
                </g>
              ) : (
                <g class="pulse msg" onClick={(e) => {
                  e.stopPropagation();
                  if (l.message) ui.select({ kind: "message", id: l.message });
                }} onPointerEnter={hoverAt({ kind: "message", id: l.message ?? "" })}>
                  <circle class="tail" cx={at(0.08).x} cy={at(0.08).y} r="2.5" />
                  <circle class="tail" cx={at(0.04).x} cy={at(0.04).y} r="3.5" />
                  <circle cx={at(0).x} cy={at(0).y} r="5" />
                </g>
              );
            })()}
          </Show>
        </g>
      </Show>
    );
  };

  // ---- marks -----------------------------------------------------------------------

  const markOf = (id: string) => {
    store.animsVersion();
    return store.anims.markFor(id, frames.now());
  };

  const MarkView = (props: { id: string }): JSX.Element => {
    const mark = createMemo(() => markOf(props.id));
    const b = () => box(props.id);
    return (
      <Show when={mark() && b() && (mark()!.kind === "running" || markLive(mark()!, frames.now()))}>
        {(() => {
          const m = () => mark() as Mark;
          const s = () => (reduced ? null : sweep(m(), frames.now()));
          const toneClass = () => (m().kind === "glance" ? (m().label.startsWith("!") ? "tone-warn" : "tone-glance") : `tone-${m().tone}`);
          const click = (e: MouseEvent) => {
            e.stopPropagation();
            const ev = m().event;
            if (ev?.commit) void navigator.clipboard?.writeText(ev.commit).then(() => ui.setStatus(`copied ${ev.commit!.slice(0, 12)}`), () => {});
            if (ev?.seq !== undefined) {
              ui.setLogFilter({ ...ui.logFilter(), vcs: true });
              ui.setRoute({ view: "logs", sel: { kind: "event", seq: ev.seq } });
            }
          };
          return (
            <g class={`mark ${m().kind} ${toneClass()}`}>
              <Show when={s() !== null && m().kind === "done"}>
                <rect class="sweep" x={b()!.x} y={b()!.y} width={b()!.w} height={b()!.h} rx="7" pathLength="1" stroke-dasharray="0.16 0.84" stroke-dashoffset={-s()!} />
              </Show>
              <Show when={s() !== null && m().kind === "glance"}>
                <line class="sweep" x1={b()!.x + 6} y1={b()!.y + b()!.h} x2={b()!.x + b()!.w - 6} y2={b()!.y + b()!.h} pathLength="1" stroke-dasharray="0.22 1" stroke-dashoffset={0.22 - s()! * 1.22} />
              </Show>
              <foreignObject x={b()!.x} y={b()!.y + b()!.h + 4} width={b()!.w} height="24" class="chip-host">
                <div class="chip-row">
                  <span class="chip" data-mark={props.id} role="button" tabindex="-1" onClick={click} onPointerDown={(e) => e.stopPropagation()}
                    onPointerEnter={hoverAt({ kind: "vcs", event: m().event! })} onPointerLeave={unhover}>
                    <Show when={m().kind === "running"}><span class="spin">◐</span> </Show>
                    {m().label}
                  </span>
                </div>
              </foreignObject>
            </g>
          );
        })()}
      </Show>
    );
  };

  // ---- structure -------------------------------------------------------------------

  const ringIds = () => layout().ring;
  const sats = () => [...layout().satellites.entries()];
  const p2pLive = () => {
    store.animsVersion();
    return store.anims.p2pLive(frames.now());
  };
  const hoverLine = createMemo(() => {
    const pair = ui.hoverPair();
    if (!pair) return null;
    const [a, b] = [box(pair[0] === "operator" ? ORCHESTRATOR_ID : pair[0]), box(pair[1] === "operator" ? ORCHESTRATOR_ID : pair[1])];
    if (!a || !b || a === b) return null;
    return linkGeometry(a, b, center(hubBox()), a !== hubBox() && b !== hubBox());
  });

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") setHover(null);
  };
  document.addEventListener("keydown", onKey);
  onCleanup(() => document.removeEventListener("keydown", onKey));

  return (
    <div class="graph-host" ref={host} classList={{ dragging: !!drag() }}>
      <ViewHeader title="Team" meta={`${nodes().length} agents`} />
      <Show when={nodes().length} fallback={<EmptyState title="No agents yet" description="Agents appear here when the team starts." />}>
      <div class="graph-canvas">
      <Show when={p2pLive()}>
        <div class="p2p-banner" role="status">p2p channel OPEN · agents talking directly</div>
      </Show>
      <Viewport label="agent graph" class="agent-graph" bounds={bounds} api={(a) => ((api = a), (app.views.graph = a))}
        onDispose={() => { app.views.graph = undefined; api = undefined; }}
        onBackground={() => {
          ui.setSelected(null);
          ui.setBusPair(null);
          if (ui.sel()) ui.select(null);
        }}>
        <g class="spokes">
          <For each={ringIds()}>
            {(id) => {
              const g = () => {
                const [a, b] = [hubBox(), box(id)];
                return a && b ? linkGeometry(a, b, center(a), false) : null;
              };
              return <Show when={g()}><path class="spoke" d={pathOf(g()!)} /></Show>;
            }}
          </For>
          <For each={sats()}>
            {([kid, parent]) => {
              const g = () => {
                const [a, b] = [box(parent), box(kid)];
                return a && b ? linkGeometry(a, b, center(hubBox()), false) : null;
              };
              return <Show when={g()}><path class="tether" d={pathOf(g()!)} /></Show>;
            }}
          </For>
        </g>
        <Show when={hoverLine()}>
          <path class="hover-link" d={pathOf(hoverLine()!)} />
        </Show>
        <g class="links">
          <For each={links()}>{(l) => <LinkView link={l} />}</For>
        </g>
        <g class="nodes">
          <HubNode box={hubBox()} agent={hubAgent()} asking={asking(ORCHESTRATOR_ID) || (!!hubAgent() && asking(hubAgent()!.id))}
            tabStop={hubAgent() ? tabStop() === hubAgent()!.id : false}
            selected={!!hubAgent() && ui.selected() === hubAgent()!.id}
            onDown={(e) => hubAgent() && onNodeDown(e, hubAgent()!.id)} onMove={onNodeMove} onUp={onNodeUp}
            onKey={(e) => hubAgent() && onNodeKey(e, hubAgent()!.id)}
            onFocus={() => hubAgent() && ui.setSelected(hubAgent()!.id)}
            onHover={(e) => hubAgent() && hoverAt({ kind: "agent", id: hubAgent()!.id })(e)} onLeave={unhover}
            onQuestion={() => {
              const q = questionOf(hubAgent()?.id ?? ORCHESTRATOR_ID) ?? store.openQuestions()[0];
              if (q) ui.openDialog({ kind: "question", id: q.id });
            }}
            onModel={() => hubAgent() && ui.openDialog({ kind: "model", agent: hubAgent()!.id })}
            onMenu={(e) => hubAgent() && openMenu(e, hubAgent()!.id)}
            onOpen={() => hubAgent() && open(hubAgent()!.id, "terminal")} />
          <For each={workers()}>
            {(agent: Agent) => (
              <Show when={box(agent.id)}>
                <AgentNode agent={agent} box={box(agent.id)!} satellite={layout().satellites.has(agent.id)}
                  selected={ui.selected() === agent.id} tabStop={tabStop() === agent.id} asking={asking(agent.id)}
                  onDown={(e) => onNodeDown(e, agent.id)} onMove={onNodeMove} onUp={onNodeUp}
                  onKey={(e) => onNodeKey(e, agent.id)}
                  onFocus={() => ui.setSelected(agent.id)}
                  onHover={hoverAt({ kind: "agent", id: agent.id })} onLeave={unhover}
                  onTask={(task, e) => {
                    e.stopPropagation();
                    ui.select({ kind: "task", id: task });
                  }}
                  onTaskHover={(task) => hoverAt({ kind: "task", id: task })}
                  onQuestion={() => {
                    const q = questionOf(agent.id);
                    if (q) ui.openDialog({ kind: "question", id: q.id });
                  }}
                  onModel={() => ui.openDialog({ kind: "model", agent: agent.id })}
                  onMenu={(e) => openMenu(e, agent.id)}
                  onOpen={() => open(agent.id, "terminal")} />
              </Show>
            )}
          </For>
        </g>
        <g class="marks">
          <MarkView id={ORCHESTRATOR_ID} />
          <For each={workers()}>{(agent) => <MarkView id={agent.id} />}</For>
        </g>
      </Viewport>
      <div class="graph-legend" aria-label="Connection legend"><span class="legend-orchestrator">Orchestrator</span><span class="legend-p2p">Direct message</span><span class="legend-vcs">Version control</span><span class="legend-tether">Subagent</span></div>
      </div>
      </Show>
      <Show when={hover()}>
        <Hover target={hover()!.target} x={hover()!.x} y={hover()!.y} />
      </Show>
    </div>
  );

  function openMenu(e: MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    setHover(null);
    ui.setSelected(id);
    const r = e.type === "contextmenu" ? { left: e.clientX, bottom: e.clientY } : (e.currentTarget as Element).getBoundingClientRect();
    ui.setMenu({ agent: id, x: r.left, y: r.bottom });
  }
}
