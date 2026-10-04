import { For, createMemo, createSignal } from "solid-js";
import { events, eventsVersion } from "./store";

const ROW = 20, OVERSCAN = 10;

// Fixed-height rows; only the window is in the DOM, whatever the log's length.
export function LogList() {
  const [top, setTop] = createSignal(0);
  const [height, setHeight] = createSignal(400);
  let stick = true;
  let el!: HTMLDivElement;
  const range = createMemo(() => {
    eventsVersion();
    const first = Math.max(0, Math.floor(top() / ROW) - OVERSCAN);
    const last = Math.min(events.length, Math.ceil((top() + height()) / ROW) + OVERSCAN);
    return { first, rows: events.slice(first, last), total: events.length };
  });
  const onScroll = () => { setTop(el.scrollTop); stick = el.scrollTop + el.clientHeight >= el.scrollHeight - ROW; };
  // Follow the tail while the reader is at the bottom.
  createMemo(() => { eventsVersion(); queueMicrotask(() => { if (stick && el) { el.scrollTop = el.scrollHeight; setTop(el.scrollTop); setHeight(el.clientHeight); } }); });
  return (
    <div ref={el} class="log" onScroll={onScroll}>
      <div class="log-pad" style={{ height: `${range().total * ROW}px` }}>
        <div class="log-rows" style={{ transform: `translateY(${range().first * ROW}px)` }}>
          <For each={range().rows}>
            {(e) => (
              <div class="row" classList={{ [e.level]: true }}>
                <span class="seq">{e.seq}</span>
                <span class="type">{e.event_type}</span>
                <span class="agent">{e.agent_id ?? ""}</span>
                <span class="msg">{e.message}</span>
              </div>
            )}
          </For>
        </div>
      </div>
    </div>
  );
}
