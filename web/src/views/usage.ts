// Usage figures as the TUI words them (crates/crew-tui/src/views/usage.rs).

import { fmtElapsed } from "../format";
import type { Agent, Quota, RuntimeKind } from "../protocol";

/** A figure nobody has reported for this long is muted. */
export const STALE_MS = 20 * 60_000;
export const RUNTIMES: RuntimeKind[] = ["claude", "codex", "pi"];

/** `session`, `week`, or a model's own weekly window as `Fable week`. */
export function windowLabel(window: string): string {
  if (window === "5h") return "session";
  return window.startsWith("week/") ? `${window.slice(5)} week` : window;
}

/** How long until a reset, short: `45m`, `2h14m`, `1d14h`. */
export function until(ms: number): string {
  const minutes = Math.floor((Math.max(0, ms) + 59_999) / 60_000);
  const hours = Math.floor(minutes / 60);
  const days = Math.floor(hours / 24);
  if (hours === 0) return `${minutes}m`;
  if (days === 0) return `${hours}h${String(minutes % 60).padStart(2, "0")}m`;
  if (days > 99) return "99d+";
  return `${days}d${hours % 24}h`;
}

export const levelTone = (percent: number) => (percent >= 90 ? "err" : percent >= 70 ? "warn" : "gauge");

export type UsageRow = { runtime: RuntimeKind; quotas: Quota[]; paused: Agent[] };

export function usageRows(quotas: readonly Quota[], agents: readonly Agent[]): UsageRow[] {
  return RUNTIMES.map((runtime) => ({
    runtime,
    quotas: quotas.filter((q) => q.runtime === runtime),
    paused: agents.filter((a) => a.runtime === runtime && a.limit),
  })).filter((r) => r.quotas.length > 0 || r.paused.length > 0);
}

/** `⊘ usage limit reached · 2 paused (a, b) · resets in 1h58m · resumes in 1h59m` */
export function pausedText(paused: readonly Agent[], now: number): string {
  const limits = paused.map((a) => a.limit!).filter(Boolean);
  const kind = limits[0]?.kind;
  const what = kind === "credits" ? "out of credits" : kind === "auth" ? "sign-in needed" : "usage limit reached";
  let text = `⊘ ${what} · ${paused.length} paused (${paused.map((a) => a.id).join(", ")})`;
  const resets = limits.map((l) => l.resets_at).filter((x): x is number => x != null);
  if (resets.length) text += ` · resets in ${until(Math.min(...resets) - now)}`;
  const retries = limits.map((l) => l.retry_at).filter((x): x is number => x != null);
  if (retries.length) {
    const at = Math.min(...retries);
    text += at > now ? ` · resumes in ${fmtElapsed(at - now)}` : " · resuming";
  } else text += " · waiting for you";
  return text;
}
