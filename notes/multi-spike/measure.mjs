// Measures what notes/web-multi.md says about the shared server, against two
// real daemons: the extra hop, switching, a line break, taking a workspace
// out, a daemon that stops and comes back in every order, the listing's
// revisions. Where revision 1 of the design was wrong, the wrong behaviour is
// replayed first ("revision 1, reproduced") and then the rule that replaces
// it. Prints numbers and PASS/FAIL lines; never a token, code or secret.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { connect } from "node:net";
import { open } from "./ws.mjs";

const { ROOT, CREW, HUB_PORT, HUB_PORT_NOCHECK, LABEL, CODE } = process.env;
const N = Number(process.env.N ?? 300);
const crew = (...a) => execFileSync(CREW, a, { encoding: "utf8", cwd: ROOT });
const sha = (s) => createHash("sha256").update(s).digest("hex");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const q = (xs, p) => { const s = [...xs].sort((a, b) => a - b); return s.length ? s[Math.min(s.length - 1, Math.floor(p * s.length))] : NaN; };
const stat = (xs) => `p50 ${q(xs, 0.5).toFixed(0)} µs, p95 ${q(xs, 0.95).toFixed(0)} µs, max ${q(xs, 1).toFixed(0)} µs (n ${xs.length})`;
let failed = 0;
const check = (name, ok, detail = "") => { console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `: ${detail}` : ""}`); if (!ok) failed++; };
const until = async (what, ms = 5000) => { const t0 = Date.now(); while (Date.now() - t0 < ms) { if (await what()) return true; await sleep(20); } return false; };

// The daemon's own server for workspace a (the shipped one), to compare with.
const native = JSON.parse(crew("web", "--json", "--no-open", "-C", `${ROOT}/a`));
const nativeHost = new URL(native.url).host;
const hubHost = `${LABEL}.localhost:${HUB_PORT}`;

const sessions = () => readdirSync(`${process.env.XDG_STATE_HOME}/crew/sessions`).map((f) => JSON.parse(readFileSync(`${process.env.XDG_STATE_HOME}/crew/sessions/${f}`, "utf8")));
const reg = sessions();
const infoOf = (name) => reg.find((r) => r.project === name);
const idOf = (info) => info.socket.split("/").pop().replace(".sock", "");
const tokenOf = (info) => readFileSync(info.socket.replace(/\.sock$/, ".token"), "utf8").trim();
/** An operator on a daemon's own Unix socket. */
function unixOperator(info) {
  return new Promise((resolve) => {
    const s = connect(info.socket);
    let buf = "", next = 1;
    const waiting = new Map();
    const c = {
      send(method, params) { const id = next++; s.write(JSON.stringify({ id, method, params }) + "\n"); return id; },
      call(method, params) { const id = c.send(method, params); return new Promise((ok) => waiting.set(id, ok)); },
      close() { s.destroy(); },
    };
    s.on("error", () => {});
    s.on("data", (d) => { buf += d; let nl; while ((nl = buf.indexOf("\n")) >= 0) { const f = JSON.parse(buf.slice(0, nl)); buf = buf.slice(nl + 1); const w = waiting.get(f.id); if (w) (waiting.delete(f.id), w(f)); } });
    s.on("connect", async () => { await c.call("hello", { role: "operator", token: tokenOf(info), client: "spike", protocol: 5 }); resolve(c); });
  });
}
const a = infoOf("a"), b = infoOf("b");
const A = idOf(a), B = idOf(b);
const sender = await unixOperator(a);

// --- the index ---------------------------------------------------------------
// Every listing the index connection is sent, in the order it arrives.
const listings = [];
let onListing = () => {};
const index = await open(HUB_PORT, hubHost, { onPush: (push, at) => { if (push.kind === "workspaces") { listings.push({ via: "push", rev: push.data.rev, at }); onListing(push.data, at); } } });
const paired = await index.call("web_pair", { code: CODE });
const session = paired.result;
const hello = await index.call("hello", { role: "operator", token: session.token, client: "web", protocol: 5 });
check("index hello: the proof hashes to the verifier", sha(hello.result.web_proof) === session.verifier);
const listing = async () => { const r = (await index.call("workspaces")).result; listings.push({ via: "reply", rev: r.rev }); return r; };
const entry = async (id) => (await listing()).joined.find((w) => w.id === id);
const knobs = (k) => index.call("spike_knobs", k);
{
  const l = await listing();
  check("index lists both running workspaces", l.joined.length === 2 && l.joined.every((w) => w.running) && l.joined.some((w) => w.id === A) && l.joined.some((w) => w.id === B), l.joined.map((w) => w.project).join(", "));
  check("a call the index does not have is refused there", (await index.call("snapshot", {})).error?.code === "invalid");
}

const helloParams = (extra) => ({ role: "operator", token: session.token, client: "web", protocol: 5, ...extra });
async function viaHub(id, onPush, opts = {}) {
  const c = await open(HUB_PORT, hubHost, { onPush, ...opts });
  const h = await c.call("hello", helloParams({ workspace: id }));
  return { c, h };
}
async function viaNative(onPush) {
  const c = await open(native.port, nativeHost, { onPush });
  const p = await c.call("web_pair", { code: new URL(JSON.parse(crew("web", "--json", "--no-open", "-C", `${ROOT}/a`)).pair_url).hash.replace("#pair=", "") });
  const h = await c.call("hello", { role: "operator", token: p.result.token, client: "web", protocol: 5 });
  return { c, h };
}
{
  const { c, h } = await viaHub(A);
  check("workspace hello through the hub: the daemon's reply plus the proof", h.result?.project === "a" && sha(h.result.web_proof) === session.verifier && h.result.workspace === A);
  const snap = await c.call("snapshot", { messages: 0, events: 0 });
  check("the connection is that workspace's daemon", snap.result?.session?.project === "a" && snap.result.agents.length === 10);
  check("a second hello on it is the daemon's to refuse, and it does", (await c.call("hello", { role: "agent", agent_id: "w1", token: "x", client: "web", protocol: 5 })).error?.code === "forbidden");
  c.close();
  const wrong = await open(HUB_PORT, hubHost);
  check("hello with a token that is not the session's is refused", (await wrong.call("hello", { role: "operator", token: "x".repeat(64), client: "web", protocol: 5, workspace: A })).error?.code === "unauthorized");
  check("hello for a workspace that is not joined is not_found", (await wrong.call("hello", helloParams({ workspace: "0".repeat(16) }))).error?.code === "not_found");
  wrong.close();
}

// --- M1: the extra hop ---------------------------------------------------------
// A `send` on the Unix socket; the `message` push it causes, timed at a
// subscriber. One subscriber at a time, alternating, so neither waits for
// the other in the daemon or in this process.
async function latency(make, label, n) {
  const sent = new Map(), out = [];
  let done;
  const all = new Promise((r) => (done = r));
  const { c } = await make((push, at) => {
    if (push.kind !== "message") return;
    const t0 = sent.get(push.data.id);
    if (t0 === undefined) return;
    out.push(Number(at - t0) / 1000);
    if (out.length === n) done();
  });
  await c.call("subscribe");
  for (let i = 0; i < n; i++) {
    const id = `m-spike-${label}-${Date.now().toString(36)}-${i}`;
    sent.set(id, process.hrtime.bigint());
    sender.send("send", { to: `w${(i % 10) + 1}`, type: "message", body: `spike ${i}`, id });
    await sleep(8);
  }
  await Promise.race([all, sleep(5000)]);
  c.close();
  return out;
}
const nat = [], hub = [];
for (let round = 0; round < 4; round++) {
  nat.push(...(await latency((onPush) => viaNative(onPush), `n${round}`, N / 4)));
  hub.push(...(await latency((onPush) => viaHub(A, onPush), `h${round}`, N / 4)));
}
console.log(`M1 send to push, the daemon's own server: ${stat(nat)}`);
console.log(`M1 send to push, through the hub:         ${stat(hub)}`);
const hop = q(hub, 0.5) - q(nat, 0.5), hop95 = q(hub, 0.95) - q(nat, 0.95);
console.log(`M1 the hop costs ${hop.toFixed(0)} µs at the median, ${hop95.toFixed(0)} µs at p95`);
check("M1 every push arrived on both paths", nat.length === N && hub.length === N, `${nat.length}, ${hub.length} of ${N}`);
check("M1 the hop adds less than 150 µs at the median", hop < 150);

