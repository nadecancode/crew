import { render } from "solid-js/web";
import { createSignal, batch } from "solid-js";
import "./style.css";
import { Conn } from "./net";
import { applyPush, applySnapshot, state, events } from "./store";
import { Graph } from "./Graph";
import { LogList } from "./LogList";
import { lazy, Show, Suspense } from "solid-js";
const Term = lazy(() => import("./Term"));

// The design's storage: per origin, shared by the origin's tabs.
const KEY = "crew.web.session";
const fromUrl = new URLSearchParams(location.hash.slice(1)).get("k");
if (fromUrl) { localStorage.setItem(KEY, fromUrl); history.replaceState(null, "", location.pathname); }
const token = localStorage.getItem(KEY) ?? "";

const [conn, setConn] = createSignal("idle");
const [open, setOpen] = createSignal<string | null>(null);

// Push-to-frame: from the WebSocket message event to the frame that shows it.
const perf = { toFrame: [] as number[], apply: [] as number[], lag: [] as number[], frames: [] as number[] };
(window as any).__perf = perf;
(window as any).__state = { state, events };

function onPush(p: { kind: string; data?: any }, t0: number) {
  applyPush(p);
  const t1 = performance.now();
  perf.apply.push(t1 - t0);
  if (p.kind === "event" && p.data?.timestamp) perf.lag.push(Date.now() - p.data.timestamp);
  requestAnimationFrame(() => requestAnimationFrame(() => perf.toFrame.push(performance.now() - t0)));
}
const c = new Conn(token, { snapshot: applySnapshot, push: onPush, state: setConn });
(window as any).__conn = c;

// Synthetic load through the same reducers: `rate` pushes a second for `ms`.
(window as any).__bench = (rate: number, ms: number) =>
  new Promise((done) => {
    const ids = state.agents.map((a) => a.id);
    let seq = (events[events.length - 1]?.seq ?? 0) + 1, sent = 0, last = performance.now();
    const start = last; perf.frames = [];
    let prev = last;
    const tickFrame = () => { const n = performance.now(); perf.frames.push(n - prev); prev = n; if (n - start < ms) requestAnimationFrame(tickFrame); };
    requestAnimationFrame(tickFrame);
    const timer = setInterval(() => {
      const n = performance.now(), due = Math.floor(((n - last) * rate) / 1000);
      if (due > 0) last = n;
      for (let i = 0; i < due; i++, sent++) {
        const id = ids[sent % ids.length], t0 = performance.now();
        const kind = sent % 10;
        if (kind === 0) onPush({ kind: "message", data: { id: "m" + seq, from: id, to: ids[(sent + 1) % ids.length], type: "message", body: "x", timestamp: Date.now() } }, t0);
        else if (kind === 1) onPush({ kind: "agent", data: { ...JSON.parse(JSON.stringify(state.agents.find((a) => a.id === id))), activity: "step " + sent, progress: sent % 100 } }, t0);
        else onPush({ kind: "event", data: { seq: seq++, timestamp: Date.now(), source: "bench", event_type: "bench.tick", agent_id: id, message: "synthetic event " + sent, level: "info" } }, t0);
      }
      if (n - start >= ms) { clearInterval(timer); setTimeout(() => done(sent), 100); }
    }, 4);
  });

render(
  () => (
    <>
      <header>
        <b>crew</b> <span>{state.session.project ?? ""}</span> <span id="conn">{conn()}</span> <span>{state.agents.length} agents</span> <span>{open() ?? ""}</span>
      </header>
      <Graph onOpen={setOpen} />
      <Show when={open()} fallback={<LogList />}><Suspense><Term agent={open()!} token={token} /></Suspense></Show>
    </>
  ),
  document.getElementById("app")!,
);
