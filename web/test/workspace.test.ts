// The page's two levels (notes/web-multi.md §multi-ui "Two levels",
// §multi-tests "Frontend"): the index connection holds the page's state and
// the listing; the workspace on screen is computed from the newest listing
// and from whether its connection is live. A connection says only what
// happened to that connection.
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { STORAGE_KEY } from "../src/net/session";
import { FakeSocket, flush } from "./fakes";
import { A, B, entry, indexSocket, listing, newest, page, refuse, serveIndex, serveWorkspace, workspaceSockets } from "./multi";

beforeEach(() => {
  FakeSocket.all = [];
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
});
afterEach(() => vi.useRealTimers());

const settle = () => flush(6);
const after = async (ms: number) => {
  await vi.advanceTimersByTimeAsync(ms);
  await settle();
};
/** The timers armed now, as vitest's fake clock holds them. */
const timers = () => vi.getTimerCount();

/** A page whose index is live under `l`, and whose workspace connection to A is live. */
async function livePage(l = listing(1, [entry(A)])) {
  const p = page(A);
  await serveIndex(indexSocket(), l, settle);
  await serveWorkspace(newest(), settle);
  expect(p.link.shown()).toBe("live");
  return p;
}

describe("the page and the workspace are two levels", () => {
  test("no workspace connection before the index is live", async () => {
    const p = page(A);
    expect(FakeSocket.all.length).toBe(1);
    const index = indexSocket();
    index.open();
    await settle();
    expect(FakeSocket.all.length).toBe(1);
    index.reply(index.last("hello").id, { role: "operator", protocol: 6, version: "x", pid: 1, web_proof: "s3cret-of-the-server" });
    await settle();
    // The listing is there, the preferences are not: the page is not live yet.
    index.reply(index.last("workspaces").id, listing(1, [entry(A)]));
    await settle();
    expect(FakeSocket.all.length).toBe(1);
    index.reply(index.last("web_prefs").id, { prefs: {} });
    await settle();
    expect(p.hub.conn()).toBe("live");
    expect(FakeSocket.all.length).toBe(2);
    expect(newest().workspace).toBeUndefined();
    newest().open();
    await settle();
    expect(newest().last("hello").params).toMatchObject({ workspace: A, protocol: 6 });
  });

  test("no workspace connection, and no timer, for a workspace that is not running", async () => {
    for (const state of ["stopped", "unreachable", "incompatible"] as const) {
      FakeSocket.all = [];
      const p = page(A);
      await serveIndex(indexSocket(), listing(1, [entry(A, { state, daemon: null })]), settle);
      expect(FakeSocket.all.length).toBe(1);
      expect(p.link.shown()).toBe(state === "incompatible" ? "incompatible" : "not-running");
      expect(timers()).toBe(0);
      await after(60_000);
      expect(FakeSocket.all.length).toBe(1);
      p.link.dispose();
      p.hub.stop("gone");
    }
  });

  test("the daemon's shutdown on a workspace connection stops neither the page nor its session", async () => {
    const p = await livePage();
    newest().push("shutdown");
    await settle();
    expect(p.applied.shutdowns).toBe(1);
    expect(p.hub.conn()).toBe("live");
    expect(p.storage.map.has(STORAGE_KEY)).toBe(true);
    expect(p.link.shown()).toBe("reconnecting");
  });

  test("the server's shutdown on the index stops the page, closes every connection and removes the session", async () => {
    const p = await livePage();
    const ws = newest();
    indexSocket().push("shutdown");
    await settle();
    expect(p.hub.conn()).toBe("stopped");
    expect(p.hub.stopReason()).toBe("shutdown");
    expect(ws.readyState).toBe(3);
    expect(p.storage.map.has(STORAGE_KEY)).toBe(false);
    const n = FakeSocket.all.length;
    await after(60_000);
    expect(FakeSocket.all.length).toBe(n);
  });

  test("a failed proof on a workspace connection stops the page", async () => {
    const p = page(A);
    await serveIndex(indexSocket(), listing(1, [entry(A)]), settle);
    await serveWorkspace(newest(), settle, { proof: "forged" });
    expect(p.hub.conn()).toBe("stopped");
    expect(p.hub.stopReason()).toBe("proof");
    expect(p.storage.map.has(STORAGE_KEY)).toBe(false);
    expect(indexSocket().readyState).toBe(3);
    expect(p.applied.snapshots.length).toBe(0);
  });

  test("unauthorized on a workspace connection signs the page out", async () => {
    const p = page(A);
    await serveIndex(indexSocket(), listing(1, [entry(A)]), settle);
    await refuse(newest(), settle, "unauthorized");
    expect(p.hub.conn()).toBe("signed-out");
    expect(p.storage.map.has(STORAGE_KEY)).toBe(false);
  });

  test("while the index is lost no attempt is started; the page is reconnecting", async () => {
    const p = await livePage();
    const ws = newest();
    ws.drop(4001);
    indexSocket().drop();
    await settle();
    expect(p.hub.conn()).toBe("reconnecting");
    expect(p.link.live()).toBe(false);
    const n = FakeSocket.all.length;
    // The index's retry is due at 250 ms; the workspace's would be too.
    await after(250);
    expect(FakeSocket.all.length).toBe(n + 1);
    expect(newest().sent.length).toBe(0);
    await serveIndex(newest(), listing(1, [entry(A)]), settle);
    expect(p.hub.conn()).toBe("live");
    // Live again: the workspace is tried at once.
    expect(FakeSocket.all.length).toBe(n + 2);
    await serveWorkspace(newest(), settle);
    expect(p.link.shown()).toBe("live");
  });
});

