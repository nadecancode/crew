// Dialogs (notes/web-design.md §ui): modal, they keep focus inside, close
// with `esc`, and give focus back to where it was. A confirmation names what
// will happen and to whom; focus starts on "cancel".

import { For, Match, Show, Switch, createEffect, createMemo, createResource, createSignal, on, onMount, type JSX } from "solid-js";
import { useApp } from "../app";
import { COMMANDS, DECLINED, usage } from "../commands";
import { fmtElapsed, runtimeLabel } from "../format";
import { KEYS } from "../keys";
import { asker, decodeModels, options, RUNTIME_KIND, type ModelChoice, type RuntimeKind } from "../protocol";
import { Linked } from "../text";
import type { Confirm } from "../ui";

const FOCUSABLE = "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";

/** A modal dialog: focus stays inside; esc closes. `refocus` names what
 * redraws the dialog's controls: when it changes and the focused control went
 * with the redraw, focus goes back to the first one. */
export function Modal(props: { title: string; label?: string; class?: string; onClose(): void; children: JSX.Element; initial?: string; refocus?: () => unknown }) {
  let box!: HTMLDivElement;
  const focusFirst = () => {
    const wanted = props.initial ? [...box.querySelectorAll<HTMLElement>(props.initial)].find((el) => !(el as HTMLButtonElement).disabled) : undefined;
    (wanted ?? box.querySelector<HTMLElement>(FOCUSABLE) ?? box).focus();
  };
  onMount(focusFirst);
  createEffect(on(() => props.refocus?.(), () => queueMicrotask(() => {
    const at = document.activeElement;
    if (box.isConnected && (at === box || !box.contains(at))) focusFirst();
  }), { defer: true }));
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      props.onClose();
      return;
    }
    if (e.key !== "Tab") return;
    const items = [...box.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) => el.offsetParent !== null || el === document.activeElement);
    if (items.length === 0) return e.preventDefault();
    const i = items.indexOf(document.activeElement as HTMLElement);
    const next = e.shiftKey ? (i <= 0 ? items.length - 1 : i - 1) : i === items.length - 1 ? 0 : i + 1;
    e.preventDefault();
    items[next].focus();
  };
  return (
    <div class="scrim" onPointerDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div ref={box} class={`dialog ${props.class ?? ""}`} role="dialog" aria-modal="true" aria-label={props.label ?? props.title} tabindex="-1" onKeyDown={onKey}>
        <h2 class="dialog-title">{props.title}</h2>
        {props.children}
      </div>
    </div>
  );
}

export function Dialogs() {
  const { ui } = useApp();
  const d = () => ui.dialog();
  return (
    <Switch>
      <Match when={d()?.kind === "question"}><QuestionDialog id={(d() as { id: string }).id} /></Match>
      <Match when={d()?.kind === "model"}><ModelDialog agent={(d() as { agent: string }).agent} /></Match>
      <Match when={d()?.kind === "confirm"}><ConfirmDialog confirm={(d() as { confirm: Confirm }).confirm} /></Match>
      <Match when={d()?.kind === "help"}><HelpDialog /></Match>
    </Switch>
  );
}

function QuestionDialog(props: { id: string }) {
  const app = useApp();
  const { store, ui, link } = app;
  const open = () => store.openQuestions();
  const q = () => open().find((x) => x.id === props.id);
  const index = () => open().findIndex((x) => x.id === props.id);
  const [own, setOwn] = createSignal("");
  const [sending, setSending] = createSignal(false);
  // Another question: its own answer starts empty.
  createEffect(on(() => props.id, () => setOwn(""), { defer: true }));
  const send = async (body: string) => {
    const question = q();
    if (!question || !body.trim() || sending() || !link.live()) return;
    setSending(true);
    const ok = await app.actions.answer(question.id, body);
    setSending(false);
    if (!ok) return;
    // On to the next question, if any.
    const next = store.openQuestions().find((x) => x.id !== question.id);
    if (next) ui.updateDialog({ kind: "question", id: next.id });
    else ui.closeDialog();
  };
  const onKey = (e: KeyboardEvent) => {
    const t = e.target as HTMLElement;
    if (t instanceof HTMLTextAreaElement) return;
    const n = Number(e.key);
    const opts = q() ? options(q()!) : [];
    if (n >= 1 && n <= opts.length && !e.ctrlKey && !e.altKey && !e.metaKey) {
      e.preventDefault();
      void send(opts[n - 1]);
    }
  };
  const step = (by: number) => {
    const list = open();
    if (list.length < 2) return;
    const next = list[(index() + by + list.length) % list.length];
    ui.updateDialog({ kind: "question", id: next.id });
  };
  return (
    <Modal title={open().length > 1 ? `question ${index() + 1} of ${open().length}` : "question"} label="question" class="question-dialog" onClose={() => ui.closeDialog()} initial=".options button, textarea" refocus={() => [props.id, open().length, sending(), link.live()]}>
      <Show when={q()} fallback={<><p>This question has been answered.</p><div class="dialog-actions"><button onClick={() => ui.closeDialog()}>close</button></div></>}>
        {(question) => (
          <div onKeyDown={onKey}>
            <p class="asker"><b>{asker(question())}</b> <span class="muted">· {fmtElapsed(store.serverNow(Date.now()) - question().timestamp)} ago</span></p>
            <pre class="body question-body"><Linked text={question().body} /></pre>
            <Show when={options(question()).length}>
              <ol class="options">
                <For each={options(question())}>
                  {(o, i) => <li><button disabled={!link.live() || sending()} onClick={() => void send(o)}><kbd>{i() + 1}</kbd> {o}</button></li>}
                </For>
              </ol>
            </Show>
            <label class="own">your own answer
              <textarea rows="3" value={own()} onInput={(e) => setOwn(e.currentTarget.value)} disabled={!link.live()}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    if (own().trim().startsWith("/")) return ui.setStatus("that is a command, not an answer — esc closes this box first", true);
                    void send(own());
                  }
                }} />
            </label>
            <div class="dialog-actions">
              <Show when={open().length > 1}>
                <button onClick={() => step(-1)}>previous</button>
                <button onClick={() => step(1)}>next question</button>
              </Show>
              <span class="spacer" />
              <button onClick={() => ui.closeDialog()} title="esc: later; a reopens it">later</button>
              <button class="primary" disabled={!link.live() || sending() || !own().trim()} onClick={() => void send(own())}>send</button>
            </div>
          </div>
        )}
      </Show>
    </Modal>
  );
}

