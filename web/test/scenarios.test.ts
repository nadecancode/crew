// @vitest-environment node
import { readFileSync } from "node:fs";
import { expect, test } from "vitest";
import { startMock } from "../dev/mock.ts";
import { advanceScenario, SCENARIO_NAMES, SCENARIO_TIME } from "../dev/scenarios.ts";
import { decodeSnapshot, decodeWorkspaces, TASK_STATUS } from "../src/protocol.ts";
import { FIXTURES } from "./fixtures";

test.each(SCENARIO_NAMES)("%s scenario is deterministic v6 data; generated fixture stays untouched", async (scenario) => {
  const original = readFileSync(`${FIXTURES}subagents.json`, "utf8");
  const first = await startMock({ scenario, fixture: "subagents" });
  const second = await startMock({ scenario, fixture: "subagents" });
  try {
    const a = decodeSnapshot(first.snapshot()), b = decodeSnapshot(second.snapshot());
    // Server addresses stay random for isolation; visible scenario content and
    // the server clock are deterministic.
    a.session.web = b.session.web = "";
    expect(a).toEqual(b);
    expect(first.now()).toBe(SCENARIO_TIME);
    expect(decodeWorkspaces(first.listing()).joined).toHaveLength(scenario === "multi" ? 5 : 1);
    if (scenario === "baseline") expect(first.state.agents).toEqual(JSON.parse(original).agents);
    if (scenario === "empty") for (const list of ["agents", "tasks", "messages", "events", "questions"] as const) expect(first.state[list]).toEqual([]);
    if (scenario === "busy") {
      expect(new Set(first.state.tasks.map((t) => t.status))).toEqual(new Set(TASK_STATUS));
      expect(first.state.agents.some((a) => a.usage === null)).toBe(true);
      expect(first.state.agents.some((a) => a.usage?.input_tokens === 0)).toBe(true);
      expect(first.state.session.quotas.some((q: { updated_at: number }) => SCENARIO_TIME - q.updated_at > 900_000)).toBe(true);
      expect(first.state.session.quotas.map((q: { window: string }) => q.window)).toEqual(["5h", "week", "5h"]);
      expect(first.state.agents.find((a) => a.limit)?.limit.kind).toBe("usage");
    }
    if (scenario === "attention") expect(first.state.agents.some((a) => a.status === "blocked" && a.block_kind === "refused" && a.error)).toBe(true);
    expect(readFileSync(`${FIXTURES}subagents.json`, "utf8")).toBe(original);
  } finally {
    await Promise.all([first.stop(), second.stop()]);
  }
});

test("activity is explicit and changes the same state later snapshots read", async () => {
  const mock = await startMock({ scenario: "busy" });
  try {
    const before = mock.state.events.length;
    advanceScenario(mock, "busy");
    expect(mock.state.events).toHaveLength(before + 1);
    expect(decodeSnapshot(mock.snapshot()).events.at(-1)?.event_type).toBe("scenario.step");
    advanceScenario(mock, "disconnected");
    expect(decodeWorkspaces(mock.listing()).joined[0].state).toBe("unreachable");
    expect(mock.state.events).toHaveLength(before + 1);
  } finally {
    await mock.stop();
  }
});
