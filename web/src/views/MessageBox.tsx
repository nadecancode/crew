// The message box and command line (notes/web-design.md §ui): `m` addresses
// it to the selected agent, `M` to everyone, `/` or ctrl-k opens it as the
// command line; in the inspector `i` makes it the prompt box (what is typed
// is submitted to the agent's terminal as one prompt). Enter sends, shift-enter
// is a new line, tab completes a command, esc leaves the box. A line that
// starts with `/` is never sent as a message; a failure keeps the text.

import { For, Show, createSignal } from "solid-js";
import { useApp } from "../app";
import { complete, hint } from "../commands";
import { BROADCAST } from "../protocol";
import { useOps } from "./ops";

export function MessageBox() {
  const app = useApp();
  const ops = useOps();
  const { ui, store, link } = app;
  const [note, setNote] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  let back: HTMLElement | null = null;
  const c = () => ui.compose();
  const set = (patch: Partial<ReturnType<typeof ui.compose>>) => ui.setCompose({ ...c(), ...patch });
  const line = () => note() ?? hint(c().text);

  const submit = async () => {
    if (busy() || !link.live()) return;
    const { to, text, prompt } = c();
    if (!text.trim() && !prompt) return;
    setBusy(true);
    try {
      const r = await ops.submit(to, text, prompt);
      setNote(r.hint ?? null);
      if (!r.keep) set({ text: "", prompt: false });
    } finally {
      setBusy(false);
    }
  };
  const onKey = (e: KeyboardEvent) => {
    const el = e.currentTarget as HTMLTextAreaElement;
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void submit();
    } else if (e.key === "Tab" && !e.shiftKey && c().text.startsWith("/") && !/\s/.test(c().text)) {
      // Completing a command name; otherwise tab moves focus as usual.
      const done = complete(c().text);
      if (done && done !== c().text) {
        e.preventDefault();
        set({ text: done });
        queueMicrotask(() => el.setSelectionRange(done.length, done.length));
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      setNote(null);
      if (c().prompt) set({ prompt: false });
      el.blur();
      if (back?.isConnected) back.focus();
    }
  };
  const recipients = () => store.state.agents.map((a) => a.id);
  return (
    <section class="message-box" data-region="compose" aria-label="message box" classList={{ prompt: c().prompt, command: c().text.startsWith("/") }}>
      <div class="mb-row">
        <Show when={!c().prompt} fallback={<span class="to-label">prompt to <b>{c().to}</b></span>}>
          <label class="to-label">to
            <select aria-label="recipient" value={c().to} disabled={!link.live()} onChange={(e) => set({ to: e.currentTarget.value })}>
              <option value={BROADCAST}>everyone</option>
              <For each={recipients()}>{(id) => <option value={id}>{id}</option>}</For>
            </select>
          </label>
        </Show>
        <textarea id="compose-text" rows="1" aria-label={c().prompt ? `prompt for ${c().to}` : "message, or a / command"} spellcheck={false}
          placeholder={link.live() ? (c().prompt ? "typed into the agent's terminal as one prompt" : "message · / for a command · m message · M everyone") : "read-only: not connected"}
          disabled={!link.live()} value={c().text}
          onFocus={(e) => {
            if (e.relatedTarget instanceof HTMLElement && !e.currentTarget.closest("section")!.contains(e.relatedTarget)) back = e.relatedTarget;
          }}
          onInput={(e) => {
            setNote(null);
            set({ text: e.currentTarget.value });
          }}
          onKeyDown={onKey} />
        <button class="primary" disabled={!link.live() || busy() || (!c().text.trim() && !c().prompt)} onClick={() => void submit()}>
          {c().text.startsWith("/") ? "run" : c().prompt ? "submit" : "send"}
        </button>
      </div>
      <Show when={line()}>
        <div class="mb-hint" classList={{ err: !!note() }} role={note() ? "alert" : undefined}>{line()}</div>
      </Show>
    </section>
  );
}
