// The entry module. It requests every chunk the page will ever run now, from
// the server that has just served index.html (notes/web-design.md §security
// "No code after the start"): opening a terminal later awaits this promise
// and never calls import() again. After its start the page makes no request
// to its origin except WebSocket upgrades.
import { render } from "solid-js/web";
import "./theme.css";
import { createPage } from "./app";
import { browserEnv } from "./net/conn";
import { Session, takePairCode } from "./net/session";
import { Root } from "./views/Shell";

const terminal = import("./term/Terminal");

// The pairing code leaves the address before anything else runs; what is left
// names the workspace to open first.
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
const page = createPage(browserEnv(), session, terminal);

render(() => <Root page={page} />, document.getElementById("app")!);

page.start();
// A connection that dropped while the tab slept is tried again at once.
document.addEventListener("visibilitychange", () => document.visibilityState === "visible" && page.poke());
window.addEventListener("online", () => page.poke());

// For e2e/perf.spec.ts: timings only, no state and no credential.
Object.defineProperty(window, "crewPerf", { value: { measure: page.measure, log: page.perf }, enumerable: false });
