import { batch, createSignal } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";

export const MAX_MESSAGES = 500;
export const MAX_EVENTS = 5000;

export type Agent = { id: string; role: string; status: string; activity: string; orchestrator?: boolean; parent?: string | null; progress?: number | null; task?: string | null; runtime: string };
export type Ev = { seq: number; timestamp: number; source: string; event_type: string; agent_id?: string | null; message: string; level: string; data?: any };
export type Msg = { id: string; from: string; to: string; type: string; body: string; timestamp: number };

export const [state, setState] = createStore({ agents: [] as Agent[], tasks: [] as any[], session: {} as any });
// Logs: a plain ring outside the reactive graph, with one version signal the
// virtual list reads. A push costs O(1) whatever the log's length.
export const events: Ev[] = [];
export const messages: Msg[] = [];
export const [eventsVersion, bumpEvents] = createSignal(0);
export const [messagesVersion, bumpMessages] = createSignal(0);
export type Pulse = { from: string; to: string; kind: string; started: number };
export const [pulses, setPulses] = createSignal<Pulse[]>([]);

export function applySnapshot(s: any) {
  batch(() => {
    setState("session", reconcile(s.session));
    setState("agents", reconcile(s.agents, { key: "id" }));
    setState("tasks", reconcile(s.tasks, { key: "id" }));
    events.length = 0; events.push(...s.events.slice(-MAX_EVENTS));
    messages.length = 0; messages.push(...s.messages.slice(-MAX_MESSAGES));
    bumpEvents((v) => v + 1); bumpMessages((v) => v + 1);
  });
}

export function applyPush(p: { kind: string; data?: any }) {
  switch (p.kind) {
    case "agent": {
      const i = state.agents.findIndex((a) => a.id === p.data.id);
      if (i < 0) setState("agents", state.agents.length, p.data);
      else setState("agents", i, reconcile(p.data));
      break;
    }
    case "agent_removed": setState("agents", (as) => as.filter((a) => a.id !== p.data.id)); break;
    case "task": {
      const i = state.tasks.findIndex((t) => t.id === p.data.id);
      if (i < 0) setState("tasks", state.tasks.length, p.data);
      else setState("tasks", i, reconcile(p.data));
      break;
    }
    case "session": setState("session", reconcile(p.data)); break;
    case "event": {
      const last = events[events.length - 1];
      if (last && p.data.seq !== 0 && p.data.seq <= last.seq) break;
      events.push(p.data);
      if (events.length > MAX_EVENTS) events.splice(0, events.length - MAX_EVENTS);
      bumpEvents((v) => v + 1);
      break;
    }
    case "message": {
      messages.push(p.data);
      if (messages.length > MAX_MESSAGES) messages.shift();
      bumpMessages((v) => v + 1);
      const now = performance.now();
      const from = p.data.from === "operator" ? "orchestrator" : p.data.from;
      const tos = p.data.to === "*" ? state.agents.filter((a) => a.id !== from).map((a) => a.id) : [p.data.to];
      setPulses((ps) => [...ps.filter((x) => now - x.started < 3000), ...tos.map((to) => ({ from, to, kind: "p2p", started: now }))].slice(-48));
      break;
    }
  }
}
export { produce };
