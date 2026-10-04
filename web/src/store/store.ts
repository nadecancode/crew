// The page's copy of the daemon's state, and the reducers that keep it
// (notes/web-design.md §transport "A snapshot is a barrier", §perf).
//
// Agents, tasks, the session and the open questions are a Solid store
// reconciled by id: a push that changes one field of one agent rewrites one
// text node. Messages and events are rings outside the reactive graph with
// one version signal each. Version control events drive the animation state
// (`anims`), with its own version signal.

import { batch, createSignal } from "solid-js";
import { createStore, reconcile, unwrap } from "solid-js/store";
import { Anims } from "../anim/anims";
import {
  AGENT_ERROR_EVENT,
  AGENT_REFUSED_EVENT,
  OPERATOR_ID,
  asker,
  isOperatorQuestion,
  vcsOf,
  type Agent,
  type Event,
  type Message,
  type Push,
  type SessionInfo,
  type Snapshot,
  type Task,
  type VcsEvent,
} from "../protocol";
import { isOrchestrator, satelliteParent, sortAgents } from "./order";
import { Ring } from "./ring";

export const MAX_MESSAGES = 1000;
export const MAX_EVENTS = 5000;
/** A VCS event older than this when it arrives is history: it plays nothing. */
export const VCS_FRESH_MS = 10_000;
/** Points of each agent's context line in the usage view. */
export const USAGE_POINTS = 600;

export type State = {
  loaded: boolean;
  session: SessionInfo | null;
  agents: Agent[];
  tasks: Task[];
  /** Questions agents put to the operator that nobody answered yet, oldest first. */
  questions: Message[];
  /** Questions whose answer is on its way. */
  answering: string[];
  /** The daemon's clock minus the page's, from the last snapshot. */
  clockOffset: number;
};

/** What the store tells the page about: the TUI's status line and notices. */
export type Notices = {
  status(text: string, error: boolean): void;
  notice(text: string): void;
  agentRemoved(id: string): void;
};

const quiet: Notices = { status() {}, notice() {}, agentRemoved() {} };

export type UsagePoint = { t: number; percent: number };

