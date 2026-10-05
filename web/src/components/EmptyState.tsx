import { Show, type JSX } from "solid-js";

export function EmptyState(props: { title: string; description?: string; children?: JSX.Element; action?: JSX.Element }) {
  return <div class="empty empty-state"><h3>{props.title}</h3><Show when={props.description}><p>{props.description}</p></Show>{props.children}{props.action}</div>;
}
