// Development-only v6 scenarios. Generated Rust fixtures remain normative and
// untouched. Initial data is seeded before subscription; later activity uses
// the same controller pushes as the browser tests.
import type { Mock, MockConn, MockOptions, MockWorkspace } from "./mock.ts";
import { MESSAGE_TYPE, TASK_STATUS } from "../src/protocol.ts";
import type { Agent, Event, Message, Task } from "../src/protocol.ts";

export const SCENARIO_NAMES = ["baseline", "busy", "attention", "empty", "long-text", "limits", "disconnected", "multi"] as const;
export type ScenarioName = (typeof SCENARIO_NAMES)[number];
export const SCENARIO_TIME = 1_760_000_000_000;
export const LONG_TEXT = "Preserve checkout compatibility across currencies, retries, and reconciliation. 日本語の請求明細 · café · مرحبًا · 👩🏽‍💻. ";
export const QUESTION_ID = "m-scenario-operator-question";

/** Add multi-workspace defaults only when the caller did not supply its own. */
export function scenarioOptions(options: MockOptions): MockOptions {
  if (options.scenario !== "multi" || options.workspaces) return options;
  return {
    ...options,
    workspaces: [
      { fixture: options.fixture ?? "subagents" },
      { fixture: "team10", project: "search", workspace: "/work/search" },
      { fixture: "snapshot", project: "archived", workspace: "/work/archived", status: "stopped" },
      { fixture: "snapshot", project: "old-daemon", workspace: "/work/old-daemon", status: "incompatible" },
      { id: "8a1d2c3b4e5f6071", project: "offline", workspace: "/work/offline", status: "unreachable", detail: "daemon socket unavailable" },
    ],
    others: options.others ?? [{ id: "9a1d2c3b4e5f6071", project: "docs", workspace: "/work/docs" }],
  };
}

function seedBusy(ws: MockWorkspace, now: number): void {
  ws.push({ kind: "session", data: { ...ws.state.session, objective: "Ship a reliable checkout service with compatible APIs and clear reconciliation evidence.", sandbox: "required" } });
  const members = ws.state.agents.filter((a) => !a.orchestrator && !a.parent);
  members.forEach((raw, i) => {
    const a = raw as Agent;
    ws.push({ kind: "agent", data: {
      ...a, status: (["working", "waiting", "blocked", "idle", "done", "failed"] as const)[i % 6],
      activity: ["Checking rounding rules", "Waiting for schema review", "Needs dependency review", "Ready for work", "Committed test evidence", "Test command exited 1"][i % 6],
      runtime: i === 3 ? "pi" : i === 5 ? "shell" : a.runtime,
      last_activity_at: now - 15_000 * (i + 1), progress: i === 0 ? 51 : null,
      context: i === 2 ? null : { tokens: 24_000 + i * 18_000, window: 200_000, percent: 12 + i * 9, updated_at: now - 30_000 },
      usage: i === 2 ? null : i === 3 ? { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_creation_tokens: 0, turns: 0 } : a.usage,
      compact_pending: i === 1, compactions: i, inbox_unread: i === 0 ? 2 : 0,
      error: i === 5 ? "The test command exited with code 1. Inspect the terminal output before restarting." : null,
    } });
  });
  TASK_STATUS.forEach((status, i) => {
    const t: Task = {
      id: `scenario-${status}`, title: `${status[0].toUpperCase()}${status.slice(1)} checkout work`,
      description: "Verify compatibility, record evidence, and ask the reviewer to inspect the result.",
      status, assigned_agent: members[i % members.length]?.id ?? null,
      dependencies: i === 0 ? [] : [`scenario-${TASK_STATUS[i - 1]}`],
      reviewer: members[1]?.id ?? null, created_at: now - 600_000,
      started_at: i > 1 ? now - 300_000 : null,
      completed_at: status === "done" || status === "failed" ? now - 60_000 : null,
      progress: status === "working" ? 51 : status === "done" ? 100 : null,
      note: status === "blocked" ? "Waiting for the adapter contract." : null,
      result: status === "done" ? "Compatibility tests passed; implementation and evidence committed." : status === "failed" ? "Reconciliation assertion failed." : null,
      timeout_secs: status === "working" ? 1800 : null,
      artifacts: status === "done" ? [
        { kind: "commit", value: "3f4e5d6c7b8a9012", description: "Checkout compatibility", agent: members[0]?.id, created_at: now - 60_000 },
        { kind: "file", value: "reports/checkout-compatibility.txt", description: "Recorded test evidence", agent: members[0]?.id, created_at: now - 60_000 },
      ] : [],
    };
    ws.push({ kind: "task", data: t });
  });
  MESSAGE_TYPE.forEach((type, i) => {
    const m: Message = {
      id: `m-scenario-${type}`, from: members[i % members.length]?.id ?? "orchestrator",
      to: i % 3 === 0 ? "*" : members[(i + 1) % members.length]?.id ?? "orchestrator", type,
      body: `${type}: The checkout adapter now preserves integer cents. Review the rounding evidence and confirm the dependency before the next change.`,
      timestamp: now - (MESSAGE_TYPE.length - i) * 30_000,
      correlation_id: "checkout-adapter-review", reply_to: i === 3 ? "m-scenario-question" : null, metadata: null,
    };
    ws.push({ kind: "message", data: m });
  });
  let seq = Math.max(0, ...ws.state.events.map((e) => e.seq)) + 1;
  (["debug", "info", "warn", "error"] as const).forEach((level, i) => {
    const e: Event = { seq: seq++, timestamp: now - (4 - i) * 30_000, source: "scheduler", event_type: "task.updated", agent_id: members[i]?.id ?? null, task_id: `scenario-${TASK_STATUS[i]}`, message: `${level}: Checkout dependency and review state updated. Evidence remains available in the task card.`, level, data: { retry: i, diagnostic: "<script>plain text only</script>" } };
    ws.push({ kind: "event", data: e });
  });
  ws.push({ kind: "event", data: {
    seq, timestamp: now - 20_000, source: "vcs", event_type: "vcs.cherry_pick", agent_id: members[0]?.id ?? null, task_id: "scenario-blocked",
    message: "Cherry-pick needs conflict resolution in the checkout adapter.", level: "warn",
    data: { backend: "git", action: "cherry_pick", outcome: "conflict", op: "scenario-vcs", agent: members[0]?.id ?? "orchestrator", peer: members[1]?.id, files: ["src/checkout.rs", "tests/compatibility.rs"], summary: "Resolve rounding conflict before integration." },
  } });
}

