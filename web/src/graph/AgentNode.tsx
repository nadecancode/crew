// One box of the agent graph: what the TUI's box shows
// (widgets/agent_box.rs): runtime glyph and colour, id, the task's title,
// what the agent is doing and for how long, progress, the context gauge, the
// model, and the badges (unread inbox, `?`, paused, usage limit, `temp`,
// in-session subagents `⑂N`).

import { Show, createMemo } from "solid-js";
import { useApp } from "../app";
import { useSecond } from "../clock";
import { agentIcon, badge, doing, progressOf, providerError, runningLabel, runtimeGlyph, stateTone, statusTail, windowSize, workLine, workOf } from "../format";
import type { Agent } from "../protocol";
import type { Box } from "./view";

type NodeEvents = {
  onDown(e: PointerEvent): void;
  onMove(e: PointerEvent): void;
  onUp(e: PointerEvent): void;
  onKey(e: KeyboardEvent): void;
  onHover(e: PointerEvent): void;
  onLeave(): void;
  onQuestion(): void;
  onModel(): void;
  onMenu(e: MouseEvent): void;
  /** Double click: the inspector on its terminal tab. */
  onOpen(): void;
};

const ACTIONS = ["agent_pause", "agent_resume", "agent_restart", "agent_restart_fresh", "agent_kill", "agent_set_model"];

export function AgentNode(props: NodeEvents & {
  agent: Agent;
  box: Box;
  satellite: boolean;
  selected: boolean;
  tabStop: boolean;
  asking: boolean;
  onTask(task: string, e: MouseEvent): void;
  onTaskHover(task: string): (e: PointerEvent) => void;
}) {
  const app = useApp();
  const second = useSecond();
  const a = () => props.agent;
  const serverNow = () => (second(), app.store.serverNow(Date.now()));
  const work = createMemo(() => workOf(app.store.state.tasks, a().id, serverNow()));
  const line = () => workLine(a(), work());
  const what = () => doing(a(), work(), serverNow());
  const progress = () => progressOf(a());
  const pending = () => ACTIONS.some((m) => app.isPending(m, a().id));
  const model = () => runningLabel(a());
  const ctx = () => a().context;
  const unread = () => a().inbox_unread;

  return (
    <g
      class={`node ${props.satellite ? "satellite" : "member"} st-${a().status}`}
      classList={{ selected: props.selected, paused: a().paused, provider: !!providerError(a()), limited: !!a().limit, asking: props.asking }}
      data-agent={a().id}
      role="button"
      aria-label={`${a().id}, ${a().status}`}
      tabindex={props.tabStop ? 0 : -1}
      transform={`translate(${props.box.x} ${props.box.y})`}
      onPointerDown={props.onDown}
      onPointerMove={props.onMove}
      onPointerUp={props.onUp}
      onPointerCancel={props.onUp}
      onPointerEnter={props.onHover}
      onPointerLeave={props.onLeave}
      onKeyDown={props.onKey}
      onDblClick={(e) => {
        e.stopPropagation();
        props.onOpen();
      }}
      onContextMenu={props.onMenu}
    >
      <rect class="box" width={props.box.w} height={props.box.h} rx="7" />
      <foreignObject width={props.box.w} height={props.box.h}>
        <div class="node-body">
          <div class="node-head">
            <span class={`glyph rt-${a().runtime}`}>{props.satellite ? "↳" : runtimeGlyph(a().runtime)}</span>
            <span class={`id rt-${a().runtime}`}>{a().id}</span>
            <Show when={props.asking}>
              <button class="badge ask" title="answer the question" onPointerDown={(e) => e.stopPropagation()} onClick={(e) => (e.stopPropagation(), props.onQuestion())} tabindex="-1">?</button>
            </Show>
            <Show when={unread() > 0}><span class="badge unread" title={`${unread()} unread`}>✉{unread()}</span></Show>
            <Show when={a().limit}><span class="badge limit" title={a().limit!.message}>limit</span></Show>
            <Show when={a().paused}><span class="badge paused">‖</span></Show>
            <Show when={badge(a())}><span class="badge">{badge(a())}</span></Show>
            <Show when={pending()}><span class="badge pending" title="waiting for the daemon">…</span></Show>
          </div>
          <Show when={!props.satellite}>
            <div class="node-row">
              <span class={`icon ${stateTone(a())}`} classList={{ spin: a().status === "working" && !a().paused }}>{props.asking ? "?" : agentIcon(a().status, a().paused)}</span>
              <Show when={work()} fallback={<span class="work" classList={{ current: line().current }}>{line().text}</span>}>
                <span class="work task-link" classList={{ current: line().current }} data-task-link={work()!.task.id}
                  onPointerDown={(e) => e.stopPropagation()} onClick={(e) => props.onTask(work()!.task.id, e)} onPointerEnter={props.onTaskHover(work()!.task.id)}>{line().text}</span>
              </Show>
              <span class={`tail ${stateTone(a())}`}>{statusTail(a())}</span>
            </div>
          </Show>
          <div class={`node-row doing tone-${what().tone}`}>
            <span class="what">{what().text}</span>
            <span class="time">{what().time}</span>
          </div>
          <Show when={!props.satellite}>
            <div class="node-row foot">
              <Show when={ctx()} fallback={<Show when={progress() !== null}><span class="bar"><span class="fill" style={{ width: `${progress()}%` }} /></span></Show>}>
                <span class="ctx" title={`context ${ctx()!.percent}% of ${windowSize(ctx()!.window)}`}>
                  {windowSize(ctx()!.window)} <span class="bar gauge"><span class="fill" style={{ width: `${ctx()!.percent}%` }} /></span> {ctx()!.percent}%
                </span>
              </Show>
              <Show when={model()}>
                <button class="model" classList={{ fallback: !!a().fallback }} tabindex="-1" onPointerDown={(e) => e.stopPropagation()} onClick={(e) => (e.stopPropagation(), props.onModel())}>{model()}</button>
              </Show>
            </div>
          </Show>
        </div>
      </foreignObject>
    </g>
  );
}

