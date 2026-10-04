// Slash commands (notes/web-design.md §ui "Commands", §tests "commands.test.ts").
import { expect, test } from "vitest";
import { COMMANDS, DECLINED, complete, find, hint, run, slashCommand } from "../src/commands";
import { fixture } from "./fixtures";

test("every command of commands.json is implemented or declined by name, with the TUI's aliases and arguments", () => {
  const tui = fixture<{ name: string; aliases: string[]; args: string; about: string }[]>("commands.json");
  for (const c of tui) {
    const web = find(c.name);
    expect(web, c.name).toBeDefined();
    expect(web!.aliases).toEqual(c.aliases);
    expect(web!.args).toBe(c.args);
    expect(web!.about).toBe(c.about);
    const ctx = { agents: ["claude-1"], to: "claude-1", selected: "claude-1", orchestrator: "orchestrator" };
    const effect = run(c.name, "", ctx);
    if (web!.web === "declined") expect(effect).toEqual({ kind: "note", text: `/${c.name}: ${DECLINED}` });
    else expect(effect.kind).not.toBe("note");
  }
  // The browser's own are marked so.
  for (const c of COMMANDS.filter((c) => !tui.some((t) => t.name === c.name))) expect(c.web, c.name).toBe("new");
});

test("a line is a command only when it starts with a slash and a word", () => {
  expect(slashCommand("/exit")).toEqual(["exit", ""]);
  expect(slashCommand("/kill  claude-2 \n")).toEqual(["kill", "claude-2"]);
  expect(slashCommand("/?")).toEqual(["?", ""]);
  for (const text of ["", "/", "/ exit", " /exit", "//exit", "/src/main.rs is slow", "/2fast", "/exit.", "exit"]) expect(slashCommand(text), text).toBeNull();
});

test("tab completes and the hint follows the typing, as slash.rs", () => {
  expect(complete("/st")).toBe("/stop");
  expect(complete("/k")).toBe("/kill ");
  expect(complete("/re")).toBe("/restart ");
  expect(complete("/resume")).toBe("/resume");
  expect(complete("/")).toBe("/exit");
  expect(complete("/zz")).toBeNull();
  expect(complete("/kill cl")).toBeNull();
  expect(complete("hello")).toBeNull();
  expect(hint("hello")).toBeNull();
  expect(hint(" /exit")).toBeNull();
  expect(hint("/")!.startsWith("/exit · /stop · /kill [agent]")).toBe(true);
  expect(hint("/re")).toBe("/restart [agent] · /restart-fresh [agent] · /resume");
  expect(hint("/sto")).toBe("/stop  stop the team: every agent and the daemon (asks first) · tab completes");
  expect(hint("/quit")).toBe("/exit  leave the dashboard — the team keeps running");
  expect(hint("/kill claude-2")).toBe("/kill [agent]  stop one agent's process (asks first)");
  expect(hint("/clear")!.startsWith("no command /clear")).toBe(true);
});

test("the agent a command is about: named, else addressed, else selected", () => {
  const ctx = { agents: ["claude-1", "claude-2"], to: "*", selected: "claude-2", orchestrator: "orchestrator" };
  expect(run("kill", "claude-1", ctx)).toEqual({ kind: "confirm-kill", agent: "claude-1" });
  expect(run("kill", "", { ...ctx, to: "claude-1" })).toEqual({ kind: "confirm-kill", agent: "claude-1" });
  expect(run("kill", "", ctx)).toEqual({ kind: "confirm-kill", agent: "claude-2" });
  expect(() => run("kill", "", { ...ctx, selected: null })).toThrow("name the agent: /kill [agent]");
  expect(() => run("kill", "nobody", ctx)).toThrow("no agent named nobody (there are: claude-1, claude-2)");
  expect(run("model", "", { ...ctx, selected: null })).toEqual({ kind: "model", agent: "orchestrator" });
  expect(() => run("tasks", "now", ctx)).toThrow("/tasks takes no argument");
  expect(() => run("frobnicate", "", ctx)).toThrow(/^no command \/frobnicate/);
  expect(run("graph", "reset", ctx)).toEqual({ kind: "reset-layout" });
  expect(run("keys", "off", ctx)).toEqual({ kind: "keys", on: false });
  expect(() => run("keys", "maybe", ctx)).toThrow();
});