function seedLimits(ws: MockWorkspace, now: number): void {
  ws.push({ kind: "session", data: { ...ws.state.session, quotas: [
    { runtime: "claude", window: "five_hour", percent_used: 92, resets_at: now + 900_000, updated_at: now - 10_000 },
    { runtime: "claude", window: "seven_day", percent_used: 64, resets_at: now + 86_400_000, updated_at: now - 3_600_000 },
    { runtime: "codex", window: "five_hour", percent_used: 0, resets_at: null, updated_at: now - 10_000 },
  ] } });
  const a = ws.state.agents.find((a) => !a.orchestrator && !a.parent);
  if (a) ws.push({ kind: "agent", data: {
    ...a, paused: true, block_kind: "usage_limit", compact_pending: true,
    context: { tokens: 184_000, window: 200_000, percent: 92, updated_at: now - 10_000 },
    limit: { kind: "usage_limit", message: "Usage window exhausted; automatic retry after the reset.", since: now - 120_000, resets_at: now + 900_000, retry_at: now + 960_000, attempts: 2 },
    fallback: { from: "opus", note: "Requested model unavailable in the current usage window.", since: now - 180_000, policy: true },
  } });
}

export function seedScenario(mock: Mock, name: ScenarioName): void {
  const now = mock.now();
  if (name === "baseline") return;
  for (const ws of mock.workspaces) {
    if (ws.status !== "running") continue;
    if (name === "empty") {
      // There is no task/message removal push in v6. These are initial seeds,
      // not runtime deletions; no subscriber has seen this state yet.
      for (const a of [...ws.state.agents]) ws.push({ kind: "agent_removed", data: { id: a.id } });
      for (const key of ["tasks", "messages", "events", "questions"] as const) ws.state[key].splice(0);
      ws.push({ kind: "session", data: { ...ws.state.session, objective: "No team has been loaded in this workspace.", quotas: [], sandbox: "required" } });
      continue;
    }
    seedBusy(ws, now);
    if (name === "limits" || name === "busy" || name === "attention") seedLimits(ws, now);
    if (name === "long-text") {
      ws.push({ kind: "session", data: { ...ws.state.session, objective: LONG_TEXT.repeat(5) } });
      for (const a of [...ws.state.agents]) ws.push({ kind: "agent", data: { ...a, role: LONG_TEXT.repeat(2), task: LONG_TEXT.repeat(2), activity: LONG_TEXT } });
      for (const t of [...ws.state.tasks]) ws.push({ kind: "task", data: { ...t, title: LONG_TEXT.repeat(2), description: LONG_TEXT.repeat(10), result: t.result ? LONG_TEXT.repeat(8) : null } });
      ws.push({ kind: "message", data: { ...ws.state.messages.at(-1), id: "m-scenario-long", body: LONG_TEXT.repeat(20) + "\n<img src=x onerror=alert(1)>", timestamp: now - 1000 } });
      ws.push({ kind: "event", data: { ...ws.state.events.at(-1), seq: Math.max(...ws.state.events.map((e) => e.seq)) + 1, message: LONG_TEXT.repeat(10), timestamp: now - 1000 } });
    }
    if (name === "attention") {
      ws.push({ kind: "session", data: { ...ws.state.session, sandbox: "off", socket_warning: "Socket permissions allow other local users to connect.", vcs_note: "Working tree has changes outside the agent branches." } });
      for (let i = 0; i < 2; i++) ws.push({ kind: "message", data: {
        id: `${QUESTION_ID}-${i}`, from: "orchestrator", to: "operator", type: "question",
        body: "The legacy export writes amounts as integer cents. Which compatibility strategy should the adapter use?",
        timestamp: now - 10_000 + i, correlation_id: "checkout-approval", reply_to: null,
        metadata: { asked_by: ws.state.agents.find((a) => !a.orchestrator)?.id, options: ["Convert in the adapter", "Update the export contract", "Wait for review"] },
      } });
    }
  }
  if (name === "empty") for (const method of ["inbox", "agent_transcript", "git_diff", "agent_output"] as const) setPanelRead(mock, method, "empty");
}

