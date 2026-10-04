// The page and the workspace on screen (notes/web-multi.md §multi-ui).
//
// The page (`createPage`): its dashboard server's index connection (the
// listing, the operator's preferences, the page's own state), the status
// line and notices, which workspace is on screen, and switching.
//
// The workspace on screen (`createApp`): its daemon's state, the connection
// that keeps it, its route, what the page shows besides, and the operator's
// actions (notes/web-design.md §ui "Operator actions"). A switch disposes it
// and makes a new one: no agent, message or terminal of one workspace is ever
// drawn under the name of another. Components reach it through `useApp()`.

import { createContext, createSignal, useContext } from "solid-js";
import type * as TerminalChunk from "./term/Terminal";
import { now } from "./clock";
import { CallError, type Env } from "./net/conn";
import { createHub, type Hub } from "./net/hub";
import type { Session } from "./net/session";
import { createWorkspaceLink } from "./net/workspace";
import { createLayoutPrefs, joinPrefs } from "./prefs";
import {
  BROADCAST,
  decodeAgentOrNull,
  decodeSendResult,
  type Agent,
  type Push,
  type RuntimeKind,
  type WorkspaceEntry,
} from "./protocol";
import { formatRoute, parseRoute } from "./route";
import { createState } from "./store/store";
import { createFrames } from "./graph/frames";
import type { ViewportApi } from "./graph/Viewport";
import { createChrome, createUi, type Chrome, type Place } from "./ui";

/** Timings for §perf, read by e2e/perf.spec.ts. No state and no credential. */
export type PerfLog = {
  apply: number[];
  toFrame: number[];
  lag: number[];
  /** When the first snapshot was applied and the frame after it presented (ms since the navigation). */
  live: number | null;
  /** Switches: from the switch to the first frame with the new workspace's snapshot applied (P17). */
  switches: number[];
};

export type TerminalModule = typeof TerminalChunk;

type Loc = { hash: string };
type Hist = { pushState(data: unknown, unused: string, url?: string): void; replaceState(data: unknown, unused: string, url?: string): void };

/** What a switcher row calls a workspace. */
export function workspaceName(e: { id: string; project?: string | null; workspace: string; summary?: { project?: string | null } | null }): string {
  return e.project || e.summary?.project || e.workspace.split("/").filter(Boolean).pop() || e.id;
}

export type Page = {
  hub: Hub;
  chrome: Chrome;
  terminal: Promise<TerminalModule>;
  perf: PerfLog;
  /** The switcher's list is open. */
  switcher: () => boolean;
  setSwitcher: (open: boolean) => void;
  /** The workspace on screen. */
  app: () => App;
  /** Put workspace `id` on screen; `push` makes a history entry. */
  switchTo(id: string | null, how: { push: boolean; place?: Place }): void;
  /** Back and forward. */
  fromLocation(): void;
  /** For e2e/perf.spec.ts: start or stop recording timings. */
  measure(on: boolean): void;
  readonly measuring: boolean;
  /** Joined workspaces other than the one on screen. */
  others(): WorkspaceEntry[];
  start(): void;
  /** Try again now (the tab became visible, the browser went online). */
  poke(): void;
};

/** `terminal` is the terminal chunk, requested by the entry module when the
 * document started (no code is fetched later). */
