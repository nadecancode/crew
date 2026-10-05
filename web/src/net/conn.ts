// One WebSocket is one connection (notes/web-design.md §transport,
// notes/web-multi.md §multi-transport): `crew_core::rpc` frames, one JSON
// object per text message. A `hello` without a workspace is the index
// connection, answered by the dashboard server itself; with one, a
// workspace's daemon connection passed through the server. This module owns
// the handshake (pairing, `hello`, the server's proof), calls with their
// timeouts, and reconnecting. It knows nothing of what the connection is for:
// the index (net/hub.ts), a workspace's state socket (net/workspace.ts) and
// terminal sockets (term/) build on it.

import { timeoutFor } from "../generated/client-policy";
import { DecodeError, PROTOCOL, decodeFrame, decodeHello, decodePaired, type Push, type RpcError } from "../protocol";
import { proves, type Credential, type Session } from "./session";

/** What the status bar shows (`[data-conn]`), and why a page stopped. */
export type ConnState = "connecting" | "live" | "reconnecting" | "signed-out" | "stopped";

export type StopReason =
  /** The dashboard's server said it shuts down. */
  | "shutdown"
  /** Whoever answered could not prove it is this page's server. */
  | "proof"
  /** Reconnecting failed for 30 s. */
  | "gone"
  /** The session was not accepted (`unauthorized`). */
  | "unauthorized"
  /** No session and no pairing code, or the code was spent. */
  | "no-session"
  /** The server speaks another protocol version. */
  | "protocol";

/** How a connection that does not reconnect ended, for its owner to judge
 * (a workspace's attempt, notes/web-multi.md §multi-ui "What an outcome
 * does"). Nothing here says what became of the workspace. */
export type Ending =
  /** The socket closed: 4001 (the daemon's side ended), 4002 (taken out of
   * the dashboard), or any other code. */
  | { kind: "closed"; code: number }
  /** `hello` was answered with an error: `not_found`, `unavailable`, `protocol`. */
  | { kind: "refused"; code: string; message: string }
  /** A call of the handshake failed or timed out, or the socket never opened. */
  | { kind: "failed" };

/** Close codes of the dashboard server on a workspace connection. */
export const CLOSE_DAEMON_ENDED = 4001;
export const CLOSE_TAKEN_OUT = 4002;

export class CallError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
  static of(e: RpcError) {
    return new CallError(e.code, e.message);
  }
}

export { timeoutFor };

export const RETRY_FIRST_MS = 250;
export const RETRY_MAX_MS = 5_000;
export const GIVE_UP_MS = 30_000;

/** The delay before reconnect attempt `n` (0-based): 250 ms doubling to 5 s, ±20 %. */
export function backoff(n: number, random: () => number): number {
  const base = Math.min(RETRY_FIRST_MS * 2 ** Math.min(n, 10), RETRY_MAX_MS);
  return Math.round(base * (0.8 + 0.4 * random()));
}

/** What a connection needs from the world, so tests can fake all of it. */
export type Env = {
  WebSocket: new (url: string) => WebSocket;
  url: string;
  setTimeout(fn: () => void, ms: number): number;
  clearTimeout(id: number): void;
  now(): number;
  random(): number;
};

export function browserEnv(): Env {
  return {
    WebSocket,
    url: `ws://${location.host}/ws`,
    setTimeout: (fn, ms) => window.setTimeout(fn, ms),
    clearTimeout: (id) => window.clearTimeout(id),
    now: () => Date.now(),
    random: Math.random,
  };
}

type Waiter = { ok(v: unknown): void; err(e: CallError): void; timer: number };

export type ConnOptions = {
  env: Env;
  session: Session;
  /** Reconnect when the connection drops (the index); a workspace's state
   * socket and a terminal socket do not: their owners decide when to try
   * again. */
  reconnect: boolean;
  /** The workspace this connection is for, sent in `hello`; absent for the
   * index. A workspace's `hello` refused with anything but `unauthorized`
   * ends the connection (`ending`) instead of stopping the page. */
  workspace?: string;
  /** The server proved itself: start using the connection (subscribe and
   * snapshot, or attach). A throw closes the connection, which then
   * reconnects as any other failure. */
  ready(conn: Conn): Promise<void> | void;
  /** A push, applied synchronously; `t0` is when its message arrived. */
  push(push: Push, t0: number): void;
  state(state: ConnState, reason?: StopReason, detail?: string): void;
  /** A pairing code could not be traded but a stored session is used. */
  note?(text: string): void;
};

