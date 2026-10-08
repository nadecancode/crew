// Wire types mirroring crew_core (rpc.rs, types.rs, vcs.rs), and the decoders
// that check what the daemon sends before the page uses it. The fixtures under
// crates/crew/tests/fixtures/web/ are the normative examples; test/protocol.test.ts
// reads every one of them through these decoders.

export const PROTOCOL = 6;

export const ORCHESTRATOR_ID = "orchestrator";
export const OPERATOR_ID = "operator";
export const BROADCAST = "*";
export const AGENT_ERROR_EVENT = "agent.error";
export const AGENT_REFUSED_EVENT = "agent.refused";
export const VCS_SOURCE = "vcs";
export const MAX_FRAME_BYTES = 1024 * 1024;

export const AGENT_STATUS = ["spawning", "idle", "working", "waiting", "blocked", "done", "failed", "stopped"] as const;
export const TASK_STATUS = ["queued", "ready", "working", "waiting", "blocked", "review", "done", "failed"] as const;
export const MESSAGE_TYPE = ["task", "message", "question", "answer", "status", "review_request", "review_result", "artifact", "error"] as const;
export const RUNTIME_KIND = ["claude", "codex", "pi", "shell"] as const;
export const EVENT_LEVEL = ["debug", "info", "warn", "error"] as const;
export const TRANSCRIPT_KIND = ["prompt", "reply", "tool", "note"] as const;
export const ROLE = ["operator", "agent"] as const;
export const PUSH_KIND = ["event", "message", "agent", "agent_removed", "task", "session", "resync", "deliver", "compact", "pty", "pty_closed", "shutdown", "summary", "workspaces"] as const;
export const VCS_ACTION = ["workspace", "commit", "cherry_pick", "merge", "integrate", "status", "diff"] as const;
export const VCS_OUTCOME = ["started", "ok", "conflict", "error"] as const;
export const WORKSPACE_STATE = ["running", "stopped", "incompatible", "unreachable"] as const;

export type AgentStatus = (typeof AGENT_STATUS)[number];
export type TaskStatus = (typeof TASK_STATUS)[number];
export type MessageType = (typeof MESSAGE_TYPE)[number];
export type RuntimeKind = (typeof RUNTIME_KIND)[number];
export type EventLevel = (typeof EVENT_LEVEL)[number];
export type TranscriptKind = (typeof TRANSCRIPT_KIND)[number];
export type VcsAction = (typeof VCS_ACTION)[number];
export type VcsOutcome = (typeof VCS_OUTCOME)[number];
export type WorkspaceState = (typeof WORKSPACE_STATE)[number];

export type Usage = { input_tokens: number; output_tokens: number; cache_read_tokens: number; cache_creation_tokens: number; turns: number };
export type ContextUsage = { tokens: number; window: number; percent: number; updated_at: number };
export type ModelFallback = { from: string; note: string; since: number; policy: boolean };
export type UsageLimit = { kind: string; message: string; since: number; resets_at?: number | null; retry_at?: number | null; attempts: number };

// A member's effective toolbox (notes/toolboxes.md §Display). Status words come from the
// daemon; codes only steer attention. Unknown kinds and codes stay displayable.
export type ToolboxItemStatus = { code: string; text: string };
export type ToolboxItem = { kind: string; name: string; origin: string; required: boolean; status: ToolboxItemStatus; detail?: string | null };
export type ToolboxSource = { origin: string; kind?: string | null; label: string; status: ToolboxItemStatus; names: string[]; detail?: string | null };
export type AgentToolbox = {
  fingerprint: string;
  pending_fingerprint?: string | null;
  definition_digest?: string | null;
  tool_count?: number | null;
  definition_bytes?: number | null;
  items: ToolboxItem[];
  sources: ToolboxSource[];
};

