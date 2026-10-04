// One message (notes/web-design.md §ui "Bus"): the whole text, shown as
// written, `reply_to` and the replies as links. A message the page does not
// hold is asked for with `message_get`.

import { For, Show, createResource } from "solid-js";
import { useApp } from "../app";
import { clockTime } from "../format";
import { askedBy, decodeMessage, options, type Message } from "../protocol";
import { Linked } from "../text";
import { kindOf } from "./Bus";
import { PanelHead } from "./SidePanel";

export function MessagePanel(props: { id: string }) {
  const app = useApp();
  const { store, ui, link } = app;
  const local = (id: string): Message | undefined => {
    store.messagesVersion();
    for (const m of store.messages.newest()) if (m.id === id) return m;
    return store.state.questions.find((q) => q.id === id);
  };
  const [fetched] = createResource(
    () => (local(props.id) ? null : props.id),
    (id) => link.call("message_get", { id }, decodeMessage),
  );
  const message = () => local(props.id) ?? fetched();
  const replies = () => {
    store.messagesVersion();
    return store.messages.toArray().filter((m) => m.reply_to === props.id);
  };
  const go = (id: string) => ui.select({ kind: "message", id });
  return (
    <div class="panel-body message-panel">
      <PanelHead title={`message ${props.id}`} />
      <Show when={message()} fallback={<p class="pad muted">{fetched.error ? `not found: ${fetched.error.message ?? fetched.error}` : "loading…"}</p>}>
        {(m) => (
          <div class="pad scroll" tabindex="0" data-focus-first>
            <dl class="facts">
              <dt>from</dt><dd><AgentLink id={m().from} /></dd>
              <dt>to</dt><dd>{m().to === "*" ? "everyone" : <AgentLink id={m().to} />}</dd>
              <dt>type</dt><dd class={`k-${kindOf(m())}`}>{m().type}</dd>
              <dt>sent</dt><dd>{clockTime(m().timestamp)}</dd>
              <Show when={askedBy(m())}><dt>for</dt><dd>{askedBy(m())}</dd></Show>
              <Show when={m().reply_to}><dt>reply to</dt><dd><button class="link" onClick={() => go(m().reply_to!)}>{m().reply_to}</button></dd></Show>
            </dl>
            <pre class="body"><Linked text={m().body} /></pre>
            <Show when={options(m()).length}>
              <p class="muted">suggested answers:</p>
              <ul><For each={options(m())}>{(o) => <li>{o}</li>}</For></ul>
            </Show>
            <Show when={replies().length}>
              <p class="muted">replies:</p>
              <ul class="replies">
                <For each={replies()}>{(r) => <li><button class="link" onClick={() => go(r.id)}>{r.from}: {r.body.split("\n", 1)[0]}</button></li>}</For>
              </ul>
            </Show>
          </div>
        )}
      </Show>
    </div>
  );
}

export function AgentLink(props: { id: string }) {
  const { store, ui } = useApp();
  return (
    <Show when={store.agent(props.id)} fallback={<span>{props.id}</span>}>
      <button class="link agent-link" onClick={() => {
        ui.setSelected(props.id);
        ui.select({ kind: "agent", id: props.id });
      }}>{props.id}</button>
    </Show>
  );
}
