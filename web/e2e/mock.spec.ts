// The mock daemon itself (dev/mock.ts), through plain HTTP and a WebSocket
// client written here: the rules of §security every request must pass, the
// framing, pairing and the proof, and the controls the browser specs rely on.
// No browser.

import { createHash, randomBytes } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import type { IncomingHttpHeaders } from "node:http";
import type { Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { NO_REPLY, WORKSPACE_IDS, contentSecurityPolicy, startMock } from "../dev/mock.ts";
import type { Mock, MockOptions } from "../dev/mock.ts";

// A bundle of known files, so that the answers can be checked exactly.
let dist = "";
test.beforeAll(() => {
  dist = mkdtempSync(join(tmpdir(), "crew-mock-dist-"));
  mkdirSync(join(dist, "assets"));
  writeFileSync(join(dist, "index.html"), '<!doctype html><title>crew</title><script type="module" src="/assets/index-0a1b2c3d.js"></script>');
  writeFileSync(join(dist, "assets/index-0a1b2c3d.js"), "export {};\n");
  writeFileSync(join(dist, "assets/index-0a1b2c3d.css"), "body{}\n");
  writeFileSync(join(dist, "assets/mono-0a1b2c3d.woff2"), Buffer.from([0x77, 0x4f, 0x46, 0x32]));
});
test.afterAll(() => rmSync(dist, { recursive: true, force: true }));

const started: Mock[] = [];
test.afterEach(async () => {
  await Promise.all(started.splice(0).map((m) => m.stop()));
});
async function start(options: MockOptions = {}): Promise<Mock> {
  const mock = await startMock({ dist, ...options });
  started.push(mock);
  return mock;
}

// ---- HTTP -------------------------------------------------------------------

type Reply = { status: number; headers: IncomingHttpHeaders; body: string };
type RequestOptions = { method?: string; host?: string; address?: string; headers?: Record<string, string>; body?: string };

function request(mock: Mock, path: string, o: RequestOptions = {}): Promise<Reply> {
  return new Promise((done, failed) => {
    const req = http.request(
      { host: o.address ?? "127.0.0.1", port: mock.port, path, method: o.method ?? "GET", headers: { Host: o.host ?? mock.host, ...o.headers }, agent: false },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c: Buffer) => chunks.push(c));
        res.on("end", () => done({ status: res.statusCode ?? 0, headers: res.headers, body: Buffer.concat(chunks).toString() }));
      },
    );
    req.on("error", failed);
    req.end(o.body);
  });
}

type UpgradeOptions = { host?: string; origin?: string | null; version?: string | null; key?: string | null; path?: string; address?: string; method?: string };
type Upgraded = { status: number; headers: IncomingHttpHeaders; socket?: Socket; head?: Buffer };

function upgrade(mock: Mock, o: UpgradeOptions = {}): Promise<Upgraded> {
  const headers: Record<string, string> = { Host: o.host ?? mock.host, Connection: "Upgrade", Upgrade: "websocket" };
  if (o.origin !== null) headers.Origin = o.origin ?? mock.origin;
  if (o.version !== null) headers["Sec-WebSocket-Version"] = o.version ?? "13";
  if (o.key !== null) headers["Sec-WebSocket-Key"] = o.key ?? randomBytes(16).toString("base64");
  return new Promise((done, failed) => {
    const req = http.request({ host: o.address ?? "127.0.0.1", port: mock.port, path: o.path ?? "/ws", method: o.method ?? "GET", headers, agent: false });
    req.on("upgrade", (res, socket, head) => done({ status: res.statusCode ?? 0, headers: res.headers, socket, head }));
    req.on("response", (res) => {
      res.resume();
      done({ status: res.statusCode ?? 0, headers: res.headers });
    });
    req.on("error", failed);
    req.end();
  });
}

// ---- a WebSocket client -------------------------------------------------------

/** The browser's side of RFC 6455, enough to test the server: masked frames
 * of any length, fragments, control frames. */
class Ws {
  readonly socket: Socket;
  /** Every text message received, parsed. */
  readonly frames: any[] = [];
  readonly pongs: Buffer[] = [];
  /** The code of the server's close frame; null while none came. */
  closeCode: number | null = null;
  closeReason = "";
  readonly closed: Promise<number | null>;
  #buffer = Buffer.alloc(0);
  #taken = new Set<unknown>();
  #waiters: { ok(f: any): boolean; done(f: any): void }[] = [];
  #next = 1;

