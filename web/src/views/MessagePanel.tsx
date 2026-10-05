// One message (notes/web-design.md §ui "Bus"): the whole text, shown as
// written, `reply_to` and the replies as links. A message the page does not
// hold is asked for with `message_get`.

import { For, Show, createResource, createSignal } from "solid-js";
import { useApp } from "../app";
import { clockTime } from "../format";
import { askedBy, decodeMessage, options, type Message } from "../protocol";
import { EmptyState } from "../components";
import { Linked } from "../text";
import { kindOf } from "./Bus";
import { PanelHead } from "./SidePanel";
import { waiting } from "./waiting";

export function MessagePanel(props: { id: string }) {
  const app = useApp();
  const { store, ui, link } = app;
  const local = (id: string): Message | undefined => {
    store.messagesVersion();
    for (const m of store.messages.newest()) if (m.id === id) return m;
    return store.state.questions.find((q) => q.id === id);
  };
  // Asked only while the page is live; asked again when it is live again.
  const [askedFor, setAskedFor] = createSignal<string | null>(null);
  const [fetched] = createResource(
    () => (local(props.id) || !link.live() ? null : props.id),
    (id) => (setAskedFor(id), link.call("message_get", { id }, decodeMessage)),
  );
  // A resource keeps its last value, or error, when its source goes (the page
  // stopped): only what was asked for this message is shown. One that failed
  // throws when read: its error is shown instead.
  const failed = () => (askedFor() === props.id ? fetched.error : undefined);
  const message = () => {
    const m = local(props.id);
    if (m) return m;
    if (fetched.error) return undefined;
    const f = fetched();
    return f?.id === props.id ? f : undefined;
  };
  const why = () => {
    const e = failed();
    if (!e) return waiting(link, "loading…");
    const text = e instanceof Error ? e.message : String(e);
    return (e as { code?: string }).code === "not_found" ? `not found: ${text}` : `not fetched: ${text}`;
  };
  const replies = () => {
    store.messagesVersion();
    return store.messages.toArray().filter((m) => m.reply_to === props.id);
  };
  const go = (id: string) => ui.select({ kind: "message", id });
  return (
    <div class="panel-body message-panel" aria-busy={fetched.loading}>
      <PanelHead title={`message ${props.id}`} />
      <Show when={message()} fallback={<EmptyState title={fetched.loading ? "Loading message…" : "Message unavailable"} description={why()} />}>
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
              <p class="muted">replies in loaded history:</p>
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
