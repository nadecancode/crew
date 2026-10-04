// The page over several workspaces (notes/web-multi.md §multi-ui, §multi-tests
// "Frontend"): addresses with `/w/<id>`, which workspace opens first, a
// switch, the switcher's rows, the title and the badges.
import { fireEvent, render } from "@solidjs/testing-library";
import { beforeEach, describe, expect, test } from "vitest";
import { AppContext, createPage, otherCounts, pageTitle } from "../src/app";
import { Session } from "../src/net/session";
import { formatRoute, parseRoute } from "../src/route";
import { SwitcherButton, addCommand, homeDir, shellQuote } from "../src/views/Switcher";
import { FakeSocket, fakeEnv, settle } from "./fakes";
import { fixture } from "./fixtures";
import { A, B, entry, indexSocket, listing, newest, serveIndex, serveWorkspace, stored } from "./multi";

beforeEach(() => {
  FakeSocket.all = [];
  document.body.innerHTML = "";
});

function openPage(hash: string) {
  const loc = { hash };
  const history: string[] = [];
  const hist = {
    pushState: (_: unknown, __: string, url?: string) => (history.push(`push ${url}`), (loc.hash = url!)),
    replaceState: (_: unknown, __: string, url?: string) => (history.push(`replace ${url}`), (loc.hash = url!)),
  };
  const page = createPage(fakeEnv(), new Session(stored(), null), new Promise(() => {}), { loc, hist });
  page.start();
  return { page, loc, history };
}

const summary = (questions: number, attention = 0) => ({ rev: 1, project: null, agents: 4, working: 1, attention, questions, tasks: 9, tasks_done: 1 });

describe("addresses", () => {
  test("a route names its workspace; one without is the same route with none", () => {
    expect(parseRoute(`#/w/${A}/`)).toEqual({ workspace: A, view: "graph", sel: null });
    expect(parseRoute(`#/w/${A}/tasks?task=billing`)).toEqual({ workspace: A, view: "tasks", sel: { kind: "task", id: "billing" } });
    expect(parseRoute(`#/w/${A}/logs?event=12`)).toEqual({ workspace: A, view: "logs", sel: { kind: "event", seq: 12 } });
    expect(parseRoute(`#/w/${A}`)).toEqual({ workspace: A, view: "graph", sel: null });
    expect(parseRoute("#/usage?agent=claude-1")).toEqual({ workspace: null, view: "usage", sel: { kind: "agent", id: "claude-1" } });
    expect(formatRoute({ workspace: A, view: "graph", sel: null })).toBe(`#/w/${A}/`);
    expect(formatRoute({ workspace: A, view: "tasks", sel: { kind: "agent", id: "a b" } })).toBe(`#/w/${A}/tasks?agent=a%20b`);
    expect(formatRoute({ workspace: null, view: "logs", sel: null })).toBe("#/logs");
    for (const hash of [`#/w/${A}/`, `#/w/${B}/usage?message=m-1`, `#/w/${A}/logs?event=3`]) expect(formatRoute(parseRoute(hash))).toBe(hash);
  });

  test("the command that adds a session names its exact directory, quoted for a shell", () => {
    expect(addCommand("/home/op/work/docs")).toBe("crew web -C /home/op/work/docs");
    expect(addCommand("/srv/crew work/billing")).toBe("crew web -C '/srv/crew work/billing'");
    expect(addCommand("/home/op/o'brien")).toBe("crew web -C '/home/op/o'\\''brien'");
    expect(addCommand("/home/another-operator/project")).toBe("crew web -C /home/another-operator/project");
    expect(shellQuote("$(rm -rf ~)")).toBe("'$(rm -rf ~)'");
    expect(shellQuote("~/x")).toBe("'~/x'");
    expect(shellQuote("")).toBe("''");
  });

  test("home is shown as ~", () => {
    expect(homeDir("/home/op/work/payments")).toBe("~/work/payments");
    expect(homeDir("/Users/op/x")).toBe("~/x");
    expect(homeDir("/home/op")).toBe("~");
    expect(homeDir("/srv/home/op/x")).toBe("/srv/home/op/x");
  });
});