  constructor(socket: Socket, head: Buffer) {
    this.socket = socket;
    socket.on("data", (d: Buffer) => this.#read(d));
    socket.on("error", () => {});
    this.closed = new Promise((done) => socket.on("close", () => done(this.closeCode)));
    if (head.length) this.#read(head);
  }

  static async open(mock: Mock, o: UpgradeOptions = {}): Promise<Ws> {
    const up = await upgrade(mock, o);
    if (up.status !== 101 || !up.socket) throw new Error(`upgrade answered ${up.status}`);
    return new Ws(up.socket, up.head ?? Buffer.alloc(0));
  }

  frame(opcode: number, payload: Buffer, fin = true, masked = true): void {
    const len = payload.length;
    const head = len < 126 ? Buffer.alloc(2) : len < 65_536 ? Buffer.alloc(4) : Buffer.alloc(10);
    head[0] = (fin ? 0x80 : 0) | opcode;
    if (len < 126) head[1] = len;
    else if (len < 65_536) {
      head[1] = 126;
      head.writeUInt16BE(len, 2);
    } else {
      head[1] = 127;
      head.writeBigUInt64BE(BigInt(len), 2);
    }
    if (!masked) {
      this.socket.write(Buffer.concat([head, payload]));
      return;
    }
    head[1] |= 0x80;
    const mask = randomBytes(4);
    const body = Buffer.from(payload);
    for (let i = 0; i < body.length; i++) body[i] ^= mask[i & 3];
    this.socket.write(Buffer.concat([head, mask, body]));
  }

  sendText(text: string): void {
    this.frame(0x1, Buffer.from(text));
  }

  /** Send a request; its id. */
  request(method: string, params?: unknown): number {
    const id = this.#next++;
    this.sendText(JSON.stringify(params === undefined ? { id, method } : { id, method, params }));
    return id;
  }

  call(method: string, params?: unknown): Promise<any> {
    const id = this.request(method, params);
    return this.next((f) => f.id === id);
  }

  /** The first frame not taken yet that `ok` accepts, received or to come. */
  next(ok: (f: any) => boolean, timeout = 3000): Promise<any> {
    const found = this.frames.find((f) => !this.#taken.has(f) && ok(f));
    if (found) {
      this.#taken.add(found);
      return Promise.resolve(found);
    }
    return new Promise((done, failed) => {
      const timer = setTimeout(() => failed(new Error(`no such frame within ${timeout} ms; got ${JSON.stringify(this.frames).slice(0, 400)}`)), timeout);
      this.#waiters.push({
        ok,
        done: (f) => {
          clearTimeout(timer);
          done(f);
        },
      });
    });
  }

  close(code = 1000): void {
    const payload = Buffer.alloc(2);
    payload.writeUInt16BE(code, 0);
    this.frame(0x8, payload);
  }

  #read(d: Buffer): void {
    this.#buffer = Buffer.concat([this.#buffer, d]);
    for (;;) {
      const b = this.#buffer;
      if (b.length < 2) return;
      expect(b[1] & 0x80, "a server does not mask").toBe(0);
      let len = b[1] & 0x7f;
      let at = 2;
      if (len === 126) {
        if (b.length < 4) return;
        len = b.readUInt16BE(2);
        at = 4;
      } else if (len === 127) {
        if (b.length < 10) return;
        len = Number(b.readBigUInt64BE(2));
        at = 10;
      }
      if (b.length < at + len) return;
      const opcode = b[0] & 0x0f;
      const payload = b.subarray(at, at + len);
      this.#buffer = b.subarray(at + len);
      if (opcode === 0x1) {
        const frame = JSON.parse(payload.toString());
        this.frames.push(frame);
        const w = this.#waiters.find((x) => x.ok(frame));
        if (w) {
          this.#waiters.splice(this.#waiters.indexOf(w), 1);
          this.#taken.add(frame);
          w.done(frame);
        }
      } else if (opcode === 0x8) {
        this.closeCode = payload.length >= 2 ? payload.readUInt16BE(0) : 1005;
        this.closeReason = payload.subarray(2).toString();
      } else if (opcode === 0xa) {
        this.pongs.push(Buffer.from(payload));
      }
    }
  }
}

const sha256 = (s: string) => createHash("sha256").update(s).digest("hex");
/** A workspace connection's hello (the first workspace's), unless `extra` says otherwise; `{ workspace: undefined }` is the index's. */
const hello = (token: string, extra: object = {}) => ({ role: "operator", token, client: "test", protocol: 6, workspace: WORKSPACE_IDS[0], ...extra });
const quiet = (ms: number) => new Promise((done) => setTimeout(done, ms));

/** A connection that paired and said hello: to the first workspace, unless `extra` says otherwise. */
async function signedIn(mock: Mock, extra: object = {}): Promise<{ ws: Ws; token: string; verifier: string; info: any }> {
  const ws = await Ws.open(mock);
  const paired = await ws.call("web_pair", { code: mock.code() });
  const answered = await ws.call("hello", hello(paired.result.token, extra));
  expect(answered.error).toBeUndefined();
  return { ws, token: paired.result.token, verifier: paired.result.verifier, info: answered.result };
}

// ---- the listener -------------------------------------------------------------

test("a fresh name per start, on one port of both loopback addresses", async () => {
  const one = await start();
  const two = await start();
  expect(one.label).toMatch(/^[0-9a-f]{32}$/);
  expect(one.label).not.toBe(two.label);
  expect(one.origin).toBe(`http://${one.label}.localhost:${one.port}`);
  expect(one.pairUrl("abc")).toBe(`${one.origin}/#pair=abc`);
  expect(one.state.session.web).toBe(`${one.origin}/`);
  expect((await request(one, "/")).status).toBe(200);
  if (one.addresses.includes("::1")) {
    expect((await request(one, "/", { address: "::1" })).status).toBe(200);
    expect(one.requests.map((r) => r.address)).toEqual(["127.0.0.1", "::1"]);
  }
  // A port named explicitly is that port or an error.
  await expect(startMock({ dist, port: one.port })).rejects.toThrow(/EADDRINUSE/);
});

test("answers only to its own name", async () => {
  const mock = await start();
  for (const host of [`127.0.0.1:${mock.port}`, `localhost:${mock.port}`, `${"0".repeat(32)}.localhost:${mock.port}`, `evil.example:${mock.port}`, mock.label + ".localhost"]) {
    expect((await request(mock, "/", { host })).status, host).toBe(421);
  }
  expect((await request(mock, "/")).status).toBe(200);
});