describe("the listing's order", () => {
  test("a reply older than a push that arrived before it is dropped", async () => {
    const p = page(A);
    const index = indexSocket();
    index.open();
    await settle();
    index.reply(index.last("hello").id, { role: "operator", protocol: 6, version: "x", pid: 1, web_proof: "s3cret-of-the-server" });
    await settle();
    index.push("workspaces", listing(5, [entry(A, { project: "newer" })]));
    index.reply(index.last("workspaces").id, listing(4, [entry(A, { project: "older" })]));
    index.reply(index.last("web_prefs").id, { prefs: {} });
    await settle();
    expect(p.hub.listing()!.rev).toBe(5);
    expect(p.hub.entry(A)!.project).toBe("newer");
  });

  test("an older push after a newer reply is dropped", async () => {
    const p = await livePage(listing(7, [entry(A)]));
    indexSocket().push("workspaces", listing(6, [entry(A, { state: "stopped", daemon: null })]));
    await settle();
    expect(p.hub.listing()!.rev).toBe(7);
    expect(p.link.shown()).toBe("live");
  });

  test("pushes before the proof are dropped; the reply that follows covers them", async () => {
    const p = page(A);
    const index = indexSocket();
    index.open();
    await settle();
    index.push("workspaces", listing(9, [entry(A, { project: "early" })]));
    await settle();
    expect(p.hub.listing()).toBeNull();
    index.reply(index.last("hello").id, { role: "operator", protocol: 6, version: "x", pid: 1, web_proof: "s3cret-of-the-server" });
    await settle();
    index.reply(index.last("workspaces").id, listing(9, [entry(A, { project: "reply" })]));
    index.reply(index.last("web_prefs").id, { prefs: {} });
    await settle();
    expect(p.hub.entry(A)!.project).toBe("reply");
  });

  test("equal revisions are not applied twice", async () => {
    const p = await livePage(listing(3, [entry(A)]));
    let applied = 0;
    p.hub.onListing(() => applied++);
    const first = p.hub.listing();
    indexSocket().push("workspaces", listing(3, [entry(A, { project: "same rev" })]));
    await settle();
    expect(applied).toBe(0);
    expect(p.hub.listing()).toBe(first);
  });
});

describe("a connection is not the daemon", () => {
  const outcomes: [string, (s: FakeSocket) => Promise<void>][] = [
    ["a close with 4001", async (s) => s.drop(4001)],
    ["the daemon's shutdown push", async (s) => s.push("shutdown")],
    ["a close without a code", async (s) => s.drop()],
  ];
  for (const [name, end] of outcomes) {
    test(`${name}, under a listing that says running and never changes: tried at 250 ms, 500 ms … 5 s without limit, live when a try succeeds`, async () => {
      const p = await livePage();
      await end(newest());
      await settle();
      expect(p.link.shown()).toBe("reconnecting");
      // Every try fails with `unavailable`, the listing stays as it is.
      const delays = [250, 500, 1000, 2000, 4000, 5000, 5000, 5000, 5000, 5000, 5000, 5000];
      for (const d of delays) {
        const n = FakeSocket.all.length;
        await after(d - 1);
        expect(FakeSocket.all.length, `before ${d} ms`).toBe(n);
        await after(1);
        expect(FakeSocket.all.length, `at ${d} ms`).toBe(n + 1);
        await refuse(newest(), settle, "unavailable");
        expect(p.link.shown()).toBe("reconnecting");
      }
      // No call for the listing was made for any of these outcomes.
      expect(indexSocket().methods().filter((m) => m === "workspaces").length).toBe(1);
      await after(5000);
      await serveWorkspace(newest(), settle);
      expect(p.link.shown()).toBe("live");
      expect(p.applied.snapshots.length).toBe(2);
    });
  }

  test("a hello that times out ends the attempt and is tried again", async () => {
    const p = page(A);
    await serveIndex(indexSocket(), listing(1, [entry(A)]), settle);
    newest().open();
    await after(3000);
    expect(newest().readyState).toBe(3);
    expect(p.link.shown()).toBe("connecting");
    await after(250);
    await serveWorkspace(newest(), settle);
    expect(p.link.shown()).toBe("live");
  });

  test("the tries stop when a listing says stopped; a listing that says running connects at once", async () => {
    const p = await livePage(listing(1, [entry(A)]));
    newest().drop(4001);
    await settle();
    indexSocket().push("workspaces", listing(2, [entry(A, { state: "stopped", daemon: null })]));
    await settle();
    expect(p.link.shown()).toBe("not-running");
    const n = FakeSocket.all.length;
    expect(timers()).toBe(0);
    await after(60_000);
    expect(FakeSocket.all.length).toBe(n);
    indexSocket().push("workspaces", listing(3, [entry(A, { daemon: { pid: 101, started_at: 2000 } })]));
    await settle();
    expect(FakeSocket.all.length).toBe(n + 1);
    await serveWorkspace(newest(), settle);
    expect(p.link.shown()).toBe("live");
  });
});