/** The hub: the orchestrator, or an empty hub when the team has none. */
export function HubNode(props: NodeEvents & { box: Box; agent: Agent | undefined; asking: boolean; tabStop: boolean; selected: boolean }) {
  const app = useApp();
  const done = () => app.store.state.tasks.filter((t) => t.status === "done").length;
  const total = () => app.store.state.tasks.length;
  const questions = () => app.store.openQuestions().length;
  const complete = () => !!app.store.state.session?.completed_at;
  const a = () => props.agent;
  const state = () => {
    if (questions() > 0) return { icon: "?", text: questions() === 1 ? "your answer needed" : `${questions()} answers needed`, tone: "warn" };
    if (complete()) return { icon: "✓", text: "all tasks done", tone: "ok" };
    const ag = a();
    if (!ag) return { icon: "·", text: "scheduler", tone: "muted" };
    return { icon: agentIcon(ag.status, ag.paused), text: ag.paused ? "paused" : ag.status, tone: stateTone(ag) };
  };
  const model = () => (a() ? runningLabel(a()!) : null);
  return (
    <g
      class="node hub"
      classList={{ selected: props.selected, asking: questions() > 0, empty: !a() }}
      data-agent={a()?.id}
      role={a() ? "button" : undefined}
      aria-label={a() ? `${a()!.id}, ${a()!.status}` : "hub"}
      tabindex={a() ? (props.tabStop ? 0 : -1) : undefined}
      transform={`translate(${props.box.x} ${props.box.y})`}
      onPointerDown={props.onDown}
      onPointerMove={props.onMove}
      onPointerUp={props.onUp}
      onPointerCancel={props.onUp}
      onPointerEnter={props.onHover}
      onPointerLeave={props.onLeave}
      onKeyDown={props.onKey}
      onDblClick={(e) => (e.stopPropagation(), props.onOpen())}
      onContextMenu={props.onMenu}
    >
      <rect class="box" width={props.box.w} height={props.box.h} rx="9" />
      <foreignObject width={props.box.w} height={props.box.h}>
        <div class="node-body">
          <div class="node-head">
            <span class="glyph orch">◆</span>
            <span class="id orch">{a()?.id ?? "orchestrator"}</span>
            <Show when={questions() > 0}>
              <button class="badge ask" tabindex="-1" onPointerDown={(e) => e.stopPropagation()} onClick={(e) => (e.stopPropagation(), props.onQuestion())}>?</button>
            </Show>
          </div>
          <div class="node-row">
            <span class="orch">status</span><span class="dim"> · </span><b>{done()}/{total()}</b>
          </div>
          <div class={`node-row doing ${state().tone}`}>
            <span class={`icon ${state().tone}`} classList={{ spin: a()?.status === "working" && !a()?.paused && questions() === 0 }}>{state().icon}</span>
            <span class="what">{state().text}</span>
          </div>
          <Show when={model()}>
            <div class="node-row foot">
              <span class={`glyph rt-${a()!.runtime}`}>{runtimeGlyph(a()!.runtime)}</span>
              <button class="model" tabindex="-1" onPointerDown={(e) => e.stopPropagation()} onClick={(e) => (e.stopPropagation(), props.onModel())}>{model()}</button>
            </div>
          </Show>
        </div>
      </foreignObject>
    </g>
  );
}
