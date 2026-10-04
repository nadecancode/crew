// A page against a fake dashboard server: the index connection and the
// workspace connections behind fake sockets, every step answered by the test
// (notes/web-multi.md §multi-tests "Frontend").
import { expect } from "vitest";
import { createHub } from "../src/net/hub";
import { Session, STORAGE_KEY } from "../src/net/session";
import { createWorkspaceLink, type Sink } from "../src/net/workspace";
import { PROTOCOL, decodeSnapshot, type Push, type Snapshot, type WorkspaceEntry, type Workspaces } from "../src/protocol";
import { FakeSocket, MemoryStorage, fakeEnv, sha256 } from "./fakes";
import { fixture } from "./fixtures";
import type { Env } from "../src/net/conn";

export const SECRET = "s3cret-of-the-server";
export const TOKEN = "t0ken";
export const A = "946d4617aa702870";
export const B = "0b54c1d2e3f4a596";

export function stored(secret = SECRET) {
  const st = new MemoryStorage();
  st.setItem(STORAGE_KEY, JSON.stringify({ token: TOKEN, verifier: sha256(secret) }));
  return st;
}

export function entry(id: string, over: Partial<WorkspaceEntry> = {}): WorkspaceEntry {
  return {
    id,
    generation: 1,
    daemon: { pid: 100, started_at: 1000 },
    project: id === A ? "payments" : "search",
    workspace: id === A ? "/home/op/work/payments" : "/home/op/work/search",
    state: "running",
    detail: "",
    summary: null,
    joined_at: 0,
    ...over,
  };
}

export function listing(rev: number, joined: WorkspaceEntry[], others: Workspaces["others"] = []): Workspaces {
  return { rev, joined, others };
}

/** The index connection's socket: the newest whose hello named no
 * workspace, else the first (it has not said hello yet). */
export const indexSocket = () =>
  [...FakeSocket.all].reverse().find((s) => s.sent.some((r) => r.method === "hello") && s.workspace === undefined) ?? FakeSocket.all[0];
/** The sockets whose hello named workspace `id`. */
export const workspaceSockets = (id: string) => FakeSocket.all.filter((s) => s.workspace === id);
export const newest = () => FakeSocket.all[FakeSocket.all.length - 1];

const helloReply = (proof: string | null, project: string | null = null) => ({ role: "operator", protocol: PROTOCOL, version: "x", pid: 1, project, ...(proof === null ? {} : { web_proof: proof }) });

export type Settle = () => Promise<void>;

/** Answer the index: hello with `proof`, then `workspaces` and `web_prefs`. */
export async function serveIndex(s: FakeSocket, l: Workspaces, settle: Settle, o: { proof?: string | null; prefs?: unknown } = {}) {
  s.open();
  await settle();
  const hello = s.last("hello");
  expect(hello.params).toMatchObject({ role: "operator", token: TOKEN, client: "web", protocol: PROTOCOL });
  expect(hello.params.workspace).toBeUndefined();
  s.reply(hello.id, helloReply(o.proof === undefined ? SECRET : o.proof));
  await settle();
  if (s.readyState !== 1) return;
  s.reply(s.last("workspaces").id, l);
  // Read once per document: a reconnect does not ask again.
  const prefs = s.last("web_prefs");
  if (prefs) s.reply(prefs.id, { prefs: o.prefs ?? {} });
  await settle();
}

/** Answer a workspace connection: hello, subscribe, snapshot. */
export async function serveWorkspace(s: FakeSocket, settle: Settle, o: { proof?: string | null; snapshot?: Snapshot | unknown } = {}) {
  s.open();
  await settle();
  const hello = s.last("hello");
  expect(hello.params.workspace).toBeDefined();
  s.reply(hello.id, helloReply(o.proof === undefined ? SECRET : o.proof, "payments"));
  await settle();
  if (s.readyState !== 1 || !s.last("subscribe")) return;
  s.reply(s.last("subscribe").id, { subscribed: true });
  await settle();
  s.reply(s.last("snapshot").id, o.snapshot ?? fixture("snapshot.json"));
  await settle();
}

/** Open a workspace connection and answer its hello with an error. */
export async function refuse(s: FakeSocket, settle: Settle, code: string) {
  s.open();
  await settle();
  s.fail(s.last("hello").id, code);
  await settle();
}

export function page(id: string | null = A, o: { storage?: MemoryStorage; code?: string | null; env?: Partial<Env> } = {}) {
  const storage = o.storage ?? stored();
  const session = new Session(storage, o.code ?? null);
  const hub = createHub(fakeEnv(o.env), session);
  const applied: { snapshots: Snapshot[]; pushes: string[]; shutdowns: number } = { snapshots: [], pushes: [], shutdowns: 0 };
  const sink: Sink = {
    snapshot: (snap) => applied.snapshots.push(snap),
    push: (p: Push) => (applied.pushes.push(p.kind === "agent" ? `agent:${p.data.activity}` : p.kind), true),
    shutdown: () => applied.shutdowns++,
    now: () => Date.now(),
  };
  const link = createWorkspaceLink(hub, id, sink);
  hub.start();
  link.start();
  return { hub, link, session, storage, applied };
}

export const snapshot = () => decodeSnapshot(fixture("snapshot.json"));
