// The connection (notes/web-design.md §tests "conn.test.ts"): hello, the
// server's proof, subscribe, snapshot in that order; nothing is used before
// the proof matched; every way of stopping removes the session and makes no
// further attempt; reconnecting backs off; only `send` is replayed.
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { CallError, Conn, backoff, timeoutFor, GIVE_UP_MS } from "../src/net/conn";
import { Session, STORAGE_KEY } from "../src/net/session";
import { createLink } from "../src/net/state";
import { decodeSnapshot, type Snapshot } from "../src/protocol";
import { FakeSocket, MemoryStorage, fakeEnv, flush, settle, sha256 } from "./fakes";
import { fixture } from "./fixtures";

const SECRET = "s3cret-of-the-server";
const TOKEN = "t0ken";

function stored(secret = SECRET) {
  const st = new MemoryStorage();
  st.setItem(STORAGE_KEY, JSON.stringify({ token: TOKEN, verifier: sha256(secret) }));
  return st;
}

function harness(opts: { storage?: MemoryStorage; code?: string | null } = {}) {
  const storage = opts.storage ?? stored();
  const session = new Session(storage, opts.code ?? null);
  const applied: { snapshots: Snapshot[]; pushes: string[] } = { snapshots: [], pushes: [] };
  let shutdown = 0;
  const link = createLink(fakeEnv(), session, {
    snapshot: (s) => applied.snapshots.push(s),
    push: (p) => (applied.pushes.push(p.kind === "agent" ? `agent:${p.data.activity}` : p.kind), true),
    shutdown: () => shutdown++,
    now: () => Date.now(),
  });
  return { link, session, storage, applied, shutdowns: () => shutdown };
}

const sock = () => FakeSocket.all[FakeSocket.all.length - 1];
const snapshot = () => decodeSnapshot(fixture("snapshot.json"));

/** Serve the handshake: hello with `proof`, subscribe, snapshot. */
async function serve(s: FakeSocket, proof: string | null = SECRET) {
  s.open();
  await settle();
  const hello = s.last("hello");
  expect(hello.params).toMatchObject({ role: "operator", token: TOKEN, client: "web", protocol: 5 });
  s.reply(hello.id, { role: "operator", protocol: 5, version: "x", pid: 1, ...(proof === null ? {} : { web_proof: proof }) });
  await settle();
}

beforeEach(() => {
  FakeSocket.all = [];
});
afterEach(() => {
  vi.useRealTimers();
});

describe("handshake", () => {
  test("hello, proof, subscribe, snapshot, in that order", async () => {
    const h = harness();
    h.link.start();
    const s = sock();
    await serve(s);
    expect(s.methods()).toEqual(["hello", "subscribe"]);
    s.reply(s.last("subscribe").id, { subscribed: true });
    await settle();
    expect(s.methods()).toEqual(["hello", "subscribe", "snapshot"]);
    expect(s.last("snapshot").params).toEqual({ messages: 300, events: 1000 });
    s.reply(s.last("snapshot").id, fixture("snapshot.json"));
    await settle();
    expect(h.applied.snapshots.length).toBe(1);
    expect(h.link.conn()).toBe("live");
  });

  for (const [name, proof] of [
    ["a missing proof", null],
    ["a wrong proof", "not-the-secret"],
    ["the verifier itself sent back as the proof", sha256(SECRET)],
  ] as const) {
    test(`${name} stops the page for good: nothing used, session removed, no other attempt`, async () => {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
      const h = harness();
      h.link.start();
      const s = sock();
      s.open();
      (await flush(), await vi.advanceTimersByTimeAsync(1));
      const hello = s.last("hello");
      s.reply(hello.id, { role: "operator", protocol: 5, version: "x", pid: 1, ...(proof === null ? {} : { web_proof: proof }) });
      (await flush(), await vi.advanceTimersByTimeAsync(1));
      // Whatever it sends next is not used.
      s.push("agent", snapshot().agents[0]);
      await vi.advanceTimersByTimeAsync(60_000);
      expect(h.link.conn()).toBe("stopped");
      expect(h.link.stopReason()).toBe("proof");
      expect(s.methods()).toEqual(["hello"]);
      expect(h.applied.pushes).toEqual([]);
      expect(h.applied.snapshots).toEqual([]);
      expect(h.storage.map.has(STORAGE_KEY)).toBe(false);
      expect(FakeSocket.all.length).toBe(1);
    });
  }

  test("unauthorized signs out and removes the session; no retry", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const h = harness();
    h.link.start();
    const s = sock();
    s.open();
    (await flush(), await vi.advanceTimersByTimeAsync(1));
    s.fail(s.last("hello").id, "unauthorized", "no such session");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(h.link.conn()).toBe("signed-out");
    expect(h.storage.map.has(STORAGE_KEY)).toBe(false);
    expect(FakeSocket.all.length).toBe(1);
  });

  test("protocol is an error without retry and without reload", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const h = harness();
    h.link.start();
    const s = sock();
    s.open();
    (await flush(), await vi.advanceTimersByTimeAsync(1));
    s.fail(s.last("hello").id, "protocol", "client speaks protocol 5, daemon speaks 6");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(h.link.conn()).toBe("stopped");
    expect(h.link.stopReason()).toBe("protocol");
    expect(FakeSocket.all.length).toBe(1);
  });

  test("shutdown stops the page and empties the storage", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const h = harness();
    h.link.start();
    const s = sock();
    s.open();
    (await flush(), await vi.advanceTimersByTimeAsync(1));
    s.reply(s.last("hello").id, { role: "operator", protocol: 5, version: "x", pid: 1, web_proof: SECRET });
    (await flush(), await vi.advanceTimersByTimeAsync(1));
    s.reply(s.last("subscribe").id, { subscribed: true });
    (await flush(), await vi.advanceTimersByTimeAsync(1));
    s.reply(s.last("snapshot").id, fixture("snapshot.json"));
    (await flush(), await vi.advanceTimersByTimeAsync(1));
    s.push("shutdown");
    s.drop();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(h.shutdowns()).toBe(1);
    expect(h.link.conn()).toBe("stopped");
    expect(h.link.stopReason()).toBe("shutdown");
    expect(h.storage.map.has(STORAGE_KEY)).toBe(false);
    expect(FakeSocket.all.length).toBe(1);
  });

  test("30 s of failed reconnects stop the page", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const h = harness();
    h.link.start();
    sock().drop();
    for (let t = 0; t < 40_000; t += 100) {
      await vi.advanceTimersByTimeAsync(100);
      const s = sock();
      if (s.readyState === 0) s.drop();
    }
    expect(h.link.conn()).toBe("stopped");
    expect(h.link.stopReason()).toBe("gone");
    expect(h.storage.map.has(STORAGE_KEY)).toBe(false);
    const n = FakeSocket.all.length;
    await vi.advanceTimersByTimeAsync(60_000);
    expect(FakeSocket.all.length).toBe(n);
  });
});