describe("which workspace opens first", () => {
  test("the address's, even when another was last; it keeps its view", async () => {
    const { page, loc } = openPage(`#/w/${B}/tasks`);
    await serveIndex(indexSocket(), listing(1, [entry(A), entry(B)]), settle, { prefs: { last: A } });
    expect(page.app().link.id).toBe(B);
    expect(page.app().ui.view()).toBe("tasks");
    expect(loc.hash).toBe(`#/w/${B}/tasks`);
  });

  test("without one in the address: `last`, written into the address in place, the view kept", async () => {
    const { page, loc, history } = openPage("#/logs");
    await serveIndex(indexSocket(), listing(1, [entry(A), entry(B)]), settle, { prefs: { last: B } });
    expect(page.app().link.id).toBe(B);
    expect(page.app().ui.view()).toBe("logs");
    expect(loc.hash).toBe(`#/w/${B}/logs`);
    expect(history).toEqual([`replace #/w/${B}/logs`]);
  });

  test("then the first that is running, then the first joined", async () => {
    let { page } = openPage("#/");
    await serveIndex(indexSocket(), listing(1, [entry(A, { state: "stopped", daemon: null }), entry(B)]), settle, { prefs: { last: "ffffffffffffffff" } });
    expect(page.app().link.id).toBe(B);
    FakeSocket.all = [];
    ({ page } = openPage("#/"));
    await serveIndex(indexSocket(), listing(1, [entry(A, { state: "stopped", daemon: null }), entry(B, { state: "incompatible", daemon: null })]), settle);
    expect(page.app().link.id).toBe(A);
    expect(page.app().link.shown()).toBe("not-running");
  });

  test("an id that is not joined is not fallen through: it says so", async () => {
    const { page } = openPage(`#/w/${B}/`);
    await serveIndex(indexSocket(), listing(1, [entry(A)]), settle);
    expect(page.app().link.id).toBe(B);
    expect(page.app().link.shown()).toBe("removed");
    expect(page.app().link.seen()).toBe(false);
  });

  test("nothing joined: none", async () => {
    const { page } = openPage("#/");
    await serveIndex(indexSocket(), listing(1, []), settle);
    expect(page.app().link.id).toBeNull();
    expect(page.app().link.shown()).toBe("none");
  });
});

describe("a switch", () => {
  test("disposes the workspace left, makes a new store, pushes history, writes last, and back returns with the view it had", async () => {
    const { page, loc, history } = openPage(`#/w/${A}/`);
    await serveIndex(indexSocket(), listing(1, [entry(A), entry(B)]), settle);
    const first = page.app();
    const sock = newest();
    await serveWorkspace(sock, settle);
    first.ui.setView("tasks");
    expect(first.store.state.agents.length).toBeGreaterThan(0);
    page.switchTo(B, { push: true });
    const second = page.app();
    expect(second).not.toBe(first);
    expect(second.store).not.toBe(first.store);
    expect(second.store.state.agents).toEqual([]);
    expect(sock.readyState).toBe(3);
    expect(loc.hash).toBe(`#/w/${B}/`);
    expect(history.at(-1)).toBe(`push #/w/${B}/`);
    expect(page.hub.prefs.prefs.last).toBe(B);
    // The new workspace's connection names it.
    newest().open();
    await settle();
    expect(newest().last("hello").params.workspace).toBe(B);
    // Back: the address names A again.
    loc.hash = `#/w/${A}/tasks`;
    page.fromLocation();
    expect(page.app().link.id).toBe(A);
    expect(page.app().ui.view()).toBe("tasks");
    // A route without a workspace stays on the one on screen.
    loc.hash = "#/logs";
    page.fromLocation();
    expect(page.app().link.id).toBe(A);
    expect(loc.hash).toBe(`#/w/${A}/logs`);
  });

  test("a question appearing in another workspace: one notice with a way there; none for the one on screen", async () => {
    const { page } = openPage(`#/w/${A}/`);
    await serveIndex(indexSocket(), listing(1, [entry(A, { summary: summary(0) }), entry(B, { summary: summary(1) })]), settle);
    expect(page.chrome.notices()).toEqual([]);
    indexSocket().push("workspaces", listing(2, [entry(A, { summary: summary(3) }), entry(B, { summary: summary(2) })]));
    await settle();
    const notices = page.chrome.notices();
    expect(notices.map((n) => n.text)).toEqual(["search: 2 questions are waiting"]);
    notices[0].action!.run();
    expect(page.app().link.id).toBe(B);
    // Fewer questions say nothing.
    indexSocket().push("workspaces", listing(3, [entry(A, { summary: summary(1) }), entry(B, { summary: summary(2) })]));
    await settle();
    expect(page.chrome.notices().length).toBe(1);
  });
});

