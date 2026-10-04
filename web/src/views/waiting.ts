// What a pane that fetches from the daemon says while it waits.

import type { Link } from "../net/state";

/** What a pane that fetches says while the page cannot ask: `reading` while
 * it is live and waiting for the answer. */
export function waiting(link: Link, reading: string): string {
  if (link.live()) return reading;
  const c = link.conn();
  return c === "stopped" || c === "signed-out" ? "not connected: this page is stopped and fetches nothing" : "not connected: it is fetched when the dashboard is live";
}
