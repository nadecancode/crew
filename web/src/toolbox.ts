// A member's effective toolbox in words (notes/toolboxes.md §Display). The
// daemon supplies every status text; these helpers only arrange it, in the
// same words as the Rust `AgentToolbox` helpers the TUI and native apps use.

import type { AgentToolbox, ToolboxItem } from "./protocol";

export const TOOLBOX_PENDING = "Toolbox changed: restart to apply";
export const TOOLBOX_UNPINNED = "MCP definitions not pinned (proxy pending)";

// The kinds in canonical order; any other kind is grouped last, under "Other".
const KINDS = ["mcp", "skills", "plugins", "subagents", "hooks"];
const HEADINGS = ["MCP servers", "Skills", "Plugins", "Subagents", "Hooks", "Other"];

/** The fingerprint as shown: its first 12 characters. */
export const shortFingerprint = (t: AgentToolbox) => t.fingerprint.slice(0, 12);

/** What follows the fingerprint: ` · 4 items · 1 not available`. */
export function toolboxCounts(t: AgentToolbox): string {
  const n = t.items.length;
  const off = t.items.filter((i) => !["active", "planned", "not_applicable"].includes(i.status.code)).length;
  return ` · ${n} item${n === 1 ? "" : "s"}${off ? ` · ${off} not available` : ""}`;
}

/** `Toolbox 3f9a0c21e7b4 · 4 items · 1 not available`. */
export const toolboxSummary = (t: AgentToolbox) => `Toolbox ${shortFingerprint(t)}${toolboxCounts(t)}`;

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
export const originLabel = (origin: string) =>
  origin === "default" ? "Team default" : origin === "member" ? "Member" : origin.replace(/^bundle:/, "Bundle ");

/** Items grouped by kind in canonical order; groups without entries are left out. */
export function toolboxGroups(t: AgentToolbox): { heading: string; items: ToolboxItem[] }[] {
  // Another kind's -1 becomes 5, "Other".
  const group = (i: ToolboxItem) => (KINDS.indexOf(i.kind) + 6) % 6;
  return HEADINGS.map((heading, k) => ({ heading, items: t.items.filter((i) => group(i) === k) })).filter((g) => g.items.length);
}
