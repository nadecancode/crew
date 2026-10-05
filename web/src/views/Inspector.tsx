// An agent's inspector (notes/web-design.md §ui "Inspector"): a header with
// its facts and actions, and four tabs, each remembering its scroll position:
// overview (facts, tasks, messages with delivery state, problems), terminal,
// transcript, diff.
//
// No polling (§transport "No polling"): every fetch has a cause. The inbox:
// the inspector opening, an `agent` push whose inbox counts changed. The
// transcript: its tab opening, the refresh key, an `agent` push whose status
// changed, one request in flight at most. The diff: its tab opening, the
// refresh key, a `vcs.commit` event of the agent.

import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onMount, type JSX } from "solid-js";
import { useApp } from "../app";
import { useSecond } from "../clock";
import { agentIcon, clockTime, fmtElapsed, modelLabel, oneLine, runningLabel, runtimeGlyph, stateTone, statusTail, taskIcon, taskTone, tokens, workOf } from "../format";
import { decodeEvents, decodeGitOutcome, decodeInbox, decodeTranscript, isP2p, type Agent, type Event, type GitOutcome, type InboxEntry, type TranscriptEntry, type TranscriptResult } from "../protocol";
import { Linked } from "../text";
import type { InspectorTab } from "../ui";
import { useOps } from "./ops";
import { PanelHead } from "./SidePanel";
import { waiting } from "./waiting";
import { TerminalTab } from "./TerminalTab";
import { VirtualList } from "./VirtualList";

const TABS: [InspectorTab, string, string][] = [
  ["overview", "overview", ""],
  ["terminal", "terminal", ""],
  ["transcript", "transcript", ""],
  ["diff", "diff", "d"],
];

/** Scroll positions per agent and tab, for as long as the page lives. */
const scrolls = new Map<string, number>();

export function Inspector(props: { agent: Agent }) {
  const app = useApp();
  const ops = useOps();
  const { ui, link } = app;
  const a = () => props.agent;
  const second = useSecond();
  const serverNow = () => (second(), app.store.serverNow(Date.now()));
  const can = () => link.live();
  const pending = (m: string) => app.isPending(m, a().id);

  const tabKey = (e: KeyboardEvent) => {
    const i = TABS.findIndex(([t]) => t === ui.tab());
    const next = e.key === "ArrowRight" ? i + 1 : e.key === "ArrowLeft" ? i - 1 : e.key === "Home" ? 0 : e.key === "End" ? TABS.length - 1 : null;
    if (next === null) return;
    e.preventDefault();
    e.stopPropagation();
    const t = TABS[(next + TABS.length) % TABS.length][0];
    ui.setTab(t);
    queueMicrotask(() => document.getElementById(`tab-${t}`)?.focus());
  };

  return (
    <div class="panel-body inspector" data-inspector={a().id}>
      <PanelHead title={a().id} sub={a().role}>
        <button onClick={() => ui.setPanelFull(!ui.panelFull())} aria-pressed={ui.panelFull()} title="fill the main area (f)">{ui.panelFull() ? "dock" : "fill"}</button>
      </PanelHead>
      <div class="insp-head">
        <span class={`glyph rt-${a().runtime}`}>{runtimeGlyph(a().runtime)}</span>
        <span class={stateTone(a())}><span classList={{ spin: a().status === "working" && !a().paused }}>{agentIcon(a().status, a().paused)}</span> {a().paused ? "paused" : a().status} {statusTail(a()) !== a().status ? `· ${statusTail(a())}` : ""}</span>
        <Show when={runningLabel(a())}><button class="model link" onClick={() => ops.model(a().id)} title="model and CLI (s)">{runningLabel(a())}</button></Show>
        <Show when={a().branch}><span class="mono muted">{a().branch}</span></Show>
        <Show when={a().started_at}><span class="muted">up {fmtElapsed(serverNow() - a().started_at!)}</span></Show>
      </div>
      <div class="insp-actions" role="toolbar" aria-label={`actions for ${a().id}`}>
        <button class="primary" disabled={!can()} onClick={() => ops.compose(a().id)} title="message (m)">message</button>
        <button disabled={!can() || pending("agent_pause") || pending("agent_resume")} onClick={() => void app.actions.togglePause(a())} title="pause / resume (p)">{a().paused ? "resume" : "pause"}</button>
        <button aria-haspopup="menu" onClick={(e) => { const r = e.currentTarget.getBoundingClientRect(); ui.setMenu({ agent:a().id, x:r.left, y:r.bottom }); }}>More</button>
      </div>
      <div class="tabs" role="tablist" aria-label="inspector" onKeyDown={tabKey}>
        <For each={TABS}>
          {([t, label, key], i) => (
            <button id={`tab-${t}`} role="tab" aria-selected={ui.tab() === t} aria-controls={`tabpanel-${t}`} tabindex={ui.tab() === t ? 0 : -1}
              data-focus-first={i() === 0 ? true : undefined} classList={{ on: ui.tab() === t }} onClick={() => ui.setTab(t)} title={key ? `${label} (${key})` : label}>{label}</button>
          )}
        </For>
      </div>
      <div class="tab-body" role="tabpanel" id={`tabpanel-${ui.tab()}`} aria-labelledby={`tab-${ui.tab()}`}>
        <Switch>
          <Match when={ui.tab() === "overview"}><Overview agent={a()} /></Match>
          <Match when={ui.tab() === "terminal"}><TerminalTab agent={a()} /></Match>
          <Match when={ui.tab() === "transcript"}><Transcript agent={a()} /></Match>
          <Match when={ui.tab() === "diff"}><Diff agent={a()} /></Match>
        </Switch>
      </div>
    </div>
  );
}

