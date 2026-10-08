// A member's effective toolbox in words (notes/toolboxes.md §Display). The
// daemon supplies every status text; these helpers only arrange it, in the
// same words as the Rust `AgentToolbox` helpers the TUI and native apps use.

import type { AgentToolbox, ToolboxItem } from "./protocol";

export const TOOLBOX_PENDING = "Toolbox changed: restart to apply";
export const TOOLBOX_UNPINNED = "MCP definitions not pinned (proxy pending)";

const HEADINGS: [string, string][] = [
  ["mcp", "MCP servers"],
  ["skills", "Skills"],
  ["plugins", "Plugins"],
  ["subagents", "Subagents"],
  ["hooks", "Hooks"],
];

/** The fingerprint as shown: its first 12 characters. */
export const shortFingerprint = (t: AgentToolbox) => t.fingerprint.slice(0, 12);

const counted = (code: string) => code !== "active" && code !== "planned" && code !== "not_applicable";

/** `Toolbox 3f9a0c21e7b4 · 4 items · 1 not available`. */
export function toolboxSummary(t: AgentToolbox): string {
  const n = t.items.length;
  const off = t.items.filter((i) => counted(i.status.code)).length;
  return `Toolbox ${shortFingerprint(t)} · ${n} item${n === 1 ? "" : "s"}${off ? ` · ${off} not available` : ""}`;
}

/** The MCP definitions are not pinned yet. */
export const definitionsUnpinned = (t: AgentToolbox) =>
  t.definition_digest == null && t.items.some((i) => i.kind === "mcp");

/** Why the member needs attention: a pending change, or a required item that is not active. */
export function toolboxAttention(t: AgentToolbox | null | undefined): string | null {
  if (!t) return null;
  if (t.pending_fingerprint) return TOOLBOX_PENDING;
  const item = t.items.find((i) => i.required && i.status.code !== "active");
  return item ? `Required ${item.name}: ${item.status.text}` : null;
}

/** `Team default`, `Bundle web`, `Member`. */
export function originLabel(origin: string): string {
  if (origin === "default") return "Team default";
  if (origin === "member") return "Member";
  return origin.startsWith("bundle:") ? `Bundle ${origin.slice("bundle:".length)}` : origin;
}

/** Items grouped by kind in canonical order; groups without entries are left out. */
export function toolboxGroups(t: AgentToolbox): { heading: string; items: ToolboxItem[] }[] {
  const known = new Set(HEADINGS.map(([kind]) => kind));
  const groups = HEADINGS.map(([kind, heading]) => ({ heading, items: t.items.filter((i) => i.kind === kind) }));
  groups.push({ heading: "Other", items: t.items.filter((i) => !known.has(i.kind)) });
  return groups.filter((g) => g.items.length > 0);
}
