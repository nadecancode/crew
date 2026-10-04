// A mock dashboard server for the dashboard (notes/web-design.md §tests,
// notes/web-multi.md §multi-tests, the `mock` project). It serves the built
// bundle under a name of its own, `<label>.localhost:<port>`, with the
// headers of §security, and speaks the server's side of the WebSocket
// (protocol 6): pairing, `hello` with the server's proof; on the index
// connection the listing of its workspaces (`workspaces`, with a `rev`) and
// the operator's preferences; on a workspace connection that workspace's
// daemon: `subscribe`, `snapshot` and the calls of §transport, answered from
// a live state seeded with the wire fixtures. One workspace by default, as
// many as the test asks for; the controller stops, starts, removes and joins
// them as the server would see it happen. `bun run mock`
// serves it for `bun run dev` (dev/serve.ts); the Playwright specs start one
// per test (e2e/fixtures.ts) and drive it through the controller.
//
// Node only, no dependency: the WebSocket server is written by hand, so that
// what the page meets is the framing of RFC 6455 and nothing a library adds.
// Run as is by node 24 (type stripping): no syntax that needs a transform.

import { createHash, randomBytes } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import { STATUS_CODES, createServer } from "node:http";
import type { IncomingHttpHeaders, IncomingMessage, Server, ServerResponse } from "node:http";
import type { AddressInfo, Socket } from "node:net";
import { dirname, extname, join, resolve } from "node:path";
import type { Duplex } from "node:stream";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
export const FIXTURES = join(ROOT, "crates/crew/tests/fixtures/web");
export const DEFAULT_DIST = join(ROOT, "crates/crew/web-dist");

const ENUMS = readJson("enums.json");
export const PROTOCOL: number = ENUMS.protocol;
const MESSAGE_TYPES: string[] = ENUMS.message_type;
const RUNTIMES: string[] = ENUMS.runtime_kind;

const OPERATOR = "operator";
/** `MAX_FRAME_BYTES`: a message from the browser after `hello`. */
const MAX_MESSAGE = 1024 * 1024;
/** Before `hello` succeeded (§security, "Limits"). */
const MAX_MESSAGE_BEFORE_HELLO = 4 * 1024;
const MAX_HEADERS = 48;
const CODE_TTL_MS = 120_000;
const MAX_CODES = 8;
const MAX_PREFS = 16 * 1024;
const MAX_BODY = 65_536;
/** As `daemon/questions.rs`: how much of a question an answer quotes. */
const QUOTE_CHARS = 400;
const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

export type FixtureName = "snapshot" | "subagents" | "team10";
export type WorkspaceStatus = "running" | "stopped" | "incompatible" | "unreachable";
/** The ids the first workspaces get: `crew_core`'s 16 hex digits. */
export const WORKSPACE_IDS = ["946d4617aa702870", "0b54c1d2e3f4a596", "4c2e8a1b9d0f3e57", "7a1d2c3b4e5f6071"];
/** Close codes of a workspace connection (notes/web-multi.md §multi-transport). */
export const CLOSE_ENDED = 4001;
export const CLOSE_REMOVED = 4002;
export type WorkspaceOptions = {
  id?: string;
  fixture?: FixtureName;
  /** In place of the fixture's `session.project`. */
  project?: string;
  /** In place of the fixture's `session.workspace`. */
  workspace?: string;
  status?: WorkspaceStatus;
  detail?: string;
  /** False: running beside the server, not joined (`others` does not list it by itself). */
  joined?: boolean;
};
export const FIXTURE_NAMES: readonly FixtureName[] = ["snapshot", "subagents", "team10"];

/** What `hello` shows as `web_proof`: the session's secret (`ok`), something
 * else (`wrong`), the stored verifier itself (`verifier`), or nothing. */
export type ProofMode = "ok" | "wrong" | "verifier" | "missing";

export type MockOptions = {
  /** 0 (the default) picks a port that is free on both loopback addresses. */
  port?: number;
  /** The built bundle; `crates/crew/web-dist` by default. */
  dist?: string;
  fixture?: FixtureName;
  /** Answer only to `<label>.localhost:<port>` (Host) and its origin (the
   * upgrade's Origin). False behind the Vite dev proxy, which presents
   * `localhost:5173`. */
  strictHost?: boolean;
  /** A WebSocket that has not said hello by then is closed; 0 never. */
  helloTimeoutMs?: number;
  /** The workspaces the server shows, joined in this order; one by default
   * (with `fixture`). */
  workspaces?: WorkspaceOptions[];
  /** Running sessions that are not joined, as the listing names them. */
  others?: { id: string; project?: string | null; workspace: string }[];
};

type Obj = Record<string, any>;
export type Push = { kind: string; data?: unknown };
export type PushFrame = { push: Push };
/** What a handler throws to answer with an error. */
export type RpcFailure = { code: string; message: string };
/** The daemon's state as the mock keeps it; what `snapshot` reads. */
export type MockState = { session: Obj; agents: Obj[]; tasks: Obj[]; messages: Obj[]; events: Obj[]; questions: Obj[] };
export type MockCall = {
  /** Index in `mock.calls`. */
  seq: number;
  id: number;
  method: string;
  params: any;
  conn: MockConn;
  /** `Date.now()` on arrival. */
  at: number;
};
export type HttpRecord = {
  method: string;
  /** As requested, query included. */
  path: string;
  /** `Date.now()` on arrival. */
  time: number;
  status: number;
  /** An `Upgrade` request (the WebSocket, answered 101 when accepted). */
  upgrade: boolean;
  host: string | null;
  origin: string | null;
  /** The local address it came in on: `127.0.0.1` or `::1`. */
  address: string;
  headers: IncomingHttpHeaders;
};
/** Answers a call instead of the mock: a result, `NO_REPLY`, or a thrown `RpcFailure`. */
export type Handler = (params: any, conn: MockConn) => unknown;
/** A handler's result that sends no answer (as `attach_input`). */
export const NO_REPLY: unique symbol = Symbol("no reply");

/** One WebSocket, as tests see it. */
export interface MockConn {
  readonly id: number;
  readonly authenticated: boolean;
  /** After `hello`: null for the index, else the workspace's id. */
  readonly workspace: string | null | undefined;
  /** Text messages sent to the browser on it. */
  readonly sent: number;
  /** Receives the pushes of `mock.push` (after `subscribe` was answered). */
  readonly subscribed: boolean;
  /** The agent whose terminal this connection carries (after `attach`). */
  readonly attached: string | null;
  readonly open: boolean;
  readonly host: string | null;
  readonly origin: string | null;
  readonly address: string;
  /** Write one frame as it is (`{id, result}`, `{push}`, anything). */
  send(frame: unknown): void;
  push(push: Push | PushFrame): void;
  /** A close handshake with this code. */
  close(code?: number, reason?: string): void;
  /** Gone without a close frame (the browser sees 1006). */
  drop(): void;
}

/** One workspace behind the server: its daemon's state and controls. */
export interface MockWorkspace {
  readonly id: string;
  readonly fixture: FixtureName;
  readonly state: MockState;
  /** What its daemon's `web_prefs` keeps (the layout). */
  prefs: Record<string, unknown>;
  readonly status: WorkspaceStatus;
  readonly generation: number;
  readonly connections: readonly MockConn[];
  push(frameOrPush: Push | PushFrame): number;
  ptyWrite(agent: string, data: string | Uint8Array): number;
  snapshot(params?: { messages?: number; events?: number }): Obj;
  holdSnapshot(): () => void;
  /** The next `hello` for this workspace is answered `code` after `ms`. */
  holdHello(code: string, ms: number, message?: string): void;
}

