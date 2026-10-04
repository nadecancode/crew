// The index connection: the page's link to its dashboard server
// (notes/web-multi.md §multi-transport "The index connection", §multi-ui
// "Two levels"). It holds the page's state, today's rules unchanged
// (`connecting`, `live`, `reconnecting`, `signed-out`, `stopped`), the listing
// of the workspaces the server shows, and the operator's preferences.
//
// The listing's order: a reply and a push both carry a whole listing, and
// either may arrive first. The page applies a listing only if its `rev` is
// greater than that of the last one it applied, and keeps that number for the
// document's life (its server is the same process for the document's life:
// no other can show the proof). Nothing a connection carries is applied
// before its proof was checked; then `workspaces` is called, and its reply
// covers every push dropped before.
//
// The page stops for good on the server's `shutdown`, a failed proof on any
// connection, `unauthorized`, or 30 s without the server; a workspace's
// connection never stops it otherwise.

import { createSignal } from "solid-js";
import { createDashboardPrefs } from "../prefs";
import { decodeWorkspaces, type WorkspaceEntry, type Workspaces } from "../protocol";
import { Conn, type ConnState, type Env, type StopReason } from "./conn";
import type { Session } from "./session";

export type Hub = ReturnType<typeof createHub>;

export function createHub(env: Env, session: Session, hooks: { note?(text: string): void } = {}) {
  const [conn, setConn] = createSignal<ConnState>("connecting");
  const [stopReason, setStopReason] = createSignal<StopReason | null>(null);
  const [stopDetail, setStopDetail] = createSignal<string | null>(null);
  const [stoppedAt, setStoppedAt] = createSignal<number | null>(null);
  const [listing, setListing] = createSignal<Workspaces | null>(null);
  /** The `rev` of the last listing applied; -1 before the first. */
  let rev = -1;
  const liveListeners = new Set<() => void>();
  const listingListeners = new Set<() => void>();
  const stopListeners = new Set<() => void>();

  /** Apply `next` if it is newer than what the page has; true if it was. */
  function apply(next: Workspaces): boolean {
    if (next.rev <= rev) return false;
    rev = next.rev;
    setListing(next);
    for (const f of [...listingListeners]) f();
    return true;
  }

  const index: Conn = new Conn({
    env,
    session,
    reconnect: true,
    async ready(c) {
      // The listing and the operator's preferences, together. The page is
      // live once both were answered; the preferences are best-effort.
      await Promise.all([
        c.call("workspaces", undefined, decodeWorkspaces).then(apply),
        prefs.loaded ? null : prefs.load().catch(() => {}),
      ]);
    },
    push(push) {
      if (push.kind === "workspaces") apply(push.data);
      else if (push.kind === "shutdown") index.stop("shutdown");
    },
    state(s, reason, detail) {
      setConn(s);
      if (s === "live") for (const f of [...liveListeners]) f();
      if (s === "stopped" || s === "signed-out") {
        setStopReason(reason ?? null);
        setStopDetail(detail ?? null);
        setStoppedAt(Date.now());
        // A stopped page keeps no session: removed from storage and memory.
        if (reason !== "protocol") session.remove();
        for (const f of [...stopListeners]) f();
      }
    },
    note: hooks.note,
  });

  const prefs = createDashboardPrefs((method, params) => index.call(method, params));

  const listen = (set: Set<() => void>) => (f: () => void) => {
    set.add(f);
    return () => void set.delete(f);
  };

  return {
    env,
    session,
    conn,
    stopReason,
    stopDetail,
    stoppedAt,
    /** The newest listing applied. */
    listing,
    /** The `rev` of the newest listing applied, -1 before the first. */
    get rev() {
      return rev;
    },
    entry: (id: string | null): WorkspaceEntry | null => (id === null ? null : listing()?.joined.find((e) => e.id === id) ?? null),
    prefs,
    start: () => index.start(),
    poke: () => index.poke(),
    /** Stop the page for good: every connection is closed. */
    stop: (reason: StopReason, detail?: string) => index.stop(reason, detail),
    live: () => conn() === "live",
    stopped: () => conn() === "stopped" || conn() === "signed-out",
    /** Ask for the listing: resolves once answered, applied or not (an equal
     * `rev` says the page's listing is current), or failed. */
    refresh(): Promise<void> {
      return index.call("workspaces", undefined, decodeWorkspaces).then(apply, () => false).then(() => {});
    },
    call: <T>(method: string, params?: unknown, decode?: (v: unknown) => T) => index.call<T>(method, params, decode),
    onLive: listen(liveListeners),
    onListing: listen(listingListeners),
    onStop: listen(stopListeners),
  };
}
