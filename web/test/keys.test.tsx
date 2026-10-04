// The key table and its dispatcher (notes/web-design.md §ui "Focus and keys",
// §tests "keys.test.ts"). The browser half (tab order, the graph's roving
// tabindex) is e2e/keyboard.spec.ts.
import { render } from "@solidjs/testing-library";
import { createRoot } from "solid-js";
import { beforeEach, describe, expect, test, vi } from "vitest";
import type { App } from "../src/app";
import { AppContext } from "../src/app";
import { KEYS, handleKey } from "../src/keys";
import { operations } from "../src/operate";
import { createPrefs } from "../src/prefs";
import { decodeSnapshot } from "../src/protocol";
import { createState } from "../src/store/store";
import { createUi } from "../src/ui";
import { HelpDialog } from "../src/views/dialogs";
import { fixture } from "./fixtures";

type Calls = string[];

function fakeApp(calls: Calls) {
  return createRoot(() => {
    const loc = { hash: "#/" };
    const hist = { pushState: (_: unknown, __: string, url?: string) => (loc.hash = url!), replaceState: (_: unknown, __: string, url?: string) => (loc.hash = url!) };
    const ui = createUi(loc, hist);
    const store = createState();
    const snap = decodeSnapshot(fixture("snapshot.json"));
    store.applySnapshot(snap, snap.server_time);
    const prefs = createPrefs(async () => ({ prefs: {} }), { setTimeout: () => 0, clearTimeout: () => {} });
    const record = (name: string) => (...args: unknown[]) => {
      calls.push(`${name}${args.length ? `(${args.map((a) => (typeof a === "object" ? (a as { id?: string }).id : a)).join(",")})` : ""}`);
      return Promise.resolve(undefined);
    };
    const actions = new Proxy({}, { get: (_t, name: string) => record(name) }) as App["actions"];
    const viewport = { fit: vi.fn(), zoom: vi.fn() };
    const app = {
      ui,
      store,
      prefs,
      actions,
      link: { live: () => true, conn: () => "live" },
      views: { graph: viewport, tasks: viewport },
      isPending: () => false,
    } as unknown as App;
    return { app, ops: operations(app), viewport };
  });
}

const press = (app: App, ops: ReturnType<typeof operations>, key: string, init: KeyboardEventInit = {}, target: Element = document.body) => {
  const e = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  Object.defineProperty(e, "target", { value: target });
  handleKey(app, ops, e);
  return e;
};

let calls: Calls;
beforeEach(() => {
  calls = [];
  document.body.innerHTML = "";
});