test("GET only, and no body", async () => {
  const mock = await start();
  for (const method of ["POST", "PUT", "DELETE", "OPTIONS", "HEAD"]) {
    const reply = await request(mock, "/", { method, headers: { "Content-Length": "0" } });
    expect(reply.status, method).toBe(405);
    expect(reply.headers.allow).toBe("GET");
  }
  expect((await request(mock, "/", { headers: { "Content-Length": "5" }, body: "hello" })).status).toBe(400);
  // The name is checked before the method.
  expect((await request(mock, "/", { method: "POST", host: "localhost", headers: { "Content-Length": "0" } })).status).toBe(421);
});

test("the document and the files, with the headers of §security", async () => {
  const mock = await start();
  const doc = await request(mock, "/");
  expect(doc.status).toBe(200);
  expect(doc.body).toContain("<title>crew</title>");
  expect(doc.headers["content-type"]).toBe("text/html; charset=utf-8");
  expect(doc.headers["cache-control"]).toBe("no-store");
  expect(doc.headers["x-frame-options"]).toBe("DENY");
  expect(doc.headers["cross-origin-opener-policy"]).toBe("same-origin");
  expect(doc.headers["content-security-policy"]).toBe(
    `default-src 'none'; script-src 'self'; style-src-elem 'self' 'unsafe-inline'; style-src-attr 'none'; img-src 'self' data:; font-src 'self'; connect-src 'self' ws://${mock.label}.localhost:${mock.port}; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
  );
  expect(contentSecurityPolicy(mock.label, mock.port)).toBe(doc.headers["content-security-policy"]);

  const files = { "/assets/index-0a1b2c3d.js": "text/javascript; charset=utf-8", "/assets/index-0a1b2c3d.css": "text/css; charset=utf-8", "/assets/mono-0a1b2c3d.woff2": "font/woff2" };
  for (const [path, type] of Object.entries(files)) {
    const file = await request(mock, `${path}?v=1`);
    expect(file.status, path).toBe(200);
    expect(file.headers["content-type"]).toBe(type);
    expect(file.headers["cache-control"]).toBe("public, max-age=31536000, immutable");
    expect(file.headers["content-security-policy"]).toBeUndefined();
  }
  const missing = ["/nope", "/assets", "/assets/../index.html", "/assets/%2e%2e/index.html", "/..%2findex.html", "/ws"];
  for (const path of missing) expect((await request(mock, path)).status, path).toBe(404);

  const replies = [doc, await request(mock, "/assets/index-0a1b2c3d.js"), await request(mock, "/nope"), await request(mock, "/", { host: "localhost" })];
  for (const r of replies) {
    expect(r.headers["x-content-type-options"]).toBe("nosniff");
    expect(r.headers["referrer-policy"]).toBe("no-referrer");
    expect(r.headers["cross-origin-resource-policy"]).toBe("same-origin");
    expect(r.headers.connection).toBe("close");
    expect(Object.keys(r.headers).filter((h) => h.startsWith("access-control-"))).toEqual([]);
  }
});

test("the upgrade needs the origin, the version and a key", async () => {
  const mock = await start();
  for (const origin of [null, "http://evil.example", "http://127.0.0.1:5173", `http://127.0.0.1:${mock.port}`, `http://localhost:${mock.port}`]) {
    expect((await upgrade(mock, { origin })).status, String(origin)).toBe(403);
  }
  expect((await upgrade(mock, { version: null })).status).toBe(400);
  expect((await upgrade(mock, { version: "8" })).status).toBe(400);
  expect((await upgrade(mock, { key: null })).status).toBe(400);
  expect((await upgrade(mock, { host: `localhost:${mock.port}` })).status).toBe(421);
  expect((await upgrade(mock, { path: "/socket" })).status).toBe(404);
  const ok = await upgrade(mock, {});
  expect(ok.status).toBe(101);
  ok.socket?.destroy();
  if (mock.addresses.includes("::1")) {
    const v6 = await upgrade(mock, { address: "::1" });
    expect(v6.status).toBe(101);
    v6.socket?.destroy();
  }
});

test("behind the dev proxy any name is answered", async () => {
  const mock = await start({ strictHost: false });
  expect((await request(mock, "/", { host: "localhost:5173" })).status).toBe(200);
  const ws = await Ws.open(mock, { host: "localhost:5173", origin: "http://localhost:5173" });
  expect((await ws.call("ping")).result).toEqual({ pong: true });
  // The other rules stay.
  expect((await request(mock, "/", { host: "localhost:5173", method: "POST", headers: { "Content-Length": "0" } })).status).toBe(405);
});

test("every request is recorded", async () => {
  const mock = await start();
  const t0 = Date.now();
  await request(mock, "/");
  await request(mock, "/assets/index-0a1b2c3d.js");
  await request(mock, "/", { method: "POST", headers: { "Content-Length": "0" } });
  const ws = await Ws.open(mock);
  await ws.call("ping");
  expect(mock.requests.map(({ method, path, status, upgrade }) => ({ method, path, status, upgrade }))).toEqual([
    { method: "GET", path: "/", status: 200, upgrade: false },
    { method: "GET", path: "/assets/index-0a1b2c3d.js", status: 200, upgrade: false },
    { method: "POST", path: "/", status: 405, upgrade: false },
    { method: "GET", path: "/ws", status: 101, upgrade: true },
  ]);
  for (const r of mock.requests) expect(r.time).toBeGreaterThanOrEqual(t0);
  expect(mock.requests[3].origin).toBe(mock.origin);
  expect(mock.calls.map((c) => c.method)).toEqual(["ping"]);
});