export function createPage(env: Env, session: Session, terminal: Promise<TerminalModule>, o: { loc?: Loc; hist?: Hist } = {}): Page {
  terminal.catch(() => {});
  const loc = o.loc ?? location;
  const hist = o.hist ?? history;
  const chrome = createChrome();
  const hub = createHub(env, session, { note: (text) => chrome.setStatus(text) });
  const perf: PerfLog = { apply: [], toFrame: [], lag: [], live: null, switches: [] };
  let measuring = false;
  /** Each workspace's route when it was left, for this document. */
  const places = new Map<string, Place>();
  const [switcher, setSwitcher] = createSignal(false);

  const start = parseRoute(loc.hash);
  /** Whether the workspace on screen was chosen: the address named one, or
   * the first listing (with the preferences) was there to choose from. */
  let chosen = start.workspace !== null;

  const page: Page = {
    hub,
    chrome,
    terminal,
    perf,
    switcher,
    setSwitcher,
    app: () => app(),
    switchTo,
    fromLocation,
    measure(on: boolean) {
      measuring = on;
      if (on) perf.apply.length = perf.toFrame.length = perf.lag.length = perf.switches.length = 0;
    },
    get measuring() {
      return measuring;
    },
    others: (): WorkspaceEntry[] => (hub.listing()?.joined ?? []).filter((e) => e.id !== app().link.id),
    start() {
      hub.start();
      app().start();
    },
    poke() {
      app().link.poke();
    },
  };

  const [app, setApp] = createSignal<App>(createApp(page, start.workspace, { view: start.view, sel: start.sel }));

  /** In this order: the address (already taken), `last` of the preferences,
   * the first joined workspace that is running, the first joined. */
  function choose(): string | null {
    const joined = hub.listing()?.joined ?? [];
    const last = hub.prefs.prefs.last;
    if (last && joined.some((e) => e.id === last)) return last;
    return (joined.find((e) => e.state === "running") ?? joined[0])?.id ?? null;
  }

  /** Put workspace `id` on screen. `push`: a history entry, so that the
   * back button returns to the workspace left. */
  function switchTo(id: string | null, how: { push: boolean; place?: Place }): void {
    const cur = app();
    if (cur.link.id === id && !how.place) return;
    if (cur.link.id === id && how.place) return cur.ui.setRoute(how.place, how.push);
    const t0 = performance.now();
    if (cur.link.id !== null) places.set(cur.link.id, cur.ui.route());
    cur.dispose();
    const place = how.place ?? (id !== null ? places.get(id) : undefined) ?? { view: "graph" as const, sel: null };
    const next = createApp(page, id, place);
    if (id !== null) {
      const hash = formatRoute({ workspace: id, ...place });
      if (loc.hash !== hash) (how.push ? hist.pushState : hist.replaceState).call(hist, null, "", hash);
      if (hub.entry(id)) hub.prefs.setLast(id);
    }
    next.switchedAt = t0;
    setApp(next);
    next.start();
  }

  /** Back and forward: the address names the workspace. */
  function fromLocation(): void {
    const r = parseRoute(loc.hash);
    const cur = app();
    if (r.workspace === null) {
      // A route without a workspace is this one's.
      hist.replaceState(null, "", formatRoute({ ...r, workspace: cur.link.id }));
      return cur.ui.fromLocation();
    }
    if (r.workspace === cur.link.id) return cur.ui.fromLocation();
    switchTo(r.workspace, { push: false, place: { view: r.view, sel: r.sel } });
  }

  hub.onLive(() => {
    if (chosen) {
      // The workspace on screen is the one last on screen.
      const id = app().link.id;
      if (id !== null && hub.entry(id)) hub.prefs.setLast(id);
      return;
    }
    chosen = true;
    const id = choose();
    // The address keeps its view and selection; only the workspace is added.
    if (id !== null) switchTo(id, { push: false, place: app().ui.route() });
  });

  // A question in another workspace: one notice, with a way there. The
  // summary holds counts only, so no text of one workspace shows in another.
  const questions = new Map<string, number>();
  hub.onListing(() => {
    for (const e of hub.listing()?.joined ?? []) {
      const n = e.summary?.questions ?? 0;
      const before = questions.get(e.id);
      questions.set(e.id, n);
      if (before === undefined || n <= before || e.id === app().link.id) continue;
      chrome.notice(`${workspaceName(e)}: ${n === 1 ? "1 question is" : `${n} questions are`} waiting`, {
        label: "switch",
        run: () => switchTo(e.id, { push: true }),
      });
    }
  });

  return page;
}