/** The controller. The daemon's methods (`state`, `push`, `snapshot`, …)
 * act on the first workspace. */
export interface Mock {
  readonly label: string;
  readonly port: number;
  /** `<label>.localhost:<port>`. */
  readonly host: string;
  /** `http://<label>.localhost:<port>`. */
  readonly origin: string;
  /** The origin with `/`. */
  readonly url: string;
  /** The loopback addresses served: `127.0.0.1`, and `::1` where the host has it. */
  readonly addresses: readonly string[];
  readonly dist: string;
  readonly fixture: FixtureName;
  readonly strictHost: boolean;
  /** Live: seeded from the fixture, changed by what the mock does and pushes. */
  readonly state: MockState;
  /** Every request on every WebSocket, in order of arrival. */
  readonly calls: MockCall[];
  /** Every HTTP request, the upgrades included, in order of arrival. */
  readonly requests: HttpRecord[];
  /** The WebSockets open now. */
  readonly connections: readonly MockConn[];
  proofMode: ProofMode;
  /** What the server's `web_prefs` keeps (keys, panel, last). */
  prefs: Record<string, unknown>;
  /** Running sessions that are not joined. */
  others: { id: string; project?: string | null; workspace: string }[];
  /** Every workspace, joined or not, in the order of the options. */
  readonly workspaces: readonly MockWorkspace[];
  /** The listing's revision now. */
  readonly rev: number;
  /** A workspace by id or by index (0: the first). */
  workspace(which?: string | number): MockWorkspace;
  /** What `workspaces` would answer now. */
  listing(): Obj;
  /** Its daemon stops: its `shutdown` push, its connections closed with
   * 4001, the listing says `stopped`. */
  stopDaemon(id?: string): void;
  /** Its daemon starts again (a new pid): the listing says `running`. */
  startDaemon(id?: string): void;
  setStatus(id: string, status: WorkspaceStatus, detail?: string): void;
  /** `crew web --remove`: the listing without it, its connections closed
   * with 4002 (after `holdCloseMs`), nothing of theirs forwarded meanwhile. */
  remove(id: string, o?: { holdCloseMs?: number }): void;
  /** `crew web` for it: joined again, with a new generation. */
  join(id: string): void;
  /** Removed and joined again in one listing; the old connections' 4002
   * held back `holdCloseMs`. */
  removeAndRejoin(id: string, o?: { holdCloseMs?: number }): void;
  setOthers(others: { id: string; project?: string | null; workspace: string }[]): void;
  /** The mock's server time: the fixture's `server_time`, running since the start. */
  now(): number;
  /** A new pairing code: spent by the first `web_pair`, valid for `ttlMs`. */
  code(ttlMs?: number): string;
  /** `<origin>/#pair=<code>` (`&w=<id>` with a workspace), with a new code unless one is given. */
  pairUrl(code?: string, workspace?: string): string;
  /** Answer `method` with `handler` from now on; null restores the mock's own. */
  on(method: string, handler: Handler | null): void;
  /** The first call of `method` (already made or still to come) that `predicate` accepts. */
  waitForCall(method: string, predicate?: (call: MockCall) => boolean, timeout?: number): Promise<MockCall>;
  /** Push to every subscribed connection (`pty` and `pty_closed`: to every
   * terminal), after applying it to the state. Takes `{push: {kind, data}}`
   * or `{kind, data}`; answers how many connections it was sent to. */
  push(frameOrPush: Push | PushFrame): number;
  /** Terminal output for `agent`'s attached connections. */
  ptyWrite(agent: string, data: string | Uint8Array): number;
  /** Set the `timestamp` of an event or message (bare, in a push or in a
   * frame) to `now()`. Answers its argument. */
  retime<T>(obj: T): T;
  /** Frames of pushes.jsonl, copied: all, or those whose push kind, event
   * type, data `id` or message type is `name`, or that a predicate accepts. */
  pushes(name?: string | ((frame: PushFrame) => boolean)): PushFrame[];
  /** What a `snapshot` call would answer now. */
  snapshot(params?: { messages?: number; events?: number }): Obj;
  /** The next `snapshot` is captured when asked for, and answered only when
   * the returned function is called. */
  holdSnapshot(): () => void;
  /** Destroy every WebSocket without a close frame. */
  dropSockets(): void;
  /** As the server stopping: a `shutdown` push on every index connection,
   * then every WebSocket is closed; `web_pair` and `hello` answer
   * `unavailable` from then on. The listeners stay, so a test can see that
   * nothing comes back. */
  shutdown(): Promise<void>;
  /** Close the listeners and every connection. */
  stop(): Promise<void>;
}

export async function startMock(options: MockOptions = {}): Promise<Mock> {
  const mock = new MockServer(options);
  await mock.listen(options.port ?? 0);
  return mock;
}

// ---- helpers ----------------------------------------------------------------

function readJson(name: string): any {
  return JSON.parse(readFileSync(join(FIXTURES, name), "utf8"));
}
let resultsCache: Record<string, unknown[]> | null = null;
const results = () => (resultsCache ??= readJson("results.json") as Record<string, unknown[]>);
let pushesCache: PushFrame[] | null = null;
const pushLines = () =>
  (pushesCache ??= readFileSync(join(FIXTURES, "pushes.jsonl"), "utf8")
    .split("\n")
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l) as PushFrame));

const hex = (bytes: number) => randomBytes(bytes).toString("hex");
const sha256 = (s: string) => createHash("sha256").update(s).digest("hex");
const clone = <T>(v: T): T => structuredClone(v);
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
function fail(code: string, message: string): never {
  const failure: RpcFailure = { code, message };
  throw failure;
}
function toFailure(e: unknown): RpcFailure {
  if (isObj(e) && typeof e.code === "string" && typeof e.message === "string" && !(e instanceof Error)) return { code: e.code, message: e.message };
  return { code: "internal", message: e instanceof Error ? e.message : String(e) };
}
function asFrame(input: Push | PushFrame): PushFrame {
  if (isObj(input) && isObj((input as PushFrame).push)) return input as PushFrame;
  if (isObj(input) && typeof (input as Push).kind === "string") return { push: input as Push };
  throw new TypeError("a push is {push: {kind, data}} or {kind, data}");
}
function upsert(list: Obj[], item: Obj): void {
  const i = list.findIndex((x) => x.id === item.id);
  if (i < 0) list.push(item);
  else list[i] = item;
}
const newest = <T>(list: T[], n: number): T[] => (n > 0 ? list.slice(-n) : []);
const limitOf = (v: unknown, fallback: number, max = Infinity) => Math.min(typeof v === "number" && v >= 0 ? v : fallback, max);

// ---- HTTP -------------------------------------------------------------------

/** The policy of §security, for this server's name. */
export function contentSecurityPolicy(label: string, port: number): string {
  return (
    "default-src 'none'; script-src 'self'; style-src-elem 'self' 'unsafe-inline'; style-src-attr 'none'; " +
    `img-src 'self' data:; font-src 'self'; connect-src 'self' ws://${label}.localhost:${port}; ` +
    "base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
  );
}