function ModelDialog(props: { agent: string }) {
  const app = useApp();
  const { store, ui, link } = app;
  const agent = () => store.agent(props.agent);
  const [models] = createResource(() => (link.live() ? true : null), () => link.call("models_list", undefined, decodeModels));
  const tabs: RuntimeKind[] = RUNTIME_KIND.filter((r) => r !== "shell");
  const [runtime, setRuntime] = createSignal<RuntimeKind>(agent()?.runtime && agent()!.runtime !== "shell" ? agent()!.runtime : "claude");
  const [notice, setNotice] = createSignal<string | null>(null);
  const rows = createMemo(() => ((models.error ? undefined : models())?.models ?? []).filter((m) => m.runtime === runtime()));
  const current = (m: ModelChoice) => {
    const a = agent();
    return !!a && a.runtime === m.runtime && (a.model ?? "").toLowerCase() === m.model.toLowerCase();
  };
  const choose = (m: ModelChoice) => {
    const a = agent();
    if (!a) return setNotice(`${props.agent} is no longer on the team`);
    if (m.unavailable) return setNotice(`${m.name} cannot be started here: ${m.unavailable}`);
    if (current(m)) {
      ui.setStatus(`${a.id} already runs on ${m.name}`);
      return ui.closeDialog();
    }
    const label = m.runtime === a.runtime
      ? `restarting ${a.id} on ${m.name}, resuming its session`
      : `moving ${a.id} to ${runtimeLabel(m.runtime)} on ${m.name}: a fresh session there, handed the work`;
    ui.closeDialog();
    void app.actions.setModel(a.id, m.model, m.runtime, label);
  };
  const tabKey = (e: KeyboardEvent) => {
    const i = tabs.indexOf(runtime());
    const n = e.key === "ArrowRight" ? i + 1 : e.key === "ArrowLeft" ? i - 1 : ["1", "2", "3"].includes(e.key) ? Number(e.key) - 1 : null;
    if (n === null) return;
    e.preventDefault();
    setRuntime(tabs[(n + tabs.length) % tabs.length]);
    setNotice(null);
  };
  return (
    <Modal title={`${props.agent} · model and CLI`} label="model and CLI" class="model-dialog" onClose={() => ui.closeDialog()} initial="[role=tab][aria-selected=true]">
      <div class="tabs" role="tablist" aria-label="CLI" onKeyDown={tabKey}>
        <For each={tabs}>
          {(r, i) => (
            <button role="tab" aria-selected={runtime() === r} tabindex={runtime() === r ? 0 : -1} classList={{ on: runtime() === r }} onClick={() => (setRuntime(r), setNotice(null))}>
              <kbd>{i() + 1}</kbd> {runtimeLabel(r)}{agent()?.runtime === r ? " ●" : ""}
            </button>
          )}
        </For>
      </div>
      <Show when={!models.loading} fallback={<p class="muted"><span class="spin">◐</span> asking the daemon what can be started here…</p>}>
        <Show when={!models.error} fallback={<p class="err">the models could not be listed: {models.error?.message ?? String(models.error)}</p>}>
          <ul class="models" role="listbox" aria-label={`${runtimeLabel(runtime())} models`} onKeyDown={(e) => {
            const items = [...e.currentTarget.querySelectorAll<HTMLButtonElement>("button")];
            const i = items.indexOf(document.activeElement as HTMLButtonElement);
            const step = e.key === "ArrowDown" || e.key === "j" ? 1 : e.key === "ArrowUp" || e.key === "k" ? -1 : 0;
            if (!step || items.length === 0) return;
            e.preventDefault();
            items[Math.min(items.length - 1, Math.max(0, i + step))].focus();
          }}>
            <For each={rows()} fallback={<li class="muted">no {runtimeLabel(runtime())} models are known</li>}>
              {(m) => (
                <li>
                  <button role="option" aria-selected={current(m)} classList={{ now: current(m), unavailable: !!m.unavailable }} disabled={!link.live()} onClick={() => choose(m)}
                    title={m.unavailable ? `Cannot be started here: ${m.unavailable}` : m.summary}>
                    <span class="mark">{current(m) ? "●" : ""}</span>
                    <span class="name">{m.name}</span>
                    <span class="tier muted">{m.tier}</span>
                    <span class="price muted">{m.price}</span>
                    <span class="summary">{m.unavailable ? "not here" : m.summary}</span>
                    <span class="now-label">{current(m) ? "now" : ""}</span>
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>
      <p class="muted">{agent() && runtime() === agent()!.runtime ? "same CLI: the session is restarted on the model, resumed where it was" : `another CLI: a fresh ${runtimeLabel(runtime())} session there, handed where the work stands`}</p>
      <Show when={notice()}><p class="warn" role="alert">{notice()}</p></Show>
      <div class="dialog-actions"><span class="spacer" /><button onClick={() => ui.closeDialog()}>cancel</button></div>
    </Modal>
  );
}

function ConfirmDialog(props: { confirm: Confirm }) {
  const app = useApp();
  const { ui, link, actions } = app;
  const text = (): { title: string; body: string; yes: string; run(): void } => {
    const c = props.confirm;
    switch (c.kind) {
      case "kill":
        return { title: "kill agent", body: `Stop ${c.agent}'s process? Its task stays open.`, yes: `kill ${c.agent}`, run: () => void actions.kill(c.agent) };
      case "restart-fresh":
        return { title: `restart ${c.agent} fresh · no resume`, body: "The old conversation is not resumed.", yes: "restart fresh", run: () => void actions.restartFresh(c.agent) };
      case "restart-paused":
        return {
          title: "restart paused agents",
          body: `Restart ${c.count} agent(s) paused at a usage limit? They start afresh on the login or key now in place and resume their conversations.`,
          yes: "restart",
          run: () => void actions.resumePaused(true),
        };
      case "stop":
        return { title: "stop the team", body: "Stop every agent and the crew daemon? Committed work stays on the agents' branches.", yes: "stop the team", run: () => void actions.stopTeam() };
    }
  };
  return (
    <Modal title={text().title} label={text().title} class="confirm-dialog" onClose={() => ui.closeDialog()} initial="button.cancel">
      <p>{text().body}</p>
      <div class="dialog-actions">
        <span class="spacer" />
        <button class="cancel" onClick={() => ui.closeDialog()}>cancel</button>
        <button class="danger" disabled={!link.live()} onClick={() => {
          const t = text();
          ui.closeDialog();
          t.run();
        }}>{text().yes}</button>
      </div>
    </Modal>
  );
}

export function HelpDialog() {
  const { ui, prefs } = useApp();
  const commands = COMMANDS;
  return (
    <Modal title="keys and commands" class="help-dialog" onClose={() => ui.closeDialog()}>
      <div class="help-cols">
        <table class="help-keys" aria-label="keys">
          <thead><tr><th>key</th><th>does</th><th>in the TUI</th></tr></thead>
          <tbody>
            <For each={KEYS}>{(k) => <tr data-help-key><td><kbd>{k.keys}</kbd></td><td>{k.action}</td><td class="muted">{k.tui}</td></tr>}</For>
          </tbody>
        </table>
        <table class="help-commands" aria-label="commands">
          <thead><tr><th>command</th><th>does</th></tr></thead>
          <tbody>
            <For each={commands}>{(c) => <tr data-help-command><td><code>{usage(c)}</code>{c.aliases.length ? <span class="muted"> ({c.aliases.map((a) => `/${a}`).join(", ")})</span> : null}</td><td>{c.web === "declined" ? `${c.about} — ${DECLINED}` : c.about}</td></tr>}</For>
          </tbody>
        </table>
      </div>
      <p class="muted">Single-key shortcuts are {prefs.prefs.keys ? "on" : "off"} (<code>/keys {prefs.prefs.keys ? "off" : "on"}</code>). <kbd>tab</kbd> moves through the page; every action is also a button.</p>
      <div class="dialog-actions"><span class="spacer" /><button onClick={() => ui.closeDialog()}>close</button></div>
    </Modal>
  );
}