// --- M2: switching -------------------------------------------------------------
async function switchTo(id) {
  const t0 = process.hrtime.bigint();
  const { c } = await viaHub(id);
  await c.call("subscribe");
  const snap = await c.call("snapshot", { messages: 300, events: 1000 });
  const ms = Number(process.hrtime.bigint() - t0) / 1e6;
  c.close();
  return { ms, bytes: JSON.stringify(snap).length };
}
const sw = [];
let bytes = 0;
for (let i = 0; i < 30; i++) { const r = await switchTo(i % 2 ? A : B); sw.push(r.ms * 1000); bytes = Math.max(bytes, r.bytes); }
console.log(`M2 connect, hello, subscribe, snapshot (300 messages, 1000 events; up to ${(bytes / 1024).toFixed(0)} KiB): p50 ${(q(sw, 0.5) / 1000).toFixed(1)} ms, p95 ${(q(sw, 0.95) / 1000).toFixed(1)} ms`);
check("M2 a switch has its snapshot within 50 ms at p95", q(sw, 0.95) / 1000 < 50);

// --- M4: a line break in a message ----------------------------------------------
{
  const { c } = await viaHub(A);
  c.sendRaw('{"id":901,"method":"ping"}\n{"id":902,"method":"ping"}');
  const closed = await Promise.race([c.whenClosed, sleep(2000)]);
  check("M4 a message with a line break closes the connection (1008), nothing reaches the daemon", closed?.code === 1008 && c.stray.length === 0, JSON.stringify(closed));
  const loose = await open(HUB_PORT_NOCHECK, `${LABEL}.localhost:${HUB_PORT_NOCHECK}`);
  await loose.call("hello", helloParams({ workspace: A }));
  loose.sendRaw('{"id":901,"method":"ping"}\n{"id":902,"method":"ping"}');
  await sleep(300);
  check("M4 without the check the daemon answers two requests for one message", loose.stray.length === 2, `${loose.stray.length} replies`);
  loose.close();
}

