// What a pane that fetches from the daemon says while it waits.

import type { WorkspaceLink as Link } from "../net/workspace";

/** What a pane that fetches says while the page cannot ask: `reading` while
 * it is live and waiting for the answer. */
export function waiting(link: Link, reading: string): string {
  if (link.live()) return reading;
  const c = link.conn();
  if (c === "stopped" || c === "signed-out") return "not connected: this page is stopped and fetches nothing";
  return c === "live" ? "not connected: it is fetched when the workspace is live" : "not connected: it is fetched when the dashboard is live";
}
