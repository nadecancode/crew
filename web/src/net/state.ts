// The state socket: hello, subscribe, snapshot, and the page's half of the
// snapshot barrier (notes/web-design.md §transport):
//
// 1. From the moment `snapshot` is sent until its reply arrives, pushes are
//    discarded: they are older than the snapshot that is coming. That covers
//    the time between `subscribe` and the first snapshot, and `resync`: a lag
//    reported before the reply is covered by that snapshot. There is never
//    more than one snapshot in flight; a `resync` after the reply starts the
//    next one.
// 2. The reply replaces agents, tasks, session, questions and both rings.
// 3. From then on every push is applied in the order received.
//
// A snapshot after `resync` that fails (times out, does not decode) leaves
// the page behind the daemon with the pushes in between discarded: the page
// starts over on a new connection, which takes a snapshot of its own.

import { createSignal } from "solid-js";
import { decodeSnapshot, type Push, type Snapshot } from "../protocol";
import { CallError, Conn, type ConnState, type Env, type StopReason } from "./conn";
import type { Session } from "./session";

export const SNAPSHOT_PARAMS = { messages: 300, events: 1000 };

export type Sink = {
  snapshot(snap: Snapshot, now: number): void;
  /** Applies a push; false when it is not the store's. */
  push(push: Push, now: number): boolean;
  /** `shutdown` arrived: the team was stopped. */
  shutdown(): void;
  now(): number;
  /** Measurements for §perf (handler time, push to frame). */
  measured?(push: Push, t0: number): void;
};

export type Link = ReturnType<typeof createLink>;

export function createLink(env: Env, session: Session, sink: Sink, hooks: { note?(text: string): void } = {}) {
  const [conn, setConn] = createSignal<ConnState>("connecting");
  const [stopReason, setStopReason] = createSignal<StopReason | null>(null);
  const [stopDetail, setStopDetail] = createSignal<string | null>(null);
  const [stoppedAt, setStoppedAt] = createSignal<number | null>(null);
  /** The page's half of the barrier: a snapshot is in flight. */
  let inFlight = false;
  const liveListeners = new Set<() => void>();

  async function snapshot(c: Conn): Promise<void> {
    if (inFlight) return;
    inFlight = true;
    try {
      const snap = await c.call("snapshot", SNAPSHOT_PARAMS, decodeSnapshot);
      inFlight = false;
      sink.snapshot(snap, sink.now());
    } finally {
      inFlight = false;
    }
  }

  const state = new Conn({
    env,
    session,
    reconnect: true,
    async ready(c) {
      // Subscribe first, then the snapshot; pushes in between are discarded.
      inFlight = true;
      try {
        await c.call("subscribe");
      } finally {
        inFlight = false;
      }
      await snapshot(c);
    },
    push(push, t0) {
      if (inFlight) return;
      if (push.kind === "resync") {
        void snapshot(state).catch((e) => {
          // Lost with the connection: reconnecting already takes a new snapshot.
          if (e instanceof CallError && e.code === "closed") return;
          state.restart();
        });
        return;
      }
      if (push.kind === "shutdown") {
        sink.shutdown();
        state.stop("shutdown");
        return;
      }
      sink.push(push, sink.now());
      sink.measured?.(push, t0);
    },
    state(s, reason, detail) {
      setConn(s);
      if (s === "live") for (const f of liveListeners) f();
      if (s === "stopped" || s === "signed-out") {
        setStopReason(reason ?? null);
        setStopDetail(detail ?? null);
        setStoppedAt(Date.now());
        // A stopped page keeps no session: removed from storage and memory.
        if (reason !== "protocol") session.remove();
      }
    },
    note: hooks.note,
  });

  return {
    conn,
    stopReason,
    stopDetail,
    stoppedAt,
    start: () => state.start(),
    poke: () => state.poke(),
    stop: (reason: StopReason) => state.stop(reason),
    /** True while the page may act: the state socket is live. */
    live: () => conn() === "live",
    call: <T>(method: string, params?: unknown, decode?: (v: unknown) => T) => state.call<T>(method, params, decode),
    /** Run `f` each time the state socket goes live (terminals re-attach). */
    onLive(f: () => void): () => void {
      liveListeners.add(f);
      return () => liveListeners.delete(f);
    },
    /** For the terminal sockets: same environment and session. */
    env,
    session,
    /** For tests: the page's view of the barrier. */
    get snapshotInFlight() {
      return inFlight;
    },
  };
}