// ---- the WebSocket ------------------------------------------------------------

test("framing: fragments, long messages both ways, ping, close", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  // A request in three fragments, with a ping between them.
  const text = Buffer.from(JSON.stringify({ id: 50, method: "ping" }));
  ws.frame(0x1, text.subarray(0, 5), false);
  ws.frame(0x0, text.subarray(5, 12), false);
  ws.frame(0x9, Buffer.from("are you there"));
  ws.frame(0x0, text.subarray(12), true);
  expect((await ws.next((f) => f.id === 50)).result).toEqual({ pong: true });
  await expect.poll(() => ws.pongs.map(String)).toEqual(["are you there"]);

  // 16-bit lengths both ways.
  const prefs = { note: "x".repeat(2000) };
  expect((await ws.call("web_prefs", { set: prefs })).result).toEqual({ prefs });
  // 64-bit lengths both ways: the reply repeats the body.
  const body = "y".repeat(65_000);
  const sent = await ws.call("send", { to: "claude-1", body, id: "big-1" });
  expect(sent.result.message.body).toBe(body);

  ws.close(1000);
  expect(await ws.closed).toBe(1000);
});

test("a binary message closes with 1003; a large one before hello with 1009", async () => {
  const mock = await start();
  const binary = await Ws.open(mock);
  binary.frame(0x2, Buffer.from([1, 2, 3]));
  expect(await binary.closed).toBe(1003);

  const large = await Ws.open(mock);
  large.sendText(JSON.stringify({ id: 1, method: "web_pair", params: { code: "z".repeat(5000) } }));
  expect(await large.closed).toBe(1009);

  const unmasked = await Ws.open(mock);
  unmasked.frame(0x1, Buffer.from('{"id":1,"method":"ping"}'), true, false);
  expect(await unmasked.closed).toBe(1002);
});

test("a connection that does not say hello in time is closed", async () => {
  const mock = await start({ helloTimeoutMs: 200 });
  const ws = await Ws.open(mock);
  expect(await ws.closed).toBe(1008);
});

test("malformed requests are answered invalid, with the id when there is one", async () => {
  const mock = await start();
  const ws = await Ws.open(mock);
  ws.sendText("{nope");
  expect((await ws.next((f) => f.error)).error.code).toBe("invalid");
  ws.sendText(JSON.stringify({ id: 7, params: {} }));
  expect(await ws.next((f) => f.id === 7)).toEqual({ id: 7, error: { code: "invalid", message: "malformed request: missing field `method`" } });
  expect(mock.calls).toEqual([]);
});

// ---- pairing and hello ----------------------------------------------------------

test("nothing but ping, web_pair and hello before hello", async () => {
  const mock = await start();
  const ws = await Ws.open(mock);
  expect((await ws.call("ping")).result).toEqual({ pong: true });
  for (const method of ["snapshot", "subscribe", "send", "web_prefs", "attach"]) {
    expect((await ws.call(method, {})).error, method).toEqual({ code: "unauthorized", message: "say hello first" });
  }
});

test("a pairing code works once, and the proof hashes to the verifier", async () => {
  const mock = await start();
  const code = mock.code();
  const ws = await Ws.open(mock);
  const paired = await ws.call("web_pair", { code });
  expect(Object.keys(paired.result).sort()).toEqual(["token", "verifier"]);
  const { token, verifier } = paired.result;
  expect(verifier).toMatch(/^[0-9a-f]{64}$/);

  // Spent, unknown, expired.
  expect((await ws.call("web_pair", { code })).error.code).toBe("unauthorized");
  expect((await ws.call("web_pair", { code: "nope" })).error.code).toBe("unauthorized");
  const expired = mock.code(-1);
  expect((await ws.call("web_pair", { code: expired })).error.code).toBe("unauthorized");

  const info = (await ws.call("hello", hello(token))).result;
  expect(info).toMatchObject({ role: "operator", agent_id: null, project: "checkout-rewrite", protocol: 6 });
  expect(typeof info.pid).toBe("number");
  expect(sha256(info.web_proof)).toBe(verifier);
  expect(info.web_proof).not.toBe(verifier);
  // Once per connection.
  expect((await ws.call("hello", hello(token))).error.code).toBe("forbidden");

  // The same session on another connection proves the same secret.
  const again = await Ws.open(mock);
  expect((await again.call("hello", hello(token))).result.web_proof).toBe(info.web_proof);
  // Another pairing is another session.
  const other = await signedIn(mock);
  expect(other.verifier).not.toBe(verifier);
});

test("hello refuses a wrong token, role or protocol, without a proof", async () => {
  const mock = await start();
  const { token } = await signedIn(mock);
  const ws = await Ws.open(mock);
  const refused = [
    [hello("nope"), "unauthorized"],
    [{ role: "operator", client: "test", protocol: 6 }, "unauthorized"],
    [hello(token, { role: "agent", agent_id: "claude-1" }), "unauthorized"],
    [hello(token, { protocol: 4 }), "protocol"],
  ] as const;
  for (const [params, code] of refused) {
    const reply = await ws.call("hello", params);
    expect(reply.error.code).toBe(code);
    expect(reply.result).toBeUndefined();
  }
  expect((await ws.call("hello", hello(token, { protocol: 4 }))).error.message).toBe("client speaks protocol 4, server speaks 6");
  // A failed hello leaves the connection as it was.
  expect((await ws.call("snapshot", {})).error.code).toBe("unauthorized");
  expect((await ws.call("hello", hello(token))).result.role).toBe("operator");
});

