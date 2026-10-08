// A fake WebSocket and a scripted daemon behind it, for the connection's
// tests: everything the page sends is recorded, and the test answers.
import { createHash } from "node:crypto";
import type { Env } from "../src/net/conn";

export const sha256 = (s: string) => createHash("sha256").update(s).digest("hex");

export class FakeSocket {
  static all: FakeSocket[] = [];
  /** How the daemon behind answers `features_negotiate` by itself; null leaves
   * it to the test. By default as a v6 daemon: the method is unknown. */
  static negotiation: ((s: FakeSocket, id: number) => void) | null = (s, id) =>
    s.fail(id, "invalid", "malformed request: unknown variant `features_negotiate`, expected one of `hello`");
  readyState = 0;
  sent: any[] = [];
  onopen: ((e: unknown) => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: ((e: unknown) => void) | null = null;
  onerror: ((e: unknown) => void) | null = null;
  constructor(readonly url: string) {
    FakeSocket.all.push(this);
  }
  send(text: string) {
    if (this.readyState !== 1) throw new Error("send on a socket that is not open");
    const req = JSON.parse(text);
    this.sent.push(req);
    const answer = FakeSocket.negotiation;
    if (req.method === "features_negotiate" && answer) queueMicrotask(() => this.readyState === 1 && answer(this, req.id));
  }
  close() {
    if (this.readyState === 3) return;
    this.readyState = 3;
  }
  // ---- the server's side ----
  open() {
    this.readyState = 1;
    this.onopen?.({});
  }
  frame(obj: unknown) {
    this.onmessage?.({ data: JSON.stringify(obj) });
  }
  reply(id: number, result: unknown) {
    this.frame({ id, result });
  }
  fail(id: number, code: string, message = code) {
    this.frame({ id, error: { code, message } });
  }
  push(kind: string, data?: unknown) {
    this.frame({ push: data === undefined ? { kind } : { kind, data } });
  }
  /** Closed by the server's side: with `code` as a close frame says it, else 1006. */
  drop(code = 1006) {
    this.readyState = 3;
    this.onclose?.({ code });
  }
  /** The last request of `method` the page sent. */
  last(method: string) {
    return [...this.sent].reverse().find((r) => r.method === method);
  }
  methods() {
    return this.sent.map((r) => r.method);
  }
  /** The `workspace` its `hello` named; undefined for the index. */
  get workspace(): string | undefined {
    return this.sent.find((r) => r.method === "hello")?.params?.workspace;
  }
}

export function fakeEnv(over: Partial<Env> = {}): Env {
  return {
    WebSocket: FakeSocket as unknown as Env["WebSocket"],
    url: "ws://label.localhost:1/ws",
    setTimeout: (fn, ms) => setTimeout(fn, ms) as unknown as number,
    clearTimeout: (id) => clearTimeout(id),
    now: () => Date.now(),
    random: () => 0.5,
    ...over,
  };
}

/** Let promise chains (and the proof's digest) settle. */
export async function settle(times = 8) {
  for (let i = 0; i < times; i++) await new Promise((r) => setTimeout(r, 0));
}

/** The same under fake timers: the digest completes on a real turn of the event loop. */
const realTimeout = globalThis.setTimeout;
export async function flush(times = 4) {
  for (let i = 0; i < times; i++) await new Promise((r) => realTimeout(r, 1));
}

export class MemoryStorage {
  map = new Map<string, string>();
  reads = 0;
  getItem(k: string) {
    this.reads++;
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.map.set(k, v);
  }
  removeItem(k: string) {
    this.map.delete(k);
  }
}