// --- M5: taking a workspace out --------------------------------------------------
const remove = (revoke) => index.call("spike_remove", { id: B, revoke });
const join = () => index.call("spike_join", { id: B });
{
  let opB = await unixOperator(b);
  const delivered = async (id) => !(await opB.call("message_get", { id })).error;
  /** A client that ignores the index and every goodbye: it sends a message
   * every 5 ms from before the removal until 300 ms after its answer. */
  async function stubborn(revoke, tag) {
    const { c } = await viaHub(B, undefined, { ignoreClose: true });
    await c.call("subscribe");
    const sentAt = new Map();
    let n = 0, sending = true;
    const loop = (async () => { while (sending) { const id = `m-spike-${tag}-${n++}`; sentAt.set(id, Date.now()); c.sendRaw(JSON.stringify({ id: 1000 + n, method: "send", params: { to: "w1", type: "message", body: "still here", id } })); await sleep(5); } })();
    await sleep(150);
    const r = await remove(revoke);
    const removedAt = Date.now();
    await sleep(300);
    sending = false;
    await loop;
    await sleep(200);
    let before = 0, after = 0;
    for (const [id, at] of sentAt) if (await delivered(id)) at < removedAt ? before++ : after++;
    const sentAfter = [...sentAt.values()].filter((at) => at >= removedAt).length;
    return { c, r, before, after, sentAfter };
  }

  // As revision 1 read: the row leaves the table, and that is all.
  {
    const s = await stubborn(false, "v1");
    const gone = !(await entry(B));
    const fresh = await open(HUB_PORT, hubHost);
    const refused = (await fresh.call("hello", helloParams({ workspace: B }))).error?.code === "not_found";
    fresh.close();
    check("M5 (revision 1, reproduced) a connection opened before the removal still acts on the workspace after it", gone && refused && s.before > 0 && s.after > 0, `${s.after} of the ${s.sentAfter} messages sent after the removal answered were delivered`);
    s.c.close();
    await join();
  }
  {
    const p = await open(HUB_PORT, hubHost);
    const pending = p.call("hello", helloParams({ workspace: B, spike_delay_ms: 400 }));
    await sleep(100);
    await remove(false);
    const h = await pending;
    const snap = await p.call("snapshot", { messages: 0, events: 0 });
    check("M5 (revision 1, reproduced) a hello that was dialling during the removal is answered and works afterwards", !h.error && snap.result?.session?.project === "b");
    p.close();
    await join();
  }

  // Revision 2: the generation is revoked first; the answer comes when every
  // connection of it let go of the daemon.
  const before = (await entry(B)).generation;
  {
    const second = (await viaHub(B, undefined, { ignoreClose: true })).c; // a terminal's connection, idle
    const s = await stubborn(true, "v2");
    check("M5 removal answers once every connection of the workspace let go of its daemon", s.r.result?.removed === true && s.r.result.drained === true && s.r.result.generation === before);
    check("M5 a client that ignores the removal: nothing it sent after the answer reached the daemon", s.before > 0 && s.after === 0 && s.sentAfter > 20, `${s.before} delivered before, ${s.after} of ${s.sentAfter} after`);
    const [c1, c2] = [s.c.closed, second.closed];
    check("M5 both connections were told 4002", c1?.code === 4002 && c2?.code === 4002, JSON.stringify([c1?.code, c2?.code]));
    const fresh = await open(HUB_PORT, hubHost);
    check("M5 a new hello for it is not_found", (await fresh.call("hello", helloParams({ workspace: B }))).error?.code === "not_found");
    fresh.close();
    s.c.close(); second.close();
    // The spike joins what it sees (the design's `join = "all"`): a rescan must leave it out.
    await index.call("spike_rescan");
    await sleep(400);
    check("M5 a rescan does not bring back a workspace the operator took out", !(await entry(B)) && (await entry(A))?.running === true);
  }
  {
    const j = await join();
    const { c, h } = await viaHub(B);
    check("M5 joining again is a new generation, and works", j.result?.generation > before && h.result?.project === "b", `${before} then ${j.result?.generation}`);
    c.close();
  }
  {
    const p = await open(HUB_PORT, hubHost);
    const t0 = Date.now();
    let answeredAt = 0;
    const pending = p.call("hello", helloParams({ workspace: B, spike_delay_ms: 1500 })).then((h) => ((answeredAt = Date.now()), h));
    await sleep(100);
    const r = await remove(true);
    const removedAt = Date.now();
    const h = await pending;
    check("M5 a hello dialling during the removal is refused (not_found) without waiting for its dial, and the removal waited for it", h.error?.code === "not_found" && r.result?.drained === true && answeredAt - t0 < 600 && answeredAt <= removedAt + 5, `refused after ${answeredAt - t0} ms, removal answered after ${removedAt - t0} ms`);
    const snap = await Promise.race([p.call("snapshot", { messages: 0, events: 0 }).catch(() => null), sleep(1000).then(() => null)]);
    check("M5 that connection cannot act", !snap?.result);
    p.close();
    await join();
  }
  // Forced: a connection that does not let go is aborted after a second, and
  // the answer waits until its daemon side is really gone.
  {
    await knobs({ stuck_pipe_ms: 8000 });
    const { c } = await viaHub(B, undefined, { ignoreClose: true });
    const t0 = Date.now();
    const r = await remove(true);
    const took = Date.now() - t0;
    const id = `m-spike-stuck-${t0}`;
    c.sendRaw(JSON.stringify({ id: 1, method: "send", params: { to: "w1", type: "message", body: "after the abort", id } }));
    await sleep(300);
    check("M5 a connection that holds on is aborted after a second; the removal answers when its daemon side is gone, not when the abort was asked", r.result?.drained === true && r.result.aborted === 1 && took >= 1000 && took < 2500 && !(await delivered(id)), `answered after ${took} ms, ${r.result?.aborted} aborted`);
    await knobs({ stuck_pipe_ms: 0 });
    c.close();
    await join();
  }
  // A browser that does not read: its connection is stuck writing to it and
  // never looks at the revocation.
  {
    const { c } = await viaHub(B, undefined, { ignoreClose: true });
    c.pause();
    for (let i = 0; i < 200; i++) c.sendRaw(JSON.stringify({ id: 5000 + i, method: "snapshot", params: { messages: 300, events: 1000 } }));
    await sleep(1500);
    const t0 = Date.now();
    const r = await remove(true);
    const took = Date.now() - t0;
    const id = `m-spike-slow-${t0}`;
    c.sendRaw(JSON.stringify({ id: 1, method: "send", params: { to: "w1", type: "message", body: "after the abort", id } }));
    await sleep(300);
    check("M5 a browser that does not read cannot hold the removal up: its connection is aborted, then the answer", r.result?.drained === true && r.result.aborted >= 1 && took >= 1000 && took < 2500 && !(await delivered(id)), `answered after ${took} ms, ${r.result?.aborted} aborted`);
    c.close();
    await join();
  }
  opB.close();
}

