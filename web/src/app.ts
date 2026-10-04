// The page: the daemon's state, the connection that keeps it, the
// preferences, what the page shows besides, and the operator's actions
// (notes/web-design.md §ui "Operator actions"). Components reach it through
// `useApp()`.

import { createContext, createSignal, useContext } from "solid-js";
import type * as TerminalChunk from "./term/Terminal";
import { now } from "./clock";
import { CallError, type Env } from "./net/conn";
import type { Session } from "./net/session";
import { createLink } from "./net/state";
import { createPrefs } from "./prefs";
import {
  BROADCAST,
  decodeAgentOrNull,
  decodeSendResult,
  type Agent,
  type Push,
  type RuntimeKind,
} from "./protocol";
import { createState } from "./store/store";
import { createFrames } from "./graph/frames";
import type { ViewportApi } from "./graph/Viewport";
import { createUi } from "./ui";

/** Timings for §perf, read by e2e/perf.spec.ts. No state and no credential. */
export type PerfLog = { apply: number[]; toFrame: number[]; lag: number[] };

export type TerminalModule = typeof TerminalChunk;

/** `terminal` is the terminal chunk, requested by the entry module when the
 * document started (no code is fetched later). */
export function createApp(env: Env, session: Session, terminal: Promise<TerminalModule>) {
  terminal.catch(() => {});
  const ui = createUi();
  const store = createState({
    status: (text, error) => ui.setStatus(text, error),
    notice: (text) => ui.notice(text),
    agentRemoved(id) {
      // A removed agent's inspector closes; its neighbour keeps the keys.
      const sel = ui.sel();
      if (sel?.kind === "agent" && sel.id === id) ui.select(null);
      if (ui.selected() === id) ui.setSelected(null);
      if (ui.logFilter().agent === id) ui.setLogFilter({ ...ui.logFilter(), agent: null });
    },
  });
  const perf: PerfLog = { apply: [], toFrame: [], lag: [] };
  const [teamStopped, setTeamStopped] = createSignal(false);
  let measuring = false;
  const link = createLink(
    env,
    session,
    {
      snapshot: (snap, t) => store.applySnapshot(snap, t),
      push: (push: Push, t: number) => store.applyPush(push, t),
      shutdown: () => setTeamStopped(true),
      now,
      measured(push, t0) {
        if (!measuring) return;
        const t1 = performance.now();
        perf.apply.push(t1 - t0);
        if (push.kind === "event") perf.lag.push(Date.now() - push.data.timestamp);
        requestAnimationFrame(() => requestAnimationFrame(() => perf.toFrame.push(performance.now() - t0)));
      },
    },
    { note: (text) => ui.setStatus(text) },
  );
  const prefs = createPrefs((method, params) => link.call(method, params));
  link.onLive(() => {
    if (!prefs.loaded) void prefs.load().catch(() => {});
  });

  // ---- actions --------------------------------------------------------------

  /** Controls waiting for the daemon: `method:id`. No optimistic state: the
   * page changes only when the daemon says so. */
  const [pending, setPending] = createSignal<ReadonlySet<string>>(new Set());
  const mark = (key: string, on: boolean) =>
    setPending((s) => {
      const n = new Set(s);
      if (on) n.add(key);
      else n.delete(key);
      return n;
    });
  const isPending = (method: string, id = "") => pending().has(`${method}:${id}`);

  const errorText = (e: unknown) => (e instanceof CallError ? e.message : e instanceof Error ? e.message : String(e));

  /** Run one action: a pending mark while it is out, `✓ label` or
   * `label: <daemon's message>` when it is back, as the TUI words it. */
  async function act<T>(key: string, label: string, method: string, params: unknown, decode?: (v: unknown) => T): Promise<T | undefined> {
    if (!link.live()) {
      ui.setStatus(`${label}: not connected`, true);
      return undefined;
    }
    mark(key, true);
    try {
      const out = await link.call(method, params, decode);
      ui.setStatus(`✓ ${label}`);
      return out;
    } catch (e) {
      ui.setStatus(`${label}: ${errorText(e)}`, true);
      throw e;
    } finally {
      mark(key, false);
    }
  }
  const quietly = <T>(p: Promise<T>) => p.catch(() => undefined);

  const newMessageId = () => `w-${Array.from(crypto.getRandomValues(new Uint8Array(8)), (b) => b.toString(16).padStart(2, "0")).join("")}`;

  /** A message or broadcast. The page always sets the id: a message whose
   * reply was lost to a dropped connection is sent once more after the
   * reconnect, which the daemon takes as a no-op if it had arrived. */
  async function send(to: string, body: string): Promise<boolean> {
    const params = { to, type: "message", body, id: newMessageId() };
    const label = to === BROADCAST ? "broadcast sent" : `message sent to ${to}`;
    if (!link.live()) {
      ui.setStatus(`${label}: not connected`, true);
      return false;
    }
    mark("send:", true);
    try {
      try {
        await link.call("send", params, decodeSendResult);
      } catch (e) {
        if (!(e instanceof CallError) || e.code !== "closed") throw e;
        await new Promise<void>((resolve, reject) => {
          const off = link.onLive(() => {
            off();
            resolve();
          });
          window.setTimeout(() => {
            off();
            reject(new CallError("closed", "the connection did not come back"));
          }, 30_000);
        });
        await link.call("send", params, decodeSendResult);
      }
      ui.setStatus(`✓ ${label}`);
      return true;
    } catch (e) {
      ui.setStatus(`${label}: ${errorText(e)}`, true);
      return false;
    } finally {
      mark("send:", false);
    }
  }

  async function answer(id: string, body: string): Promise<boolean> {
    store.setAnswering(id, true);
    try {
      await link.call("answer", { id, body });
      store.setAnswering(id, false);
      ui.setStatus("✓ answer sent");
      return true;
    } catch (e) {
      // The question is still open: it shows again.
      store.setAnswering(id, false);
      ui.setStatus(`the answer was not sent: ${errorText(e)}`, true);
      return false;
    }
  }

  const agentCall = (method: string, label: string) => (id: string) => quietly(act(`${method}:${id}`, label.replace("{id}", id), method, { id }, decodeAgentOrNull));

  const actions = {
    send,
    answer,
    pause: agentCall("agent_pause", "pause {id}"),
    resume: agentCall("agent_resume", "resume {id}"),
    restart: agentCall("agent_restart", "restart {id}"),
    restartFresh: agentCall("agent_restart_fresh", "restart {id} fresh (no resume)"),
    kill: agentCall("agent_kill", "kill {id}"),
    compact: (id: string) => quietly(act(`agent_compact:${id}`, `compaction of ${id}'s context queued`, "agent_compact", { id })),
    focus: (id: string) => quietly(act(`agent_focus:${id}`, `focus ${id}`, "agent_focus", { id })),
    togglePause(agent: Agent) {
      return agent.paused ? actions.resume(agent.id) : actions.pause(agent.id);
    },
    setModel(id: string, model: string, runtime: RuntimeKind, label: string) {
      ui.setStatus(`${label}…`);
      return quietly(act(`agent_set_model:${id}`, label, "agent_set_model", { id, model, runtime }));
    },
    resumePaused(restart: boolean) {
      const label = restart ? "restarted the paused agents on the current login" : "told the paused agents to continue";
      return quietly(act(`resume:${restart}`, label, "resume", { agents: [], restart }));
    },
    /** A prompt typed for an agent: submitted as one (paste, then Enter). */
    prompt(id: string, text: string) {
      const submit = text.length > 0;
      return quietly(act(`agent_input:${id}`, `typed into ${id}`, "agent_input", { id, text: submit ? text : "\r", submit }));
    },
    async stopTeam() {
      if (!link.live()) return ui.setStatus("stopping the team: not connected", true);
      mark("shutdown:", true);
      ui.setStatus("stopping the team…");
      try {
        await link.call("shutdown", { stop_agents: true });
      } catch (e) {
        // The daemon may be gone before its answer got through; its
        // `shutdown` push says so.
        if (!teamStopped()) ui.setStatus(`stopping the team: ${errorText(e)}`, true);
      } finally {
        mark("shutdown:", false);
      }
    },
  };

  const frames = createFrames(store, { reduced: typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches });
  /** The viewports of the graph and the DAG, for the zoom keys. */
  const views: { graph?: ViewportApi; tasks?: ViewportApi } = {};

  return {
    ui,
    store,
    link,
    frames,
    views,
    terminal,
    prefs,
    actions,
    isPending,
    teamStopped,
    perf,
    /** For e2e/perf.spec.ts: start or stop recording timings. */
    measure(on: boolean) {
      measuring = on;
      if (on) perf.apply.length = perf.toFrame.length = perf.lag.length = 0;
    },
  };
}

export type App = ReturnType<typeof createApp>;

export const AppContext = createContext<App>();

export function useApp(): App {
  const app = useContext(AppContext);
  if (!app) throw new Error("useApp outside AppContext");
  return app;
}