describe("an outcome is about its attempt", () => {
  test("the delayed old close: 4002 after a listing with the next generation; asks, connects at once, never removed", async () => {
    const p = await livePage(listing(1, [entry(A, { generation: 1 })]));
    const seen: string[] = [];
    const first = newest();
    // The removal and the rejoin, coalesced into one listing.
    indexSocket().push("workspaces", listing(2, [entry(A, { generation: 2 })]));
    await settle();
    seen.push(p.link.shown());
    first.drop(4002);
    await settle();
    seen.push(p.link.shown());
    const asked = indexSocket().last("workspaces");
    expect(asked).toBeDefined();
    expect(indexSocket().methods().filter((m) => m === "workspaces").length).toBe(2);
    const n = FakeSocket.all.length;
    indexSocket().reply(asked.id, listing(2, [entry(A, { generation: 2 })]));
    await settle();
    expect(FakeSocket.all.length).toBe(n + 1);
    await serveWorkspace(newest(), settle);
    seen.push(p.link.shown());
    expect(seen).not.toContain("removed");
    expect(p.link.shown()).toBe("live");
  });

  test("4002, then a newer listing without the workspace: removed, no attempt", async () => {
    const p = await livePage(listing(1, [entry(A)]));
    newest().drop(4002);
    await settle();
    const n = FakeSocket.all.length;
    indexSocket().reply(indexSocket().last("workspaces").id, listing(2, []));
    await settle();
    expect(p.link.shown()).toBe("removed");
    expect(p.link.seen()).toBe(true);
    expect(timers()).toBe(0);
    await after(60_000);
    expect(FakeSocket.all.length).toBe(n);
  });

  test("4002, then a newer listing that says running: live", async () => {
    const p = await livePage(listing(1, [entry(A)]));
    newest().drop(4002);
    await settle();
    indexSocket().reply(indexSocket().last("workspaces").id, listing(2, [entry(A, { generation: 2 })]));
    await settle();
    await serveWorkspace(newest(), settle);
    expect(p.link.shown()).toBe("live");
  });

  for (const code of ["protocol", "not_found"]) {
    test(`a late \`${code}\` under a listing that says running: asks; the same rev settles it; the next attempt is live`, async () => {
      const p = page(A);
      await serveIndex(indexSocket(), listing(4, [entry(A)]), settle);
      const shown: string[] = [];
      await refuse(newest(), settle, code);
      shown.push(p.link.shown());
      const asked = indexSocket().last("workspaces");
      expect(indexSocket().methods().filter((m) => m === "workspaces").length).toBe(2);
      let applied = 0;
      p.hub.onListing(() => applied++);
      indexSocket().reply(asked.id, listing(4, [entry(A)]));
      await settle();
      // Not applied a second time; still settles: the next try is on the backoff.
      expect(applied).toBe(0);
      await after(250);
      await serveWorkspace(newest(), settle);
      shown.push(p.link.shown());
      expect(shown).not.toContain(code === "protocol" ? "incompatible" : "removed");
      expect(p.link.shown()).toBe("live");
    });

    test(`\`${code}\`, then a listing that says ${code === "protocol" ? "incompatible" : "it is not joined"}: that, and no attempt`, async () => {
      const p = page(A);
      await serveIndex(indexSocket(), listing(4, [entry(A)]), settle);
      await refuse(newest(), settle, code);
      const n = FakeSocket.all.length;
      indexSocket().reply(indexSocket().last("workspaces").id, listing(5, code === "protocol" ? [entry(A, { state: "incompatible", daemon: null, detail: "protocol 5" })] : []));
      await settle();
      expect(p.link.shown()).toBe(code === "protocol" ? "incompatible" : "removed");
      await after(60_000);
      expect(FakeSocket.all.length).toBe(n);
    });
  }

  test("a 4002, a protocol and a close from an attempt already replaced are dropped: no call, no change", async () => {
    const p = await livePage(listing(1, [entry(A)]));
    const old = newest();
    // The old attempt ends; the next one is made and goes live.
    old.drop(4001);
    await after(250);
    await serveWorkspace(newest(), settle);
    expect(p.link.shown()).toBe("live");
    const calls = indexSocket().sent.length;
    const n = FakeSocket.all.length;
    // Whatever the old socket still delivers.
    old.onclose?.({ code: 4002 });
    old.fail(1, "protocol");
    old.drop(4002);
    await settle();
    expect(indexSocket().sent.length).toBe(calls);
    expect(FakeSocket.all.length).toBe(n);
    expect(p.link.shown()).toBe("live");
  });

  test("the backoff starts over when generation or daemon differ from the last attempt's, and not otherwise", async () => {
    const p = await livePage(listing(1, [entry(A)]));
    newest().drop(4001);
    await settle();
    // Fail a few tries: the delay grows.
    for (const d of [250, 500, 1000]) {
      await after(d);
      await refuse(newest(), settle, "unavailable");
    }
    // A listing that changes something else: the delay keeps growing (2 s).
    indexSocket().push("workspaces", listing(2, [entry(A, { summary: { rev: 1, agents: 1, working: 0, attention: 0, questions: 0, tasks: 0, tasks_done: 0 } })]));
    await settle();
    let n = FakeSocket.all.length;
    await after(1999);
    expect(FakeSocket.all.length).toBe(n);
    await after(1);
    expect(FakeSocket.all.length).toBe(n + 1);
    await refuse(newest(), settle, "unavailable");
    // Another daemon: at once, and the delay is the first one again.
    n = FakeSocket.all.length;
    indexSocket().push("workspaces", listing(3, [entry(A, { daemon: { pid: 200, started_at: 5000 } })]));
    await settle();
    expect(FakeSocket.all.length).toBe(n + 1);
    await refuse(newest(), settle, "unavailable");
    n = FakeSocket.all.length;
    await after(250);
    expect(FakeSocket.all.length).toBe(n + 1);
    await refuse(newest(), settle, "unavailable");
    // Another generation: at once as well.
    n = FakeSocket.all.length;
    indexSocket().push("workspaces", listing(4, [entry(A, { generation: 2, daemon: { pid: 200, started_at: 5000 } })]));
    await settle();
    expect(FakeSocket.all.length).toBe(n + 1);
    await serveWorkspace(newest(), settle);
    expect(p.link.shown()).toBe("live");
  });

  test("a snapshot after resync that fails ends the attempt; the next one takes its own snapshot", async () => {
    const p = await livePage();
    const s = newest();
    s.push("resync");
    await settle();
    s.fail(s.last("snapshot").id, "invalid");
    await settle();
    expect(s.readyState).toBe(3);
    expect(p.link.shown()).toBe("reconnecting");
    await after(250);
    await serveWorkspace(newest(), settle);
    expect(p.applied.snapshots.length).toBe(2);
  });
});

describe("which workspace", () => {
  test("a page without a workspace is `none` and connects nothing", async () => {
    const p = page(null);
    await serveIndex(indexSocket(), listing(1, [entry(A), entry(B)]), settle);
    expect(p.link.shown()).toBe("none");
    expect(FakeSocket.all.length).toBe(1);
  });

  test("an id that is not joined: removed, never seen, nothing sent", async () => {
    const p = page(B);
    await serveIndex(indexSocket(), listing(1, [entry(A)]), settle);
    expect(p.link.shown()).toBe("removed");
    expect(p.link.seen()).toBe(false);
    expect(workspaceSockets(B).length).toBe(0);
  });

  test("dispose closes the connection and arms nothing", async () => {
    const p = await livePage();
    const s = newest();
    p.link.dispose();
    expect(s.readyState).toBe(3);
    indexSocket().push("workspaces", listing(2, [entry(A, { generation: 2 })]));
    await settle();
    await after(10_000);
    expect(workspaceSockets(A).length).toBe(1);
  });
});