test("proofMode makes hello misbehave", async () => {
  const mock = await start();
  const { token, verifier } = await signedIn(mock);
  const proof = async () => (await (await Ws.open(mock)).call("hello", hello(token))).result.web_proof;

  mock.proofMode = "wrong";
  const wrong = await proof();
  expect(typeof wrong).toBe("string");
  expect(sha256(wrong)).not.toBe(verifier);
  mock.proofMode = "verifier";
  expect(await proof()).toBe(verifier);
  mock.proofMode = "missing";
  expect(await proof()).toBeUndefined();
  mock.proofMode = "ok";
  expect(sha256(await proof())).toBe(verifier);
});

// ---- state, pushes and calls ------------------------------------------------------

test("subscribe, then pushes; an unknown kind reaches a subscriber as it is", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  const listener = await signedIn(mock);
  // Not subscribed: nothing.
  expect(mock.push({ kind: "resync" })).toBe(0);
  expect(await ws.call("subscribe")).toEqual({ id: expect.any(Number), result: { subscribed: true } });
  const future = mock.pushes("future_kind")[0];
  expect(future).toEqual({ push: { kind: "future_kind", data: { anything: [1, 2, 3] } } });
  expect(mock.push(future)).toBe(1);
  expect(await ws.next((f) => f.push)).toEqual(future);
  // `{kind, data}` is taken too.
  mock.push({ kind: "agent_removed", data: { id: "claude-6" } });
  expect(await ws.next((f) => f.push)).toEqual({ push: { kind: "agent_removed", data: { id: "claude-6" } } });
  expect(listener.ws.frames.filter((f) => f.push)).toEqual([]);
});

test("the state follows what was pushed; snapshot reads it", async () => {
  const mock = await start({ fixture: "team10" });
  const { ws } = await signedIn(mock);
  const first = (await ws.call("snapshot", { messages: 300, events: 1000 })).result;
  expect(first.agents).toHaveLength(11);
  expect(first.server_time).toBeGreaterThanOrEqual(1_760_000_000_000);
  expect(first.server_time).toBeLessThan(1_760_000_000_000 + 60_000);
  expect(first.session.web).toBe(mock.url);

  const event = mock.retime(mock.pushes("agent.error")[0]);
  expect(event.push.data).toMatchObject({ seq: 1001, timestamp: expect.any(Number) });
  expect((event.push.data as any).timestamp).toBeGreaterThanOrEqual(first.server_time);
  mock.push(event);
  mock.push(mock.pushes("agent_removed")[0]);
  mock.push(mock.pushes("question")[0]);
  const later = (await ws.call("snapshot", { messages: 2, events: 1 })).result;
  expect(later.agents.map((a: any) => a.id)).not.toContain("claude-6");
  expect(later.events.map((e: any) => e.seq)).toEqual([1001]);
  expect(later.messages).toHaveLength(2);
  expect(later.messages[1].id).toBe("m-9001");
  expect(later.questions.map((q: any) => q.id)).toEqual(["m-9001"]);
  expect(mock.snapshot({ messages: 0 }).messages).toEqual([]);
});

test("holdSnapshot: a push between the request and the reply comes first and is not in it", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  await ws.call("subscribe");
  const release = mock.holdSnapshot();
  const id = ws.request("snapshot", { messages: 300, events: 1000 });
  await mock.waitForCall("snapshot");
  const late = mock.retime(mock.pushes("m-0101")[0]);
  expect(mock.push(late)).toBe(1);
  await ws.next((f) => f.push?.kind === "message");
  await quiet(100);
  expect(ws.frames.some((f) => f.id === id)).toBe(false);
  release();
  const reply = await ws.next((f) => f.id === id);
  // Captured when it was asked for.
  expect(reply.result.messages.map((m: any) => m.id)).not.toContain("m-0101");
  expect(ws.frames.findIndex((f) => f.push)).toBeLessThan(ws.frames.indexOf(reply));
  // The state has it now; the next snapshot is not held.
  expect((await ws.call("snapshot", {})).result.messages.map((m: any) => m.id)).toContain("m-0101");
});

test("send stores and pushes a message once per id", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  await ws.call("subscribe");
  const params = { to: "claude-1", type: "message", body: "Please add the rounding rule.", id: "web-1" };
  const first = (await ws.call("send", params)).result;
  expect(first).toMatchObject({ duplicate: false, recipients: ["claude-1"], message: { id: "web-1", from: "operator", to: "claude-1", body: params.body, reply_to: null } });
  expect(await ws.next((f) => f.push?.kind === "message")).toEqual({ push: { kind: "message", data: first.message } });
  const again = (await ws.call("send", params)).result;
  expect(again).toMatchObject({ duplicate: true, message: first.message });
  expect((await ws.call("send", { ...params, body: "something else" })).error.code).toBe("conflict");
  expect((await ws.call("send", { to: "nobody", body: "hi" })).error.code).toBe("not_found");
  const broadcast = (await ws.call("send", { to: "*", body: "everyone" })).result;
  expect(broadcast.recipients).toHaveLength(mock.state.agents.length);
  expect(broadcast.message.id).toMatch(/^m-/);
  await quiet(100);
  expect(ws.frames.filter((f) => f.push?.kind === "message").map((f) => f.push.data.id)).toEqual(["web-1", broadcast.message.id]);
  const call = await mock.waitForCall("send", (c) => c.params.id === "web-1");
  expect(call.params).toEqual(params);
  expect(call.conn.authenticated).toBe(true);
});

