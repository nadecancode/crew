import type { JSX } from "solid-js";

export type Tone = "neutral" | "info" | "ok" | "warn" | "err";

export function StatusBadge(props: { children: JSX.Element; tone?: Tone }) {
  return <span class={`status-badge ${props.tone === "info" ? "orch" : !props.tone || props.tone === "neutral" ? "muted" : props.tone}`}>{props.children}</span>;
}