// --- M7: one failed dial while the workspace runs ---------------------------------
{
  const before = await listing();
  const pushes = listings.length;
  const p = await open(HUB_PORT, hubHost);
  const first = await p.call("hello", helloParams({ workspace: A, spike_fail_dial: true }));
  await sleep(1000);
  const after = await listing();
  const quiet = listings.slice(pushes).every((l) => l.via === "reply") && after.rev === before.rev && after.joined.find((w) => w.id === A).running;
  check("M7 (revision 1, reproduced) a dial fails, the listing still says running and nothing more is said: a page waiting for a listing would wait for ever", first.error?.code === "unavailable" && quiet);
  const second = await p.call("hello", helloParams({ workspace: A }));
  check("M7 trying again while the listing says running gets through", second.result?.project === "a");
  p.close();
}

// --- M8: the page's rules for a connection's outcome, replayed ----------------------
// The spike has no page. This is a model of what §multi-ui says the page does
// for one workspace, as revision 2 had it and as revision 3 has it, driven by
// the real hub. `closeDelay` is how late the page notices a close.
function pageModel(rule, id, firstHello = {}) {
  const m = { state: "none", listing: null, conn: null, attempt: 0, tried: 0, closeDelay: 0 };
  const entryOf = () => m.listing?.joined.find((w) => w.id === id);
  async function evaluate() {
    const e = entryOf();
    if (!e) return void (m.state = "removed");
    if (!e.running) return void (m.state = "not-running");
    if (m.conn) return void (m.state = "live");
    m.state = "connecting";
    const attempt = ++m.attempt;
    const c = await open(HUB_PORT, hubHost, { ignoreClose: true });
    const h = await c.call("hello", helloParams({ workspace: id, ...(m.tried++ === 0 ? firstHello : {}) }));
    if (attempt !== m.attempt) return c.close();
    if (h.error) { c.close(); return outcome(attempt, h.error.code); }
    m.conn = c;
    m.state = "live";
    c.whenClosed.then((closed) => setTimeout(() => outcome(attempt, closed?.code ?? 1006), m.closeDelay));
  }
  async function outcome(attempt, what) {
    if (attempt !== m.attempt) return; // from a connection it replaced: dropped, in both versions
    m.conn?.close();
    m.conn = null;
    if (rule === 2) {
      // Revision 2: these say what the workspace is, until another listing comes.
      if (what === 4002 || what === "not_found") return void (m.state = "removed");
      if (what === "protocol") return void (m.state = "incompatible");
      return evaluate();
    }
    // Revision 3: an outcome ends its attempt and says nothing about the
    // workspace. These three mean the listing the attempt was started under
    // may be out of date: ask, then act on the newest listing there is,
    // whether or not the answer was newer.
    if (what === 4002 || what === "not_found" || what === "protocol") {
      const r = await listing();
      if (!m.listing || r.rev > m.listing.rev) m.listing = r;
    }
    return evaluate();
  }
  return {
    m,
    /** A listing reaches the page. */
    async listing(l) { if (m.listing && l.rev <= m.listing.rev) return; m.listing = l; await evaluate(); },
    close() { m.attempt++; m.conn?.close(); },
  };
}
for (const rule of [2, 3]) {
  // Taken out and joined again within one coalescing interval: the page gets
  // one listing, "running", and then the old connection's close.
  const p = pageModel(rule, B);
  await p.listing(await listing());
  const g0 = (await entry(B)).generation;
  p.m.closeDelay = 400;
  await remove(true);
  await join();
  await until(async () => (await entry(B))?.running === true);
  await p.listing(await listing());
  await sleep(1500);
  const now = await listing();
  const e = now.joined.find((w) => w.id === B);
  if (rule === 2) check("M8 (revision 2, reproduced) taken out and joined again, the old close arrives after the new listing: the page says removed, the listing says running, and nothing more will be said", p.m.state === "removed" && e?.running === true && e.generation > g0 && now.rev === p.m.listing.rev);
  else {
    const snap = await p.m.conn?.call("snapshot", { messages: 0, events: 0 });
    check("M8 the old close ends the old attempt and nothing else: the page asks for the listing, finds running, and is live on the new generation", p.m.state === "live" && p.m.attempt === 2 && snap?.result?.session?.project === "b" && e.generation > g0);
  }
  p.close();
}
for (const rule of [2, 3]) {
  // An error that is about a daemon that is no longer the one the listing shows.
  const p = pageModel(rule, B, { spike_refuse: "protocol" });
  await p.listing(await listing());
  await sleep(800);
  const now = await listing();
  if (rule === 2) check("M8 (revision 2, reproduced) a late `protocol` answer: the page says incompatible, the listing says running, and nothing more will be said", p.m.state === "incompatible" && now.joined.find((w) => w.id === B)?.running === true && now.rev === p.m.listing.rev);
  else check("M8 a late `protocol` answer ends its attempt: the listing, asked again and unchanged, still says running, and the next attempt is live", p.m.state === "live" && p.m.attempt === 2 && now.rev === p.m.listing.rev);
  p.close();
}

