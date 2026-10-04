// What the dashboard server may do while nothing happens (notes/web-multi.md §multi-perf P20): wake
// at its 5 s housekeeping tick and when it sends a WebSocket ping, and at no other time. A recorder
// samples the sum of the server threads' voluntary context switches every 20 ms; a wake-up is a run of
// samples in which it moved. The pings are seen directly, at the clients' end: every wake-up must be on
// the tick's lattice (period 5 s, phase found from the data) or at a ping the test's own connections
// received. Anything else is polling, at whatever period; `wakeups.spec.ts` proves that on synthetic
// schedules.

export const SAMPLE_MS = 20;
/** Samples this close are one wake-up. */
export const MERGE_MS = 60;
/** One wake-up lasts no longer than this. */
export const MAX_SPAN_MS = 300;
export const TICK_MS = 5000;
/** How far from its schedule a wake-up may fall and still be the tick's. */
export const TICK_TOLERANCE_MS = 300;
/** A ping is seen at the client some microseconds after the server woke for it; the sampler
 * sees the switch up to one sample late. */
/** tokio's timer wheel keeps a timer further off than 4 s in a slot of 4096 ms and the runtime wakes at the
 * slot's start to move it down (tokio 1.53, time/wheel: level 2). A pending 20 s ping or a 5 s tick is such a timer,
 * so the runtime itself wakes on a lattice of 4096 ms whose phase is the process's own. These wake-ups are
 * counted apart, bounded (one per slot) and reported; polling is not on that lattice except by chance at that very period. */
export const WHEEL_MS = 4096;
export const WHEEL_TOLERANCE_MS = 40;
export const PING_BEFORE_MS = 120;
export const PING_AFTER_MS = 40;

export type Wakeup = { at: number; span: number };

/** Times (ms from the start) at which the counter was seen to have moved, as wake-ups. */
export function cluster(moved: number[]): Wakeup[] {
  const out: Wakeup[] = [];
  for (const t of moved) {
    const last = out[out.length - 1];
    if (last && t - (last.at + last.span) <= MERGE_MS) last.span = t - last.at;
    else out.push({ at: t, span: 0 });
  }
  return out;
}

const near = (value: number, period: number) => {
  const r = ((value % period) + period) % period;
  return Math.min(r, period - r);
};

export type Verdict = { ok: boolean; wakeups: number; ticks: number; pings: number; wheel: number; unexplained: number[]; longest_span_ms: number; why: string[] };

/** `moved`: when the counter was seen to move; `pings`: when the clients received a ping; both in ms
 * from the same origin. */
export function verdict(moved: number[], pings: number[]): Verdict {
  const w = cluster(moved);
  const at = w.map((x) => x.at);
  let best = { phase: 0, hits: -1 };
  for (const candidate of at) {
    const phase = ((candidate % TICK_MS) + TICK_MS) % TICK_MS;
    const hits = at.filter((t) => near(t - phase, TICK_MS) <= TICK_TOLERANCE_MS).length;
    if (hits > best.hits) best = { phase, hits };
  }
  const onTick = (t: number) => best.hits >= 0 && near(t - best.phase, TICK_MS) <= TICK_TOLERANCE_MS;
  const onPing = (t: number) => pings.some((p) => t >= p - PING_BEFORE_MS && t <= p + PING_AFTER_MS);
  const rest = at.filter((t) => !onTick(t) && !onPing(t));
  // The runtime's own: the lattice of 4096 ms, phase from the data.
  let wheel = { phase: 0, hits: -1 };
  for (const candidate of rest) {
    const phase = ((candidate % WHEEL_MS) + WHEEL_MS) % WHEEL_MS;
    const hits = rest.filter((t) => near(t - phase, WHEEL_MS) <= WHEEL_TOLERANCE_MS).length;
    if (hits > wheel.hits) wheel = { phase, hits };
  }
  const onWheel = (t: number) => wheel.hits >= 0 && near(t - wheel.phase, WHEEL_MS) <= WHEEL_TOLERANCE_MS;
  const unexplained = rest.filter((t) => !onWheel(t));
  const longest = Math.max(0, ...w.map((x) => x.span));
  const why: string[] = [];
  if (unexplained.length) why.push(`${unexplained.length} wake-ups are neither the 5 s tick, a ping nor the timer wheel's 4096 ms lattice: at ${unexplained.slice(0, 8).join(", ")} ms`);
  if (longest > MAX_SPAN_MS) why.push(`a wake-up lasted ${longest} ms (limit ${MAX_SPAN_MS})`);
  return { ok: why.length === 0, wakeups: w.length, ticks: Math.max(best.hits, 0), pings: pings.length, wheel: rest.length - unexplained.length, unexplained, longest_span_ms: longest, why };
}
