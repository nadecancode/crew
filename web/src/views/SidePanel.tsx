// The side panel (notes/web-design.md §ui "Layout"): the bus when nothing is
// selected; an agent's inspector; a task's card; a message; an event.
// Resizable, and it can take the whole main area (`f`). `esc` closes it and
// gives focus back to the node it was opened from (keys.ts).

import { Match, Show, Switch, createSignal, onCleanup, type JSX } from "solid-js";
import { useApp } from "../app";
import { Bus } from "./Bus";
import { EventPanel } from "./EventPanel";
import { Inspector } from "./Inspector";
import { MessagePanel } from "./MessagePanel";
import { TaskPanel } from "./TaskPanel";

const MIN = 300;

export function SidePanel() {
  const app = useApp();
  const { ui, prefs } = app;
  const [dragW, setDragW] = createSignal<number | null>(null);
  const width = () => dragW() ?? prefs.prefs.panel ?? 460;
  let start: { x: number; w: number; id: number } | null = null;
  const down = (e: PointerEvent) => {
    start = { x: e.clientX, w: width(), id: e.pointerId };
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
  };
  const move = (e: PointerEvent) => {
    if (!start || e.pointerId !== start.id) return;
    setDragW(Math.max(MIN, Math.min(window.innerWidth - 320, start.w + (start.x - e.clientX))));
  };
  const up = (e: PointerEvent) => {
    if (!start || e.pointerId !== start.id) return;
    start = null;
    const w = dragW();
    setDragW(null);
    if (w !== null) prefs.setPanel(Math.round(w));
  };
  const keyResize = (e: KeyboardEvent) => {
    const step = e.shiftKey ? 80 : 20;
    if (e.key === "ArrowLeft") (e.preventDefault(), prefs.setPanel(Math.min(window.innerWidth - 320, width() + step)));
    if (e.key === "ArrowRight") (e.preventDefault(), prefs.setPanel(Math.max(MIN, width() - step)));
  };
  onCleanup(() => (start = null));
  const sel = () => ui.sel();
  // What is shown, by identity: another agent, task, message or event is a
  // panel of its own, so nothing one fetched (or is still fetching) ever
  // shows under another's name.
  const shown = () => {
    const s = sel();
    return s ? `${s.kind}:${"id" in s ? s.id : s.seq}` : null;
  };
  return (
    <aside id="side-panel" class="side-panel" data-region="panel" aria-label="side panel" style={{ "--panel-w": `${width()}px` }} classList={{ open: !!sel() }}>
      <div class="resizer" role="separator" aria-orientation="vertical" aria-label="resize the side panel" tabindex="-1"
        onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={up} onKeyDown={keyResize} />
      <Show when={shown()} keyed fallback={<Bus />}>{(_key: string) => (
      <Switch>
        <Match when={sel()?.kind === "agent" && app.store.agent((sel() as { id: string }).id)}>
          {(agent) => <Inspector agent={agent()} />}
        </Match>
        <Match when={sel()?.kind === "agent"}>
          <Gone what={`agent ${(sel() as { id: string }).id} is not on the team`} />
        </Match>
        <Match when={sel()?.kind === "task"}>
          <TaskPanel id={(sel() as { id: string }).id} />
        </Match>
        <Match when={sel()?.kind === "message"}>
          <MessagePanel id={(sel() as { id: string }).id} />
        </Match>
        <Match when={sel()?.kind === "event"}>
          <EventPanel seq={(sel() as { seq: number }).seq} />
        </Match>
      </Switch>
      )}</Show>
    </aside>
  );
}

function Gone(props: { what: string }) {
  const { ui } = useApp();
  return (
    <div class="panel-body">
      <PanelHead title={props.what} />
      <Show when={true}><p class="muted pad">It may have left the team. <button onClick={() => ui.select(null)}>back to the bus</button></p></Show>
    </div>
  );
}

/** A panel's title row with its close button. */
export function PanelHead(props: { title: string; sub?: string; children?: JSX.Element }) {
  const { ui } = useApp();
  return (
    <div class="panel-head">
      <h2 class="panel-title" title={props.title}>{props.title}</h2>
      <Show when={props.sub}><span class="muted">{props.sub}</span></Show>
      <span class="spacer" />
      {props.children}
      <button class="close" aria-label="close the panel (esc)" title="close (esc)" onClick={() => {
        const s = ui.sel();
        ui.select(null);
        ui.setPanelFull(false);
        const id = s?.kind === "agent" ? s.id : ui.selected();
        if (id) queueMicrotask(() => document.querySelector<HTMLElement>(`[data-agent="${CSS.escape(id)}"]`)?.focus());
      }}>×</button>
    </div>
  );
}