test("the backoff: 250 ms doubling to 5 s, ±20 %", () => {
  const at = (n: number, r: number) => backoff(n, () => r);
  expect([0, 1, 2, 3, 4, 5, 6].map((n) => at(n, 0.5))).toEqual([250, 500, 1000, 2000, 4000, 5000, 5000]);
  expect(at(0, 0)).toBe(200);
  expect(at(0, 1)).toBe(300);
  expect(at(9, 1)).toBe(6000);
});

test("timeouts per call, as the TUI's", () => {
  expect(timeoutFor("snapshot")).toBe(3000);
  for (const m of ["agent_kill", "agent_restart", "agent_restart_fresh", "agent_resume", "agent_set_model"]) expect(timeoutFor(m)).toBe(30_000);
  expect(timeoutFor("resume")).toBe(90_000);
  expect(timeoutFor("shutdown")).toBe(90_000);
  expect(timeoutFor("models_list")).toBe(20_000);
});

test("a call that times out fails; its late answer is ignored; the connection stays", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  const session = new Session(stored(), null);
  const conn = new Conn({ env: fakeEnv(), session, reconnect: true, ready: () => {}, push: () => {}, state: () => {} });
  conn.start();
  const s = sock();
  s.open();
  (await flush(), await vi.advanceTimersByTimeAsync(1));
  s.reply(s.last("hello").id, { role: "operator", protocol: 5, version: "x", pid: 1, web_proof: SECRET });
  (await flush(), await vi.advanceTimersByTimeAsync(1));
  const p = conn.call("task_get", { id: "x" });
  const caught = p.catch((e) => e);
  await vi.advanceTimersByTimeAsync(3001);
  const err = await caught;
  expect(err).toBeInstanceOf(CallError);
  expect(err.code).toBe("timeout");
  s.reply(s.last("task_get").id, { task: {} });
  expect(conn.live).toBe(true);
});

test("nothing pushed before the proof matched reaches the page", async () => {
  const pushes: string[] = [];
  const session = new Session(stored(), null);
  const conn = new Conn({ env: fakeEnv(), session, reconnect: true, ready: () => {}, push: (p) => pushes.push(p.kind), state: () => {} });
  conn.start();
  const s = sock();
  s.open();
  await settle();
  s.push("resync");
  s.reply(s.last("hello").id, { role: "operator", protocol: 5, version: "x", pid: 1, web_proof: SECRET });
  await settle();
  s.push("resync");
  expect(pushes).toEqual(["resync"]);
  expect(GIVE_UP_MS).toBe(30_000);
});
