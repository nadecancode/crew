import { createSignal, type JSX } from "solid-js";
import { ViewHeader } from "../components";

/** Choices last for the document, including leaving and returning to a view. */
const initial: "graph" | "list" = typeof matchMedia === "function" && matchMedia("(max-width: 639px)").matches ? "list" : "graph";
const modes = { team: createSignal(initial), tasks: createSignal(initial) };
export const presentation = (view: keyof typeof modes = "team") => modes[view];

export function Presentation(props: { title: string; meta: JSX.Element; mode: "graph" | "list"; change(mode: "graph" | "list"): void; children?: JSX.Element }) {
  return <ViewHeader title={props.title} meta={props.meta} actions={
    <div class="presentation" role="group" aria-label={`${props.title} presentation`}>
      <button aria-pressed={props.mode === "graph"} onClick={() => props.change("graph")}>Graph</button>
      <button aria-pressed={props.mode === "list"} onClick={() => props.change("list")}>List</button>
    </div>
  }>{props.children}</ViewHeader>;
}