/** A scroll container that keeps its position per agent and tab. */
function Scroll(props: { key: string; children: JSX.Element; class?: string }) {
  let el!: HTMLDivElement;
  onMount(() => (el.scrollTop = scrolls.get(props.key) ?? 0));
  return <div ref={el} class={`scroll ${props.class ?? ""}`} tabindex="0" onScroll={() => scrolls.set(props.key, el.scrollTop)}>{props.children}</div>;
}

function Facts(props: { rows:[string,string,string?][] }) {
  return <dl class="facts"><For each={props.rows}>{([key,value,tone]) => <><dt>{key}</dt><dd class={tone ?? ""}>{value}</dd></>}</For></dl>;
}

function Overview(props: { agent: Agent }) {
  const app = useApp();
  const { store, link, ui } = app;
  const a = () => props.agent;
  const second = useSecond();
  const serverNow = () => (second(), store.serverNow(Date.now()));
  const [inbox, setInbox] = createSignal<Map<string, InboxEntry>>(new Map());
  const [fetchedEvents, setFetchedEvents] = createSignal<Event[] | null>(null);

  const readInbox = () => {
    if (!link.live()) return;
    link.call("inbox", { agent: a().id, unread_only: false, ack: false, limit: 100 }, decodeInbox)
      .then((r) => setInbox(new Map(r.messages.map((m) => [m.id, m]))), () => {});
  };
  // The inspector opening; an `agent` push whose inbox counts changed.
  createEffect(on(() => [a().id, a().inbox_total, a().inbox_unread, ui.refreshTick(), link.live()], readInbox));
  // Once per panel (one panel per agent), when it is live.
  createEffect(on(link.live, (live) => {
    if (live && fetchedEvents() === null) link.call("events", { agent: a().id, limit: 200 }, decodeEvents).then(setFetchedEvents, () => {});
  }));

  const facts = (): [string, string, string?][] => {
    const ag = a();
    const out: [string, string, string?][] = [];
    out.push(["status", `${ag.status}${ag.paused ? " (paused)" : ""}${ag.block_kind ? ` · ${ag.block_kind}` : ""}${ag.progress != null ? ` · ${statusTail(ag)}` : ""}`, stateTone(ag)]);
    if (ag.error) out.push(["error", oneLine(ag.error), "err"]);
    out.push(["task", ag.task ?? "—"]);
    if (ag.activity) out.push(["activity", oneLine(ag.activity)]);
    out.push(["runtime", [ag.runtime, modelLabel(ag), ag.effort].filter(Boolean).join(" · ")]);
    if (ag.fallback) out.push(["fallback", `↓ on ${modelLabel(ag) ?? "another model"} instead of ${ag.fallback.from}${ag.fallback.note ? ` — ${ag.fallback.note}` : ""}`, "warn"]);
    out.push(["backend", ag.backend_ref ? `${ag.backend} · ${ag.backend_ref}` : ag.backend]);
    if (ag.delivery) out.push(["delivery", ag.delivery]);
    if (ag.parent) out.push(["parent", ag.parent]);
    const kids = store.state.agents.filter((x) => x.parent === ag.id).map((x) => `${x.id} (${x.status})`);
    if (kids.length) out.push(["subagents", kids.join(", ")]);
    if (ag.subagents > 0) out.push(["in-session", `${ag.subagents} subagents`]);
    out.push(["pid", ag.pid != null ? (ag.exit_code != null ? `${ag.pid} (exited ${ag.exit_code})` : String(ag.pid)) : ag.exit_code != null ? `exited ${ag.exit_code}` : "—"]);
    out.push(
      ["workdir", ag.workdir],
      ["branch", ag.branch ?? "—"],
      ["session", ag.session_id ?? "—"],
      ["started", ag.started_at != null ? `${clockTime(ag.started_at)} · up ${fmtElapsed(serverNow() - ag.started_at)}` : "not started"],
      ["last seen", `${fmtElapsed(serverNow() - ag.last_activity_at)} ago`],
      ["restarts", String(ag.restarts)],
      ["mailbox", `${ag.inbox_unread} unread of ${ag.inbox_total} · ${ag.outbox_total} sent`],
    );
    if (ag.usage) {
      out.push(
        ["tokens", `${tokens(ag.usage.input_tokens)} in · ${tokens(ag.usage.output_tokens)} out · ${ag.usage.turns} turns`],
        ["cache", `${tokens(ag.usage.cache_read_tokens)} read · ${tokens(ag.usage.cache_creation_tokens)} written`],
      );
    } else out.push(["tokens", "not reported", "dim"]);
    if (ag.context) {
      const c = ag.context;
      out.push(["context", c.window > 0 ? `${c.percent}% full · ${tokens(c.tokens)} of ${tokens(c.window)}` : `${tokens(c.tokens)} tokens (window size not reported)`, c.percent >= 90 ? "err" : c.percent >= 75 ? "warn" : undefined]);
    }
    if (ag.compact_pending || ag.compactions) out.push(["compact", ag.compact_pending ? (ag.compactions ? `queued · done ${ag.compactions}× so far` : "queued for the end of this turn") : `done ${ag.compactions}×`]);
    if (ag.limit) out.push(["limit", `${ag.limit.kind}: ${oneLine(ag.limit.message)}`, "warn"]);
    return out;
  };

  const work = createMemo(() => workOf(store.state.tasks, a().id, serverNow()));
  const process = ["backend","delivery","parent","subagents","in-session","pid","workdir","session","restarts","compact"];
  const tasks = () => store.state.tasks.filter((t) => t.assigned_agent === a().id || t.reviewer === a().id);
  const messages = createMemo(() => {
    store.messagesVersion();
    const id = a().id;
    const out = [];
    for (const m of store.messages.newest()) {
      if (m.from === id || m.to === id || m.to === "*") out.push(m);
      if (out.length === 30) break;
    }
    return out.reverse();
  });
  const problems = createMemo(() => {
    store.eventsVersion();
    const id = a().id;
    const relevant = (e: Event) => e.agent_id === id && (e.level === "warn" || e.level === "error");
    const live = store.events.toArray().filter(relevant);
    const fetched = fetchedEvents();
    if (!fetched) return live.slice(-30);
    const last = Math.max(0, ...fetched.map((e) => e.seq));
    return [...fetched.filter(relevant), ...live.filter((e) => e.seq > last)].slice(-30);
  });

  return (
    <Scroll key={`${a().id}:overview`} class="overview">
      <section class="current-work"><h3>Current work</h3><Show when={work()}>{(w) => <button class="link current-title" onClick={() => ui.select({kind:"task",id:w().task.id})}>{w().task.title}</button>}</Show><Facts rows={facts().filter(([key]) => ["status","task","activity"].includes(key))} /></section>
      <section><h3>Session</h3><Facts rows={facts().filter(([key]) => !process.includes(key) && !["status","task","activity","error"].includes(key))} /></section>
      <section>
      <h3>Problems</h3>
      <ul class="insp-problems">
        <Show when={a().error}><li class="err">{oneLine(a().error!)}</li></Show>
        <For each={problems()} fallback={<Show when={!a().error}><li class="dim">none</li></Show>}>
          {(e) => (
            <li>
              <button class="link" onClick={() => ui.setRoute({ view: "logs", sel: { kind: "event", seq: e.seq } })}>
                <span class="dim">{clockTime(e.timestamp)}</span> <span class={`lv-${e.level}`}>{e.level}</span> <span class="muted">{e.event_type}</span> <Linked text={oneLine(e.message)} />
              </button>
            </li>
          )}
        </For>
      </ul>
      </section><section>
      <h3>Messages</h3>
      <ul class="insp-messages">
        <For each={messages()} fallback={<li class="dim">no messages yet</li>}>
          {(m) => {
            const out = m.from === a().id;
            const mark = () => {
              if (out) return "";
              const d = inbox().get(m.id);
              return d?.acked_at ? "Read" : d?.delivered_at ? "Delivered" : "";
            };
            return (
              <li>
                <button class="link msg-line" onClick={() => ui.select({ kind: "message", id: m.id })}>
                  <span class="dim">{clockTime(m.timestamp)}</span> <span class={isP2p(m) ? "accent" : "orch"}>{out ? "→" : "←"} {out ? m.to : m.from}</span>{" "}
                  <span class="ok" title={mark()}>{mark()}</span> <span class="muted">{m.type}</span> {oneLine(m.body).slice(0, 140)}
                </button>
              </li>
            );
          }}
        </For>
      </ul>
      </section><section>
      <h3>Assigned and review tasks</h3>
      <ul class="insp-tasks">
        <For each={tasks()} fallback={<li class="dim">no tasks assigned</li>}>
          {(t) => (
            <li>
              <button class="link" onClick={() => ui.select({ kind: "task", id: t.id })}><span class={taskTone(t.status)}>{taskIcon(t.status)}</span> <b>{t.id}</b></button>
              <span class={taskTone(t.status)}> {t.status}</span>
              <Show when={t.reviewer === a().id && t.assigned_agent !== a().id}><span class="orch"> (reviewing)</span></Show>
              <Show when={t.dependencies.length}><span class="muted"> ← {t.dependencies.join(", ")}</span></Show>
              <div class="muted">{t.title}</div>
              <Show when={t.note}><div class={taskTone(t.status)}>{oneLine(t.note!)}</div></Show>
            </li>
          )}
        </For>
      </ul>
      </section>
      <details class="process-details"><summary>Process and delivery details</summary><Facts rows={facts().filter(([key]) => process.includes(key))} /></details>
    </Scroll>
  );
}

