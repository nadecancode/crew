// One event of the log; a `vcs.*` event shows its `VcsEvent` fields
// (commit, change, source, branch, files).

import { For, Show } from "solid-js";
import { useApp } from "../app";
import { clockTime } from "../format";
import { vcsOf, type Event } from "../protocol";
import { actionName } from "../anim/anims";
import { Linked } from "../text";
import { AgentLink } from "./MessagePanel";
import { PanelHead } from "./SidePanel";

export function EventPanel(props: { seq: number }) {
  const app = useApp();
  const { store, ui } = app;
  const event = (): Event | undefined => {
    store.eventsVersion();
    for (const e of store.events.newest()) if (e.seq === props.seq) return e;
    return undefined;
  };
  const copy = (v: string) => navigator.clipboard?.writeText(v).then(() => ui.setStatus(`copied ${v.slice(0, 12)}`), () => {});
  return (
    <div class="panel-body event-panel">
      <PanelHead title={`event ${props.seq}`} />
      <Show when={event()} fallback={<p class="pad muted">not in the page's log any more</p>}>
        {(e) => {
          const v = () => vcsOf(e());
          return (
            <div class="pad scroll" tabindex="0" data-focus-first>
              <dl class="facts">
                <dt>time</dt><dd>{clockTime(e().timestamp)}</dd>
                <dt>level</dt><dd class={`lv-${e().level}`}>{e().level}</dd>
                <dt>type</dt><dd class="mono">{e().event_type}</dd>
                <dt>source</dt><dd>{e().source}</dd>
                <Show when={e().agent_id}><dt>agent</dt><dd><AgentLink id={e().agent_id!} /></dd></Show>
                <Show when={e().task_id}><dt>task</dt><dd><button class="link" onClick={() => ui.select({ kind: "task", id: e().task_id! })}>{e().task_id}</button></dd></Show>
              </dl>
              <pre class="body"><Linked text={e().message} /></pre>
              <Show when={v()}>
                {(x) => (
                  <dl class="facts vcs-facts">
                    <dt>action</dt><dd>{actionName(x().action)}</dd>
                    <dt>outcome</dt><dd class={`out-${x().outcome}`}>{x().outcome}</dd>
                    <dt>operation</dt><dd class="mono">{x().op} · {x().backend}</dd>
                    <dt>by</dt><dd>{x().agent}{x().peer ? ` ← ${x().peer}` : ""}</dd>
                    <Show when={x().commit}><dt>commit</dt><dd><button class="link mono" title="copy" onClick={() => copy(x().commit!)}>{x().commit}</button></dd></Show>
                    <Show when={x().change}><dt>change</dt><dd class="mono">{x().change}</dd></Show>
                    <Show when={x().source}><dt>source</dt><dd class="mono">{x().source}</dd></Show>
                    <Show when={x().branch}><dt>branch</dt><dd class="mono">{x().branch}</dd></Show>
                    <Show when={x().files.length}><dt>files</dt><dd><ul class="files"><For each={x().files}>{(f) => <li class="mono">{f}</li>}</For></ul></dd></Show>
                    <Show when={x().summary}><dt>summary</dt><dd>{x().summary}</dd></Show>
                  </dl>
                )}
              </Show>
            </div>
          );
        }}
      </Show>
    </div>
  );
}