/** One explicit live step. No scenario installs a timer by default. */
export function advanceScenario(mock: Mock, name: ScenarioName): void {
  if (name === "disconnected") {
    mock.setStatus(mock.workspace().id, "unreachable", "Mock daemon socket unavailable; cached evidence retained.");
    for (const c of mock.workspace().connections) c.drop();
    return;
  }
  const a = mock.state.agents.find((a) => !a.orchestrator);
  if (a) mock.push({ kind: "agent", data: { ...a, activity: "Validating the next compatibility case", last_activity_at: mock.now(), progress: ((a.progress ?? 0) + 1) % 100 } });
  mock.push({ kind: "event", data: { seq: Math.max(0, ...mock.state.events.map((e) => e.seq)) + 1, timestamp: mock.now(), source: "mock", event_type: "scenario.step", agent_id: a?.id ?? null, task_id: null, message: "An explicit scenario activity step was requested.", level: "info", data: null } });
}

export const PANEL_METHODS = ["inbox", "events", "task_get", "message_get", "agent_transcript", "git_diff", "agent_output"] as const;
export type PanelMethod = (typeof PANEL_METHODS)[number];
export const READ_MODES = ["ready", "pending", "error", "empty", "unavailable", "truncated"] as const;
export type ReadMode = (typeof READ_MODES)[number];

function panelResult(mock: Mock, method: PanelMethod, p: Record<string, any>, conn: MockConn, mode: ReadMode): unknown {
  const s = mock.workspace(conn.workspace ?? 0).state;
  const empty = mode === "empty" || mode === "unavailable";
  switch (method) {
    case "inbox": return { messages: empty ? [] : s.messages.filter((m) => m.to === (p.agent ?? p.id) || m.to === "*").slice(-20).map((m, i) => ({ ...m, delivered_at: m.timestamp + 100, acked_at: i % 2 ? m.timestamp + 200 : null })) };
    case "events": return empty ? [] : s.events.filter((e) => !p.agent || e.agent_id === p.agent).slice(-(p.limit ?? 100));
    case "task_get": {
      const task = s.tasks.find((t) => t.id === p.id);
      if (!task) throw { code: "not_found", message: `No scenario task '${p.id}'` };
      return { task, dependencies: s.tasks.filter((t) => task.dependencies.includes(t.id)) };
    }
    case "message_get": {
      const message = s.messages.find((m) => m.id === p.id);
      if (!message) throw { code: "not_found", message: `No scenario message '${p.id}'` };
      return message;
    }
    case "agent_transcript": return { available: mode !== "unavailable", truncated: mode === "truncated", entries: empty ? [] : [
      { kind: "prompt", at: mock.now() - 60_000, text: `[crew] Verify checkout compatibility (${p.id}).` },
      { kind: "tool", at: mock.now() - 45_000, text: "cargo test checkout_compatibility" },
      { kind: "reply", at: mock.now() - 30_000, text: "Compatibility tests passed. Evidence committed." },
      { kind: "note", at: null, text: "Session compacted; earlier context retained in summary." },
    ] };
    case "git_diff": return { ok: true, commit: null, conflict: null, text: empty ? "" : `diff --git a/src/checkout.rs b/src/checkout.rs\n--- a/src/checkout.rs\n+++ b/src/checkout.rs\n@@ -1 +1,2 @@\n pub fn total() {}\n+pub fn round_cents() {}\n` };
    case "agent_output": return { alive: mode !== "unavailable", text: empty ? "" : `\u001b[1m${p.id}\u001b[0m $ cargo test checkout_compatibility\n\ntest result: ok. 12 passed\n` };
  }
}

/** Capture-ready pending reads have an explicit release, never a timed race. */
export function setPanelRead(mock: Mock, method: PanelMethod, mode: ReadMode): () => void {
  let release = () => {};
  const gate = new Promise<void>((done) => { release = done; });
  mock.on(method, async (params, conn) => {
    if (mode === "error") throw { code: "unavailable", message: `Mock ${method} read failed. Previously loaded evidence is unchanged.` };
    const result = structuredClone(panelResult(mock, method, params ?? {}, conn, mode));
    if (mode === "pending") await gate;
    return result;
  });
  return () => { release(); mock.on(method, null); };
}
