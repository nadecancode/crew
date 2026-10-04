// What keys, commands, buttons and menus do: one implementation each, worded
// as the TUI words it (crates/crew-tui/src/input/navigation.rs).

import type { App } from "./app";
import { run, slashCommand, type Effect } from "./commands";
import { BROADCAST } from "./protocol";
import { isOrchestrator } from "./store/order";

export function operations(app: App) {
  const { store, ui, actions, prefs } = app;
  const selectedAgent = () => {
    const id = ui.selected();
    return id ? store.agent(id) : undefined;
  };
  const noSelection = () => ui.setStatus("no agent selected", true);
  const focusBox = () => queueMicrotask(() => (document.getElementById("compose-text") as HTMLTextAreaElement | null)?.focus());

  const ops = {
    selectedAgent,

    /** The message box, addressed to `to`, else the selected agent, else everyone. */
    compose(to?: string, text = "") {
      const target = to ?? ui.selected() ?? BROADCAST;
      ui.setCompose({ to: target, text, prompt: false });
      focusBox();
    },
    /** The message box as a command line: it opens with the slash typed. */
    command() {
      ui.setCompose({ to: ui.selected() ?? BROADCAST, text: "/", prompt: false });
      focusBox();
    },
    /** The inspector's prompt box: what is typed is submitted to the agent as one prompt. */
    prompt(id: string) {
      ui.setCompose({ to: id, text: "", prompt: true });
      focusBox();
    },
    /** The answer box on the oldest question waiting for the operator. */
    questions() {
      const q = store.openQuestions()[0];
      if (q) ui.openDialog({ kind: "question", id: q.id });
      else ui.setStatus("no question is waiting for you");
    },
    togglePause() {
      const a = selectedAgent();
      if (!a) return noSelection();
      void actions.togglePause(a);
    },
    restart() {
      const id = ui.selected();
      if (!id) return noSelection();
      void actions.restart(id);
    },
    kill() {
      const id = ui.selected();
      if (!id) return noSelection();
      ui.openDialog({ kind: "confirm", confirm: { kind: "kill", agent: id } });
    },
    focusPane(id = ui.selected()) {
      if (!id) return noSelection();
      // Each agent runs on a backend of its own, as the inspector's button checks.
      const backend = store.agent(id)?.backend ?? store.state.session?.backend;
      if (backend !== "herdr") {
        ui.setStatus(`g jumps to a herdr pane; ${id} runs on the ${backend || "pty"} backend: the terminal tab shows it`, true);
        return;
      }
      void actions.focus(id);
    },
    model(id?: string | null) {
      const target = id ?? ui.selected() ?? store.orchestrator()?.id;
      if (!target) return;
      ui.openDialog({ kind: "model", agent: target });
    },
    orchestratorModel() {
      const o = store.orchestrator();
      if (!o) return ui.setStatus("this team runs without an orchestrator agent", true);
      ui.openDialog({ kind: "model", agent: o.id });
    },
    selectOrchestrator() {
      const o = store.orchestrator();
      if (!o) return ui.setStatus("this team runs without an orchestrator agent", true);
      ui.setSelected(o.id);
      if (ui.sel()?.kind === "agent") ui.select({ kind: "agent", id: o.id });
      queueMicrotask(() => (document.querySelector(`[data-agent="${CSS.escape(o.id)}"]`) as HTMLElement | null)?.focus());
    },
    resumePaused() {
      if (store.state.agents.some((a) => a.limit)) void actions.resumePaused(false);
      else ui.setStatus("no agent is paused at a usage limit", true);
    },
    restartPaused() {
      const n = store.state.agents.filter((a) => a.limit).length;
      if (n === 0) ui.setStatus("no agent is paused at a usage limit", true);
      else ui.openDialog({ kind: "confirm", confirm: { kind: "restart-paused", count: n } });
    },
    stop() {
      ui.openDialog({ kind: "confirm", confirm: { kind: "stop" } });
    },
    open(id: string, tab: "overview" | "terminal" | "transcript" | "diff" = "overview") {
      ui.setSelected(id);
      ui.select({ kind: "agent", id });
      ui.setTab(tab);
    },
    toggleView(view: "tasks" | "logs" | "usage") {
      ui.setView(ui.view() === view ? "graph" : view);
    },

    /** Do what a command asks. */
    effect(e: Effect) {
      switch (e.kind) {
        case "confirm-stop":
          return ops.stop();
        case "confirm-kill":
          return ui.openDialog({ kind: "confirm", confirm: { kind: "kill", agent: e.agent } });
        case "confirm-restart-fresh":
          return ui.openDialog({ kind: "confirm", confirm: { kind: "restart-fresh", agent: e.agent } });
        case "restart":
          return void actions.restart(e.agent);
        case "pause":
          return void actions.pause(e.agent);
        case "unpause":
          return void actions.resume(e.agent);
        case "compact":
          return void actions.compact(e.agent);
        case "model":
          return ui.openDialog({ kind: "model", agent: e.agent });
        case "resume":
          return ops.resumePaused();
        case "answer":
          return ops.questions();
        case "view":
          return ui.setView(e.view);
        case "reset-layout":
          prefs.resetLayout();
          app.views.graph?.fit();
          ui.setView("graph");
          return ui.setStatus("the graph's layout was reset");
        case "keys":
          prefs.setKeys(e.on);
          return ui.setStatus(e.on ? "single-key shortcuts on" : "single-key shortcuts off: /keys on brings them back; ctrl-k opens the command line");
        case "help":
          return ui.openDialog({ kind: "help" });
        case "note":
          return ui.setStatus(e.text);
      }
    },

    /** The message box was submitted. Returns what to leave in the box: the
     * text again when nothing was sent (a failure, a refused command). */
    async submit(to: string, text: string, prompt: boolean): Promise<{ keep: boolean; hint?: string }> {
      if (prompt) {
        await actions.prompt(to, text);
        return { keep: false };
      }
      const sc = slashCommand(text);
      if (sc) {
        try {
          const effect = run(sc[0], sc[1], {
            agents: store.state.agents.map((a) => a.id),
            to,
            selected: ui.selected(),
            orchestrator: store.state.agents.find(isOrchestrator)?.id ?? null,
          });
          // `/exit` and `/mouse` answer in the hint line.
          if (effect.kind === "note") return { keep: false, hint: effect.text };
          ops.effect(effect);
          return { keep: false };
        } catch (err) {
          return { keep: true, hint: err instanceof Error ? err.message : String(err) };
        }
      }
      // A space first sends a line that looks like a command as text.
      const body = text.startsWith(" /") ? text.slice(1) : text;
      if (!body.trim()) return { keep: true };
      const sent = await actions.send(to, body);
      return { keep: !sent };
    },
  };
  return ops;
}

export type Operations = ReturnType<typeof operations>;
