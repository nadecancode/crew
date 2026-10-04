// A port of crates/crew-tui/src/anim.rs (`Anims`), unchanged in meaning: a
// pulse travels along the link from sender to receiver and the link stays lit
// for a while; version control actions (`vcs.*` events) add marks on the
// agents' boxes: a sweep around the border for a finished write, a spinner for
// a running operation, a glance for a read. The rules are notes/jj-design.md
// §anim. Plain TypeScript with the clock passed in; test/anims.test.ts runs
// the cases generated from the Rust implementation.

import { BROADCAST, OPERATOR_ID, ORCHESTRATOR_ID, isP2p, isRead, type Message, type VcsAction, type VcsEvent } from "../protocol";

/** How long the pulse takes to travel the link. */
export const PULSE_MS = 900;
/** How long the link stays highlighted in total. */
export const LINK_MS = 3000;
/** How long the bright segment takes around a box after a write. */
export const MARK_SWEEP_MS = 700;
/** How long a write's label stays on the box. */
export const MARK_MS = 2400;
/** How long the segment of a read takes along the bottom border. */
export const GLANCE_SWEEP_MS = 300;
/** How long a read's label stays. */
export const GLANCE_MS = 900;
/** A running operation whose end never came is dropped after this long. */
export const RUNNING_MAX_MS = 600_000;
export const MAX_LINKS = 48;
export const MAX_MARKS = 48;

/** How a VCS link or mark is coloured: `--ok`, `--err`, `--warn`. */
export type Tone = "ok" | "conflict" | "error";
const toneOf = (outcome: VcsEvent["outcome"]): Tone => (outcome === "conflict" ? "conflict" : outcome === "error" ? "error" : "ok");

/** `p2p`: worker to worker, bypassing the orchestrator; `orchestrator`:
 * traffic to or from the orchestrator (or the operator); `vcs`: a version
 * control operation between two members (or one and the hub). */
export type LinkKind = "p2p" | "orchestrator" | "broadcast" | "vcs";

export type Link = {
  from: string;
  to: string;
  kind: LinkKind;
  /** Set for `vcs` links. */
  tone: Tone | null;
  started: number;
  /** The running operation (`VcsEvent.op`) that keeps this link lit. */
  held: string | null;
  /** The message a message link carries (for hover and click-through). */
  message?: string;
  /** The event a VCS link comes from. */
  event?: VcsEvent;
};

const ease = (t: number) => (t < 0.5 ? 2 * t * t : 1 - (-2 * t + 2) ** 2 / 2);

/** Position of the pulse along the link (0..1, eased) while it travels. A held link has none. */
export function pulse(link: Link, now: number): number | null {
  if (link.held !== null) return null;
  const dt = now - link.started;
  if (dt < 0 || dt >= PULSE_MS) return null;
  return ease(dt / PULSE_MS);
}

export const linkLive = (link: Link, now: number) => link.held !== null || now - link.started < LINK_MS;

export type MarkKind = "running" | "done" | "glance";

/** What a VCS action leaves on one box. */
export type Mark = {
  /** The box it is drawn on (`orchestrator` for the hub). */
  agent: string;
  kind: MarkKind;
  /** For a running mark: the operation it waits for. */
  op: string | null;
  tone: Tone;
  /** Without the spinner a running mark adds. */
  label: string;
  /** When it is drawn from; later than now while the pulse travels. */
  started: number;
  /** The event it shows (for hover and click-through). */
  event?: VcsEvent;
};

const duration = (m: Mark): number | null => (m.kind === "running" ? null : m.kind === "done" ? MARK_MS : GLANCE_MS);
const sweepMs = (m: Mark) => (m.kind === "done" ? MARK_SWEEP_MS : m.kind === "glance" ? GLANCE_SWEEP_MS : 0);

/** Position of the sweep (done: around the border; glance: along the bottom) while it runs, 0..1. */
export function sweep(m: Mark, now: number): number | null {
  const dt = now - m.started;
  const ms = sweepMs(m);
  return dt >= 0 && dt < ms ? dt / ms : null;
}

/** Drawn now: started and not yet over (a running mark: until its end). */
export function markLive(m: Mark, now: number): boolean {
  const d = duration(m);
  return now >= m.started && (d === null || now - m.started < d);
}

/** Not over yet, though it may not have started. */
function pendingOrLive(m: Mark, now: number): boolean {
  const d = duration(m);
  return d === null || now - m.started < d;
}

function expired(m: Mark, now: number): boolean {
  const d = duration(m);
  return d !== null ? now - m.started >= d : now - m.started >= RUNNING_MAX_MS;
}

/** The operator has no box of their own; their traffic enters at the hub. */
export const endpoint = (id: string) => (id === OPERATOR_ID ? ORCHESTRATOR_ID : id);
/** Where a VCS actor is drawn: the integration has no box, it is the hub. */
export const vcsEndpoint = (id: string) => (id === "integration" ? ORCHESTRATOR_ID : endpoint(id));