// --- M3: a daemon stops and comes back; nothing is read on a timer ----------------
const stopB = () => crew("stop", "-C", `${ROOT}/b`);
const startB = () => crew("run", "b.yaml", "--no-attach");
const pidOfB = () => sessions().find((s) => s.project === "b")?.pid;
let lastB = b;
{
  const scans0 = (await index.call("spike_stats")).result.scans;
  let stoppedAt = null;
  onListing = (l, at) => { if (stoppedAt === null && l.joined.find((w) => w.id === B)?.running === false) stoppedAt = at; };
  const { c } = await viaHub(B);
  await c.call("subscribe");
  const tokenBefore = sha(tokenOf(b));
  const t0 = process.hrtime.bigint();
  stopB();
  const closed = await Promise.race([c.whenClosed, sleep(3000)]);
  check("M3 the workspace's socket ends with 4001 when its daemon stops", closed?.code === 4001, JSON.stringify(closed));
  await until(() => stoppedAt !== null, 3000);
  const e = await entry(B);
  check("M3 the index says b is not running, keeps it joined, and stays connected", stoppedAt !== null && e?.running === false && index.closed === null, `${(Number(stoppedAt - t0) / 1e6).toFixed(0)} ms after the stop was asked for`);
  check("M3 it learned that from the end of its watch: no scan ran", (await index.call("spike_stats")).result.scans === scans0);
  const refused = await open(HUB_PORT, hubHost);
  check("M3 hello for a joined workspace that is not running is unavailable", (await refused.call("hello", helloParams({ workspace: B }))).error?.code === "unavailable");
  refused.close();
  check("M3 a is untouched", (await (await viaHub(A)).c.call("snapshot", { messages: 0, events: 0 })).result?.session?.project === "a");
  startB();
  await index.call("spike_rescan"); // what the daemon says when it has started
  const back = await until(async () => (await entry(B))?.running === true);
  const b2 = sessions().find((s) => s.project === "b");
  lastB = b2;
  check("M3 the restarted daemon has the same id, another pid and another operator token", idOf(b2) === B && b2.pid !== b.pid && sha(tokenOf(b2)) !== tokenBefore);
  check("M3 the index says b is back, with the new pid", back && (await entry(B)).pid === b2.pid);
  const again = await viaHub(B);
  check("M3 the same browser session reaches the restarted workspace", again.h.result?.project === "b" && sha(again.h.result.web_proof) === session.verifier && again.h.result.pid === b2.pid);
  again.c.close();
  onListing = () => {};
}

