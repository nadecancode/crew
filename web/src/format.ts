// How the TUI words and marks things (crates/crew-tui/src/theme.rs,
// widgets/agent_box.rs, crew_core::util), so both dashboards read the same.

import type { Agent, AgentStatus, RuntimeKind, Task, TaskStatus } from "./protocol";

export const SPINNER = ["◐", "◓", "◑", "◒"];
export const SPINNER_MS = 150;

const RUNTIME_GLYPH: Record<RuntimeKind, string> = { claude: "✱", codex: "❂", pi: "π", shell: "$" };
const RUNTIME_LABEL: Record<RuntimeKind, string> = { claude: "Claude Code", codex: "Codex", pi: "pi", shell: "shell" };
export const runtimeGlyph = (r: RuntimeKind) => RUNTIME_GLYPH[r] ?? "?";
export const runtimeLabel = (r: RuntimeKind) => RUNTIME_LABEL[r] ?? r;

const AGENT_ICON: Record<AgentStatus, string> = {
  working: "◐", done: "✓", waiting: "○", blocked: "⊘", failed: "✗", spawning: "…", idle: "·", stopped: "■",
};
/** Glyph for an agent's state; a working agent's turns (the spinner, in CSS). */
export const agentIcon = (status: AgentStatus, paused: boolean) => (paused ? "‖" : AGENT_ICON[status]);

const TASK_ICON: Record<TaskStatus, string> = {
  queued: "·", ready: "▸", working: "◐", waiting: "○", blocked: "⊘", review: "◈", done: "✓", failed: "✗",
};
export const taskIcon = (status: TaskStatus) => TASK_ICON[status];