export type Agent = {
  id: string;
  name: string;
  role: string;
  task?: string | null;
  status: AgentStatus;
  activity: string;
  runtime: RuntimeKind;
  model?: string | null;
  workdir: string;
  branch?: string | null;
  backend: string;
  backend_ref?: string | null;
  pid?: number | null;
  session_id?: string | null;
  orchestrator: boolean;
  parent?: string | null;
  temporary: boolean;
  subagents: number;
  delivery: string;
  started_at?: number | null;
  last_activity_at: number;
  progress?: number | null;
  paused: boolean;
  block_kind?: string | null;
  limit?: UsageLimit | null;
  context?: ContextUsage | null;
  compact_pending: boolean;
  compactions: number;
  running_model?: string | null;
  model_name?: string | null;
  effort?: string | null;
  fallback?: ModelFallback | null;
  exit_code?: number | null;
  restarts: number;
  inbox_unread: number;
  inbox_total: number;
  outbox_total: number;
  usage?: Usage | null;
  error?: string | null;
  toolbox?: AgentToolbox | null;
};

export type Message = {
  id: string;
  from: string;
  to: string;
  type: MessageType;
  body: string;
  timestamp: number;
  correlation_id?: string | null;
  reply_to?: string | null;
  metadata?: unknown;
};

export type Artifact = { kind: string; value: string; description: string; agent?: string | null; created_at: number };

export type Task = {
  id: string;
  title: string;
  description: string;
  assigned_agent?: string | null;
  status: TaskStatus;
  dependencies: string[];
  created_at: number;
  started_at?: number | null;
  completed_at?: number | null;
  artifacts: Artifact[];
  reviewer?: string | null;
  result?: string | null;
  progress?: number | null;
  note?: string | null;
  timeout_secs?: number | null;
};

export type Event = {
  seq: number;
  timestamp: number;
  source: string;
  event_type: string;
  agent_id?: string | null;
  task_id?: string | null;
  message: string;
  level: EventLevel;
  data?: unknown;
};

export type VcsEvent = {
  action: VcsAction;
  outcome: VcsOutcome;
  op: string;
  backend: string;
  agent: string;
  peer?: string | null;
  commit?: string | null;
  change?: string | null;
  source?: string | null;
  branch?: string | null;
  files: string[];
  summary?: string | null;
  /** Page-only: the `seq` of the event it came in, for click-through to the log. */
  seq?: number;
};

export type Quota = { runtime: RuntimeKind; window: string; percent_used: number; resets_at?: number | null; updated_at: number };

export type SessionInfo = {
  project?: string | null;
  objective: string;
  style?: string | null;
  quotas: Quota[];
  workspace: string;
  state_dir: string;
  backend: string;
  isolation: string;
  sandbox: string;
  vcs: string;
  vcs_note: string;
  socket_warning: string;
  daemon_started_at: number;
  team_started_at?: number | null;
  completed_at?: number | null;
  summary?: string | null;
  herdr_workspace?: string | null;
  daemon_pid: number;
  version: string;
  /** The dashboard's address while it is served, else empty. */
  web: string;
  /** The workspace run, on a connection that negotiated features. */
  workspace_run_id?: string | null;
};

export type Snapshot = {
  session: SessionInfo;
  agents: Agent[];
  tasks: Task[];
  messages: Message[];
  events: Event[];
  questions: Message[];
  server_time: number;
};

export type HelloInfo = { role: string; agent_id?: string | null; project?: string | null; version: string; protocol: number; pid: number; web_proof?: string | null };
export type WebPaired = { token: string; verifier: string };
export type SendResult = { message: Message; duplicate: boolean; recipients: string[] };
export type TaskCard = { task: Task; dependencies: Task[] };
export type InboxEntry = Message & { delivered_at?: number | null; acked_at?: number | null };
export type InboxResult = { messages: InboxEntry[] };
export type CommitInfo = { sha: string; short: string; branch: string; summary: string; files_changed: number; change_id?: string | null };
export type ConflictReport = { operation: string; files: string[]; detail: string };
export type GitOutcome = { ok: boolean; commit?: CommitInfo | null; conflict?: ConflictReport | null; text: string };
export type OutputResult = { text: string; alive: boolean };
export type ModelChoice = { runtime: RuntimeKind; model: string; name: string; tier: string; price: string; summary: string; unavailable?: string | null };
export type ModelsResult = { models: ModelChoice[] };
export type TranscriptEntry = { kind: TranscriptKind; at?: number | null; text: string };
export type TranscriptResult = { entries: TranscriptEntry[]; available: boolean; truncated: boolean };