test("answer pushes the operator's answer and closes the question", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  await ws.call("subscribe");
  mock.push(mock.pushes("question")[0]);
  await ws.next((f) => f.push?.data?.id === "m-9001");
  expect(mock.state.questions.map((q) => q.id)).toEqual(["m-9001"]);

  const answered = (await ws.call("answer", { id: "m-9001", body: "Convert in the adapter" })).result;
  expect(answered.message).toMatchObject({ from: "operator", to: "orchestrator", type: "answer", reply_to: "m-9001" });
  expect(answered.message.body).toMatch(/^Re: "The legacy export.*"\n\nConvert in the adapter$/s);
  const pushed = await ws.next((f) => f.push?.data?.reply_to === "m-9001");
  expect(pushed.push.data).toEqual(answered.message);
  expect((await ws.call("snapshot", {})).result.questions).toEqual([]);
  expect((await ws.call("answer", { id: "m-9001", body: "again" })).error.code).toBe("invalid");
  expect((await ws.call("answer", { id: "m-nope", body: "x" })).error.code).toBe("not_found");
});

test("pause and resume push the agent; other actions answer it", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  await ws.call("subscribe");
  const paused = (await ws.call("agent_pause", { id: "claude-1" })).result;
  expect(paused).toMatchObject({ id: "claude-1", paused: true });
  expect((await ws.next((f) => f.push?.kind === "agent")).push.data).toEqual(paused);
  expect((await ws.call("snapshot", {})).result.agents.find((a: any) => a.id === "claude-1").paused).toBe(true);
  expect((await ws.call("agent_resume", { id: "claude-1" })).result.paused).toBe(false);
  expect((await ws.next((f) => f.push?.kind === "agent")).push.data.paused).toBe(false);
  for (const method of ["agent_restart", "agent_restart_fresh", "agent_kill", "agent_compact"]) {
    expect((await ws.call(method, { id: "claude-2" })).result.id, method).toBe("claude-2");
  }
  expect((await ws.call("agent_kill", { id: "claude-99" })).error).toEqual({ code: "not_found", message: "no agent 'claude-99'" });
  expect((await ws.call("agent_output", { id: "claude-99" })).error.code).toBe("not_found");
});

test("the other calls answer from the state or results.json, unless overridden", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  const results = (await import("../../crates/crew/tests/fixtures/web/results.json", { with: { type: "json" } })).default as Record<string, unknown[]>;
  expect((await ws.call("models_list")).result).toEqual(results.models_list[0]);
  expect((await ws.call("agent_transcript", { id: "claude-1", limit: 400 })).result).toEqual(results.agent_transcript[0]);
  expect((await ws.call("shutdown", { stop_agents: true })).result).toEqual({ stopping: true });
  const card = (await ws.call("task_get", { id: "billing" })).result;
  expect(card.task.id).toBe("billing");
  expect(card.dependencies.map((t: any) => t.id)).toEqual(mock.state.tasks.find((t) => t.id === "billing")!.dependencies);
  expect((await ws.call("task_get", { id: "nope" })).error.code).toBe("not_found");
  expect((await ws.call("events", { agent: "claude-5", limit: 10 })).result.every((e: any) => e.agent_id === "claude-5")).toBe(true);
  expect((await ws.call("history", { limit: 3 })).result).toEqual(mock.state.messages.slice(-3));
  expect((await ws.call("message_get", { id: "m-0001" })).result.id).toBe("m-0001");
  // The daemon no longer serves the dashboard's own calls; a web session cannot load a team.
  for (const method of ["web_open", "web_close"]) expect((await ws.call(method, {})).error.code, method).toBe("invalid");
  expect((await ws.call("load_team", {})).error.code).toBe("forbidden");
  expect((await ws.call("no_such_call", {})).error).toEqual({ code: "invalid", message: "malformed request: unknown variant `no_such_call`" });

  mock.on("models_list", () => {
    throw { code: "internal", message: "no models today" };
  });
  expect((await ws.call("models_list")).error).toEqual({ code: "internal", message: "no models today" });
  mock.on("agent_output", async (params, conn) => ({ text: `${params.id} on ${conn.id}`, alive: true }));
  expect((await ws.call("agent_output", { id: "claude-3" })).result.text).toMatch(/^claude-3 on \d+$/);
  mock.on("agent_focus", () => NO_REPLY);
  const unanswered = ws.request("agent_focus", { id: "claude-3" });
  await mock.waitForCall("agent_focus");
  await quiet(100);
  expect(ws.frames.some((f) => f.id === unanswered)).toBe(false);
  mock.on("models_list", null);
  expect((await ws.call("models_list")).result).toEqual(results.models_list[0]);
});

test("web_prefs keeps an object of at most 16 KiB: the workspace's on its connection, the server's on the index", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  expect((await ws.call("web_prefs", {})).result).toEqual({ prefs: {} });
  expect((await ws.call("web_prefs", { set: { layout: { "claude-1": [1, 2] } } })).result.prefs.layout).toEqual({ "claude-1": [1, 2] });
  expect((await (await signedIn(mock)).ws.call("web_prefs")).result).toEqual({ prefs: { layout: { "claude-1": [1, 2] } } });
  expect(mock.workspace().prefs).toEqual({ layout: { "claude-1": [1, 2] } });
  expect((await ws.call("web_prefs", { set: [1] })).error.code).toBe("invalid");
  expect((await ws.call("web_prefs", { set: { big: "x".repeat(17_000) } })).error.code).toBe("invalid");
  const index = await signedIn(mock, { workspace: undefined });
  expect((await index.ws.call("web_prefs", { set: { keys: false, last: WORKSPACE_IDS[0] } })).result.prefs.keys).toBe(false);
  expect(mock.prefs).toEqual({ keys: false, last: WORKSPACE_IDS[0] });
  expect(mock.workspace().prefs).toEqual({ layout: { "claude-1": [1, 2] } });
});

