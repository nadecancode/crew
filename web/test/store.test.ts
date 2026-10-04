// The store's reducers (notes/web-design.md §tests "store.test.ts").
import { describe, expect, test, vi } from "vitest";
import { decodeSnapshot, type Event, type Message, type Snapshot } from "../src/protocol";
import { createState, MAX_EVENTS, MAX_MESSAGES } from "../src/store/store";
import { fixture } from "./fixtures";

const snap = (name = "snapshot.json"): Snapshot => decodeSnapshot(fixture(name));
const NOW = 1_760_000_000_000;

function loaded(name?: string, notices = { status: vi.fn(), notice: vi.fn(), agentRemoved: vi.fn() }) {
  const s = createState(notices);
  const sn = snap(name);
  s.applySnapshot(sn, sn.server_time);
  return { s, sn, notices };
}

const message = (id: string, over: Partial<Message> = {}): Message => ({ id, from: "claude-1", to: "claude-2", type: "message", body: "x", timestamp: NOW, ...over });
const event = (seq: number, over: Partial<Event> = {}): Event => ({ seq, timestamp: NOW, source: "daemon", event_type: "x", message: "m", level: "info", ...over });

describe("snapshot, then pushes", () => {
  test("an event with a known seq and a message with a known id are dropped", () => {
    const { s, sn } = loaded();
    const lastSeq = sn.events[sn.events.length - 1].seq;
    const events = s.events.length;
    s.applyPush({ kind: "event", data: event(lastSeq) }, NOW);
    s.applyPush({ kind: "event", data: event(lastSeq - 1) }, NOW);
    expect(s.events.length).toBe(events);
    s.applyPush({ kind: "event", data: event(lastSeq + 1) }, NOW);
    s.applyPush({ kind: "event", data: event(0) }, NOW);
    expect(s.events.length).toBe(events + 2);
    const messages = s.messages.length;
    s.applyPush({ kind: "message", data: message(sn.messages[sn.messages.length - 1].id) }, NOW);
    expect(s.messages.length).toBe(messages);
    s.applyPush({ kind: "message", data: message("m-new") }, NOW);
    expect(s.messages.length).toBe(messages + 1);
  });

  test("agent order is the TUI's: natural id order, subagents after their parent, the orchestrator last", () => {
    const { s } = loaded("subagents.json");
    expect(s.state.agents.map((a) => a.id)).toEqual(["claude-1", "claude-2", "claude-3", "claude-3-s1", "claude-4", "claude-5", "claude-6", "orchestrator"]);
    const ten = loaded("team10.json").s;
    expect(ten.state.agents.map((a) => a.id)).toEqual([...Array.from({ length: 10 }, (_, i) => `claude-${i + 1}`), "orchestrator"]);
    // A new agent takes its place in the order.
    s.applyPush({ kind: "agent", data: { ...JSON.parse(JSON.stringify(s.state.agents[0])), id: "claude-10" } }, NOW);
    expect(s.state.agents.map((a) => a.id).indexOf("claude-10")).toBe(7);
  });

  test("a push replaces one agent by id and changes nothing else", () => {
    const { s } = loaded();
    const before = s.state.agents[1];
    s.applyPush({ kind: "agent", data: { ...JSON.parse(JSON.stringify(s.state.agents[0])), activity: "Run tests" } }, NOW);
    expect(s.state.agents[0].activity).toBe("Run tests");
    expect(s.state.agents[1]).toBe(before);
  });

  test("a removed agent leaves, takes its marks, and its inspector is told to close", () => {
    const { s, notices } = loaded();
    s.applyPush({ kind: "agent_removed", data: { id: "claude-2" } }, NOW);
    expect(s.state.agents.some((a) => a.id === "claude-2")).toBe(false);
    expect(notices.agentRemoved).toHaveBeenCalledWith("claude-2");
  });

  test("a question joins on its message and leaves on the operator's answer", () => {
    const { s, notices } = loaded();
    const q = message("m-q", { from: "orchestrator", to: "operator", type: "question", body: "which?" });
    s.applyPush({ kind: "message", data: q }, NOW);
    expect(s.openQuestions().map((m) => m.id)).toContain("m-q");
    expect(notices.status).toHaveBeenCalledWith("? orchestrator has a question for you", false);
    s.setAnswering("m-q", true);
    expect(s.openQuestions().map((m) => m.id)).not.toContain("m-q");
    s.applyPush({ kind: "message", data: message("m-a", { from: "operator", to: "orchestrator", type: "answer", reply_to: "m-q" }) }, NOW);
    expect(s.state.questions.map((m) => m.id)).not.toContain("m-q");
    expect(s.state.answering).toEqual([]);
  });

  test("the rings cap", () => {
    const { s } = loaded();
    for (let i = 0; i < MAX_EVENTS + 10; i++) s.applyPush({ kind: "event", data: event(10_000 + i) }, NOW);
    expect(s.events.length).toBe(MAX_EVENTS);
    expect(s.events.last()!.seq).toBe(10_000 + MAX_EVENTS + 9);
    for (let i = 0; i < MAX_MESSAGES + 10; i++) s.applyPush({ kind: "message", data: message(`m-x${i}`) }, NOW);
    expect(s.messages.length).toBe(MAX_MESSAGES);
  });

  test("an agent.error event raises a notice; a replayed one does not", () => {
    const { s, sn, notices } = loaded();
    const last = sn.events[sn.events.length - 1].seq;
    s.applyPush({ kind: "event", data: event(last, { event_type: "agent.error", level: "error", message: "provider said no" }) }, NOW);
    expect(notices.notice).not.toHaveBeenCalled();
    s.applyPush({ kind: "event", data: event(last + 1, { event_type: "agent.refused", level: "error", message: "refused" }) }, NOW);
    expect(notices.notice).toHaveBeenCalledWith("refused");
  });

  test("a fresh vcs event starts its animation; one older than 10 s only ends what runs", () => {
    const { s, sn } = loaded();
    const now = sn.server_time;
    const vcs = (seq: number, outcome: string, ts: number) => event(seq, { source: "vcs", event_type: "vcs.merge", timestamp: ts, data: { action: "merge", outcome, op: "v99", backend: "jj", agent: "claude-3", peer: "claude-5" } });
    s.applyPush({ kind: "event", data: vcs(1000, "started", now) }, now);
    expect(s.anims.running()).toBe(true);
    s.applyPush({ kind: "event", data: vcs(1001, "ok", now - 11_000) }, now);
    expect(s.anims.running()).toBe(false);
    expect(s.anims.links.length).toBe(0);
  });

  test("a snapshot with a started operation restores it as running; one with both shows nothing", () => {
    const sn = snap();
    const started = event(500, { source: "vcs", event_type: "vcs.merge", timestamp: sn.server_time - 2000, data: { action: "merge", outcome: "started", op: "v7", backend: "jj", agent: "claude-3", peer: "claude-5" } });
    const s = createState();
    s.applySnapshot({ ...sn, events: [...sn.events, started] }, sn.server_time);
    expect(s.anims.markFor("claude-3", sn.server_time)?.label).toBe("merge claude-5");
    const ended = { ...started, seq: 501, data: { ...(started.data as object), outcome: "ok" } };
    s.applySnapshot({ ...sn, events: [...sn.events, started, ended] }, sn.server_time);
    expect(s.anims.marks).toEqual([]);
  });
});
