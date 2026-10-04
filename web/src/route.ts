// Routes live in the fragment and are parsed by hand (notes/web-design.md
// §ui, notes/web-multi.md §multi-ui "Addresses"): `#/w/<id>/`,
// `#/w/<id>/tasks`, `#/w/<id>/logs`, `#/w/<id>/usage`, and one selection after
// the route: `?agent=<id>`, `?task=<id>`, `?message=<id>`, `?event=<seq>`. A
// route without `/w/<id>` is rewritten to the chosen workspace's.
// `#pair=<code>` is not a route: it is taken out before the router starts.

export type View = "graph" | "tasks" | "logs" | "usage";

export type Selection =
  | { kind: "agent"; id: string }
  | { kind: "task"; id: string }
  | { kind: "message"; id: string }
  | { kind: "event"; seq: number }
  | null;

/** `workspace`: the id in the address, or null when it names none. */
export type Route = { workspace: string | null; view: View; sel: Selection };

/** A workspace's id: the 16 hex digits that name its daemon's socket. */
export const WORKSPACE_ID = /^[0-9a-f]{16}$/;

const PATHS: Record<View, string> = { graph: "/", tasks: "/tasks", logs: "/logs", usage: "/usage" };

export function parseRoute(hash: string): Route {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  let [path, query = ""] = raw.split("?", 2);
  let workspace: string | null = null;
  const w = /^\/w\/([^/]*)(\/.*)?$/.exec(path);
  if (w) {
    // Kept as written, so that the page can say it is not joined; it is only
    // ever sent once a listing names it.
    workspace = w[1].slice(0, 64) || null;
    path = w[2] ?? "/";
  }
  const view = (Object.keys(PATHS) as View[]).find((v) => PATHS[v] === (path || "/")) ?? "graph";
  const q = new URLSearchParams(query);
  let sel: Selection = null;
  const agent = q.get("agent"), task = q.get("task"), message = q.get("message"), event = q.get("event");
  if (agent) sel = { kind: "agent", id: agent };
  else if (task) sel = { kind: "task", id: task };
  else if (message) sel = { kind: "message", id: message };
  else if (event && /^\d+$/.test(event)) sel = { kind: "event", seq: Number(event) };
  return { workspace, view, sel };
}

export function formatRoute(route: Route): string {
  const path = route.workspace === null ? PATHS[route.view] : `/w/${encodeURIComponent(route.workspace)}${PATHS[route.view]}`;
  const s = route.sel;
  if (!s) return `#${path}`;
  const q = s.kind === "event" ? `event=${s.seq}` : `${s.kind}=${encodeURIComponent(s.id)}`;
  return `#${path}?${q}`;
}

export const sameSelection = (a: Selection, b: Selection) =>
  a === b || (!!a && !!b && a.kind === b.kind && (a.kind === "event" ? a.seq === (b as { seq: number }).seq : a.id === (b as { id: string }).id));
