// Feature negotiation on a workspace connection (notes/protocol-features.md
// §Common records): before `subscribe`, only the implemented features, a v6
// daemon's unknown method as legacy and nothing else, and observer pushes
// outside the snapshot barrier, for the live connection only.
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { CallError, type Conn } from "../src/net/conn";
import { WEB_FEATURES, accepts, featureSession, negotiate } from "../src/net/features";
import { FakeSocket, settle } from "./fakes";
import { fixture } from "./fixtures";
import { A, entry, indexSocket, listing, newest, page, serveIndex, serveWorkspace } from "./multi";

const usage = fixture("usage.json");
const v6 = FakeSocket.negotiation;
beforeEach(() => {
  FakeSocket.all = [];
});
afterEach(() => {
  FakeSocket.negotiation = v6;
});

const answering = (reply: (s: FakeSocket, id: number) => void) => (FakeSocket.negotiation = reply);
const connFailing = (e: unknown) => ({ call: () => Promise.reject(e) }) as unknown as Conn;

async function live(reply: (s: FakeSocket, id: number) => void) {
  answering(reply);
  const p = page(A);
  await serveIndex(indexSocket(), listing(1, [entry(A)]), settle);
  await serveWorkspace(newest(), settle);
  expect(p.link.shown()).toBe("live");
  return p;
}

describe("a session grants only what was asked", () => {
  const ok = usage.negotiated;
  test("the fixture's session", () => {
    expect(featureSession(ok)).toEqual(ok);
    expect(accepts({ state: "negotiated", session: ok }, "usage_metrics")).toBe(true);
    expect(accepts({ state: "negotiated", session: ok }, "epics")).toBe(false);
    expect(accepts({ state: "legacy" }, "usage_metrics")).toBe(false);
  });
  test.each([
    ["another version", { ...ok, accepted: { usage_metrics: 2 } }],
    ["a feature not asked for", { ...ok, accepted: { usage_metrics: 1, epics: 1 } }],
    ["accepted and unavailable at once", { ...ok, unavailable: { usage_metrics: "not_ready" } }],
    ["unavailable but not asked for", { ...ok, unavailable: { docs: "not_ready" } }],
    ["no connection epoch", { ...ok, connection_epoch: "" }],
    ["a run that is not a string", { ...ok, workspace_run_id: 7 }],
    ["not an object", null],
  ])("refused: %s", (_, v) => expect(featureSession(v)).toBeNull());
});

describe("negotiation outcomes", () => {
  test("only the unknown method of a v6 decoder is legacy", async () => {
    const unknown = new CallError("invalid", "malformed request: unknown variant `features_negotiate`, expected one of `hello`");
    expect(await negotiate(connFailing(unknown))).toEqual({ state: "legacy" });
    expect(await negotiate(connFailing(new CallError("invalid", "malformed request: invalid type")))).toEqual({ state: "unavailable", reason: "invalid" });
    expect(await negotiate(connFailing(new CallError("forbidden", "no")))).toEqual({ state: "unavailable", reason: "forbidden" });
  });
  test("a timeout or a lost connection is the connection's failure", async () => {
    await expect(negotiate(connFailing(new CallError("timeout", "x")))).rejects.toThrow();
    await expect(negotiate(connFailing(new CallError("closed", "x")))).rejects.toThrow();
  });

  test("asked before subscribe, with only the implemented features", async () => {
    const p = await live((s, id) => s.reply(id, usage.negotiated));
    const s = newest();
    expect(s.methods().slice(0, 3)).toEqual(["hello", "features_negotiate", "subscribe"]);
    expect(s.last("features_negotiate").params).toEqual({ versions: WEB_FEATURES });
    expect(p.link.features()).toEqual({ state: "negotiated", session: usage.negotiated });
  });

  test("a reply granting more than was asked is unavailable, and the page still works", async () => {
    const p = await live((s, id) => s.reply(id, { ...usage.negotiated, accepted: { usage_metrics: 1, epics: 1 } }));
    expect(p.link.features()).toEqual({ state: "unavailable", reason: "malformed" });
    expect(p.applied.snapshots.length).toBe(1);
  });
});

describe("observer pushes", () => {
  test("reach their views during a snapshot, only from the live connection, and features reset with it", async () => {
    const p = await live((s, id) => s.reply(id, usage.negotiated));
    const got: [string, unknown][] = [];
    p.link.onFeature((name, data) => got.push([name, data]));
    const s = newest();
    // A resync puts a snapshot in flight: ordinary pushes are discarded.
    s.push("resync");
    await settle();
    s.push("task", fixture("snapshot.json").tasks[0]);
    s.frame(usage.changed);
    await settle();
    expect(p.applied.pushes).not.toContain("task");
    expect(got).toEqual([["usage_changed", usage.changed.push.data]]);
    s.drop(4001);
    await settle();
    expect(p.link.features()).toEqual({ state: "pending" });
    s.frame(usage.changed);
    await settle();
    expect(got.length).toBe(1);
  });
});
