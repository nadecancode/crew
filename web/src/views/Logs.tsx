// The event log (notes/web-design.md §ui "Logs"), virtual. Columns: time,
// level, source, type, agent, message. Filters: agent (`f` toggles the
// selected agent), level, text, and `vcs` in one click. It follows the tail
// while scrolled to the bottom; scrolled up it stays where the reader is.
// "Load older" asks `events` with a larger limit (the daemon caps it at 5000).

import { For, Show, createMemo, createSignal } from "solid-js";
import { useApp } from "../app";
import { clockTime, oneLine } from "../format";
import { EVENT_LEVEL, decodeEvents, type Event, type EventLevel } from "../protocol";
import { MAX_EVENTS } from "../store/store";
import { VirtualList } from "./VirtualList";

export const LOG_ROW = 22;
const RANK: Record<EventLevel, number> = { debug: 0, info: 1, warn: 2, error: 3 };

export function Logs() {
  const app = useApp();
  const { store, ui, link } = app;
  const [older, setOlder] = createSignal(0);
  const [loading, setLoading] = createSignal(false);
  const f = () => ui.logFilter();
  const filtering = () => !!(f().agent || f().vcs || f().text || f().level !== "debug");
  const filtered = createMemo(() => {
    store.eventsVersion();
    if (!filtering()) return null;
    const { agent, vcs, text, level } = f();
    const needle = text.toLowerCase();
    const min = RANK[level];
    return store.events.toArray().filter((e) =>
      (!agent || e.agent_id === agent) &&
      (!vcs || e.source === "vcs") &&
      RANK[e.level] >= min &&
      (!needle || e.message.toLowerCase().includes(needle) || e.event_type.toLowerCase().includes(needle) || (e.agent_id ?? "").toLowerCase().includes(needle)));
  });
  const count = () => (store.eventsVersion(), filtered()?.length ?? store.events.length);
  const slice = (from: number, to: number) => {
    const list = filtered();
    return list ? list.slice(from, to) : store.events.slice(from, to);
  };
  const at = (i: number): Event | undefined => {
    const list = filtered();
    return list ? list[i] : store.events.at(i);
  };
  const selectedSeq = () => (ui.sel()?.kind === "event" ? (ui.sel() as { seq: number }).seq : null);
  const cursor = createMemo(() => {
    const seq = selectedSeq();
    if (seq === null) return null;
    store.eventsVersion();
    const list = filtered() ?? store.events.toArray();
    const i = list.findIndex((e) => e.seq === seq);
    return i < 0 ? null : i;
  });

  const loadOlder = async () => {
    setLoading(true);
    try {
      const limit = Math.min(MAX_EVENTS, store.events.length + 1000);
      const list = await link.call("events", { limit }, decodeEvents);
      const before = store.events.length;
      store.prependEvents(list);
      setOlder((n) => n + (store.events.length - before));
    } catch (e) {
      ui.setStatus(`older events: ${e instanceof Error ? e.message : e}`, true);
    } finally {
      setLoading(false);
    }
  };

  const agents = () => store.state.agents.map((a) => a.id);
  return (
    <div class="logs" data-view="logs">
      <div class="filters" role="toolbar" aria-label="log filters">
        <label>agent
          <select value={f().agent ?? ""} onChange={(e) => ui.setLogFilter({ ...f(), agent: e.currentTarget.value || null })}>
            <option value="">all</option>
            <For each={agents()}>{(id) => <option value={id}>{id}</option>}</For>
          </select>
        </label>
        <label>level
          <select value={f().level} onChange={(e) => ui.setLogFilter({ ...f(), level: e.currentTarget.value as EventLevel })}>
            <For each={EVENT_LEVEL}>{(l) => <option value={l}>{l === "debug" ? "all" : `${l} and up`}</option>}</For>
          </select>
        </label>
        <button aria-pressed={f().vcs} classList={{ on: f().vcs }} onClick={() => ui.setLogFilter({ ...f(), vcs: !f().vcs })} title="only version control events">vcs</button>
        <input type="search" placeholder="filter text" aria-label="filter text" value={f().text} onInput={(e) => ui.setLogFilter({ ...f(), text: e.currentTarget.value })} />
        <Show when={filtering()}><button onClick={() => ui.setLogFilter({ agent: null, level: "debug", text: "", vcs: false })}>clear</button></Show>
        <span class="spacer" />
        <span class="muted">{count()} of {store.events.length}</span>
        <button disabled={!link.live() || loading() || store.events.length >= MAX_EVENTS} onClick={loadOlder}>load older</button>
      </div>
      <div class="log-head" aria-hidden="true"><span class="time">time</span><span class="level">level</span><span class="source">source</span><span class="type">type</span><span class="agent">agent</span><span class="msg">message</span></div>
      <VirtualList
        label="event log"
        class="log-list"
        count={count}
        slice={slice}
        rowHeight={LOG_ROW}
        prepended={older}
        selected={cursor}
        onSelect={(i) => {
          const e = at(i);
          if (e) ui.setRoute({ view: "logs", sel: { kind: "event", seq: e.seq } }, false);
        }}
        onOpen={(i) => {
          const e = at(i);
          if (e) ui.select({ kind: "event", seq: e.seq });
        }}
        empty={<div class="empty">{filtering() ? "nothing matches the filters" : "no events yet"}</div>}
        render={(e: Event) => (
          <div class={`log-row lv-${e.level}`} classList={{ vcs: e.source === "vcs" }} data-seq={e.seq}>
            <span class="time">{clockTime(e.timestamp)}</span>
            <span class="level">{e.level}</span>
            <span class="source">{e.source}</span>
            <span class="type">{e.event_type}</span>
            <span class="agent">{e.agent_id ?? ""}</span>
            <span class="msg">{oneLine(e.message)}</span>
          </div>
        )}
      />
    </div>
  );
}
