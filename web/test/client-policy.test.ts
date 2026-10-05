// The generated production imports must preserve the existing browser policy.
import { expect, test } from "vitest";
import { COMMANDS, find, run, type SlashCommand } from "../src/commands";
import { timeoutFor } from "../src/net/conn";
import { fixture } from "./fixtures";

test("generated timeouts preserve every fixture method and the unknown-method fallback", () => {
  const oldLifecycle = new Set(["agent_kill", "agent_restart", "agent_restart_fresh", "agent_resume", "agent_set_model"]);
  const oldLong = new Set(["resume", "shutdown"]);
  const results = fixture<Record<string, unknown[]>>("results.json");
  const methods = new Set([...Object.keys(results), ...oldLifecycle, ...oldLong, "models_list"]);
  for (const method of methods) {
    const expected = oldLifecycle.has(method) ? 30_000 : oldLong.has(method) ? 90_000 : method === "models_list" ? 20_000 : 3_000;
    expect(timeoutFor(method), method).toBe(expected);
  }
  for (const method of ["", "unknown", "__proto__", "constructor", "toString", "AGENT_KILL", "hello", "web_pair"]) {
    expect(timeoutFor(method), method).toBe(3_000);
  }
});

test("generated command catalog preserves full order, annotations, aliases and usage metadata", () => {
  const tui = fixture<Omit<SlashCommand, "web">[]>("commands.json");
  const expected: SlashCommand[] = [
    ...tui.map((command) => ({ ...command, web: ["exit", "mouse"].includes(command.name) ? "declined" as const : undefined })),
    { name: "usage", about: "show usage: context windows, tokens, quotas", args: "", aliases: [], web: "new" },
    { name: "graph", about: "show the agent graph; reset forgets dragged positions", args: "[reset]", aliases: [], web: "new" },
    { name: "keys", about: "turn single-key shortcuts on or off", args: "on|off", aliases: [], web: "new" },
  ];
  expect(COMMANDS).toEqual(expected);
  for (const command of expected) {
    for (const name of [command.name, ...command.aliases]) {
      expect(find(name)).toEqual(command);
      expect(find(name.toUpperCase())).toEqual(command);
    }
  }
});

test("catalog reuse preserves current dispatch and unknown-command refusal", () => {
  const context = { agents: ["claude-1", "claude-2"], to: "*", selected: "claude-1", orchestrator: "claude-2" };
  for (const command of COMMANDS) {
    const argument = command.name === "keys" ? "on" : "";
    const expected = run(command.name, argument, context);
    for (const alias of command.aliases) expect(run(alias, argument, context)).toEqual(expected);
  }
  expect(() => run("unknown-command", "claude-2", context)).toThrow();
  expect(run("kill", "claude-2", context)).toEqual({ kind: "confirm-kill", agent: "claude-2" });
  expect(run("restart-fresh", "claude-2", context)).toEqual({ kind: "confirm-restart-fresh", agent: "claude-2" });
  expect(run("stop", "", context)).toEqual({ kind: "confirm-stop" });
  expect(run("unpause", "claude-2", context)).toEqual({ kind: "unpause", agent: "claude-2" });
});