export type RpcError = { code: string; message: string };

// The dashboard server's own (notes/web-multi.md §multi-join "What the listing
// holds", §multi-transport "Summaries").

/** What a workspace switcher shows of a session: counts only, no text. */
export type WorkspaceSummary = {
  rev: number;
  project?: string | null;
  agents: number;
  working: number;
  attention: number;
  questions: number;
  tasks: number;
  tasks_done: number;
};
/** One run of a workspace's daemon, as the registry publishes it. */
export type DaemonRef = { pid: number; started_at: number };
export type WorkspaceEntry = {
  /** 16 hex digits: the stem of the daemon's socket. */
  id: string;
  /** Greater after the workspace was taken out and joined again. */
  generation: number;
  /** The daemon the server watches; absent unless `running`. */
  daemon?: DaemonRef | null;
  project?: string | null;
  /** The workspace directory. */
  workspace: string;
  state: WorkspaceState;
  /** Why, for `incompatible` and `unreachable`. */
  detail: string;
  summary?: WorkspaceSummary | null;
  joined_at: number;
};
/** A running session of this user that is not joined: named, not reachable. */
export type OtherSession = { id: string; project?: string | null; workspace: string };
/** The listing, whole each time: the `workspaces` reply and push. */
export type Workspaces = { rev: number; joined: WorkspaceEntry[]; others: OtherSession[] };

export type Push =
  | { kind: "event"; data: Event }
  | { kind: "message"; data: Message }
  | { kind: "agent"; data: Agent }
  | { kind: "agent_removed"; data: { id: string } }
  | { kind: "task"; data: Task }
  | { kind: "session"; data: SessionInfo }
  | { kind: "resync" }
  | { kind: "pty"; data: { data: string } }
  | { kind: "pty_closed" }
  | { kind: "shutdown" }
  /** On the index connection only. */
  | { kind: "workspaces"; data: Workspaces }
  /** A negotiated observer feature's push (`usage_changed`, `docs_changed`,
   * `aside`), outside the snapshot barrier; its view decodes `data`. */
  | { kind: "feature"; name: string; data: unknown };

export type Frame =
  | { kind: "result"; id: number; result: unknown }
  | { kind: "error"; id: number | null; error: RpcError }
  | { kind: "push"; push: Push }
  /** A push of a kind this page does not handle (the daemon may add kinds). */
  | { kind: "ignored" };

// ---- decoders --------------------------------------------------------------
//
// They check what the page relies on (types of the fields it reads, the enum
// values it switches on) and fill the defaults the Rust side has
// (`serde(default)`), so the rest of the code can trust the shape. They do not
// copy: the decoded value is the parsed JSON object, completed in place.

export class DecodeError extends Error {}

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
const fail = (what: string): never => {
  throw new DecodeError(what);
};
function obj(v: unknown, what: string): Obj {
  return isObj(v) ? v : fail(`${what}: not an object`);
}
function str(o: Obj, k: string, what: string): string {
  const v = o[k];
  return typeof v === "string" ? v : fail(`${what}.${k}: not a string`);
}
function num(o: Obj, k: string, what: string): number {
  const v = o[k];
  return typeof v === "number" && Number.isFinite(v) ? v : fail(`${what}.${k}: not a number`);
}
function oneOf<T extends string>(o: Obj, k: string, values: readonly T[], what: string): T {
  const v = o[k];
  return typeof v === "string" && (values as readonly string[]).includes(v) ? (v as T) : fail(`${what}.${k}: unknown value ${JSON.stringify(v)}`);
}
/** A field with a default: absent or null gives `d`; otherwise it must have the right type. */
function def<T>(o: Obj, k: string, d: T, check: (v: unknown) => boolean, what: string): void {
  const v = o[k];
  if (v === undefined || v === null) o[k] = d;
  else if (!check(v)) fail(`${what}.${k}: wrong type`);
}
function opt(o: Obj, k: string, check: (v: unknown) => boolean, what: string): void {
  const v = o[k];
  if (v !== undefined && v !== null && !check(v)) fail(`${what}.${k}: wrong type`);
}
const isStr = (v: unknown) => typeof v === "string";
const isNum = (v: unknown) => typeof v === "number" && Number.isFinite(v);
const isBool = (v: unknown) => typeof v === "boolean";
const isStrArr = (v: unknown) => Array.isArray(v) && v.every(isStr);
function list<T>(v: unknown, item: (x: unknown, what: string) => T, what: string): T[] {
  if (!Array.isArray(v)) fail(`${what}: not a list`);
  return (v as unknown[]).map((x, i) => item(x, `${what}[${i}]`));
}

