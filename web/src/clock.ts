// Time. `now()` is milliseconds since the epoch with sub-millisecond
// resolution, the clock animations and elapsed times run on.

import { createSignal, onCleanup } from "solid-js";

export const now = (): number => performance.timeOrigin + performance.now();

// One second clock for elapsed times (§perf 8): a single interval, running
// only while the tab is visible and something on screen shows a duration.
const [second, setSecond] = createSignal(Math.floor(now() / 1000));
let users = 0;
let timer: number | null = null;

function run(): void {
  const want = users > 0 && document.visibilityState === "visible";
  if (want && timer === null) {
    setSecond(Math.floor(now() / 1000));
    timer = window.setInterval(() => setSecond(Math.floor(now() / 1000)), 1000);
  } else if (!want && timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

if (typeof document !== "undefined") document.addEventListener("visibilitychange", run);

/** The current second, for a component that shows a duration; the clock
 * runs while at least one such component is mounted. */
export function useSecond(): () => number {
  users++;
  run();
  onCleanup(() => {
    users--;
    run();
  });
  return second;
}

