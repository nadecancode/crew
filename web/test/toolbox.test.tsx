// A member's effective toolbox in the web inspector and graph
// (notes/toolboxes.md §Display, case T15). The wire example is
// crates/crew/tests/fixtures/web/toolbox.json, written from the Rust types.
import { fireEvent, render } from "@solidjs/testing-library";
import { createRoot } from "solid-js";
import { describe, expect, test } from "vitest";
import type { App } from "../src/app";
import { AppContext } from "../src/app";
import { DecodeError, decodeAgent, type Agent } from "../src/protocol";
import { TOOLBOX_PENDING, TOOLBOX_UNPINNED, definitionsUnpinned, originLabel, toolboxAttention, toolboxGroups, toolboxSummary } from "../src/toolbox";
import { EntityList } from "../src/graph/EntityList";
import { agentSummary } from "../src/graph/Summary";
import { ToolboxSection } from "../src/views/Inspector";
import { fixture } from "./fixtures";

const agents = (): Agent[] => (fixture("toolbox.json") as unknown[]).map((a, i) => decodeAgent(a, `toolbox[${i}]`));

function fakeApp(calls: string[]): App {
  return createRoot(() => {
    const record = (name: string) => (...args: unknown[]) => (calls.push(`${name}(${args.join(",")})`), Promise.resolve(undefined));
    return {
      actions: new Proxy({}, { get: (_t, name: string) => record(name) }),
      link: { live: () => true },
      isPending: () => false,
    } as unknown as App;
  });
}

