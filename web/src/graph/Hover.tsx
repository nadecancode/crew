// The card shown while the pointer rests on a node, a link, a pulse, a mark
// or a task (notes/web-design.md §graphs "Interaction").

import { For, Match, Show, Switch } from "solid-js";
import { useApp } from "../app";
import { fmtElapsed, oneLine, runningLabel, tokens } from "../format";
import { askedBy, type VcsEvent } from "../protocol";
import { actionName } from "../anim/anims";

export type HoverTarget =
  | { kind: "agent"; id: string }
  | { kind: "task"; id: string }
  | { kind: "pair"; a: string; b: string }
  | { kind: "message"; id: string }
  | { kind: "vcs"; event: VcsEvent };

export function Hover(props: { target: HoverTarget; x: number; y: number }) {
  const app = useApp();
  const { store } = app;
  const now = () => store.serverNow(Date.now());
  const firstLine = (s: string) => oneLine(s.split("\n", 1)[0]).slice(0, 160);
  return (
    <div class="hover-card" role="tooltip" style={{ left: `${props.x + 14}px`, top: `${props.y + 14}px` }}>
      <Switch>
        <Match when={props.target.kind === "agent" && store.agent((props.target as { id: string }).id)}>
          {(agent) => {
            const a = agent();
            const q = store.openQuestions().find((q) => (askedBy(q) ?? q.from) === a.id);
            return (
              <dl>
                <dt>{a.id}</dt><dd>{a.role}</dd>
                <dt>status</dt><dd>{a.paused ? "paused" : a.status} · active {fmtElapsed(now() - a.last_activity_at)} ago</dd>
                <Show when={a.task}><dt>task</dt><dd>{a.task}</dd></Show>
                <Show when={runningLabel(a)}><dt>model</dt><dd>{runningLabel(a)}</dd></Show>
                <Show when={a.context}><dt>context</dt><dd>{a.context!.percent}% of {tokens(a.context!.window)}</dd></Show>
                <Show when={a.usage}><dt>tokens</dt><dd>{tokens(a.usage!.input_tokens)} in · {tokens(a.usage!.output_tokens)} out · {a.usage!.turns} turns</dd></Show>
                <Show when={a.activity}><dt>doing</dt><dd>{firstLine(a.activity)}</dd></Show>
                <Show when={a.branch}><dt>branch</dt><dd class="mono">{a.branch}</dd></Show>
                <Show when={a.limit}><dt>limit</dt><dd class="warn">{firstLine(a.limit!.message)}</dd></Show>
                <Show when={a.error}><dt>error</dt><dd class="err">{firstLine(a.error!)}</dd></Show>
                <Show when={q}><dt>asks</dt><dd class="warn">{firstLine(q!.body)}</dd></Show>
              </dl>
            );
          }}
        </Match>
        <Match when={props.target.kind === "task" && store.state.tasks.find((t) => t.id === (props.target as { id: string }).id)}>
          {(task) => {
            const t = task();
            const waits = t.dependencies.filter((d) => store.state.tasks.find((x) => x.id === d)?.status !== "done");
            return (
              <dl>
                <dt>{t.id}</dt><dd>{t.title}</dd>
                <dt>status</dt><dd>{t.status}{t.progress != null ? ` · ${t.progress}%` : ""}</dd>
                <Show when={t.note}><dt>note</dt><dd>{firstLine(t.note!)}</dd></Show>
                <Show when={waits.length}><dt>waits for</dt><dd>{waits.join(", ")}</dd></Show>
              </dl>
            );
          }}
        </Match>
        <Match when={props.target.kind === "pair"}>
          {(() => {
            const { a, b } = props.target as { a: string; b: string };
            const between = () => {
              store.messagesVersion();
              const out = [];
              for (const m of store.messages.newest()) {
                if ((m.from === a && m.to === b) || (m.from === b && m.to === a) || ((m.from === "operator" || m.to === "operator") && (a === "orchestrator" || b === "orchestrator") && (m.from === a || m.to === a || m.from === b || m.to === b))) out.push(m);
                if (out.length === 3) break;
              }
              return out;
            };
            return (
              <div>
                <div class="hover-title">{a} ↔ {b}</div>
                <For each={between()} fallback={<div class="muted">no messages kept between them</div>}>
                  {(m) => <div class="hover-msg"><span class="muted">{m.from} → {m.to}</span> {firstLine(m.body)}</div>}
                </For>
              </div>
            );
          })()}
        </Match>
        <Match when={props.target.kind === "message"}>
          {(() => {
            const id = (props.target as { id: string }).id;
            const m = () => {
              store.messagesVersion();
              for (const x of store.messages.newest(200)) if (x.id === id) return x;
              return undefined;
            };
            return <Show when={m()} fallback={<span class="muted">{id}</span>}><div><span class="muted">{m()!.from} → {m()!.to} · {m()!.type}</span><br />{firstLine(m()!.body)}</div></Show>;
          })()}
        </Match>
        <Match when={props.target.kind === "vcs"}>
          {(() => {
            const e = (props.target as { event: VcsEvent }).event;
            return (
              <dl>
                <dt>{actionName(e.action)}</dt><dd class={e.outcome === "conflict" ? "err" : e.outcome === "error" ? "warn" : e.outcome === "ok" ? "ok" : "muted"}>{e.outcome} · {e.backend} · {e.op}</dd>
                <dt>agent</dt><dd>{e.agent}{e.peer ? ` ← ${e.peer}` : ""}</dd>
                <Show when={e.commit}><dt>commit</dt><dd class="mono">{e.commit}</dd></Show>
                <Show when={e.change}><dt>change</dt><dd class="mono">{e.change}</dd></Show>
                <Show when={e.source}><dt>source</dt><dd class="mono">{e.source}</dd></Show>
                <Show when={e.branch}><dt>branch</dt><dd class="mono">{e.branch}</dd></Show>
                <Show when={e.files.length}><dt>files</dt><dd class="mono">{e.files.join(", ")}</dd></Show>
                <Show when={e.summary}><dt>summary</dt><dd>{e.summary}</dd></Show>
              </dl>
            );
          })()}
        </Match>
      </Switch>
    </div>
  );
}
