import type { JSX } from "solid-js";

export type Tone = "neutral" | "info" | "ok" | "warn" | "err";

export function StatusBadge(props: { children: JSX.Element; tone?: Tone }) {
  return <span class={`badge status-badge ${props.tone === "info" ? "orch" : props.tone ?? ""}`}>{props.children}</span>;
}