describe("each key does its action on the selected agent", () => {
  test("views, dialogs, actions", () => {
    const { app, ops, viewport } = fakeApp(calls);
    const { ui } = app;
    press(app, ops, "t");
    expect(ui.view()).toBe("tasks");
    press(app, ops, "t");
    expect(ui.view()).toBe("graph");
    press(app, ops, "l");
    expect(ui.view()).toBe("logs");
    press(app, ops, "l");
    press(app, ops, "v");
    expect(ui.view()).toBe("usage");
    press(app, ops, "v");
    press(app, ops, "?");
    expect(ui.dialog()).toEqual({ kind: "help" });
    ui.closeDialog();
    // Nothing selected yet: the TUI's word for it.
    press(app, ops, "p");
    expect(ui.status()?.text).toBe("no agent selected");
    ui.setSelected("claude-2");
    press(app, ops, "p");
    press(app, ops, "r");
    press(app, ops, "g");
    expect(calls).toEqual(["togglePause(claude-2)", "restart(claude-2)", "focus(claude-2)"]);
    press(app, ops, "k");
    expect(ui.dialog()).toEqual({ kind: "confirm", confirm: { kind: "kill", agent: "claude-2" } });
    ui.closeDialog();
    press(app, ops, "S");
    expect(ui.dialog()).toEqual({ kind: "confirm", confirm: { kind: "stop" } });
    ui.closeDialog();
    press(app, ops, "s");
    expect(ui.dialog()).toEqual({ kind: "model", agent: "claude-2" });
    ui.closeDialog();
    press(app, ops, "O");
    expect(ui.dialog()).toEqual({ kind: "model", agent: "orchestrator" });
    ui.closeDialog();
    press(app, ops, "u");
    expect(ui.status()?.text).toBe("no agent is paused at a usage limit");
    press(app, ops, "U");
    expect(ui.dialog()).toBeNull();
    press(app, ops, "a");
    expect(ui.status()?.text).toBe("no question is waiting for you");
    press(app, ops, "o");
    expect(ui.selected()).toBe("orchestrator");
    press(app, ops, "m");
    expect(ui.compose()).toEqual({ to: "orchestrator", text: "", prompt: false });
    press(app, ops, "M");
    expect(ui.compose().to).toBe("*");
    press(app, ops, "/");
    expect(ui.compose().text).toBe("/");
    press(app, ops, "+");
    press(app, ops, "-");
    press(app, ops, "0");
    expect(viewport.zoom).toHaveBeenCalledTimes(2);
    expect(viewport.fit).toHaveBeenCalledTimes(1);
    press(app, ops, "Enter");
    expect(ui.sel()).toEqual({ kind: "agent", id: "orchestrator" });
    press(app, ops, "d");
    expect(ui.tab()).toBe("diff");
    press(app, ops, "f");
    expect(ui.panelFull()).toBe(true);
    press(app, ops, "i");
    expect(ui.compose()).toEqual({ to: "orchestrator", text: "", prompt: true });
    const tick = ui.refreshTick();
    press(app, ops, "r");
    expect(ui.refreshTick()).toBe(tick + 1);
    press(app, ops, "R");
    expect(calls.at(-1)).toBe("restart(orchestrator)");
    press(app, ops, "Escape");
    expect(ui.sel()).toBeNull();
    press(app, ops, "q");
    expect(calls.at(-1)).toBe("restart(orchestrator)");
  });

  test("u and U with an agent paused at a limit", () => {
    const { app, ops } = fakeApp(calls);
    app.store.applyPush({ kind: "agent", data: { ...JSON.parse(JSON.stringify(app.store.state.agents[0])), limit: { kind: "usage", message: "limit", since: 0, attempts: 0 } } }, 0);
    press(app, ops, "u");
    expect(calls).toEqual(["resumePaused(false)"]);
    press(app, ops, "U");
    expect(app.ui.dialog()).toEqual({ kind: "confirm", confirm: { kind: "restart-paused", count: 1 } });
  });
});

describe("when keys do nothing", () => {
  test("none fires while a text field, a dialog or the terminal has focus", () => {
    const { app, ops } = fakeApp(calls);
    app.ui.setSelected("claude-1");
    const input = document.createElement("textarea");
    document.body.append(input);
    for (const k of ["k", "p", "t", "S"]) press(app, ops, k, {}, input);
    const term = document.createElement("div");
    term.setAttribute("data-terminal", "claude-1");
    const inner = document.createElement("span");
    term.append(inner);
    document.body.append(term);
    for (const k of ["k", "p", "t", "S"]) press(app, ops, k, {}, inner);
    app.ui.openDialog({ kind: "help" });
    for (const k of ["k", "p", "t"]) press(app, ops, k);
    expect(calls).toEqual([]);
    expect(app.ui.view()).toBe("graph");
    expect(app.ui.dialog()).toEqual({ kind: "help" });
  });

  test("after /keys off; ctrl-k still opens the command line", () => {
    const { app, ops } = fakeApp(calls);
    app.prefs.setKeys(false);
    app.ui.setSelected("claude-1");
    for (const k of ["k", "p", "t", "S", "m"]) press(app, ops, k);
    expect(calls).toEqual([]);
    expect(app.ui.dialog()).toBeNull();
    const e = press(app, ops, "k", { ctrlKey: true });
    expect(e.defaultPrevented).toBe(true);
    expect(app.ui.compose().text).toBe("/");
  });

  test("tab is never handled outside a dialog; browser keys are not rebound", () => {
    const { app, ops } = fakeApp(calls);
    for (const [key, init] of [["Tab", {}], ["Tab", { shiftKey: true }], ["w", { ctrlKey: true }], ["t", { ctrlKey: true }], ["r", { ctrlKey: true }], ["F5", {}]] as const) {
      const e = press(app, ops, key, init);
      expect(e.defaultPrevented, key).toBe(false);
    }
    expect(app.ui.view()).toBe("graph");
  });
});

test("the help overlay lists exactly the table", () => {
  const { app } = fakeApp(calls);
  const { container } = render(() => (
    <AppContext.Provider value={app}>
      <HelpDialog />
    </AppContext.Provider>
  ));
  const rows = [...document.querySelectorAll("[data-help-key]")].map((r) => r.querySelector("kbd")!.textContent);
  expect(rows).toEqual(KEYS.map((k) => k.keys));
  expect(container).toBeTruthy();
});
