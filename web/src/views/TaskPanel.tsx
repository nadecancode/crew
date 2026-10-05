// A task's card (notes/web-design.md §ui "Tasks"): `task_get` when the panel
// opens and on a `task` push for that task. Description, status, assignee,
// reviewer, progress note, result, artifacts (a commit id copies on click),
// and the tasks it builds on as links.

import { For, Show, createEffect, createSignal, on, onCleanup } from "solid-js";
import { useApp } from "../app";
import { absoluteTime, fmtElapsed, taskIcon, taskTone } from "../format";
import { decodeTaskCard, type TaskCard } from "../protocol";
import { Linked } from "../text";
import { AgentLink } from "./MessagePanel";
import { PanelHead } from "./SidePanel";
import { waiting } from "./waiting";

export function TaskPanel(props: { id: string }) {
  const app = useApp();
  const { store, ui, link } = app;
  const [card, setCard] = createSignal<TaskCard | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const local = () => store.state.tasks.find((t) => t.id === props.id);
  let active = true;
  onCleanup(() => (active = false));
  const fetch = async () => {
    if (loading() || !link.live()) return;
    const id = props.id;
    setLoading(true);
    try {
      const next = await link.call("task_get", { id }, decodeTaskCard);
      if (!active || props.id !== id) return;
      setCard(next);
      setError(null);
    } catch (e) {
      if (active && props.id === id) setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (active) setLoading(false);
    }
  };
  // On open, and on every `task` push for it (its fields change).
  createEffect(on(() => [props.id, JSON.stringify(local() ?? null), link.live()], () => void fetch()));
  createEffect(on(app.ui.refreshTick, () => void fetch(), { defer: true }));
  const task = () => local() ?? (card()?.task.id === props.id ? card()!.task : undefined);
  const dates = (): [string, number | null | undefined][] => {
    const t = task();
    return t ? [["created", t.created_at], ["started", t.started_at], ["completed", t.completed_at]].filter(([, value]) => value != null) as [string, number][] : [];
  };
  const copy = (value: string) => navigator.clipboard?.writeText(value).then(() => ui.setStatus(`copied ${value}`), () => ui.setStatus("could not copy", true));
  return (
    <div class="panel-body task-panel" aria-busy={loading()}>
      <PanelHead title={`task ${props.id}`}>
        <button onClick={() => ui.setView("tasks")} title="the task graph (t)">graph</button>
      </PanelHead>
      <Show when={loading() || error()}>
        <div class="fetch-state" role="status">
          {loading() ? "Loading task details…" : <><span class="err">{error()}</span> <button disabled={!link.live()} onClick={fetch}>Retry</button></>}
        </div>
      </Show>
      <Show when={task()} fallback={<p class="pad muted">{error() ?? waiting(link, "loading…")}</p>}>
        {(t) => (
          <div class="pad scroll" tabindex="0" data-focus-first>
            <h3 class="task-title">{t().title}</h3>
            <dl class="facts">
              <dt>status</dt><dd class={taskTone(t().status)}>{taskIcon(t().status)} {t().status}{t().progress != null ? ` · ${t().progress}%` : ""}</dd>
              <dt>assignee</dt><dd>{t().assigned_agent ? <AgentLink id={t().assigned_agent!} /> : "—"}</dd>
              <Show when={t().reviewer}><dt>reviewer</dt><dd><AgentLink id={t().reviewer!} /></dd></Show>
              <Show when={t().note}><dt>note</dt><dd><Linked text={t().note!} /></dd></Show>
            </dl>
            <Show when={t().description}><pre class="body"><Linked text={t().description} /></pre></Show>
            <Show when={t().dependencies.length}>
              <h4>builds on</h4>
              <ul class="deps">
                <For each={t().dependencies}>
                  {(id) => {
                    const dep = () => store.state.tasks.find((x) => x.id === id) ?? card()?.dependencies.find((d) => d.id === id);
                    return (
                      <li>
                        <button class="link" onClick={() => ui.select({ kind: "task", id })}>{id}</button>{" "}
                        <Show when={dep()} fallback={<span class="muted">unresolved</span>}><span class={taskTone(dep()!.status)}>{dep()!.status}</span> <span class="muted">{dep()!.result?.split("\n", 1)[0]}</span></Show>
                      </li>
                    );
                  }}
                </For>
              </ul>
            </Show>
            <Show when={t().result}>
              <h4>result</h4>
              <pre class="body"><Linked text={t().result!} /></pre>
            </Show>
            <Show when={t().artifacts.length}>
              <h4>artifacts</h4>
              <ul class="artifacts">
                <For each={t().artifacts}>
                  {(a) => (
                    <li>
                      <span class="muted">{a.kind}</span>{" "}
                      <button class="link mono" title={`Copy ${a.kind}`} onClick={() => copy(a.value)}>{a.value}</button>
                      <Show when={a.description}> <span>{a.description}</span></Show>
                      <div class="muted artifact-meta"><Show when={a.agent}>By <AgentLink id={a.agent!} /> · </Show>{absoluteTime(a.created_at)}</div>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
            <h4>timeline</h4>
            <dl class="facts">
              <For each={dates()}>{([label, ms]) => <><dt>{label}</dt><dd>{absoluteTime(ms!)}</dd></>}</For>
              <Show when={t().started_at != null && t().completed_at != null}><dt>elapsed</dt><dd>{fmtElapsed(t().completed_at! - t().started_at!)}</dd></Show>
              <Show when={t().timeout_secs != null}><dt>timeout</dt><dd>{t().timeout_secs}s</dd></Show>
            </dl>
          </div>
        )}
      </Show>
    </div>
  );
}
