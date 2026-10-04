// Measures what notes/web-multi.md says about the shared server, against two
// real daemons: the extra hop, switching, a restarted daemon, line breaks.
// Prints numbers and PASS/FAIL lines; never a token, code or secret.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { connect } from "node:net";
import { open } from "./ws.mjs";

const { ROOT, CREW, HUB_PORT, HUB_PORT_NOCHECK, LABEL, CODE, SECRET } = process.env;
const N = Number(process.env.N ?? 300);
const crew = (...a) => execFileSync(CREW, a, { encoding: "utf8", cwd: ROOT });
const sha = (s) => createHash("sha256").update(s).digest("hex");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const q = (xs, p) => { const s = [...xs].sort((a, b) => a - b); return s.length ? s[Math.min(s.length - 1, Math.floor(p * s.length))] : NaN; };
const stat = (xs) => `p50 ${q(xs, 0.5).toFixed(0)} µs, p95 ${q(xs, 0.95).toFixed(0)} µs, max ${q(xs, 1).toFixed(0)} µs (n ${xs.length})`;
let failed = 0;
const check = (name, ok, detail = "") => { console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `: ${detail}` : ""}`); if (!ok) failed++; };

// The daemon's own server for workspace a (the shipped one), to compare with.
const native = JSON.parse(crew("web", "--json", "--no-open", "-C", `${ROOT}/a`));
const nativeHost = new URL(native.url).host;
const hubHost = `${LABEL}.localhost:${HUB_PORT}`;

// An operator on a's Unix socket: the sender.
const sockA = JSON.parse(crew("status", "--json", "-C", `${ROOT}/a`)).session;
const reg = readdirSync(`${process.env.XDG_STATE_HOME}/crew/sessions`).map((f) => JSON.parse(readFileSync(`${process.env.XDG_STATE_HOME}/crew/sessions/${f}`, "utf8")));
const infoOf = (name) => reg.find((r) => r.project === name);
const idOf = (info) => info.socket.split("/").pop().replace(".sock", "");
const tokenOf = (info) => readFileSync(info.socket.replace(/\.sock$/, ".token"), "utf8").trim();
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
    s.on("data", (d) => { buf += d; let nl; while ((nl = buf.indexOf("\n")) >= 0) { const f = JSON.parse(buf.slice(0, nl)); buf = buf.slice(nl + 1); const w = waiting.get(f.id); if (w) (waiting.delete(f.id), w(f)); } });
    s.on("connect", async () => { await c.call("hello", { role: "operator", token: tokenOf(info), client: "spike", protocol: 5 }); resolve(c); });
  });
}
const a = infoOf("a"), b = infoOf("b");
const sender = await unixOperator(a);

// --- the index ---------------------------------------------------------------
const index = await open(HUB_PORT, hubHost);
const paired = await index.call("web_pair", { code: CODE });
const session = paired.result;
const hello = await index.call("hello", { role: "operator", token: session.token, client: "web", protocol: 5 });
check("index hello: the proof hashes to the verifier", sha(hello.result.web_proof) === session.verifier);
const list = (await index.call("workspaces")).result;
check("index lists both running workspaces", list.length === 2 && list.some((w) => w.id === idOf(a)) && list.some((w) => w.id === idOf(b)), list.map((w) => w.project).join(", "));
check("a call the index does not have is refused there", (await index.call("snapshot", {})).error?.code === "invalid");

async function viaHub(id, onPush) {
  const c = await open(HUB_PORT, hubHost, { onPush });
  const h = await c.call("hello", { role: "operator", token: session.token, client: "web", protocol: 5, workspace: id });
  return { c, h };
}
async function viaNative(onPush) {
  const c = await open(native.port, nativeHost, { onPush });
  const p = await c.call("web_pair", { code: new URL(JSON.parse(crew("web", "--json", "--no-open", "-C", `${ROOT}/a`)).pair_url).hash.replace("#pair=", "") });
  const h = await c.call("hello", { role: "operator", token: p.result.token, client: "web", protocol: 5 });
  return { c, h };
}
{
  const { c, h } = await viaHub(idOf(a));
  check("workspace hello through the hub: the daemon's reply plus the proof", h.result?.project === "a" && sha(h.result.web_proof) === session.verifier && h.result.workspace === idOf(a));
  const snap = await c.call("snapshot", { messages: 0, events: 0 });
  check("the connection is that workspace's daemon", snap.result?.session?.project === "a" && snap.result.agents.length === 10);
  c.close();
  const wrong = await open(HUB_PORT, hubHost);
  check("hello with a token that is not the session's is refused", (await wrong.call("hello", { role: "operator", token: "x".repeat(64), client: "web", protocol: 5, workspace: idOf(a) })).error?.code === "unauthorized");
  check("hello for a workspace that is not running is unavailable", (await wrong.call("hello", { role: "operator", token: session.token, client: "web", protocol: 5, workspace: "0".repeat(16) })).error?.code === "unavailable");
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
  hub.push(...(await latency((onPush) => viaHub(idOf(a), onPush), `h${round}`, N / 4)));
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
for (let i = 0; i < 30; i++) { const r = await switchTo(i % 2 ? idOf(a) : idOf(b)); sw.push(r.ms * 1000); bytes = Math.max(bytes, r.bytes); }
console.log(`M2 connect, hello, subscribe, snapshot (300 messages, 1000 events; up to ${(bytes / 1024).toFixed(0)} KiB): p50 ${(q(sw, 0.5) / 1000).toFixed(1)} ms, p95 ${(q(sw, 0.95) / 1000).toFixed(1)} ms`);
check("M2 a switch has its snapshot within 50 ms at p95", q(sw, 0.95) / 1000 < 50);

// --- M4: a line break in a message ----------------------------------------------
{
  const { c } = await viaHub(idOf(a));
  c.sendRaw('{"id":901,"method":"ping"}\n{"id":902,"method":"ping"}');
  const closed = await Promise.race([c.whenClosed, sleep(2000)]);
  check("M4 a message with a line break closes the connection (1008), nothing reaches the daemon", closed?.code === 1008 && c.stray.length === 0, JSON.stringify(closed));
  const loose = await open(HUB_PORT_NOCHECK, `${LABEL}.localhost:${HUB_PORT_NOCHECK}`);
  await loose.call("hello", { role: "operator", token: session.token, client: "web", protocol: 5, workspace: idOf(a) });
  loose.sendRaw('{"id":901,"method":"ping"}\n{"id":902,"method":"ping"}');
  await sleep(300);
  check("M4 without the check the daemon answers two requests for one message", loose.stray.length === 2, `${loose.stray.length} replies`);
  loose.close();
}

// --- M3: a daemon stops and comes back -------------------------------------------
{
  const changes = [];
  index.onPush = (push, at) => push.kind === "workspaces" && changes.push({ at, ids: push.data.map((w) => `${w.project}:${w.pid}`) });
  const { c } = await viaHub(idOf(b));
  await c.call("subscribe");
  const tokenBefore = sha(tokenOf(b));
  const t0 = process.hrtime.bigint();
  crew("stop", "-C", `${ROOT}/b`);
  const closed = await Promise.race([c.whenClosed, sleep(3000)]);
  check("M3 the workspace's socket ends with 4001 when its daemon stops", closed?.code === 4001, JSON.stringify(closed));
  await sleep(400);
  check("M3 the index says b is gone, and stays connected", changes.length >= 1 && changes.at(-1).ids.length === 1 && index.closed === null, `${((Number(changes[0]?.at - t0)) / 1e6).toFixed(0)} ms after the stop was asked for`);
  check("M3 a is untouched", (await (await viaHub(idOf(a))).c.call("snapshot", { messages: 0, events: 0 })).result?.session?.project === "a");
  crew("run", "b.yaml", "--no-attach");
  const t1 = Date.now();
  while (Date.now() - t1 < 5000 && !(changes.at(-1)?.ids.length === 2)) await sleep(20);
  const b2 = JSON.parse(readFileSync(`${process.env.XDG_STATE_HOME}/crew/sessions/${idOf(b)}.json`, "utf8"));
  check("M3 the restarted daemon has the same id, another pid and another operator token", idOf(b2) === idOf(b) && b2.pid !== b.pid && sha(tokenOf(b2)) !== tokenBefore);
  check("M3 the index says b is back", changes.at(-1)?.ids.length === 2);
  const again = await viaHub(idOf(b));
  check("M3 the same browser session reaches the restarted workspace", again.h.result?.project === "b" && sha(again.h.result.web_proof) === session.verifier && again.h.result.pid === b2.pid);
  again.c.close();
}
sender.close();
index.close();
console.log(failed ? `FAIL (${failed})` : "PASS");
process.exit(failed ? 1 : 0);
