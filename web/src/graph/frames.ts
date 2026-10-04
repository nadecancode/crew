// Frames only while something moves (notes/web-design.md §graphs "Frames",
// §perf 5): a `requestAnimationFrame` loop runs while `anims.animating(now)`,
// writing one time signal the moving elements read; expiries (a link going
// dark, a label leaving, a pending mark starting) are one `setTimeout` at
// `anims.nextExpiry(now)`. An idle page requests no frame. Spinners are CSS.

import { createEffect, createRoot, createSignal, on } from "solid-js";
import { now as clock } from "../clock";
import type { Store } from "../store/store";

/** With `reduced` (prefers-reduced-motion) nothing moves: no frames, only
 * the expiries, so links and labels still light up and go. */
export function createFrames(
  store: Store,
  { reduced = false, raf = (cb: FrameRequestCallback) => requestAnimationFrame(cb), timers = window as { setTimeout(fn: () => void, ms: number): number; clearTimeout(id: number): void } } = {},
) {
  return createRoot(() => {
    const [t, setT] = createSignal(clock());
    let frame = 0;
    let timer: number | null = null;

    function step(): void {
      frame = 0;
      const now = clock();
      if (store.anims.gc(now)) store.bumpAnims();
      setT(now);
      schedule(now);
    }

    function schedule(now: number): void {
      if (!reduced && store.anims.animating(now)) {
        if (!frame) frame = raf(step);
        return;
      }
      if (timer !== null) timers.clearTimeout(timer);
      timer = null;
      const next = store.anims.nextExpiry(now);
      if (next !== null) {
        timer = timers.setTimeout(() => {
          timer = null;
          step();
        }, Math.max(next, 1));
      }
    }

    // Every change to the animation state (a push started a pulse, a
    // snapshot restored a running mark) reconsiders what to schedule.
    createEffect(on(store.animsVersion, () => {
      const now = clock();
      setT(now);
      schedule(now);
    }));

    // A hidden tab gets no frames; on return what expired meanwhile goes.
    if (typeof document !== "undefined") document.addEventListener("visibilitychange", () => document.visibilityState === "visible" && step());

    return { now: t };
  });
}

export type Frames = ReturnType<typeof createFrames>;
