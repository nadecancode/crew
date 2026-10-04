// A virtual list (notes/web-design.md §perf 4): fixed row height, rows for
// the visible range plus 10 above and below, the rest one spacer's height.
// Scrolled to the bottom it follows the tail; scrolled up, appends do not move
// what is being read. One tab stop; the arrow keys, `j`/`k`, PageUp/PageDown
// and Home/End move the selected row.

import { For, createEffect, createMemo, createSignal, on, onCleanup, onMount, untrack, type JSX } from "solid-js";

export const OVERSCAN = 10;

export function VirtualList<T>(props: {
  /** How many rows there are; read with the list's version signal. */
  count: () => number;
  /** Rows `from` to `to` (exclusive), oldest first; read with the list's version signal. */
  slice: (from: number, to: number) => T[];
  rowHeight: number;
  render: (item: T, index: () => number, selected: () => boolean) => JSX.Element;
  label: string;
  class?: string;
  /** The selected row's index, if the list has a selection. */
  selected?: () => number | null;
  onSelect?(index: number): void;
  onOpen?(index: number): void;
  /** Rows at the top were added (older entries loaded): keep the reader's place. */
  prepended?: () => number;
  empty?: JSX.Element;
}) {
  let el!: HTMLDivElement;
  const [top, setTop] = createSignal(0);
  const [height, setHeight] = createSignal(400);
  const [stick, setStick] = createSignal(true);
  const H = props.rowHeight;

  const range = createMemo(() => {
    const n = props.count();
    const first = Math.max(0, Math.floor(top() / H) - OVERSCAN);
    const last = Math.min(n, Math.ceil((top() + height()) / H) + OVERSCAN);
    return { first, items: props.slice(first, last), total: n };
  });

  const toBottom = () => {
    el.scrollTop = el.scrollHeight;
    setTop(el.scrollTop);
  };
  onMount(() => {
    const ro = new ResizeObserver(() => {
      setHeight(el.clientHeight);
      if (stick()) toBottom();
    });
    ro.observe(el);
    onCleanup(() => ro.disconnect());
    setHeight(el.clientHeight || 400);
    toBottom();
  });
  // Follow the tail while the reader is at the bottom.
  createEffect(on(() => props.count(), () => {
    if (stick()) queueMicrotask(() => el && toBottom());
  }, { defer: true }));
  // Older rows put in front: the same rows stay under the reader.
  createEffect(on(() => props.prepended?.() ?? 0, (n, prev) => {
    const added = n - (prev ?? 0);
    if (added > 0 && !stick()) queueMicrotask(() => {
      el.scrollTop += added * H;
      setTop(el.scrollTop);
    });
  }, { defer: true }));

  const onScroll = () => {
    setTop(el.scrollTop);
    setStick(el.scrollTop + el.clientHeight >= el.scrollHeight - H / 2);
  };

  const ensureVisible = (i: number) => {
    const y = i * H;
    if (y < el.scrollTop) el.scrollTop = y;
    else if (y + H > el.scrollTop + el.clientHeight) el.scrollTop = y + H - el.clientHeight;
    onScroll();
  };
  const move = (to: number) => {
    const n = props.count();
    if (n === 0) return;
    const i = Math.min(n - 1, Math.max(0, to));
    props.onSelect?.(i);
    ensureVisible(i);
  };
  const page = () => Math.max(1, Math.floor(el.clientHeight / H) - 1);
  const onKey = (e: KeyboardEvent) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    const cur = props.selected?.() ?? null;
    const at = cur ?? Math.min(props.count() - 1, Math.floor((el.scrollTop + el.clientHeight) / H) - 1);
    const keys: Record<string, () => void> = {
      ArrowDown: () => move(at + 1),
      j: () => move(at + 1),
      ArrowUp: () => move(cur === null ? at : at - 1),
      k: () => move(cur === null ? at : at - 1),
      PageDown: () => move(at + page()),
      PageUp: () => move(at - page()),
      Home: () => move(0),
      End: () => move(props.count() - 1),
      Enter: () => cur !== null && props.onOpen?.(cur),
    };
    const f = keys[e.key];
    if (f) {
      e.preventDefault();
      e.stopPropagation();
      f();
    }
  };

  return (
    <div ref={el} class={`vlist ${props.class ?? ""}`} tabindex="0" role="listbox" aria-label={props.label} onScroll={onScroll} onKeyDown={onKey}
      aria-activedescendant={props.selected?.() != null ? `${props.label.replace(/\W+/g, "-")}-${props.selected!()}` : undefined}>
      <div class="vlist-pad" style={{ height: `${range().total * H}px` }}>
        <div class="vlist-rows" style={{ transform: `translateY(${range().first * H}px)` }}>
          <For each={range().items}>
            {(item, k) => {
              const i = () => range().first + k();
              return (
              <div class="vrow" role="option" id={`${props.label.replace(/\W+/g, "-")}-${i()}`} style={{ height: `${H}px` }}
                aria-selected={props.selected?.() === i()}
                classList={{ selected: props.selected?.() === i() }}
                onClick={() => props.onSelect?.(i())}
                onDblClick={() => props.onOpen?.(i())}>
                {untrack(() => props.render(item, i, () => props.selected?.() === i()))}
              </div>
              );
            }}
          </For>
        </div>
      </div>
      {range().total === 0 ? props.empty : null}
    </div>
  );
}
