// A virtual list (notes/web-design.md §perf 4): fixed row height, rows for
// the visible range plus 10 above and below, the rest one spacer's height.
// Scrolled to the bottom it follows the tail; scrolled up, what is being read
// stays put: appends do not move it, and with `key` neither do rows taken
// from the front (a full ring drops its oldest) or put there (older rows
// loaded), until the row read itself is dropped. One tab stop; the arrow keys, `j`/`k`, PageUp/PageDown
// and Home/End move the selected row.

import { For, createEffect, createMemo, createSignal, on, onCleanup, onMount, untrack, type JSX } from "solid-js";

export const OVERSCAN = 10;

export function VirtualList<T>(props: {
  /** How many rows there are; read with the list's version signal. */
  count: () => number;
  /** Rows `from` to `to` (exclusive), oldest first; read with the list's version signal. */
  slice: (from: number, to: number) => T[];
  rowHeight: number;
  /** Reading lists may opt out of tail-following (default true). */
  follow?: boolean;
  render: (item: T, index: () => number, selected: () => boolean) => JSX.Element;
  label: string;
  class?: string;
  /** The selected row's index, if the list has a selection. */
  selected?: () => number | null;
  onSelect?(index: number): void;
  onOpen?(index: number): void;
  /** Rows at the top were added (older entries loaded): keep the reader's place.
   * Not needed with `key`, which keeps it whatever moved. */
  prepended?: () => number;
  /** A row's identity: the reader's place is kept by it. */
  key?: (item: T) => string | number;
  empty?: JSX.Element;
}) {
  let el!: HTMLDivElement;
  const [top, setTop] = createSignal(0);
  const [height, setHeight] = createSignal(400);
  const [stick, setStick] = createSignal(true);
  const following = () => props.follow !== false && stick();
  const H = () => props.rowHeight;

  const range = createMemo(() => {
    const n = props.count();
    // Never past the end, whatever the scroll position says.
    const t = Math.min(top(), Math.max(0, n * H() - height()));
    const first = Math.max(0, Math.floor(t / H()) - OVERSCAN);
    const last = Math.min(n, Math.ceil((t + height()) / H()) + OVERSCAN);
    return { first, items: props.slice(first, last), total: n };
  });

  const toBottom = () => {
    el.scrollTop = el.scrollHeight;
    setTop(el.scrollTop);
  };
  onMount(() => {
    const ro = new ResizeObserver(() => {
      setHeight(el.clientHeight);
      if (following()) toBottom();
    });
    ro.observe(el);
    onCleanup(() => ro.disconnect());
    setHeight(el.clientHeight || 400);
    if (following()) toBottom();
    else {
      const selected = props.selected?.();
      if (selected != null) ensureVisible(selected);
      setAnchor();
    }
  });
  // Follow the tail while the reader is at the bottom.
  createEffect(on(() => props.count(), () => {
    if (following()) queueMicrotask(() => el && toBottom());
  }, { defer: true }));
  // Older rows put in front: the same rows stay under the reader.
  createEffect(on(() => props.prepended?.() ?? 0, (n, prev) => {
    const added = n - (prev ?? 0);
    if (added > 0 && !following() && !props.key) queueMicrotask(() => {
      el.scrollTop += added * H();
      setTop(el.scrollTop);
    });
  }, { defer: true }));

  // The row at the top of the view, by identity.
  let anchor: { i: number; key: string | number } | null = null;
  const keyAt = (i: number) => {
    const item = props.slice(i, i + 1)[0];
    return item === undefined ? undefined : props.key!(item);
  };
  const setAnchor = () => {
    if (!props.key) return;
    const i = Math.floor(el.scrollTop / H());
    const key = untrack(() => keyAt(i));
    anchor = key === undefined ? null : { i, key };
  };
  // A responsive row keeps the same item and fractional offset under the
  // reader. Wait for the new spacer geometry before assigning scrollTop.
  createEffect(on(H, (h, previous) => {
    if (previous === undefined) return;
    const row = el.scrollTop / previous;
    queueMicrotask(() => {
      if (following()) toBottom();
      else {
        el.scrollTop = row * h;
        setTop(el.scrollTop);
        setAnchor();
      }
    });
  }, { defer: true }));
  // The rows changed: where the anchor went, the view goes (nearest first, as
  // a few rows dropped or added is the common case).
  createEffect(on(() => (props.count(), props.slice(0, 0)), () => {
    if (!props.key || following() || !anchor) return;
    const a = anchor;
    untrack(() => {
      const n = props.count();
      if (keyAt(a.i) === a.key) return;
      for (let d = 1; d < n; d++) {
        for (const j of [a.i - d, a.i + d]) {
          if (j < 0 || j >= n || keyAt(j) !== a.key) continue;
          el.scrollTop += (j - a.i) * H();
          anchor = { i: j, key: a.key };
          setTop(el.scrollTop);
          return;
        }
        if (a.i - d < 0 && a.i + d >= n) break;
      }
      // Dropped: the row now in its place is read instead.
      setAnchor();
    });
  }, { defer: true }));

  const onScroll = () => {
    setAnchor();
    setTop(el.scrollTop);
    setStick(el.scrollTop + el.clientHeight >= el.scrollHeight - H() / 2);
  };

  const ensureVisible = (i: number) => {
    const y = i * H();
    if (y < el.scrollTop) el.scrollTop = y;
    else if (y + H() > el.scrollTop + el.clientHeight) el.scrollTop = y + H() - el.clientHeight;
    onScroll();
  };
  const move = (to: number) => {
    const n = props.count();
    if (n === 0) return;
    const i = Math.min(n - 1, Math.max(0, to));
    props.onSelect?.(i);
    ensureVisible(i);
  };
  const page = () => Math.max(1, Math.floor(el.clientHeight / H()) - 1);
  const onKey = (e: KeyboardEvent) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    const cur = props.selected?.() ?? null;
    const at = cur ?? Math.min(props.count() - 1, Math.floor((el.scrollTop + el.clientHeight) / H()) - 1);
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
      <div class="vlist-pad" style={{ height: `${range().total * H()}px` }}>
        <div class="vlist-rows" style={{ transform: `translateY(${range().first * H()}px)` }}>
          <For each={range().items}>
            {(item, k) => {
              const i = () => range().first + k();
              return (
              <div class="vrow" role="option" id={`${props.label.replace(/\W+/g, "-")}-${i()}`} style={{ height: `${H()}px` }}
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
