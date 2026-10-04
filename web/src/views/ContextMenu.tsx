// A node's context menu (notes/web-design.md §graphs "Interaction"):
// message, pause/resume, restart, model, compact, kill.

import { For, Show, onCleanup, onMount } from "solid-js";
import { useApp } from "../app";
import { useOps } from "./ops";

export function ContextMenu() {
  const app = useApp();
  const ops = useOps();
  const { ui, store, link, actions } = app;
  const menu = () => ui.menu();
  let el: HTMLDivElement | undefined;
  onMount(() => {
    const close = (e: PointerEvent) => {
      if (menu() && el && !el.contains(e.target as Node)) ui.setMenu(null);
    };
    document.addEventListener("pointerdown", close, true);
    onCleanup(() => document.removeEventListener("pointerdown", close, true));
  });
  const items = () => {
    const m = menu();
    const a = m ? store.agent(m.agent) : undefined;
    if (!a) return [];
    return [
      ["message", () => ops.compose(a.id)],
      [a.paused ? "resume" : "pause", () => void actions.togglePause(a)],
      ["restart", () => void actions.restart(a.id)],
      ["model and CLI", () => ops.model(a.id)],
      ["compact", () => void actions.compact(a.id)],
      ["open terminal", () => ops.open(a.id, "terminal")],
      ["kill…", () => ui.openDialog({ kind: "confirm", confirm: { kind: "kill", agent: a.id } })],
    ] as [string, () => void][];
  };
  const onKey = (e: KeyboardEvent) => {
    const buttons = [...(el?.querySelectorAll<HTMLButtonElement>("button:not([disabled])") ?? [])];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      const id = menu()?.agent;
      ui.setMenu(null);
      if (id) document.querySelector<HTMLElement>(`[data-agent="${CSS.escape(id)}"]`)?.focus();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      buttons[(i + (e.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length]?.focus();
    }
  };
  return (
    <Show when={menu() && items().length}>
      <div class="context-menu" role="menu" aria-label={`actions for ${menu()!.agent}`}
        style={{ left: `${Math.min(menu()!.x, window.innerWidth - 200)}px`, top: `${Math.min(menu()!.y, window.innerHeight - 260)}px` }}
        onKeyDown={onKey} ref={(e) => ((el = e), queueMicrotask(() => e.querySelector<HTMLElement>("button")?.focus()))}>
        <div class="menu-title">{menu()!.agent}</div>
        <For each={items()}>
          {([label, run]) => (
            <button role="menuitem" disabled={!link.live() && label !== "open terminal"} onClick={() => {
              ui.setMenu(null);
              run();
            }}>{label}</button>
          )}
        </For>
      </div>
    </Show>
  );
}
