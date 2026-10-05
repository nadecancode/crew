// A node's context menu (notes/web-design.md §graphs "Interaction"):
// Every agent operation is available from More and the graph's context menu.

import { For, Show, createEffect, onCleanup, onMount } from "solid-js";
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
      if (menu() && el && !el.contains(e.target as Node)) ui.closeMenu(false);
    };
    document.addEventListener("pointerdown", close, true);
    onCleanup(() => document.removeEventListener("pointerdown", close, true));
  });
  const items = () => {
    const m = menu();
    const a = m ? store.agent(m.agent) : undefined;
    if (!a) return [];
    return [
      ["message", () => ops.compose(a.id), "m", ""],
      ["prompt", () => ops.prompt(a.id), "i", ""],
      [a.paused ? "resume" : "pause", () => void actions.togglePause(a), "p", `agent_${a.paused ? "resume" : "pause"}`],
      ["restart", () => void actions.restart(a.id), "r", "agent_restart"],
      ["restart fresh…", () => ui.openDialog({ kind: "confirm", confirm: { kind: "restart-fresh", agent: a.id } }), "", "agent_restart_fresh"],
      ["model and CLI", () => ops.model(a.id), "s", "agent_set_model"],
      ["compact", () => void actions.compact(a.id), "", "agent_compact"],
      ["open terminal", () => ops.open(a.id, "terminal"), "", "local"],
      ...((a.backend ?? store.state.session?.backend) === "herdr" ? [["jump to pane", () => ops.focusPane(a.id), "g", "agent_focus"]] : []),
      ["kill…", () => ui.openDialog({ kind: "confirm", confirm: { kind: "kill", agent: a.id } }), "k", "agent_kill"],
    ] as [string, () => void, string, string][];
  };
  createEffect(() => {
    if (!menu()) return;
    if (!items().length) return ui.closeMenu(false);
    queueMicrotask(() => {
      if (menu() && el?.isConnected && !el.contains(document.activeElement)) el.querySelector<HTMLElement>("button:not([disabled])")?.focus();
    });
  });
  const onKey = (e: KeyboardEvent) => {
    const buttons = [...(el?.querySelectorAll<HTMLButtonElement>("button:not([disabled])") ?? [])];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      ui.closeMenu();
    } else if (e.key === "Tab") ui.closeMenu();
    else if (["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) {
      e.preventDefault();
      buttons[e.key === "Home" ? 0 : e.key === "End" ? buttons.length - 1 : (i + (e.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length]?.focus();
    }
  };
  return (
    <Show when={menu()} keyed>
      {(m) =>
      <div class="context-menu popover" role="menu" aria-label={`actions for ${menu()!.agent}`}
        onFocusOut={(e) => e.relatedTarget && !e.currentTarget.contains(e.relatedTarget as Node) && ui.closeMenu(false)}
        onKeyDown={onKey} ref={(e) => {
          el = e;
          queueMicrotask(() => {
            const r = e.getBoundingClientRect();
            e.style.left = `${Math.max(8, Math.min(m.x, window.innerWidth - r.width - 8))}px`;
            e.style.top = `${Math.max(8, Math.min(m.y, window.innerHeight - r.height - 8))}px`;
            e.querySelector<HTMLElement>("button:not([disabled])")?.focus();
          });
        }}>
        <div class="menu-title">{menu()!.agent}</div>
        <For each={items()}>
          {([label, run, key, method]) => (
            <button role="menuitem" classList={{ danger: method === "agent_kill" }} disabled={method !== "local" && (!link.live() || (!!method && app.isPending(method, m.agent)))} onClick={() => {
              ui.closeMenu();
              run();
            }}>{label}<kbd aria-hidden="true">{key}</kbd></button>
          )}
        </For>
      </div>}
    </Show>
  );
}
