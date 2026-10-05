// The bus (notes/web-design.md §ui "Bus"): the message list, newest at the
// bottom, virtual. A row: endpoints, type, time, and a two-line body preview;
// as in the TUI. Click or `enter`: the message's panel. Hovering a row lights
// the link between its two agents on the graph; clicking a link filters the
// list to that pair.

import { Show, createMemo, createSignal, onCleanup } from "solid-js";
import { useApp } from "../app";
import { clockTime, oneLine } from "../format";
import { decodeMessages, isBroadcast, isP2p, ORCHESTRATOR_ID, OPERATOR_ID, type Message } from "../protocol";
import { EmptyState } from "../components";
import { VirtualList } from "./VirtualList";
import { PanelHead } from "./SidePanel";

export const BUS_ROW = 84;

export function kindOf(m: Message): "orch" | "p2p" | "broadcast" {
  if (isBroadcast(m)) return "broadcast";
  return isP2p(m) ? "p2p" : "orch";
}

const inPair = (m: Message, [a, b]: [string, string]) => {
  const ends = (x: string) => (x === OPERATOR_ID ? ORCHESTRATOR_ID : x);
  const [f, t] = [ends(m.from), ends(m.to)];
  return (f === a && t === b) || (f === b && t === a);
};

export function Bus() {
  const app = useApp();
  const { store, ui, link } = app;
  // The selected row by identity: it stays selected as rows come and go.
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let active = true;
  onCleanup(() => (active = false));
  const filtered = createMemo(() => {
    store.messagesVersion();
    const pair = ui.busPair();
    return pair ? store.messages.toArray().filter((m) => inPair(m, pair)) : null;
  });
  const count = () => (store.messagesVersion(), filtered()?.length ?? store.messages.length);
  const slice = (from: number, to: number) => {
    const f = filtered();
    return f ? f.slice(from, to) : store.messages.slice(from, to);
  };
  const at = (i: number) => {
    const f = filtered();
    return f ? f[i] : store.messages.at(i);
  };
  const cursor = createMemo(() => {
    const id = selectedId();
    if (id === null) return null;
    store.messagesVersion();
    const i = (filtered() ?? store.messages.toArray()).findIndex((m) => m.id === id);
    return i < 0 ? null : i;
  });

  const loadOlder = async () => {
    if (!link.live() || loading()) return;
    setLoading(true);
    setError(null);
    try {
      const have = store.messages.length;
      const list = await link.call("history", { limit: Math.min(have + 300, store.messages.capacity) }, decodeMessages);
      if (active) store.prependMessages(list);
    } catch (e) {
      if (active) setError(`Older messages: ${e instanceof Error ? e.message : e}`);
    } finally {
      if (active) setLoading(false);
    }
  };

  return (
    <div class="panel-body bus" aria-busy={loading()}>
      <PanelHead title="Message bus" sub={`${count()} loaded`}>
        <button disabled={!link.live() || loading()} onClick={loadOlder} title="ask the daemon for older messages">{loading() ? "Loading…" : "Load older"}</button>
      </PanelHead>
        <Show when={ui.busPair()}>
          <div class="bus-scope"><span class="chip filter">{ui.busPair()![0]} ↔ {ui.busPair()![1]} <button aria-label="clear the filter" onClick={() => ui.setBusPair(null)}>×</button></span></div>
        </Show>
      <Show when={error()}><div class="fetch-state err" role="status">{error()}</div></Show>
      <VirtualList
        label="messages"
        class="bus-list"
        count={count}
        slice={slice}
        rowHeight={BUS_ROW}
        key={(m: Message) => m.id}
        selected={cursor}
        onSelect={(i) => {
          const m = at(i);
          setSelectedId(m?.id ?? null);
          if (m) ui.setHoverPair([m.from, m.to]);
        }}
        onOpen={(i) => {
          const m = at(i);
          if (m) ui.select({ kind: "message", id: m.id });
        }}
        empty={<EmptyState title={ui.busPair() ? "No messages between these agents" : "No messages yet"} description="Messages appear here as the team communicates." />}
        render={(m: Message) => (
          <div class={`bus-row k-${kindOf(m)} t-${m.type}`} data-message={m.id}
            onPointerEnter={() => ui.setHoverPair([m.from, m.to])} onPointerLeave={() => ui.setHoverPair(null)}
            onClick={() => ui.select({ kind: "message", id: m.id })}>
            <div class="bus-meta">
              <span class="who"><span class="from">{m.from}</span> → <span class="to">{m.to === "*" ? "all" : m.to}</span></span>
              <span class="type">{m.type}</span>
              <span class="time">{clockTime(m.timestamp)}</span>
            </div>
            <span class="first">{oneLine(m.body)}</span>
          </div>
        )}
      />
    </div>
  );
}