describe("toolbox", () => {
  test("toolbox.json decodes, and a toolbox stays optional", () => {
    const [qa, release] = agents();
    expect(qa.toolbox!.items.map((i) => `${i.kind}.${i.name}`)).toEqual(["mcp.context7", "mcp.playwright", "skills.frontend-qa"]);
    expect(release.toolbox!.sources[0].names).toEqual(["ambient", "docs-local"]);
    expect(release.toolbox!.items[0].required).toBe(false);
    const plain = { ...(fixture("toolbox.json") as Record<string, unknown>[])[0] };
    delete plain.toolbox;
    expect(decodeAgent(plain).toolbox).toBeUndefined();
  });

  test("a toolbox without its status words is refused", () => {
    const broken = structuredClone((fixture("toolbox.json") as { toolbox: { items: { status: { text?: string } }[] } }[])[0]);
    delete broken.toolbox.items[0].status.text;
    expect(() => decodeAgent(broken)).toThrow(DecodeError);
  });

  test("the summary, origins and attention use the same words as the Rust helpers", () => {
    const [qa, release] = agents();
    expect(toolboxSummary(qa.toolbox!)).toBe("Toolbox 3f9a0c21e7b4 · 3 items");
    expect(toolboxSummary(release.toolbox!)).toBe("Toolbox b56e01d7c4a9 · 4 items · 2 not available");
    expect(toolboxAttention(qa.toolbox)).toBeNull();
    expect(toolboxAttention(release.toolbox)).toBe(TOOLBOX_PENDING);
    expect(toolboxAttention(null)).toBeNull();
    expect(definitionsUnpinned(qa.toolbox!)).toBe(true);
    expect([originLabel("default"), originLabel("bundle:web"), originLabel("member")]).toEqual(["Team default", "Bundle web", "Member"]);
    expect(toolboxGroups(release.toolbox!).map((g) => g.heading)).toEqual(["MCP servers", "Skills", "Plugins"]);
    const required = structuredClone(qa.toolbox!);
    required.items[1].status = { code: "failed_preflight", text: "Failed preflight" };
    expect(toolboxAttention(required)).toBe("Required playwright: Failed preflight");
  });

  test("the inspector section shows groups with entries, words for every status, sources and Restart", () => {
    const calls: string[] = [];
    const release = agents()[1];
    const { container, getByRole } = render(() => (
      <AppContext.Provider value={fakeApp(calls)}>
        <ToolboxSection agent={release} toolbox={release.toolbox!} />
      </AppContext.Provider>
    ));
    const headings = [...container.querySelectorAll("h3, h4")].map((h) => h.textContent);
    expect(headings).toEqual(["Toolbox", "MCP servers", "Skills", "Plugins", "Sources"]);
    const text = container.textContent!;
    // The summary under the heading does not repeat it to sight; the
    // fingerprint is named for a screen reader.
    const summary = container.querySelector(".toolbox-summary")!;
    expect(summary.textContent).toBe("Toolbox fingerprint b56e01d7c4a9 · 4 items · 2 not available");
    expect(summary.querySelector(".vh")!.textContent).toBe("Toolbox fingerprint ");
    expect(text).toContain("Needs $SHOP_GITHUB_TOKEN");
    expect(text).toContain("Not available on Codex");
    expect(text).toContain("Bundle github");
    expect(text).toContain("Your Codex MCP servers: Disabled (ambient, docs-local)");
    expect(text).toContain("Your subagents: Cannot be excluded on Codex · without the sandbox");
    expect(text).toContain(TOOLBOX_UNPINNED);
    expect(container.querySelector(".fingerprint")!.getAttribute("title")).toBe(release.toolbox!.fingerprint);
    expect(text).toContain(TOOLBOX_PENDING);
    fireEvent.click(getByRole("button", { name: "Restart" }));
    expect(calls).toEqual(["restart(release)"]);
  });

  test("without a pending change or MCP servers there is no notice and no Restart", () => {
    const qa = agents()[0];
    const toolbox = { ...qa.toolbox!, items: qa.toolbox!.items.filter((i) => i.kind !== "mcp") };
    const { container, queryByRole } = render(() => (
      <AppContext.Provider value={fakeApp([])}>
        <ToolboxSection agent={qa} toolbox={toolbox} />
      </AppContext.Provider>
    ));
    expect(queryByRole("button", { name: "Restart" })).toBeNull();
    expect(container.textContent).not.toContain(TOOLBOX_UNPINNED);
    expect([...container.querySelectorAll("h4")].map((h) => h.textContent)).toEqual(["Skills", "Sources"]);
  });

  test("the team List carries the reason in its summary line and the row's description, and only while it applies", () => {
    const [qa, release] = agents();
    const settled = { ...release, toolbox: { ...release.toolbox!, pending_fingerprint: null } };
    const required = structuredClone(qa);
    required.toolbox!.items[1].status = { code: "needs", text: "Needs $PLAYWRIGHT_TOKEN" };
    expect(agentSummary(release, [], 0).detail).toContain(TOOLBOX_PENDING);
    expect(agentSummary(required, [], 0).attention).toBe("Required playwright: Needs $PLAYWRIGHT_TOKEN");
    for (const a of [qa, settled]) {
      expect(agentSummary(a, [], 0).attention).toBeUndefined();
      expect(agentSummary(a, [], 0).detail).not.toContain("Toolbox");
    }
    // jsdom lays nothing out: the list pane is 800 px tall, so both rows render.
    globalThis.ResizeObserver ??= class { observe() {} unobserve() {} disconnect() {} } as unknown as typeof ResizeObserver;
    const height = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
    Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get() { return this.classList.contains("vlist") ? 800 : 0; } });
    const { container } = render(() => (
      <EntityList kind="agent" items={[qa, release]} selected={null} select={() => {}} open={() => {}}
        summary={(a) => agentSummary(a, [], 0)} />
    ));
    const row = (id: string) => container.querySelector(`[data-agent="${id}"]`)!;
    expect(row("release").getAttribute("aria-description")).toBe(TOOLBOX_PENDING);
    expect(row("release").getAttribute("title")).toBe(TOOLBOX_PENDING);
    expect(row("release").querySelector(".summary-detail")!.textContent).toContain(TOOLBOX_PENDING);
    expect(row("qa").hasAttribute("aria-description")).toBe(false);
    if (height) Object.defineProperty(HTMLElement.prototype, "clientHeight", height);
    else delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
  });
});