export function actionName(action: VcsAction): string {
  return action === "cherry_pick" ? "cherry-pick" : action;
}

const short = (commit: string) => [...commit].slice(0, 8).join("");

export class Anims {
  links: Link[] = [];
  marks: Mark[] = [];

  /** Start the animation for a message that just arrived. `workers` lists
   * the worker ids a broadcast fans out to. */
  onMessage(msg: Message, workers: readonly string[], now: number): void {
    const from = endpoint(msg.from);
    if (msg.to === BROADCAST) {
      const kind: LinkKind = from === ORCHESTRATOR_ID ? "orchestrator" : "broadcast";
      for (const worker of workers) if (worker !== from) this.start(from, worker, kind, now, msg.id);
      return;
    }
    const to = endpoint(msg.to);
    if (from === to) return;
    this.start(from, to, isP2p(msg) ? "p2p" : "orchestrator", now, msg.id);
  }

  private start(from: string, to: string, kind: LinkKind, now: number, message?: string): void {
    this.pushLink({ from, to, kind, tone: null, started: now, held: null, message });
  }

  private pushLink(link: Link): void {
    this.links = this.links.filter((l) => !(l.from === link.from && l.to === link.to));
    this.links.push(link);
    if (this.links.length > MAX_LINKS) this.links.splice(0, this.links.length - MAX_LINKS);
  }

  private pushMark(mark: Mark, now: number): void {
    if (mark.kind === "done") {
      // A write's result replaces whatever the box showed.
      this.marks = this.marks.filter((m) => m.agent !== mark.agent || m.kind === "running");
    } else if (mark.kind === "glance") {
      const blocked = this.marks.some((m) => m.agent === mark.agent && m.kind === "done" && pendingOrLive(m, now));
      if (blocked) return;
      this.marks = this.marks.filter((m) => m.agent !== mark.agent || m.kind !== "glance");
    }
    this.marks.push(mark);
    if (this.marks.length > MAX_MARKS) this.marks.splice(0, this.marks.length - MAX_MARKS);
  }

  /** Start, or end, what §anim says for a VCS event. */
  onVcs(event: VcsEvent, now: number): void {
    if (event.outcome === "started") {
      this.vcsStarted(event, now);
      return;
    }
    // The operation is over: its running mark and held link go. The
    // terminal row starts its own link, along the line already lit.
    this.endOp(event.op);
    this.vcsEnded(event, now);
  }

  /** The operation is over: its running mark and held link go. */
  endOp(op: string): void {
    this.marks = this.marks.filter((m) => !(m.kind === "running" && m.op === op));
    this.links = this.links.filter((l) => l.held !== op);
  }

  private vcsStarted(event: VcsEvent, now: number): void {
    const agent = vcsEndpoint(event.agent);
    const peerRaw = event.peer != null ? vcsEndpoint(event.peer) : null;
    const peer = peerRaw !== null && peerRaw !== agent ? peerRaw : null;
    let on: string, label: string;
    switch (event.action) {
      case "workspace":
        on = peer ?? ORCHESTRATOR_ID;
        label = `workspace ${event.agent}`;
        break;
      case "merge":
        on = agent;
        label = event.peer != null ? `merge ${event.peer}` : "merge";
        break;
      case "integrate":
        on = ORCHESTRATOR_ID;
        label = "integrating";
        break;
      default:
        on = agent;
        label = actionName(event.action);
    }
    this.pushMark({ agent: on, kind: "running", op: event.op, tone: "ok", label, started: now, event }, now);
    if (event.action === "merge" && peer !== null) {
      this.pushLink({ from: peer, to: agent, kind: "vcs", tone: "ok", started: now, held: event.op, event });
    }
  }

