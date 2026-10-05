import { expect, test } from "vitest";
import type { Agent } from "../src/protocol";
import { reportedTokens, usageTotals } from "../src/views/usage";
import { fixture } from "./fixtures";

const base = fixture<{ agents: Agent[] }>("snapshot.json").agents[0];
const usage = { input_tokens: 10, output_tokens: 20, cache_read_tokens: 30, cache_creation_tokens: 40, turns: 2 };
const zero = { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_creation_tokens: 0, turns: 0 };
const limit = { kind: "usage", message: "Usage window exhausted", since: 1, attempts: 1 };

test("usage includes all categories and counts explicit zero reports separately from missing data", () => {
  const agents: Agent[] = [
    { ...base, id: "reported", usage, paused: true, limit },
    { ...base, id: "zero", usage: zero, paused: false, limit },
    { ...base, id: "missing", usage: null, paused: true, limit: null },
    { ...base, id: "absent", usage: undefined, paused: false, limit: null },
  ];
  expect(reportedTokens(usage)).toBe(100);
  expect(usageTotals(agents)).toEqual({ tokens: 100, reported: 2 });
});

test("no usage reports differs from a team reporting zero", () => {
  expect(usageTotals([{ ...base, usage: null, paused: false, limit: null }])).toEqual({ tokens: 0, reported: 0 });
  expect(usageTotals([{ ...base, usage: zero, paused: false, limit: null }])).toEqual({ tokens: 0, reported: 1 });
  expect(usageTotals([])).toEqual({ tokens: 0, reported: 0 });
});
