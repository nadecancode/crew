// The TUI's order of agents (`App::sort_agents`): workers in natural id
// order, each followed by its subagents, then the orchestrator. It is the
// order of the ring on the graph and the order the arrow keys walk.

import { ORCHESTRATOR_ID, type Agent } from "../protocol";

const cmp = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

/** `claude-2` before `claude-10`: a trailing number compares as a number. */
export function naturalCmp(a: string, b: string): number {
  const split = (s: string): [string, bigint | null] => {
    const m = /[0-9]*$/.exec(s)!;
    const digits = m[0];
    // u64 in the TUI: a number too long for it compares as none.
    return [s.slice(0, s.length - digits.length), digits && digits.length <= 20 && BigInt(digits) < 2n ** 64n ? BigInt(digits) : null];
  };
  const [ah, an] = split(a);
  const [bh, bn] = split(b);
  return cmp(ah, bh) || (an === bn ? 0 : an === null ? -1 : bn === null ? 1 : an < bn ? -1 : 1) || cmp(a, b);
}

export const isOrchestrator = (a: Pick<Agent, "id" | "orchestrator">) => a.orchestrator || a.id === ORCHESTRATOR_ID;

/** The agent a subagent hangs off: its parent, when that is a known
 * top-level worker. Subagents of the orchestrator (or of an agent that is
 * gone) are shown as ordinary members of the ring. */
export function satelliteParent<A extends Pick<Agent, "id" | "orchestrator" | "parent">>(agents: readonly A[], agent: A): A | null {
  if (isOrchestrator(agent) || !agent.parent) return null;
  const parent = agents.find((a) => a.id === agent.parent);
  return parent && parent.id !== agent.id && !isOrchestrator(parent) && !parent.parent ? parent : null;
}

export function sortAgents<A extends Pick<Agent, "id" | "orchestrator" | "parent">>(agents: readonly A[]): A[] {
  const roots = new Map<string, string>();
  for (const a of agents) {
    const p = satelliteParent(agents, a);
    if (p) roots.set(a.id, p.id);
  }
  return [...agents].sort((a, b) => {
    const ra = roots.get(a.id) ?? a.id;
    const rb = roots.get(b.id) ?? b.id;
    return (
      Number(isOrchestrator(a)) - Number(isOrchestrator(b)) ||
      naturalCmp(ra, rb) ||
      Number(roots.has(a.id)) - Number(roots.has(b.id)) ||
      naturalCmp(a.id, b.id)
    );
  });
}
