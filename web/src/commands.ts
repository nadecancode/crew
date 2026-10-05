// Slash commands of the message box (notes/web-design.md §ui "Commands"):
// the TUI's `slash::DASHBOARD` with the same names, aliases, arguments,
// completion and hints, and the same rule: a line starting with `/` is never
// sent as a message. `/exit` and `/mouse` have nothing to do in a browser and
// say so; `/usage`, `/graph [reset]` and `/keys on|off` are the browser's own.

import { COMMANDS, type GeneratedCommand } from "./generated/client-policy";

export { COMMANDS };
export type SlashCommand = GeneratedCommand;

/** Why a declined command does nothing here. */
export const DECLINED = "not needed in the browser";

export const usage = (c: SlashCommand) => (c.args ? `/${c.name} ${c.args}` : `/${c.name}`);

/** The slash command a line opens with, as `[name, rest]` (`crew_core::util::slash_command`). */
export function slashCommand(text: string): [string, string] | null {
  if (!text.startsWith("/")) return null;
  const line = text.slice(1);
  const m = /\s/.exec(line);
  const end = m ? m.index : line.length;
  const name = line.slice(0, end), rest = line.slice(end).trim();
  const word = /^[A-Za-z][A-Za-z0-9_-]*$/.test(name);
  return word || name === "?" ? [name, rest] : null;
}

export function find(name: string): SlashCommand | undefined {
  const n = name.toLowerCase();
  return COMMANDS.find((c) => c.name === n || c.aliases.includes(n));
}

function typing(text: string): string | null {
  if (!text.startsWith("/")) return null;
  const word = text.slice(1);
  return /\s/.test(word) ? null : word;
}

const candidates = (word: string) => COMMANDS.filter((c) => c.name.startsWith(word.toLowerCase()));

/** What `tab` turns a partly typed command into. */
export function complete(text: string): string | null {
  const word = typing(text);
  if (word === null) return null;
  const exact = find(word);
  const c = exact && exact.name === word.toLowerCase() ? exact : candidates(word)[0];
  if (!c) return null;
  return c.args ? `/${c.name} ` : `/${c.name}`;
}

export const unknown = (name: string) => `no command /${name} — /help lists them (start with a space to send it as text)`;
export const noArgument = (c: SlashCommand) => `/${c.name} takes no argument`;

/** The line under the box while a command is being typed; null when `text` is not one. */
export function hint(text: string): string | null {
  if (!text.startsWith("/")) return null;
  const describe = (c: SlashCommand) => `${usage(c)}  ${c.about}`;
  const word = typing(text);
  if (word === null) {
    const sc = slashCommand(text);
    if (!sc) return null;
    const c = find(sc[0]);
    return c ? describe(c) : unknown(sc[0]);
  }
  const c = find(word);
  if (c) return describe(c);
  const cs = candidates(word);
  if (cs.length === 0) return unknown(word);
  if (cs.length === 1) return `${describe(cs[0])} · tab completes`;
  return cs.map(usage).join(" · ");
}

/** What running a command asks of the page. */
export type Effect =
  | { kind: "confirm-stop" }
  | { kind: "confirm-kill"; agent: string }
  | { kind: "confirm-restart-fresh"; agent: string }
  | { kind: "restart"; agent: string }
  | { kind: "pause"; agent: string }
  | { kind: "unpause"; agent: string }
  | { kind: "compact"; agent: string }
  | { kind: "model"; agent: string }
  | { kind: "resume" }
  | { kind: "answer" }
  | { kind: "view"; view: "graph" | "tasks" | "logs" | "usage" }
  | { kind: "reset-layout" }
  | { kind: "keys"; on: boolean }
  | { kind: "help" }
  | { kind: "note"; text: string };

export type Context = {
  agents: readonly string[];
  /** Who the box was addressed to (`*` for a broadcast). */
  to: string;
  selected: string | null;
  orchestrator: string | null;
};

/** The agent a command is about: the one named, else the one the box was
 * addressed to, else the selected one. */
function agentOf(c: SlashCommand, rest: string, ctx: Context): string {
  if (!rest) {
    const id = ctx.to && ctx.to !== "*" ? ctx.to : ctx.selected;
    if (!id) throw new Error(`name the agent: ${usage(c)}`);
    return id;
  }
  if (ctx.agents.includes(rest)) return rest;
  throw new Error(`no agent named ${rest} (there are: ${ctx.agents.join(", ")})`);
}

/** Run a command typed into the box: what it asks of the page, or an
 * `Error` saying why nothing was run (the box then stays open to say so). */
export function run(name: string, rest: string, ctx: Context): Effect {
  const c = find(name);
  if (!c) throw new Error(unknown(name));
  if (!c.args && rest) throw new Error(noArgument(c));
  switch (c.name) {
    case "exit":
    case "mouse":
      return { kind: "note", text: `/${c.name}: ${DECLINED}` };
    case "stop":
      return { kind: "confirm-stop" };
    case "kill":
      return { kind: "confirm-kill", agent: agentOf(c, rest, ctx) };
    case "restart":
      return { kind: "restart", agent: agentOf(c, rest, ctx) };
    case "restart-fresh":
      return { kind: "confirm-restart-fresh", agent: agentOf(c, rest, ctx) };
    case "pause":
      return { kind: "pause", agent: agentOf(c, rest, ctx) };
    case "unpause":
      return { kind: "unpause", agent: agentOf(c, rest, ctx) };
    case "compact":
      return { kind: "compact", agent: agentOf(c, rest, ctx) };
    case "model": {
      // Nobody named and nobody selected: the orchestrator.
      try {
        return { kind: "model", agent: agentOf(c, rest, ctx) };
      } catch (e) {
        if (!rest && ctx.orchestrator) return { kind: "model", agent: ctx.orchestrator };
        throw e;
      }
    }
    case "resume":
      return { kind: "resume" };
    case "answer":
      return { kind: "answer" };
    case "tasks":
      return { kind: "view", view: "tasks" };
    case "logs":
      return { kind: "view", view: "logs" };
    case "usage":
      return { kind: "view", view: "usage" };
    case "graph":
      if (rest === "reset") return { kind: "reset-layout" };
      if (rest) throw new Error("/graph takes nothing or reset");
      return { kind: "view", view: "graph" };
    case "keys":
      if (rest === "on" || rest === "off") return { kind: "keys", on: rest === "on" };
      throw new Error("/keys on or /keys off");
    case "help":
      return { kind: "help" };
  }
  throw new Error(unknown(name));
}
