// Fixture-only adapter. The JSON corpus owns expectations; existing web
// decoders, reducers and current-work presentation supply observations.
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "vitest";
import { workOf } from "../../../web/src/format";
import { decodeFrame, decodeSnapshot, type Agent, type Task } from "../../../web/src/protocol";
import { createState } from "../../../web/src/store/store";

const directory = resolve(process.cwd(), "fixtures/native/state") + "/";
const cases = readdirSync(directory).filter((name) => name.endsWith(".case.json")).sort()
  .map((name) => JSON.parse(readFileSync(directory + name, "utf8")));

function subset(actual: unknown, expected: unknown, path: string): void {
  if (Array.isArray(expected)) {
    assert.ok(Array.isArray(actual), path + ": not an array");
    assert.equal(actual.length, expected.length, path + ": array length");
    expected.forEach((value, index) => subset(actual[index], value, path + "[" + index + "]"));
  } else if (expected !== null && typeof expected === "object") {
    assert.ok(actual !== null && typeof actual === "object" && !Array.isArray(actual), path + ": not an object");
    for (const [key, value] of Object.entries(expected)) {
      assert.ok(Object.hasOwn(actual, key), path + "." + key + ": missing");
      subset((actual as Record<string, unknown>)[key], value, path + "." + key);
    }
  } else assert.deepEqual(actual, expected, path);
}

// Rust serializes absent Option fields as null. Normalize only the nullable
// fields inspected by this corpus; never turn an absent value into zero.
function agentFields(agent: Agent) {
  return { ...agent, task: agent.task ?? null, progress: agent.progress ?? null, usage: agent.usage ?? null, context: agent.context ?? null };
}
function taskFields(task: Task) {
  return { ...task, started_at: task.started_at ?? null, completed_at: task.completed_at ?? null, progress: task.progress ?? null, note: task.note ?? null };
}

for (const fixture of cases) {
  test(fixture.id, () => {
    const store = createState();
    for (const [index, step] of fixture.steps.entries()) {
      if (step.snapshot) store.applySnapshot(decodeSnapshot(structuredClone(step.snapshot)), step.at_ms);
      else {
        const frame = decodeFrame(JSON.stringify(step.frame));
        assert.equal(frame.kind, "push");
        if (frame.kind === "push") assert.equal(store.applyPush(frame.push, step.at_ms), true);
      }
      const expected = { ...step.expect, ...step.legacy_client_expect?.web };
      const work = Object.fromEntries(Object.keys(expected.work ?? {}).map((id) => {
        const value = workOf(store.state.tasks, id, store.serverNow(step.at_ms));
        return [id, value ? { task_id: value.task.id, done: value.done, elapsed_ms: value.elapsed } : null];
      }));
      const actual = {
        agent_ids: store.state.agents.map((a) => a.id).sort(),
        task_ids: store.state.tasks.map((t) => t.id),
        agents: Object.fromEntries(store.state.agents.map((a) => [a.id, agentFields(a)])),
        tasks: Object.fromEntries(store.state.tasks.map((t) => [t.id, taskFields(t)])),
        work,
        message_ids: store.messages.toArray().map((m) => m.id),
        messages: store.messages.toArray(),
        event_seqs: store.events.toArray().map((e) => e.seq),
        events: store.events.toArray(),
        question_ids: store.state.questions.map((q) => q.id),
        selection_validity: (expected.selection_validity ?? []).map((query: { kind: string; id: string }) => ({
          ...query,
          exists: query.kind === "agent" ? store.state.agents.some((a) => a.id === query.id) : store.state.tasks.some((t) => t.id === query.id),
        })),
        clock_offset_ms: store.state.clockOffset,
      };
      subset(actual, expected, fixture.id + "/" + index + ": " + step.reason);
    }
  });
}