export class Conn {
  private ws: WebSocket | null = null;
  private next = 1;
  private waiting = new Map<number, Waiter>();
  private attempt = 0;
  private failingSince: number | null = null;
  private retryTimer: number | null = null;
  /** The give-up time while not live: it bounds a socket that never opens
   * and a handshake that never ends, not only the waits between attempts. */
  private deadline: number | null = null;
  private stoppedFor: StopReason | null = null;
  private isLive = false;
  private generation = 0;
  /** Why it ended, once it did without being stopped or closed by its owner. */
  ending: Ending | null = null;

  constructor(private readonly o: ConnOptions) {}

  get stopped(): StopReason | null {
    return this.stoppedFor;
  }

  get live(): boolean {
    return this.isLive;
  }

  start(): void {
    this.o.state("connecting");
    this.failingSince = this.o.env.now();
    this.open();
  }

  /** Start over on a new connection, as when a call of the handshake fails:
   * what the page shows can no longer be brought up to date on this one. */
  restart(): void {
    if (this.stoppedFor || !this.ws) return;
    this.drop();
    this.lost();
  }

  /** Connect again now (the tab became visible, the browser went online). */
  poke(): void {
    if (this.stoppedFor || this.ws || this.retryTimer === null) return;
    this.o.env.clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.open();
  }

  /** A call; its answer, or a `CallError` (`timeout`, `closed`, or the daemon's code).
   * Only on a connection whose server proved itself: until then nothing the
   * page asks or says may reach whoever answers (§security). */
  call<T = unknown>(method: string, params?: unknown, decode?: (v: unknown) => T): Promise<T> {
    if (!this.isLive) return Promise.reject(new CallError("closed", "not connected"));
    return this.request(method, params, decode);
  }

  /** A request on the open socket, proven or not: the handshake's own
   * `web_pair` and `hello` only. */
  private request<T>(method: string, params?: unknown, decode?: (v: unknown) => T): Promise<T> {
    const ws = this.ws;
    if (this.stoppedFor || !ws || ws.readyState !== 1) return Promise.reject(new CallError("closed", "not connected"));
    const id = this.next++;
    return new Promise<T>((ok, err) => {
      const timer = this.o.env.setTimeout(() => {
        // A late answer is ignored; the connection stays.
        if (this.waiting.delete(id)) err(new CallError("timeout", `${method}: the daemon did not answer`));
      }, timeoutFor(method));
      this.waiting.set(id, {
        ok: (v) => {
          try {
            ok(decode ? decode(v) : (v as T));
          } catch (e) {
            err(new CallError("invalid", e instanceof Error ? e.message : String(e)));
          }
        },
        err,
        timer,
      });
      ws.send(JSON.stringify(params === undefined ? { id, method } : { id, method, params }));
    });
  }

  /** A request the daemon does not answer (`attach_input`, `attach_resize`). */
  notify(method: string, params: unknown): void {
    const ws = this.ws;
    if (this.stoppedFor || !this.isLive || !ws || ws.readyState !== 1) return;
    ws.send(JSON.stringify({ id: this.next++, method, params }));
  }

  /** For good: no request of any kind is made after this. */
  stop(reason: StopReason, detail?: string): void {
    if (this.stoppedFor) return;
    this.stoppedFor = reason;
    this.isLive = false;
    this.clearDeadline();
    if (this.retryTimer !== null) this.o.env.clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.drop();
    this.o.state(reason === "unauthorized" || reason === "no-session" ? "signed-out" : "stopped", reason, detail);
  }

  /** Close without stopping: a terminal socket that is no longer shown. */
  close(): void {
    this.stoppedFor = this.stoppedFor ?? "gone";
    this.isLive = false;
    this.clearDeadline();
    if (this.retryTimer !== null) this.o.env.clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.drop();
  }

  private clearDeadline(): void {
    if (this.deadline !== null) this.o.env.clearTimeout(this.deadline);
    this.deadline = null;
  }

  private drop(): void {
    const ws = this.ws;
    this.ws = null;
    this.generation++;
    this.failAll("closed", "connection closed");
    if (ws) {
      ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
      try {
        ws.close();
      } catch {
        // Already closed.
      }
    }
  }

  private failAll(code: string, message: string): void {
    for (const w of this.waiting.values()) {
      this.o.env.clearTimeout(w.timer);
      w.err(new CallError(code, message));
    }
    this.waiting.clear();
  }

