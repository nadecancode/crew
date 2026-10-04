// On one subscribed connection, can a push that is newer than a snapshot
// arrive before that snapshot's reply? (notes/web-design.md §transport,
// "Snapshot is a barrier".) Speaks the socket protocol directly.
//
//   bun snapshot-race.mjs <socket> [messages]
import net from "node:net";
import { readFileSync } from "node:fs";

const socket = process.argv[2];
const total = Number(process.argv[3] ?? 400);
const token = readFileSync(socket.replace(/\.sock$/, ".token"), "utf8").trim();

function connect(onFrame) {
  const c = net.createConnection(socket);
  let buf = "", next = 1;
  const waiting = new Map();
  c.on("data", (d) => {
    buf += d;
    for (let i; (i = buf.indexOf("\n")) >= 0; buf = buf.slice(i + 1)) {
      const f = JSON.parse(buf.slice(0, i));
      if (f.push) onFrame?.(f); else { onFrame?.(f); waiting.get(f.id)?.(f); waiting.delete(f.id); }
    }
  });
  const call = (method, params, mark) => new Promise((done) => { const id = next++; mark?.(id); waiting.set(id, done); c.write(JSON.stringify({ id, method, params }) + "\n"); });
  return { call, end: () => c.end() };
}
const hello = { role: "operator", token, client: "race", protocol: Number(process.env.PROTOCOL ?? 4) };

// The watcher applies frames as a page without a fence would: a push updates,
// a snapshot replaces.
const AGENT = "claude-1";
let shown = -1, newestPush = -1, snapshots = 0, stale = 0;
const snapshotIds = new Set();
const watcher = connect((f) => {
  if (f.push?.kind === "agent" && f.push.data.id === AGENT) { shown = newestPush = f.push.data.inbox_total; return; }
  if (f.id !== undefined && snapshotIds.has(f.id)) {
    snapshots++;
    const s = f.result.agents.find((a) => a.id === AGENT).inbox_total;
    if (s < newestPush) stale++;          // the reply is older than a push that came before it
    shown = s;                            // … and replacing shows the older state
  }
});
await watcher.call("hello", hello);
await watcher.call("subscribe");
const start = (await watcher.call("snapshot", { messages: 0, events: 0 })).result.agents.find((a) => a.id === AGENT).inbox_total;

const sender = connect();
await sender.call("hello", hello);
let sending = true;
const snapshotLoop = (async () => {
  while (sending) {
    await watcher.call("snapshot", { messages: 300, events: 1000 }, (id) => snapshotIds.add(id));
  }
})();
for (let i = 0; i < total; i++) await sender.call("send", { to: AGENT, type: "message", body: "race " + i });
sending = false;
await snapshotLoop;
await new Promise((r) => setTimeout(r, 300));
const truth = (await sender.call("snapshot", { messages: 0, events: 0 })).result.agents.find((a) => a.id === AGENT).inbox_total;
console.log(JSON.stringify({ sent: total, inbox_before: start, inbox_now: truth, snapshots, snapshot_older_than_an_earlier_push: stale, shown_at_the_end: shown, stale_at_the_end: shown !== truth }));
watcher.end(); sender.end();