const CONTENT_TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".map": "application/json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".webp": "image/webp",
  ".ico": "image/x-icon",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".ttf": "font/ttf",
  ".txt": "text/plain; charset=utf-8",
  ".wasm": "application/wasm",
};

/** Headers on every response. Every response closes its connection, as the
 * daemon's: a page load is a handful of requests and keeps no state. */
const EVERY_RESPONSE = {
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "no-referrer",
  "Cross-Origin-Resource-Policy": "same-origin",
  Connection: "close",
};

type Answer = { status: number; headers: Record<string, string>; body: Buffer };

function answer(status: number, headers: Record<string, string> = {}, body: Buffer | string = `${STATUS_CODES[status] ?? status}\n`): Answer {
  const bytes = typeof body === "string" ? Buffer.from(body) : body;
  return {
    status,
    headers: { "Content-Type": "text/plain; charset=utf-8", ...headers, ...EVERY_RESPONSE, "Content-Length": String(bytes.length) },
    body: bytes,
  };
}

/** An answer on a socket that left the HTTP server (an upgrade refused). */
function writeRaw(socket: Duplex, a: Answer): void {
  const head = [`HTTP/1.1 ${a.status} ${STATUS_CODES[a.status] ?? ""}`, ...Object.entries(a.headers).map(([k, v]) => `${k}: ${v}`), "", ""].join("\r\n");
  socket.on("error", () => {});
  socket.end(Buffer.concat([Buffer.from(head, "latin1"), a.body]));
}

// ---- WebSocket framing (RFC 6455) --------------------------------------------

const OP_CONTINUATION = 0x0;
const OP_TEXT = 0x1;
const OP_BINARY = 0x2;
const OP_CLOSE = 0x8;
const OP_PING = 0x9;
const OP_PONG = 0xa;

/** One unfragmented, unmasked frame: what a server sends. */
function encodeFrame(opcode: number, payload: Buffer): Buffer {
  const len = payload.length;
  let head: Buffer;
  if (len < 126) {
    head = Buffer.from([0x80 | opcode, len]);
  } else if (len < 65_536) {
    head = Buffer.alloc(4);
    head[1] = 126;
    head.writeUInt16BE(len, 2);
  } else {
    head = Buffer.alloc(10);
    head[1] = 127;
    head.writeBigUInt64BE(BigInt(len), 2);
  }
  head[0] = 0x80 | opcode;
  return Buffer.concat([head, payload]);
}

const utf8 = new TextDecoder("utf-8", { fatal: true });

type Hooks = {
  request(conn: Connection, text: string): Promise<void>;
  gone(conn: Connection): void;
};

class Connection implements MockConn {
  readonly id: number;
  readonly host: string | null;
  readonly origin: string | null;
  readonly address: string;
  authenticated = false;
  /** After `hello`: null for the index, else the workspace's id. */
  workspace: string | null | undefined = undefined;
  /** Its workspace's generation was revoked: nothing is forwarded any more. */
  revoked = false;
  sent = 0;
  subscribed = false;
  attached: string | null = null;
  /** Run once the answer to the current request was written: how
   * `subscribe` starts pushes only after its reply, as the daemon does. */
  afterReply: (() => void) | null = null;