/** The workspace `id` on screen (null: none is). */
export function createApp(page: Page, id: string | null, place: Place) {
  const { chrome, perf } = page;
  const ui = createUi({ workspace: id, place, chrome });
  const store = createState({
    status: (text, error) => ui.setStatus(text, error),
    notice: (text) => ui.notice(text),
    agentRemoved(agent) {
      // A removed agent's inspector closes; its neighbour keeps the keys.
      const sel = ui.sel();
      if (sel?.kind === "agent" && sel.id === agent) ui.select(null);
      if (ui.selected() === agent) ui.setSelected(null);
      if (ui.logFilter().agent === agent) ui.setLogFilter({ ...ui.logFilter(), agent: null });
    },
  });
  const [teamStopped, setTeamStopped] = createSignal(false);
  let switchedAt: number | null = null;
  const link = createWorkspaceLink(page.hub, id, {
    snapshot(snap, t) {
      store.applySnapshot(snap, t);
      setTeamStopped(false);
      const firstFrame = (f: (t: number) => void) => requestAnimationFrame(() => requestAnimationFrame(() => f(performance.now())));
      if (perf.live === null) firstFrame((t1) => (perf.live ??= t1));
      if (switchedAt !== null) {
        const t0 = switchedAt;
        switchedAt = null;
        if (page.measuring) firstFrame((t1) => perf.switches.push(t1 - t0));
      }
    },
    push: (push: Push, t: number) => store.applyPush(push, t),
    shutdown: () => setTeamStopped(true),
    now,
    measured(push, t0) {
      if (!page.measuring) return;
      const t1 = performance.now();
      perf.apply.push(t1 - t0);
      if (push.kind === "event") perf.lag.push(Date.now() - push.data.timestamp);
      requestAnimationFrame(() => requestAnimationFrame(() => perf.toFrame.push(performance.now() - t0)));
    },
  });
  const layout = createLayoutPrefs((method, params) => link.call(method, params));
  link.onLive(() => {
    if (!layout.loaded) void layout.load().then(() => page.hub.prefs.adopt(layout.legacy()), () => {});
  });
  const prefs = joinPrefs(layout, page.hub.prefs);

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
  const isPending = (method: string, key = "") => pending().has(`${method}:${key}`);

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

  async function answer(question: string, body: string): Promise<boolean> {
    if (!link.live()) {
      ui.setStatus("the answer was not sent: not connected", true);
      return false;
    }
    // The question is hidden from the call on: once answered it stays hidden
    // until the operator's answer message (or a snapshot without it) removes it.
    store.setAnswering(question, true);
    try {
      await link.call("answer", { id: question, body });
      ui.setStatus("✓ answer sent");
      return true;
    } catch (e) {
      // The question is still open: it shows again.
      store.setAnswering(question, false);
      ui.setStatus(`the answer was not sent: ${errorText(e)}`, true);
      return false;
    }
  }

  const agentCall = (method: string, label: string) => (agent: string) => quietly(act(`${method}:${agent}`, label.replace("{id}", agent), method, { id: agent }, decodeAgentOrNull));

  const actions = {
    send,
    answer,
    pause: agentCall("agent_pause", "pause {id}"),
    resume: agentCall("agent_resume", "resume {id}"),
    restart: agentCall("agent_restart", "restart {id}"),
    restartFresh: agentCall("agent_restart_fresh", "restart {id} fresh (no resume)"),
    kill: agentCall("agent_kill", "kill {id}"),
    compact: (agent: string) => quietly(act(`agent_compact:${agent}`, `compaction of ${agent}'s context queued`, "agent_compact", { id: agent })),
    focus: (agent: string) => quietly(act(`agent_focus:${agent}`, `focus ${agent}`, "agent_focus", { id: agent })),
    togglePause(agent: Agent) {
      return agent.paused ? actions.resume(agent.id) : actions.pause(agent.id);
    },
    setModel(agent: string, model: string, runtime: RuntimeKind, label: string) {
      ui.setStatus(`${label}…`);
      return quietly(act(`agent_set_model:${agent}`, label, "agent_set_model", { id: agent, model, runtime }));
    },
    resumePaused(restart: boolean) {
      const label = restart ? "restarted the paused agents on the current login" : "told the paused agents to continue";
      return quietly(act(`resume:${restart}`, label, "resume", { agents: [], restart }));
    },
    /** A prompt typed for an agent: submitted as one (paste, then Enter).
     * True once the daemon took it. */
    async prompt(agent: string, text: string): Promise<boolean> {
      const submit = text.length > 0;
      const label = `typed into ${agent}`;
      if (!link.live()) {
        ui.setStatus(`${label}: not connected`, true);
        return false;
      }
      return act(`agent_input:${agent}`, label, "agent_input", { id: agent, text: submit ? text : "\r", submit }).then(() => true, () => false);
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
    page,
    ui,
    store,
    link,
    frames,
    views,
    terminal: page.terminal,
    prefs,
    actions,
    isPending,
    teamStopped,
    perf,
    /** When the switch that made it began, until its first snapshot (P17). */
    set switchedAt(t: number | null) {
      switchedAt = t;
    },
    start: () => link.start(),
    /** Left: its connection closed, its frames and timers gone. */
    dispose() {
      layout.flush();
      link.dispose();
      frames.dispose();
    },
  };
}

export type App = ReturnType<typeof createApp>;

/** Questions waiting and agents needing attention in the joined workspaces
 * not on screen: the switcher's badges. */
export function otherCounts(page: Page): { questions: number; attention: number } {
  let questions = 0, attention = 0;
  for (const e of page.others()) {
    questions += e.summary?.questions ?? 0;
    attention += e.summary?.attention ?? 0;
  }
  return { questions, attention };
}

/** The workspace's name on screen: its team's, else the listing's. */
export function shownName(app: App): string | null {
  const e = app.page.hub.entry(app.link.id);
  return app.store.state.session?.project ?? (e ? workspaceName(e) : null);
}

/** The document's title: `<project> · crew`, with the questions waiting in
 * any joined workspace in front, `(3) payments · crew`. */
export function pageTitle(app: App): string {
  const here = app.store.state.loaded ? app.store.openQuestions().length : app.page.hub.entry(app.link.id)?.summary?.questions ?? 0;
  const n = here + otherCounts(app.page).questions;
  const project = shownName(app);
  return `${n > 0 ? `(${n}) ` : ""}${project ? `${project} · ` : ""}crew`;
}

export const AppContext = createContext<App>();

export function useApp(): App {
  const app = useContext(AppContext);
  if (!app) throw new Error("useApp outside AppContext");
  return app;
}