// --- M6: the orders in which a restart can be noticed ------------------------------
{
  // (a) The rescan for the new daemon is handled before the old watch's end.
  // Revision 1: a rescan dials only entries without a watch; the end of a watch means stopped.
  await knobs({ v1_reconcile: true, watch_end_delay_ms: 1500 });
  stopB(); startB();
  await index.call("spike_rescan");
  await sleep(2500);
  const lost = await entry(B);
  check("M6 (revision 1, reproduced) rescan before the old watch's end: the new daemon runs and the dashboard says stopped, for good", lost?.running === false && pidOfB() !== undefined && pidOfB() !== lastB.pid, `daemon pid ${pidOfB()}, listing says running: ${lost?.running}`);
  await knobs({ v1_reconcile: false, watch_end_delay_ms: 0 });
  await index.call("spike_rescan");
  await until(async () => (await entry(B))?.running === true);
  lastB = sessions().find((s) => s.project === "b");

  // Revision 2: a watch belongs to one incarnation; the registry decides; a superseded watch's end is not news.
  await knobs({ watch_end_delay_ms: 1500 });
  stopB(); startB();
  const newPid = pidOfB();
  await index.call("spike_rescan");
  const quick = await until(async () => (await entry(B))?.pid === newPid && (await entry(B)).running, 1000);
  await sleep(2000); // the old watch's end arrives now
  const e = await entry(B);
  check("M6 rescan before the old watch's end: the new daemon is shown at once and stays shown when the old end arrives", quick && e?.running === true && e.pid === newPid && newPid !== lastB.pid);
  await knobs({ watch_end_delay_ms: 0 });
  lastB = sessions().find((s) => s.project === "b");

  // (b) A rescan asked for while a scan runs. The scan read the registry
  // before the daemon was there; the request made after it started must not
  // be swallowed by the scan under way.
  for (const lossy of [true, false]) {
    stopB();
    await until(async () => (await entry(B))?.running === false);
    await knobs({ lossy_rescan: lossy, scan_delay_ms: 1500 });
    const scans0 = (await index.call("spike_stats")).result.scans;
    await index.call("spike_rescan"); // reads the registry now: no b
    await sleep(50);
    startB();
    await index.call("spike_rescan"); // b's daemon says it started, during that scan
    await sleep(3600);
    const e = await entry(B);
    const scans = (await index.call("spike_stats")).result.scans - scans0;
    if (lossy) check("M6 (a rescan dropped while one runs, reproduced) the daemon that started during a scan is never seen", e?.running === false && scans === 1, `${scans} scan`);
    else check("M6 a rescan asked for during a scan is followed by another scan, which finds the daemon", e?.running === true && scans === 2, `${scans} scans`);
    await knobs({ lossy_rescan: false, scan_delay_ms: 0 });
    if (lossy) { await index.call("spike_rescan"); await until(async () => (await entry(B))?.running === true); }
  }
}

// --- the listing's revisions --------------------------------------------------------
{
  const pushes = listings.filter((l) => l.via === "push").map((l) => l.rev);
  check("revisions: every listing pushed has a greater rev than the one before", pushes.length > 6 && pushes.every((r, i) => i === 0 || r > pushes[i - 1]), `${pushes.length} pushes, rev ${pushes[0]} to ${pushes.at(-1)}`);
  let seen = 0, ok = true;
  for (const l of listings) { if (l.via === "reply" && l.rev < seen) ok = false; seen = Math.max(seen, l.rev); }
  check("revisions: a reply is never older than a push that arrived before it", ok);
}
sender.close();
index.close();
console.log(failed ? `FAIL (${failed})` : "PASS");
process.exit(failed ? 1 : 0);