  #socket: Socket;
  #hooks: Hooks;
  #buffer: Buffer = Buffer.alloc(0);
  #parts: Buffer[] | null = null;
  #partsLength = 0;
  /** Requests are handled one at a time per connection, as the daemon's
   * reader does: an answer is written before the next request is read. */
  #queue: Promise<void> = Promise.resolve();
  #closing = false;
  #broken = false;
  #ended = false;
  #helloTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(id: number, socket: Socket, req: IncomingMessage, hooks: Hooks, helloTimeoutMs: number) {
    this.id = id;
    this.host = req.headers.host ?? null;
    this.origin = req.headers.origin ?? null;
    this.address = socket.localAddress ?? "";
    this.#socket = socket;
    this.#hooks = hooks;
    socket.setNoDelay(true);
    socket.setTimeout(0);
    socket.on("data", (d: Buffer) => this.#read(d));
    socket.on("error", () => {});
    socket.on("close", () => this.#end());
    if (helloTimeoutMs > 0) {
      this.#helloTimer = setTimeout(() => this.close(1008, "say hello first"), helloTimeoutMs);
      this.#helloTimer.unref();
    }
  }

  get open(): boolean {
    return !this.#ended && !this.#closing;
  }

  signedIn(): void {
    this.authenticated = true;
    if (this.#helloTimer) clearTimeout(this.#helloTimer);
  }

  send(frame: unknown): void {
    if (!this.open) return;
    this.sent++;
    this.#socket.write(encodeFrame(OP_TEXT, Buffer.from(JSON.stringify(frame))));
  }

  push(push: Push | PushFrame): void {
    this.send(asFrame(push));
  }

  close(code = 1000, reason = ""): void {
    if (!this.open) return;
    this.#closing = true;
    const text = Buffer.from(reason);
    const payload = Buffer.alloc(2 + text.length);
    payload.writeUInt16BE(code, 0);
    text.copy(payload, 2);
    this.#socket.write(encodeFrame(OP_CLOSE, payload));
    this.#socket.end();
    // A peer that never answers the close frame is not waited for.
    setTimeout(() => this.#socket.destroy(), 1000).unref();
  }

  drop(): void {
    this.#socket.destroy();
  }

  /** Resolves when the socket is gone. */
  ended(): Promise<void> {
    return this.#ended ? Promise.resolve() : new Promise((done) => this.#socket.once("close", () => done()));
  }

  #limit(): number {
    return this.authenticated ? MAX_MESSAGE : MAX_MESSAGE_BEFORE_HELLO;
  }

  #read(data: Buffer): void {
    if (this.#broken) return;
    this.#buffer = this.#buffer.length ? Buffer.concat([this.#buffer, data]) : data;
    for (;;) {
      const b = this.#buffer;
      if (b.length < 2) return;
      const fin = (b[0] & 0x80) !== 0;
      const rsv = b[0] & 0x70;
      const opcode = b[0] & 0x0f;
      const masked = (b[1] & 0x80) !== 0;
      let len = b[1] & 0x7f;
      let at = 2;
      if (len === 126) {
        if (b.length < 4) return;
        len = b.readUInt16BE(2);
        at = 4;
      } else if (len === 127) {
        if (b.length < 10) return;
        const big = b.readBigUInt64BE(2);
        len = big > BigInt(Number.MAX_SAFE_INTEGER) ? Infinity : Number(big);
        at = 10;
      }
      // No extension was negotiated, and a browser masks every frame.
      if (rsv !== 0 || !masked) return this.#fail(1002, "protocol error");
      const control = opcode >= 0x8;
      if (control && (!fin || len > 125)) return this.#fail(1002, "malformed control frame");
      // Refused from the header, before the payload is buffered.
      if (!control && this.#partsLength + len > this.#limit()) return this.#fail(1009, "message too large");
      if (b.length < at + 4 + len) return;
      const mask = b.subarray(at, at + 4);
      const payload = Buffer.from(b.subarray(at + 4, at + 4 + len));
      for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
      this.#buffer = b.subarray(at + 4 + len);
      this.#frame(fin, opcode, payload);
      if (this.#broken) return;
    }
  }

  #frame(fin: boolean, opcode: number, payload: Buffer): void {
    switch (opcode) {
      case OP_CONTINUATION:
        if (!this.#parts) return this.#fail(1002, "continuation without a message");
        this.#parts.push(payload);
        this.#partsLength += payload.length;
        if (fin) this.#message();
        return;
      case OP_TEXT:
        if (this.#parts) return this.#fail(1002, "a message inside a fragmented message");
        this.#parts = [payload];
        this.#partsLength = payload.length;
        if (fin) this.#message();
        return;
      case OP_BINARY:
        return this.#fail(1003, "binary messages are not used");
      case OP_CLOSE:
        if (!this.#closing) {
          this.#closing = true;
          this.#socket.write(encodeFrame(OP_CLOSE, payload.subarray(0, 2)));
        }
        this.#socket.end();
        return;
      case OP_PING:
        if (!this.#closing) this.#socket.write(encodeFrame(OP_PONG, payload));
        return;
      case OP_PONG:
        return;
      default:
        return this.#fail(1002, `unknown opcode ${opcode}`);
    }
  }

  #message(): void {
    const data = Buffer.concat(this.#parts ?? []);
    this.#parts = null;
    this.#partsLength = 0;
    if (this.#closing) return;
    let text: string;
    try {
      text = utf8.decode(data);
    } catch {
      return this.#fail(1007, "not UTF-8");
    }
    this.#queue = this.#queue.then(() => this.#hooks.request(this, text)).catch(() => {});
  }

  #fail(code: number, reason: string): void {
    this.#broken = true;
    this.#buffer = Buffer.alloc(0);
    this.close(code, reason);
  }

  #end(): void {
    if (this.#ended) return;
    this.#ended = true;
    if (this.#helloTimer) clearTimeout(this.#helloTimer);
    this.#hooks.gone(this);
  }
}

// ---- the server and its workspaces -------------------------------------------

type WebSession = { token: string; secret: string; verifier: string };
type Waiter = { ok(call: MockCall): boolean; resolve(call: MockCall): void; reject(e: Error): void; timer: ReturnType<typeof setTimeout> };
type Hold = { promise: Promise<void>; release(): void };
type HeldHello = { code: string; message: string; ms: number };

/** Calls a connection may make before `hello` succeeded (§security, "Limits"). */
const BEFORE_HELLO = new Set(["ping", "web_pair", "hello"]);
/** Calls of the index connection (notes/web-multi.md §multi-transport). */
const INDEX_CALLS = new Set(["ping", "workspaces", "web_prefs"]);

const counts = (state: MockState): Obj => ({
  project: state.session.project ?? null,
  agents: state.agents.length,
  working: state.agents.filter((a) => a.status === "working").length,
  attention: state.agents.filter((a) => a.status === "blocked" || a.status === "failed").length,
  questions: state.questions.length,
  tasks: state.tasks.length,
  tasks_done: state.tasks.filter((t) => t.status === "done").length,
});

class Workspace implements MockWorkspace {
  readonly id: string;
  readonly fixture: FixtureName;
  readonly state: MockState;
  prefs: Record<string, unknown> = {};
  status: WorkspaceStatus;
  detail = "";
  generation = 0;
  joined = false;
  joinedAt = 0;
  daemon: { pid: number; started_at: number } | null = null;
  summary: Obj | null = null;
  summaryRev = 0;
  holds: Hold[] = [];
  heldHello: HeldHello | null = null;
  readonly #server: MockServer;

  constructor(server: MockServer, o: WorkspaceOptions, id: string) {
    this.#server = server;
    this.id = id;
    this.fixture = o.fixture ?? "snapshot";
    if (!FIXTURE_NAMES.includes(this.fixture)) throw new Error(`unknown fixture ${this.fixture}: one of ${FIXTURE_NAMES.join(", ")}`);
    const snap = readJson(`${this.fixture}.json`);
    if (o.project !== undefined) snap.session.project = o.project;
    if (o.workspace !== undefined) {
      snap.session.workspace = o.workspace;
      snap.session.state_dir = `${o.workspace}/.crew`;
    }
    this.state = {
      session: snap.session,
      agents: snap.agents,
      tasks: snap.tasks,
      messages: snap.messages,
      events: snap.events,
      questions: snap.questions ?? [],
    };
    this.status = o.status ?? "running";
    if (this.status === "running") this.daemon = { pid: snap.session.daemon_pid ?? 4242, started_at: snap.session.daemon_started_at || Date.now() };
    this.detail = o.detail ?? (this.status === "incompatible" ? "started by another version of crew (protocol 5): restart the team" : "");
    this.refreshSummary();
  }

  get connections(): readonly MockConn[] {
    return this.#server.connections.filter((c) => c.workspace === this.id);
  }

  /** Recompute what the switcher shows; true when it changed. */
  refreshSummary(): boolean {
    if (this.status !== "running") return false;
    const next = counts(this.state);
    if (this.summary && Object.keys(next).every((k) => this.summary![k] === next[k])) return false;
    this.summary = { rev: ++this.summaryRev, ...next };
    return true;
  }

  entry(): Obj {
    const s = this.state.session;
    return {
      id: this.id,
      generation: this.generation,
      daemon: this.status === "running" ? this.daemon : null,
      project: this.summary?.project ?? s.project ?? null,
      workspace: s.workspace ?? "",
      state: this.status,
      detail: this.status === "incompatible" || this.status === "unreachable" ? this.detail : "",
      summary: this.summary,
      joined_at: this.joinedAt,
    };
  }

  push(frameOrPush: Push | PushFrame): number {
    const frame = clone(asFrame(frameOrPush));
    this.#apply(frame.push);
    const terminal = frame.push.kind === "pty" || frame.push.kind === "pty_closed";
    let sent = 0;
    for (const conn of this.connections) {
      if (!conn.open || !(terminal ? conn.attached !== null : conn.subscribed)) continue;
      conn.send(frame);
      sent++;
    }
    if (this.refreshSummary()) this.#server.listingChanged();
    return sent;
  }

  ptyWrite(agent: string, data: string | Uint8Array): number {
    const frame = { push: { kind: "pty", data: { data: Buffer.from(data).toString("base64") } } };
    let sent = 0;
    for (const conn of this.connections) {
      if (conn.open && conn.attached === agent) {
        conn.send(frame);
        sent++;
      }
    }
    return sent;
  }

  /** What a push changes in the daemon's state, so that a later snapshot has it. */
  #apply(push: Push): void {
    const s = this.state;
    const d = isObj(push.data) ? push.data : {};
    switch (push.kind) {
      case "agent":
        upsert(s.agents, clone(d));
        return;
      case "agent_removed":
        s.agents = s.agents.filter((a) => a.id !== d.id);
        return;
      case "task":
        upsert(s.tasks, clone(d));
        return;
      case "session":
        s.session = clone(d);
        return;
      case "message":
        if (!s.messages.some((m) => m.id === d.id)) s.messages.push(clone(d));
        if (d.to === OPERATOR && d.type === "question" && !s.questions.some((q) => q.id === d.id)) s.questions.push(clone(d));
        if (d.from === OPERATOR && d.reply_to) s.questions = s.questions.filter((q) => q.id !== d.reply_to);
        return;
      case "event":
        if (d.seq === 0 || !s.events.some((e) => e.seq === d.seq)) s.events.push(clone(d));
        return;
    }
  }

  snapshot(params: { messages?: number; events?: number } = {}): Obj {
    const s = this.state;
    return clone({
      session: s.session,
      agents: s.agents,
      tasks: s.tasks,
      messages: newest(s.messages, limitOf(params.messages, 200, 2000)),
      events: newest(s.events, limitOf(params.events, 300, 5000)),
      questions: s.questions,
      server_time: this.#server.now(),
    });
  }

  holdSnapshot(): () => void {
    let release: () => void = () => {};
    const hold: Hold = {
      promise: new Promise<void>((r) => {
        release = r;
      }),
      release: () => {
        this.holds = this.holds.filter((h) => h !== hold);
        release();
      },
    };
    this.holds.push(hold);
    return hold.release;
  }

  holdHello(code: string, ms: number, message = code): void {
    this.heldHello = { code, message, ms };
  }
}

class MockServer implements Mock {
  label = "";
  port = 0;
  addresses: string[] = [];
  readonly dist: string;
  readonly strictHost: boolean;
  readonly calls: MockCall[] = [];
  readonly requests: HttpRecord[] = [];
  proofMode: ProofMode = "ok";
  /** The server's preferences: the operator's (keys, panel, last). */
  prefs: Record<string, unknown> = {};
  others: { id: string; project?: string | null; workspace: string }[] = [];
  readonly workspaces: Workspace[] = [];
  rev = 1;

  #helloTimeoutMs: number;
  #servers: Server[] = [];
  #conns = new Set<Connection>();
  #nextConn = 1;
  #codes = new Map<string, number>();
  #sessions = new Map<string, WebSession>();
  #handlers = new Map<string, Handler>();
  #waiters: Waiter[] = [];
  #generations = 0;
  #serverTime: number;
  #startedAt = Date.now();
  #stopping = false;
  #stopped = false;

  constructor(options: MockOptions) {
    this.dist = resolve(options.dist ?? DEFAULT_DIST);
    this.strictHost = options.strictHost ?? true;
    this.#helloTimeoutMs = options.helloTimeoutMs ?? 5000;
    const list = options.workspaces ?? [{ fixture: options.fixture }];
    if (list.length === 0) throw new Error("a mock serves at least one workspace");
    list.forEach((o, i) => {
      const ws = new Workspace(this, { fixture: options.fixture, ...o }, o.id ?? WORKSPACE_IDS[i] ?? hex(8));
      this.workspaces.push(ws);
      if (o.joined !== false) this.#join(ws);
    });
    this.others = clone(options.others ?? []);
    this.#serverTime = readJson(`${this.workspaces[0].fixture}.json`).server_time;
  }

  /** The first workspace: what the controller's daemon methods act on. */
  get primary(): Workspace {
    return this.workspaces[0];
  }
  get fixture(): FixtureName {
    return this.primary.fixture;
  }
  get state(): MockState {
    return this.primary.state;
  }
  get host(): string {
    return `${this.label}.localhost:${this.port}`;
  }
  get origin(): string {
    return `http://${this.host}`;
  }
  get url(): string {
    return `${this.origin}/`;
  }
  get connections(): readonly MockConn[] {
    return [...this.#conns];
  }

  now(): number {
    return this.#serverTime + (Date.now() - this.#startedAt);
  }

  workspace(which: string | number = 0): Workspace {
    const ws = typeof which === "number" ? this.workspaces[which] : this.workspaces.find((w) => w.id === which);
    if (!ws) throw new Error(`no workspace ${which}`);
    return ws;
  }

  listing(): Obj {
    return clone({ rev: this.rev, joined: this.workspaces.filter((w) => w.joined).sort((a, b) => a.joinedAt - b.joinedAt).map((w) => w.entry()), others: this.others });
  }

  /** Something in the listing changed: a new revision, pushed to every index connection. */
  listingChanged(): void {
    this.rev++;
    const frame = { push: { kind: "workspaces", data: this.listing() } };
    for (const conn of this.#conns) if (conn.open && conn.workspace === null) conn.send(frame);
  }

  #join(ws: Workspace): void {
    ws.joined = true;
    ws.generation = ++this.#generations;
    ws.joinedAt = Date.now() + this.#generations;
  }

  join(id: string): void {
    const ws = this.workspace(id);
    if (ws.joined) return;
    this.#join(ws);
    this.listingChanged();
  }

  remove(id: string, o: { holdCloseMs?: number } = {}): void {
    const ws = this.workspace(id);
    ws.joined = false;
    const conns = ws.connections;
    this.listingChanged();
    this.#closeLater(conns, CLOSE_REMOVED, "taken out of this dashboard", o.holdCloseMs ?? 0);
  }

  removeAndRejoin(id: string, o: { holdCloseMs?: number } = {}): void {
    const ws = this.workspace(id);
    const conns = ws.connections;
    this.#join(ws);
    this.listingChanged();
    this.#closeLater(conns, CLOSE_REMOVED, "taken out of this dashboard", o.holdCloseMs ?? 0);
  }

  #closeLater(conns: readonly MockConn[], code: number, reason: string, ms: number): void {
    // The connections are this generation's no longer: nothing more reaches them.
    for (const c of conns) (c as Connection).revoked = true;
    const close = () => conns.forEach((c) => c.close(code, reason));
    if (ms > 0) setTimeout(close, ms).unref();
    else close();
  }

  stopDaemon(id: string = this.primary.id): void {
    const ws = this.workspace(id);
    const conns = ws.connections;
    for (const c of conns) if (c.subscribed) c.push({ kind: "shutdown" });
    ws.status = "stopped";
    ws.daemon = null;
    for (const c of conns) c.close(CLOSE_ENDED, "the workspace's connection ended");
    this.listingChanged();
  }

  startDaemon(id: string = this.primary.id): void {
    const ws = this.workspace(id);
    ws.status = "running";
    ws.detail = "";
    ws.daemon = { pid: (ws.daemon?.pid ?? ws.state.session.daemon_pid ?? 4242) + 1, started_at: Date.now() };
    ws.refreshSummary();
    this.listingChanged();
  }

  setStatus(id: string, status: WorkspaceStatus, detail = ""): void {
    if (status === "running") return this.startDaemon(id);
    const ws = this.workspace(id);
    const conns = ws.connections;
    ws.status = status;
    ws.detail = detail;
    ws.daemon = null;
    for (const c of conns) c.close(CLOSE_ENDED, "the workspace's connection ended");
    this.listingChanged();
  }

  setOthers(others: { id: string; project?: string | null; workspace: string }[]): void {
    this.others = clone(others);
    this.listingChanged();
  }

  // -- listening --

  /** Both loopback addresses on one port, as the server (§security, "The
   * listener"): a port is taken when either address is. The label is made
   * only once both are bound. */
  async listen(wanted: number): Promise<void> {
    for (let attempt = 0; ; attempt++) {
      const v4 = this.#server();
      const port = await listenOn(v4, wanted, "127.0.0.1");
      const v6 = this.#server();
      try {
        await listenOn(v6, port, "::1");
        this.#servers = [v4, v6];
        this.addresses = ["127.0.0.1", "::1"];
      } catch (e) {
        const code = (e as NodeJS.ErrnoException).code;
        if (code === "EADDRNOTAVAIL" || code === "EAFNOSUPPORT") {
          this.#servers = [v4];
          this.addresses = ["127.0.0.1"];
        } else {
          await closeServer(v4);
          if (wanted !== 0 || code !== "EADDRINUSE" || attempt >= 20) throw e;
          continue;
        }
      }
      this.port = port;
      this.label = hex(16);
      for (const ws of this.workspaces) ws.state.session.web = this.url;
      return;
    }
  }

  #server(): Server {
    const server = createServer({ maxHeaderSize: 8192, headersTimeout: 5000, requestTimeout: 5000 });
    server.on("request", (req: IncomingMessage, res: ServerResponse) => {
      const a = this.#route(req);
      this.#record(req, a.status, false);
      res.writeHead(a.status, a.headers);
      res.end(a.body);
    });
    server.on("upgrade", (req: IncomingMessage, socket: Duplex, head: Buffer) => this.#upgrade(req, socket as Socket, head));
    // A head that is too large, too slow or not HTTP: 400, and the connection closes.
    server.on("clientError", (_e: Error, socket: Duplex) => {
      if (socket.writable) socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
      else socket.destroy();
    });
    return server;
  }

  #record(req: IncomingMessage, status: number, upgrade: boolean): void {
    this.requests.push({
      method: req.method ?? "",
      path: req.url ?? "",
      time: Date.now(),
      status,
      upgrade,
      host: req.headers.host ?? null,
      origin: req.headers.origin ?? null,
      address: req.socket.localAddress ?? "",
      headers: req.headers,
    });
  }

  /** What every request must pass before routing (§security). */
  #gate(req: IncomingMessage): Answer | null {
    if (req.rawHeaders.length / 2 > MAX_HEADERS) return answer(400);
    const length = req.headers["content-length"];
    if ((length !== undefined && length !== "0") || req.headers["transfer-encoding"] !== undefined) return answer(400);
    if (this.strictHost && req.headers.host !== this.host) return answer(421);
    if (req.method !== "GET") return answer(405, { Allow: "GET" });
    return null;
  }

  #route(req: IncomingMessage): Answer {
    const refused = this.#gate(req);
    if (refused) return refused;
    const path = (req.url ?? "/").split("?")[0];
    if (path === "/" || path === "/index.html") return this.#document();
    if (path === "/ws" || !path.startsWith("/") || /\.\.|%|\\|\0/.test(path)) return answer(404);
    const file = join(this.dist, path);
    try {
      if (!statSync(file).isFile()) return answer(404);
      return answer(
        200,
        { "Content-Type": CONTENT_TYPES[extname(file)] ?? "application/octet-stream", "Cache-Control": "public, max-age=31536000, immutable" },
        readFileSync(file),
      );
    } catch {
      return answer(404);
    }
  }

  #document(): Answer {
    let body: Buffer;
    try {
      body = readFileSync(join(this.dist, "index.html"));
    } catch {
      return answer(404, {}, `no bundle in ${this.dist}: run \`bun run build\`\n`);
    }
    return answer(
      200,
      {
        "Content-Type": "text/html; charset=utf-8",
        "Cache-Control": "no-store",
        "X-Frame-Options": "DENY",
        "Cross-Origin-Opener-Policy": "same-origin",
        "Content-Security-Policy": contentSecurityPolicy(this.label, this.port),
      },
      body,
    );
  }

  #upgrade(req: IncomingMessage, socket: Socket, head: Buffer): void {
    const path = (req.url ?? "/").split("?")[0];
    const websocket = (req.headers.upgrade ?? "").toLowerCase() === "websocket";
    let refused = this.#gate(req);
    // Anything but the WebSocket on /ws is routed as a plain request.
    if (!refused && !(websocket && path === "/ws")) refused = this.#route(req);
    if (!refused && this.strictHost && req.headers.origin !== this.origin) refused = answer(403);
    const key = req.headers["sec-websocket-key"];
    if (!refused && (req.headers["sec-websocket-version"] !== "13" || typeof key !== "string" || Buffer.from(key, "base64").length !== 16)) {
      refused = answer(400);
    }
    this.#record(req, refused ? refused.status : 101, true);
    if (refused) return writeRaw(socket, refused);

    const accept = createHash("sha1").update(`${key}${WS_GUID}`).digest("base64");
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    const conn = new Connection(
      this.#nextConn++,
      socket,
      req,
      { request: (c, text) => this.#request(c, text), gone: (c) => this.#conns.delete(c) },
      this.#helloTimeoutMs,
    );
    this.#conns.add(conn);
    if (head.length) socket.emit("data", head);
  }

  // -- requests --

  async #request(conn: Connection, text: string): Promise<void> {
    // One request per message: a line break would be two requests to a daemon.
    if (/[\r\n]/.test(text)) return conn.close(1008, "one request per message: no line breaks");
    // A connection of a generation that was revoked forwards nothing.
    if (conn.revoked) return;
    let req: unknown;
    try {
      req = JSON.parse(text);
    } catch (e) {
      conn.send({ id: 0, error: { code: "invalid", message: `malformed request: ${(e as Error).message}` } });
      return;
    }
    const id = isObj(req) && Number.isInteger(req.id) && req.id >= 0 ? (req.id as number) : null;
    if (!isObj(req) || id === null || typeof req.method !== "string") {
      const missing = !isObj(req) ? "a request is an object" : id === null ? "missing field `id`" : "missing field `method`";
      conn.send({ id: id ?? 0, error: { code: "invalid", message: `malformed request: ${missing}` } });
      return;
    }
    const call: MockCall = { seq: this.calls.length, id, method: req.method, params: req.params, conn, at: Date.now() };
    this.calls.push(call);
    this.#notify(call);

    conn.afterReply = null;
    let frame: unknown;
    try {
      const result = await this.#dispatch(conn, req.method, req.params);
      if (result === NO_REPLY) return;
      frame = { id, result: result === undefined ? null : result };
    } catch (e) {
      conn.afterReply = null;
      frame = { id, error: toFailure(e) };
    }
    if (conn.revoked) return;
    conn.send(frame);
    // Set by the handler while it was awaited.
    const after = conn.afterReply as (() => void) | null;
    conn.afterReply = null;
    after?.();
  }

  async #dispatch(conn: Connection, method: string, params: unknown): Promise<unknown> {
    if (!conn.authenticated && !BEFORE_HELLO.has(method)) fail("unauthorized", "say hello first");
    const handler = this.#handlers.get(method);
    if (handler) {
      const result = await handler(params, conn);
      if (method === "hello") {
        conn.signedIn();
        const w = isObj(params) && typeof params.workspace === "string" ? params.workspace : null;
        conn.workspace = w;
      }
      return result;
    }
    const p: Obj = isObj(params) ? params : {};
    switch (method) {
      case "ping":
        return { pong: true };
      case "web_pair":
        return this.#pair(p);
      case "hello":
        return this.#hello(conn, p);
    }
    if (conn.workspace === null) {
      if (!INDEX_CALLS.has(method)) fail("invalid", `${method} is a call of a workspace connection`);
      if (method === "workspaces") return this.listing();
      return this.#prefs(this, p);
    }
    const ws = this.workspace(conn.workspace!);
    switch (method) {
      case "subscribe":
        conn.afterReply = () => {
          conn.subscribed = true;
          conn.attached = null;
        };
        return { subscribed: true };
      case "snapshot": {
        const snap = ws.snapshot(p);
        const hold = ws.holds.shift();
        if (hold) await hold.promise;
        return snap;
      }
      case "send":
        return this.#send(ws, p);
      case "answer":
        return this.#answer(ws, p);
      case "agent_pause":
      case "agent_resume": {
        const agent = this.#agent(ws, p.id);
        agent.paused = method === "agent_pause";
        ws.push({ kind: "agent", data: agent });
        return clone(agent);
      }
      case "agent_set_model": {
        const agent = this.#agent(ws, p.id);
        if (typeof p.model === "string" && p.model) agent.model = p.model;
        if (RUNTIMES.includes(p.runtime)) agent.runtime = p.runtime;
        ws.push({ kind: "agent", data: agent });
        return clone(agent);
      }
      case "agent_restart":
      case "agent_restart_fresh":
      case "agent_kill":
      case "agent_compact":
        return clone(this.#agent(ws, p.id));
      case "agents_list":
        return clone(ws.state.agents);
      case "tasks_list":
        return clone(ws.state.tasks);
      case "task_get": {
        const task = ws.state.tasks.find((t) => t.id === p.id) ?? fail("not_found", `no task '${p.id}'`);
        const dependencies = (task.dependencies as string[]).map((d) => ws.state.tasks.find((t) => t.id === d)).filter(Boolean);
        return clone({ task, dependencies });
      }
      case "message_get":
        return clone(ws.state.messages.find((m) => m.id === p.id) ?? fail("not_found", `no message '${p.id}'`));
      case "events": {
        const list = ws.state.events.filter((e) => p.agent == null || e.agent_id === p.agent);
        return clone(newest(list, limitOf(p.limit, 100)));
      }
      case "history": {
        const list = ws.state.messages.filter((m) => p.agent == null || m.from === p.agent || m.to === p.agent || m.to === "*");
        return clone(newest(list, limitOf(p.limit, 100)));
      }
      case "web_prefs":
        return this.#prefs(ws, p);
      case "attach": {
        const agent = typeof p.agent === "string" ? p.agent : fail("invalid", "malformed request: missing field `agent`");
        this.#agent(ws, agent);
        // The attachment replaces the subscription; its output starts after the reply.
        conn.subscribed = false;
        conn.afterReply = () => {
          conn.attached = agent;
          const screen = `\x1b[2J\x1b[H\x1b[1m${agent}\x1b[0m: mock terminal (${p.cols ?? "?"}x${p.rows ?? "?"})\r\n\r\n$ `;
          conn.push({ kind: "pty", data: { data: Buffer.from(screen).toString("base64") } });
        };
        return { attached: agent };
      }
      case "attach_input": {
        if (!conn.attached) fail("invalid", "not attached");
        const typed = Buffer.from(typeof p.data === "string" ? p.data : "", "base64");
        // An echo, as a shell's line discipline would: Enter starts a new line.
        const echo = Buffer.from(typed.toString("latin1").replace(/\r/g, "\r\n"), "latin1");
        conn.push({ kind: "pty", data: { data: echo.toString("base64") } });
        return NO_REPLY;
      }
      case "attach_resize":
        if (!conn.attached) fail("invalid", "not attached");
        return NO_REPLY;
      case "web_open":
        fail("invalid", "web_open is served by the dashboard server: run `crew web`");
      case "web_close":
        fail("invalid", "web_close is served by the dashboard server: run `crew web --stop`");
      case "load_team":
      case "web_attach":
      case "web_event":
        fail("forbidden", `web sessions cannot call ${method}`);
    }
    // The rest answer the fixture's example, for an agent that exists.
    if (method.startsWith("agent_") && typeof p.id === "string") this.#agent(ws, p.id);
    if ((method === "inbox" || method === "git_diff") && typeof p.agent === "string") this.#agent(ws, p.agent);
    const examples = results()[method];
    if (Array.isArray(examples) && examples.length) return clone(examples[0]);
    fail("invalid", `malformed request: unknown variant \`${method}\``);
  }

  #prefs(owner: { prefs: Record<string, unknown> }, p: Obj): unknown {
    if (p.set != null) {
      if (!isObj(p.set)) fail("invalid", "web preferences are a JSON object");
      if (Buffer.byteLength(JSON.stringify(p.set)) > MAX_PREFS) fail("invalid", "web preferences exceed 16 KiB");
      owner.prefs = clone(p.set);
    }
    return { prefs: clone(owner.prefs) };
  }