describe("title and badges", () => {
  test("from the summaries of the others, and the store of the one on screen", async () => {
    const { page } = openPage(`#/w/${A}/`);
    await serveIndex(indexSocket(), listing(1, [entry(A, { summary: summary(5) }), entry(B, { summary: summary(2, 1) }), entry("4c2e8a1b9d0f3e57", { project: "billing", summary: summary(1, 2) })]), settle);
    expect(otherCounts(page)).toEqual({ questions: 3, attention: 3 });
    // Before its snapshot: the summary's count.
    expect(pageTitle(page.app())).toBe("(8) payments · crew");
    await serveWorkspace(newest(), settle);
    // After it: its own open questions (none in the fixture), and its team's name.
    expect(pageTitle(page.app())).toBe(`(3) ${page.app().store.state.session!.project} · crew`);
  });
});

describe("the switcher's rows", () => {
  test("from the listing fixture: state, name, directory as ~, counts, badges, numbers; others with their command", async () => {
    const { page } = openPage("#/w/946d4617aa702870/");
    const l = fixture("results.json").workspaces[0];
    await serveIndex(indexSocket(), l, settle);
    const view = render(() => (
      <AppContext.Provider value={page.app()}>
        <SwitcherButton />
      </AppContext.Provider>
    ));
    const button = view.container.querySelector<HTMLButtonElement>("[data-switcher-button]")!;
    expect(button.textContent).toBe("payments ▾");
    // The others' badges: billing's kept summary counts, the current one's do not.
    expect(view.container.querySelector(".switcher > .badge.q")!.textContent).toBe("? 2");
    fireEvent.click(button);
    const rows = [...view.container.querySelectorAll<HTMLElement>("[role=option]")];
    expect(rows.map((r) => r.dataset.state)).toEqual(["running", "stopped", "incompatible", "unreachable"]);
    expect(rows[0].getAttribute("aria-current")).toBe("true");
    expect(rows[0].textContent).toContain("~/work/payments");
    expect(rows[0].textContent).toContain("6 agents · 3 working");
    expect(rows[0].textContent).toContain("tasks 4/6");
    expect(rows[1].textContent).toContain("billing");
    expect(rows[1].textContent).toContain("stopped");
    expect(rows[1].classList.contains("dimmed")).toBe(true);
    expect(rows[2].textContent).toContain("started by another version of crew (protocol 5): restart the team");
    expect(rows[3].textContent).toContain("its daemon did not answer within 2 s");
    expect(rows.map((r) => r.querySelector(".ws-key")!.textContent)).toEqual(["1", "2", "3", "4"]);
    const other = view.container.querySelector<HTMLElement>("[data-other]")!;
    expect(other.textContent).toContain("crew web -C ~/work/docs");
    expect(other.getAttribute("role")).toBeNull();
    // Typing filters by project and path.
    const list = view.container.querySelector<HTMLElement>("[role=listbox]")!;
    expect(document.activeElement).toBe(list);
    for (const key of "infra") fireEvent.keyDown(list, { key });
    expect([...view.container.querySelectorAll("[role=option]")].map((r) => (r as HTMLElement).dataset.workspace)).toEqual(["7a1d2c3b4e5f6071"]);
    fireEvent.keyDown(list, { key: "Escape" });
    expect(view.container.querySelector("[role=listbox]")).toBeNull();
  });
});
