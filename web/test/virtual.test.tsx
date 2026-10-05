// The virtual list (notes/web-design.md §perf 4, §tests "virtual.test.tsx").
import { render } from "@solidjs/testing-library";
import { batch, createMemo, createSignal } from "solid-js";
import { beforeAll, expect, test } from "vitest";
import { VirtualList } from "../src/views/VirtualList";

const H = 20;
const VIEW = 400;

beforeAll(() => {
  // jsdom lays nothing out: a list of fixed rows in a 400 px pane.
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get() { return this.classList.contains("vlist") ? VIEW : 0; } });
  Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
    configurable: true,
    get() {
      const pad = (this as HTMLElement).querySelector?.(".vlist-pad") as HTMLElement | null;
      return pad ? parseFloat(pad.style.height) || 0 : 0;
    },
  });
});

function list(n: number) {
  const [items, setItems] = createSignal(Array.from({ length: n }, (_, i) => i));
  const r = render(() => (
    <VirtualList label="rows" count={() => items().length} slice={(a, b) => items().slice(a, b)} rowHeight={H} render={(i: number) => <span class="cell">{i}</span>} />
  ));
  const el = r.container.querySelector(".vlist") as HTMLDivElement;
  return { el, setItems, items, rows: () => [...r.container.querySelectorAll(".cell")].map((c) => Number(c.textContent)) };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

test("5000 rows put fewer than 80 in the DOM", async () => {
  const { rows } = list(5000);
  await tick();
  expect(rows().length).toBeLessThan(80);
  expect(rows().length).toBeGreaterThan(VIEW / H);
});

test("it follows the tail at the bottom, and stays put when scrolled up", async () => {
  const { el, rows, setItems, items } = list(1000);
  await tick();
  // At the bottom: the newest rows are drawn.
  expect(rows()).toContain(999);
  setItems([...items(), 1000, 1001]);
  await tick();
  await tick();
  expect(rows()).toContain(1001);
  // Scrolled up: appending does not move what is being read.
  el.scrollTop = 200 * H;
  el.dispatchEvent(new Event("scroll"));
  await tick();
  const before = rows();
  expect(before).toContain(205);
  setItems([...items(), 1002, 1003, 1004]);
  await tick();
  await tick();
  expect(el.scrollTop).toBe(200 * H);
  expect(rows()).toEqual(before);
});

function responsiveList(follow?: boolean, initial: string | null = "row-22", initialHeight = 36) {
  const [items, setItems] = createSignal(Array.from({ length: 1000 }, (_, i) => ({ id: `row-${i}` })));
  const [rowHeight, setRowHeight] = createSignal(initialHeight);
  const [chosen, setChosen] = createSignal<string | null>(initial);
  const result = render(() => {
    const selected = createMemo(() => {
      const i = items().findIndex((item) => item.id === chosen());
      return i < 0 ? null : i;
    });
    return <VirtualList label="responsive rows" count={() => items().length} slice={(a, b) => items().slice(a, b)}
      rowHeight={rowHeight()} key={(item) => item.id} selected={selected} follow={follow}
      onSelect={(i) => setChosen(items()[i].id)} render={(item) => <span class="cell" data-key={item.id}>{item.id}</span>} />;
  });
  const el = result.container.querySelector<HTMLDivElement>(".vlist")!;
  const scroll = (value: number) => { el.scrollTop = value; el.dispatchEvent(new Event("scroll")); };
  const selectedKey = () => el.querySelector(".vrow.selected .cell")?.getAttribute("data-key");
  return { el, items, setItems, rowHeight, setRowHeight, scroll, selectedKey, chosen };
}

test("responsive row height preserves the keyed item, fractional offset, selection and row bound", async () => {
  const list = responsiveList();
  await tick();
  list.scroll(20 * 36 + 9);
  await tick();
  expect(list.selectedKey()).toBe("row-22");
  expect(list.el.querySelectorAll(".vrow").length).toBeLessThanOrEqual(12 + 20);
  list.setRowHeight(64);
  await tick();
  expect(list.el.scrollTop).toBe(20 * 64 + 16);
  expect(list.selectedKey()).toBe("row-22");
  expect(list.el.querySelectorAll(".vrow").length).toBeLessThanOrEqual(7 + 20);
  expect(list.el.querySelector<HTMLElement>(".vrow")?.style.height).toBe("64px");
  // The ring drops three old rows while this reader is away from the tail.
  list.setItems(list.items().slice(3));
  await tick();
  expect(list.el.scrollTop).toBe(17 * 64 + 16);
  expect(list.selectedKey()).toBe("row-22");
  list.setRowHeight(36);
  await tick();
  expect(list.el.scrollTop).toBe(17 * 36 + 9);
  expect(list.selectedKey()).toBe("row-22");
});

test("responsive rows still follow the last item through resize and append", async () => {
  const list = responsiveList();
  await tick();
  list.setRowHeight(64);
  list.setItems([...list.items(), { id: "row-newest" }]);
  await tick(); await tick();
  expect(list.el.querySelector('[data-key="row-newest"]')).not.toBeNull();
  expect(list.el.querySelector<HTMLElement>(".vlist-pad")?.style.height).toBe("64064px");
  expect(list.el.querySelectorAll(".vrow").length).toBeLessThanOrEqual(7 + 20);
});

test("same-batch row resize and ring eviction preserve the key and its fractional offset", async () => {
  const list = responsiveList();
  await tick();
  list.scroll(20 * 36 + 9);
  await tick();
  batch(() => {
    list.setRowHeight(64);
    list.setItems(list.items().slice(3));
  });
  await tick();
  expect(list.el.scrollTop).toBe(17 * 64 + 16);
  expect(list.selectedKey()).toBe("row-22");
});

test("follow=false starts at the top and appends leave its keyed reading position", async () => {
  const list = responsiveList(false, null);
  await tick();
  expect(list.el.scrollTop).toBe(0);
  expect(list.el.querySelector('[data-key="row-0"]')).not.toBeNull();
  expect(list.el.querySelector('[data-key="row-999"]')).toBeNull();
  list.scroll(20 * 36 + 9);
  list.setItems([...list.items(), { id: "row-newest" }]);
  await tick();
  expect(list.el.scrollTop).toBe(20 * 36 + 9);
  list.setItems(list.items().slice(3));
  await tick();
  expect(list.el.scrollTop).toBe(17 * 36 + 9);
});

test("follow=false restores an existing selection without jumping to the tail", async () => {
  const list = responsiveList(false, "row-400");
  await tick();
  expect(list.selectedKey()).toBe("row-400");
  expect(list.el.querySelector('[data-key="row-999"]')).toBeNull();
  list.setItems([...list.items(), { id: "row-newest" }]);
  await tick();
  expect(list.selectedKey()).toBe("row-400");
  expect(list.el.querySelector('[data-key="row-newest"]')).toBeNull();
});

test("a row taller than its viewport keeps selected identity at the top on mount and keys", async () => {
  const descriptor = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight")!;
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get() { return this.classList.contains("vlist") ? 80 : 0; } });
  try {
    const list = responsiveList(false, "row-400", 160);
    await tick();
    expect(list.el.scrollTop).toBe(400 * 160);
    expect(list.selectedKey()).toBe("row-400");
    list.el.dispatchEvent(new KeyboardEvent("keydown", { key: "Home", bubbles: true, cancelable: true }));
    await tick();
    expect(list.el.scrollTop).toBe(0);
    expect(list.selectedKey()).toBe("row-0");
    list.el.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }));
    await tick();
    expect(list.el.scrollTop).toBe(160);
    expect(list.selectedKey()).toBe("row-1");
  } finally { Object.defineProperty(HTMLElement.prototype, "clientHeight", descriptor); }
});
