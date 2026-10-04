import { describe, expect, it } from "vitest";
import { applyPush, applySnapshot, events, state } from "../src/store";

describe("reducers", () => {
  it("applies a snapshot, then pushes, ignoring events it already has", () => {
    applySnapshot({ session: { project: "p" }, agents: [{ id: "a", role: "r", status: "idle", activity: "", runtime: "shell" }], tasks: [], messages: [], events: [{ seq: 5, timestamp: 0, source: "d", event_type: "x", message: "m", level: "info" }] });
    applyPush({ kind: "event", data: { seq: 5, timestamp: 0, source: "d", event_type: "x", message: "again", level: "info" } });
    applyPush({ kind: "event", data: { seq: 6, timestamp: 0, source: "d", event_type: "x", message: "new", level: "info" } });
    applyPush({ kind: "agent", data: { id: "a", role: "r", status: "working", activity: "go", runtime: "shell" } });
    expect(events.map((e) => e.seq)).toEqual([5, 6]);
    expect(state.agents[0].status).toBe("working");
  });
});
