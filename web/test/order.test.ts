// The page's half of the snapshot barrier (notes/web-design.md §transport
// "A snapshot is a barrier", §tests "order.test.ts").
import { beforeEach, describe, expect, test } from "vitest";
import { Session, STORAGE_KEY } from "../src/net/session";
import { createLink } from "../src/net/state";
import { decodeSnapshot, type Agent, type Snapshot } from "../src/protocol";
import { createState } from "../src/store/store";
import { FakeSocket, MemoryStorage, fakeEnv, settle, sha256 } from "./fakes";
import { fixture } from "./fixtures";

const SECRET = "proof";
const base = (): Snapshot => decodeSnapshot(fixture("snapshot.json"));

async function live() {
  const st = new MemoryStorage();
  st.setItem(STORAGE_KEY, JSON.stringify({ token: "t", verifier: sha256(SECRET) }));
  const store = createState();
  const link = createLink(fakeEnv(), new Session(st, null), {
    snapshot: (s, now) => store.applySnapshot(s, now),
    push: (p, now) => store.applyPush(p, now),
    shutdown: () => {},
    now: () => Date.now(),
  });
  link.start();
  const s = FakeSocket.all[FakeSocket.all.length - 1];
  s.open();
  await settle();
  s.reply(s.last("hello").id, { role: "operator", protocol: 5, version: "x", pid: 1, web_proof: SECRET });
  await settle();
  return { s, store, link };
}

const agentAt = (snap: Snapshot, n: number): Agent => ({ ...structuredClone(snap.agents[0]), activity: `n=${n}` });
const activity = (store: ReturnType<typeof createState>) => store.state.agents.find((a) => a.id === "claude-1")!.activity;

beforeEach(() => {
  FakeSocket.all = [];
});

describe("while a snapshot is in flight", () => {
  test("pushes are discarded, resync among them, and no second snapshot is asked for", async () => {
    const { s, store, link } = await live();
    // Between subscribe and the first snapshot.
    s.push("agent", agentAt(base(), 1));
    s.reply(s.last("subscribe").id, { subscribed: true });
    await settle();
    expect(link.snapshotInFlight).toBe(true);
    s.push("agent", agentAt(base(), 2));
    s.push("resync");
    s.push("event", { seq: 9999, timestamp: 0, source: "x", event_type: "x", message: "", level: "info" });
    await settle();
    expect(s.methods().filter((m) => m === "snapshot").length).toBe(1);
    const snap = base();
    snap.agents[0].activity = "n=3";
    s.reply(s.last("snapshot").id, snap);
    await settle();
    expect(activity(store)).toBe("n=3");
    expect(store.events.last()!.seq).not.toBe(9999);
    // After the reply every push is applied in order.
    s.push("agent", agentAt(base(), 4));
    s.push("agent", agentAt(base(), 5));
    expect(activity(store)).toBe("n=5");
    // A resync after the reply starts the next snapshot, one at a time.
    s.push("resync");
    s.push("resync");
    await settle();
    expect(s.methods().filter((m) => m === "snapshot").length).toBe(2);
  });
});

/** A seeded generator, so a failure can be replayed. */
function rng(seed: number) {
  return () => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);
}

/**
 * One history of one agent's activity `n=1…N`, as the daemon would stream it
 * on one subscribed connection when a snapshot is asked for mid-way. With
 * `guarantee`, the server keeps the barrier: pushes before the reply are older
 * than the capture, every push sent after the marker follows the reply. Without
 * it (the daemon as it was), a push newer than the capture can come before the
 * reply, and none after it.
 */
function stream(n: number, random: () => number, guarantee: boolean): { frames: ({ push: number } | { reply: number })[]; final: number } {
  const marker = 1 + Math.floor(random() * n);
  const capture = marker + Math.floor(random() * (n - marker + 1));
  const frames: ({ push: number } | { reply: number })[] = [];
  if (guarantee) {
    // Before the reply: some of the pushes older than the marker (the old
    // forwarder was stopped); all of them are in the snapshot.
    for (let i = 1; i < marker; i++) if (random() < 0.7) frames.push({ push: i });
    frames.push({ reply: capture });
    for (let i = marker; i <= n; i++) frames.push({ push: i });
  } else {
    // The race the barrier removes: a change after the capture queued before the reply.
    for (let i = 1; i <= Math.min(n, capture + 1); i++) frames.push({ push: i });
    frames.push({ reply: capture });
  }
  return { frames, final: n };
}

/** The page's rule, as a model: discard while a snapshot is in flight, replace on the reply, apply after. */
function pageModel(frames: ({ push: number } | { reply: number })[]): number {
  let state = 0, inFlight = true;
  for (const f of frames) {
    if ("reply" in f) {
      state = f.reply;
      inFlight = false;
    } else if (!inFlight) state = f.push;
  }
  return state;
}

/** What a page that applies everything and then replaces on the reply ends with. */
function naive(frames: ({ push: number } | { reply: number })[]): number {
  let state = 0;
  for (const f of frames) state = "reply" in f ? f.reply : f.push;
  return state;
}

test("property: under the server's guarantee the page ends in the daemon's final state", () => {
  const random = rng(42);
  for (let run = 0; run < 500; run++) {
    const { frames, final } = stream(2 + Math.floor(random() * 40), random, true);
    expect(pageModel(frames), JSON.stringify(frames)).toBe(final);
  }
});

test("property: a stream that breaks the guarantee leaves the naive reducer stale (the test can fail)", () => {
  const random = rng(7);
  let stale = 0;
  for (let run = 0; run < 200; run++) {
    const { frames, final } = stream(5 + Math.floor(random() * 20), random, false);
    if (naive(frames) !== final) stale++;
  }
  expect(stale).toBeGreaterThan(0);
});

test("property: the real link and store follow the model on the fake socket", async () => {
  const random = rng(1234);
  for (let run = 0; run < 25; run++) {
    FakeSocket.all = [];
    const { s, store } = await live();
    s.reply(s.last("subscribe").id, { subscribed: true });
    await settle();
    const { frames, final } = stream(2 + Math.floor(random() * 30), random, true);
    for (const f of frames) {
      if ("reply" in f) {
        const snap = base();
        snap.agents[0].activity = `n=${f.reply}`;
        s.reply(s.last("snapshot").id, snap);
        await settle();
      } else s.push("agent", agentAt(base(), f.push));
    }
    expect(activity(store)).toBe(`n=${final}`);
  }
});