export function createState(notices: Notices = quiet) {
  const [state, setState] = createStore<State>({
    loaded: false,
    session: null,
    agents: [],
    tasks: [],
    questions: [],
    answering: [],
    clockOffset: 0,
  });
  const messages = new Ring<Message>(MAX_MESSAGES);
  const events = new Ring<Event>(MAX_EVENTS);
  const [messagesVersion, setMessagesVersion] = createSignal(0);
  const [eventsVersion, setEventsVersion] = createSignal(0);
  const anims = new Anims();
  const [animsVersion, setAnimsVersion] = createSignal(0);
  const usage = new Map<string, Ring<UsagePoint>>();
  const [usageVersion, setUsageVersion] = createSignal(0);
  const bumpMessages = () => setMessagesVersion((v) => v + 1);
  const bumpEvents = () => setEventsVersion((v) => v + 1);
  const bumpAnims = () => setAnimsVersion((v) => v + 1);

  const serverNow = (now: number) => now + state.clockOffset;
  const workers = () => state.agents.filter((a) => !isOrchestrator(a)).map((a) => a.id);

  function sample(agent: Agent, now: number): void {
    const c = agent.context;
    if (!c) return;
    let ring = usage.get(agent.id);
    if (!ring) usage.set(agent.id, (ring = new Ring(USAGE_POINTS)));
    const last = ring.last();
    if (last && last.percent === c.percent && now - last.t < 1000) return;
    ring.push({ t: now, percent: c.percent });
    setUsageVersion((v) => v + 1);
  }

  /** After a snapshot: what still runs shows as running, nothing else. */
  function resumeVcs(now: number): void {
    const server = serverNow(now);
    const parsed: [VcsEvent, number][] = [];
    for (const e of events.toArray()) {
      const vcs = vcsOf(e);
      if (!vcs) continue;
      vcs.seq = e.seq;
      parsed.push([vcs, Math.max(0, server - e.timestamp)]);
    }
    anims.resume(parsed, now);
    bumpAnims();
  }

  /** The reply to `snapshot` replaces everything (rule 2 of the barrier). */
  function applySnapshot(snap: Snapshot, now: number): void {
    batch(() => {
      setState("clockOffset", snap.server_time - now);
      setState("session", reconcile(snap.session));
      setState("agents", reconcile(sortAgents(snap.agents), { key: "id" }));
      setState("tasks", reconcile(snap.tasks, { key: "id" }));
      setState("questions", reconcile(snap.questions, { key: "id" }));
      setState("answering", (ids) => ids.filter((id) => snap.questions.some((q) => q.id === id)));
      messages.reset(snap.messages);
      events.reset(snap.events);
      for (const a of snap.agents) sample(a, now);
      resumeVcs(now);
      setState("loaded", true);
      bumpMessages();
      bumpEvents();
    });
    // Agents that left while the page was away.
    for (const id of [...usage.keys()]) if (!snap.agents.some((a) => a.id === id)) usage.delete(id);
  }

  function upsertAgent(agent: Agent, now: number): void {
    const i = state.agents.findIndex((a) => a.id === agent.id);
    if (i < 0) {
      setState("agents", reconcile(sortAgents([...unwrap(state.agents), agent]), { key: "id" }));
    } else {
      const reparented = (state.agents[i].parent ?? null) !== (agent.parent ?? null);
      setState("agents", i, reconcile(agent));
      if (reparented) setState("agents", reconcile(sortAgents(unwrap(state.agents)), { key: "id" }));
    }
    sample(agent, now);
  }

  function removeAgent(id: string): void {
    if (!state.agents.some((a) => a.id === id)) return;
    // Subagents of a removed parent rejoin the ring until they go too.
    setState("agents", reconcile(sortAgents(unwrap(state.agents).filter((a) => a.id !== id)), { key: "id" }));
    anims.removeAgent(id);
    usage.delete(id);
    bumpAnims();
    notices.agentRemoved(id);
  }

  function upsertTask(task: Task): void {
    const i = state.tasks.findIndex((t) => t.id === task.id);
    if (i < 0) setState("tasks", state.tasks.length, task);
    else setState("tasks", i, reconcile(task));
  }

  function addMessage(msg: Message, now: number): void {
    // A snapshot can overlap the push stream: the newest 64 are checked.
    for (const m of messages.newest(64)) if (m.id === msg.id) return;
    if (isOperatorQuestion(msg)) {
      if (!state.questions.some((q) => q.id === msg.id)) {
        notices.status(`? ${asker(msg)} has a question for you`, false);
        setState("questions", state.questions.length, msg);
      }
    } else if (msg.from === OPERATOR_ID && msg.type === "answer" && msg.reply_to) {
      // Answered, here or from another terminal.
      const id = msg.reply_to;
      setState("questions", (qs) => qs.filter((q) => q.id !== id));
      setState("answering", (ids) => ids.filter((x) => x !== id));
    }
    anims.onMessage(msg, workers(), now);
    bumpAnims();
    messages.push(msg);
    bumpMessages();
  }

  function addEvent(event: Event, now: number): void {
    const last = events.last();
    const fresh = !last || event.seq === 0 || event.seq > last.seq;
    if (!fresh) return;
    const vcs = vcsOf(event);
    if (vcs) {
      vcs.seq = event.seq;
      // Too late to play, not too late to end what is still shown as running.
      if (serverNow(now) - event.timestamp > VCS_FRESH_MS) anims.endOp(vcs.op);
      else anims.onVcs(vcs, now);
      bumpAnims();
    }
    if (event.event_type === AGENT_ERROR_EVENT || event.event_type === AGENT_REFUSED_EVENT) notices.notice(event.message);
    events.push(event);
    bumpEvents();
  }

  /** One push, in the order received (rule 3 of the barrier). Returns false
   * for a push the store does not handle (the connection layer's). */
  function applyPush(push: Push, now: number): boolean {
    switch (push.kind) {
      case "agent":
        upsertAgent(push.data, now);
        return true;
      case "agent_removed":
        removeAgent(push.data.id);
        return true;
      case "task":
        upsertTask(push.data);
        return true;
      case "message":
        addMessage(push.data, now);
        return true;
      case "event":
        addEvent(push.data, now);
        return true;
      case "session":
        setState("session", reconcile(push.data));
        return true;
      default:
        return false;
    }
  }

  /** Older events from `events` ("load older"), put in front of the ring. */
  function prependEvents(older: Event[]): void {
    const first = events.at(0);
    const keep = (first ? older.filter((e) => e.seq < first.seq) : [...older]).sort((a, b) => a.seq - b.seq);
    if (keep.length === 0) return;
    events.prepend(keep);
    bumpEvents();
  }

  function prependMessages(older: Message[]): void {
    const have = new Set(messages.toArray().map((m) => m.id));
    const first = messages.at(0);
    const keep = older.filter((m) => !have.has(m.id) && (!first || m.timestamp <= first.timestamp)).sort((a, b) => a.timestamp - b.timestamp);
    if (keep.length === 0) return;
    messages.prepend(keep);
    bumpMessages();
  }

  function setAnswering(id: string, on: boolean): void {
    setState("answering", (ids) => (on ? (ids.includes(id) ? ids : [...ids, id]) : ids.filter((x) => x !== id)));
  }

  return {
    state,
    messages,
    events,
    anims,
    usage,
    messagesVersion,
    eventsVersion,
    animsVersion,
    usageVersion,
    bumpAnims,
    serverNow,
    applySnapshot,
    applyPush,
    prependEvents,
    prependMessages,
    setAnswering,
    // Queries.
    agent: (id: string) => state.agents.find((a) => a.id === id),
    orchestrator: () => state.agents.find(isOrchestrator),
    satelliteParent: (a: Agent) => satelliteParent(state.agents, a),
    openQuestions: () => state.questions.filter((q) => !state.answering.includes(q.id)),
  };
}

export type Store = ReturnType<typeof createState>;