export function decodeAgent(v: unknown, what = "agent"): Agent {
  const o = obj(v, what);
  str(o, "id", what);
  def(o, "name", o.id, isStr, what);
  def(o, "role", "", isStr, what);
  oneOf(o, "status", AGENT_STATUS, what);
  oneOf(o, "runtime", RUNTIME_KIND, what);
  def(o, "activity", "", isStr, what);
  def(o, "workdir", "", isStr, what);
  def(o, "backend", "", isStr, what);
  def(o, "delivery", "", isStr, what);
  num(o, "last_activity_at", what);
  for (const k of ["orchestrator", "temporary", "paused", "compact_pending"]) def(o, k, false, isBool, what);
  for (const k of ["subagents", "compactions", "restarts", "inbox_unread", "inbox_total", "outbox_total"]) def(o, k, 0, isNum, what);
  for (const k of ["task", "model", "branch", "backend_ref", "session_id", "parent", "block_kind", "running_model", "model_name", "effort", "error"]) opt(o, k, isStr, what);
  for (const k of ["pid", "started_at", "progress", "exit_code"]) opt(o, k, isNum, what);
  opt(o, "usage", isObj, what);
  opt(o, "context", isObj, what);
  opt(o, "limit", isObj, what);
  opt(o, "fallback", isObj, what);
  if (o.toolbox != null) o.toolbox = decodeToolbox(o.toolbox, `${what}.toolbox`);
  return o as Agent;
}

export function decodeToolbox(v: unknown, what = "toolbox"): AgentToolbox {
  const o = obj(v, what);
  str(o, "fingerprint", what);
  for (const k of ["pending_fingerprint", "definition_digest"]) opt(o, k, isStr, what);
  for (const k of ["tool_count", "definition_bytes"]) opt(o, k, isNum, what);
  // An item and a source: their strings, a status in the daemon's words, an
  // optional detail, and what else each has.
  const entry = (key: string, strs: string[], more: (e: Obj, w: string) => void) => {
    def(o, key, [], Array.isArray, what);
    list(o[key], (x, w) => {
      const e = obj(x, w), s = obj(e.status, `${w}.status`);
      for (const k of strs) str(e, k, w);
      str(s, "code", `${w}.status`);
      str(s, "text", `${w}.status`);
      opt(e, "detail", isStr, w);
      more(e, w);
    }, `${what}.${key}`);
  };
  entry("items", ["kind", "name", "origin"], (e, w) => def(e, "required", false, isBool, w));
  entry("sources", ["origin", "label"], (e, w) => (opt(e, "kind", isStr, w), def(e, "names", [], isStrArr, w)));
  return o as AgentToolbox;
}

export function decodeMessage(v: unknown, what = "message"): Message {
  const o = obj(v, what);
  str(o, "id", what);
  str(o, "from", what);
  str(o, "to", what);
  oneOf(o, "type", MESSAGE_TYPE, what);
  str(o, "body", what);
  num(o, "timestamp", what);
  opt(o, "correlation_id", isStr, what);
  opt(o, "reply_to", isStr, what);
  return o as Message;
}