const TRANSCRIPT_ROW = 22;

function Transcript(props: { agent: Agent }) {
  const app = useApp();
  const { link, ui } = app;
  const a = () => props.agent;
  const [result, setResult] = createSignal<TranscriptResult | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [cursor, setCursor] = createSignal<number | null>(null);
  let inFlight = false;
  const read = () => {
    if (inFlight || !link.live()) return;
    inFlight = true;
    link.call("agent_transcript", { id: a().id, limit: 400 }, decodeTranscript)
      .then((r) => (setResult(r), setError(null)), (e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => (inFlight = false));
  };
  // The tab opening, the refresh key, a turn that ended (the status changed).
  createEffect(on(() => [a().id, a().status, ui.refreshTick(), link.live()], read));
  const entries = () => result()?.entries ?? [];
  const chosen = (): TranscriptEntry | undefined => (cursor() !== null ? entries()[cursor()!] : undefined);
  return (
    <div class="transcript">
      <Show when={result() && error()}><p class="pad warn" role="status">{error()}</p></Show>
      <Show when={result()} fallback={<p class="pad muted">{error() ?? waiting(link, "reading the session…")}</p>}>
        <Show when={result()!.available} fallback={<p class="pad muted">session file unavailable for this runtime</p>}>
          <Show when={result()!.truncated}><p class="pad dim">older entries are not shown</p></Show>
          <Show when={!entries().length}><p class="pad muted">no transcript entries yet</p></Show>
          <VirtualList label="transcript" class="transcript-list" count={() => entries().length} slice={(f, t) => entries().slice(f, t)} rowHeight={TRANSCRIPT_ROW}
            selected={cursor} onSelect={setCursor} onOpen={setCursor}
            render={(e: TranscriptEntry) => (
              <div class={`tr-row k-${e.kind}`}>
                <span class="time">{e.at != null ? clockTime(e.at) : ""}</span>
                <span class="kind">{e.kind}</span>
                <span class="first">{oneLine(e.text).slice(0, 300)}</span>
              </div>
            )} />
          <Show when={chosen()}>
            <div class="tr-detail" tabindex="0"><pre class="body"><Linked text={chosen()!.text} /></pre></div>
          </Show>
        </Show>
      </Show>
    </div>
  );
}

function Diff(props: { agent: Agent }) {
  const app = useApp();
  const { link, store, ui } = app;
  const a = () => props.agent;
  const [diff, setDiff] = createSignal<GitOutcome | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  let inFlight = false;
  const read = () => {
    if (inFlight || !link.live()) return;
    inFlight = true;
    link.call("git_diff", { agent: a().id }, decodeGitOutcome)
      .then((r) => (setDiff(r), setError(null)), (e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => (inFlight = false));
  };
  // A commit of this agent: the newest `vcs.commit` event's seq.
  const lastCommit = createMemo(() => {
    store.eventsVersion();
    for (const e of store.events.newest(200)) if (e.event_type === "vcs.commit" && e.agent_id === a().id) return e.seq;
    return 0;
  });
  createEffect(on(() => [a().id, lastCommit(), ui.refreshTick(), link.live()], read));
  const lines = () => (diff()?.text ?? "").split("\n");
  return (
    <Scroll key={`${a().id}:diff`} class="diff">
      <Show when={diff() && error()}><p class="pad warn" role="status">{error()}</p></Show>
      <Show when={diff()} fallback={<p class="pad muted">{error() ?? waiting(link, "reading the diff…")}</p>}>
        <Show when={diff()!.text.trim()} fallback={<p class="pad muted">no uncommitted changes</p>}>
          <pre class="diff-text">
            <For each={lines()}>
              {(l) => <div class={l.startsWith("+++") || l.startsWith("---") ? "d-file" : l.startsWith("+") ? "d-add" : l.startsWith("-") ? "d-del" : l.startsWith("@@") ? "d-hunk" : l.startsWith("diff ") ? "d-file" : ""}>{l || " "}</div>}
            </For>
          </pre>
        </Show>
      </Show>
    </Scroll>
  );
}