/** `4m05s`, as `crew_core::util::fmt_elapsed`. */
export function fmtElapsed(ms: number): string {
  const s = Math.floor(Math.max(0, ms) / 1000);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(s / 3600)}h${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}m`;
}

/** Whitespace and control characters folded into single spaces. */
export function oneLine(s: string): string {
  return s.replace(/[\s\p{Cc}]+/gu, " ").trim();
}

/** `1M`, `200k`: a context window's size. */
export function windowSize(tokens: number): string {
  if (tokens >= 1_000_000) {
    const tenths = Math.floor((tokens + 50_000) / 100_000);
    return tenths % 10 === 0 ? `${tenths / 10}M` : `${Math.floor(tenths / 10)}.${tenths % 10}M`;
  }
  if (tokens >= 1000) return `${Math.floor(tokens / 1000)}k`;
  return String(tokens);
}

export function progressOf(agent: Agent): number | null {
  if (agent.status === "done") return 100;
  return agent.progress != null ? Math.min(agent.progress, 100) : null;
}

/** A provider-blocked turn is an error, even if old activity is still there. */
export function providerError(agent: Agent): string | null {
  if (agent.status !== "blocked" || (agent.block_kind !== "refused" && agent.block_kind !== "provider_error")) return null;
  const text = agent.error?.trim();
  return text || (agent.block_kind === "refused" ? "provider refused the turn" : "provider error");
}

/** ` 51%` when progress is known, the state otherwise. */
export function statusTail(agent: Agent): string {
  if (providerError(agent)) return agent.block_kind === "refused" ? "refused" : "error";
  const p = progressOf(agent);
  if (p !== null) return `${p}%`;
  return agent.paused ? "paused" : agent.status;
}

/** The colour class of an agent's state; at work it shows its runtime's. */
export function stateTone(agent: Agent): string {
  if (providerError(agent)) return "err";
  switch (agent.status) {
    case "working": return `rt-${agent.runtime}`;
    case "done": return "ok";
    case "failed": return "err";
    case "blocked": return "warn";
    case "spawning": return "orch";
    case "stopped": return "dim";
    default: return "muted";
  }
}

export function taskTone(status: TaskStatus): string {
  switch (status) {
    case "working": return "accent";
    case "done": return "ok";
    case "failed": return "err";
    case "blocked": return "warn";
    case "review": return "orch";
    case "ready": return "text";
    default: return "muted";
  }
}

export type WorkOn = { task: Task; done: boolean; elapsed: number | null };

/** The task an agent is on, else the one it finished last (`App::work_of`). */
export function workOf(tasks: readonly Task[], id: string, serverNow: number): WorkOn | null {
  const mine = tasks.filter((t) => t.assigned_agent === id);
  let current: Task | null = null;
  for (const t of mine) if (t.status !== "done" && t.status !== "failed" && t.started_at != null && (!current || t.started_at > current.started_at!)) current = t;
  if (current) return { task: current, done: false, elapsed: Math.max(0, serverNow - current.started_at!) };
  let last: Task | null = null;
  for (const t of mine) if (t.status === "done" && (!last || (t.completed_at ?? 0) > (last.completed_at ?? 0))) last = t;
  return last ? { task: last, done: true, elapsed: last.completed_at != null ? Math.max(0, serverNow - last.completed_at) : null } : null;
}

/** What the first row of a box says: the task, else the role. */
export function workLine(agent: Agent, work: WorkOn | null): { text: string; current: boolean } {
  if (work) return { text: work.task.title, current: !work.done };
  if (agent.task) return { text: agent.task, current: true };
  return { text: agent.role || "—", current: false };
}

export type Doing = { text: string; time: string; tone: "live" | "warn" | "bad" | "quiet" };

/** What the agent is doing right now, and for how long. */
export function doing(agent: Agent, work: WorkOn | null, serverNow: number): Doing {
  const err = providerError(agent);
  if (err) return { text: oneLine(err), time: "", tone: "bad" };
  if (agent.paused) return { text: "paused", time: "", tone: "warn" };
  const activity = oneLine(agent.activity);
  const idle = fmtElapsed(serverNow - agent.last_activity_at);
  switch (agent.status) {
    case "spawning": return { text: "starting…", time: "", tone: "quiet" };
    case "working": return { text: `▸ ${activity || "working"}`, time: work && !work.done && work.elapsed != null ? fmtElapsed(work.elapsed) : "", tone: "live" };
    case "blocked": return { text: activity || agent.block_kind || "blocked", time: "", tone: "warn" };
    case "failed": return { text: agent.error ? oneLine(agent.error) : "failed", time: "", tone: "bad" };
    case "waiting": return { text: "waiting", time: idle, tone: "quiet" };
    case "idle": return { text: "idle", time: idle, tone: "quiet" };
    case "done": return { text: "✓ finished", time: work?.done && work.elapsed != null ? `${fmtElapsed(work.elapsed)} ago` : "", tone: "quiet" };
    default: return { text: "stopped", time: "", tone: "quiet" };
  }
}

/** `temp`, `⑂2`, `+1`: the badge of a worker box. */
export function badge(agent: Agent, hiddenSubagents = 0): string {
  const parts: string[] = [];
  if (agent.temporary) parts.push("temp");
  if (agent.subagents > 0) parts.push(`⑂${agent.subagents}`);
  if (hiddenSubagents > 0) parts.push(`+${hiddenSubagents}`);
  return parts.join(" ");
}

/** A model id without what only lengthens it (`crew_core::types::short_model`). */
export function shortModel(id: string): string {
  let s = id.trim().split("[")[0];
  const slash = s.lastIndexOf("/");
  if (slash >= 0) s = s.slice(slash + 1);
  if (s.startsWith("claude-")) s = s.slice(7);
  const dash = s.lastIndexOf("-");
  if (dash >= 0 && /^\d{8}$/.test(s.slice(dash + 1))) return s.slice(0, dash);
  return s;
}

/** `claude-opus-5-5` is `Opus 5.5` (`crew_core::types::model_display_name`). */
export function modelDisplayName(id: string): string {
  const words: string[] = [];
  for (const part of shortModel(id).split("-").filter(Boolean)) {
    const last = words[words.length - 1];
    if (/^\d+$/.test(part) && last !== undefined && /^[\d.]+$/.test(last)) words[words.length - 1] = `${last}.${part}`;
    else if (/^gpt$/i.test(part)) words.push("GPT");
    else if (/^glm$/i.test(part)) words.push("GLM");
    else if (/^deepseek$/i.test(part)) words.push("DeepSeek");
    else words.push(part[0].toUpperCase() + part.slice(1));
  }
  return words.join(" ");
}

/** The model the agent runs on, by its proper name. */
export function modelLabel(agent: Agent): string | null {
  const name = agent.model_name ?? (agent.running_model ?? agent.model ? modelDisplayName((agent.running_model ?? agent.model)!) : null);
  return name || null;
}

/** `Opus 5.5 · high`, `↓ Opus 5.5` when off the model the member was given. */
export function runningLabel(agent: Agent): string | null {
  const model = modelLabel(agent);
  const effort = agent.effort || null;
  const m = model && agent.fallback ? `↓ ${model}` : model;
  if (m && effort) return `${m} · ${effort}`;
  return m ?? effort;
}

/** `52.3k`: tokens, short. */
export function tokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 100_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** `14:03:07` in the browser's time zone. */
export function clockTime(ms: number): string {
  const d = new Date(ms);
  return [d.getHours(), d.getMinutes(), d.getSeconds()].map((n) => String(n).padStart(2, "0")).join(":");
}

/** An absolute date and time in the browser's locale and time zone. */
export const absoluteTime = (ms: number): string => new Date(ms).toLocaleString();

/** `Mon 14:03` or `14:03` today: when a usage window resets. */
export function resetTime(ms: number, nowMs: number): string {
  const d = new Date(ms);
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  return new Date(nowMs).toDateString() === d.toDateString() ? hm : `${d.toLocaleDateString(undefined, { weekday: "short" })} ${hm}`;
}