export function decodeTask(v: unknown, what = "task"): Task {
  const o = obj(v, what);
  str(o, "id", what);
  str(o, "title", what);
  def(o, "description", "", isStr, what);
  oneOf(o, "status", TASK_STATUS, what);
  def(o, "dependencies", [], isStrArr, what);
  num(o, "created_at", what);
  def(o, "artifacts", [], Array.isArray, what);
  (o.artifacts as unknown[]).forEach((a, i) => {
    const w = `${what}.artifacts[${i}]`, x = obj(a, w);
    str(x, "kind", w);
    str(x, "value", w);
    def(x, "description", "", isStr, w);
    def(x, "created_at", 0, isNum, w);
  });
  for (const k of ["assigned_agent", "reviewer", "result", "note"]) opt(o, k, isStr, what);
  for (const k of ["started_at", "completed_at", "progress", "timeout_secs"]) opt(o, k, isNum, what);
  return o as Task;
}

export function decodeEvent(v: unknown, what = "event"): Event {
  const o = obj(v, what);
  num(o, "seq", what);
  num(o, "timestamp", what);
  str(o, "source", what);
  str(o, "event_type", what);
  str(o, "message", what);
  oneOf(o, "level", EVENT_LEVEL, what);
  opt(o, "agent_id", isStr, what);
  opt(o, "task_id", isStr, what);
  return o as Event;
}

export function decodeSession(v: unknown, what = "session"): SessionInfo {
  const o = obj(v, what);
  for (const k of ["objective", "workspace", "state_dir", "backend", "isolation", "sandbox", "vcs", "vcs_note", "socket_warning", "version", "web"]) def(o, k, "", isStr, what);
  def(o, "quotas", [], Array.isArray, what);
  def(o, "daemon_started_at", 0, isNum, what);
  def(o, "daemon_pid", 0, isNum, what);
  for (const k of ["project", "style", "summary", "herdr_workspace", "workspace_run_id"]) opt(o, k, isStr, what);
  for (const k of ["team_started_at", "completed_at"]) opt(o, k, isNum, what);
  (o.quotas as unknown[]).forEach((q, i) => {
    const w = `${what}.quotas[${i}]`, x = obj(q, w);
    oneOf(x, "runtime", RUNTIME_KIND, w);
    str(x, "window", w);
    num(x, "percent_used", w);
    def(x, "updated_at", 0, isNum, w);
    opt(x, "resets_at", isNum, w);
  });
  return o as SessionInfo;
}

export function decodeSnapshot(v: unknown, what = "snapshot"): Snapshot {
  const o = obj(v, what);
  o.session = decodeSession(o.session, `${what}.session`);
  o.agents = list(o.agents, decodeAgent, `${what}.agents`);
  o.tasks = list(o.tasks, decodeTask, `${what}.tasks`);
  o.messages = list(o.messages, decodeMessage, `${what}.messages`);
  o.events = list(o.events, decodeEvent, `${what}.events`);
  o.questions = o.questions == null ? [] : list(o.questions, decodeMessage, `${what}.questions`);
  num(o, "server_time", what);
  return o as Snapshot;
}

/** The `VcsEvent` an event carries, or null: not a VCS event, or a payload
 * this build does not know (an action or outcome added later). As
 * `VcsEvent::from_event`. */
export function vcsOf(event: Event): VcsEvent | null {
  if (event.source !== VCS_SOURCE || !event.event_type.startsWith("vcs.")) return null;
  const d = event.data;
  if (!isObj(d)) return null;
  if (!(VCS_ACTION as readonly unknown[]).includes(d.action) || !(VCS_OUTCOME as readonly unknown[]).includes(d.outcome)) return null;
  if (typeof d.op !== "string" || typeof d.agent !== "string") return null;
  if (d.files !== undefined && !isStrArr(d.files)) return null;
  for (const k of ["peer", "commit", "change", "source", "branch", "summary"]) if (d[k] != null && typeof d[k] !== "string") return null;
  return { backend: "", files: [], ...d } as unknown as VcsEvent;
}