  #agent(ws: Workspace, id: unknown): Obj {
    return ws.state.agents.find((a) => a.id === id) ?? fail("not_found", `no agent '${String(id)}'`);
  }

  #pair(p: Obj): unknown {
    if (this.#stopping) fail("unavailable", "the web dashboard is stopping");
    const code = typeof p.code === "string" ? p.code : "";
    const expires = this.#codes.get(code);
    this.#codes.delete(code);
    if (expires === undefined || expires < Date.now()) fail("unauthorized", "this pairing code is not valid: it works once, for 2 minutes; run `crew web` for a new one");
    const secret = hex(32);
    const session: WebSession = { token: hex(32), secret, verifier: sha256(secret) };
    this.#sessions.set(session.token, session);
    return { token: session.token, verifier: session.verifier };
  }

  async #hello(conn: Connection, p: Obj): Promise<unknown> {
    if (conn.authenticated) fail("forbidden", "this connection is already authenticated as operator; open a new connection to change identity");
    if (this.#stopping) fail("unavailable", "the web dashboard is stopping");
    if (p.protocol !== PROTOCOL) fail("protocol", `client speaks protocol ${typeof p.protocol === "number" ? p.protocol : 0}, server speaks ${PROTOCOL}`);
    if (p.role !== OPERATOR) fail("unauthorized", "the web dashboard signs in the operator only");
    const session = (typeof p.token === "string" && this.#sessions.get(p.token)) || fail("unauthorized", "this browser session is not known: run `crew web`");
    const proof = (info: Obj) => {
      if (this.proofMode === "ok") info.web_proof = session.secret;
      else if (this.proofMode === "wrong") info.web_proof = hex(32);
      else if (this.proofMode === "verifier") info.web_proof = session.verifier;
      return info;
    };
    if (p.workspace === undefined || p.workspace === null) {
      conn.signedIn();
      conn.workspace = null;
      return proof({ role: OPERATOR, agent_id: null, project: null, version: this.primary.state.session.version ?? "0.1.0", protocol: PROTOCOL, pid: process.pid });
    }
    const ws = this.workspaces.find((w) => w.id === p.workspace && w.joined) ?? fail("not_found", `workspace ${String(p.workspace)} is not in this dashboard: run \`crew web\` in it`);
    const held = ws.heldHello;
    if (held) {
      ws.heldHello = null;
      await new Promise((r) => setTimeout(r, held.ms));
      fail(held.code, held.message);
    }
    if (ws.status === "incompatible") fail("protocol", `${ws.state.session.project ?? ws.id} ${ws.detail || "was started by another version of crew"}`);
    if (ws.status !== "running") fail("unavailable", `${ws.state.session.project ?? ws.id} is not running: start it (\`crew run\` in ${ws.state.session.workspace})`);
    conn.signedIn();
    conn.workspace = ws.id;
    const s = ws.state.session;
    return proof({ role: OPERATOR, agent_id: null, project: s.project ?? null, version: s.version ?? "0.1.0", protocol: PROTOCOL, pid: ws.daemon?.pid ?? s.daemon_pid ?? process.pid });
  }

  #send(ws: Workspace, p: Obj): unknown {
    const to = typeof p.to === "string" ? p.to : fail("invalid", "malformed request: missing field `to`");
    const body = typeof p.body === "string" ? p.body : fail("invalid", "malformed request: missing field `body`");
    const type = p.type ?? "message";
    if (!MESSAGE_TYPES.includes(type)) fail("invalid", `malformed request: unknown variant \`${type}\``);
    if (Buffer.byteLength(body) > MAX_BODY) fail("too_large", `message body exceeds ${MAX_BODY} bytes`);
    if (!body.trim()) fail("invalid", "a message needs a body");
    if (to !== "*" && !ws.state.agents.some((a) => a.id === to)) fail("not_found", `no agent '${to}'`);
    const id = typeof p.id === "string" && p.id ? p.id : `m-${hex(6)}`;
    const recipients = to === "*" ? ws.state.agents.map((a) => a.id as string) : [to];
    const known = ws.state.messages.find((m) => m.id === id);
    if (known) {
      // Sending an id again is a no-op (retries are safe); another message under it is not.
      if (known.from !== OPERATOR || known.to !== to || known.body !== body) fail("conflict", `message id ${id} is already used for another message`);
      return { message: clone(known), duplicate: true, recipients };
    }
    const message = {
      id,
      from: OPERATOR,
      to,
      type,
      body,
      timestamp: this.now(),
      correlation_id: p.correlation_id ?? null,
      reply_to: p.reply_to ?? null,
      metadata: p.metadata ?? null,
    };
    ws.push({ kind: "message", data: message });
    return { message: clone(message), duplicate: false, recipients };
  }

  #answer(ws: Workspace, p: Obj): unknown {
    const id = String(p.id ?? "");
    const body = typeof p.body === "string" ? p.body.trim() : "";
    if (!body) fail("invalid", "an answer needs some text");
    const question = ws.state.questions.find((q) => q.id === id);
    if (!question) {
      const asked = ws.state.messages.some((m) => m.id === id && m.to === OPERATOR && m.type === "question");
      return fail(asked ? "invalid" : "not_found", asked ? `question ${id} was already answered` : `no open question '${id}'`);
    }
    const line = String(question.body).split(/\s+/).join(" ").trim();
    const quote = [...line].length > QUOTE_CHARS ? `${[...line].slice(0, QUOTE_CHARS - 1).join("")}…` : line;
    const message = {
      id: `m-${hex(6)}`,
      from: OPERATOR,
      to: question.from,
      type: "answer",
      body: `Re: "${quote}"\n\n${body}`,
      timestamp: this.now(),
      correlation_id: question.correlation_id ?? null,
      reply_to: question.id,
      metadata: null,
    };
    // The push carries `reply_to`, which takes the question off the state.
    ws.push({ kind: "message", data: message });
    return { message: clone(message), duplicate: false, recipients: [question.from] };
  }

  // -- the controller --

  code(ttlMs = CODE_TTL_MS): string {
    const code = hex(32);
    this.#codes.set(code, Date.now() + ttlMs);
    // At most eight outstanding: a ninth drops the oldest.
    while (this.#codes.size > MAX_CODES) this.#codes.delete(this.#codes.keys().next().value!);
    return code;
  }

  pairUrl(code: string = this.code(), workspace?: string): string {
    return `${this.origin}/#pair=${code}${workspace ? `&w=${workspace}` : ""}`;
  }

  on(method: string, handler: Handler | null): void {
    if (handler) this.#handlers.set(method, handler);
    else this.#handlers.delete(method);
  }

  waitForCall(method: string, predicate?: (call: MockCall) => boolean, timeout = 5000): Promise<MockCall> {
    const ok = (c: MockCall) => c.method === method && (!predicate || predicate(c));
    const found = this.calls.find(ok);
    if (found) return Promise.resolve(found);
    const waiting = new Promise<MockCall>((resolve, reject) => {
      const waiter: Waiter = {
        ok,
        resolve,
        reject,
        timer: setTimeout(() => {
          this.#waiters = this.#waiters.filter((w) => w !== waiter);
          const seen = this.calls.map((c) => c.method).join(", ") || "none";
          reject(new Error(`no matching ${method} call within ${timeout} ms (calls so far: ${seen})`));
        }, timeout),
      };
      this.#waiters.push(waiter);
    });
    // A wait the test abandoned (it failed first) must not end the worker
    // with an unhandled rejection; whoever awaits it still gets the error.
    waiting.catch(() => {});
    return waiting;
  }

  #notify(call: MockCall): void {
    for (const w of [...this.#waiters]) {
      if (!w.ok(call)) continue;
      this.#waiters = this.#waiters.filter((x) => x !== w);
      clearTimeout(w.timer);
      w.resolve(call);
    }
  }

  push(frameOrPush: Push | PushFrame): number {
    return this.primary.push(frameOrPush);
  }

  ptyWrite(agent: string, data: string | Uint8Array): number {
    return this.primary.ptyWrite(agent, data);
  }

  retime<T>(obj: T): T {
    const o: any = obj;
    const target = isObj(o) && isObj(o.push) ? o.push.data : isObj(o) && typeof o.kind === "string" ? o.data : o;
    if (isObj(target)) target.timestamp = this.now();
    return obj;
  }

  pushes(name?: string | ((frame: PushFrame) => boolean)): PushFrame[] {
    const match =
      typeof name === "function"
        ? name
        : (f: PushFrame) => {
            if (name === undefined) return true;
            const d: Obj = isObj(f.push.data) ? f.push.data : {};
            return f.push.kind === name || d.event_type === name || d.id === name || (f.push.kind === "message" && d.type === name);
          };
    return pushLines().filter(match).map(clone);
  }

  snapshot(params: { messages?: number; events?: number } = {}): Obj {
    return this.primary.snapshot(params);
  }

  holdSnapshot(): () => void {
    return this.primary.holdSnapshot();
  }

  dropSockets(): void {
    for (const conn of this.#conns) conn.drop();
  }

  async shutdown(): Promise<void> {
    this.#stopping = true;
    for (const conn of this.#conns) if (conn.open && conn.workspace === null) conn.push({ kind: "shutdown" });
    const conns = [...this.#conns];
    for (const conn of conns) conn.close(1001, "the dashboard's server stopped");
    await Promise.all(conns.map((c) => c.ended()));
  }

  async stop(): Promise<void> {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#stopping = true;
    for (const ws of this.workspaces) for (const hold of [...ws.holds]) hold.release();
    for (const w of this.#waiters.splice(0)) {
      clearTimeout(w.timer);
      w.reject(new Error("the mock stopped"));
    }
    const conns = [...this.#conns];
    for (const conn of conns) conn.drop();
    await Promise.all([...conns.map((c) => c.ended()), ...this.#servers.map(closeServer)]);
  }
}

function listenOn(server: Server, port: number, host: string): Promise<number> {
  return new Promise((done, failed) => {
    server.once("error", failed);
    server.listen({ port, host }, () => {
      server.off("error", failed);
      done((server.address() as AddressInfo).port);
    });
  });
}

function closeServer(server: Server): Promise<void> {
  return new Promise((done) => {
    server.closeAllConnections();
    server.close(() => done());
  });
}
