// The page's credential (notes/web-design.md §security, §tests "session.test.ts").
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { beforeEach, expect, test } from "vitest";
import { Session, STORAGE_KEY, proves, takePairCode } from "../src/net/session";
import { createLink } from "../src/net/state";
import { FakeSocket, MemoryStorage, fakeEnv, settle, sha256 } from "./fakes";

beforeEach(() => {
  FakeSocket.all = [];
});

function sources(dir = join(process.cwd(), "src")): [string, string][] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    return statSync(p).isDirectory() ? sources(p) : [[p.slice(process.cwd().length + 1), readFileSync(p, "utf8")] as [string, string]];
  });
}

test("the #pair= fragment is removed from the address before anything else", () => {
  const calls: string[] = [];
  const hist = { replaceState: (_d: unknown, _u: string, url?: string) => calls.push(url!) };
  // As the real `location`: the hash is gone once the state was replaced.
  const place = { hash: "#pair=abc123", pathname: "/", search: "" };
  const live = { replaceState: (_d: unknown, _u: string, url?: string) => ((place.hash = ""), calls.push(url!)) };
  expect(takePairCode(place, live)).toBe("abc123");
  expect(place.hash).toBe("");
  calls.length = 0;
  expect(takePairCode({ hash: "#pair=3f9a0e1", pathname: "/", search: "" }, hist)).toBe("3f9a0e1");
  expect(calls).toEqual(["/"]);
  expect(takePairCode({ hash: "#/tasks", pathname: "/", search: "" }, hist)).toBeNull();
  expect(calls.length).toBe(1);
  // Removed even when it is not a code the page would trade.
  expect(takePairCode({ hash: "#pair=<script>", pathname: "/", search: "" }, hist)).toBeNull();
  expect(calls.length).toBe(2);
});

test("a code is always traded, even with a stored session, and the new session is adopted", async () => {
  const st = new MemoryStorage();
  st.setItem(STORAGE_KEY, JSON.stringify({ token: "old", verifier: sha256("old-secret") }));
  const session = new Session(st, "the-code");
  const link = createLink(fakeEnv(), session, { snapshot() {}, push: () => true, shutdown() {}, now: Date.now });
  link.start();
  const s = FakeSocket.all[0];
  s.open();
  await settle();
  expect(s.methods()).toEqual(["web_pair"]);
  expect(s.last("web_pair").params).toEqual({ code: "the-code" });
  s.reply(s.last("web_pair").id, { token: "new", verifier: sha256("new-secret") });
  await settle();
  expect(s.last("hello").params.token).toBe("new");
  // Token and verifier under the one key, and nowhere else; the proof nowhere.
  expect([...st.map.keys()]).toEqual([STORAGE_KEY]);
  expect(JSON.parse(st.map.get(STORAGE_KEY)!)).toEqual({ token: "new", verifier: sha256("new-secret") });
  s.reply(s.last("hello").id, { role: "operator", protocol: 5, version: "x", pid: 1, web_proof: "new-secret" });
  await settle();
  expect(JSON.stringify([...st.map.values()])).not.toContain("new-secret");
});

test("a spent code with a stored session uses the stored one; without one the page is signed out", async () => {
  const st = new MemoryStorage();
  st.setItem(STORAGE_KEY, JSON.stringify({ token: "kept", verifier: sha256("x") }));
  const link = createLink(fakeEnv(), new Session(st, "spent"), { snapshot() {}, push: () => true, shutdown() {}, now: Date.now });
  link.start();
  let s = FakeSocket.all[0];
  s.open();
  await settle();
  s.fail(s.last("web_pair").id, "unauthorized", "unknown or spent code");
  await settle();
  expect(s.last("hello").params.token).toBe("kept");

  FakeSocket.all = [];
  const empty = new MemoryStorage();
  const link2 = createLink(fakeEnv(), new Session(empty, "spent"), { snapshot() {}, push: () => true, shutdown() {}, now: Date.now });
  link2.start();
  s = FakeSocket.all[0];
  s.open();
  await settle();
  s.fail(s.last("web_pair").id, "unauthorized", "unknown or spent code");
  await settle();
  expect(link2.conn()).toBe("signed-out");
  expect(s.methods()).toEqual(["web_pair"]);
});

test("without a code or a session the page is signed out and asks nothing", async () => {
  const link = createLink(fakeEnv(), new Session(new MemoryStorage(), null), { snapshot() {}, push: () => true, shutdown() {}, now: Date.now });
  link.start();
  FakeSocket.all[0].open();
  await settle();
  expect(link.conn()).toBe("signed-out");
  expect(FakeSocket.all[0].sent).toEqual([]);
});

test("the key is read once: later changes to the storage change nothing the page does", async () => {
  const st = new MemoryStorage();
  st.setItem(STORAGE_KEY, JSON.stringify({ token: "first", verifier: sha256("first-secret") }));
  const session = new Session(st, null);
  expect(st.reads).toBe(1);
  // Another document of the origin rewrites the storage.
  st.setItem(STORAGE_KEY, JSON.stringify({ token: "attacker", verifier: sha256("attacker") }));
  const link = createLink(fakeEnv(), session, { snapshot() {}, push: () => true, shutdown() {}, now: Date.now });
  link.start();
  let s = FakeSocket.all[0];
  s.open();
  await settle();
  expect(s.last("hello").params.token).toBe("first");
  // A reconnect still checks against the first verifier: the attacker's proof is refused.
  s.reply(s.last("hello").id, { role: "operator", protocol: 5, version: "x", pid: 1, web_proof: "attacker" });
  await settle();
  expect(link.conn()).toBe("stopped");
  expect(st.reads).toBe(1);
  s = FakeSocket.all[0];
  expect(FakeSocket.all.length).toBe(1);
});

test("the proof must hash to the verifier", async () => {
  const c = { token: "t", verifier: sha256("secret") };
  expect(await proves("secret", c)).toBe(true);
  expect(await proves(c.verifier, c)).toBe(false);
  expect(await proves("", c)).toBe(false);
  expect(await proves(undefined, c)).toBe(false);
  expect(await proves(42, c)).toBe(false);
});

test("no storage listener and no service worker registration exist in src/; storage is touched in one module", () => {
  for (const [path, text] of sources()) {
    expect(text, path).not.toMatch(/addEventListener\(\s*["']storage["']/);
    expect(text, path).not.toMatch(/onstorage/);
    expect(text, path).not.toMatch(/serviceWorker/);
    expect(text, path).not.toMatch(/sessionStorage|indexedDB|document\.cookie/);
    if (!path.endsWith("net/session.ts") && !path.endsWith("main.tsx")) expect(text, path).not.toMatch(/localStorage/);
  }
});

test("import( appears only at the top level of the entry module", () => {
  for (const [path, text] of sources()) {
    const code = text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
    const uses = [...code.matchAll(/\bimport\s*\(/g)];
    if (path.endsWith("main.tsx")) {
      expect(uses.length).toBe(1);
      // At the top level: the line starts the statement, outside any function.
      expect(text).toMatch(/^const terminal = import\("\.\/term\/Terminal"\);$/m);
    } else expect(uses.length, path).toBe(0);
  }
});

test("the page never reloads or navigates by itself", () => {
  for (const [path, text] of sources()) {
    expect(text, path).not.toMatch(/location\.(reload|assign|replace)\s*\(|location\.href\s*=|window\.location\s*=/);
  }
});
