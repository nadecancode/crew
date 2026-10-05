import { Show, type JSX } from "solid-js";

export function ViewHeader(props: { title: string; meta?: JSX.Element; description?: string; actions?: JSX.Element; children?: JSX.Element }) {
  return <header class="view-header"><div><h2>{props.title} <span class="muted">{props.meta}</span></h2><Show when={props.description}><p>{props.description}</p></Show></div><span class="spacer" />{props.actions}{props.children}</header>;
}
