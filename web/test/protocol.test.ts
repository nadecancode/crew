// Every fixture file is read by the decoders; the protocol number and every
// enum list are the Rust side's (notes/web-design.md §tests).
import { describe, expect, test } from "vitest";
import * as P from "../src/protocol";
import { fixture, fixtureLines } from "./fixtures";

const RESULT_DECODERS: Record<string, (v: unknown) => unknown> = {
  hello: P.decodeHello,
  web_pair: P.decodePaired,
  snapshot: P.decodeSnapshot,
  send: P.decodeSendResult,
  answer: P.decodeSendResult,
  task_get: P.decodeTaskCard,
  inbox: P.decodeInbox,
  git_diff: P.decodeGitOutcome,
  git_merge: P.decodeGitOutcome,
  agent_output: P.decodeOutput,
  models_list: P.decodeModels,
  agent_transcript: P.decodeTranscript,
  events: P.decodeEvents,
  history: P.decodeMessages,
  message_get: P.decodeMessage,
  tasks_list: (v) => (v as unknown[]).map((t) => P.decodeTask(t)),
  agent_pause: P.decodeAgentOrNull,
  agent_resume: P.decodeAgentOrNull,
  agent_restart: P.decodeAgentOrNull,
  agent_restart_fresh: P.decodeAgentOrNull,
  agent_kill: P.decodeAgentOrNull,
  agent_compact: P.decodeAgentOrNull,
  agent_set_model: P.decodeAgentOrNull,
};

describe("fixtures", () => {
  for (const name of ["snapshot.json", "subagents.json", "team10.json"]) {
    test(`${name} is a snapshot`, () => {
      const snap = P.decodeSnapshot(fixture(name));
      expect(snap.agents.length).toBeGreaterThan(0);
      expect(snap.session.web).toBe(snap.session.web ?? "");
    });
  }

  test("every frame of pushes.jsonl decodes; the unknown kind is ignored", () => {
    const kinds = new Set<string>();
    for (const line of fixtureLines("pushes.jsonl")) {
      const frame = P.decodeFrame(JSON.stringify(line));
      if (frame.kind === "push") kinds.add(frame.push.kind);
      else {
        expect(frame.kind).toBe("ignored");
        expect(line.push.kind).toBe("future_kind");
      }
    }
    for (const kind of ["event", "message", "agent", "agent_removed", "task", "session", "resync", "pty", "pty_closed", "shutdown"]) expect(kinds).toContain(kind);
  });

  test("every vcs.* event of pushes.jsonl carries a payload the page reads", () => {
    for (const line of fixtureLines("pushes.jsonl")) {
      const frame = P.decodeFrame(JSON.stringify(line));
      if (frame.kind !== "push" || frame.push.kind !== "event" || frame.push.data.source !== "vcs") continue;
      const vcs = P.vcsOf(frame.push.data);
      expect(vcs, frame.push.data.event_type).not.toBeNull();
      expect(`vcs.${vcs!.action}`).toBe(frame.push.data.event_type);
    }
  });

  test("results.json: every method's examples decode", () => {
    const results = fixture<Record<string, unknown[]>>("results.json");
    for (const [method, examples] of Object.entries(results)) {
      const decode = RESULT_DECODERS[method];
      for (const ex of examples) {
        const frame = P.decodeFrame(JSON.stringify({ id: 1, result: ex }));
        expect(frame.kind).toBe("result");
        if (decode) expect(() => decode(structuredClone(ex)), method).not.toThrow();
      }
    }
    // The calls of §transport's table that answer something the page reads.
    for (const method of Object.keys(RESULT_DECODERS)) expect(Object.keys(results), method).toContain(method);
  });

  test("errors.json: one error frame per code", () => {
    const codes = fixture<unknown[]>("errors.json").map((f) => {
      const frame = P.decodeFrame(JSON.stringify(f));
      expect(frame.kind).toBe("error");
      return frame.kind === "error" ? frame.error.code : "";
    });
    for (const code of ["unauthorized", "protocol", "forbidden", "unavailable", "invalid", "not_found"]) expect(codes).toContain(code);
  });
});

test("the protocol number and every enum list are the daemon's", () => {
  const enums = fixture<Record<string, unknown>>("enums.json");
  expect(P.PROTOCOL).toBe(enums.protocol);
  expect([...P.AGENT_STATUS]).toEqual(enums.agent_status);
  expect([...P.TASK_STATUS]).toEqual(enums.task_status);
  expect([...P.MESSAGE_TYPE]).toEqual(enums.message_type);
  expect([...P.RUNTIME_KIND]).toEqual(enums.runtime_kind);
  expect([...P.EVENT_LEVEL]).toEqual(enums.event_level);
  expect([...P.TRANSCRIPT_KIND]).toEqual(enums.transcript_kind);
  expect([...P.ROLE]).toEqual(enums.role);
  expect([...P.PUSH_KIND]).toEqual(enums.push);
  expect([...P.VCS_ACTION]).toEqual(enums.vcs_action);
  expect([...P.VCS_OUTCOME]).toEqual(enums.vcs_outcome);
});

test("malformed frames are refused, not applied", () => {
  expect(() => P.decodeFrame("not json")).toThrow(P.DecodeError);
  expect(() => P.decodeFrame(JSON.stringify({ push: { kind: "agent", data: { id: 1 } } }))).toThrow(P.DecodeError);
  expect(() => P.decodeFrame(JSON.stringify({ push: { kind: "agent", data: { id: "a", status: "dancing", runtime: "claude", last_activity_at: 0 } } }))).toThrow(P.DecodeError);
  expect(P.decodeFrame(JSON.stringify({ push: { kind: "deliver", data: { ids: [], text: "" } } })).kind).toBe("ignored");
});

test("a VCS payload this build does not know is an ordinary log line", () => {
  const e = { seq: 1, timestamp: 0, source: "vcs", event_type: "vcs.rebase", message: "", level: "info" as const, data: { action: "rebase", outcome: "ok", op: "v1", agent: "a", backend: "jj" } };
  expect(P.vcsOf(e)).toBeNull();
  expect(P.vcsOf({ ...e, event_type: "vcs.merge", data: { ...e.data, action: "merge", outcome: "later" } })).toBeNull();
  expect(P.vcsOf({ ...e, event_type: "vcs.merge", data: { ...e.data, action: "merge" } })).not.toBeNull();
});
