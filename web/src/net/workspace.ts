// The workspace on screen: its state connection, through the dashboard
// server to its daemon (notes/web-multi.md §multi-ui "Two levels"), with the
// snapshot barrier of notes/web-design.md §transport:
//
// 1. From the moment `snapshot` is sent until its reply arrives, pushes are
//    discarded: they are older than the snapshot that is coming. That covers
//    the time between `subscribe` and the first snapshot, and `resync`.
// 2. The reply replaces agents, tasks, session, questions and both rings.
// 3. From then on every push is applied in the order received.
//
// What the workspace *is* comes from the listing, never from a connection.
// Its state is computed from the newest listing and from whether the page has
// a live connection to it:
//
//   the listing says               shown
//   not joined                     `removed` (`none` without an id)  no tries
//   `stopped`, `unreachable`       `not-running`                     no tries
//   `incompatible`                 `incompatible`                    no tries
//   `running`, a live connection   `live`
//   `running`, none                `connecting`, `reconnecting`      tries
//
// evaluated when a newer listing is applied, when the current attempt ends,
// and when the index connection becomes live.
//
// An attempt is one try to connect: a socket, its `hello`, and what follows.
// It records the entry's `generation` and `daemon` it was started under. Only
// the current attempt's reports are acted on. Every way it can end ends that
// attempt and nothing else; 4002, `not_found` and `protocol` say the listing
// may be stale, so the page asks for it first and then evaluates with the
// newest it has (an equal `rev` settles it as well). A failed proof and
// `unauthorized` are about the page's server: they stop the page.
//
// Tries: while the listing says `running`, after 250 ms doubling to 5 s
// (±20 %), without limit; at once, and from the first delay again, when the
// entry's `generation` or `daemon` differ from the last attempt's, or the
// workspace turns `running`.

import { createSignal } from "solid-js";
import { decodeSnapshot, type DaemonRef, type Push, type Snapshot, type WorkspaceEntry } from "../protocol";
import { CLOSE_TAKEN_OUT, CallError, Conn, backoff, type Ending, type StopReason } from "./conn";
import type { Hub } from "./hub";

export const SNAPSHOT_PARAMS = { messages: 300, events: 1000 };

/** `[data-workspace-state]`. */
export type Shown = "connecting" | "live" | "reconnecting" | "not-running" | "incompatible" | "removed" | "none";

export type Sink = {
  snapshot(snap: Snapshot, now: number): void;
  /** Applies a push; false when it is not the store's. */
  push(push: Push, now: number): boolean;
  /** The daemon's `shutdown` push arrived: its team stops. */
  shutdown(): void;
  now(): number;
  /** Measurements for §perf (handler time, push to frame). */
  measured?(push: Push, t0: number): void;
};

type Attempt = {
  n: number;
  conn: Conn;
  generation: number;
  daemon: DaemonRef | null;
  live: boolean;
  /** The page's half of the barrier: a snapshot is in flight. */
  inFlight: boolean;
};

const sameDaemon = (a: DaemonRef | null, b: DaemonRef | null) => a === b || (!!a && !!b && a.pid === b.pid && a.started_at === b.started_at);

/** Outcomes after which the listing may be out of date. */
function stale(ending: Ending | null): boolean {
  if (!ending) return false;
  if (ending.kind === "closed") return ending.code === CLOSE_TAKEN_OUT;
  return ending.kind === "refused" && (ending.code === "not_found" || ending.code === "protocol");
}

export type WorkspaceLink = ReturnType<typeof createWorkspaceLink>;

