import { Show, createMemo } from "solid-js";
import { VirtualList } from "../views/VirtualList";
import type { EntrySummary } from "./Summary";

/** One bounded list implementation for work entities; identity stays an ID. */
export function EntityList<T extends { id: string; status: string }>(props: {
  kind: "agent" | "task";
  items: T[];
  selected: string | null;
  select(id: string): void;
  open(id: string): void;
  menu?(e: MouseEvent, id: string): void;
  summary(item: T): EntrySummary;
  satellite?(item: T): boolean;
}) {
  const index = () => Math.max(0, props.items.findIndex((item) => item.id === props.selected));
  const key = (e: KeyboardEvent) => {
    if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey) return;
    const id = props.items[index()]?.id;
    if (!id) return;
    if (props.menu && (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey))) {
      e.preventDefault(); e.stopPropagation();
      const target = e.currentTarget as HTMLElement, r = target.getBoundingClientRect();
      props.menu(new MouseEvent("contextmenu", { clientX: r.left, clientY: r.top, cancelable: true }), id);
    } else {
      const mapped = ({ ArrowLeft: "ArrowUp", ArrowRight: "ArrowDown", " ": "Enter", ...(props.kind === "task" ? { h: "ArrowUp", l: "ArrowDown" } : {}) } as Record<string, string>)[e.key];
      if (!mapped) return;
      e.preventDefault(); e.stopPropagation();
      (e.currentTarget as HTMLElement).querySelector(".vlist")?.dispatchEvent(new KeyboardEvent("keydown", { key: mapped, bubbles: true, cancelable: true }));
    }
  };
  return <div class="entity-list" onKeyDown={key} onFocusIn={(e) => {
    if ((e.target as Element).matches(".vlist") && !props.selected && props.items.length) props.select(props.items[index()].id);
  }}>
    <VirtualList label={`${props.kind === "agent" ? "team" : "task"} list`} class={`${props.kind === "agent" ? "team" : "task"}-list`}
      count={() => props.items.length} slice={(from, to) => props.items.slice(from, to)} rowHeight={160} follow={false} key={(item) => item.id}
      selected={index} onSelect={(i) => props.select(props.items[i].id)} onOpen={(i) => props.open(props.items[i].id)}
      render={(item) => {
        const summary = createMemo(() => props.summary(item));
        return <div class={`${props.kind === "agent" ? "team" : "task"}-row`} data-entity={item.id}
        ref={(el) => el.setAttribute(`data-${props.kind}`, item.id)} role="button" tabindex="-1" aria-label={`${item.id}, ${item.status}`} aria-description={summary().attention} title={summary().attention}
        classList={{ selected: props.selected === item.id, satellite: props.satellite?.(item) }} onClick={() => props.open(item.id)}
        onContextMenu={(e) => props.menu?.(e, item.id)}>
          <div class="entity-summary">
            <div class="summary-head"><b class={`mono ${summary().tone ?? ""}`}>{summary().name ?? item.id}</b><span>{summary().status ?? item.status}</span>
              <Show when={props.menu}><button aria-label={`More actions for ${item.id}`} aria-haspopup="menu" tabindex="-1"
                onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") e.stopPropagation(); }}
                onClick={(e) => { e.stopPropagation(); props.menu?.(e, item.id); }}>More</button></Show>
            </div>
            <div class="summary-title">{summary().title}</div>
            <div class="summary-info muted">{summary().info}</div>
            <div class="summary-info summary-detail muted">{summary().detail}</div>
          </div>
        </div>;
      }} />
  </div>;
}
