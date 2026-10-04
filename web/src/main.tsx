// The entry module. It requests every chunk the page will ever run now, from
// the server that has just served index.html (notes/web-design.md §security
// "No code after the start"): opening a terminal later awaits this promise
// and never calls import() again. After its start the page makes no request
// to its origin except WebSocket upgrades.
import { render } from "solid-js/web";
import "./theme.css";
import { AppContext, createApp } from "./app";
import { browserEnv } from "./net/conn";
import { Session, takePairCode } from "./net/session";
import { Shell } from "./views/Shell";

const terminal = import("./term/Terminal");

// The pairing code leaves the address before anything else runs.
const code = takePairCode(location, history);

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

// The session is read from storage once, here, at the document's start.
const session = new Session(storage(), code);
const app = createApp(browserEnv(), session, terminal);

render(
  () => (
    <AppContext.Provider value={app}>
      <Shell />
    </AppContext.Provider>
  ),
  document.getElementById("app")!,
);

app.link.start();
// A connection that dropped while the tab slept is tried again at once.
document.addEventListener("visibilitychange", () => document.visibilityState === "visible" && app.link.poke());
window.addEventListener("online", () => app.link.poke());

// For e2e/perf.spec.ts: timings only, no state and no credential.
Object.defineProperty(window, "crewPerf", { value: { measure: app.measure, log: app.perf }, enumerable: false });