test("attach turns a connection into a terminal", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  const decode = (f: any) => Buffer.from(f.push.data.data, "base64").toString();
  expect((await ws.call("attach", { agent: "claude-3", rows: 24, cols: 80 })).result).toEqual({ attached: "claude-3" });
  const screen = await ws.next((f) => f.push?.kind === "pty");
  expect(decode(screen)).toContain("claude-3");
  expect(ws.frames.indexOf(screen)).toBeGreaterThan(0);

  const input = ws.request("attach_input", { data: Buffer.from("ls\r").toString("base64") });
  expect(decode(await ws.next((f) => f.push?.kind === "pty"))).toBe("ls\r\n");
  const resize = ws.request("attach_resize", { rows: 30, cols: 100 });
  expect(mock.ptyWrite("claude-3", "output")).toBe(1);
  expect(decode(await ws.next((f) => f.push?.kind === "pty"))).toBe("output");
  // State pushes do not go to a terminal; pty pushes do.
  expect(mock.push({ kind: "agent_removed", data: { id: "claude-6" } })).toBe(0);
  expect(mock.push({ kind: "pty_closed" })).toBe(1);
  await ws.next((f) => f.push?.kind === "pty_closed");
  await quiet(100);
  expect(ws.frames.filter((f) => f.id === input || f.id === resize)).toEqual([]);
  expect((await ws.call("attach", { agent: "claude-99", rows: 1, cols: 1 })).error.code).toBe("not_found");
});

test("dropSockets ends every WebSocket without a close frame", async () => {
  const mock = await start();
  const one = await signedIn(mock);
  const two = await Ws.open(mock);
  expect(mock.connections).toHaveLength(2);
  mock.dropSockets();
  expect(await one.ws.closed).toBeNull();
  expect(await two.closed).toBeNull();
  await expect.poll(() => mock.connections.length).toBe(0);
  // The session outlives its connections.
  expect((await (await Ws.open(mock)).call("hello", hello(one.token))).result.role).toBe("operator");
});

test("shutdown (the server's) pushes shutdown on the index, closes every connection, and refuses from then on", async () => {
  const mock = await start();
  const index = await signedIn(mock, { workspace: undefined });
  const { ws, token } = await signedIn(mock);
  await ws.call("subscribe");
  const terminal = await signedIn(mock);
  await terminal.ws.call("attach", { agent: "claude-1", rows: 24, cols: 80 });
  const code = mock.code();
  await mock.shutdown();
  expect(index.ws.frames.at(-1)).toEqual({ push: { kind: "shutdown" } });
  expect(ws.frames.some((f) => f.push?.kind === "shutdown")).toBe(false);
  expect(await index.ws.closed).toBe(1001);
  expect(await ws.closed).toBe(1001);
  expect(await terminal.ws.closed).toBe(1001);
  const late = await Ws.open(mock);
  expect((await late.call("hello", hello(token))).error.code).toBe("unavailable");
  expect((await late.call("web_pair", { code })).error.code).toBe("unavailable");
});

// ---- the index and the workspaces ------------------------------------------------

const TWO = { workspaces: [{ fixture: "snapshot" as const }, { fixture: "team10" as const, project: "search", workspace: "/home/op/work/search" }], others: [{ id: "4c2e8a1b9d0f3e57", project: "docs", workspace: "/home/op/work/docs" }] };

test("the index: the server's hello, the listing with its revision, and nothing of a daemon", async () => {
  const mock = await start(TWO);
  const { ws, info } = await signedIn(mock, { workspace: undefined });
  expect(info).toMatchObject({ role: "operator", project: null, protocol: 6 });
  expect(typeof info.web_proof).toBe("string");
  const listing = (await ws.call("workspaces")).result;
  expect(listing.rev).toBe(mock.rev);
  expect(listing.joined.map((e: any) => [e.id, e.project, e.state, e.generation])).toEqual([
    [WORKSPACE_IDS[0], "checkout-rewrite", "running", 1],
    [WORKSPACE_IDS[1], "search", "running", 2],
  ]);
  expect(listing.joined[1].summary).toMatchObject({ agents: 11, project: "search" });
  expect(listing.others).toEqual(TWO.others);
  for (const method of ["subscribe", "snapshot", "send"]) expect((await ws.call(method, {})).error.code, method).toBe("invalid");
  expect((await ws.call("ping")).result).toEqual({ pong: true });
});