  private open(): void {
    if (this.stoppedFor) return;
    this.isLive = false;
    if (this.deadline === null && this.failingSince !== null) {
      const left = Math.max(0, this.failingSince + GIVE_UP_MS - this.o.env.now());
      this.deadline = this.o.env.setTimeout(() => {
        this.deadline = null;
        if (this.failingSince === null) return;
        this.ending ??= { kind: "failed" };
        this.stop("gone");
      }, left);
    }
    this.retryTimer = null;
    const generation = ++this.generation;
    const ws = new this.o.env.WebSocket(this.o.env.url);
    this.ws = ws;
    ws.onopen = () => {
      if (generation === this.generation) void this.handshake(generation);
    };
    ws.onmessage = (ev: MessageEvent) => {
      if (generation === this.generation) this.frame(ev.data);
    };
    ws.onclose = (ev: CloseEvent) => {
      if (generation !== this.generation) return;
      this.ending ??= { kind: "closed", code: typeof ev?.code === "number" ? ev.code : 1006 };
      this.lost();
    };
    ws.onerror = () => {};
  }

  private frame(data: unknown): void {
    const t0 = performance.now();
    if (typeof data !== "string") return;
    let frame;
    try {
      frame = decodeFrame(data);
    } catch (e) {
      // Malformed: not this page's server, or a bug. Dropped, never applied.
      if (!(e instanceof DecodeError)) throw e;
      return;
    }
    switch (frame.kind) {
      case "push":
        // Nothing the connection carries is used before the server proved itself.
        if (this.isLive) this.o.push(frame.push, t0);
        return;
      case "ignored":
        return;
      case "result":
      case "error": {
        if (frame.id === null) return;
        const w = this.waiting.get(frame.id);
        if (!w) return;
        this.waiting.delete(frame.id);
        this.o.env.clearTimeout(w.timer);
        if (frame.kind === "error") w.err(CallError.of(frame.error));
        else w.ok(frame.result);
      }
    }
  }

  private async handshake(generation: number): Promise<void> {
    const current = () => generation === this.generation && !this.stoppedFor;
    try {
      const code = this.o.session.takeCode();
      if (code !== null) {
        try {
          this.o.session.adopt(await this.request("web_pair", { code }, decodePaired));
        } catch (e) {
          if (!current()) return;
          if (!(e instanceof CallError) || (e.code !== "unauthorized" && e.code !== "unavailable")) throw e;
          if (this.o.session.current()) this.o.note?.("this address was already used to sign in; the stored session is used");
          else return this.stop("no-session", e.code === "unavailable" ? e.message : "this address was already used, or it expired");
        }
      }
      const credential: Credential | null = this.o.session.current();
      if (!credential) return this.stop("no-session");
      let hello;
      const params: Record<string, unknown> = { role: "operator", token: credential.token, client: "web", protocol: PROTOCOL };
      if (this.o.workspace !== undefined) params.workspace = this.o.workspace;
      try {
        hello = await this.request("hello", params, decodeHello);
      } catch (e) {
        if (!current()) return;
        if (e instanceof CallError && e.code === "unauthorized") return this.stop("unauthorized");
        // A workspace's refusal is about this attempt: its owner judges it.
        if (e instanceof CallError && this.o.workspace !== undefined && !["timeout", "closed", "invalid"].includes(e.code)) {
          this.ending = { kind: "refused", code: e.code, message: e.message };
          this.drop();
          return this.lost();
        }
        if (e instanceof CallError && e.code === "protocol") return this.stop("protocol", e.message);
        throw e;
      }
      // The proof first: nothing else the connection carries is used before it.
      const proven = await proves(hello.web_proof, credential);
      if (!current()) return;
      if (!proven) return this.stop("proof");
      this.isLive = true;
      await this.o.ready(this);
      if (!current()) return;
      this.attempt = 0;
      this.failingSince = null;
      this.clearDeadline();
      this.o.state("live");
    } catch {
      // A call failed or timed out: start over on a new connection.
      if (current()) {
        this.ending ??= { kind: "failed" };
        this.drop();
        this.lost();
      }
    }
  }

  private lost(): void {
    this.isLive = false;
    this.ws = null;
    this.generation++;
    this.failAll("closed", "connection closed");
    if (this.stoppedFor) return;
    if (!this.o.reconnect) {
      this.clearDeadline();
      this.stoppedFor = "gone";
      this.o.state("stopped", "gone");
      return;
    }
    const now = this.o.env.now();
    this.failingSince ??= now;
    if (now - this.failingSince >= GIVE_UP_MS) return this.stop("gone");
    this.o.state("reconnecting");
    const delay = Math.min(backoff(this.attempt++, this.o.env.random), Math.max(0, this.failingSince + GIVE_UP_MS - now));
    this.retryTimer = this.o.env.setTimeout(() => {
      this.retryTimer = null;
      if (this.o.env.now() - (this.failingSince ?? this.o.env.now()) >= GIVE_UP_MS) this.stop("gone");
      else this.open();
    }, delay);
  }
}
