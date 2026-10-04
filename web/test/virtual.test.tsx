// The virtual list (notes/web-design.md §perf 4, §tests "virtual.test.tsx").
import { render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
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