export function decodePush(v: unknown): Push | null {
  const o = obj(v, "push");
  const kind = o.kind;
  switch (kind) {
    case "event": return { kind, data: decodeEvent(o.data) };
    case "message": return { kind, data: decodeMessage(o.data) };
    case "agent": return { kind, data: decodeAgent(o.data) };
    case "agent_removed": return { kind, data: { id: str(obj(o.data, "agent_removed"), "id", "agent_removed") } };
    case "task": return { kind, data: decodeTask(o.data) };
    case "session": return { kind, data: decodeSession(o.data) };
    case "pty": return { kind, data: { data: str(obj(o.data, "pty"), "data", "pty") } };
    case "workspaces": return { kind, data: decodeWorkspaces(o.data) };
    case "resync":
    case "pty_closed":
    case "shutdown":
      return { kind };
    case "usage_changed":
    case "docs_changed":
    case "aside":
      return { kind: "feature", name: kind, data: o.data };
    // `deliver` and `compact` are for agents, `summary` for the dashboard
    // server's watch; anything else is a kind this page does not know. All
    // are ignored.
    default:
      return null;
  }
}

/** One text message from the daemon. Throws `DecodeError` on a malformed frame. */
export function decodeFrame(text: string): Frame {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    throw new DecodeError("not JSON");
  }
  const o = obj(v, "frame");
  if (o.push !== undefined) {
    const push = decodePush(o.push);
    return push ? { kind: "push", push } : { kind: "ignored" };
  }
  const id = typeof o.id === "number" ? o.id : null;
  if (o.error !== undefined) {
    const e = obj(o.error, "error");
    return { kind: "error", id, error: { code: str(e, "code", "error"), message: str(e, "message", "error") } };
  }
  if (id === null) fail("frame without id, result or push");
  return { kind: "result", id: id!, result: o.result ?? null };
}

// Result decoders, by method: what the page reads of each answer.

export function decodeHello(v: unknown): HelloInfo {
  const o = obj(v, "hello");
  str(o, "role", "hello");
  num(o, "protocol", "hello");
  opt(o, "web_proof", isStr, "hello");
  return o as HelloInfo;
}

export function decodePaired(v: unknown): WebPaired {
  const o = obj(v, "web_pair");
  return { token: str(o, "token", "web_pair"), verifier: str(o, "verifier", "web_pair") };
}

export function decodeSendResult(v: unknown): SendResult {
  const o = obj(v, "send");
  o.message = decodeMessage(o.message, "send.message");
  def(o, "duplicate", false, isBool, "send");
  def(o, "recipients", [], isStrArr, "send");
  return o as SendResult;
}

export function decodeTaskCard(v: unknown): TaskCard {
  const o = obj(v, "task_get");
  o.task = decodeTask(o.task, "task_get.task");
  o.dependencies = o.dependencies == null ? [] : list(o.dependencies, decodeTask, "task_get.dependencies");
  return o as TaskCard;
}

export function decodeInbox(v: unknown): InboxResult {
  const o = obj(v, "inbox");
  o.messages = list(o.messages, (m, w) => {
    const x = decodeMessage(m, w) as Obj;
    opt(x, "delivered_at", isNum, w);
    opt(x, "acked_at", isNum, w);
    return x as InboxEntry;
  }, "inbox.messages");
  return o as InboxResult;
}

export function decodeGitOutcome(v: unknown): GitOutcome {
  const o = obj(v, "git");
  def(o, "ok", false, isBool, "git");
  def(o, "text", "", isStr, "git");
  opt(o, "commit", isObj, "git");
  if (isObj(o.conflict)) def(o.conflict, "files", [], isStrArr, "git.conflict");
  else opt(o, "conflict", isObj, "git");
  return o as GitOutcome;
}

export function decodeOutput(v: unknown): OutputResult {
  const o = obj(v, "agent_output");
  def(o, "text", "", isStr, "agent_output");
  def(o, "alive", false, isBool, "agent_output");
  return o as OutputResult;
}