  private vcsEnded(event: VcsEvent, now: number): void {
    const tone = toneOf(event.outcome);
    const agent = vcsEndpoint(event.agent);
    const peerRaw = event.peer != null ? vcsEndpoint(event.peer) : null;
    const peer = peerRaw !== null && peerRaw !== agent ? peerRaw : null;
    const action = event.action;
    // The box the result shows on, the link before it, and the label.
    let on: string, linkFrom: string | null, label: string | null;
    if (action === "status" || action === "diff") {
      on = agent;
      linkFrom = null;
      label = event.outcome === "error" ? `! ${actionName(action)}` : `· ${actionName(action)}`;
    } else if (action === "workspace" && event.outcome === "error") {
      [on, linkFrom, label] = [peer ?? ORCHESTRATOR_ID, null, `! workspace ${event.agent}`];
    } else if (action === "integrate" && event.outcome === "error") {
      [on, linkFrom, label] = [ORCHESTRATOR_ID, null, "! integrate"];
    } else if (event.outcome === "error") {
      [on, linkFrom, label] = [agent, null, `! ${actionName(action)}`];
    } else if (action === "workspace") {
      [on, linkFrom, label] = [agent, peer, `+ ${event.branch ?? `agents/${event.agent}`}`];
    } else if (action === "commit") {
      [on, linkFrom, label] = [agent, null, event.commit != null ? `◆ ${short(event.commit)}` : "◆ commit"];
    } else if ((action === "cherry_pick" || action === "merge") && event.outcome === "conflict") {
      const n = event.files.length;
      [on, linkFrom, label] = [agent, peer, n === 0 ? "✗ conflict" : n === 1 ? "✗ 1 file" : `✗ ${n} files`];
    } else if (action === "cherry_pick" || action === "merge") {
      [on, linkFrom, label] = [agent, peer, event.commit != null ? `◆ ${short(event.commit)}` : "◆ done"];
    } else if (peer !== null) {
      // One branch of the integration run: the work travels to the hub.
      [on, linkFrom, label] = [ORCHESTRATOR_ID, peer, null];
    } else {
      // The run's tally.
      [on, linkFrom, label] = [ORCHESTRATOR_ID, null, event.summary ?? "integrated"];
    }
    const linked = linkFrom !== null;
    if (linkFrom !== null) this.pushLink({ from: linkFrom, to: on, kind: "vcs", tone, started: now, held: null, event });
    if (label === null) return;
    this.pushMark(
      {
        agent: on,
        kind: isRead(action) ? "glance" : "done",
        op: null,
        tone,
        label,
        // On arrival, when a pulse comes first.
        started: linked ? now + PULSE_MS : now,
        event,
      },
      now,
    );
  }

  /** From a snapshot's events, oldest first: restore the operations that
   * started and have not ended, as running marks and held links. Every VCS
   * link and mark that was there goes first; nothing else starts, since
   * events in a snapshot are history. `age` is how long ago each happened. */
  resume(events: Iterable<readonly [VcsEvent, number]>, now: number): void {
    this.links = this.links.filter((l) => l.kind !== "vcs" && l.held === null);
    this.marks = [];
    let open: (readonly [VcsEvent, number])[] = [];
    for (const [event, age] of events) {
      if (event.outcome !== "started") open = open.filter(([e]) => e.op !== event.op);
      else open.push([event, age]);
    }
    for (const [event, age] of open) if (age < RUNNING_MAX_MS) this.vcsStarted(event, now - Math.max(age, 0));
  }

  /** Drop what expired. Returns true when something was removed. */
  gc(now: number): boolean {
    const [links, marks] = [this.links.length, this.marks.length];
    this.links = this.links.filter((l) => (l.held !== null ? now - l.started < RUNNING_MAX_MS : linkLive(l, now)));
    this.marks = this.marks.filter((m) => !expired(m, now));
    return this.links.length !== links || this.marks.length !== marks;
  }

  /** Something is moving: a pulse, a sweep or a glance. */
  animating(now: number): boolean {
    return this.links.some((l) => pulse(l, now) !== null) || this.marks.some((m) => sweep(m, now) !== null);
  }

  /** The mark a box shows: a write or read while it lives, else the newest operation still running there. */
  markFor(id: string, now: number): Mark | null {
    const mine = this.marks.filter((m) => m.agent === id);
    for (let i = mine.length - 1; i >= 0; i--) if (mine[i].kind !== "running" && markLive(mine[i], now)) return mine[i];
    for (let i = mine.length - 1; i >= 0; i--) if (mine[i].kind === "running") return mine[i];
    return null;
  }

  /** An agent left the team: nothing of it is drawn, so nothing of it runs. */
  removeAgent(id: string): void {
    this.marks = this.marks.filter((m) => m.agent !== id);
    this.links = this.links.filter((l) => !(l.kind === "vcs" && (l.from === id || l.to === id)));
  }

  /** An operation is running: the spinner turns. */
  running(): boolean {
    return this.marks.some((m) => m.kind === "running");
  }

  /** Workers are talking directly right now. */
  p2pLive(now: number): boolean {
    return this.links.some((l) => l.kind === "p2p" && linkLive(l, now));
  }

  /** Milliseconds until the next link or mark changes without an event: a
   * link expires, a mark starts or ends, a running one reaches its cap. */
  nextExpiry(now: number): number | null {
    let min: number | null = null;
    const see = (ms: number) => {
      ms = Math.max(ms, 0);
      if (min === null || ms < min) min = ms;
    };
    for (const l of this.links) see(l.held !== null ? l.started + RUNNING_MAX_MS - now : l.started + LINK_MS - now);
    for (const m of this.marks) {
      if (m.started > now) see(m.started - now);
      else {
        const d = duration(m);
        see(d !== null ? m.started + d - now : m.started + RUNNING_MAX_MS - now);
      }
    }
    return min;
  }
}