export function createWorkspaceLink(hub: Hub, id: string | null, sink: Sink) {
  const env = hub.env;
  const [shown, setShown] = createSignal<Shown>(id === null ? "none" : "connecting");
  /** Whether a listing ever had this workspace: `removed` or never joined. */
  const [seen, setSeen] = createSignal(false);
  const [detail, setDetail] = createSignal("");
  let attempt: Attempt | null = null;
  let attempts = 0;
  /** The backoff's step. */
  let tries = 0;
  let retry: number | null = null;
  /** The retry's delay is over: the next evaluation tries. */
  let due = false;
  /** What the last attempt recorded. */
  let recorded: { generation: number; daemon: DaemonRef | null } | null = null;
  let wasRunning = false;
  let asking = false;
  let everLive = false;
  let disposed = false;
  const liveListeners = new Set<() => void>();

  function clearRetry(): void {
    if (retry !== null) env.clearTimeout(retry);
    retry = null;
    due = false;
  }

  /** Close the current attempt, by the page's own decision: no outcome. */
  function closeAttempt(): void {
    const a = attempt;
    attempt = null;
    a?.conn.close();
  }

  function evaluate(): void {
    if (disposed) return;
    if (hub.stopped()) return halt();
    // While the index is lost no attempt is started and no listing acted on.
    if (!hub.live() || asking || !hub.listing()) return;
    const e: WorkspaceEntry | null = hub.entry(id);
    if (!e) {
      closeAttempt();
      clearRetry();
      wasRunning = false;
      setDetail("");
      return void setShown(id === null ? "none" : "removed");
    }
    setSeen(true);
    if (e.state !== "running") {
      closeAttempt();
      clearRetry();
      wasRunning = false;
      setDetail(e.detail);
      return void setShown(e.state === "incompatible" ? "incompatible" : "not-running");
    }
    setDetail("");
    const turned = !wasRunning;
    wasRunning = true;
    if (attempt) {
      if (attempt.live) setShown("live");
      return;
    }
    setShown(everLive ? "reconnecting" : "connecting");
    const changed = recorded !== null && (recorded.generation !== e.generation || !sameDaemon(recorded.daemon, e.daemon ?? null));
    if (turned || changed) {
      tries = 0;
      clearRetry();
      return begin(e);
    }
    if (retry !== null) return;
    if (recorded === null || due) {
      due = false;
      return begin(e);
    }
    retry = env.setTimeout(() => {
      retry = null;
      due = true;
      evaluate();
    }, backoff(tries++, env.random));
  }

  /** The page stopped: every connection closed, nothing requested again. */
  function halt(): void {
    closeAttempt();
    clearRetry();
  }

  async function snapshot(a: Attempt): Promise<void> {
    if (a.inFlight) return;
    a.inFlight = true;
    try {
      const snap = await a.conn.call("snapshot", SNAPSHOT_PARAMS, decodeSnapshot);
      a.inFlight = false;
      if (a === attempt) sink.snapshot(snap, sink.now());
    } finally {
      a.inFlight = false;
    }
  }

  /** The current attempt ended by itself: judge the outcome. */
  function ended(a: Attempt, reason: StopReason | undefined, ending: Ending | null): void {
    if (a !== attempt) return;
    attempt = null;
    a.conn.close();
    if (reason === "proof" || reason === "unauthorized") return hub.stop(reason);
    if (shown() === "live") setShown("reconnecting");
    if (!stale(ending)) return evaluate();
    asking = true;
    void hub.refresh().finally(() => {
      asking = false;
      evaluate();
    });
  }

  function begin(e: WorkspaceEntry): void {
    const a: Attempt = { n: ++attempts, conn: null as unknown as Conn, generation: e.generation, daemon: e.daemon ?? null, live: false, inFlight: false };
    recorded = { generation: a.generation, daemon: a.daemon };
    a.conn = new Conn({
      env,
      session: hub.session,
      reconnect: false,
      workspace: id!,
      async ready(c) {
        // Subscribe first, then the snapshot; pushes in between are discarded.
        a.inFlight = true;
        try {
          await c.call("subscribe");
        } finally {
          a.inFlight = false;
        }
        await snapshot(a);
      },
      push(push, t0) {
        if (a !== attempt || a.inFlight) return;
        if (push.kind === "resync") {
          void snapshot(a).catch((err) => {
            // Lost with the connection: its end is reported on its own.
            if (err instanceof CallError && err.code === "closed") return;
            // Behind the daemon with pushes discarded: a new attempt takes
            // a snapshot of its own.
            ended(a, "gone", { kind: "failed" });
          });
          return;
        }
        if (push.kind === "shutdown") {
          sink.shutdown();
          return ended(a, "gone", null);
        }
        sink.push(push, sink.now());
        sink.measured?.(push, t0);
      },
      state(s, reason) {
        if (a !== attempt) return;
        if (s === "live") {
          a.live = true;
          everLive = true;
          tries = 0;
          setShown("live");
          for (const f of [...liveListeners]) f();
        } else if (s === "stopped" || s === "signed-out") ended(a, reason, a.conn.ending);
      },
    });
    attempt = a;
    a.conn.start();
  }

  const offs = [hub.onListing(evaluate), hub.onLive(evaluate), hub.onStop(halt)];

  return {
    id,
    env,
    session: hub.session,
    hub,
    /** `[data-workspace-state]`. */
    shown,
    /** Whether a listing ever had it: `removed` says "taken out", not "not joined". */
    seen,
    /** Why it is `incompatible` or not running, from the listing. */
    detail,
    /** The page's state: the index connection's. */
    conn: hub.conn,
    stopReason: hub.stopReason,
    stopDetail: hub.stopDetail,
    stoppedAt: hub.stoppedAt,
    start: () => evaluate(),
    /** Try now (the tab became visible, the browser went online). */
    poke(): void {
      hub.poke();
      if (retry !== null) {
        clearRetry();
        due = true;
        evaluate();
      }
    },
    /** Stop the page for good. */
    stop: (reason: StopReason) => hub.stop(reason),
    /** True while the page may act on this workspace. */
    live: () => hub.live() && shown() === "live",
    call<T>(method: string, params?: unknown, decode?: (v: unknown) => T): Promise<T> {
      const a = attempt;
      if (!a?.live) return Promise.reject(new CallError("closed", "not connected"));
      return a.conn.call<T>(method, params, decode);
    },
    /** Run `f` each time a connection to the workspace goes live (terminals re-attach). */
    onLive(f: () => void): () => void {
      liveListeners.add(f);
      return () => liveListeners.delete(f);
    },
    /** The workspace was left: its connection closed, no more tries. */
    dispose(): void {
      if (disposed) return;
      disposed = true;
      for (const off of offs) off();
      halt();
      liveListeners.clear();
    },
    /** For tests: the page's view of the barrier, and the attempts made. */
    get snapshotInFlight() {
      return attempt?.inFlight ?? false;
    },
    get attempts() {
      return attempts;
    },
  };
}
