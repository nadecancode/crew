// The P20 wake-up verdict (wakeups.ts) against synthetic schedules: the permitted ones pass, polling fails.
import { expect, test } from "@playwright/test";
import { verdict } from "./wakeups.ts";

const MS = 60_000;
const CONNS = 6;
/** The ping times of six connections, 20 s apart each, and the wake-ups they and the 5 s tick cause (each 20 ms). */
const permitted = () => {
  const pings: number[] = [];
  for (let c = 0; c < CONNS; c++) for (let at = 3000 + c * 3300; at < MS; at += 20_000) pings.push(at);
  const moved: number[] = [];
  for (let at = 2500; at < MS; at += 5000) moved.push(at, at + 20);
  for (const p of pings) moved.push(p + 1, p + 21);
  return { pings, moved: moved.sort((a, b) => a - b) };
};
const poll = (every: number, from = 100) => {
  const t: number[] = [];
  for (let at = from; at < MS; at += every) t.push(at, at + 20);
  return t;
};

test("the permitted schedule passes", () => {
  const { pings, moved } = permitted();
  const v = verdict(moved, pings);
  expect(v.ok, v.why.join("; ")).toBe(true);
});

test("an idle server with no wake-ups at all passes", () => {
  expect(verdict([], []).ok).toBe(true);
});

for (const every of [50, 100, 500, 1000, 2000, 3000, 4000, 7000, 11_000]) {
  test(`polling every ${every} ms fails, alone and on top of the permitted schedule, whatever its alignment`, () => {
    const { pings, moved } = permitted();
    for (const from of [0, 20, 100, 1000, 2500, 2520, 4999]) {
      expect(verdict(poll(every, from), pings).ok, `alone, from ${from}`).toBe(false);
      const both = [...moved, ...poll(every, from)].sort((a, b) => a - b);
      expect(verdict(both, pings).ok, `on top, from ${from}`).toBe(false);
    }
  });
}

test("a poll at the tick's own period and phase is the tick (slower than that is fine, faster is not)", () => {
  const { pings, moved } = permitted();
  const same = [...moved, ...poll(5000, 2500)].sort((a, b) => a - b);
  expect(verdict(same, pings).ok).toBe(true);
});
