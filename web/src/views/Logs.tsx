// The event log (notes/web-design.md §ui "Logs"), virtual. Columns: time,
// level, source, type, agent, message. Filters: agent (`f` toggles the
// selected agent), level, text, and `vcs` in one click. It follows the tail
// while scrolled to the bottom; scrolled up it stays where the reader is.
// "Load older" asks `events` with a larger limit (the daemon caps it at 5000).

import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";
import { useApp } from "../app";
import { clockTime, oneLine } from "../format";
import { EVENT_LEVEL, decodeEvents, type Event, type EventLevel } from "../protocol";
import { MAX_EVENTS } from "../store/store";
import { EmptyState, ViewHeader } from "../components";
import { VirtualList } from "./VirtualList";

export const LOG_ROW = 36;
export const LOG_NARROW_ROW = 64;
const RANK: Record<EventLevel, number> = { debug: 0, info: 1, warn: 2, error: 3 };

export function Logs() {
  const app = useApp();
  const { store, ui, link } = app;
  const media = window.matchMedia("(max-width: 767px)");
  const [narrow, setNarrow] = createSignal(media.matches);
  const resize = () => setNarrow(media.matches);
  media.addEventListener("change", resize);
  onCleanup(() => media.removeEventListener("change", resize));
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let active = true;
  onCleanup(() => (active = false));
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
    if (!link.live() || loading()) return;
    setLoading(true);
    setError(null);
    try {
      const limit = Math.min(MAX_EVENTS, store.events.length + 1000);
      const list = await link.call("events", { limit }, decodeEvents);
      if (active) store.prependEvents(list);
    } catch (e) {
      if (active) setError(`Older events: ${e instanceof Error ? e.message : e}`);
    } finally {
      if (active) setLoading(false);
    }
  };

  const agents = () => store.state.agents.map((a) => a.id);
  return (
    <div class="logs" data-view="logs" aria-busy={loading()}>
      <ViewHeader title="Events" meta={`${count()} loaded`} description="Search covers loaded events." />
      <div class="filters" role="toolbar" aria-label="event filters">
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
        <button aria-pressed={f().vcs} classList={{ on: f().vcs }} onClick={() => ui.setLogFilter({ ...f(), vcs: !f().vcs })} title="only version control events">Version control</button>
        <label>Search <input type="search" aria-label="filter text" value={f().text} onInput={(e) => ui.setLogFilter({ ...f(), text: e.currentTarget.value })} /></label>
        <Show when={filtering()}><button onClick={() => ui.setLogFilter({ agent: null, level: "debug", text: "", vcs: false })}>clear</button></Show>
        <span class="spacer" />
        <span class="muted">{count()} of {(store.eventsVersion(), store.events.length)}</span>
        <button disabled={!link.live() || loading() || store.events.length >= MAX_EVENTS} onClick={loadOlder}>{loading() ? "Loading…" : "Load older"}</button>
      </div>
      <Show when={error()}><div class="fetch-state err" role="status">{error()}</div></Show>
      <div class="log-head" aria-hidden="true"><span class="time">time</span><span class="level">level</span><span class="source-type">source / type</span><span class="agent">agent</span><span class="msg">message</span></div>
      <VirtualList
        label="event log"
        class="log-list"
        count={count}
        slice={slice}
        rowHeight={narrow() ? LOG_NARROW_ROW : LOG_ROW}
        key={(e: Event) => e.seq}
        selected={cursor}
        onSelect={(i) => {
          const e = at(i);
          if (e) ui.setRoute({ view: "logs", sel: { kind: "event", seq: e.seq } }, false);
        }}
        onOpen={(i) => {
          const e = at(i);
          if (e) ui.select({ kind: "event", seq: e.seq });
        }}
        empty={<EmptyState title={filtering() ? "No events match these filters" : "No events yet"} description={filtering() ? "Clear the filters to see loaded events." : "Events appear as the team works."} />}
        render={(e: Event) => (
          <div class={`log-row lv-${e.level}`} classList={{ vcs: e.source === "vcs" }} data-seq={e.seq}>
            <span class="time">{clockTime(e.timestamp)}</span>
            <span class="level">{e.level}</span>
            <span class="source-type"><span class="source">{e.source}</span> · <span class="type">{e.event_type}</span></span>
            <span class="agent">{e.agent_id ?? ""}</span>
            <span class="msg">{oneLine(e.message)}</span>
          </div>
        )}
      />
    </div>
  );
}
