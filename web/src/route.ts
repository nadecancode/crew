// Routes live in the fragment and are parsed by hand (notes/web-design.md
// §ui): `#/`, `#/tasks`, `#/logs`, `#/usage`, and one selection after the
// route: `?agent=<id>`, `?task=<id>`, `?message=<id>`, `?event=<seq>`.
// `#pair=<code>` is not a route: it is taken out before the router starts.

export type View = "graph" | "tasks" | "logs" | "usage";

export type Selection =
  | { kind: "agent"; id: string }
  | { kind: "task"; id: string }
  | { kind: "message"; id: string }
  | { kind: "event"; seq: number }
  | null;

export type Route = { view: View; sel: Selection };

const PATHS: Record<View, string> = { graph: "/", tasks: "/tasks", logs: "/logs", usage: "/usage" };

export function parseRoute(hash: string): Route {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  const [path, query = ""] = raw.split("?", 2);
  const view = (Object.keys(PATHS) as View[]).find((v) => PATHS[v] === (path || "/")) ?? "graph";
  const q = new URLSearchParams(query);
  let sel: Selection = null;
  const agent = q.get("agent"), task = q.get("task"), message = q.get("message"), event = q.get("event");
  if (agent) sel = { kind: "agent", id: agent };
  else if (task) sel = { kind: "task", id: task };
  else if (message) sel = { kind: "message", id: message };
  else if (event && /^\d+$/.test(event)) sel = { kind: "event", seq: Number(event) };
  return { view, sel };
}

export function formatRoute(route: Route): string {
  const path = PATHS[route.view];
  const s = route.sel;
  if (!s) return `#${path}`;
  const q = s.kind === "event" ? `event=${s.seq}` : `${s.kind}=${encodeURIComponent(s.id)}`;
  return `#${path}?${q}`;
}

export const sameSelection = (a: Selection, b: Selection) =>
  a === b || (!!a && !!b && a.kind === b.kind && (a.kind === "event" ? a.seq === (b as { seq: number }).seq : a.id === (b as { id: string }).id));
