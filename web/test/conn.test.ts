// The connections (notes/web-design.md §tests "conn.test.ts", notes/web-multi.md
// §multi-tests "Frontend"): on the index, hello and the server's proof, then
// the listing; on a workspace connection, hello with its id, the proof,
// subscribe, snapshot in that order; nothing is used before the proof
// matched; every way of stopping the page removes the session and makes no
// further attempt; reconnecting backs off; only `send` is replayed.
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { CallError, Conn, backoff, timeoutFor, GIVE_UP_MS } from "../src/net/conn";
import { Session, STORAGE_KEY } from "../src/net/session";
import { createPage } from "../src/app";
import { FakeSocket, fakeEnv, flush, settle, sha256 } from "./fakes";
import { fixture } from "./fixtures";
import { A, SECRET, entry, indexSocket, listing, newest, page, serveIndex, serveWorkspace, stored } from "./multi";

const sock = newest;
const helloReply = (proof: string | null = SECRET) => ({ role: "operator", protocol: 6, version: "x", pid: 1, ...(proof === null ? {} : { web_proof: proof }) });

beforeEach(() => {
  FakeSocket.all = [];
});
afterEach(() => {
  vi.useRealTimers();
});

describe("handshake", () => {
  test("index: hello, proof, then the listing and the operator's preferences; workspace: hello with its id, proof, feature negotiation, subscribe, snapshot", async () => {
    const p = page(A);
    const index = indexSocket();
    index.open();
    await settle();
    expect(index.methods()).toEqual(["hello"]);
    index.reply(index.last("hello").id, helloReply());
    await settle();
    expect(index.methods()).toEqual(["hello", "workspaces", "web_prefs"]);
    index.reply(index.last("workspaces").id, listing(1, [entry(A)]));
    index.reply(index.last("web_prefs").id, { prefs: {} });
    await settle();
    expect(p.hub.conn()).toBe("live");
    const s = sock();
    s.open();
    await settle();
    expect(s.last("hello").params).toEqual({ role: "operator", token: "t0ken", client: "web", protocol: 6, workspace: A });
    s.reply(s.last("hello").id, helloReply());
    await settle();
    expect(s.methods()).toEqual(["hello", "features_negotiate", "subscribe"]);
    expect(s.last("features_negotiate").params).toEqual({ versions: { usage_metrics: 1 } });
    s.reply(s.last("subscribe").id, { subscribed: true });
    await settle();
    expect(s.methods()).toEqual(["hello", "features_negotiate", "subscribe", "snapshot"]);
    expect(s.last("snapshot").params).toEqual({ messages: 300, events: 1000 });
    s.reply(s.last("snapshot").id, fixture("snapshot.json"));
    await settle();
    expect(p.applied.snapshots.length).toBe(1);
    expect(p.link.shown()).toBe("live");
    expect(p.link.live()).toBe(true);
    expect(p.link.features()).toEqual({ state: "legacy" });
  });

  for (const [name, proof] of [
    ["a missing proof", null],
    ["a wrong proof", "not-the-secret"],
    ["the verifier itself sent back as the proof", sha256(SECRET)],
  ] as const) {
    test(`${name} on the index stops the page for good: nothing used, session removed, no other attempt`, async () => {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
      const p = page(A);
      const s = sock();
      s.open();
      (await flush(), await vi.advanceTimersByTimeAsync(1));
      s.reply(s.last("hello").id, helloReply(proof));
      (await flush(), await vi.advanceTimersByTimeAsync(1));
      // Whatever it sends next is not used.
      s.push("workspaces", listing(1, [entry(A)]));
      await vi.advanceTimersByTimeAsync(60_000);
      expect(p.hub.conn()).toBe("stopped");
      expect(p.hub.stopReason()).toBe("proof");
      expect(s.methods()).toEqual(["hello"]);
      expect(p.hub.listing()).toBeNull();
      expect(p.storage.map.has(STORAGE_KEY)).toBe(false);
      expect(FakeSocket.all.length).toBe(1);
    });
  }

  test("unauthorized signs out and removes the session; no retry", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const p = page(A);
    const s = sock();
    s.open();
    (await flush(), await vi.advanceTimersByTimeAsync(1));
    s.fail(s.last("hello").id, "unauthorized", "no such session");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(p.hub.conn()).toBe("signed-out");
    expect(p.storage.map.has(STORAGE_KEY)).toBe(false);
    expect(FakeSocket.all.length).toBe(1);
  });

  test("protocol on the index is an error without retry and without reload", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const p = page(A);
    const s = sock();
    s.open();
    (await flush(), await vi.advanceTimersByTimeAsync(1));
    s.fail(s.last("hello").id, "protocol", "client speaks protocol 6, server speaks 7");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(p.hub.conn()).toBe("stopped");
    expect(p.hub.stopReason()).toBe("protocol");
    expect(FakeSocket.all.length).toBe(1);
  });

  test("the server's shutdown stops the page and empties the storage", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const tick = async () => (await flush(), await vi.advanceTimersByTimeAsync(1));
    const p = page(A);
    await serveIndex(indexSocket(), listing(1, [entry(A)]), tick);
    await serveWorkspace(sock(), tick);
    indexSocket().push("shutdown");
    indexSocket().drop();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(p.hub.conn()).toBe("stopped");
    expect(p.hub.stopReason()).toBe("shutdown");
    expect(p.storage.map.has(STORAGE_KEY)).toBe(false);
    expect(FakeSocket.all.length).toBe(2);
  });

  test("30 s of failed reconnects of the index stop the page", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const p = page(A);
    sock().drop();
    for (let t = 0; t < 40_000; t += 100) {
      await vi.advanceTimersByTimeAsync(100);
      const s = sock();
      if (s.readyState === 0) s.drop();
    }
    expect(p.hub.conn()).toBe("stopped");
    expect(p.hub.stopReason()).toBe("gone");
    expect(p.storage.map.has(STORAGE_KEY)).toBe(false);
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
  s.reply(s.last("hello").id, helloReply());
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

test("a workspace connection's refusal is its ending, not the page's stop; the close code is kept", async () => {
  const states: string[] = [];
  const conn = new Conn({ env: fakeEnv(), session: new Session(stored(), null), reconnect: false, workspace: A, ready: () => {}, push: () => {}, state: (s, r) => states.push(`${s}:${r ?? ""}`) });
  conn.start();
  const s = sock();
  s.open();
  await settle();
  s.fail(s.last("hello").id, "protocol", "infra was started by another version of crew");
  await settle();
  expect(conn.ending).toEqual({ kind: "refused", code: "protocol", message: "infra was started by another version of crew" });
  expect(states).toEqual(["connecting:", "stopped:gone"]);

  const closed = new Conn({ env: fakeEnv(), session: new Session(stored(), null), reconnect: false, workspace: A, ready: () => {}, push: () => {}, state: () => {} });
  closed.start();
  sock().open();
  await settle();
  sock().reply(sock().last("hello").id, helloReply());
  await settle();
  sock().drop(4002);
  expect(closed.ending).toEqual({ kind: "closed", code: 4002 });
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
  s.reply(s.last("hello").id, helloReply());
  await settle();
  s.push("resync");
  expect(pushes).toEqual(["resync"]);
  expect(GIVE_UP_MS).toBe(30_000);
});

/** A page as main.tsx makes it, on workspace A. */
function realPage() {
  const loc = { hash: `#/w/${A}/` };
  const hist = { pushState: (_: unknown, __: string, url?: string) => (loc.hash = url!), replaceState: (_: unknown, __: string, url?: string) => (loc.hash = url!) };
  const p = createPage(fakeEnv(), new Session(stored(), null), new Promise(() => {}), { loc, hist });
  p.start();
  return p;
}

test("nothing the page asks goes out before the proof matched: not on the first connection, not after a reconnect", async () => {
  const p = realPage();
  await serveIndex(indexSocket(), listing(1, [entry(A)]), settle);
  const app = p.app();
  // Before the first proof: the socket is open, hello has no answer yet.
  const first = sock();
  first.open();
  await settle();
  await expect(app.link.call("inbox", { agent: "claude-1" })).rejects.toMatchObject({ code: "closed" });
  expect(await app.actions.answer("m-9001", "Convert in the adapter")).toBe(false);
  expect(first.methods()).toEqual(["hello"]);
  first.drop();
  await new Promise((r) => setTimeout(r, 400));
  await serveWorkspace(sock(), settle);
  expect(app.link.live()).toBe(true);
  // Lost again; the next server has not proved itself.
  sock().drop(4001);
  await new Promise((r) => setTimeout(r, 400));
  const third = sock();
  third.open();
  await settle();
  expect(await app.actions.answer("m-9001", "Convert in the adapter")).toBe(false);
  await expect(app.link.call("answer", { id: "m-9001", body: "x" })).rejects.toMatchObject({ code: "closed" });
  expect(third.methods()).toEqual(["hello"]);
});

test("a send whose reply was lost goes once more with the same id after the reconnect; nothing else is replayed", async () => {
  const p = realPage();
  await serveIndex(indexSocket(), listing(1, [entry(A)]), settle);
  const app = p.app();
  await serveWorkspace(sock(), settle);
  expect(app.link.live()).toBe(true);
  const sent = app.actions.send("claude-1", "hello there");
  const paused = app.actions.pause("claude-2");
  await settle();
  const first = sock();
  const id = first.last("send").params.id;
  expect(id).toMatch(/^w-[0-9a-f]{16}$/);
  first.drop(4001);
  await new Promise((r) => setTimeout(r, 400));
  const second = sock();
  expect(second).not.toBe(first);
  await serveWorkspace(second, settle);
  await settle();
  expect(second.last("send").params).toEqual({ to: "claude-1", type: "message", body: "hello there", id });
  expect(second.methods().filter((m) => m === "agent_pause")).toEqual([]);
  second.reply(second.last("send").id, { message: { id, from: "operator", to: "claude-1", type: "message", body: "hello there", timestamp: 0 }, duplicate: true, recipients: ["claude-1"] });
  expect(await sent).toBe(true);
  await paused;
  expect(app.ui.status()?.text).toBe("✓ message sent to claude-1");
});