test("a workspace's hello: refused not_found, unavailable, protocol as the listing says; each its own daemon", async () => {
  const mock = await start(TWO);
  const { token } = await signedIn(mock);
  const say = async (workspace: string) => (await (await Ws.open(mock)).call("hello", hello(token, { workspace })));
  expect((await say("ffffffffffffffff")).error.code).toBe("not_found");
  expect((await say(WORKSPACE_IDS[1])).result.project).toBe("search");
  mock.stopDaemon(WORKSPACE_IDS[1]);
  expect((await say(WORKSPACE_IDS[1])).error.code).toBe("unavailable");
  mock.setStatus(WORKSPACE_IDS[1], "incompatible", "started by another version of crew (protocol 5): restart the team");
  expect((await say(WORKSPACE_IDS[1])).error.code).toBe("protocol");
  mock.remove(WORKSPACE_IDS[0]);
  expect((await say(WORKSPACE_IDS[0])).error.code).toBe("not_found");
  // Pushes reach their own workspace's subscribers only.
  const b = await Ws.open(mock);
  mock.startDaemon(WORKSPACE_IDS[1]);
  await b.call("hello", hello(token, { workspace: WORKSPACE_IDS[1] }));
  await b.call("subscribe");
  expect(mock.workspace(1).push({ kind: "agent_removed", data: { id: "claude-1" } })).toBe(1);
  expect(mock.push({ kind: "agent_removed", data: { id: "claude-1" } })).toBe(0);
});

test("a daemon that stops: its shutdown push, 4001, the listing says stopped; it starts again with another pid", async () => {
  const mock = await start(TWO);
  const index = await signedIn(mock, { workspace: undefined });
  const { ws } = await signedIn(mock);
  await ws.call("subscribe");
  const rev = mock.rev;
  mock.stopDaemon(WORKSPACE_IDS[0]);
  expect(await ws.next((f) => f.push?.kind === "shutdown")).toEqual({ push: { kind: "shutdown" } });
  expect(await ws.closed).toBe(4001);
  const pushed = (await index.ws.next((f) => f.push?.kind === "workspaces")).push.data;
  expect(pushed.rev).toBe(rev + 1);
  expect(pushed.joined[0]).toMatchObject({ state: "stopped", daemon: null });
  expect(pushed.joined[0].summary.agents).toBe(7);
  mock.startDaemon(WORKSPACE_IDS[0]);
  const again = (await index.ws.next((f) => f.push?.kind === "workspaces")).push.data;
  expect(again.joined[0].state).toBe("running");
  expect(again.joined[0].daemon.pid).not.toBe(pushed.joined[0].daemon?.pid);
});

test("removed and joined again in one listing; the old connection's 4002 held back, nothing of it forwarded meanwhile", async () => {
  const mock = await start(TWO);
  const index = await signedIn(mock, { workspace: undefined });
  const { ws } = await signedIn(mock);
  const t0 = Date.now();
  mock.removeAndRejoin(WORKSPACE_IDS[0], { holdCloseMs: 300 });
  const pushed = (await index.ws.next((f) => f.push?.kind === "workspaces")).push.data;
  expect(pushed.joined.find((e: any) => e.id === WORKSPACE_IDS[0]).generation).toBe(3);
  const asked = ws.request("ping");
  expect(await ws.closed).toBe(4002);
  expect(Date.now() - t0).toBeGreaterThanOrEqual(280);
  expect(ws.frames.some((f) => f.id === asked)).toBe(false);
  mock.remove(WORKSPACE_IDS[1]);
  expect((await index.ws.next((f) => f.push?.kind === "workspaces")).push.data.joined.map((e: any) => e.id)).toEqual([WORKSPACE_IDS[0]]);
  mock.join(WORKSPACE_IDS[1]);
  expect((await index.ws.next((f) => f.push?.kind === "workspaces")).push.data.joined.at(-1)).toMatchObject({ id: WORKSPACE_IDS[1], generation: 4 });
});

test("holdHello answers the next hello late, with the code given; a message with a line break closes with 1008", async () => {
  const mock = await start(TWO);
  const { token } = await signedIn(mock);
  mock.workspace(0).holdHello("protocol", 200);
  const t0 = Date.now();
  const late = await (await Ws.open(mock)).call("hello", hello(token));
  expect(late.error.code).toBe("protocol");
  expect(Date.now() - t0).toBeGreaterThanOrEqual(190);
  expect((await (await Ws.open(mock)).call("hello", hello(token))).result.role).toBe("operator");
  const ws = await Ws.open(mock);
  ws.sendText('{"id":1,"method":"ping"}\n{"id":2,"method":"ping"}');
  expect(await ws.closed).toBe(1008);
  expect(ws.frames).toEqual([]);
});

test("the summary follows the daemon's state: a question raises it and pushes the listing", async () => {
  const mock = await start(TWO);
  const index = await signedIn(mock, { workspace: undefined });
  const before = mock.listing().joined[1].summary;
  mock.workspace(1).push({ kind: "message", data: { id: "q-77", from: "orchestrator", to: "operator", type: "question", body: "Ship it?", timestamp: mock.now() } });
  const pushed = (await index.ws.next((f) => f.push?.kind === "workspaces")).push.data;
  expect(pushed.joined[1].summary.questions).toBe(before.questions + 1);
  expect(pushed.joined[1].summary.rev).toBe(before.rev + 1);
  // A push that changes no count changes no summary and pushes no listing.
  const rev = mock.rev;
  mock.workspace(1).push({ kind: "event", data: { seq: 0, timestamp: mock.now(), source: "x", event_type: "x", message: "", level: "info" } });
  expect(mock.rev).toBe(rev);
});

test("waitForCall sees calls made and to come, and gives up", async () => {
  const mock = await start();
  const { ws } = await signedIn(mock);
  expect((await mock.waitForCall("hello")).params.client).toBe("test");
  const mark = mock.calls.length;
  const pending = mock.waitForCall("ping", (c) => c.seq >= mark);
  ws.request("ping");
  expect((await pending).seq).toBe(mark);
  await expect(mock.waitForCall("snapshot", undefined, 100)).rejects.toThrow(/no matching snapshot call within 100 ms/);
});
