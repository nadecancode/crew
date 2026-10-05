// @vitest-environment node
import { readFileSync } from "node:fs";
import { expect, test } from "vitest";
import { startMock } from "../dev/mock.ts";
import { advanceScenario, PARKED_TASKS, SCENARIO_NAMES, SCENARIO_TIME } from "../dev/scenarios.ts";
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

test("parked architecture cards retain identity and original start through review and resumption", async () => {
  const mock = await startMock({ scenario: "parked", fixture: "subagents" });
  try {
    const worker = () => mock.state.agents.find((a) => !a.orchestrator)!;
    const read = (id: string) => mock.state.tasks.find((t) => t.id === id)!;
    const originalStart = read(PARKED_TASKS.older).started_at;
    const newer = structuredClone(read(PARKED_TASKS.newer));
    expect(mock.state.agents.filter((a) => !a.orchestrator)).toHaveLength(1);
    expect(mock.state.tasks.map((t) => t.assigned_agent)).toEqual([worker().id, worker().id, worker().id]);
    expect(mock.state.tasks.filter((t) => t.status === "working").map((t) => t.id)).toEqual([PARKED_TASKS.current]);
    expect(worker().task).toBe(read(PARKED_TASKS.current).title);
    advanceScenario(mock, "parked");
    expect(read(PARKED_TASKS.current).status).toBe("review");
    expect(mock.state.tasks.filter((t) => t.status === "working")).toEqual([]);
    expect(worker().task).toBe(newer.title);
    advanceScenario(mock, "parked");
    expect(read(PARKED_TASKS.current).status).toBe("done");
    expect(mock.state.tasks.filter((t) => t.status === "working").map((t) => t.id)).toEqual([PARKED_TASKS.older]);
    expect(read(PARKED_TASKS.older).started_at).toBe(originalStart);
    expect(read(PARKED_TASKS.newer)).toEqual(newer);
    expect(worker().task).toBe(read(PARKED_TASKS.older).title);
    expect(worker().progress).toBe(read(PARKED_TASKS.older).progress);
    const final = decodeSnapshot(mock.snapshot());
    advanceScenario(mock, "parked");
    expect(decodeSnapshot(mock.snapshot())).toEqual(final);
  } finally { await mock.stop(); }
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