export function decodeModels(v: unknown): ModelsResult {
  const o = obj(v, "models_list");
  o.models = list(o.models, (m, w) => {
    const x = obj(m, w);
    oneOf(x, "runtime", RUNTIME_KIND, w);
    def(x, "model", "", isStr, w);
    str(x, "name", w);
    for (const k of ["tier", "price", "summary"]) def(x, k, "", isStr, w);
    opt(x, "unavailable", isStr, w);
    return x as ModelChoice;
  }, "models_list.models");
  return o as ModelsResult;
}

export function decodeTranscript(v: unknown): TranscriptResult {
  const o = obj(v, "agent_transcript");
  o.entries = list(o.entries, (e, w) => {
    const x = obj(e, w);
    oneOf(x, "kind", TRANSCRIPT_KIND, w);
    str(x, "text", w);
    opt(x, "at", isNum, w);
    return x as TranscriptEntry;
  }, "agent_transcript.entries");
  def(o, "available", false, isBool, "agent_transcript");
  def(o, "truncated", false, isBool, "agent_transcript");
  return o as TranscriptResult;
}

export function decodeSummary(v: unknown, what = "summary"): WorkspaceSummary {
  const o = obj(v, what);
  num(o, "rev", what);
  opt(o, "project", isStr, what);
  for (const k of ["agents", "working", "attention", "questions", "tasks", "tasks_done"]) def(o, k, 0, isNum, what);
  return o as WorkspaceSummary;
}

export function decodeWorkspaces(v: unknown, what = "workspaces"): Workspaces {
  const o = obj(v, what);
  num(o, "rev", what);
  o.joined = list(o.joined ?? [], (e, w) => {
    const x = obj(e, w);
    str(x, "id", w);
    num(x, "generation", w);
    oneOf(x, "state", WORKSPACE_STATE, w);
    def(x, "workspace", "", isStr, w);
    def(x, "detail", "", isStr, w);
    def(x, "joined_at", 0, isNum, w);
    opt(x, "project", isStr, w);
    if (isObj(x.daemon)) {
      num(x.daemon, "pid", `${w}.daemon`);
      num(x.daemon, "started_at", `${w}.daemon`);
    } else opt(x, "daemon", isObj, w);
    if (x.summary != null) x.summary = decodeSummary(x.summary, `${w}.summary`);
    return x as WorkspaceEntry;
  }, `${what}.joined`);
  o.others = list(o.others ?? [], (e, w) => {
    const x = obj(e, w);
    str(x, "id", w);
    def(x, "workspace", "", isStr, w);
    opt(x, "project", isStr, w);
    return x as OtherSession;
  }, `${what}.others`);
  return o as Workspaces;
}

export const decodeEvents = (v: unknown): Event[] => list(v, decodeEvent, "events");
export const decodeMessages = (v: unknown): Message[] => list(v, decodeMessage, "history");
export const decodeAgentOrNull = (v: unknown): Agent | null => (v == null ? null : decodeAgent(v));

// ---- helpers the Rust types have -------------------------------------------

export const isBroadcast = (m: Message) => m.to === BROADCAST;
export const isP2p = (m: Message) =>
  !isBroadcast(m) && m.from !== ORCHESTRATOR_ID && m.to !== ORCHESTRATOR_ID && m.from !== OPERATOR_ID && m.to !== OPERATOR_ID;
export const isOperatorQuestion = (m: Message) => m.to === OPERATOR_ID && m.type === "question";
export function options(m: Message): string[] {
  const md = m.metadata;
  return isObj(md) && Array.isArray(md.options) ? md.options.filter(isStr) as string[] : [];
}
export function askedBy(m: Message): string | null {
  const md = m.metadata;
  return isObj(md) && typeof md.asked_by === "string" ? md.asked_by : null;
}
/** Who wants to know: `orchestrator`, or `orchestrator, for claude-2`. */
export const asker = (m: Message) => (askedBy(m) ? `${m.from}, for ${askedBy(m)}` : m.from);
export const isTerminalTask = (s: TaskStatus) => s === "done" || s === "failed";
export const isTerminalAgent = (s: AgentStatus) => s === "failed" || s === "stopped";
export const isRead = (a: VcsAction) => a === "status" || a === "diff";
